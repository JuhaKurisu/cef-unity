using System;
using System.Collections.Generic;
using System.Reflection;
using System.Text;
using CefUnity.Interop;
#if UNITY_EDITOR
using UnityEditor;
#endif
using UnityEngine;
using UnityEngine.LowLevel;
using UnityEngine.PlayerLoop;
using UnityEngine.Rendering;
using UnityEngine.Serialization;
using UnityEngine.UI;

namespace CefUnity.Runtime
{
    // PlayerLoop に挿入するサブシステムの識別用マーカー型
    public struct CefUnityEarlyUpdate { }
    public struct CefUnityPostLateUpdate { }

    public class CefUnityBrowserSample : MonoBehaviour
    {
        private const float DoubleClickTime = 0.3f;
        private const int DoubleClickDistance = 4;


        [SerializeField] private string _url;
        [SerializeField] private RawImage _rawImage;
        [SerializeField] private float _resolutionScale = 1;
        [SerializeField] private bool _enableLog;

        [Header("Audio")]
        [Tooltip("CEF の音声を Unity の AudioSource で再生する (CEF/ブラウザ側では鳴らさない)")]
        [SerializeField] private bool _enableAudio = true;

        [Tooltip("音声レンダラ。UnityMixer=AudioSource 再生 (ミキサ統合, ~160ms) / " +
                 "Native=OS の音声 API 直結 (macOS: AudioUnit / Windows: WASAPI, 低遅延)")]
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

            /// <summary>
            ///     OS のネイティブ音声 API で再生 (macOS: AudioUnit / Windows: WASAPI)。
            ///     低遅延 (macOS 実測 ~30ms) だが Unity ミキサ機能は効かない。
            ///     未対応 OS では開始に失敗し、UnityMixer 経路へフォールバックする。
            /// </summary>
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
        private float _diagnosticsTimer;
        private bool _imeActive;
        private bool _imeSuppressKeys;

        // Accelerated paint (IOSurface / Metal via Mach port)
        private bool _useAcceleratedPaint;
        private IntPtr _lastAcceleratedTexturePointer;

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
        [FormerlySerializedAs("_zeroFrameWaitMs")]
        private float _zeroFrameWaitMilliseconds = 10f;
        // 待ち判定の状態機械 (定数・streak 推定・プローブ窓は CefZeroFramePacer に集約)。
        private readonly CefZeroFramePacer _pacer = new CefZeroFramePacer();
        // このフレームで CEF へ入力イベントを送ったか (アクティブ判定の即時トリガー)。
        private bool _inputSentThisFrame;
        // 0F 待ち検証メトリクス
        private int _doublePumpFreshCount;     // 待ちの後 (or 待ちゼロで) 新 paint を取得できた回数
        private int _doublePumpFallbackCount;  // デッドラインまでに届かず諦めた回数 (前フレーム内容を継続表示)
        private int _doublePumpIdleCount;      // 非アクティブで待ちをスキップした回数
        private double _doublePumpBlockSumMilliseconds;  // recv hook でのブロック時間合計
        private double _doublePumpBlockMaxMilliseconds;

        // -----------------------------------------------------------------------
        // Jitter 計装 (機構切り分け用)
        // -----------------------------------------------------------------------
        // 機構1: フレーム時間 (present 間隔) の分布。double-pump のブロックが present 直前に
        //        入るため、ブロック量のジッタがそのままフレーム間隔のジッタ = ジャダーになる。
        private double _frameTimeSum, _frameTimeSumSquared, _frameTimeMax;
        private int _frameTimeCount, _frameTimeOver18, _frameTimeOver20, _frameTimeOver25; // 18=16.67ms+余裕,20,25ms 超過数
        // 機構2: コンテンツ更新間隔 (fresh paint を取得した実時刻の連続差) の分布。
        //        Chromium のスクロール曲線はこの実時刻でサンプルされるので、間隔のジッタが
        //        見かけのスクロール速度のジッタ = judder に直結する。
        private float _lastFreshRealtime = -1f;
        private double _contentIntervalSum, _contentIntervalSumSquared, _contentIntervalMax;
        private int _contentIntervalCount;
#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
        private bool _navigationTestDone; // 計測用
        private bool _audioTestDone; // 音声テスト用 (cef_load_url トリガー)
#endif

        // PlayerLoop hook 用の singleton 参照 (現在のサンプル構成は単一 Browser のみ対応)
        private static CefUnityBrowserSample s_instance;

        // --- 開発用診断 (Editor の CefFpsMonitorWindow が参照する読み取り専用サーフェス) ---

        /// <summary>診断用: 現在のインスタンス (Play 中でなければ null)。</summary>
        public static CefUnityBrowserSample DiagnosticsInstance => s_instance;

        /// <summary>
        ///     診断用: CEF 内部 paint の累積数 (accel_frame_id)。CEF は damage 駆動で
        ///     paint するため、静止ページでは増えないのが正常。
        /// </summary>
        public ulong DiagnosticsAcceleratedFrameId =>
            _browser != null && _useAcceleratedPaint ? _browser.PeekAcceleratedFrameId() : 0;

        /// <summary>診断用: Unity テクスチャへ実際に適用した paint の累積数。</summary>
        public ulong DiagnosticsTexturesApplied { get; private set; }
        // PlayerLoop hook を install したかどうか
        private bool _playerLoopHooked;

        // 同 Unity フレーム内で 1 回取得したらフレーム末まで再取得しないフラグ
        private int _textureUpdatedFrame = -1;

        // 検証用メトリクス
        private int _postLateUpdateInvokeCount;  // PostLateUpdate hook の呼び出し回数
        private int _gotInPostLateUpdateCount;   // PostLateUpdate で取得成功した回数
        private int _receiveFailCount;              // 取得失敗 (1 frame 遅延扱い)
        // 最近の生サンプルを保持 (frame_count, paint_unity_frame, delta) でログ出力
        private readonly System.Collections.Generic.Queue<(int frameCount, ulong paintFrame, int delta)> _recentSamples
            = new System.Collections.Generic.Queue<(int, ulong, int)>();

        // スクロール入力パイプライン: ソース drain・ルーティング・平滑・リサンプル・録画は
        // ScrollInputPipeline に集約 (本クラスは座標決定と SendMouseWheel のみ担当)。
        // HasNativeSource が false なら Input.mouseScrollDelta → AddWheelSteps のフォールバック。
        private readonly ScrollInputPipeline _scrollInput = new ScrollInputPipeline();

#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
        // 開発トグル cef_scroll_interp / cef_scroll_record の再チェック間隔 (60F に 1 回)。
        private int _scrollToggleCheckCountdown;
#endif

#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
        // --- 分析用 (開発ビルドのみ): 毎フレームの scroll 量/frame time/paint を CSV 記録 ---
        private readonly System.Collections.Generic.List<string> _performanceLog = new();
        private int _frameSentDeltaY;
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
#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
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
                    _zeroFrameWaitMilliseconds = 0f;
#endif

