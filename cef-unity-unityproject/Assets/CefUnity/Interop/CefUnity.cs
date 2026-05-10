using System;
using System.Text;

namespace CefUnity.Interop
{
    public enum MouseButton : byte
    {
        Left = 0,
        Middle = 1,
        Right = 2
    }

    public enum KeyEventType : byte
    {
        RawKeyDown = 0,
        KeyUp = 1,
        Char = 2
    }

    /// <summary>
    ///     CEF が要求するキーコード情報。
    ///     Windows 仮想キーコード、macOS ネイティブキーコード、文字値を保持する。
    /// </summary>
    public readonly struct CefKeyCode
    {
        /// <summary>Windows 仮想キーコード (VK_*)</summary>
        public readonly int WindowsKeyCode;

        /// <summary>macOS ネイティブキーコード (kVK_*)</summary>
        public readonly int NativeKeyCode;

        /// <summary>CEF が要求する文字値 (macOS の NSEvent.characters に対応)</summary>
        public readonly char Character;

        public CefKeyCode(int windowsKeyCode, int nativeKeyCode, char character)
        {
            WindowsKeyCode = windowsKeyCode;
            NativeKeyCode = nativeKeyCode;
            Character = character;
        }
    }

    /// <summary>
    ///     CEF modifier flags (cef_event_flags_t)。
    ///     マウス・キーイベントの modifiers パラメータに使用する。
    /// </summary>
    [Flags]
    public enum CefEventFlags : uint
    {
        None = 0,
        CapsLockOn = 1 << 0,
        ShiftDown = 1 << 1,
        ControlDown = 1 << 2,
        AltDown = 1 << 3,
        LeftMouseDown = 1 << 4,
        MiddleMouseDown = 1 << 5,
        RightMouseDown = 1 << 6,
        CommandDown = 1 << 7, // macOS Cmd
        NumLockOn = 1 << 8,
        IsKeyPad = 1 << 9,
        IsLeft = 1 << 10,
        IsRight = 1 << 11
    }

    /// <summary>
    ///     非印字キーの CEF キーコード定義。
    ///     プラットフォーム固有の VK / native keycode / character をライブラリ側で管理する。
    /// </summary>
    public static class CefKeyCodes
    {
        // 制御キー
        public static readonly CefKeyCode Backspace = new(0x08, 51, '\u007F'); // NSDeleteCharacter
        public static readonly CefKeyCode Tab = new(0x09, 48, '\t');
        public static readonly CefKeyCode Return = new(0x0D, 36, '\r');
        public static readonly CefKeyCode Escape = new(0x1B, 53, '\u001B');
        public static readonly CefKeyCode Delete = new(0x2E, 117, '\uF728'); // NSDeleteFunctionKey
        public static readonly CefKeyCode Insert = new(0x2D, 114, '\uF727'); // NSInsertFunctionKey

        // ナビゲーション
        public static readonly CefKeyCode UpArrow = new(0x26, 126, '\uF700');
        public static readonly CefKeyCode DownArrow = new(0x28, 125, '\uF701');
        public static readonly CefKeyCode LeftArrow = new(0x25, 123, '\uF702');
        public static readonly CefKeyCode RightArrow = new(0x27, 124, '\uF703');
        public static readonly CefKeyCode Home = new(0x24, 115, '\uF729');
        public static readonly CefKeyCode End = new(0x23, 119, '\uF72B');
        public static readonly CefKeyCode PageUp = new(0x21, 116, '\uF72C');
        public static readonly CefKeyCode PageDown = new(0x22, 121, '\uF72D');

