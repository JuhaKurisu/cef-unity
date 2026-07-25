using CefUnity.Interop;

namespace CefUnity.Viewer
{
    /// <summary>CEF 側窓口: BeginFrame/Pump/IOSurface テクスチャ受信/リサイズ (spec §CefFrameSource)。</summary>
    internal sealed class CefFrameSource : IDisposable
    {
        private readonly Browser _browser;
        private IntPtr _currentTexture;
        private int _textureWidth;
        private int _textureHeight;
        private ulong _frameIndex;

        public CefFrameSource(int width, int height, string url)
        {
            _browser = new Browser(width, height, url);
        }

        public Browser Browser => _browser;

        /// <summary>毎フレーム 1 回。新フレームが無ければ直前のテクスチャを返し続ける。</summary>
        public bool TickFrame(out IntPtr texturePointer, out int width, out int height)
        {
            _browser.SendExternalBeginFrame(_frameIndex++);
            CefRuntime.Pump();
            if (Browser.TryReceiveIOSurfaceTexture(out var newTexture, out var newWidth, out var newHeight, out _))
            {
                if (_currentTexture != IntPtr.Zero) Browser.ReleaseMetalTexture(_currentTexture);
                _currentTexture = newTexture;
                _textureWidth = newWidth;
                _textureHeight = newHeight;
            }
            texturePointer = _currentTexture;
            width = _textureWidth;
            height = _textureHeight;
            return _currentTexture != IntPtr.Zero;
        }

        /// <summary>server 側が was_resized + invalidate を行う (CLAUDE.md のリサイズ既知の罠は server 実装済み)。</summary>
        public void Resize(int width, int height) => _browser.Resize(width, height);

        public void Dispose()
        {
            if (_currentTexture != IntPtr.Zero)
            {
                Browser.ReleaseMetalTexture(_currentTexture);
                _currentTexture = IntPtr.Zero;
            }
            _browser.Dispose();
        }
    }
}
