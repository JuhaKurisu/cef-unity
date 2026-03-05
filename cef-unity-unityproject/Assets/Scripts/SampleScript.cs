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

    private void Start()
    {
        try
        {
            CefRuntime.Init();
            _browser = new Browser(_width, _height, _url);
            Debug.Log("[CefUnity] Initialized");
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

    // -----------------------------------------------------------------------
    // CEF modifier flags (cef_event_flags_t)
    // -----------------------------------------------------------------------
    private const uint EVENTFLAG_SHIFT_DOWN   = 1 << 1;
    private const uint EVENTFLAG_CONTROL_DOWN = 1 << 2;
    private const uint EVENTFLAG_ALT_DOWN     = 1 << 3;
    private const uint EVENTFLAG_COMMAND_DOWN  = 1 << 7; // macOS Cmd
    private const uint EVENTFLAG_IS_KEY_PAD   = 1 << 9;

    private uint GetCefModifiers()
    {
        uint m = 0;
        if (Input.GetKey(KeyCode.LeftShift)   || Input.GetKey(KeyCode.RightShift))   m |= EVENTFLAG_SHIFT_DOWN;
        if (Input.GetKey(KeyCode.LeftControl) || Input.GetKey(KeyCode.RightControl)) m |= EVENTFLAG_CONTROL_DOWN;
        if (Input.GetKey(KeyCode.LeftAlt)     || Input.GetKey(KeyCode.RightAlt))     m |= EVENTFLAG_ALT_DOWN;
        if (Input.GetKey(KeyCode.LeftCommand) || Input.GetKey(KeyCode.RightCommand)) m |= EVENTFLAG_COMMAND_DOWN;
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
            _browser.SendMouseClick(bx, by, cefButton, false, modifiers: mods);
        if (Input.GetMouseButtonUp(unityButton))
            _browser.SendMouseClick(bx, by, cefButton, true, modifiers: mods);
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

        bx = Mathf.Clamp((int)(nx * _width), 0, _width - 1);
        by = Mathf.Clamp((int)(ny * _height), 0, _height - 1);
        return true;
    }

    // -----------------------------------------------------------------------
    // Keyboard
    // -----------------------------------------------------------------------

    // Unity KeyCode → Windows Virtual-Key の対応テーブル (非印字キー)
    private static readonly (KeyCode unity, int vk)[] SpecialKeyTable =
    {
        // 制御キー
        (KeyCode.Backspace,   0x08), // VK_BACK
        (KeyCode.Tab,         0x09), // VK_TAB
        (KeyCode.Return,      0x0D), // VK_RETURN
        (KeyCode.Escape,      0x1B), // VK_ESCAPE
        (KeyCode.Delete,      0x2E), // VK_DELETE
        (KeyCode.Insert,      0x2D), // VK_INSERT

        // ナビゲーション
        (KeyCode.UpArrow,     0x26), // VK_UP
        (KeyCode.DownArrow,   0x28), // VK_DOWN
        (KeyCode.LeftArrow,   0x25), // VK_LEFT
        (KeyCode.RightArrow,  0x27), // VK_RIGHT
        (KeyCode.Home,        0x24), // VK_HOME
        (KeyCode.End,         0x23), // VK_END
        (KeyCode.PageUp,      0x21), // VK_PRIOR
        (KeyCode.PageDown,    0x22), // VK_NEXT

        // ファンクションキー
        (KeyCode.F1,  0x70), (KeyCode.F2,  0x71), (KeyCode.F3,  0x72),
        (KeyCode.F4,  0x73), (KeyCode.F5,  0x74), (KeyCode.F6,  0x75),
        (KeyCode.F7,  0x76), (KeyCode.F8,  0x77), (KeyCode.F9,  0x78),
        (KeyCode.F10, 0x79), (KeyCode.F11, 0x7A), (KeyCode.F12, 0x7B),

        // テンキー (文字は inputString で処理されるが Down/Up は必要)
        (KeyCode.Keypad0, 0x60), (KeyCode.Keypad1, 0x61), (KeyCode.Keypad2, 0x62),
        (KeyCode.Keypad3, 0x63), (KeyCode.Keypad4, 0x64), (KeyCode.Keypad5, 0x65),
        (KeyCode.Keypad6, 0x66), (KeyCode.Keypad7, 0x67), (KeyCode.Keypad8, 0x68),
        (KeyCode.Keypad9, 0x69),
        (KeyCode.KeypadPeriod,   0x6E), // VK_DECIMAL
        (KeyCode.KeypadDivide,   0x6F), // VK_DIVIDE
        (KeyCode.KeypadMultiply, 0x6A), // VK_MULTIPLY
        (KeyCode.KeypadMinus,    0x6D), // VK_SUBTRACT
        (KeyCode.KeypadPlus,     0x6B), // VK_ADD
        (KeyCode.KeypadEnter,    0x0D), // VK_RETURN

        // 修飾キー (CEF にも Down/Up を通知)
        (KeyCode.LeftShift,    0x10), // VK_SHIFT
        (KeyCode.RightShift,   0x10),
        (KeyCode.LeftControl,  0x11), // VK_CONTROL
        (KeyCode.RightControl, 0x11),
        (KeyCode.LeftAlt,      0x12), // VK_MENU
        (KeyCode.RightAlt,     0x12),
        (KeyCode.LeftCommand,  0x5B), // VK_LWIN (macOS Cmd)
        (KeyCode.RightCommand, 0x5C), // VK_RWIN
        (KeyCode.CapsLock,     0x14), // VK_CAPITAL
    };

    private void HandleKeyboardInput()
    {
        if (_browser == null) return;

        var mods = GetCefModifiers();

        // 1) 印字可能文字 — Input.inputString 経由 (RAWKEYDOWN + CHAR + KEYUP)
        foreach (var c in Input.inputString)
        {
            if (char.IsControl(c)) continue;
            var vk = CharToWindowsVk(c);
            _browser.SendKeyEvent(KeyEventType.RawKeyDown, vk, modifiers: mods, character: c, unmodifiedCharacter: c);
            _browser.SendKeyEvent(KeyEventType.Char, c, modifiers: mods, character: c, unmodifiedCharacter: c);
            _browser.SendKeyEvent(KeyEventType.KeyUp, vk, modifiers: mods, character: c, unmodifiedCharacter: c);
        }

        // 2) 非印字キー — GetKeyDown / GetKeyUp (RAWKEYDOWN / KEYUP のみ)
        foreach (var (key, vk) in SpecialKeyTable)
        {
            if (Input.GetKeyDown(key))
                _browser.SendKeyEvent(KeyEventType.RawKeyDown, vk, modifiers: mods);
            if (Input.GetKeyUp(key))
                _browser.SendKeyEvent(KeyEventType.KeyUp, vk, modifiers: mods);
        }
    }

    private static int CharToWindowsVk(char c)
    {
        return c switch
        {
            >= 'a' and <= 'z' => c - 32, // VK_A..VK_Z (0x41-0x5A)
            >= 'A' and <= 'Z' => c,       // VK_A..VK_Z
            >= '0' and <= '9' => c,        // VK_0..VK_9 (0x30-0x39)
            ' ' => 0x20,                   // VK_SPACE
            ';' or ':' => 0xBA,            // VK_OEM_1
            '=' or '+' => 0xBB,            // VK_OEM_PLUS
            ',' or '<' => 0xBC,            // VK_OEM_COMMA
            '-' or '_' => 0xBD,            // VK_OEM_MINUS
            '.' or '>' => 0xBE,            // VK_OEM_PERIOD
            '/' or '?' => 0xBF,            // VK_OEM_2
            '`' or '~' => 0xC0,            // VK_OEM_3
            '[' or '{' => 0xDB,            // VK_OEM_4
            '\\' or '|' => 0xDC,           // VK_OEM_5
            ']' or '}' => 0xDD,            // VK_OEM_6
            '\'' or '"' => 0xDE,           // VK_OEM_7
            _ => c,
        };
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