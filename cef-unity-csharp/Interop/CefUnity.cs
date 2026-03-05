using System.Text;
using CefUnity;

namespace Interop;

public enum MouseButton : byte
{
    Left = 0,
    Middle = 1,
    Right = 2,
}

public enum KeyEventType : byte
{
    RawKeyDown = 0,
    KeyUp = 1,
    Char = 2,
}

/// <summary>
/// CEF が要求するキーコード情報。
/// Windows 仮想キーコード、macOS ネイティブキーコード、文字値を保持する。
/// </summary>
public readonly struct CefKeyCode
{
    public readonly int WindowsKeyCode;
    public readonly int NativeKeyCode;
    public readonly char Character;

    public CefKeyCode(int windowsKeyCode, int nativeKeyCode, char character)
    {
        WindowsKeyCode = windowsKeyCode;
        NativeKeyCode = nativeKeyCode;
        Character = character;
    }
}

[Flags]
public enum CefEventFlags : uint
{
    None          = 0,
    CapsLockOn    = 1 << 0,
    ShiftDown     = 1 << 1,
    ControlDown   = 1 << 2,
    AltDown       = 1 << 3,
    LeftMouseDown = 1 << 4,
    MiddleMouseDown = 1 << 5,
    RightMouseDown = 1 << 6,
    CommandDown   = 1 << 7,
    NumLockOn     = 1 << 8,
    IsKeyPad      = 1 << 9,
    IsLeft        = 1 << 10,
    IsRight       = 1 << 11,
}

public static class CefKeyCodes
{
    public static readonly CefKeyCode Backspace = new(0x08,  51, '\u007F');
    public static readonly CefKeyCode Tab       = new(0x09,  48, '\t');
    public static readonly CefKeyCode Return    = new(0x0D,  36, '\r');
    public static readonly CefKeyCode Escape    = new(0x1B,  53, '\u001B');
    public static readonly CefKeyCode Delete    = new(0x2E, 117, '\uF728');
    public static readonly CefKeyCode Insert    = new(0x2D, 114, '\uF727');

    public static readonly CefKeyCode UpArrow    = new(0x26, 126, '\uF700');
    public static readonly CefKeyCode DownArrow  = new(0x28, 125, '\uF701');
    public static readonly CefKeyCode LeftArrow  = new(0x25, 123, '\uF702');
    public static readonly CefKeyCode RightArrow = new(0x27, 124, '\uF703');
    public static readonly CefKeyCode Home       = new(0x24, 115, '\uF729');
    public static readonly CefKeyCode End        = new(0x23, 119, '\uF72B');
    public static readonly CefKeyCode PageUp     = new(0x21, 116, '\uF72C');
    public static readonly CefKeyCode PageDown   = new(0x22, 121, '\uF72D');

    public static readonly CefKeyCode F1  = new(0x70, 122, '\uF704');
    public static readonly CefKeyCode F2  = new(0x71, 120, '\uF705');
    public static readonly CefKeyCode F3  = new(0x72,  99, '\uF706');
    public static readonly CefKeyCode F4  = new(0x73, 118, '\uF707');
    public static readonly CefKeyCode F5  = new(0x74,  96, '\uF708');
    public static readonly CefKeyCode F6  = new(0x75,  97, '\uF709');
    public static readonly CefKeyCode F7  = new(0x76,  98, '\uF70A');
    public static readonly CefKeyCode F8  = new(0x77, 100, '\uF70B');
    public static readonly CefKeyCode F9  = new(0x78, 101, '\uF70C');
    public static readonly CefKeyCode F10 = new(0x79, 109, '\uF70D');
    public static readonly CefKeyCode F11 = new(0x7A, 103, '\uF70E');
    public static readonly CefKeyCode F12 = new(0x7B, 111, '\uF70F');

