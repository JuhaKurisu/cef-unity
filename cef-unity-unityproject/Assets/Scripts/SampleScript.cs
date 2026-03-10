using System;
using System.Collections.Generic;
using CefUnity;
using CefUnity.Interop;
using TMPro;
using UnityEngine;
using UnityEngine.UI;

public class SampleScript : MonoBehaviour
{
    private const float DoubleClickTime = 0.3f;
    private const int DoubleClickDistance = 4;
    private const float KeyRepeatDelay = 0.5f;
    private const float KeyRepeatRate = 0.035f;


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

    [SerializeField] private int _width = 1280;
    [SerializeField] private int _height = 720;
    [SerializeField] private string _url;
    [SerializeField] private RawImage _rawImage;

    private Browser _browser;
    private int _clickCount;
    private int _currentHeight;
    private int _currentWidth;
    private float _diagTimer;
    private bool _imeActive;
    private bool _imeCommitPending;
    private bool _imeSuppressKeys;

    // IME proxy
    private TMP_InputField _imeProxy;

    // Double/triple click detection
    private float _lastClickTime;
    private int _lastClickX = -1;
    private int _lastClickY = -1;
    private string _lastComposition = "";
    private int _lastMouseX = -1;
    private int _lastMouseY = -1;
    private readonly Dictionary<KeyCode, float> _keyDownTime = new();
    private string _imeDebugText = "";
    private readonly Dictionary<KeyCode, float> _keyLastRepeat = new();
    private Texture2D _texture;