        // ファンクションキー
        public static readonly CefKeyCode F1 = new(0x70, 122, '\uF704');
        public static readonly CefKeyCode F2 = new(0x71, 120, '\uF705');
        public static readonly CefKeyCode F3 = new(0x72, 99, '\uF706');
        public static readonly CefKeyCode F4 = new(0x73, 118, '\uF707');
        public static readonly CefKeyCode F5 = new(0x74, 96, '\uF708');
        public static readonly CefKeyCode F6 = new(0x75, 97, '\uF709');
        public static readonly CefKeyCode F7 = new(0x76, 98, '\uF70A');
        public static readonly CefKeyCode F8 = new(0x77, 100, '\uF70B');
        public static readonly CefKeyCode F9 = new(0x78, 101, '\uF70C');
        public static readonly CefKeyCode F10 = new(0x79, 109, '\uF70D');
        public static readonly CefKeyCode F11 = new(0x7A, 103, '\uF70E');
        public static readonly CefKeyCode F12 = new(0x7B, 111, '\uF70F');

        // テンキー
        public static readonly CefKeyCode Keypad0 = new(0x60, 82, '0');
        public static readonly CefKeyCode Keypad1 = new(0x61, 83, '1');
        public static readonly CefKeyCode Keypad2 = new(0x62, 84, '2');
        public static readonly CefKeyCode Keypad3 = new(0x63, 85, '3');
        public static readonly CefKeyCode Keypad4 = new(0x64, 86, '4');
        public static readonly CefKeyCode Keypad5 = new(0x65, 87, '5');
        public static readonly CefKeyCode Keypad6 = new(0x66, 88, '6');
        public static readonly CefKeyCode Keypad7 = new(0x67, 89, '7');
        public static readonly CefKeyCode Keypad8 = new(0x68, 91, '8');
        public static readonly CefKeyCode Keypad9 = new(0x69, 92, '9');
        public static readonly CefKeyCode KeypadPeriod = new(0x6E, 65, '.');
        public static readonly CefKeyCode KeypadDivide = new(0x6F, 75, '/');
        public static readonly CefKeyCode KeypadMultiply = new(0x6A, 67, '*');
        public static readonly CefKeyCode KeypadMinus = new(0x6D, 78, '-');
        public static readonly CefKeyCode KeypadPlus = new(0x6B, 69, '+');
        public static readonly CefKeyCode KeypadEnter = new(0x0D, 76, '\r');

        // 修飾キー
        public static readonly CefKeyCode LeftShift = new(0x10, 56, '\0');
        public static readonly CefKeyCode RightShift = new(0x10, 60, '\0');
        public static readonly CefKeyCode LeftControl = new(0x11, 59, '\0');
        public static readonly CefKeyCode RightControl = new(0x11, 62, '\0');
        public static readonly CefKeyCode LeftAlt = new(0x12, 58, '\0');
        public static readonly CefKeyCode RightAlt = new(0x12, 61, '\0');
        public static readonly CefKeyCode LeftCommand = new(0x5B, 55, '\0');
        public static readonly CefKeyCode RightCommand = new(0x5C, 54, '\0');
        public static readonly CefKeyCode CapsLock = new(0x14, 57, '\0');