    public static readonly CefKeyCode Keypad0        = new(0x60, 82, '0');
    public static readonly CefKeyCode Keypad1        = new(0x61, 83, '1');
    public static readonly CefKeyCode Keypad2        = new(0x62, 84, '2');
    public static readonly CefKeyCode Keypad3        = new(0x63, 85, '3');
    public static readonly CefKeyCode Keypad4        = new(0x64, 86, '4');
    public static readonly CefKeyCode Keypad5        = new(0x65, 87, '5');
    public static readonly CefKeyCode Keypad6        = new(0x66, 88, '6');
    public static readonly CefKeyCode Keypad7        = new(0x67, 89, '7');
    public static readonly CefKeyCode Keypad8        = new(0x68, 91, '8');
    public static readonly CefKeyCode Keypad9        = new(0x69, 92, '9');
    public static readonly CefKeyCode KeypadPeriod   = new(0x6E, 65, '.');
    public static readonly CefKeyCode KeypadDivide   = new(0x6F, 75, '/');
    public static readonly CefKeyCode KeypadMultiply = new(0x6A, 67, '*');
    public static readonly CefKeyCode KeypadMinus    = new(0x6D, 78, '-');
    public static readonly CefKeyCode KeypadPlus     = new(0x6B, 69, '+');
    public static readonly CefKeyCode KeypadEnter    = new(0x0D, 76, '\r');

    public static readonly CefKeyCode LeftShift    = new(0x10, 56, '\0');
    public static readonly CefKeyCode RightShift   = new(0x10, 60, '\0');
    public static readonly CefKeyCode LeftControl  = new(0x11, 59, '\0');
    public static readonly CefKeyCode RightControl = new(0x11, 62, '\0');
    public static readonly CefKeyCode LeftAlt      = new(0x12, 58, '\0');
    public static readonly CefKeyCode RightAlt     = new(0x12, 61, '\0');
    public static readonly CefKeyCode LeftCommand  = new(0x5B, 55, '\0');
    public static readonly CefKeyCode RightCommand = new(0x5C, 54, '\0');
    public static readonly CefKeyCode CapsLock     = new(0x14, 57, '\0');

    public static int CharToWindowsVk(char c)
    {
        return c switch
        {
            >= 'a' and <= 'z' => c - 32,
            >= 'A' and <= 'Z' => c,
            >= '0' and <= '9' => c,
            ' '  => 0x20,
            ';' or ':' => 0xBA,
            '=' or '+' => 0xBB,
            ',' or '<' => 0xBC,
            '-' or '_' => 0xBD,
            '.' or '>' => 0xBE,
            '/' or '?' => 0xBF,
            '`' or '~' => 0xC0,
            '[' or '{' => 0xDB,
            '\\' or '|' => 0xDC,
            ']' or '}' => 0xDD,
            '\'' or '"' => 0xDE,
            _ => c,
        };
    }
}

public static class CefRuntime
{
    public static void Init()
    {
        var result = NativeMethods.cef_unity_init();
        if (result != 0)
            throw new InvalidOperationException($"CEF initialization failed (code {result})");
    }

    public static void Shutdown()
    {
        NativeMethods.cef_unity_shutdown();
    }

    public static void Pump()
    {
        NativeMethods.cef_unity_pump();
    }
}

public sealed class Browser : IDisposable
{
    private unsafe CefUnityBrowser* _handle;
    private bool _disposed;

    public Browser(int width, int height, string url)
    {
        unsafe
        {
            fixed (byte* urlPtr = ToUtf8Null(url))
            {
                _handle = NativeMethods.cef_unity_create_browser(width, height, urlPtr);
            }

            if (_handle == null)
                throw new InvalidOperationException("Failed to create browser");
        }
    }

    public void LoadUrl(string url)
    {
        ThrowIfDisposed();
        unsafe
        {
            fixed (byte* urlPtr = ToUtf8Null(url))
            {
                NativeMethods.cef_unity_load_url(_handle, urlPtr);
            }
        }
    }

    public void Resize(int width, int height)
    {
        ThrowIfDisposed();
        unsafe
        {
            NativeMethods.cef_unity_resize(_handle, width, height);
        }
    }

