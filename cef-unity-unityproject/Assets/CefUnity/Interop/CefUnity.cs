using System;
using System.Text;

namespace CefUnity.Interop
{

    public enum MouseButton : byte
    {
        Left = 0,
        Middle = 1,
        Right = 2,
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
        /// CEF メッセージループを駆動する。毎フレーム、メインスレッドから呼ぶこと。
        /// </summary>
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
            if (_disposed) throw new Exception();
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
                rgba[i] = bgra[i + 2]; // R <- B
                rgba[i + 1] = bgra[i + 1]; // G
                rgba[i + 2] = bgra[i]; // B <- R
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
}