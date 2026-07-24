using System;

namespace CefUnity.Runtime
{
    /// <summary>ScrollInputPipeline.StartNativeSource の結果。</summary>
    public enum NativeScrollSourceStart
    {
        /// <summary>native ソース有効。以後 Input.mouseScrollDelta は読まないこと (二重計上防止)。</summary>
        Started,

        /// <summary>このプラットフォームに native ソース実装がない。フォールバック (ログ不要)。</summary>
        NotSupported,

        /// <summary>実装はあるが開始できなかった (ヘッドレス等)。フォールバック。</summary>
        Unavailable,

        /// <summary>初期化が例外を投げた (dylib 不在等の P/Invoke 失敗)。フォールバック。</summary>
        Failed,
    }

    /// <summary>
    ///     スクロール入力パイプライン。native ソースの drain、precise→リサンプラ /
    ///     非 precise (ホイールノッチ)→スムーザのルーティング、毎フレームの排出計算、
    ///     開発用の生イベント録画 (cef_scroll_record) を 1 箇所に集約した純 C# クラス。
    ///     呼び出し側 (MonoBehaviour) の責務は座標決定と SendMouseWheel のみ。
    ///     設計: docs/superpowers/specs/2026-07-20-scroll-smoothing-design.md,
    ///     docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md
    ///
    ///     フレーム内の呼び出し順序 (排出列の互換に必要):
    ///     Drain(overBrowser, scale) → TickResampler → (送信) → TickSmoother → (送信)。
    /// </summary>
    public sealed class ScrollInputPipeline : IDisposable
    {
        /// <summary>
        ///     マウスホイール 1 ステップ (Input.mouseScrollDelta の 1.0) あたりのスクロール量
        ///     (CEF view px)。Chromium ネイティブは macOS ~40px/ライン・Windows ~100px/ノッチ
        ///     相当で、その中間に置いている。体感調整はここを変える。
        /// </summary>
        public const float WheelPixelsPerStep = 60f;

        /// <summary>
        ///     スムーザ時定数 (秒)。体感チューニング (2026-07-20, ビルドで τ=45/25/15/0 を A/B)
        ///     で確定した値。遅延感を最小化しつつジッター/フリック巨大単発を均す最弱設定
        ///     (初フレームで残距離の約 67% を排出)。
        /// </summary>
        public const float SmoothTau = 0.015f;

        // 生 delta を残距離に蓄積し、毎フレーム均一化して排出 (トラックパッド慣性減衰の
        // 0.0x 級微小 delta の端数繰り越しは ScrollSmoother 内部に統合済み)。
        private readonly ScrollSmoother _smoother = new ScrollSmoother();

        // Predictive=true 既定: 低遅延 (~5ms) の予測リサンプル。ビルド A/B で採用 (2026-07-22)。
        private readonly ScrollResampler _resampler = new ScrollResampler { Predictive = true };

        private readonly ScrollInputEvent[] _eventBuf = new ScrollInputEvent[256];
        private IScrollEventSource _source;

        /// <summary>native ソースが有効か。false なら呼び出し側はフォールバック経路を使う。</summary>
        public bool HasNativeSource => _source != null;

        /// <summary>リサンプラの予測モード (既定 true)。開発トグル cef_scroll_interp 用。</summary>
        public bool Predictive
        {
            get => _resampler.Predictive;
            set => _resampler.Predictive = value;
        }

        /// <summary>
        ///     プラットフォームの native ソースを生成して開始する。プラットフォーム分岐は
        ///     ここに封じ込める (呼び出し側に #if を増やさない)。
        /// </summary>
        public NativeScrollSourceStart StartNativeSource(out Exception error)
        {
            error = null;
#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
            var src = new MacNativeScrollSource();
            try
            {
                if (src.Start())
                {
                    _source = src;
                    return NativeScrollSourceStart.Started;
                }
            }
            catch (Exception e)
            {
                // dylib 不在等の P/Invoke 例外は回復可能 — フォールバックに落とす。
                error = e;
                src.Dispose();
                return NativeScrollSourceStart.Failed;
            }
            src.Dispose();
            return NativeScrollSourceStart.Unavailable;
#else
            return NativeScrollSourceStart.NotSupported;
#endif
        }

        /// <summary>
        ///     任意の IScrollEventSource を接続する (Start 済みを渡すこと)。
        ///     テスト、および将来の Windows (WndProc) / Linux (XInput2) ソース差し込み用。
        /// </summary>
        public void AttachSource(IScrollEventSource source)
        {
            _source = source;
        }

        /// <summary>
        ///     フォールバック経路: Input.mouseScrollDelta のステップ値をスムーザに蓄積する。
        ///     resolutionScale は view (CSS px) が広がった分の補正 — 画面上の体感速度を
        ///     scale に依らず一定に保つ。
        /// </summary>
        public void AddWheelSteps(float xSteps, float ySteps, float resolutionScale)
        {
            _smoother.AddInput(
                xSteps * WheelPixelsPerStep * resolutionScale,
                ySteps * WheelPixelsPerStep * resolutionScale);
        }