                // ログのマスタースイッチ: Unity 側 (CefLog) と Rust 側 (client/server)
                // の両方を _enableLog 一つで制御する。
                CefLog.Enabled = _enableLog;
                // GPU 経路 (macOS: IOSurface / Windows: D3D11 共有テクスチャ) を常に要求する。
                // サーバー側がプール構築に失敗した場合は software paint へ自動フォールバックする。
                CefRuntime.Initialize(useGpu: true, enableLog: _enableLog);
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
                // キーリピートは OS 設定から取得する (取得失敗時は既定値に落ちる)。
                // 既定値と一致しているかを実機で判別できるようログに出す。
                if (_enableLog)
                    CefLog.Log($"[CefUnity] key repeat: delay={CefKeyboardMapper.KeyRepeatDelay:F3}s rate={CefKeyboardMapper.KeyRepeatRate:F4}s");
                SetupImeProxy();
                // Native レンダラは FMOD ミキサを使わないので DSP バッファ変更は不要。
                if (_audioRenderer == AudioRendererMode.UnityMixer) ApplyAudioDspBufferSize();
                SetupAudioOutput();
                SetupScrollInput();
            }
            catch (Exception exception)
            {
                CefLog.LogError($"[CefUnity] Init failed: {exception}");
            }
        }

        /// <summary>
        ///     生スクロール入力 (C案) の初期化。native ソースが使えれば有効化し、以後
        ///     Input.mouseScrollDelta は読まない (二重計上防止)。失敗時は現行経路のまま。
        /// </summary>
        private void SetupScrollInput()
        {
#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
            // 開発トグル: cef_scroll_legacy で強制フォールバック (A/C 体感比較用)。
            if (System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_scroll_legacy")))
            {
                CefLog.Log("[CefUnity] scroll: legacy mode (cef_scroll_legacy)");
                return;
            }
#endif
            switch (_scrollInput.StartNativeSource(out var error))
            {
                case NativeScrollSourceStart.Started:
                    // macOS = NSEvent ローカルモニタ / Windows = Raw Input。
                    // 経路名を出さないのは、プラットフォームが増えるたびに文言が
                    // 実装とずれるのを避けるため。
                    CefLog.Log("[CefUnity] scroll: native source active");
                    break;
                case NativeScrollSourceStart.Failed:
                    CefLog.Log($"[CefUnity] scroll: native source init threw ({error.GetType().Name}) — fallback");
                    CefLog.Log("[CefUnity] scroll: native source unavailable — frame-polled fallback");
                    break;
                case NativeScrollSourceStart.Unavailable:
                    CefLog.Log("[CefUnity] scroll: native source unavailable — frame-polled fallback");
                    break;
                // NotSupported (非 macOS): 従来どおりログなしでフォールバック
            }
        }

        private void Update()
        {
            CefRuntime.Pump();
            // 入力処理 + BeginFrame 発行は PlayerLoop の EarlyUpdate 末尾 (OnEarlyUpdateLast)
            // で行うため、ここからは削除した。MonoBehaviour.Update の役割は Pump と診断のみ。

#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
            // 開発トグル: temp ファイルで testufo 遷移 + 擬似ゲーム負荷 (8ms 空回し)。
            var temporaryDirectory = System.IO.Path.GetTempPath();
            if (!_navigationTestDone && System.IO.File.Exists(System.IO.Path.Combine(temporaryDirectory, "cef_load_testufo")))
            {
                _navigationTestDone = true;
                LoadUrl("https://testufo.com/mouserate");
            }
            // 計測用 (一時): cef_load_url にファイル内容の URL を書くとそこへ遷移する。
            // 音声テスト (440Hz トーンの data: URI 等) を実行中の PlayMode へ渡すために使う。
            // Time.frameCount > 60: 初期 URL のナビゲーションと競合すると LoadUrl が
            // 負けて遷移しないことがあるため、初期ロードが落ち着いてから発火させる。
            var navigationUrlFile = System.IO.Path.Combine(temporaryDirectory, "cef_load_url");
            if (!_audioTestDone && Time.frameCount > 60 && System.IO.File.Exists(navigationUrlFile))
            {
                _audioTestDone = true;
                var url = System.IO.File.ReadAllText(navigationUrlFile).Trim();
                if (!string.IsNullOrEmpty(url)) LoadUrl(url);
            }
            if (System.IO.File.Exists(System.IO.Path.Combine(temporaryDirectory, "cef_fake_work")))
            {
                var until = Time.realtimeSinceStartup + 0.008f;
                while (Time.realtimeSinceStartup < until) { }
            }
#endif

            // 機構1 計装: フレーム時間 (present 間隔) の分布を毎フレーム集計。
            var frameTime = Time.unscaledDeltaTime;
            _frameTimeSum += frameTime;
            _frameTimeSumSquared += (double)frameTime * frameTime;
            _frameTimeCount++;
            if (frameTime > _frameTimeMax) _frameTimeMax = frameTime;
            if (frameTime > 0.018) _frameTimeOver18++;
            if (frameTime > 0.020) _frameTimeOver20++;
            if (frameTime > 0.025) _frameTimeOver25++;

#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
            // 開発トグル: cef_perf_probe がある間、毎フレーム記録し 30 フレームごとに CSV 追記。
            if (System.IO.File.Exists(System.IO.Path.Combine(temporaryDirectory, "cef_perf_probe")))
            {
                long acceleratedFrameIdNow = _useAcceleratedPaint && _browser != null ? (long)_browser.PeekAcceleratedFrameId() : 0;
                _performanceLog.Add($"{Time.frameCount},{frameTime * 1000f:F2},{acceleratedFrameIdNow},{_frameSentDeltaY}");
                _frameSentDeltaY = 0;
                if (_performanceLog.Count >= 30)
                {
                    try
                    {
                        System.IO.File.AppendAllText(
                            System.IO.Path.Combine(temporaryDirectory, "cef_perf.csv"),
                            string.Join("\n", _performanceLog) + "\n");
                    }
                    catch { }
                    _performanceLog.Clear();
                }
            }
#endif

            _diagnosticsTimer += Time.deltaTime;
            if (_diagnosticsTimer >= 2f)
            {
                _diagnosticsTimer = 0f;

                if (_enableLog)
                {
                    var paintCount = NativeMethods.cef_unity_get_paint_count();
                    var pumpCount = NativeMethods.cef_unity_get_pump_count();
                    // afi = accel_frame_id (server が Mach 送信を完了した paint の累積数)。
                    // 2 秒窓の増分が「CEF が Unity へ届けた paint レート」= CEF 出力 fps。
                    // paint= は software 経路のカウンタなので GPU 経路では常に 0。
                    var acceleratedFrameId = _useAcceleratedPaint ? _browser.PeekAcceleratedFrameId() : 0;
                    CefLog.Log($"[CefUnity] diag: paint={paintCount} pump={pumpCount} afi={acceleratedFrameId}");
                    var logs = CefRuntime.GetLogs();
                    foreach (var line in logs)
                        CefLog.Log($"[CefServer] {line}");

                    if (_delaySampleCount > 0)
                    {
                        var average = (float)_delaySumFrames / _delaySampleCount;
                        var stringBuilder = new StringBuilder();
                        stringBuilder.Append($"[CefUnity] end-to-end frame delay (n={_delaySampleCount}): avg={average:F2} min={_delayMinFrames} max={_delayMaxFrames} buckets=[");
                        for (int bucketIndex = 0; bucketIndex < _delayBuckets.Length; bucketIndex++)
                        {
                            if (bucketIndex > 0) stringBuilder.Append(' ');
                            stringBuilder.Append($"{bucketIndex}{(bucketIndex == _delayBuckets.Length - 1 ? "+" : "")}:{_delayBuckets[bucketIndex]}");
                        }
                        stringBuilder.Append(']');
                        CefLog.Log(stringBuilder.ToString());

                        // 検証メトリクス: PostLateUpdate hook での取得統計
                        CefLog.Log($"[CefUnity] verify: PostLateUpdate={_postLateUpdateInvokeCount} recv_ok={_gotInPostLateUpdateCount} recv_fail={_receiveFailCount}");
                        var stringBuilder2 = new StringBuilder("[CefUnity] verify samples (fc, paint_fc, delta):");
                        foreach (var sample in _recentSamples)
                            stringBuilder2.Append($" ({sample.frameCount},{sample.paintFrame},{sample.delta})");
                        CefLog.Log(stringBuilder2.ToString());

                        _delaySampleCount = 0;
                        _delaySumFrames = 0;
                        _delayMaxFrames = 0;
                        _delayMinFrames = int.MaxValue;
                        for (int bucketIndex = 0; bucketIndex < _delayBuckets.Length; bucketIndex++) _delayBuckets[bucketIndex] = 0;
                        _postLateUpdateInvokeCount = 0;
                        _gotInPostLateUpdateCount = 0;
                        _receiveFailCount = 0;
                        _recentSamples.Clear();
                    }

                    // 0F 待ち専用メトリクス (fresh=新paint取得 / fallback=届かず1F / idle=待ちスキップ)。
                    if (_zeroFrameWaitMilliseconds > 0f && _useAcceleratedPaint)
                    {
                        var doublePumpActive = _doublePumpFreshCount + _doublePumpFallbackCount;
                        var blockAverage = doublePumpActive > 0 ? _doublePumpBlockSumMilliseconds / doublePumpActive : 0.0;
                        CefLog.Log($"[CefUnity] 0F-wait: fresh={_doublePumpFreshCount} fallback(1F)={_doublePumpFallbackCount} idle={_doublePumpIdleCount} block_avg={blockAverage:F2}ms block_max={_doublePumpBlockMaxMilliseconds:F2}ms");
                        _doublePumpFreshCount = 0;
                        _doublePumpFallbackCount = 0;
                        _doublePumpIdleCount = 0;
                        _doublePumpBlockSumMilliseconds = 0;
                        _doublePumpBlockMaxMilliseconds = 0;
                    }

                    // jitter 計装: 機構1 (フレーム時間=present 間隔) と 機構2 (content 更新間隔)。
                    // 0F 待ち ON/OFF どちらでも出力して比較できるようにする。
                    var doublePump = (_zeroFrameWaitMilliseconds > 0f && _useAcceleratedPaint) ? "ON " : "OFF";
                    var frameTimeMean = _frameTimeCount > 0 ? _frameTimeSum / _frameTimeCount : 0.0;
                    var frameTimeStandardDeviation = _frameTimeCount > 0 ? Math.Sqrt(Math.Max(0, _frameTimeSumSquared / _frameTimeCount - frameTimeMean * frameTimeMean)) : 0.0;
                    var contentIntervalMean = _contentIntervalCount > 0 ? _contentIntervalSum / _contentIntervalCount : 0.0;
                    var contentIntervalStandardDeviation = _contentIntervalCount > 0 ? Math.Sqrt(Math.Max(0, _contentIntervalSumSquared / _contentIntervalCount - contentIntervalMean * contentIntervalMean)) : 0.0;
                    CefLog.Log(
                        $"[CefUnity] jitter dp={doublePump}: " +
                        $"frame(n={_frameTimeCount}) mean={frameTimeMean * 1000:F2}ms std={frameTimeStandardDeviation * 1000:F2}ms max={_frameTimeMax * 1000:F1}ms over18/20/25={_frameTimeOver18}/{_frameTimeOver20}/{_frameTimeOver25} | " +
                        $"content(n={_contentIntervalCount}) mean={contentIntervalMean * 1000:F2}ms std={contentIntervalStandardDeviation * 1000:F2}ms max={_contentIntervalMax * 1000:F1}ms");
                    _frameTimeSum = _frameTimeSumSquared = _frameTimeMax = 0; _frameTimeCount = _frameTimeOver18 = _frameTimeOver20 = _frameTimeOver25 = 0;
                    _contentIntervalSum = _contentIntervalSumSquared = _contentIntervalMax = 0; _contentIntervalCount = 0;
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
            // グライド途中の残距離/履歴を新ページへ流し込まない。
            _scrollInput.Reset();
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
            // ソース解放 + 録画バッファの最終フラッシュ (末尾行の消失防止)。
            _scrollInput.Dispose();

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

            if (_lastAcceleratedTexturePointer != IntPtr.Zero)
            {
                Browser.ReleaseMetalTexture(_lastAcceleratedTexturePointer);
                _lastAcceleratedTexturePointer = IntPtr.Zero;
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
#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
            // 開発トグル: cef_scroll_test が在るあいだ、実ユーザーのホイール操作を模して
            // 毎フレーム ±60px のスクロールを注入する (3 秒ごとに方向反転)。実ホイールと
            // 同じ EarlyUpdate 内で送ること (Update 内だと _inputSentThisFrame がこの hook
            // 冒頭のリセットで消え、連続入力判定・アクティブ判定に乗らない)。
            if (System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_scroll_test")))
            {
                var direction = ((int)(Time.realtimeSinceStartup / 3f)) % 2 == 0 ? -1 : 1;
                self._browser.SendMouseWheel(self._currentWidth / 2, self._currentHeight / 2, 0, direction * 60);
                self._inputSentThisFrame = true;
            }
            // 開発トグル: cef_scroll_slow が在る間、毎フレーム正確に -10px の均一スクロールを注入。
            // 「数学的に完璧に均一な入力」でもカクつくか(=パイプライン位相ビート)を切り分ける。
            if (System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_scroll_slow")))
            {
                self._browser.SendMouseWheel(self._currentWidth / 2, self._currentHeight / 2, 0, -10);
                self._frameSentDeltaY = -10;
                self._inputSentThisFrame = true;
            }
#endif
            // 生イベント経路 (C案): native ソースを drain し、リサンプラ排出を送る。
            self.TickNativeScroll();
            // スクロール平滑の排出 (非 precise / フォールバック)。BeginFrame#1 の前なので
            // 同フレームの paint に乗る。
            self.TickScrollSmoother();
            self.UpdateCompositionCursorPosition();
            self.HandleImeInput();
            self.HandleKeyboardInput();
            // BeginFrame#1 直前の paint カウンタと時刻を記録 (recv 側の増分検知・待ち基準)。
            // 入力ハンドラ群の後なので _inputSentThisFrame は確定済み。Peek → 時刻取得の
            // 順序は旧実装と同一に保つ。
            var acceleratedFrameIdNow = self._useAcceleratedPaint ? self._browser.PeekAcceleratedFrameId() : 0UL;
            self._pacer.OnBeginFrame(Time.realtimeSinceStartup, acceleratedFrameIdNow, self._inputSentThisFrame);
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
        private static void OnPostLateUpdateReceive()
        {
            var self = s_instance;
            if (self == null || self._browser == null) return;
            self._postLateUpdateInvokeCount++;
            self.ReceiveBeforeRender();
        }

        /// <summary>
        /// 描画発行前の recv 本体。server-side flush の結果 (accel_frame_id 増分) を
        /// _zeroFrameWaitMilliseconds (BF#1 からの経過時間 cap) まで待ち、届いた最新 paint を
        /// 同フレームの present に乗せる (0F)。ゲーム処理が重いフレームではここへの到達が
        /// 遅く cap を過ぎているため自動的に待ちゼロ (flush 結果は自然に到着済み)。
        /// デッドラインまでに届かなければ従来通り 1F フォールバック。待ちは SHM カウンタの
        /// busy-wait のみで IPC を発行しない (旧 client-side double-pump の reflush による
        /// IPC フラッディング → 46ms ブロック問題は構造的に発生しない)。
        /// </summary>
        private void ReceiveBeforeRender()
        {
            // software 経路 / 待ち無効時は従来のノンブロッキング受信のみ。
            if (!_useAcceleratedPaint || _zeroFrameWaitMilliseconds <= 0f)
            {
                if (TryUpdateTextureOnce()) OnFreshPaint();
                else OnNoPaint();
                return;
            }

            var blockStart = Time.realtimeSinceStartup;

            // プローブ判定 (静止中は待たない)。判定根拠は CefZeroFramePacer 参照。
            if (_pacer.ShouldSkipAsIdle(_inputSentThisFrame))
            {
                if (TryUpdateTextureOnce()) OnFreshPaint();
                else OnNoPaint();
                _doublePumpIdleCount++;
                return;
            }

            // damage streak 抑止推定・連続入力中は待ちスキップ (根拠は CefZeroFramePacer 参照)。
            if (_pacer.ShouldSkipAsSuppressed())
            {
                if (TryUpdateTextureOnce()) { OnFreshPaint(); _doublePumpFreshCount++; }
                else { OnNoPaint(); _doublePumpFallbackCount++; }
                return;
            }

            var window = _pacer.OpenWaitWindow(_zeroFrameWaitMilliseconds);
            while (true)
            {
                var now = Time.realtimeSinceStartup;
                if (window.DeadlineReached(now)) break;
                if (window.OnAcceleratedFrameIdSample(now, _browser.PeekAcceleratedFrameId())) break;
                // Peek (FFI + SHM read) のフル回転を避けて CPU/メモリバス圧を下げる。
                // SpinWait はデスケジュールされない (Thread.Sleep(1) は macOS で 10ms+
                // オーバースリープするため使用不可)。時間精度は ~µs で十分。
                System.Threading.Thread.SpinWait(64);
            }

            // 増分で抜けた場合はその paint を、デッドライン切れでも直前に届いた分があれば拾う
            // (TryReceive は queue を drain して最新を返すため、どちらでも最新が取れる)。
            if (TryUpdateTextureOnce()) { OnFreshPaint(); _doublePumpFreshCount++; }
            else { OnNoPaint(); _doublePumpFallbackCount++; }

            var blockMilliseconds = (Time.realtimeSinceStartup - blockStart) * 1000.0;
            _doublePumpBlockSumMilliseconds += blockMilliseconds;
            if (blockMilliseconds > _doublePumpBlockMaxMilliseconds) _doublePumpBlockMaxMilliseconds = blockMilliseconds;
        }

        /// <summary>recv 成功時の共通処理 (verify 計装 + activity/streak カウンタ更新)。</summary>
        private void OnFreshPaint()
        {
            _gotInPostLateUpdateCount++;
            _pacer.OnFreshPaint();
            RecordContentInterval();
        }

        /// <summary>recv 失敗 (新 paint なし) 時の共通処理。</summary>
        private void OnNoPaint()
        {
            if (_textureUpdatedFrame != Time.frameCount) _receiveFailCount++;
            _pacer.OnNoPaint();
        }

        /// <summary>機構2 計装: 新テクスチャを適用した実時刻の連続差 (= コンテンツがカバーする
        /// 実時間幅) を集計する。Chromium のスクロール曲線はこの実時刻でサンプルされるため、
        /// この間隔のジッタが見かけのスクロール速度のジッタ (judder) に直結する。</summary>
        private void RecordContentInterval()
        {
            var nowRealtime = Time.realtimeSinceStartup;
            if (_lastFreshRealtime >= 0f)
            {
                double contentInterval = nowRealtime - _lastFreshRealtime;
                _contentIntervalSum += contentInterval;
                _contentIntervalSumSquared += contentInterval * contentInterval;
                _contentIntervalCount++;
                if (contentInterval > _contentIntervalMax) _contentIntervalMax = contentInterval;
            }
            _lastFreshRealtime = nowRealtime;
        }

        // recv フックの挿入先アンカー (優先順)。受信テクスチャを同フレームの present に
        // 乗せるには Canvas ジオメトリ更新 (PlayerUpdateCanvases) より前に RawImage の
        // テクスチャを差し替える必要がある。Unity 6000.3 では描画発行 (FinishFrameRendering)
        // と present (PresentAfterDraw) が PostLateUpdate 内にあるため、リスト末尾への
        // Append は present より後 = 反映が次フレームの描画にずれる (実画面 +1F)。
        private static readonly Type[] ReceiveAnchorTypes =
        {
            typeof(PostLateUpdate.PlayerUpdateCanvases),
            typeof(PostLateUpdate.PlayerEmitCanvasGeometry),
            typeof(PostLateUpdate.FinishFrameRendering),
        };

        private static void InstallPlayerLoopHooks()
        {
            var loop = PlayerLoop.GetCurrentPlayerLoop();
            for (int index = 0; index < loop.subSystemList.Length; index++)
            {
                if (loop.subSystemList[index].type == typeof(EarlyUpdate))
                    loop.subSystemList[index] = AppendSubsystem(loop.subSystemList[index], typeof(CefUnityEarlyUpdate), OnEarlyUpdateLast);
                else if (loop.subSystemList[index].type == typeof(PostLateUpdate))
                    loop.subSystemList[index] = InsertSubsystemBeforeAnchor(loop.subSystemList[index], ReceiveAnchorTypes, typeof(CefUnityPostLateUpdate), OnPostLateUpdateReceive);
            }
            PlayerLoop.SetPlayerLoop(loop);
        }

        private static void UninstallPlayerLoopHooks()
        {
            var loop = PlayerLoop.GetCurrentPlayerLoop();
            for (int index = 0; index < loop.subSystemList.Length; index++)
            {
                if (loop.subSystemList[index].type == typeof(EarlyUpdate))
                    loop.subSystemList[index] = RemoveSubsystem(loop.subSystemList[index], typeof(CefUnityEarlyUpdate));
                else if (loop.subSystemList[index].type == typeof(PostLateUpdate))
                    loop.subSystemList[index] = RemoveSubsystem(loop.subSystemList[index], typeof(CefUnityPostLateUpdate));
            }
            PlayerLoop.SetPlayerLoop(loop);
        }

        private static PlayerLoopSystem AppendSubsystem(PlayerLoopSystem parent, Type marker, PlayerLoopSystem.UpdateFunction update)
        {
            var oldList = parent.subSystemList ?? Array.Empty<PlayerLoopSystem>();
            // 既に同 marker が入っていたら何もしない (二重 install 防止)
            for (int index = 0; index < oldList.Length; index++)
                if (oldList[index].type == marker) return parent;
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
            for (int index = 0; index < oldList.Length; index++)
                if (oldList[index].type == marker) return parent;

            int insertAt = -1;
            foreach (var anchor in anchors)
            {
                insertAt = Array.FindIndex(oldList, subsystem => subsystem.type == anchor);
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
            var index = Array.FindIndex(oldList, subsystem => subsystem.type == marker);
            if (index < 0) return parent;
            var newList = new PlayerLoopSystem[oldList.Length - 1];
            Array.Copy(oldList, 0, newList, 0, index);
            Array.Copy(oldList, index + 1, newList, index, oldList.Length - index - 1);
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

            var composition = Input.compositionString;
            var input = Input.inputString;

            if (!string.IsNullOrEmpty(composition))
            {
                // IME が暗黙的に確定して新しい composition を開始した場合を検出
                // (例: "嗚呼亜" → Enter なしで次の文字 → "あ")
                // この場合 Input.inputString に確定テキストが入っている
                if (_imeActive && !string.IsNullOrEmpty(input))
                {
                    var commitStringBuilder = new StringBuilder();
                    foreach (var character in input)
                        if (!char.IsControl(character))
                            commitStringBuilder.Append(character);
                    if (commitStringBuilder.Length > 0)
                    {
                        var commitText = commitStringBuilder.ToString();
                        _browser.ImeCommitText(commitText);
                    }
                }

                // composition 開始/変更
                _browser.ImeSetComposition(composition, (uint)composition.Length, (uint)composition.Length);
                _imeActive = true;
                _imeSuppressKeys = true;
                _inputSentThisFrame = true;
            }
            else if (_imeActive)
            {
                _inputSentThisFrame = true;
                // composition 終了 (非空 → 空に変化)
                var committed = false;
                foreach (var character in input)
                    if (!char.IsControl(character))
                    {
                        committed = true;
                        break;
                    }

                if (committed)
                {
                    // 制御文字を除いた確定テキストを取得
                    var stringBuilder = new StringBuilder();
                    foreach (var character in input)
                        if (!char.IsControl(character))
                            stringBuilder.Append(character);
                    var text = stringBuilder.ToString();
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

        private void UpdateCompositionCursorPosition()
        {
            if (_browser == null || _rawImage == null) return;

            _browser.GetImeCaret(out var caretX, out var caretY, out var caretWidth, out var caretHeight);

            // まだキャレット位置が報告されていない場合はスキップ
            if (caretX == 0 && caretY == 0 && caretWidth == 0 && caretHeight == 0) return;

            var rectTransform = _rawImage.rectTransform;
            var rect = rectTransform.rect;

            var normalizedX = (float)caretX / _currentWidth;
            var normalizedY = (float)(caretY + caretHeight) / _currentHeight;

            var localX = rect.x + normalizedX * rect.width;
            var localY = rect.y + (1f - normalizedY) * rect.height;
            var localPoint = new Vector3(localX, localY, 0);

            var canvas = _rawImage.canvas;
            var camera = canvas.renderMode == RenderMode.ScreenSpaceOverlay ? null : canvas.worldCamera;
            var worldPoint = rectTransform.TransformPoint(localPoint);
            var screenPosition = RectTransformUtility.WorldToScreenPoint(camera, worldPoint);

#if UNITY_EDITOR
            // Editor の Game View Scale 補正: Scale 2x では表示が2倍ズームされるため
            // compositionCursorPos もスケール倍する必要がある
            var scale = GetEditorGameViewScale();
            screenPosition *= scale;
#endif

            Input.compositionCursorPos = screenPosition;
        }

        private uint GetCefModifiers()
        {
            uint modifiers = 0;
            if (Input.GetKey(KeyCode.LeftShift) || Input.GetKey(KeyCode.RightShift)) modifiers |= (uint)CefEventFlags.ShiftDown;
            if (Input.GetKey(KeyCode.LeftControl) || Input.GetKey(KeyCode.RightControl)) modifiers |= (uint)CefEventFlags.ControlDown;
            if (Input.GetKey(KeyCode.LeftAlt) || Input.GetKey(KeyCode.RightAlt)) modifiers |= (uint)CefEventFlags.AltDown;
            if (Input.GetKey(KeyCode.LeftCommand) || Input.GetKey(KeyCode.RightCommand)) modifiers |= (uint)CefEventFlags.CommandDown;
            if (Input.GetMouseButton(0)) modifiers |= (uint)CefEventFlags.LeftMouseDown;
            if (Input.GetMouseButton(1)) modifiers |= (uint)CefEventFlags.RightMouseDown;
            if (Input.GetMouseButton(2)) modifiers |= (uint)CefEventFlags.MiddleMouseDown;
            return modifiers;
        }

        // -----------------------------------------------------------------------
        // Mouse
        // -----------------------------------------------------------------------
        private void HandleMouseInput()
        {
            if (_browser == null || _rawImage == null) return;

            if (!TryGetBrowserCoordinates(out var browserX, out var browserY))
                return;

            var modifiers = GetCefModifiers();

            if (browserX != _lastMouseX || browserY != _lastMouseY)
            {
                _lastMouseX = browserX;
                _lastMouseY = browserY;
                _browser.SendMouseMove(browserX, browserY, modifiers);
                _inputSentThisFrame = true;
            }

            HandleButton(browserX, browserY, 0, MouseButton.Left, modifiers);
            HandleButton(browserX, browserY, 1, MouseButton.Right, modifiers);
            HandleButton(browserX, browserY, 2, MouseButton.Middle, modifiers);

            // native ソース有効時は生イベント経路 (TickNativeScroll) が担うため、
            // フレーム量子化された Input.mouseScrollDelta は読まない (二重計上防止)。
            if (!_scrollInput.HasNativeSource)
            {
                var scroll = Input.mouseScrollDelta;
                if (scroll.y != 0f || scroll.x != 0f)
                {
                    // 送信は即時ではなくスムーザへ蓄積し、OnEarlyUpdateLast の
                    // TickScrollSmoother が毎フレーム均一化して排出する。
                    _scrollInput.AddWheelSteps(scroll.x, scroll.y, _resolutionScale);
                }
            }
        }

        /// <summary>
        ///     native スクロールソースの 1 フレーム処理。drain・ルーティング・排出計算は
        ///     ScrollInputPipeline が担い、ここは座標決定と送信のみ。
        /// </summary>
        private void TickNativeScroll()
        {
            if (!_scrollInput.HasNativeSource || _browser == null) return;
#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
            // 開発トグル: cef_scroll_interp で補間モード (予測が既定)、cef_scroll_record で生イベント録画。
            if (--_scrollToggleCheckCountdown <= 0)
            {
                _scrollToggleCheckCountdown = 60;
                var temporaryDirectory = System.IO.Path.GetTempPath();
                // 既定は予測モード。cef_scroll_interp で補間モードに切替 (A/B 比較用オプトアウト)。
                _scrollInput.Predictive = !System.IO.File.Exists(
                    System.IO.Path.Combine(temporaryDirectory, "cef_scroll_interp"));
                _scrollInput.RecordingEnabled = System.IO.File.Exists(
                    System.IO.Path.Combine(temporaryDirectory, "cef_scroll_record"));
            }
#endif
            _scrollInput.Drain(TryGetBrowserCoordinates(out _, out _), _resolutionScale);
            if (!_scrollInput.TickResampler(out var deltaX, out var deltaY)) return;
            if (deltaX == 0 && deltaY == 0) return;
            SendWheelAtLastCursor(deltaX, deltaY);
        }

        /// <summary>
        /// スムーザの 1 フレーム分排出 (per-frame スクロール量ジッターの平滑)。
        /// HandleMouseInput の外に置くのは、カーソルがブラウザ外に出ても
        /// (TryGetBrowserCoordinates 失敗でも) グライド途中の排出を最後の有効座標で
        /// 継続するため。
        /// </summary>
        private void TickScrollSmoother()
        {
            if (_browser == null) return;
            if (!_scrollInput.TickSmoother(Time.unscaledDeltaTime, out var deltaX, out var deltaY)) return;
            if (deltaX == 0 && deltaY == 0) return;
            SendWheelAtLastCursor(deltaX, deltaY);
        }

        /// <summary>最後の有効マウス座標 (未取得なら画面中央) へホイールを送る。</summary>
        private void SendWheelAtLastCursor(int deltaX, int deltaY)
        {
            var browserX = _lastMouseX >= 0 ? _lastMouseX : _currentWidth / 2;
            var browserY = _lastMouseY >= 0 ? _lastMouseY : _currentHeight / 2;
            _browser.SendMouseWheel(browserX, browserY, deltaX, deltaY, GetCefModifiers());
            _inputSentThisFrame = true;
#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)
            _frameSentDeltaY = deltaY; // 分析用: 平滑/リサンプル後の実送信量
#endif
        }

        private void HandleButton(int browserX, int browserY, int unityButton, MouseButton cefButton, uint modifiers)
        {
            if (Input.GetMouseButtonDown(unityButton))
            {
                if (unityButton == 0)
                {
                    var now = Time.unscaledTime;
                    if (now - _lastClickTime < DoubleClickTime
                        && Math.Abs(browserX - _lastClickX) <= DoubleClickDistance
                        && Math.Abs(browserY - _lastClickY) <= DoubleClickDistance)
                        _clickCount = _clickCount >= 3 ? 1 : _clickCount + 1;
                    else
                        _clickCount = 1;
                    _lastClickTime = now;
                    _lastClickX = browserX;
                    _lastClickY = browserY;
                }
                else
                {
                    _clickCount = 1;
                }

                _browser.SendMouseClick(browserX, browserY, cefButton, false, _clickCount, modifiers);
                _inputSentThisFrame = true;
            }

            if (Input.GetMouseButtonUp(unityButton))
            {
                _browser.SendMouseClick(browserX, browserY, cefButton, true, _clickCount, modifiers);
                _inputSentThisFrame = true;
            }
        }

        /// <summary>
        ///     スクリーン上のマウス座標を RawImage のローカル座標経由でブラウザ座標 (0..width, 0..height) に変換する。
        ///     RawImage 外なら false を返す。
        /// </summary>
        private bool TryGetBrowserCoordinates(out int browserX, out int browserY)
        {
            browserX = browserY = 0;
            var rectTransform = _rawImage.rectTransform;

            // Canvas 内の Camera を取得（Overlay なら null）
            var canvas = _rawImage.canvas;
            var camera = canvas.renderMode == RenderMode.ScreenSpaceOverlay ? null : canvas.worldCamera;

            if (!RectTransformUtility.ScreenPointToLocalPointInRectangle(
                    rectTransform, Input.mousePosition, camera, out var local))
                return false;

            var rect = rectTransform.rect;
            // rect 内の 0..1 正規化座標
            var normalizedX = (local.x - rect.x) / rect.width;
            var normalizedY = (local.y - rect.y) / rect.height;

            if (normalizedX < 0f || normalizedX > 1f || normalizedY < 0f || normalizedY > 1f)
                return false;

            // uvRect (0,1,1,-1) で Y 反転しているので補正
            normalizedY = 1f - normalizedY;

            browserX = Mathf.Clamp((int)(normalizedX * _currentWidth), 0, _currentWidth - 1);
            browserY = Mathf.Clamp((int)(normalizedY * _currentHeight), 0, _currentHeight - 1);
            return true;
        }

        private void HandleKeyboardInput()
        {
            if (_browser == null) return;

            // IME composition 中・終了直後は全キー入力を抑制 (OS の IME が処理する)
            if (_imeSuppressKeys) return;

            var modifiers = GetCefModifiers();
            var commandKeyDown = (modifiers & (uint)CefEventFlags.CommandDown) != 0;
            var controlKeyDown = (modifiers & (uint)CefEventFlags.ControlDown) != 0;
            var alt = (modifiers & (uint)CefEventFlags.AltDown) != 0;

            // 1) 印字可能文字 — Input.inputString 経由 (RAWKEYDOWN + CHAR + KEYUP)
            //    IME 変換中・commit 直後は抑制（preedit/commit は別経路で CEF に送信される）
            if (string.IsNullOrEmpty(Input.compositionString))
                foreach (var character in Input.inputString)
                {
                    if (char.IsControl(character)) continue;
                    // 英数/かなキーが生成する偽スペースをフィルタ
                    if (character == ' ' && !Input.GetKey(KeyCode.Space)) continue;
                    _browser.SendCharEvent(character, modifiers);
                    _inputSentThisFrame = true;
                }

            // 2) macOS キー変換: CEF OSR は interpretKeyEvents: パイプラインが無いため手動変換
            //    Cmd+Arrow → Home/End, Alt+Arrow → Ctrl+Arrow (単語移動)
            //    Shift が併用された場合は選択操作になる (ShiftDown は baseModifiers に残る)
            var suppressHorizontalArrows = commandKeyDown || alt;
            var suppressVerticalArrows = commandKeyDown;
            if (commandKeyDown)
            {
                var baseModifiers = modifiers & ~(uint)CefEventFlags.CommandDown;
                SendKeyWithRepeat(KeyCode.LeftArrow, CefKeyCodes.Home, baseModifiers);
                SendKeyWithRepeat(KeyCode.RightArrow, CefKeyCodes.End, baseModifiers);
                SendKeyWithRepeat(KeyCode.UpArrow, CefKeyCodes.Home, baseModifiers | (uint)CefEventFlags.ControlDown);
                SendKeyWithRepeat(KeyCode.DownArrow, CefKeyCodes.End, baseModifiers | (uint)CefEventFlags.ControlDown);
            }
            else if (alt)
            {
                var wordModifiers = (modifiers & ~(uint)CefEventFlags.AltDown) | (uint)CefEventFlags.ControlDown;
                SendKeyWithRepeat(KeyCode.LeftArrow, CefKeyCodes.LeftArrow, wordModifiers);
                SendKeyWithRepeat(KeyCode.RightArrow, CefKeyCodes.RightArrow, wordModifiers);
            }

            // 3) 非印字キー — 長押しリピート対応
            foreach (var (key, cef) in CefKeyboardMapper.SpecialKeyTable)
            {
                if (suppressHorizontalArrows && (key == KeyCode.LeftArrow || key == KeyCode.RightArrow)) continue;
                if (suppressVerticalArrows && (key == KeyCode.UpArrow || key == KeyCode.DownArrow)) continue;

                SendKeyWithRepeat(key, cef, modifiers);
            }

            // 4) Cmd/Ctrl + 編集コマンド
            //    CEF OSR では send_key_event でショートカットが処理されないため Frame の編集メソッドを直接呼ぶ
            if (commandKeyDown || controlKeyDown)
            {
                if (Input.GetKeyDown(KeyCode.C)) { _browser.Copy(); _inputSentThisFrame = true; }
                if (Input.GetKeyDown(KeyCode.V)) { _browser.Paste(); _inputSentThisFrame = true; }
                if (Input.GetKeyDown(KeyCode.X)) { _browser.Cut(); _inputSentThisFrame = true; }
                if (Input.GetKeyDown(KeyCode.A)) { _browser.SelectAll(); _inputSentThisFrame = true; }
                if (Input.GetKeyDown(KeyCode.Z))
                {
                    if ((modifiers & (uint)CefEventFlags.ShiftDown) != 0) _browser.Redo();
                    else _browser.Undo();
                    _inputSentThisFrame = true;
                }
            }
        }

        private void SendKeyWithRepeat(KeyCode unityKey, CefKeyCode cefKey, uint modifiers)
        {
            if (Input.GetKeyDown(unityKey))
            {
                _browser.SendKeyEvent(KeyEventType.RawKeyDown, cefKey, modifiers);
                _keyDownTime[unityKey] = Time.unscaledTime;
                _keyLastRepeat[unityKey] = Time.unscaledTime;
                _inputSentThisFrame = true;
            }
            else if (Input.GetKey(unityKey))
            {
                var now = Time.unscaledTime;
                if (_keyDownTime.TryGetValue(unityKey, out var downTime)
                    && now - downTime >= CefKeyboardMapper.KeyRepeatDelay
                    && _keyLastRepeat.TryGetValue(unityKey, out var lastRepeat)
                    && now - lastRepeat >= CefKeyboardMapper.KeyRepeatRate)
                {
                    _browser.SendKeyEvent(KeyEventType.RawKeyDown, cefKey, modifiers);
                    _keyLastRepeat[unityKey] = now;
                    _inputSentThisFrame = true;
                }
            }

            if (Input.GetKeyUp(unityKey))
            {
                _browser.SendKeyEvent(KeyEventType.KeyUp, cefKey, modifiers);
                _keyDownTime.Remove(unityKey);
                _keyLastRepeat.Remove(unityKey);
                _inputSentThisFrame = true;
            }
        }

        private void CheckScreenResize()
        {
            var scaledWidth = Mathf.CeilToInt(Screen.width * _resolutionScale);
            var scaledHeight = Mathf.CeilToInt(Screen.height * _resolutionScale);
            if (scaledWidth != _currentWidth || scaledHeight != _currentHeight)
            {
                _currentWidth = scaledWidth;
                _currentHeight = scaledHeight;
                _browser?.Resize(_currentWidth, _currentHeight);
                if (_enableLog) CefLog.Log($"[CefUnity] Resized to {_currentWidth}x{_currentHeight}");
            }
        }

        // Profiling for accelerated texture path
        private int _acceleratedProfilingCount;
        private float _acceleratedProfilingReceiveTotal;
        private float _acceleratedProfilingUpdateTotal;
        private float _acceleratedProfilingReleaseTotal;

        /// <summary>spin / block なしで accelerated texture の取得を試みる。
        /// 取得成功 = 同フレーム内反映できた場合は true、その他 (新フレーム未到着等) は false。</summary>
        private bool TryUpdateTextureAcceleratedNonBlocking()
        {
            var timeStart = Time.realtimeSinceStartup;

            IntPtr newTexturePointer;
            int width, height;
            uint format;

#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
            // macOS: IOSurface 経由で毎フレーム新しい Metal テクスチャを受信 → Release が必要
            if (!Browser.TryReceiveIOSurfaceTexture(out newTexturePointer, out width, out height, out format))
                return false;
#elif UNITY_STANDALONE_WIN || UNITY_EDITOR_WIN
            // Windows: Unity の graphics backend に応じて D3D11/D3D12 を使い分け。
            // ポインタはサイズ変更時以外は安定 (client lib 側でキャッシュ管理)、Release 不要。
            var gotFrame = SystemInfo.graphicsDeviceType == GraphicsDeviceType.Direct3D12
                ? _browser.TryReceiveD3D12Texture(out newTexturePointer, out width, out height, out format)
                : _browser.TryReceiveD3D11Texture(out newTexturePointer, out width, out height, out format);
            if (!gotFrame) return false;
#else
            return false;
#endif

            var timeAfterReceive = Time.realtimeSinceStartup;

            if (width <= 0 || height <= 0)
            {
#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
                Browser.ReleaseMetalTexture(newTexturePointer);
#endif
                return false;
            }

            // End-to-end frame delay 計測: server が「この paint は Unity frame N の
            // BeginFrame に対応する」とマークした N を読み、現在の frameCount との差で
            // 何 Unity フレーム遅れて画面に出るかを測る。0 = 同一フレーム取得 = 0 遅延。
            var paintUnityFrame = _browser.GetAcceleratedPaintUnityFrame();
            if (paintUnityFrame > 0)
            {
                long delta = Time.frameCount - (long)paintUnityFrame;
                if (delta >= -10 && delta < 1000) // delta<0 は理論的にはあり得ないが念のため
                {
                    int deltaFrames = (int)delta;
                    _delaySumFrames += deltaFrames;
                    _delaySampleCount++;
                    if (deltaFrames > _delayMaxFrames) _delayMaxFrames = deltaFrames;
                    if (deltaFrames < _delayMinFrames) _delayMinFrames = deltaFrames;
                    int bucket = deltaFrames >= 0 && deltaFrames < _delayBuckets.Length ? deltaFrames : _delayBuckets.Length - 1;
                    _delayBuckets[bucket]++;
                    // 生サンプルを 5 件まで保持 (検証用)
                    if (_recentSamples.Count >= 5) _recentSamples.Dequeue();
                    _recentSamples.Enqueue((Time.frameCount, paintUnityFrame, deltaFrames));
                }
            }

            if (_texture == null || _texture.width != width || _texture.height != height)
            {
                if (_texture != null) Destroy(_texture);
                // Windows: 共有テクスチャは DXGI_FORMAT_B8G8R8A8_UNORM_SRGB なので linear=false (sRGB)。
                // macOS: Metal 経路も sRGB 解釈なので linear=false。
                _texture = Texture2D.CreateExternalTexture(width, height, TextureFormat.BGRA32, false, false, newTexturePointer);
                if (_rawImage != null)
                {
                    _rawImage.texture = _texture;
                    _rawImage.uvRect = new Rect(0, 1, 1, -1);
                }
            }
            else
            {
                _texture.UpdateExternalTexture(newTexturePointer);
            }

            var timeAfterUpdate = Time.realtimeSinceStartup;

#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
            // macOS のみ: 前フレームの retain を解放 (Windows は client lib 側で管理)
            if (_lastAcceleratedTexturePointer != IntPtr.Zero)
                Browser.ReleaseMetalTexture(_lastAcceleratedTexturePointer);
            _lastAcceleratedTexturePointer = newTexturePointer;
#endif

            var timeAfterRelease = Time.realtimeSinceStartup;

            _acceleratedProfilingCount++;
            _acceleratedProfilingReceiveTotal += timeAfterReceive - timeStart;
            _acceleratedProfilingUpdateTotal += timeAfterUpdate - timeAfterReceive;
            _acceleratedProfilingReleaseTotal += timeAfterRelease - timeAfterUpdate;

            if (_acceleratedProfilingCount >= 120)
            {
                if (_enableLog) CefLog.Log($"[CefUnity-Prof] C# accel x{_acceleratedProfilingCount}: recv={_acceleratedProfilingReceiveTotal * 1000f:F2}ms update={_acceleratedProfilingUpdateTotal * 1000f:F2}ms release={_acceleratedProfilingReleaseTotal * 1000f:F2}ms total={(_acceleratedProfilingReceiveTotal + _acceleratedProfilingUpdateTotal + _acceleratedProfilingReleaseTotal) * 1000f:F2}ms");
                _acceleratedProfilingCount = 0;
                _acceleratedProfilingReceiveTotal = _acceleratedProfilingUpdateTotal = _acceleratedProfilingReleaseTotal = 0;
            }
            _textureUpdatedFrame = Time.frameCount;
            DiagnosticsTexturesApplied++;
            return true;
        }

        private void UpdateTextureSoftware()
        {
            // TryGetBuffer は新しいフレームがある場合のみ true を返す
            if (!_browser.TryGetBuffer(out var buffer, out var width, out var height))
                return;

            if (width <= 0 || height <= 0) return;

            if (_texture == null || _texture.width != width || _texture.height != height)
            {
                // 古いテクスチャを破棄して GPU メモリリークを防ぐ
                if (_texture != null)
                    Destroy(_texture);

                _texture = new Texture2D(width, height, TextureFormat.BGRA32, false);
                if (_rawImage != null)
                {
                    _rawImage.texture = _texture;
                    _rawImage.uvRect = new Rect(0, 1, 1, -1);
                }
            }

            unsafe
            {
                fixed (byte* pointer = buffer)
                {
                    _texture.LoadRawTextureData((IntPtr)pointer, buffer.Length);
                }
            }

            _texture.Apply(false);
            DiagnosticsTexturesApplied++;
        }

        // -----------------------------------------------------------------------
        // OS Settings
        // -----------------------------------------------------------------------

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