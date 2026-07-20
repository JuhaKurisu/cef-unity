using System;
using System.Collections.Generic;
using System.Reflection;
using System.Text;
using CefUnity.Interop;
using UnityEditor;
using UnityEngine;
using UnityEngine.LowLevel;
using UnityEngine.PlayerLoop;
using UnityEngine.Rendering;
using UnityEngine.UI;
#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
using System.Runtime.InteropServices;
#endif

namespace CefUnity.Runtime
{
    // PlayerLoop に挿入するサブシステムの識別用マーカー型
    public struct CefUnityEarlyUpdate { }
    public struct CefUnityPostLateUpdate { }

    public class CefUnityBrowserSample : MonoBehaviour
    {
        private const float DoubleClickTime = 0.3f;
        private const int DoubleClickDistance = 4;

        private static readonly float KeyRepeatDelay = GetOSKeyRepeatDelay();
        private static readonly float KeyRepeatRate = GetOSKeyRepeatRate();


        // -----------------------------------------------------------------------
        // Keyboard
        // -----------------------------------------------------------------------

        // Unity KeyCode → CefKeyCode の対応テーブル
        private static readonly (KeyCode unity, CefKeyCode cef)[] SpecialKeyTable =
        {
            (KeyCode.Backspace, CefKeyCodes.Backspace),
            (KeyCode.Tab, CefKeyCodes.Tab),
            (KeyCode.Return, CefKeyCodes.Return),
            (KeyCode.Escape, CefKeyCodes.Escape),
            (KeyCode.Delete, CefKeyCodes.Delete),
            (KeyCode.Insert, CefKeyCodes.Insert),

            (KeyCode.UpArrow, CefKeyCodes.UpArrow),
            (KeyCode.DownArrow, CefKeyCodes.DownArrow),
            (KeyCode.LeftArrow, CefKeyCodes.LeftArrow),
            (KeyCode.RightArrow, CefKeyCodes.RightArrow),
            (KeyCode.Home, CefKeyCodes.Home),
            (KeyCode.End, CefKeyCodes.End),
            (KeyCode.PageUp, CefKeyCodes.PageUp),
            (KeyCode.PageDown, CefKeyCodes.PageDown),

            (KeyCode.F1, CefKeyCodes.F1), (KeyCode.F2, CefKeyCodes.F2),
            (KeyCode.F3, CefKeyCodes.F3), (KeyCode.F4, CefKeyCodes.F4),
            (KeyCode.F5, CefKeyCodes.F5), (KeyCode.F6, CefKeyCodes.F6),
            (KeyCode.F7, CefKeyCodes.F7), (KeyCode.F8, CefKeyCodes.F8),
            (KeyCode.F9, CefKeyCodes.F9), (KeyCode.F10, CefKeyCodes.F10),
            (KeyCode.F11, CefKeyCodes.F11), (KeyCode.F12, CefKeyCodes.F12),

            (KeyCode.Keypad0, CefKeyCodes.Keypad0), (KeyCode.Keypad1, CefKeyCodes.Keypad1),
            (KeyCode.Keypad2, CefKeyCodes.Keypad2), (KeyCode.Keypad3, CefKeyCodes.Keypad3),
            (KeyCode.Keypad4, CefKeyCodes.Keypad4), (KeyCode.Keypad5, CefKeyCodes.Keypad5),
            (KeyCode.Keypad6, CefKeyCodes.Keypad6), (KeyCode.Keypad7, CefKeyCodes.Keypad7),
            (KeyCode.Keypad8, CefKeyCodes.Keypad8), (KeyCode.Keypad9, CefKeyCodes.Keypad9),
            (KeyCode.KeypadPeriod, CefKeyCodes.KeypadPeriod),
            (KeyCode.KeypadDivide, CefKeyCodes.KeypadDivide),
            (KeyCode.KeypadMultiply, CefKeyCodes.KeypadMultiply),
            (KeyCode.KeypadMinus, CefKeyCodes.KeypadMinus),
            (KeyCode.KeypadPlus, CefKeyCodes.KeypadPlus),
            (KeyCode.KeypadEnter, CefKeyCodes.KeypadEnter),

            (KeyCode.LeftShift, CefKeyCodes.LeftShift),
            (KeyCode.RightShift, CefKeyCodes.RightShift),
            (KeyCode.LeftControl, CefKeyCodes.LeftControl),
            (KeyCode.RightControl, CefKeyCodes.RightControl),
            (KeyCode.LeftAlt, CefKeyCodes.LeftAlt),
            (KeyCode.RightAlt, CefKeyCodes.RightAlt),
            (KeyCode.LeftCommand, CefKeyCodes.LeftCommand),
            (KeyCode.RightCommand, CefKeyCodes.RightCommand),
            (KeyCode.CapsLock, CefKeyCodes.CapsLock)
        };

        [SerializeField] private string _url;
        [SerializeField] private RawImage _rawImage;
        [SerializeField] private float _resolutionScale = 1;
        [SerializeField] private bool _enableLog;

        [Header("Audio")]
        [Tooltip("CEF の音声を Unity の AudioSource で再生する (CEF/ブラウザ側では鳴らさない)")]
        [SerializeField] private bool _enableAudio = true;

        [Tooltip("音声レンダラ。UnityMixer=AudioSource 再生 (ミキサ統合, ~160ms) / Native=AudioUnit 直結 (macOS, ~30ms)")]
        [SerializeField] private AudioRendererMode _audioRenderer = AudioRendererMode.UnityMixer;

        [Tooltip("音声出力の DSP バッファサイズ (フレーム/段)。小さいほど低遅延だが負荷増。" +
                 "256=Best latency, 512=Good, 1024=Best performance。0 でプロジェクト設定のまま。" +
                 "ProjectSettings/Audio と同値だがエディタ実行時に確実に反映させるため実行時にも適用する。")]
        [SerializeField] private int _audioDspBufferSize = 256;

        /// <summary>音声レンダラの選択。</summary>
        public enum AudioRendererMode
        {
            /// <summary>Unity AudioSource (FMOD ミキサ) で再生。ミキサ統合 (エフェクト等) が効くが遅延大 (~160ms)。</summary>
            UnityMixer,

            /// <summary>ネイティブ AudioUnit で再生 (macOS)。低遅延 (~30ms) だが Unity ミキサ機能は効かない。</summary>
            Native,
        }

        private CefAudioOutput _audioOutput;
        private CefNativeAudio _nativeAudio;

        private readonly Dictionary<KeyCode, float> _keyDownTime = new();
        private readonly Dictionary<KeyCode, float> _keyLastRepeat = new();

        private Browser _browser;
        private int _clickCount;
        private int _currentHeight;
        private int _currentWidth;
        private float _diagTimer;
        private bool _imeActive;
        private bool _imeSuppressKeys;

        // Accelerated paint (IOSurface / Metal via Mach port)
        private bool _useAcceleratedPaint;
        private IntPtr _lastAccelTexPtr;

        // End-to-end frame delay measurement (BeginFrame frame - paint frame)
        private int _delaySampleCount;
        private long _delaySumFrames;
        private int _delayMaxFrames;
        private int _delayMinFrames = int.MaxValue;
        private readonly int[] _delayBuckets = new int[8]; // 0,1,2,3,4,5,6,7+ frames
        private float _delayReportTimer;

