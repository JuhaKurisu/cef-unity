using System.Runtime.InteropServices;
using CefUnity.Interop;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     CEF 側窓口: BeginFrame/Pump/テクスチャ受信/リサイズ (spec §CefFrameSource)。
    ///     受信 API は macOS (IOSurface → MTLTexture)、Windows (D3D11 共有テクスチャ)、
    ///     Linux (software paint の CPU バッファ → GL テクスチャへアップロード) で異なるため、
    ///     その分岐はこのクラスに封じ込める。
    /// </summary>
    internal sealed class CefFrameSource : IDisposable
    {
        private static readonly bool IsWindows = RuntimeInformation.IsOSPlatform(OSPlatform.Windows);
        private static readonly bool IsLinux = RuntimeInformation.IsOSPlatform(OSPlatform.Linux);

        private readonly Browser _browser;
        private IntPtr _currentTexture;
        private int _textureWidth;
        private int _textureHeight;
        private uint _currentFormat;
        private ulong _frameIndex;
        private IBgraFrameUploader? _bgraUploader;

        public CefFrameSource(int width, int height, string url)
        {
            _browser = new Browser(width, height, url);
        }

        public Browser Browser => _browser;

        /// <summary>
        ///     Linux で CPU バッファを上げる先を設定する。GL コンテキストは窓の生成後にしか
        ///     無いため、コンストラクタではなく表示バックエンドの初期化後に渡す。
        /// </summary>
        public void AttachBgraUploader(IBgraFrameUploader uploader) => _bgraUploader = uploader;

        /// <summary>
        ///     毎フレーム 1 回。新フレームが無ければ直前のテクスチャを返し続ける。
        ///     format は 0=BGRA / 1=RGBA (D3D11FrameRenderer がスワップチェーンの family 判定に使う)。
        /// </summary>
        public bool TickFrame(out IntPtr texturePointer, out int width, out int height, out uint format)
        {
            _browser.SendExternalBeginFrame(_frameIndex++);
            CefRuntime.Pump();
            if (IsWindows)
            {
                // 返るポインタは native 側のキャッシュ (AddRef 管理) なので、こちらで解放しない
                if (_browser.TryReceiveD3D11Texture(out var d3d11Texture, out var d3d11Width, out var d3d11Height, out var d3d11Format))
                {
                    _currentTexture = d3d11Texture;
                    _textureWidth = d3d11Width;
                    _textureHeight = d3d11Height;
                    _currentFormat = d3d11Format;
                }
            }
            else if (IsLinux)
            {
                // 新しいフレームが来たときだけ上げる。来なければ直前のテクスチャを描き続ける。
                // テクスチャは表示バックエンドの所有物なので、こちらでは解放しない。
                if (_bgraUploader != null
                    && _browser.TryGetBuffer(out var bgra, out var bufferWidth, out var bufferHeight)
                    && !bgra.IsEmpty)
                {
                    var uploadedTexture = _bgraUploader.UploadBgra(bgra, bufferWidth, bufferHeight);
                    if (uploadedTexture != IntPtr.Zero)
                    {
                        _currentTexture = uploadedTexture;
                        _textureWidth = bufferWidth;
                        _textureHeight = bufferHeight;
                        _currentFormat = 0;
                    }
                }
            }
            else if (Browser.TryReceiveIOSurfaceTexture(out var newTexture, out var newWidth, out var newHeight, out var newFormat))
            {
                if (_currentTexture != IntPtr.Zero) Browser.ReleaseMetalTexture(_currentTexture);
                _currentTexture = newTexture;
                _textureWidth = newWidth;
                _textureHeight = newHeight;
                _currentFormat = newFormat;
            }
            texturePointer = _currentTexture;
            width = _textureWidth;
            height = _textureHeight;
            format = _currentFormat;
            return _currentTexture != IntPtr.Zero;
        }

        /// <summary>server 側が was_resized + invalidate を行う (CLAUDE.md のリサイズ既知の罠は server 実装済み)。</summary>
        public void Resize(int width, int height) => _browser.Resize(width, height);

        public void Dispose()
        {
            // Windows の受信テクスチャは native 側のキャッシュ、Linux は表示バックエンドの
            // 所有物なので、どちらもここでは解放しない
            if (_currentTexture != IntPtr.Zero && !IsWindows && !IsLinux)
            {
                Browser.ReleaseMetalTexture(_currentTexture);
            }
            _currentTexture = IntPtr.Zero;
            _browser.Dispose();
        }
    }
}
