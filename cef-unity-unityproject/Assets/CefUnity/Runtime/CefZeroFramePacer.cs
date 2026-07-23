using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     0F 描画遅延 (server-side flush + 描画発行前 recv + 予算適応待ち) の判定
    ///     ステートマシン。純 C# (Unity API 非依存) — EditMode テストで全分岐を検証できる。
    ///
    ///     背景: CEF external BeginFrame は deadline=null で発行されるため、1 回の
    ///     BeginFrame では display compositor が renderer の submit を待たず「前フレーム」を
    ///     即 draw する (構造的 1F 遅延)。サーバーが BF#1 の +3/+6ms に内部 flush (BF#2) を
    ///     発行して最新内容を draw させる (server-side flush、server.rs)。クライアントは
    ///     描画発行前の recv 位置で flush 結果の到着 (accel_frame_id 増分) を短時間だけ
    ///     待ち、同フレームの present に乗せる (0F)。待ちの上限は BF#1 (EarlyUpdate) からの
    ///     経過時間で cap するため、ゲーム処理が重いフレームでは自動的に待ちゼロになる
    ///     (その場合 flush 結果は自然に到着済み)。間に合わなければ従来通り 1F フォールバック。
    ///
    ///     ⚠ 定数群は実測チューニングの成果物 (REFACTORING_REPORT.md §1 の不変条件)。
    ///     値を変える場合はスクロール実測 (AFI +120/2s) での回帰確認とセットで行うこと。
    /// </summary>
    public sealed class CefZeroFramePacer
    {
        // server-side flush#1 は BF#1+3ms に発行される (server.rs FLUSH_THRESHOLDS_MS[0])。
        // その draw 由来 paint が accel_frame_id に計上され得る最短時刻のマージン。これより
        // 前の増分は BF#1 由来の stale paint (#A) とみなして読み捨て、fresh (#B) を待つ。
        public const float FreshPaintMinDelayMs = 4.5f;

        // damage の有無は「flush#1 の draw 由来 paint が届き得る時刻」まで分からない
        // (renderer のタイマー/rAF 発火 → submit +2-4ms → flush#1 draw → paint +5-6ms)。
        // BF#1 からこの時間まで増分ゼロなら「このフレームに damage なし」と判断して
        // 待ちを打ち切る (5Hz 更新ページ等で damage の無いフレームの空回りを短縮)。
        public const float NoDamageGiveUpMs = 7f;

        // 早着 paint (#A、freshMinTime より前の増分) を読み捨てた後、この時刻までに
        // flush 由来 (#B) が来なければ #A の内容を採用して抜ける。#A がタイマー発火由来の
        // fresh な内容 (damage を #A が消費し #B が生成されない) ケースで、絶対上限まで
        // 粘る無駄を防ぐ。#B の標準到着 (+5-6.5ms) を跨ぐ位置に置く。
        public const float EarlyPaintAdoptMs = 7.5f;

        // server は「paint 発生フレーム」が 3 連続すると flush を抑止する (damage streak、
        // server.rs DAMAGE_STREAK_SUPPRESS_FLUSH)。抑止中は fresh (#B) が来ないため、
        // クライアント側でもスコアで同じ状態を推定し、最初の AFI 増分 (BF#1 由来 paint)
        // で即座に待ちを抜けて空回りを防ぐ。スコアは fresh 受信 +1 / 受信なし -2 の
        // ヒステリシス: 連続スクロール中に 1 フレームだけ受信を取り逃しても抑止推定を
        // 維持する (即 0 リセットにすると、取り逃しの直後 3 フレームが「非抑止」誤推定と
        // なり、来ない #B を待って earlyAdopt まで空回りする振動が起きる。実測で
        // スクロール時 block_avg 5.5ms・コンテンツ供給 85-92% に劣化した)。
        public const int StreakScoreSuppress = 3; // これ以上で抑止推定
        public const int StreakScoreMax = 6;      // 天井 (解除応答性のため小さく保つ)

        // 直近何フレーム連続で CEF へ入力を送ったか。連続入力 (スクロール/ドラッグ/
        // キーリピート) は server 側で damage streak 抑止に入りがち = 待ちの価値が無い。
        // streak スコアだけだと CEF のヒッチ (2 フレーム paint 欠落) で推定が外れて
        // 待ちが再発し、busy-wait の CPU 競合が荒れを増幅する振動が起きるため (実測)、
        // 連続入力そのものも待ちスキップの条件にする。単発入力 (クリック・単打鍵) は
        // 連続にならないので従来通り待って 0F を取る。
        public const int SustainedInputFrames = 3;

        // 直近で fresh paint を取得してからの経過フレーム数の窓。プローブ判定に使う:
        // この窓の間はページが動いている可能性があるとみなして damage プローブ待ちを行い、
        // 窓を超えたら完全静止とみなして待ちを止める (busy-wait コストをゼロにする)。
        // ページ内タイマー起点の低頻度更新 (例: 5Hz = 12 フレーム間隔) を捕捉できるよう
        // 1 秒 (60 フレーム) に設定。静止→再開の最初の 1 paint だけは 1F で拾う。
        public const int ProbeWindowFrames = 60;

        private int _streakScore;
        private int _consecutiveInputFrames;
        private int _framesSinceFreshPaint = int.MaxValue;

        /// <summary>BF#1 送信直前の実時刻 (秒)。待ちデッドラインと fresh 判定時刻の基準。</summary>
        public float Bf1Time { get; private set; }

        /// <summary>BF#1 直前の accel_frame_id (増分検知の基準)。</summary>
        public ulong AfiAtBf1 { get; private set; }

        /// <summary>
        ///     EarlyUpdate 末尾・BF#1 送信の直前に呼ぶ (入力ハンドラ群の後 =
        ///     inputSentThisFrame 確定済みであること)。afiNow は software 経路では 0 でよい。
        /// </summary>
        public void OnBeginFrame(float now, ulong afiNow, bool inputSentThisFrame)
        {
            _consecutiveInputFrames = inputSentThisFrame
                ? Math.Min(_consecutiveInputFrames + 1, 1000)
                : 0;
            AfiAtBf1 = afiNow;
            Bf1Time = now;
        }

        /// <summary>
        ///     プローブ判定: 入力を送った or 直近 ProbeWindowFrames 以内にページが動いて
        ///     いた時だけ待つ。完全静止ページでは paint 自体が来ないため待たない (ブロック 0)。
        /// </summary>
        public bool ShouldSkipAsIdle(bool inputSentThisFrame)
            => !(inputSentThisFrame || _framesSinceFreshPaint < ProbeWindowFrames);

        /// <summary>
        ///     サーバーの damage streak 抑止 (flush 無し) 推定・連続入力の待ちスキップ判定。
        ///     抑止中 = 連続描画中はコンテンツがどのみち 1F (BF#1 の即時 draw は前フレーム
        ///     内容) なので、待っても鮮度は上がらない。さらに busy-wait の CPU が CEF
        ///     プロセス群の paint 生成と競合し、スクロール中の供給を 2-7% 落とす (実測:
        ///     待ち OFF 99-100% / ON 92-98%)。抑止中・連続入力中は待ちをスキップして
        ///     CPU を返し、ノンブロッキング受信のみ行う (待ち OFF と同じ挙動 = 供給 ~100%)。
        /// </summary>
        public bool ShouldSkipAsSuppressed()
            => _streakScore >= StreakScoreSuppress || _consecutiveInputFrames >= SustainedInputFrames;

        /// <summary>busy-wait 窓を開く (BF#1 時刻基準の各デッドラインを確定)。</summary>
        public ZeroFrameWaitWindow OpenWaitWindow(float zeroFrameWaitMs)
            => new ZeroFrameWaitWindow(Bf1Time, zeroFrameWaitMs, AfiAtBf1);

        /// <summary>recv 成功 (新 paint 取得) 時に呼ぶ。</summary>
        public void OnFreshPaint()
        {
            _framesSinceFreshPaint = 0;
            if (_streakScore < StreakScoreMax) _streakScore++;
        }

        /// <summary>recv 失敗 (新 paint なし) 時に呼ぶ。</summary>
        public void OnNoPaint()
        {
            if (_framesSinceFreshPaint != int.MaxValue) _framesSinceFreshPaint++;
            _streakScore = Math.Max(0, _streakScore - 2);
        }
    }

    /// <summary>
    ///     0F 待ち busy-wait の 1 窓分の判定状態。呼び出し側のループは
    ///     「now 取得 → DeadlineReached → (Peek して) OnAfiSample → SpinWait」の順を守ること
    ///     (デッドライン超過時に余分な Peek FFI を発行しない、元実装と同一の順序)。
    /// </summary>
    public struct ZeroFrameWaitWindow
    {
        private readonly float _deadline;
        private readonly float _freshMinTime;
        private readonly float _noDamageGiveUp;
        private readonly float _earlyAdopt;
        private ulong _baseline;
        private bool _sawEarlyPaint;

        public ZeroFrameWaitWindow(float bf1Time, float zeroFrameWaitMs, ulong afiAtBf1)
        {
            _deadline = bf1Time + zeroFrameWaitMs * 0.001f;
            _freshMinTime = bf1Time + CefZeroFramePacer.FreshPaintMinDelayMs * 0.001f;
            _noDamageGiveUp = bf1Time + CefZeroFramePacer.NoDamageGiveUpMs * 0.001f;
            _earlyAdopt = bf1Time + CefZeroFramePacer.EarlyPaintAdoptMs * 0.001f;
            _baseline = afiAtBf1;
            _sawEarlyPaint = false;
        }

        /// <summary>絶対上限。true なら Peek せずに待ちを終える。</summary>
        public bool DeadlineReached(float now) => now >= _deadline;

        /// <summary>AFI 観測 1 回分の判定。true = 待ち終了 (最新 paint を回収してよい)。</summary>
        public bool OnAfiSample(float now, ulong afi)
        {
            if (afi != _baseline)
            {
                // 増分検知。flush#1 の draw があり得る時刻 (freshMinTime) より前の増分は
                // BF#1 由来 stale (#A) とみなして読み捨て、fresh (#B) を待ち続ける。
                if (now >= _freshMinTime) return true;
                _baseline = afi;
                _sawEarlyPaint = true;
                return false;
            }
            if (_sawEarlyPaint)
            {
                // 早着 (#A) は届いたが #B が来ない: タイマー発火由来の damage を #A が
                // 消費したケース。#B の標準到着時刻を跨いだら #A を採用して抜ける。
                return now >= _earlyAdopt;
            }
            // 増分ゼロのまま判定時刻超え = このフレームに damage なし。
            return now >= _noDamageGiveUp;
        }
    }
}