        // -----------------------------------------------------------------------
        // 0F 描画遅延 (server-side flush + 描画発行前 recv + 予算適応待ち)
        // -----------------------------------------------------------------------
        // CEF external BeginFrame は deadline=null で発行されるため、1 回の BeginFrame
        // では display compositor が renderer の submit を待たず「前フレーム」を即 draw する
        // (構造的 1F 遅延)。サーバーが BF#1 の +3/+6ms に内部 flush (BF#2) を発行して
        // 最新内容を draw させる (server-side flush、server.rs)。クライアントは描画発行前の
        // recv 位置で flush 結果の到着 (accel_frame_id 増分) を短時間だけ待ち、同フレームの
        // present に乗せる (0F)。待ちの上限は BF#1 (EarlyUpdate) からの経過時間で cap する
        // ため、ゲーム処理が重いフレームでは自動的に待ちゼロになる (その場合 flush 結果は
        // 自然に到着済み)。間に合わなければ従来通り 1F フォールバック。
        [SerializeField, Tooltip("BF#1 発行からこの時間 (ms) までは flush 結果の到着を待って 0F 化する " +
            "(0 で待ち無効 = 常にノンブロッキング受信)。60fps 予算 16.7ms 内に収まる 10ms 程度を推奨。")]
        private float _zeroFrameWaitMs = 10f;
        // server-side flush#1 は BF#1+3ms に発行される (server.rs FLUSH_THRESHOLDS_MS[0])。
        // その draw 由来 paint が accel_frame_id に計上され得る最短時刻のマージン。これより
        // 前の増分は BF#1 由来の stale paint (#A) とみなして読み捨て、fresh (#B) を待つ。
        private const float FreshPaintMinDelayMs = 4.5f;
        // damage の有無は「flush#1 の draw 由来 paint が届き得る時刻」まで分からない
        // (renderer のタイマー/rAF 発火 → submit +2-4ms → flush#1 draw → paint +5-6ms)。
        // BF#1 からこの時間まで増分ゼロなら「このフレームに damage なし」と判断して
        // 待ちを打ち切る (5Hz 更新ページ等で damage の無いフレームの空回りを短縮)。
        private const float NoDamageGiveUpMs = 7f;
        // 早着 paint (#A、freshMinTime より前の増分) を読み捨てた後、この時刻までに
        // flush 由来 (#B) が来なければ #A の内容を採用して抜ける。#A がタイマー発火由来の
        // fresh な内容 (damage を #A が消費し #B が生成されない) ケースで、絶対上限まで
        // 粘る無駄を防ぐ。#B の標準到着 (+5-6.5ms) を跨ぐ位置に置く。
        private const float EarlyPaintAdoptMs = 7.5f;
        // server は「paint 発生フレーム」が 3 連続すると flush を抑止する (damage streak、
        // server.rs DAMAGE_STREAK_SUPPRESS_FLUSH)。抑止中は fresh (#B) が来ないため、
        // クライアント側でもスコアで同じ状態を推定し、最初の AFI 増分 (BF#1 由来 paint)
        // で即座に待ちを抜けて空回りを防ぐ。スコアは fresh 受信 +1 / 受信なし -2 の
        // ヒステリシス: 連続スクロール中に 1 フレームだけ受信を取り逃しても抑止推定を
        // 維持する (即 0 リセットにすると、取り逃しの直後 3 フレームが「非抑止」誤推定と
        // なり、来ない #B を待って earlyAdopt まで空回りする振動が起きる。実測で
        // スクロール時 block_avg 5.5ms・コンテンツ供給 85-92% に劣化した)。
        private const int StreakScoreSuppress = 3; // これ以上で抑止推定
        private const int StreakScoreMax = 6;      // 天井 (解除応答性のため小さく保つ)
        private int _streakScore;
        // 直近何フレーム連続で CEF へ入力を送ったか。連続入力 (スクロール/ドラッグ/
        // キーリピート) は server 側で damage streak 抑止に入りがち = 待ちの価値が無い。
        // streak スコアだけだと CEF のヒッチ (2 フレーム paint 欠落) で推定が外れて
        // 待ちが再発し、busy-wait の CPU 競合が荒れを増幅する振動が起きるため (実測)、
        // 連続入力そのものも待ちスキップの条件にする。単発入力 (クリック・単打鍵) は
        // 連続にならないので従来通り待って 0F を取る。
        private const int SustainedInputFrames = 3;
        private int _consecutiveInputFrames;
        // BF#1 送信直前の実時刻 (待ちデッドラインと fresh 判定時刻の基準)。
        private float _bf1Time;
        // EarlyUpdate で BeginFrame#1 を撃つ直前の accel_frame_id (増分検知の基準)。
        private ulong _afiAtBf1;
        // 直近で fresh paint を取得してからの経過フレーム数。プローブ判定に使う:
        // この窓の間はページが動いている可能性があるとみなして damage プローブ待ちを行い、
        // 窓を超えたら完全静止とみなして待ちを止める (busy-wait コストをゼロにする)。
        // ページ内タイマー起点の低頻度更新 (例: 5Hz = 12 フレーム間隔) を捕捉できるよう
        // 1 秒 (60 フレーム) に設定。静止→再開の最初の 1 paint だけは 1F で拾う。
        private int _framesSinceFreshPaint = int.MaxValue;
        private const int ProbeWindowFrames = 60;
        // このフレームで CEF へ入力イベントを送ったか (アクティブ判定の即時トリガー)。
        private bool _inputSentThisFrame;
        // 0F 待ち検証メトリクス
        private int _dpFreshCount;     // 待ちの後 (or 待ちゼロで) 新 paint を取得できた回数
        private int _dpFallbackCount;  // デッドラインまでに届かず諦めた回数 (前フレーム内容を継続表示)
        private int _dpIdleCount;      // 非アクティブで待ちをスキップした回数
        private double _dpBlockSumMs;  // recv hook でのブロック時間合計
        private double _dpBlockMaxMs;

        // -----------------------------------------------------------------------
        // Jitter 計装 (機構切り分け用)
        // -----------------------------------------------------------------------
        // 機構1: フレーム時間 (present 間隔) の分布。double-pump のブロックが present 直前に
        //        入るため、ブロック量のジッタがそのままフレーム間隔のジッタ = ジャダーになる。
        private double _ftSum, _ftSumSq, _ftMax;
        private int _ftCount, _ftOver18, _ftOver20, _ftOver25; // 18=16.67ms+余裕,20,25ms 超過数
        // 機構2: コンテンツ更新間隔 (fresh paint を取得した実時刻の連続差) の分布。
        //        Chromium のスクロール曲線はこの実時刻でサンプルされるので、間隔のジッタが
        //        見かけのスクロール速度のジッタ = judder に直結する。
        private float _lastFreshRealtime = -1f;
        private double _ciSum, _ciSumSq, _ciMax;
        private int _ciCount;
#if UNITY_EDITOR || DEVELOPMENT_BUILD
        private bool _navTestDone; // 計測用
        private bool _audioTestDone; // 音声テスト用 (cef_load_url トリガー)
#endif

        // PlayerLoop hook 用の singleton 参照 (現在のサンプル構成は単一 Browser のみ対応)
        private static CefUnityBrowserSample s_instance;
        // PlayerLoop hook を install したかどうか
        private bool _playerLoopHooked;

        // 同 Unity フレーム内で 1 回取得したらフレーム末まで再取得しないフラグ
        private int _textureUpdatedFrame = -1;

        // 検証用メトリクス
        private int _postLateUpdateInvokeCount;  // PostLateUpdate hook の呼び出し回数
        private int _gotInPostLateUpdateCount;   // PostLateUpdate で取得成功した回数
        private int _recvFailCount;              // 取得失敗 (1 frame 遅延扱い)
        // 最近の生サンプルを保持 (frame_count, paint_unity_frame, delta) でログ出力
        private readonly System.Collections.Generic.Queue<(int fc, ulong pf, int delta)> _recentSamples
            = new System.Collections.Generic.Queue<(int, ulong, int)>();

        // マウスホイール 1 ステップ (Input.mouseScrollDelta の 1.0) あたりのスクロール量
        // (CEF view ピクセル)。Chromium ネイティブは macOS ~40px/ライン・Windows
        // ~100px/ノッチ相当で、その中間に置いている。体感調整はここを変える。
        private const float WheelPixelsPerStep = 60f;
        // スクロール平滑 (指数追従): 生 delta を残距離に蓄積し、毎フレーム均一化して排出。
        // 旧 _wheelAccum の端数繰り越し (トラックパッド慣性減衰の 0.0x 級微小 delta 対策)
        // は ScrollSmoother 内部に統合。設計: docs/superpowers/specs/2026-07-20-scroll-smoothing-design.md
        private readonly ScrollSmoother _scrollSmoother = new ScrollSmoother();
        // 時定数 (秒)。体感チューニング (2026-07-20, ビルドで τ=45/25/15/0 を A/B) で確定した値。
        // 遅延感を最小化しつつジッター/フリック巨大単発を均す最弱設定 (初フレームで残距離の約67%を排出)。
        private const float ScrollSmoothTau = 0.015f;

#if UNITY_EDITOR || DEVELOPMENT_BUILD
        // --- 分析用 (開発ビルドのみ): 毎フレームの scroll 量/frame time/paint を CSV 記録 ---
        private readonly System.Collections.Generic.List<string> _perfLog = new();
        private int _frameSentDy;
#endif

        // Double/triple click detection
        private float _lastClickTime;
        private int _lastClickX = -1;
        private int _lastClickY = -1;
        private int _lastMouseX = -1;
        private int _lastMouseY = -1;
        private Texture2D _texture;

