using System;
using System.Collections.Generic;
using System.Reflection;
using System.Text;
using CefUnity.Interop;
using UnityEditor;
using UnityEngine;
using UnityEngine.Rendering;
using UnityEngine.UI;
#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
using System.Runtime.InteropServices;
#endif

namespace CefUnity.Runtime
{
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

        // Spin/取得失敗の検証用メトリクス
        private int _lateUpdateInvokeCount;     // LateUpdate 自体の呼び出し回数
        private int _spinTimeoutCount;          // spin タイムアウトで諦めた回数
        private int _recvFailCount;             // TryRecv*Texture が false で return した回数
        private double _spinElapsedSumSec;       // spin 経過時間合計
        private double _spinElapsedMaxSec;       // spin 最長
        private int _spinZeroCount;             // spin に入った瞬間に既に条件成立していた回数
        // 最近の生サンプルを保持 (frame_count, paint_unity_frame, delta) でログ出力
        private readonly System.Collections.Generic.Queue<(int fc, ulong pf, int delta)> _recentSamples
            = new System.Collections.Generic.Queue<(int, ulong, int)>();

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
                QualitySettings.vSyncCount = 0;
                Application.targetFrameRate = 60;

                var useGpu = !(SystemInfo.graphicsDeviceType == GraphicsDeviceType.Direct3D12 || SystemInfo.graphicsDeviceType == GraphicsDeviceType.Direct3D11);
                CefRuntime.Init();
                _browser = new Browser(_currentWidth, _currentHeight, _url);