    private void Start()
    {
        try
        {
            _currentWidth = Screen.width;
            _currentHeight = Screen.height;
            CefRuntime.Init();
            _browser = new Browser(_currentWidth, _currentHeight, _url);
            Debug.Log($"[CefUnity] Initialized ({_currentWidth}x{_currentHeight})");
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

        _diagTimer += Time.deltaTime;
        if (_diagTimer >= 2f)
        {
            _diagTimer = 0f;
            var paintCount = NativeMethods.cef_unity_get_paint_count();
            var pumpCount = NativeMethods.cef_unity_get_pump_count();
            Debug.Log($"[CefUnity] diag: paint={paintCount} pump={pumpCount}");

            var logs = CefRuntime.GetLogs();
            foreach (var line in logs)
                Debug.Log($"[CefServer] {line}");
        }

        CheckScreenResize();
        UpdateTexture();
        HandleMouseInput();
        HandleImeInput();
        HandleKeyboardInput();
    }

    private void OnDestroy()
    {
        if (_imeProxy != null)
        {
            Destroy(_imeProxy.gameObject);
            _imeProxy = null;
        }

        _browser?.Dispose();
        _browser = null;

        if (_texture != null)
        {
            Destroy(_texture);
            _texture = null;
        }

        CefRuntime.Shutdown();
        Debug.Log("[CefUnity] Shutdown");
    }

    // -----------------------------------------------------------------------
    // IME
    // -----------------------------------------------------------------------
    private void SetupImeProxy()
    {
        var canvas = _rawImage.canvas;

        var go = new GameObject("ImeProxy");
        go.transform.SetParent(canvas.transform, false);

        var rt = go.AddComponent<RectTransform>();
        rt.anchorMin = Vector2.zero;
        rt.anchorMax = Vector2.zero;
        rt.anchoredPosition = Vector2.zero;
        rt.sizeDelta = new Vector2(1, 1);

        var bg = go.AddComponent<Image>();
        bg.color = Color.clear;
        bg.raycastTarget = false;

        // Text Area (TMP_InputField がクリッピング領域として使う)
        var textAreaGo = new GameObject("TextArea");
        textAreaGo.transform.SetParent(go.transform, false);
        var textAreaRt = textAreaGo.AddComponent<RectTransform>();
        textAreaRt.anchorMin = Vector2.zero;
        textAreaRt.anchorMax = Vector2.one;
        textAreaRt.offsetMin = new Vector2(5, 0);
        textAreaRt.offsetMax = new Vector2(-5, 0);
        var textAreaMask = textAreaGo.AddComponent<RectMask2D>();

        // Text
        var textGo = new GameObject("Text");
        textGo.transform.SetParent(textAreaGo.transform, false);

        var textRt = textGo.AddComponent<RectTransform>();
        textRt.anchorMin = Vector2.zero;
        textRt.anchorMax = Vector2.one;
        textRt.offsetMin = Vector2.zero;
        textRt.offsetMax = Vector2.zero;

        var tmp = textGo.AddComponent<TextMeshProUGUI>();
        tmp.fontSize = 1;
        tmp.color = Color.clear;
        tmp.raycastTarget = false;

        _imeProxy = go.AddComponent<TMP_InputField>();
        _imeProxy.textViewport = textAreaRt;
        _imeProxy.textComponent = tmp;
        _imeProxy.onFocusSelectAll = false;
        _imeProxy.ActivateInputField();

        Input.imeCompositionMode = IMECompositionMode.On;
    }

    private void HandleImeInput()
    {
        if (_browser == null) return;

        // IME proxy のフォーカスを維持
        if (_imeProxy != null && !_imeProxy.isFocused)
            _imeProxy.ActivateInputField();

        var comp = Input.compositionString;
        var input = Input.inputString;

        // デバッグ: 変化があるときだけログ出力
        if (!string.IsNullOrEmpty(comp) || !string.IsNullOrEmpty(input) || _imeActive)
        {
            Debug.Log($"[IME] comp=\"{comp}\" input=\"{EscapeForLog(input)}\" imeActive={_imeActive} commitPending={_imeCommitPending}");
        }

        if (!string.IsNullOrEmpty(comp))
        {
            // IME が暗黙的に確定して新しい composition を開始した場合を検出
            // (例: "嗚呼亜" → Enter なしで次の文字 → "あ")
            // この場合 Input.inputString に確定テキストが入っている
            if (_imeActive && !string.IsNullOrEmpty(input))
            {
                var commitSb = new System.Text.StringBuilder();
                foreach (var c in input)
                {
                    if (!char.IsControl(c))
                        commitSb.Append(c);
                }
                if (commitSb.Length > 0)
                {
                    var commitText = commitSb.ToString();
                    Debug.Log($"[IME] → implicit commit: ImeCommitText(\"{commitText}\") before new composition");
                    _browser.ImeCommitText(commitText);
                }
            }

            // composition 開始/変更
            _browser.ImeSetComposition(comp, (uint)comp.Length, (uint)comp.Length);
            _imeActive = true;
            _lastComposition = comp;
            _imeSuppressKeys = true;

            // CEF から通知されたキャレット位置を IME 候補ウィンドウ位置に反映
            UpdateCompositionCursorPos();
        }
        else if (_imeActive)
        {
            // composition 終了 (非空 → 空に変化)
            var committed = false;
            foreach (var c in input)
            {
                if (!char.IsControl(c))
                {
                    committed = true;
                    break;
                }
            }

            if (committed)
            {
                // 制御文字を除いた確定テキストを取得
                var sb = new System.Text.StringBuilder();
                foreach (var c in input)
                {
                    if (!char.IsControl(c))
                        sb.Append(c);
                }
                var text = sb.ToString();
                Debug.Log($"[IME] → ImeCommitText(\"{text}\")");
                _browser.ImeCommitText(text);
            }
            else
            {
                Debug.Log("[IME] → ImeCancelComposition()");
                _browser.ImeCancelComposition();
            }

            _imeActive = false;
            _lastComposition = "";
            _imeSuppressKeys = true; // 終了フレームもキー抑制

            if (_imeProxy != null)
                _imeProxy.text = "";
        }
        else
        {
            // 通常状態: 次フレームからキー送信を許可
            _imeSuppressKeys = false;
        }
    }

    private static string EscapeForLog(string s)
    {
        if (string.IsNullOrEmpty(s)) return "";
        var sb = new System.Text.StringBuilder();
        foreach (var c in s)
        {
            if (char.IsControl(c))
                sb.Append($"\\x{(int)c:X2}");
            else
                sb.Append(c);
        }
        return sb.ToString();
    }

    private void UpdateCompositionCursorPos()
    {
        if (_browser == null || _rawImage == null) return;

        _browser.GetImeCaret(out var cx, out var cy, out var cw, out var ch);

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

        screenPos.y = Screen.height - screenPos.y;
        Input.compositionCursorPos = screenPos;

        // デバッグマーカー位置更新 (compositionCursorPos の座標)
        _imeDebugMarkerPos = screenPos;
        _imeDebugCefCaret = new Vector4(cx, cy, cw, ch);
    }

    private Vector2 _imeDebugMarkerPos;
    private Vector4 _imeDebugCefCaret;

    private void OnGUI()
    {
        // compositionCursorPos の位置に赤い十字マーカー (OnGUI は Y=0 上端 = compositionCursorPos と同じ座標系)
        var pos = _imeDebugMarkerPos;
        var old = GUI.color;
        GUI.color = Color.red;
        GUI.Box(new UnityEngine.Rect(pos.x - 15, pos.y, 30, 2), GUIContent.none);  // 横線
        GUI.Box(new UnityEngine.Rect(pos.x, pos.y - 15, 2, 30), GUIContent.none);  // 縦線
        GUI.color = old;

        var style = new GUIStyle(GUI.skin.label) { fontSize = 14, normal = { textColor = Color.yellow } };
        var text = $"compositionCursorPos: ({pos.x:F0},{pos.y:F0})  CEF: ({_imeDebugCefCaret.x},{_imeDebugCefCaret.y},{_imeDebugCefCaret.z},{_imeDebugCefCaret.w})  Screen: {Screen.width}x{Screen.height}";
        GUI.Label(new UnityEngine.Rect(10, Screen.height - 30, Screen.width, 30), text, style);
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

        // マウスクリック後に IME proxy のフォーカスを再取得
        if (Input.GetMouseButtonDown(0) || Input.GetMouseButtonDown(1) || Input.GetMouseButtonDown(2))
        {
            if (_imeProxy != null)
                _imeProxy.ActivateInputField();
        }

        var scroll = Input.mouseScrollDelta;
        if (scroll.y != 0f || scroll.x != 0f)
            _browser.SendMouseWheel(bx, by, (int)(scroll.x * 120), (int)(scroll.y * 120), mods);
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
        if (string.IsNullOrEmpty(Input.compositionString) && !_imeCommitPending)
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
        var sw = Screen.width;
        var sh = Screen.height;
        if (sw != _currentWidth || sh != _currentHeight)
        {
            _currentWidth = sw;
            _currentHeight = sh;
            _browser?.Resize(_currentWidth, _currentHeight);
            Debug.Log($"[CefUnity] Resized to {_currentWidth}x{_currentHeight}");
        }
    }

    private void UpdateTexture()
    {
        if (_browser == null) return;

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
}