        private void Start()
        {
            try
            {
                _currentWidth = Screen.width;
                _currentHeight = Screen.height;

                // CEF Viz Compositor は VSync ロックで 60Hz paint。Unity の LateUpdate を
                // それより高頻度にすると半分以上のフレームで paint が間に合わず取得失敗 →
                // 1 フレーム遅延が発生する。Unity を 60fps に固定して CEF と同期させる。
                // ティアリング修正: ハードウェア VSync を既定に (60Hz ディスプレイで 60fps ロック、
                // present がディスプレイ走査に同期してティアリング解消)。
                QualitySettings.vSyncCount = 1;
                Application.targetFrameRate = 60;
#if UNITY_EDITOR || DEVELOPMENT_BUILD
                // 開発トグル: cef_novsync で VSync 無しの従来挙動と比較できる。
                if (System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_novsync")))
                {
                    QualitySettings.vSyncCount = 0;
                    Debug.Log("[CefUnity] VSYNC MODE: vSyncCount=0 (no vsync)");
                }

                // 開発トグル: cef_no_zero_wait マーカーで 0F 待ちを無効化 (baseline 比較用)。
                // シーンの serialized 値は Editor が外部変更を再読込しないため、既存の開発
                // トグル群と同じ temp ファイル方式で切り替える。
                if (System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_no_zero_wait")))
                    _zeroFrameWaitMs = 0f;
#endif

                var useGpu = !(SystemInfo.graphicsDeviceType == GraphicsDeviceType.Direct3D12 || SystemInfo.graphicsDeviceType == GraphicsDeviceType.Direct3D11);
                // ログのマスタースイッチ: Unity 側 (CefLog) と Rust 側 (client/server)
                // の両方を _enableLog 一つで制御する。
                CefLog.Enabled = _enableLog;
                CefRuntime.Init(enableLog: _enableLog);
                _browser = new Browser(_currentWidth, _currentHeight, _url);

                // PlayerLoop に EarlyUpdate / PostLateUpdate の hook を挿入。
                // EarlyUpdate 末尾で「入力送信 + BeginFrame」、PostLateUpdate 内の描画発行前
                // (Canvas 更新前) で「recv + 短い 0F 待ち」を行うことで、入力遅延 0 +
                // 描画遅延 0F (同フレーム present 反映) を目指す。
                s_instance = this;
                InstallPlayerLoopHooks();
                _playerLoopHooked = true;

                // 共通: macOS は Mach port 経由の IOSurface、Windows は D3D11 共有テクスチャ。
                // Init() がサーバーを起動し接続を行うため、その後にチェック。
                _useAcceleratedPaint = Browser.IsAcceleratedConnected();
                if (_enableLog) CefLog.Log($"[CefUnity] Initialized ({_currentWidth}x{_currentHeight}), acceleratedPaint={_useAcceleratedPaint}");
                SetupImeProxy();
                // Native レンダラは FMOD ミキサを使わないので DSP バッファ変更は不要。
                if (_audioRenderer == AudioRendererMode.UnityMixer) ApplyAudioDspBufferSize();
                SetupAudioOutput();
            }
            catch (Exception e)
            {
                CefLog.LogError($"[CefUnity] Init failed: {e}");
            }
        }

        private void Update()
        {
            CefRuntime.Pump();
            // 入力処理 + BeginFrame 発行は PlayerLoop の EarlyUpdate 末尾 (OnEarlyUpdateLast)
            // で行うため、ここからは削除した。MonoBehaviour.Update の役割は Pump と診断のみ。

#if UNITY_EDITOR || DEVELOPMENT_BUILD
            // 開発トグル: temp ファイルで testufo 遷移 + 擬似ゲーム負荷 (8ms 空回し)。
            var tmpDir = System.IO.Path.GetTempPath();
            if (!_navTestDone && System.IO.File.Exists(System.IO.Path.Combine(tmpDir, "cef_load_testufo")))
            {
                _navTestDone = true;
                LoadUrl("https://testufo.com/mouserate");
            }
            // 計測用 (一時): cef_load_url にファイル内容の URL を書くとそこへ遷移する。
            // 音声テスト (440Hz トーンの data: URI 等) を実行中の PlayMode へ渡すために使う。
            // Time.frameCount > 60: 初期 URL のナビゲーションと競合すると LoadUrl が
            // 負けて遷移しないことがあるため、初期ロードが落ち着いてから発火させる。
            var navUrlFile = System.IO.Path.Combine(tmpDir, "cef_load_url");
            if (!_audioTestDone && Time.frameCount > 60 && System.IO.File.Exists(navUrlFile))
            {
                _audioTestDone = true;
                var u = System.IO.File.ReadAllText(navUrlFile).Trim();
                if (!string.IsNullOrEmpty(u)) LoadUrl(u);
            }
            if (System.IO.File.Exists(System.IO.Path.Combine(tmpDir, "cef_fake_work")))
            {
                var until = Time.realtimeSinceStartup + 0.008f;
                while (Time.realtimeSinceStartup < until) { }
            }
#endif

            // 機構1 計装: フレーム時間 (present 間隔) の分布を毎フレーム集計。
            var ft = Time.unscaledDeltaTime;
            _ftSum += ft;
            _ftSumSq += (double)ft * ft;
            _ftCount++;
            if (ft > _ftMax) _ftMax = ft;
            if (ft > 0.018) _ftOver18++;
            if (ft > 0.020) _ftOver20++;
            if (ft > 0.025) _ftOver25++;

#if UNITY_EDITOR || DEVELOPMENT_BUILD
            // 開発トグル: cef_perf_probe がある間、毎フレーム記録し 30 フレームごとに CSV 追記。
            if (System.IO.File.Exists(System.IO.Path.Combine(tmpDir, "cef_perf_probe")))
            {
                long afiNow = _useAcceleratedPaint && _browser != null ? (long)_browser.PeekAccelFrameId() : 0;
                _perfLog.Add($"{Time.frameCount},{ft * 1000f:F2},{afiNow},{_frameSentDy}");
                _frameSentDy = 0;
                if (_perfLog.Count >= 30)
                {
                    try
                    {
                        System.IO.File.AppendAllText(
                            System.IO.Path.Combine(tmpDir, "cef_perf.csv"),
                            string.Join("\n", _perfLog) + "\n");
                    }
                    catch { }
                    _perfLog.Clear();
                }
            }
#endif

            _diagTimer += Time.deltaTime;
            if (_diagTimer >= 2f)
            {
                _diagTimer = 0f;

                if (_enableLog)
                {
                    var paintCount = NativeMethods.cef_unity_get_paint_count();
                    var pumpCount = NativeMethods.cef_unity_get_pump_count();
                    // afi = accel_frame_id (server が Mach 送信を完了した paint の累積数)。
                    // 2 秒窓の増分が「CEF が Unity へ届けた paint レート」= CEF 出力 fps。
                    // paint= は software 経路のカウンタなので GPU 経路では常に 0。
                    var afi = _useAcceleratedPaint ? _browser.PeekAccelFrameId() : 0;
                    CefLog.Log($"[CefUnity] diag: paint={paintCount} pump={pumpCount} afi={afi}");
                    var logs = CefRuntime.GetLogs();
                    foreach (var line in logs)
                        CefLog.Log($"[CefServer] {line}");

                    if (_delaySampleCount > 0)
                    {
                        var avg = (float)_delaySumFrames / _delaySampleCount;
                        var sb = new StringBuilder();
                        sb.Append($"[CefUnity] end-to-end frame delay (n={_delaySampleCount}): avg={avg:F2} min={_delayMinFrames} max={_delayMaxFrames} buckets=[");
                        for (int i = 0; i < _delayBuckets.Length; i++)
                        {
                            if (i > 0) sb.Append(' ');
                            sb.Append($"{i}{(i == _delayBuckets.Length - 1 ? "+" : "")}:{_delayBuckets[i]}");
                        }
                        sb.Append(']');
                        CefLog.Log(sb.ToString());

                        // 検証メトリクス: PostLateUpdate hook での取得統計
                        CefLog.Log($"[CefUnity] verify: PostLateUpdate={_postLateUpdateInvokeCount} recv_ok={_gotInPostLateUpdateCount} recv_fail={_recvFailCount}");
                        var sb2 = new StringBuilder("[CefUnity] verify samples (fc, paint_fc, delta):");
                        foreach (var s in _recentSamples)
                            sb2.Append($" ({s.fc},{s.pf},{s.delta})");
                        CefLog.Log(sb2.ToString());

                        _delaySampleCount = 0;
                        _delaySumFrames = 0;
                        _delayMaxFrames = 0;
                        _delayMinFrames = int.MaxValue;
                        for (int i = 0; i < _delayBuckets.Length; i++) _delayBuckets[i] = 0;
                        _postLateUpdateInvokeCount = 0;
                        _gotInPostLateUpdateCount = 0;
                        _recvFailCount = 0;
                        _recentSamples.Clear();
                    }

                    // 0F 待ち専用メトリクス (fresh=新paint取得 / fallback=届かず1F / idle=待ちスキップ)。
                    if (_zeroFrameWaitMs > 0f && _useAcceleratedPaint)
                    {
                        var dpActive = _dpFreshCount + _dpFallbackCount;
                        var blockAvg = dpActive > 0 ? _dpBlockSumMs / dpActive : 0.0;
                        CefLog.Log($"[CefUnity] 0F-wait: fresh={_dpFreshCount} fallback(1F)={_dpFallbackCount} idle={_dpIdleCount} block_avg={blockAvg:F2}ms block_max={_dpBlockMaxMs:F2}ms");
                        _dpFreshCount = 0;
                        _dpFallbackCount = 0;
                        _dpIdleCount = 0;
                        _dpBlockSumMs = 0;
                        _dpBlockMaxMs = 0;
                    }

                    // jitter 計装: 機構1 (フレーム時間=present 間隔) と 機構2 (content 更新間隔)。
                    // 0F 待ち ON/OFF どちらでも出力して比較できるようにする。
                    var dp = (_zeroFrameWaitMs > 0f && _useAcceleratedPaint) ? "ON " : "OFF";
                    var ftMean = _ftCount > 0 ? _ftSum / _ftCount : 0.0;
                    var ftStd = _ftCount > 0 ? Math.Sqrt(Math.Max(0, _ftSumSq / _ftCount - ftMean * ftMean)) : 0.0;
                    var ciMean = _ciCount > 0 ? _ciSum / _ciCount : 0.0;
                    var ciStd = _ciCount > 0 ? Math.Sqrt(Math.Max(0, _ciSumSq / _ciCount - ciMean * ciMean)) : 0.0;
                    CefLog.Log(
                        $"[CefUnity] jitter dp={dp}: " +
                        $"frame(n={_ftCount}) mean={ftMean * 1000:F2}ms std={ftStd * 1000:F2}ms max={_ftMax * 1000:F1}ms over18/20/25={_ftOver18}/{_ftOver20}/{_ftOver25} | " +
                        $"content(n={_ciCount}) mean={ciMean * 1000:F2}ms std={ciStd * 1000:F2}ms max={_ciMax * 1000:F1}ms");
                    _ftSum = _ftSumSq = _ftMax = 0; _ftCount = _ftOver18 = _ftOver20 = _ftOver25 = 0;
                    _ciSum = _ciSumSq = _ciMax = 0; _ciCount = 0;
                }
            }

            // 入力処理 + BeginFrame 発行は EarlyUpdate hook へ移動。
            // テクスチャ取得は PostLateUpdate hook へ移動。
            // → MonoBehaviour.Update / LateUpdate は Pump + 診断ログのみを担当。
        }

        /// <summary>同 Unity フレーム内で 1 回だけ取得試行 (spin なし、block なし)。</summary>
        /// <returns>このフレームで初めて取得成功した時のみ true。それ以外は false。</returns>
        private bool TryUpdateTextureOnce()
        {
            if (_browser == null) return false;
            if (_textureUpdatedFrame == Time.frameCount) return false;
            if (!_useAcceleratedPaint)
            {
                UpdateTextureSoftware();
                _textureUpdatedFrame = Time.frameCount;
                return true;
            }
            // accelerated paint: 取得できた時だけフラグを立てる
            return TryUpdateTextureAcceleratedNonBlocking();
        }

        public void LoadUrl(string url)
        {
            // グライド途中の残距離を新ページへ流し込まない。
            _scrollSmoother.Reset();
            _browser.LoadUrl(url);
        }

        private void OnDestroy()
        {
            if (_playerLoopHooked)
            {
                UninstallPlayerLoopHooks();
                _playerLoopHooked = false;
            }
            if (s_instance == this) s_instance = null;

            // 音声出力を先に止めてから browser を破棄する (破棄済みハンドルへのアクセス防止)。
            if (_audioOutput != null)
            {
                _audioOutput.Browser = null;
                _audioOutput.enabled = false;
            }

            if (_nativeAudio != null)
            {
                // enabled=false の OnDisable で StopNativeAudio が走る (dispose 前に停止)。
                // 仮に順序が崩れても Rust 側 destroy_browser の先頭で voice は停止される。
                _nativeAudio.enabled = false;
                _nativeAudio.Browser = null;
            }

            _browser?.Dispose();
            _browser = null;

            if (_lastAccelTexPtr != IntPtr.Zero)
            {
                Browser.ReleaseMetalTexture(_lastAccelTexPtr);
                _lastAccelTexPtr = IntPtr.Zero;
            }

            if (_texture != null)
            {
                Destroy(_texture);
                _texture = null;
            }

            CefRuntime.Shutdown();
            if (_enableLog) CefLog.Log("[CefUnity] Shutdown");
        }

        // -----------------------------------------------------------------------
        // PlayerLoop hooks
        // -----------------------------------------------------------------------

        /// <summary>
        /// EarlyUpdate の末尾に挿入される hook。
        /// Unity の Input は EarlyUpdate 内の `UpdateInputManager` / `NewInputUpdate`
        /// で更新されるので、ここに差し込めば Input は既に取得済み。
        /// 入力を CEF へ送って BeginFrame#1 を発行 → renderer に当該フレームの内容生成を
        /// 開始させる。flush (BeginFrame#2) は PostLateUpdate で発行する。
        /// </summary>
        private static void OnEarlyUpdateLast()
        {
            var self = s_instance;
            if (self == null || self._browser == null) return;
            self._inputSentThisFrame = false;
            self.CheckScreenResize();
            self.HandleMouseInput();
#if UNITY_EDITOR || DEVELOPMENT_BUILD
            // 開発トグル: cef_scroll_test が在るあいだ、実ユーザーのホイール操作を模して
            // 毎フレーム ±60px のスクロールを注入する (3 秒ごとに方向反転)。実ホイールと
            // 同じ EarlyUpdate 内で送ること (Update 内だと _inputSentThisFrame がこの hook
            // 冒頭のリセットで消え、連続入力判定・アクティブ判定に乗らない)。
            if (System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_scroll_test")))
            {
                var dir = ((int)(Time.realtimeSinceStartup / 3f)) % 2 == 0 ? -1 : 1;
                self._browser.SendMouseWheel(self._currentWidth / 2, self._currentHeight / 2, 0, dir * 60);
                self._inputSentThisFrame = true;
            }
            // 開発トグル: cef_scroll_slow が在る間、毎フレーム正確に -10px の均一スクロールを注入。
            // 「数学的に完璧に均一な入力」でもカクつくか(=パイプライン位相ビート)を切り分ける。
            if (System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_scroll_slow")))
            {
                self._browser.SendMouseWheel(self._currentWidth / 2, self._currentHeight / 2, 0, -10);
                self._frameSentDy = -10;
                self._inputSentThisFrame = true;
            }
#endif
            // スクロール平滑の排出。BeginFrame#1 の前なので同フレームの paint に乗る。
            self.TickScrollSmoother();
            self.UpdateCompositionCursorPos();
            self.HandleImeInput();
            self.HandleKeyboardInput();
            // 連続入力カウンタ更新 (入力ハンドラ群の後なので _inputSentThisFrame は確定済み)。
            self._consecutiveInputFrames = self._inputSentThisFrame
                ? Math.Min(self._consecutiveInputFrames + 1, 1000)
                : 0;
            // BeginFrame#1 直前の paint カウンタと時刻を記録 (recv 側の増分検知・待ち基準)。
            if (self._useAcceleratedPaint)
                self._afiAtBf1 = self._browser.PeekAccelFrameId();
            self._bf1Time = Time.realtimeSinceStartup;
            // BeginFrame#1: renderer に「このフレームの入力を反映した内容」を作らせる。
            self._browser.SendExternalBeginFrame((ulong)Time.frameCount);
        }

        /// <summary>
        /// PostLateUpdate 内の描画発行前 (Canvas 更新 = PlayerUpdateCanvases より前) に
        /// 挿入される hook。全 MonoBehaviour LateUpdate / Animator 完了直後。ここで受信した
        /// テクスチャは同フレームの FinishFrameRendering → present に乗る (0F 条件)。
        /// リスト末尾への Append は PresentAfterDraw より後になり反映が次フレームへずれる
        /// (実画面 +1F) ため不可。
        /// </summary>
        private static void OnPostLateUpdateRecv()
        {
            var self = s_instance;
            if (self == null || self._browser == null) return;
            self._postLateUpdateInvokeCount++;
            self.RecvBeforeRender();
        }

        /// <summary>
        /// 描画発行前の recv 本体。server-side flush の結果 (accel_frame_id 増分) を
        /// _zeroFrameWaitMs (BF#1 からの経過時間 cap) まで待ち、届いた最新 paint を
        /// 同フレームの present に乗せる (0F)。ゲーム処理が重いフレームではここへの到達が
        /// 遅く cap を過ぎているため自動的に待ちゼロ (flush 結果は自然に到着済み)。
        /// デッドラインまでに届かなければ従来通り 1F フォールバック。待ちは SHM カウンタの
        /// busy-wait のみで IPC を発行しない (旧 client-side double-pump の reflush による
        /// IPC フラッディング → 46ms ブロック問題は構造的に発生しない)。
        /// </summary>
        private void RecvBeforeRender()
        {
            // software 経路 / 待ち無効時は従来のノンブロッキング受信のみ。
            if (!_useAcceleratedPaint || _zeroFrameWaitMs <= 0f)
            {
                if (TryUpdateTextureOnce()) OnFreshPaint();
                else OnNoPaint();
                return;
            }

            var blockStart = Time.realtimeSinceStartup;

            // プローブ判定: 入力を送った or 直近 1 秒以内にページが動いていた時だけ待つ。
            // 完全静止ページでは paint 自体が来ないため待たない (ブロック 0)。
            var expectPaint = _inputSentThisFrame || _framesSinceFreshPaint < ProbeWindowFrames;
            if (!expectPaint)
            {
                if (TryUpdateTextureOnce()) OnFreshPaint();
                else OnNoPaint();
                _dpIdleCount++;
                return;
            }

            // サーバーの damage streak 抑止 (flush 無し) をクライアント側で推定。
            // 抑止中 = 連続描画中はコンテンツがどのみち 1F (BF#1 の即時 draw は前フレーム
            // 内容) なので、待っても鮮度は上がらない。さらに busy-wait の CPU が CEF
            // プロセス群の paint 生成と競合し、スクロール中の供給を 2-7% 落とす (実測:
            // 待ち OFF 99-100% / ON 92-98%)。よって抑止中・連続入力中は待ちをスキップして
            // CPU を返し、ノンブロッキング受信のみ行う (待ち OFF と同じ挙動 = 供給 ~100%)。
            if (_streakScore >= StreakScoreSuppress || _consecutiveInputFrames >= SustainedInputFrames)
            {
                if (TryUpdateTextureOnce()) { OnFreshPaint(); _dpFreshCount++; }
                else { OnNoPaint(); _dpFallbackCount++; }
                return;
            }

            var deadline = _bf1Time + _zeroFrameWaitMs * 0.001f;
            var freshMinTime = _bf1Time + FreshPaintMinDelayMs * 0.001f;
            var noDamageGiveUp = _bf1Time + NoDamageGiveUpMs * 0.001f;
            var earlyAdopt = _bf1Time + EarlyPaintAdoptMs * 0.001f;

            var baseline = _afiAtBf1;
            var sawEarlyPaint = false;
            while (true)
            {
                var now = Time.realtimeSinceStartup;
                if (now >= deadline) break;
                var afi = _browser.PeekAccelFrameId();
                if (afi != baseline)
                {
                    // 増分検知。flush#1 の draw があり得る時刻 (freshMinTime) より前の増分は
                    // BF#1 由来 stale (#A) とみなして読み捨て、fresh (#B) を待ち続ける。
                    if (now >= freshMinTime) break;
                    baseline = afi;
                    sawEarlyPaint = true;
                    continue;
                }
                if (sawEarlyPaint)
                {
                    // 早着 (#A) は届いたが #B が来ない: タイマー発火由来の damage を #A が
                    // 消費したケース。#B の標準到着時刻を跨いだら #A を採用して抜ける。
                    if (now >= earlyAdopt) break;
                }
                else if (now >= noDamageGiveUp)
                {
                    // 増分ゼロのまま判定時刻超え = このフレームに damage なし。
                    break;
                }
                // Peek (FFI + SHM read) のフル回転を避けて CPU/メモリバス圧を下げる。
                // SpinWait はデスケジュールされない (Thread.Sleep(1) は macOS で 10ms+
                // オーバースリープするため使用不可)。時間精度は ~µs で十分。
                System.Threading.Thread.SpinWait(64);
            }

            // 増分で抜けた場合はその paint を、デッドライン切れでも直前に届いた分があれば拾う
            // (TryRecv は queue を drain して最新を返すため、どちらでも最新が取れる)。
            if (TryUpdateTextureOnce()) { OnFreshPaint(); _dpFreshCount++; }
            else { OnNoPaint(); _dpFallbackCount++; }

            var blockMs = (Time.realtimeSinceStartup - blockStart) * 1000.0;
            _dpBlockSumMs += blockMs;
            if (blockMs > _dpBlockMaxMs) _dpBlockMaxMs = blockMs;
        }

        /// <summary>recv 成功時の共通処理 (verify 計装 + activity/streak カウンタ更新)。</summary>
        private void OnFreshPaint()
        {
            _gotInPostLateUpdateCount++;
            _framesSinceFreshPaint = 0;
            if (_streakScore < StreakScoreMax) _streakScore++;
            RecordContentInterval();
        }

        /// <summary>recv 失敗 (新 paint なし) 時の共通処理。</summary>
        private void OnNoPaint()
        {
            if (_textureUpdatedFrame != Time.frameCount) _recvFailCount++;
            if (_framesSinceFreshPaint != int.MaxValue) _framesSinceFreshPaint++;
            _streakScore = Math.Max(0, _streakScore - 2);
        }

        /// <summary>機構2 計装: 新テクスチャを適用した実時刻の連続差 (= コンテンツがカバーする
        /// 実時間幅) を集計する。Chromium のスクロール曲線はこの実時刻でサンプルされるため、
        /// この間隔のジッタが見かけのスクロール速度のジッタ (judder) に直結する。</summary>
        private void RecordContentInterval()
        {
            var nowRt = Time.realtimeSinceStartup;
            if (_lastFreshRealtime >= 0f)
            {
                double ci = nowRt - _lastFreshRealtime;
                _ciSum += ci;
                _ciSumSq += ci * ci;
                _ciCount++;
                if (ci > _ciMax) _ciMax = ci;
            }
            _lastFreshRealtime = nowRt;
        }

        // recv フックの挿入先アンカー (優先順)。受信テクスチャを同フレームの present に
        // 乗せるには Canvas ジオメトリ更新 (PlayerUpdateCanvases) より前に RawImage の
        // テクスチャを差し替える必要がある。Unity 6000.3 では描画発行 (FinishFrameRendering)
        // と present (PresentAfterDraw) が PostLateUpdate 内にあるため、リスト末尾への
        // Append は present より後 = 反映が次フレームの描画にずれる (実画面 +1F)。
        private static readonly Type[] RecvAnchorTypes =
        {
            typeof(PostLateUpdate.PlayerUpdateCanvases),
            typeof(PostLateUpdate.PlayerEmitCanvasGeometry),
            typeof(PostLateUpdate.FinishFrameRendering),
        };

        private static void InstallPlayerLoopHooks()
        {
            var loop = PlayerLoop.GetCurrentPlayerLoop();
            for (int i = 0; i < loop.subSystemList.Length; i++)
            {
                if (loop.subSystemList[i].type == typeof(EarlyUpdate))
                    loop.subSystemList[i] = AppendSubsystem(loop.subSystemList[i], typeof(CefUnityEarlyUpdate), OnEarlyUpdateLast);
                else if (loop.subSystemList[i].type == typeof(PostLateUpdate))
                    loop.subSystemList[i] = InsertSubsystemBeforeAnchor(loop.subSystemList[i], RecvAnchorTypes, typeof(CefUnityPostLateUpdate), OnPostLateUpdateRecv);
            }
            PlayerLoop.SetPlayerLoop(loop);
        }

        private static void UninstallPlayerLoopHooks()
        {
            var loop = PlayerLoop.GetCurrentPlayerLoop();
            for (int i = 0; i < loop.subSystemList.Length; i++)
            {
                if (loop.subSystemList[i].type == typeof(EarlyUpdate))
                    loop.subSystemList[i] = RemoveSubsystem(loop.subSystemList[i], typeof(CefUnityEarlyUpdate));
                else if (loop.subSystemList[i].type == typeof(PostLateUpdate))
                    loop.subSystemList[i] = RemoveSubsystem(loop.subSystemList[i], typeof(CefUnityPostLateUpdate));
            }
            PlayerLoop.SetPlayerLoop(loop);
        }

        private static PlayerLoopSystem AppendSubsystem(PlayerLoopSystem parent, Type marker, PlayerLoopSystem.UpdateFunction update)
        {
            var oldList = parent.subSystemList ?? Array.Empty<PlayerLoopSystem>();
            // 既に同 marker が入っていたら何もしない (二重 install 防止)
            for (int i = 0; i < oldList.Length; i++)
                if (oldList[i].type == marker) return parent;
            var newList = new PlayerLoopSystem[oldList.Length + 1];
            Array.Copy(oldList, newList, oldList.Length);
            newList[oldList.Length] = new PlayerLoopSystem { type = marker, updateDelegate = update };
            parent.subSystemList = newList;
            return parent;
        }

        /// <summary>anchors のうち最初に見つかったサブシステムの直前に marker を挿入する。
        /// どのアンカーも見つからない場合は先頭に挿入する (描画発行前であることを優先)。</summary>
        private static PlayerLoopSystem InsertSubsystemBeforeAnchor(PlayerLoopSystem parent, Type[] anchors, Type marker, PlayerLoopSystem.UpdateFunction update)
        {
            var oldList = parent.subSystemList ?? Array.Empty<PlayerLoopSystem>();
            // 既に同 marker が入っていたら何もしない (二重 install 防止)
            for (int i = 0; i < oldList.Length; i++)
                if (oldList[i].type == marker) return parent;

            int insertAt = -1;
            foreach (var anchor in anchors)
            {
                insertAt = Array.FindIndex(oldList, s => s.type == anchor);
                if (insertAt >= 0) break;
            }
            if (insertAt < 0)
            {
                CefLog.LogError("[CefUnity] recv anchor subsystem not found in PostLateUpdate; inserting at head");
                insertAt = 0;
            }

            var newList = new PlayerLoopSystem[oldList.Length + 1];
            Array.Copy(oldList, newList, insertAt);
            newList[insertAt] = new PlayerLoopSystem { type = marker, updateDelegate = update };
            Array.Copy(oldList, insertAt, newList, insertAt + 1, oldList.Length - insertAt);
            parent.subSystemList = newList;
            return parent;
        }

        private static PlayerLoopSystem RemoveSubsystem(PlayerLoopSystem parent, Type marker)
        {
            var oldList = parent.subSystemList;
            if (oldList == null) return parent;
            var idx = Array.FindIndex(oldList, s => s.type == marker);
            if (idx < 0) return parent;
            var newList = new PlayerLoopSystem[oldList.Length - 1];
            Array.Copy(oldList, 0, newList, 0, idx);
            Array.Copy(oldList, idx + 1, newList, idx, oldList.Length - idx - 1);
            parent.subSystemList = newList;
            return parent;
        }

        // -----------------------------------------------------------------------
        // IME
        // -----------------------------------------------------------------------
        private void SetupImeProxy()
        {
            Input.imeCompositionMode = IMECompositionMode.On;
        }

        // -----------------------------------------------------------------------
        // Audio
        // -----------------------------------------------------------------------
        /// <summary>
        ///     CEF の音声を Unity 側で再生するために CefAudioOutput を用意し、
        ///     現在のブラウザを割り当てる。
        /// </summary>
        private void SetupAudioOutput()
        {
            if (!_enableAudio || _browser == null) return;

            if (_audioRenderer == AudioRendererMode.Native)
            {
                _nativeAudio = GetComponent<CefNativeAudio>();
                if (_nativeAudio == null) _nativeAudio = gameObject.AddComponent<CefNativeAudio>();
                _nativeAudio.Browser = _browser;
            }
            else
            {
                _audioOutput = GetComponent<CefAudioOutput>();
                if (_audioOutput == null) _audioOutput = gameObject.AddComponent<CefAudioOutput>();
                _audioOutput.Browser = _browser;
            }
        }

        /// <summary>
        ///     音声出力の DSP バッファサイズを実行時に適用して遅延 (⑧ Unity DSP ミキサ) を詰める。
        ///     ProjectSettings/Audio の DSP Buffer Size と同値だが、エディタ実行中はプロジェクト設定の
        ///     変更が起動時にしか反映されないため、ここで <see cref="AudioSettings.Reset" /> して確実に適用する。
        ///     音声シンク生成前 (SetupAudioOutput より前) に呼ぶこと (Reset は全 AudioSource を停止するため)。
        /// </summary>
        private void ApplyAudioDspBufferSize()
        {
            if (!_enableAudio || _audioDspBufferSize <= 0) return;

            var cfg = AudioSettings.GetConfiguration();
            if (cfg.dspBufferSize == _audioDspBufferSize) return;

            int before = cfg.dspBufferSize;
            cfg.dspBufferSize = _audioDspBufferSize;
            if (AudioSettings.Reset(cfg))
            {
                if (_enableLog) CefLog.Log($"[CefUnity] DSP buffer {before} -> {_audioDspBufferSize}");
            }
            else
            {
                CefLog.LogError($"[CefUnity] AudioSettings.Reset({_audioDspBufferSize}) failed");
            }
        }

        private void HandleImeInput()
        {
            if (_browser == null) return;

            var comp = Input.compositionString;
            var input = Input.inputString;

            if (!string.IsNullOrEmpty(comp))
            {
                // IME が暗黙的に確定して新しい composition を開始した場合を検出
                // (例: "嗚呼亜" → Enter なしで次の文字 → "あ")
                // この場合 Input.inputString に確定テキストが入っている
                if (_imeActive && !string.IsNullOrEmpty(input))
                {
                    var commitSb = new StringBuilder();
                    foreach (var c in input)
                        if (!char.IsControl(c))
                            commitSb.Append(c);
                    if (commitSb.Length > 0)
                    {
                        var commitText = commitSb.ToString();
                        _browser.ImeCommitText(commitText);
                    }
                }

                // composition 開始/変更
                _browser.ImeSetComposition(comp, (uint)comp.Length, (uint)comp.Length);
                _imeActive = true;
                _imeSuppressKeys = true;
                _inputSentThisFrame = true;
            }
            else if (_imeActive)
            {
                _inputSentThisFrame = true;
                // composition 終了 (非空 → 空に変化)
                var committed = false;
                foreach (var c in input)
                    if (!char.IsControl(c))
                    {
                        committed = true;
                        break;
                    }

                if (committed)
                {
                    // 制御文字を除いた確定テキストを取得
                    var sb = new StringBuilder();
                    foreach (var c in input)
                        if (!char.IsControl(c))
                            sb.Append(c);
                    var text = sb.ToString();
                    _browser.ImeCommitText(text);
                }
                else
                {
                    _browser.ImeCancelComposition();
                }

                _imeActive = false;
                _imeSuppressKeys = true; // 終了フレームもキー抑制
            }
            else
            {
                // 通常状態: 次フレームからキー送信を許可
                _imeSuppressKeys = false;
            }
        }

        private void UpdateCompositionCursorPos()
        {
            if (_browser == null || _rawImage == null) return;

            _browser.GetImeCaret(out var cx, out var cy, out var cw, out var ch);

            // まだキャレット位置が報告されていない場合はスキップ
            if (cx == 0 && cy == 0 && cw == 0 && ch == 0) return;

            var rt = _rawImage.rectTransform;
            var rect = rt.rect;

            var nx = (float)cx / _currentWidth;
            var ny = (float)(cy + ch) / _currentHeight;

            var localX = rect.x + nx * rect.width;
            var localY = rect.y + (1f - ny) * rect.height;
            var localPoint = new Vector3(localX, localY, 0);

            var canvas = _rawImage.canvas;
            var cam = canvas.renderMode == RenderMode.ScreenSpaceOverlay ? null : canvas.worldCamera;
            var worldPoint = rt.TransformPoint(localPoint);
            var screenPos = RectTransformUtility.WorldToScreenPoint(cam, worldPoint);

#if UNITY_EDITOR
            // Editor の Game View Scale 補正: Scale 2x では表示が2倍ズームされるため
            // compositionCursorPos もスケール倍する必要がある
            var scale = GetEditorGameViewScale();
            screenPos *= scale;
#endif

            Input.compositionCursorPos = screenPos;
        }

        private uint GetCefModifiers()
        {
            uint m = 0;
            if (Input.GetKey(KeyCode.LeftShift) || Input.GetKey(KeyCode.RightShift)) m |= (uint)CefEventFlags.ShiftDown;
            if (Input.GetKey(KeyCode.LeftControl) || Input.GetKey(KeyCode.RightControl)) m |= (uint)CefEventFlags.ControlDown;
            if (Input.GetKey(KeyCode.LeftAlt) || Input.GetKey(KeyCode.RightAlt)) m |= (uint)CefEventFlags.AltDown;
            if (Input.GetKey(KeyCode.LeftCommand) || Input.GetKey(KeyCode.RightCommand)) m |= (uint)CefEventFlags.CommandDown;
            if (Input.GetMouseButton(0)) m |= (uint)CefEventFlags.LeftMouseDown;
            if (Input.GetMouseButton(1)) m |= (uint)CefEventFlags.RightMouseDown;
            if (Input.GetMouseButton(2)) m |= (uint)CefEventFlags.MiddleMouseDown;
            return m;
        }

        // -----------------------------------------------------------------------
        // Mouse
        // -----------------------------------------------------------------------
        private void HandleMouseInput()
        {
            if (_browser == null || _rawImage == null) return;

            if (!TryGetBrowserCoord(out var bx, out var by))
                return;

            var mods = GetCefModifiers();

            if (bx != _lastMouseX || by != _lastMouseY)
            {
                _lastMouseX = bx;
                _lastMouseY = by;
                _browser.SendMouseMove(bx, by, mods);
                _inputSentThisFrame = true;
            }

            HandleButton(bx, by, 0, MouseButton.Left, mods);
            HandleButton(bx, by, 1, MouseButton.Right, mods);
            HandleButton(bx, by, 2, MouseButton.Middle, mods);

            var scroll = Input.mouseScrollDelta;
            if (scroll.y != 0f || scroll.x != 0f)
            {
                // ステップ→ピクセル変換。_resolutionScale で view (CSS px) が広がった分も
                // 掛けて、画面上の体感スクロール速度を scale に依らず一定に保つ
                // (マウス座標は既に scale 込みの view 座標へ変換している)。
                // 送信は即時ではなく ScrollSmoother へ蓄積し、OnEarlyUpdateLast の
                // TickScrollSmoother が毎フレーム均一化して排出する。
                _scrollSmoother.AddInput(
                    scroll.x * WheelPixelsPerStep * _resolutionScale,
                    scroll.y * WheelPixelsPerStep * _resolutionScale);
            }
        }

        /// <summary>
        /// ScrollSmoother の 1 フレーム分排出。蓄積された wheel 残距離を指数追従で
        /// 均一化して SendMouseWheel する (per-frame スクロール量ジッターの平滑)。
        /// HandleMouseInput の外に置くのは、カーソルがブラウザ外に出ても
        /// (TryGetBrowserCoord 失敗でも) グライド途中の排出を最後の有効座標で
        /// 継続するため。
        /// </summary>
        private void TickScrollSmoother()
        {
            if (_browser == null || !_scrollSmoother.IsActive) return;
            _scrollSmoother.Tick(Time.unscaledDeltaTime, ScrollSmoothTau, out var dx, out var dy);
            if (dx == 0 && dy == 0) return;
            // 最後の有効マウス座標で送る。まだ一度も動いていなければ画面中央。
            var bx = _lastMouseX >= 0 ? _lastMouseX : _currentWidth / 2;
            var by = _lastMouseY >= 0 ? _lastMouseY : _currentHeight / 2;
            _browser.SendMouseWheel(bx, by, dx, dy, GetCefModifiers());
            _inputSentThisFrame = true;
#if UNITY_EDITOR || DEVELOPMENT_BUILD
            _frameSentDy = dy; // 分析用: 平滑後の実送信量
#endif
        }

        private void HandleButton(int bx, int by, int unityButton, MouseButton cefButton, uint mods)
        {
            if (Input.GetMouseButtonDown(unityButton))
            {
                if (unityButton == 0)
                {
                    var now = Time.unscaledTime;
                    if (now - _lastClickTime < DoubleClickTime
                        && Math.Abs(bx - _lastClickX) <= DoubleClickDistance
                        && Math.Abs(by - _lastClickY) <= DoubleClickDistance)
                        _clickCount = _clickCount >= 3 ? 1 : _clickCount + 1;
                    else
                        _clickCount = 1;
                    _lastClickTime = now;
                    _lastClickX = bx;
                    _lastClickY = by;
                }
                else
                {
                    _clickCount = 1;
                }

                _browser.SendMouseClick(bx, by, cefButton, false, _clickCount, mods);
                _inputSentThisFrame = true;
            }

            if (Input.GetMouseButtonUp(unityButton))
            {
                _browser.SendMouseClick(bx, by, cefButton, true, _clickCount, mods);
                _inputSentThisFrame = true;
            }
        }

        /// <summary>
        ///     スクリーン上のマウス座標を RawImage のローカル座標経由でブラウザ座標 (0..width, 0..height) に変換する。
        ///     RawImage 外なら false を返す。
        /// </summary>
        private bool TryGetBrowserCoord(out int bx, out int by)
        {
            bx = by = 0;
            var rt = _rawImage.rectTransform;

            // Canvas 内の Camera を取得（Overlay なら null）
            var canvas = _rawImage.canvas;
            var cam = canvas.renderMode == RenderMode.ScreenSpaceOverlay ? null : canvas.worldCamera;

            if (!RectTransformUtility.ScreenPointToLocalPointInRectangle(
                    rt, Input.mousePosition, cam, out var local))
                return false;

            var rect = rt.rect;
            // rect 内の 0..1 正規化座標
            var nx = (local.x - rect.x) / rect.width;
            var ny = (local.y - rect.y) / rect.height;

            if (nx < 0f || nx > 1f || ny < 0f || ny > 1f)
                return false;

            // uvRect (0,1,1,-1) で Y 反転しているので補正
            ny = 1f - ny;

            bx = Mathf.Clamp((int)(nx * _currentWidth), 0, _currentWidth - 1);
            by = Mathf.Clamp((int)(ny * _currentHeight), 0, _currentHeight - 1);
            return true;
        }

        private void HandleKeyboardInput()
        {
            if (_browser == null) return;

            // IME composition 中・終了直後は全キー入力を抑制 (OS の IME が処理する)
            if (_imeSuppressKeys) return;

            var mods = GetCefModifiers();
            var cmd = (mods & (uint)CefEventFlags.CommandDown) != 0;
            var ctrl = (mods & (uint)CefEventFlags.ControlDown) != 0;
            var alt = (mods & (uint)CefEventFlags.AltDown) != 0;

            // 1) 印字可能文字 — Input.inputString 経由 (RAWKEYDOWN + CHAR + KEYUP)
            //    IME 変換中・commit 直後は抑制（preedit/commit は別経路で CEF に送信される）
            if (string.IsNullOrEmpty(Input.compositionString))
                foreach (var c in Input.inputString)
                {
                    if (char.IsControl(c)) continue;
                    // 英数/かなキーが生成する偽スペースをフィルタ
                    if (c == ' ' && !Input.GetKey(KeyCode.Space)) continue;
                    _browser.SendCharEvent(c, mods);
                    _inputSentThisFrame = true;
                }

            // 2) macOS キー変換: CEF OSR は interpretKeyEvents: パイプラインが無いため手動変換
            //    Cmd+Arrow → Home/End, Alt+Arrow → Ctrl+Arrow (単語移動)
            //    Shift が併用された場合は選択操作になる (ShiftDown は baseMods に残る)
            var suppressHArrows = cmd || alt;
            var suppressVArrows = cmd;
            if (cmd)
            {
                var baseMods = mods & ~(uint)CefEventFlags.CommandDown;
                SendKeyWithRepeat(KeyCode.LeftArrow, CefKeyCodes.Home, baseMods);
                SendKeyWithRepeat(KeyCode.RightArrow, CefKeyCodes.End, baseMods);
                SendKeyWithRepeat(KeyCode.UpArrow, CefKeyCodes.Home, baseMods | (uint)CefEventFlags.ControlDown);
                SendKeyWithRepeat(KeyCode.DownArrow, CefKeyCodes.End, baseMods | (uint)CefEventFlags.ControlDown);
            }
            else if (alt)
            {
                var wordMods = (mods & ~(uint)CefEventFlags.AltDown) | (uint)CefEventFlags.ControlDown;
                SendKeyWithRepeat(KeyCode.LeftArrow, CefKeyCodes.LeftArrow, wordMods);
                SendKeyWithRepeat(KeyCode.RightArrow, CefKeyCodes.RightArrow, wordMods);
            }

            // 3) 非印字キー — 長押しリピート対応
            foreach (var (key, cef) in SpecialKeyTable)
            {
                if (suppressHArrows && (key == KeyCode.LeftArrow || key == KeyCode.RightArrow)) continue;
                if (suppressVArrows && (key == KeyCode.UpArrow || key == KeyCode.DownArrow)) continue;

                SendKeyWithRepeat(key, cef, mods);
            }

            // 4) Cmd/Ctrl + 編集コマンド
            //    CEF OSR では send_key_event でショートカットが処理されないため Frame の編集メソッドを直接呼ぶ
            if (cmd || ctrl)
            {
                if (Input.GetKeyDown(KeyCode.C)) { _browser.Copy(); _inputSentThisFrame = true; }
                if (Input.GetKeyDown(KeyCode.V)) { _browser.Paste(); _inputSentThisFrame = true; }
                if (Input.GetKeyDown(KeyCode.X)) { _browser.Cut(); _inputSentThisFrame = true; }
                if (Input.GetKeyDown(KeyCode.A)) { _browser.SelectAll(); _inputSentThisFrame = true; }
                if (Input.GetKeyDown(KeyCode.Z))
                {
                    if ((mods & (uint)CefEventFlags.ShiftDown) != 0) _browser.Redo();
                    else _browser.Undo();
                    _inputSentThisFrame = true;
                }
            }
        }

        private void SendKeyWithRepeat(KeyCode unityKey, CefKeyCode cefKey, uint mods)
        {
            if (Input.GetKeyDown(unityKey))
            {
                _browser.SendKeyEvent(KeyEventType.RawKeyDown, cefKey, mods);
                _keyDownTime[unityKey] = Time.unscaledTime;
                _keyLastRepeat[unityKey] = Time.unscaledTime;
                _inputSentThisFrame = true;
            }
            else if (Input.GetKey(unityKey))
            {
                var now = Time.unscaledTime;
                if (_keyDownTime.TryGetValue(unityKey, out var downTime)
                    && now - downTime >= KeyRepeatDelay
                    && _keyLastRepeat.TryGetValue(unityKey, out var lastRepeat)
                    && now - lastRepeat >= KeyRepeatRate)
                {
                    _browser.SendKeyEvent(KeyEventType.RawKeyDown, cefKey, mods);
                    _keyLastRepeat[unityKey] = now;
                    _inputSentThisFrame = true;
                }
            }

            if (Input.GetKeyUp(unityKey))
            {
                _browser.SendKeyEvent(KeyEventType.KeyUp, cefKey, mods);
                _keyDownTime.Remove(unityKey);
                _keyLastRepeat.Remove(unityKey);
                _inputSentThisFrame = true;
            }
        }

        private void CheckScreenResize()
        {
            var sw = Mathf.CeilToInt(Screen.width * _resolutionScale);
            var sh = Mathf.CeilToInt(Screen.height * _resolutionScale);
            if (sw != _currentWidth || sh != _currentHeight)
            {
                _currentWidth = sw;
                _currentHeight = sh;
                _browser?.Resize(_currentWidth, _currentHeight);
                if (_enableLog) CefLog.Log($"[CefUnity] Resized to {_currentWidth}x{_currentHeight}");
            }
        }

        // Profiling for accelerated texture path
        private int _accelProfCount;
        private float _accelProfRecvTotal;
        private float _accelProfUpdateTotal;
        private float _accelProfReleaseTotal;

        /// <summary>spin / block なしで accelerated texture の取得を試みる。
        /// 取得成功 = 同フレーム内反映できた場合は true、その他 (新フレーム未到着等) は false。</summary>
        private bool TryUpdateTextureAcceleratedNonBlocking()
        {
            var t0 = Time.realtimeSinceStartup;

            IntPtr newTexPtr;
            int w, h;
            uint format;

#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
            // macOS: IOSurface 経由で毎フレーム新しい Metal テクスチャを受信 → Release が必要
            if (!Browser.TryRecvIOSurfaceTexture(out newTexPtr, out w, out h, out format))
                return false;
#elif UNITY_STANDALONE_WIN || UNITY_EDITOR_WIN
            // Windows: Unity の graphics backend に応じて D3D11/D3D12 を使い分け。
            // ポインタはサイズ変更時以外は安定 (client lib 側でキャッシュ管理)、Release 不要。
            var gotFrame = SystemInfo.graphicsDeviceType == GraphicsDeviceType.Direct3D12
                ? _browser.TryRecvD3D12Texture(out newTexPtr, out w, out h, out format)
                : _browser.TryRecvD3D11Texture(out newTexPtr, out w, out h, out format);
            if (!gotFrame) return false;
#else
            return false;
#endif

            var t1 = Time.realtimeSinceStartup;

            if (w <= 0 || h <= 0)
            {
#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
                Browser.ReleaseMetalTexture(newTexPtr);
#endif
                return false;
            }

            // End-to-end frame delay 計測: server が「この paint は Unity frame N の
            // BeginFrame に対応する」とマークした N を読み、現在の frameCount との差で
            // 何 Unity フレーム遅れて画面に出るかを測る。0 = 同一フレーム取得 = 0 遅延。
            var paintUnityFrame = _browser.GetAccelPaintUnityFrame();
            if (paintUnityFrame > 0)
            {
                long delta = Time.frameCount - (long)paintUnityFrame;
                if (delta >= -10 && delta < 1000) // delta<0 は理論的にはあり得ないが念のため
                {
                    int d = (int)delta;
                    _delaySumFrames += d;
                    _delaySampleCount++;
                    if (d > _delayMaxFrames) _delayMaxFrames = d;
                    if (d < _delayMinFrames) _delayMinFrames = d;
                    int bucket = d >= 0 && d < _delayBuckets.Length ? d : _delayBuckets.Length - 1;
                    _delayBuckets[bucket]++;
                    // 生サンプルを 5 件まで保持 (検証用)
                    if (_recentSamples.Count >= 5) _recentSamples.Dequeue();
                    _recentSamples.Enqueue((Time.frameCount, paintUnityFrame, d));
                }
            }

            if (_texture == null || _texture.width != w || _texture.height != h)
            {
                if (_texture != null) Destroy(_texture);
                // Windows: 共有テクスチャは DXGI_FORMAT_B8G8R8A8_UNORM_SRGB なので linear=false (sRGB)。
                // macOS: Metal 経路も sRGB 解釈なので linear=false。
                _texture = Texture2D.CreateExternalTexture(w, h, TextureFormat.BGRA32, false, false, newTexPtr);
                if (_rawImage != null)
                {
                    _rawImage.texture = _texture;
                    _rawImage.uvRect = new Rect(0, 1, 1, -1);
                }
            }
            else
            {
                _texture.UpdateExternalTexture(newTexPtr);
            }

            var t2 = Time.realtimeSinceStartup;

#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
            // macOS のみ: 前フレームの retain を解放 (Windows は client lib 側で管理)
            if (_lastAccelTexPtr != IntPtr.Zero)
                Browser.ReleaseMetalTexture(_lastAccelTexPtr);
            _lastAccelTexPtr = newTexPtr;
#endif

            var t3 = Time.realtimeSinceStartup;

            _accelProfCount++;
            _accelProfRecvTotal += t1 - t0;
            _accelProfUpdateTotal += t2 - t1;
            _accelProfReleaseTotal += t3 - t2;

            if (_accelProfCount >= 120)
            {
                if (_enableLog) CefLog.Log($"[CefUnity-Prof] C# accel x{_accelProfCount}: recv={_accelProfRecvTotal * 1000f:F2}ms update={_accelProfUpdateTotal * 1000f:F2}ms release={_accelProfReleaseTotal * 1000f:F2}ms total={(_accelProfRecvTotal + _accelProfUpdateTotal + _accelProfReleaseTotal) * 1000f:F2}ms");
                _accelProfCount = 0;
                _accelProfRecvTotal = _accelProfUpdateTotal = _accelProfReleaseTotal = 0;
            }
            _textureUpdatedFrame = Time.frameCount;
            return true;
        }

        private void UpdateTextureSoftware()
        {
            // TryGetBuffer は新しいフレームがある場合のみ true を返す
            if (!_browser.TryGetBuffer(out var buffer, out var w, out var h))
                return;

            if (w <= 0 || h <= 0) return;

            if (_texture == null || _texture.width != w || _texture.height != h)
            {
                // 古いテクスチャを破棄して GPU メモリリークを防ぐ
                if (_texture != null)
                    Destroy(_texture);

                _texture = new Texture2D(w, h, TextureFormat.BGRA32, false);
                if (_rawImage != null)
                {
                    _rawImage.texture = _texture;
                    _rawImage.uvRect = new Rect(0, 1, 1, -1);
                }
            }

            unsafe
            {
                fixed (byte* ptr = buffer)
                {
                    _texture.LoadRawTextureData((IntPtr)ptr, buffer.Length);
                }
            }

            _texture.Apply(false);
        }

        // -----------------------------------------------------------------------
        // OS Settings
        // -----------------------------------------------------------------------

#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
        [DllImport("/usr/lib/libobjc.dylib", EntryPoint = "objc_getClass")]
        private static extern IntPtr ObjcGetClass([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport("/usr/lib/libobjc.dylib", EntryPoint = "sel_registerName")]
        private static extern IntPtr ObjcSelRegisterName([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport("/usr/lib/libobjc.dylib", EntryPoint = "objc_msgSend")]
        private static extern double ObjcMsgSendDouble(IntPtr receiver, IntPtr selector);

        private static float GetOSKeyRepeatDelay()
        {
            try
            {
                var nsEvent = ObjcGetClass("NSEvent");
                var sel = ObjcSelRegisterName("keyRepeatDelay");
                var val = ObjcMsgSendDouble(nsEvent, sel);
                return val > 0 ? (float)val : 0.5f;
            }
            catch
            {
                return 0.5f;
            }
        }

        private static float GetOSKeyRepeatRate()
        {
            try
            {
                var nsEvent = ObjcGetClass("NSEvent");
                var sel = ObjcSelRegisterName("keyRepeatInterval");
                var val = ObjcMsgSendDouble(nsEvent, sel);
                return val > 0 ? (float)val : 0.035f;
            }
            catch
            {
                return 0.035f;
            }
        }
#else
        private static float GetOSKeyRepeatDelay()
        {
            return 0.5f;
        }

        private static float GetOSKeyRepeatRate()
        {
            return 0.035f;
        }
#endif

#if UNITY_EDITOR
        private static FieldInfo _zoomAreaField;
        private static FieldInfo _scaleField;
        private static Type _gameViewType;
        private static bool _reflectionInitialized;

        private static float GetEditorGameViewScale()
        {
            if (!_reflectionInitialized)
            {
                _reflectionInitialized = true;
                var assembly = typeof(Editor).Assembly;
                _gameViewType = assembly.GetType("UnityEditor.GameView");
                if (_gameViewType != null)
                {
                    _zoomAreaField = _gameViewType.GetField("m_ZoomArea",
                        BindingFlags.Instance | BindingFlags.NonPublic);
                    if (_zoomAreaField != null)
                        _scaleField = _zoomAreaField.FieldType.GetField("m_Scale",
                            BindingFlags.Instance | BindingFlags.NonPublic);
                }
            }

            if (_gameViewType == null || _zoomAreaField == null || _scaleField == null)
                return 1f;

            var windows = Resources.FindObjectsOfTypeAll(_gameViewType);
            if (windows.Length == 0) return 1f;

            var zoomArea = _zoomAreaField.GetValue(windows[0]);
            if (zoomArea == null) return 1f;

            var scale = (Vector2)_scaleField.GetValue(zoomArea);
            return scale.y;
        }
#endif
    }
}