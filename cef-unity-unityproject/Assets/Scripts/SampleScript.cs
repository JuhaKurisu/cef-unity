using System;
using CefUnity;
using CefUnity.Interop;
using UnityEngine;
using UnityEngine.UI;

public class SampleScript : MonoBehaviour
{
    [SerializeField] private int _width = 1280;
    [SerializeField] private int _height = 720;
    [SerializeField] private string _url = "https://www.google.com";
    [SerializeField] private RawImage _rawImage;

    private Browser _browser;
    private float _diagTimer;
    private Texture2D _texture;
    private int _lastMouseX = -1;
    private int _lastMouseY = -1;
    private int _currentWidth;
    private int _currentHeight;

    // Double/triple click detection
    private float _lastClickTime;
    private int _lastClickX = -1;
    private int _lastClickY = -1;
    private int _clickCount;
    private const float DoubleClickTime = 0.3f;
    private const int DoubleClickDistance = 4;

    private void Start()
    {
        try
        {
            _currentWidth = Screen.width;
            _currentHeight = Screen.height;
            CefRuntime.Init();
            _browser = new Browser(_currentWidth, _currentHeight, _url);
            Debug.Log($"[CefUnity] Initialized ({_currentWidth}x{_currentHeight})");
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
        }

        CheckScreenResize();
        UpdateTexture();
        HandleMouseInput();
        HandleKeyboardInput();
    }