    /// <summary>
    /// 最新フレームバッファを取得する。
    /// 新しいフレームがあれば BGRA ピクセルデータの ReadOnlySpan を返す。なければ null。
    /// 返された Span は次の GetBuffer 呼び出しまで有効。
    /// </summary>
    public unsafe bool TryGetBuffer(out ReadOnlySpan<byte> buffer, out int width, out int height)
    {
        ThrowIfDisposed();

        byte* bufferPtr;
        int w, h;
        var hasNew = NativeMethods.cef_unity_get_buffer(_handle, &bufferPtr, &w, &h);

        width = w;
        height = h;

        if (w > 0 && h > 0 && bufferPtr != null)
        {
            buffer = new ReadOnlySpan<byte>(bufferPtr, w * h * 4);
        }
        else
        {
            buffer = default;
        }

        return hasNew != 0;
    }

    public void SendMouseMove(int x, int y, uint modifiers = 0)
    {
        ThrowIfDisposed();
        unsafe
        {
            NativeMethods.cef_unity_send_mouse_move(_handle, x, y, modifiers);
        }
    }

    public void SendMouseClick(int x, int y, MouseButton button, bool mouseUp, int clickCount = 1, uint modifiers = 0)
    {
        ThrowIfDisposed();
        unsafe
        {
            NativeMethods.cef_unity_send_mouse_click(_handle, x, y, modifiers, (byte)button, mouseUp ? 1 : 0, clickCount);
        }
    }

    public void SendMouseWheel(int x, int y, int deltaX, int deltaY, uint modifiers = 0)
    {
        ThrowIfDisposed();
        unsafe
        {
            NativeMethods.cef_unity_send_mouse_wheel(_handle, x, y, modifiers, deltaX, deltaY);
        }
    }

    public void SendKeyEvent(
        KeyEventType eventType,
        int windowsKeyCode,
        int nativeKeyCode = 0,
        uint modifiers = 0,
        char character = '\0',
        char unmodifiedCharacter = '\0',
        bool isSystemKey = false,
        bool focusOnEditableField = false)
    {
        ThrowIfDisposed();
        unsafe
        {
            NativeMethods.cef_unity_send_key_event(
                _handle,
                (byte)eventType,
                modifiers,
                windowsKeyCode,
                nativeKeyCode,
                character,
                unmodifiedCharacter,
                isSystemKey ? 1 : 0,
                focusOnEditableField ? 1 : 0);
        }
    }

    public void SendKeyEvent(KeyEventType eventType, CefKeyCode key, uint modifiers = 0)
    {
        SendKeyEvent(eventType, key.WindowsKeyCode, key.NativeKeyCode, modifiers,
            key.Character, key.Character);
    }

    public void SendCharEvent(char c, uint modifiers = 0)
    {
        var vk = CefKeyCodes.CharToWindowsVk(c);
        SendKeyEvent(KeyEventType.RawKeyDown, vk, modifiers: modifiers, character: c, unmodifiedCharacter: c);
        SendKeyEvent(KeyEventType.Char, c, modifiers: modifiers, character: c, unmodifiedCharacter: c);
        SendKeyEvent(KeyEventType.KeyUp, vk, modifiers: modifiers, character: c, unmodifiedCharacter: c);
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;

        unsafe
        {
            if (_handle != null)
            {
                NativeMethods.cef_unity_destroy_browser(_handle);
                _handle = null;
            }
        }
    }

    private void ThrowIfDisposed()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
    }

    /// <summary>
    /// BGRA バッファを RGBA に変換してコピーする。
    /// </summary>
    public static void ConvertBgraToRgba(ReadOnlySpan<byte> bgra, Span<byte> rgba)
    {
        if (bgra.Length != rgba.Length)
            throw new ArgumentException("bgra and rgba must have the same length");

        for (var i = 0; i < bgra.Length; i += 4)
        {
            rgba[i]     = bgra[i + 2]; // R <- B
            rgba[i + 1] = bgra[i + 1]; // G
            rgba[i + 2] = bgra[i];     // B <- R
            rgba[i + 3] = bgra[i + 3]; // A
        }
    }

    private static byte[] ToUtf8Null(string s)
    {
        var bytes = new byte[Encoding.UTF8.GetByteCount(s) + 1];
        Encoding.UTF8.GetBytes(s, bytes);
        // bytes[^1] is already 0 (null terminator)
        return bytes;
    }
}