                // 共通: macOS は Mach port 経由の IOSurface、Windows は D3D11 共有テクスチャ。
                // Init() がサーバーを起動し接続を行うため、その後にチェック。
                _useAcceleratedPaint = Browser.IsAcceleratedConnected();
                if (_enableLog) Debug.Log($"[CefUnity] Initialized ({_currentWidth}x{_currentHeight}), acceleratedPaint={_useAcceleratedPaint}");
                SetupImeProxy();
            }
            catch (Exception e)
            {
                Debug.LogError($"[CefUnity] Init failed: {e}");
            }
        }

        private void Update()
        {
            CefRuntime.Pump();

            // フレーム冒頭で BeginFrame を発行 → CEF が Update〜LateUpdate の間に paint。
            // Time.frameCount を一緒に渡すことで、UpdateTexture 時に「何フレーム前の
            // BeginFrame に対応するか」を測定できる。
            if (_browser != null)
            {
                _browser.SendExternalBeginFrame((ulong)Time.frameCount);
            }

            _diagTimer += Time.deltaTime;
            if (_diagTimer >= 2f)
            {
                _diagTimer = 0f;

                if (_enableLog)
                {
                    var paintCount = NativeMethods.cef_unity_get_paint_count();
                    var pumpCount = NativeMethods.cef_unity_get_pump_count();
                    Debug.Log($"[CefUnity] diag: paint={paintCount} pump={pumpCount}");
                    var logs = CefRuntime.GetLogs();
                    foreach (var line in logs)
                        Debug.Log($"[CefServer] {line}");

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
                        Debug.Log(sb.ToString());

                        // 検証メトリクス: LateUpdate 全体に対する取得成功率、spin の挙動、生サンプル
                        var spinAvg = _lateUpdateInvokeCount > 0 ? _spinElapsedSumSec / _lateUpdateInvokeCount * 1000.0 : 0.0;
                        Debug.Log($"[CefUnity] verify: LateUpdate={_lateUpdateInvokeCount} recv_ok={_delaySampleCount} recv_fail={_recvFailCount} spin_zero={_spinZeroCount} spin_timeout={_spinTimeoutCount} spin_avg={spinAvg:F3}ms spin_max={_spinElapsedMaxSec * 1000.0:F3}ms");
                        var sb2 = new StringBuilder("[CefUnity] verify samples (fc, paint_fc, delta):");
                        foreach (var s in _recentSamples)
                            sb2.Append($" ({s.fc},{s.pf},{s.delta})");
                        Debug.Log(sb2.ToString());

                        _delaySampleCount = 0;
                        _delaySumFrames = 0;
                        _delayMaxFrames = 0;
                        _delayMinFrames = int.MaxValue;
                        for (int i = 0; i < _delayBuckets.Length; i++) _delayBuckets[i] = 0;
                        _lateUpdateInvokeCount = 0;
                        _spinTimeoutCount = 0;
                        _recvFailCount = 0;
                        _spinElapsedSumSec = 0;
                        _spinElapsedMaxSec = 0;
                        _spinZeroCount = 0;
                        _recentSamples.Clear();
                    }
                }
            }

            // フレーム前半: 入力を server へ送る。LateUpdate のテクスチャ取得までに
            // server が処理を終えていれば、同一フレーム内で反映される。
            CheckScreenResize();
            HandleMouseInput();
            UpdateCompositionCursorPos();
            HandleImeInput();
            HandleKeyboardInput();
        }

        private void LateUpdate()
        {
            _lateUpdateInvokeCount++;
            // フレーム後半: server が paint した最新フレームを取得する。
            // Update 冒頭で投げた BeginFrame に対する paint がこのタイミングまでに
            // 到着していれば、同一 Unity フレーム内で反映される (0 遅延)。
            UpdateTexture();
        }

        public void LoadUrl(string url)
        {
            _browser.LoadUrl(url);
        }

        private void OnDestroy()
        {
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
            if (_enableLog) Debug.Log("[CefUnity] Shutdown");
        }

        // -----------------------------------------------------------------------
        // IME
        // -----------------------------------------------------------------------
        private void SetupImeProxy()
        {
            Input.imeCompositionMode = IMECompositionMode.On;
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
            }
            else if (_imeActive)
            {
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
            }

            HandleButton(bx, by, 0, MouseButton.Left, mods);
            HandleButton(bx, by, 1, MouseButton.Right, mods);
            HandleButton(bx, by, 2, MouseButton.Middle, mods);

            var scroll = Input.mouseScrollDelta;
            if (scroll.y != 0f || scroll.x != 0f)
                _browser.SendMouseWheel(bx, by, (int)(scroll.x * 60), (int)(scroll.y * 60), mods);
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
            }

            if (Input.GetMouseButtonUp(unityButton))
                _browser.SendMouseClick(bx, by, cefButton, true, _clickCount, mods);
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
                if (Input.GetKeyDown(KeyCode.C)) _browser.Copy();
                if (Input.GetKeyDown(KeyCode.V)) _browser.Paste();
                if (Input.GetKeyDown(KeyCode.X)) _browser.Cut();
                if (Input.GetKeyDown(KeyCode.A)) _browser.SelectAll();
                if (Input.GetKeyDown(KeyCode.Z))
                {
                    if ((mods & (uint)CefEventFlags.ShiftDown) != 0) _browser.Redo();
                    else _browser.Undo();
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
                }
            }

            if (Input.GetKeyUp(unityKey))
            {
                _browser.SendKeyEvent(KeyEventType.KeyUp, cefKey, mods);
                _keyDownTime.Remove(unityKey);
                _keyLastRepeat.Remove(unityKey);
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
                if (_enableLog) Debug.Log($"[CefUnity] Resized to {_currentWidth}x{_currentHeight}");
            }
        }

        private void UpdateTexture()
        {
            if (_browser == null) return;

            if (_useAcceleratedPaint)
            {
                UpdateTextureAccelerated();
                return;
            }

            UpdateTextureSoftware();
        }

        // Profiling for accelerated texture path
        private int _accelProfCount;
        private float _accelProfRecvTotal;
        private float _accelProfUpdateTotal;
        private float _accelProfReleaseTotal;

        private void UpdateTextureAccelerated()
        {
            var t0 = Time.realtimeSinceStartup;

            IntPtr newTexPtr;
            int w, h;
            uint format;

            // 0 遅延を達成するため、Update で投げた BeginFrame (= 現在 Time.frameCount) に対応する
            // paint が shm に届くまで短時間 spin で待つ。これにより BeginFrame 発行〜表示までを
            // 同一 Unity フレーム内に押し込む。
            // タイムアウト 8ms (Unity 120fps の 1 フレーム分)、超えたら諦めて取れる最新を使う。
            // GetAccelPaintUnityFrame は FFI で shm の AtomicU64 を 1 回読むだけなので極めて軽量。
            const float SpinBudgetSec = 0.008f;
            var spinStart = Time.realtimeSinceStartup;
            int currentFrame = Time.frameCount;
            bool spinTimeout = false;
            int spinIterations = 0;
            while (true)
            {
                ulong p = _browser.GetAccelPaintUnityFrame();
                if (p >= (ulong)currentFrame) break;
                spinIterations++;
                if (Time.realtimeSinceStartup - spinStart > SpinBudgetSec) { spinTimeout = true; break; }
            }
            var spinElapsed = Time.realtimeSinceStartup - spinStart;
            _spinElapsedSumSec += spinElapsed;
            if (spinElapsed > _spinElapsedMaxSec) _spinElapsedMaxSec = spinElapsed;
            if (spinIterations == 0) _spinZeroCount++; // 一回も loop せず即 break = ほぼノーコスト
            if (spinTimeout) _spinTimeoutCount++;

#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
            // macOS: IOSurface 経由で毎フレーム新しい Metal テクスチャを受信 → Release が必要
            if (!Browser.TryRecvIOSurfaceTexture(out newTexPtr, out w, out h, out format))
            {
                _recvFailCount++;
                return;
            }
#elif UNITY_STANDALONE_WIN || UNITY_EDITOR_WIN
            // Windows: Unity の graphics backend に応じて D3D11/D3D12 を使い分け。
            // ポインタはサイズ変更時以外は安定 (client lib 側でキャッシュ管理)、Release 不要。
            if (_browser == null) return;
            var gotFrame = SystemInfo.graphicsDeviceType == GraphicsDeviceType.Direct3D12
                ? _browser.TryRecvD3D12Texture(out newTexPtr, out w, out h, out format)
                : _browser.TryRecvD3D11Texture(out newTexPtr, out w, out h, out format);
            if (!gotFrame) { _recvFailCount++; return; }
#else
        return;
#endif

            var t1 = Time.realtimeSinceStartup;

            if (w <= 0 || h <= 0)
            {
#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
                Browser.ReleaseMetalTexture(newTexPtr);
#endif
                return;
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
                if (_enableLog) Debug.Log($"[CefUnity-Prof] C# accel x{_accelProfCount}: recv={_accelProfRecvTotal * 1000f:F2}ms update={_accelProfUpdateTotal * 1000f:F2}ms release={_accelProfReleaseTotal * 1000f:F2}ms total={(_accelProfRecvTotal + _accelProfUpdateTotal + _accelProfReleaseTotal) * 1000f:F2}ms");
                _accelProfCount = 0;
                _accelProfRecvTotal = _accelProfUpdateTotal = _accelProfReleaseTotal = 0;
            }
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