        /// <summary>
        ///     native ソースの新着イベントを取り込む。overBrowser のときだけ転送する
        ///     (Editor で他ウィンドウ上のスクロールを拾わない)。precise はリサンプラへ、
        ///     非 precise (ホイールノッチ) はスムーザへルーティングする。
        /// </summary>
        public void Drain(bool overBrowser, float resolutionScale)
        {
            if (_source == null) return;
            var n = _source.Poll(_eventBuf);
            for (var i = 0; i < n; i++)
            {
                ref var e = ref _eventBuf[i];
#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
                RecordEvent(in e, overBrowser, resolutionScale);
#endif
                if (!overBrowser) continue;
                if (e.Precise)
                {
                    // precise delta は CSS px 相当 → view 座標へ resolutionScale を掛ける。
                    var scaled = e;
                    scaled.DxPx *= resolutionScale;
                    scaled.DyPx *= resolutionScale;
                    _resampler.AddEvent(in scaled);
                }
                else
                {
                    // ノッチ (ライン単位) はスムーザでグライドさせる (Chrome 層3相当)。
                    _smoother.AddInput(
                        e.DxPx * WheelPixelsPerStep * resolutionScale,
                        e.DyPx * WheelPixelsPerStep * resolutionScale);
                }
            }
        }

        /// <summary>
        ///     リサンプラの 1 フレーム排出。native ソースが無ければ false。
        ///     排出 0 でも毎フレーム呼ぶこと (リサンプラ状態と録画の前提)。
        /// </summary>
        public bool TickResampler(out int dx, out int dy)
        {
            dx = 0;
            dy = 0;
            if (_source == null) return false;
            // Tick と録画で同一の now を使う (リプレイ照合の系統誤差防止)。
            var now = _source.Now;
            _resampler.Tick(now, out dx, out dy);
#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
            RecordTick(now, dx, dy);
#endif
            return true;
        }

        /// <summary>スムーザの 1 フレーム排出。非アクティブなら false (排出なし)。</summary>
        public bool TickSmoother(float dt, out int dx, out int dy)
        {
            dx = 0;
            dy = 0;
            if (!_smoother.IsActive) return false;
            _smoother.Tick(dt, SmoothTau, out dx, out dy);
            return true;
        }

        /// <summary>グライド途中の残距離/リサンプラ履歴を全消去する (新ページへ流し込まない)。</summary>
        public void Reset()
        {
            _smoother.Reset();
            _resampler.Reset();
        }

        public void Dispose()
        {
            _source?.Dispose();
            _source = null;
#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
            // 末尾 <30 行 (ジェスチャ終端が乗りやすい) を失わないための最終フラッシュ。
            FlushRecording();
#endif
        }

#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
        // --- 生イベント録画 (cef_scroll_record): オフラインリプレイ検証用 ---
        // 形式 ($TMPDIR/cef_scroll_events.csv に追記):
        //   S,resolutionScale                     … scale 変化時 (リプレイ側で乗算する)
        //   E,ts,dx,dy,phase,precise,over        … 生値 (スケール前)。over=0 は live 未転送
        //   T,now,dx,dy,predictive               … 毎 Tick (0 排出も含む — 忠実度照合に必要)
        private readonly System.Collections.Generic.List<string> _recordLog = new();
        private bool _recordEnabled;
        private float _recordedScale = float.NaN;

        /// <summary>開発トグル cef_scroll_record の反映先。無効化時に残量をフラッシュする。</summary>
        public bool RecordingEnabled
        {
            get => _recordEnabled;
            set
            {
                if (_recordEnabled && !value) FlushRecording();
                if (!_recordEnabled && value) _recordedScale = float.NaN; // 再開時に S 行を再出力
                _recordEnabled = value;
            }
        }

        private void RecordEvent(in ScrollInputEvent e, bool overBrowser, float resolutionScale)
        {
            if (!_recordEnabled) return;
            if (_recordedScale != resolutionScale)
            {
                _recordedScale = resolutionScale;
                AddRecordLine($"S,{resolutionScale:R}");
            }
            AddRecordLine(
                $"E,{e.Timestamp:R},{e.DxPx:R},{e.DyPx:R},{(byte)e.Phase},{(e.Precise ? 1 : 0)},{(overBrowser ? 1 : 0)}");
        }

        private void RecordTick(double now, int dx, int dy)
        {
            if (!_recordEnabled) return;
            AddRecordLine($"T,{now:R},{dx},{dy},{(_resampler.Predictive ? 1 : 0)}");
        }

        private void AddRecordLine(string line)
        {
            _recordLog.Add(line);
            if (_recordLog.Count >= 30) FlushRecording();
        }

        /// <summary>録画バッファを CSV へ書き出す (失敗は握りつぶし — 本経路を止めない)。</summary>
        public void FlushRecording()
        {
            if (_recordLog.Count == 0) return;
            try
            {
                System.IO.File.AppendAllText(
                    System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_scroll_events.csv"),
                    string.Join("\n", _recordLog) + "\n");
            }
            catch
            {
                // 録画は開発用診断 — IO 失敗でスクロール本経路を壊さない
            }
            _recordLog.Clear();
        }
#endif
    }
}