    private void OnDestroy()
    {
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

    private uint GetCefModifiers()
    {
        uint m = 0;
        if (Input.GetKey(KeyCode.LeftShift)   || Input.GetKey(KeyCode.RightShift))   m |= (uint)CefEventFlags.ShiftDown;
        if (Input.GetKey(KeyCode.LeftControl) || Input.GetKey(KeyCode.RightControl)) m |= (uint)CefEventFlags.ControlDown;
        if (Input.GetKey(KeyCode.LeftAlt)     || Input.GetKey(KeyCode.RightAlt))     m |= (uint)CefEventFlags.AltDown;
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
            _browser.SendMouseWheel(bx, by, (int)(scroll.x * 120), (int)(scroll.y * 120), mods);
    }

    private void HandleButton(int bx, int by, int unityButton, MouseButton cefButton, uint mods)
    {
        if (Input.GetMouseButtonDown(unityButton))
        {
            if (unityButton == 0)
            {
                float now = Time.unscaledTime;
                if (now - _lastClickTime < DoubleClickTime
                    && Math.Abs(bx - _lastClickX) <= DoubleClickDistance
                    && Math.Abs(by - _lastClickY) <= DoubleClickDistance)
                {
                    _clickCount = _clickCount >= 3 ? 1 : _clickCount + 1;
                }
                else
                {
                    _clickCount = 1;
                }
                _lastClickTime = now;
                _lastClickX = bx;
                _lastClickY = by;
            }
            else
            {
                _clickCount = 1;
            }
            _browser.SendMouseClick(bx, by, cefButton, false, clickCount: _clickCount, modifiers: mods);
        }
        if (Input.GetMouseButtonUp(unityButton))
            _browser.SendMouseClick(bx, by, cefButton, true, clickCount: _clickCount, modifiers: mods);
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

    // -----------------------------------------------------------------------
    // Keyboard
    // -----------------------------------------------------------------------

    // Unity KeyCode → CefKeyCode の対応テーブル
    private static readonly (KeyCode unity, CefKeyCode cef)[] SpecialKeyTable =
    {
        (KeyCode.Backspace,      CefKeyCodes.Backspace),
        (KeyCode.Tab,            CefKeyCodes.Tab),
        (KeyCode.Return,         CefKeyCodes.Return),
        (KeyCode.Escape,         CefKeyCodes.Escape),
        (KeyCode.Delete,         CefKeyCodes.Delete),
        (KeyCode.Insert,         CefKeyCodes.Insert),

        (KeyCode.UpArrow,        CefKeyCodes.UpArrow),
        (KeyCode.DownArrow,      CefKeyCodes.DownArrow),
        (KeyCode.LeftArrow,      CefKeyCodes.LeftArrow),
        (KeyCode.RightArrow,     CefKeyCodes.RightArrow),
        (KeyCode.Home,           CefKeyCodes.Home),
        (KeyCode.End,            CefKeyCodes.End),
        (KeyCode.PageUp,         CefKeyCodes.PageUp),
        (KeyCode.PageDown,       CefKeyCodes.PageDown),

        (KeyCode.F1,  CefKeyCodes.F1),  (KeyCode.F2,  CefKeyCodes.F2),
        (KeyCode.F3,  CefKeyCodes.F3),  (KeyCode.F4,  CefKeyCodes.F4),
        (KeyCode.F5,  CefKeyCodes.F5),  (KeyCode.F6,  CefKeyCodes.F6),
        (KeyCode.F7,  CefKeyCodes.F7),  (KeyCode.F8,  CefKeyCodes.F8),
        (KeyCode.F9,  CefKeyCodes.F9),  (KeyCode.F10, CefKeyCodes.F10),
        (KeyCode.F11, CefKeyCodes.F11), (KeyCode.F12, CefKeyCodes.F12),

        (KeyCode.Keypad0, CefKeyCodes.Keypad0), (KeyCode.Keypad1, CefKeyCodes.Keypad1),
        (KeyCode.Keypad2, CefKeyCodes.Keypad2), (KeyCode.Keypad3, CefKeyCodes.Keypad3),
        (KeyCode.Keypad4, CefKeyCodes.Keypad4), (KeyCode.Keypad5, CefKeyCodes.Keypad5),
        (KeyCode.Keypad6, CefKeyCodes.Keypad6), (KeyCode.Keypad7, CefKeyCodes.Keypad7),
        (KeyCode.Keypad8, CefKeyCodes.Keypad8), (KeyCode.Keypad9, CefKeyCodes.Keypad9),
        (KeyCode.KeypadPeriod,   CefKeyCodes.KeypadPeriod),
        (KeyCode.KeypadDivide,   CefKeyCodes.KeypadDivide),
        (KeyCode.KeypadMultiply, CefKeyCodes.KeypadMultiply),
        (KeyCode.KeypadMinus,    CefKeyCodes.KeypadMinus),
        (KeyCode.KeypadPlus,     CefKeyCodes.KeypadPlus),
        (KeyCode.KeypadEnter,    CefKeyCodes.KeypadEnter),

        (KeyCode.LeftShift,    CefKeyCodes.LeftShift),
        (KeyCode.RightShift,   CefKeyCodes.RightShift),
        (KeyCode.LeftControl,  CefKeyCodes.LeftControl),
        (KeyCode.RightControl, CefKeyCodes.RightControl),
        (KeyCode.LeftAlt,      CefKeyCodes.LeftAlt),
        (KeyCode.RightAlt,     CefKeyCodes.RightAlt),
        (KeyCode.LeftCommand,  CefKeyCodes.LeftCommand),
        (KeyCode.RightCommand, CefKeyCodes.RightCommand),
        (KeyCode.CapsLock,     CefKeyCodes.CapsLock),
    };

    private void HandleKeyboardInput()
    {
        if (_browser == null) return;

        var mods = GetCefModifiers();

        // 1) 印字可能文字 — Input.inputString 経由 (RAWKEYDOWN + CHAR + KEYUP)
        foreach (var c in Input.inputString)
        {
            if (char.IsControl(c)) continue;
            _browser.SendCharEvent(c, mods);
        }

        // 2) 非印字キー — GetKeyDown / GetKeyUp (RAWKEYDOWN / KEYUP のみ)
        foreach (var (key, cef) in SpecialKeyTable)
        {
            if (Input.GetKeyDown(key))
                _browser.SendKeyEvent(KeyEventType.RawKeyDown, cef, mods);
            if (Input.GetKeyUp(key))
                _browser.SendKeyEvent(KeyEventType.KeyUp, cef, mods);
        }

        // 3) Cmd/Ctrl + アルファキー (コピペ等のショートカット)
        //    Input.inputString は修飾キー押下時に文字を返さないため別途処理
        if ((mods & (uint)CefEventFlags.CommandDown) != 0 || (mods & (uint)CefEventFlags.ControlDown) != 0)
        {
            for (var kc = KeyCode.A; kc <= KeyCode.Z; kc++)
            {
                if (Input.GetKeyDown(kc))
                {
                    int vk = 0x41 + (kc - KeyCode.A);
                    _browser.SendKeyEvent(KeyEventType.RawKeyDown, vk, modifiers: mods);
                }
                if (Input.GetKeyUp(kc))
                {
                    int vk = 0x41 + (kc - KeyCode.A);
                    _browser.SendKeyEvent(KeyEventType.KeyUp, vk, modifiers: mods);
                }
            }
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