        /// <summary>
        ///     印字可能文字の Windows 仮想キーコードを返す。
        /// </summary>
        public static int CharToWindowsVk(char c)
        {
            return c switch
            {
                >= 'a' and <= 'z' => c - 32, // VK_A..VK_Z (0x41-0x5A)
                >= 'A' and <= 'Z' => c,
                >= '0' and <= '9' => c, // VK_0..VK_9 (0x30-0x39)
                ' ' => 0x20,
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
                _ => c
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

        /// <summary>
        ///     CEF メッセージループを駆動する。毎フレーム、メインスレッドから呼ぶこと。
        /// </summary>
        public static void Pump()
        {
            NativeMethods.cef_unity_pump();
        }

        public static string[] GetLogs()
        {
            unsafe
            {
                var required = NativeMethods.cef_unity_get_logs(null, 0);
                if (required <= 1) return Array.Empty<string>();
                var buffer = new byte[required];
                fixed (byte* ptr = buffer)
                {
                    var written = NativeMethods.cef_unity_get_logs(ptr, buffer.Length);
                    if (written <= 1) return Array.Empty<string>();
                    var raw = Encoding.UTF8.GetString(buffer, 0, written - 1);
                    return raw.Split('\0', StringSplitOptions.RemoveEmptyEntries);
                }
            }
        }
    }

    public sealed class Browser : IDisposable
    {
        private bool _disposed;
        private unsafe CefUnityBrowser* _handle;

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
        ///     最新フレームバッファを取得する。
        ///     新しいフレームがあれば BGRA ピクセルデータの ReadOnlySpan を返す。なければ null。
        ///     返された Span は次の GetBuffer 呼び出しまで有効。
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
                buffer = new ReadOnlySpan<byte>(bufferPtr, w * h * 4);
            else
                buffer = default;

            return hasNew != 0;
        }

        public void EditCommand(byte command)
        {
            ThrowIfDisposed();
            unsafe
            {
                NativeMethods.cef_unity_edit_command(_handle, command);
            }
        }

        public void Copy()
        {
            EditCommand(0);
        }

        public void Paste()
        {
            EditCommand(1);
        }

        public void Cut()
        {
            EditCommand(2);
        }

        public void SelectAll()
        {
            EditCommand(3);
        }

        public void Undo()
        {
            EditCommand(4);
        }

        public void Redo()
        {
            EditCommand(5);
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

        /// <summary>
        ///     CefKeyCode を使って非印字キーの RAWKEYDOWN / KEYUP を送信する。
        /// </summary>
        public void SendKeyEvent(KeyEventType eventType, CefKeyCode key, uint modifiers = 0)
        {
            SendKeyEvent(eventType, key.WindowsKeyCode, key.NativeKeyCode, modifiers,
                key.Character, key.Character);
        }

        /// <summary>
        ///     印字可能文字の RAWKEYDOWN + CHAR + KEYUP を一括送信する。
        /// </summary>
        public void SendCharEvent(char c, uint modifiers = 0)
        {
            var vk = CefKeyCodes.CharToWindowsVk(c);
            SendKeyEvent(KeyEventType.RawKeyDown, vk, modifiers: modifiers, character: c, unmodifiedCharacter: c);
            SendKeyEvent(KeyEventType.Char, c, modifiers: modifiers, character: c, unmodifiedCharacter: c);
            SendKeyEvent(KeyEventType.KeyUp, vk, modifiers: modifiers, character: c, unmodifiedCharacter: c);
        }

        // ----- IOSurface / Metal texture -----

        /// <summary>
        ///     IOSurface 経由の新しい accelerated paint フレームがあるか確認する。
        ///     新フレームがあれば true を返し、surface_id/width/height/format を設定する。
        /// </summary>
        public unsafe bool TryGetIOSurfaceInfo(out uint surfaceId, out int width, out int height, out uint format)
        {
            ThrowIfDisposed();
            uint sid;
            int w, h;
            uint fmt;
            var result = NativeMethods.cef_unity_get_iosurface_info(_handle, &sid, &w, &h, &fmt);
            surfaceId = sid;
            width = w;
            height = h;
            format = fmt;
            return result != 0;
        }

        /// <summary>
        ///     IOSurface から Metal テクスチャを作成する。
        ///     Metal デバイスは内部で自動取得される。成功時は MTLTexture ポインタを返す。
        /// </summary>
        public static unsafe IntPtr CreateMetalTexture(uint surfaceId, int width, int height, uint format)
        {
            return (IntPtr)NativeMethods.cef_unity_create_metal_texture(surfaceId, width, height, format);
        }

        /// <summary>
        ///     CreateMetalTexture で作成した Metal テクスチャを解放する。
        /// </summary>
        public static unsafe void ReleaseMetalTexture(IntPtr texture)
        {
            if (texture != IntPtr.Zero)
                NativeMethods.cef_unity_release_metal_texture((void*)texture);
        }

        /// <summary>
        ///     Mach port 経由で最新の IOSurface を受信し、Metal テクスチャを作成する。
        ///     新フレームがあれば MTLTexture ポインタと寸法を返す。なければ IntPtr.Zero。
        ///     返されたテクスチャは ReleaseMetalTexture で解放すること。
        /// </summary>
        public static unsafe bool TryRecvIOSurfaceTexture(out IntPtr texturePtr, out int width, out int height, out uint format)
        {
            int w, h;
            uint fmt;
            var ptr = NativeMethods.cef_unity_recv_iosurface_texture(&w, &h, &fmt);
            texturePtr = (IntPtr)ptr;
            width = w;
            height = h;
            format = fmt;
            return ptr != null;
        }


        /// <summary>
        ///     Mach IOSurface port チャネルが接続済みかどうかを返す。
        /// </summary>
        public static bool IsIOSurfaceConnected()
        {
            return NativeMethods.cef_unity_is_iosurface_connected() != 0;
        }

        // ----- Windows D3D11 共有テクスチャ -----

        /// <summary>
        ///     Unity の D3D11 device と接続済みかどうかを返す (Windows 用)。
        /// </summary>
        public static bool IsD3D11Connected()
        {
            return NativeMethods.cef_unity_is_d3d11_connected() != 0;
        }

        /// <summary>
        ///     Windows: 最新フレームの ID3D11Texture2D* を取得する (新フレームなら true)。
        ///     返るポインタは Unity の D3D11 device で開かれており、
        ///     Texture2D.CreateExternalTexture / UpdateExternalTexture に直接渡せる。
        ///     クライアントライブラリ内で AddRef/Release 管理されるため、Unity 側で
        ///     解放処理を書く必要はない (プラグイン unload 時に自動解放)。
        /// </summary>
        public unsafe bool TryRecvD3D11Texture(out IntPtr texturePtr, out int width, out int height, out uint format)
        {
            ThrowIfDisposed();
            int w, h;
            uint fmt;
            var ptr = NativeMethods.cef_unity_recv_d3d11_texture(_handle, &w, &h, &fmt);
            texturePtr = (IntPtr)ptr;
            width = w;
            height = h;
            format = fmt;
            return ptr != null;
        }

        /// <summary>
        /// Unity の D3D12 device と接続済みかどうかを返す (Windows 用)。
        /// </summary>
        public static bool IsD3D12Connected()
        {
            return NativeMethods.cef_unity_is_d3d12_connected() != 0;
        }

        /// <summary>
        /// Windows: 最新フレームの ID3D12Resource* を取得する (新フレームなら true)。
        /// 返るポインタは Unity の D3D12 device で OpenSharedHandle され、
        /// 状態は PIXEL_SHADER_RESOURCE に Unity に宣言済み。
        /// Texture2D.CreateExternalTexture / UpdateExternalTexture にそのまま渡せる。
        /// </summary>
        public unsafe bool TryRecvD3D12Texture(out IntPtr texturePtr, out int width, out int height, out uint format)
        {
            ThrowIfDisposed();
            int w, h;
            uint fmt;
            var ptr = NativeMethods.cef_unity_recv_d3d12_texture(_handle, &w, &h, &fmt);
            texturePtr = (IntPtr)ptr;
            width = w;
            height = h;
            format = fmt;
            return ptr != null;
        }

        // ----- 統一: Accelerated paint (macOS + Windows) -----

        /// <summary>
        ///     現在のプラットフォームで accelerated paint (GPU 経路) が利用可能か。
        /// </summary>
        public static bool IsAcceleratedConnected()
        {
#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
            return IsIOSurfaceConnected();
#elif UNITY_STANDALONE_WIN || UNITY_EDITOR_WIN
            return IsD3D11Connected() || IsD3D12Connected();
#else
            return false;
#endif
        }

        // ----- IME -----

        public void ImeSetComposition(string text, uint selectionStart, uint selectionEnd)
        {
            ThrowIfDisposed();
            unsafe
            {
                fixed (byte* textPtr = ToUtf8Null(text))
                {
                    NativeMethods.cef_unity_ime_set_composition(_handle, textPtr, selectionStart, selectionEnd);
                }
            }
        }

        public void ImeCommitText(string text)
        {
            ThrowIfDisposed();
            unsafe
            {
                fixed (byte* textPtr = ToUtf8Null(text))
                {
                    NativeMethods.cef_unity_ime_commit_text(_handle, textPtr);
                }
            }
        }

        public void ImeFinishComposingText(bool keepSelection = false)
        {
            ThrowIfDisposed();
            unsafe
            {
                NativeMethods.cef_unity_ime_finish_composing_text(_handle, keepSelection ? 1 : 0);
            }
        }

        public void ImeCancelComposition()
        {
            ThrowIfDisposed();
            unsafe
            {
                NativeMethods.cef_unity_ime_cancel_composition(_handle);
            }
        }

        public unsafe void GetImeCaret(out int x, out int y, out int w, out int h)
        {
            ThrowIfDisposed();
            int ox, oy, ow, oh;
            NativeMethods.cef_unity_get_ime_caret(_handle, &ox, &oy, &ow, &oh);
            x = ox;
            y = oy;
            w = ow;
            h = oh;
        }

        // ----- Blocking variants -----

        public int LoadUrlBlocking(string url)
        {
            ThrowIfDisposed();
            unsafe
            {
                fixed (byte* urlPtr = ToUtf8Null(url))
                {
                    return NativeMethods.cef_unity_load_url_blocking(_handle, urlPtr);
                }
            }
        }

        public int ResizeBlocking(int width, int height)
        {
            ThrowIfDisposed();
            unsafe
            {
                return NativeMethods.cef_unity_resize_blocking(_handle, width, height);
            }
        }

        public int SendMouseMoveBlocking(int x, int y, uint modifiers = 0)
        {
            ThrowIfDisposed();
            unsafe
            {
                return NativeMethods.cef_unity_send_mouse_move_blocking(_handle, x, y, modifiers);
            }
        }

        public int SendMouseClickBlocking(int x, int y, MouseButton button, bool mouseUp, int clickCount = 1, uint modifiers = 0)
        {
            ThrowIfDisposed();
            unsafe
            {
                return NativeMethods.cef_unity_send_mouse_click_blocking(_handle, x, y, modifiers, (byte)button, mouseUp ? 1 : 0, clickCount);
            }
        }

        public int SendMouseWheelBlocking(int x, int y, int deltaX, int deltaY, uint modifiers = 0)
        {
            ThrowIfDisposed();
            unsafe
            {
                return NativeMethods.cef_unity_send_mouse_wheel_blocking(_handle, x, y, modifiers, deltaX, deltaY);
            }
        }

        public int SendKeyEventBlocking(
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
                return NativeMethods.cef_unity_send_key_event_blocking(
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

        public int ExecuteJavaScriptBlocking(string code)
        {
            ThrowIfDisposed();
            unsafe
            {
                fixed (byte* codePtr = ToUtf8Null(code))
                {
                    return NativeMethods.cef_unity_execute_javascript_blocking(_handle, codePtr);
                }
            }
        }

        public string GetUrl()
        {
            ThrowIfDisposed();
            unsafe
            {
                var required = NativeMethods.cef_unity_get_url(_handle, null, 0);
                if (required <= 1)
                    return string.Empty;

                var buffer = new byte[required];
                fixed (byte* ptr = buffer)
                {
                    var written = NativeMethods.cef_unity_get_url(_handle, ptr, buffer.Length);
                    if (written <= 1)
                        return string.Empty;

                    return Encoding.UTF8.GetString(buffer, 0, written - 1);
                }
            }
        }

        private void ThrowIfDisposed()
        {
            if (_disposed) throw new Exception();
        }

        private static byte[] ToUtf8Null(string s)
        {
            var bytes = new byte[Encoding.UTF8.GetByteCount(s) + 1];
            Encoding.UTF8.GetBytes(s, bytes);
            // bytes[^1] is already 0 (null terminator)
            return bytes;
        }
    }
}