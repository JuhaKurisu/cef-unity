using Silk.NET.Core.Native;
using Silk.NET.Direct3D11;
using Silk.NET.DXGI;
using Silk.NET.Windowing;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     受信 ID3D11Texture2D を DXGI スワップチェーンのバックバッファへコピーして表示する
    ///     (macOS の MetalFrameRenderer に対応する Windows 実装)。
    ///
    ///     色: server はプールテクスチャを B8G8R8A8_UNORM_SRGB (RGBA 時は R8G8B8A8_UNORM_SRGB) で
    ///     作る (crates/server/src/server.rs の on_accelerated_paint)。UNORM ↔ UNORM_SRGB は同 family
    ///     なのでコピーは通り、CEF の出す sRGB エンコード済みバイトがそのままバックバッファに入る。
    ///     _UNORM のスワップチェーンで無変換表示すると正しく見える (Metal 側の blit と同じ理屈)。
    ///     ただし BGRA と RGBA は family が異なりコピーが失敗するため、受信 format タグを追跡して
    ///     スワップチェーンを作り直す。
    ///
    ///     サイズ収束: バックバッファとテクスチャのサイズが不一致なら ResizeBuffers して
    ///     そのフレームは skip する (Metal 側の drawableSize 収束と同じ方針。in-flight のバッファが
    ///     旧サイズを持ちうるため)。次フレームで一致していればコピーする。
    ///
    ///     同期: native 側 wait_fence は注入デバイスの immediate context に Wait を積むため
    ///     (crates/client/src/d3d11.rs)、コピーも必ず同じ immediate context で行う。
    ///     deferred context は作らない。
    /// </summary>
    internal sealed unsafe class D3D11FrameRenderer : IFrameRenderer
    {
        private readonly D3D11GraphicsDevice _graphicsDevice;
        private readonly DXGI _dxgi;
        private ComPtr<IDXGISwapChain1> _swapChain;
        private nint _windowHandle;
        private int _bufferWidth;
        private int _bufferHeight;
        private uint _bufferFormatTag;
        private uint _receivedFormatTag;
        private bool _copyFailureReported;

        public D3D11FrameRenderer(D3D11GraphicsDevice graphicsDevice)
        {
            _graphicsDevice = graphicsDevice;
            _dxgi = DXGI.GetApi(null);
        }

        /// <summary>
        ///     受信テクスチャの format タグ (0=BGRA, 1=RGBA) を伝える。Present より前に呼ぶこと。
        /// </summary>
        public void SetReceivedFormat(uint format) => _receivedFormatTag = format;

        private static Format ToDxgiFormat(uint formatTag)
            => formatTag == 1 ? Format.FormatR8G8B8A8Unorm : Format.FormatB8G8R8A8Unorm;

        public void Initialize(IView view)
        {
            var win32 = view.Native?.Win32
                        ?? throw new InvalidOperationException(
                            "Win32 ネイティブウィンドウハンドルを取得できません (SDL バックエンドが Windows で動作していない)");
            _windowHandle = win32.Hwnd;
            CreateSwapChain(Math.Max(view.Size.X, 1), Math.Max(view.Size.Y, 1), _receivedFormatTag);
        }

        private void CreateSwapChain(int width, int height, uint formatTag)
        {
            _swapChain.Dispose();
            _swapChain = default;

            ComPtr<IDXGIFactory2> factory = default;
            SilkMarshal.ThrowHResult(_dxgi.CreateDXGIFactory2(0u, out factory));
            try
            {
                var description = new SwapChainDesc1
                {
                    Width = (uint)width,
                    Height = (uint)height,
                    Format = ToDxgiFormat(formatTag),
                    Stereo = false,
                    SampleDesc = new SampleDesc(1u, 0u),
                    BufferUsage = DXGI.UsageRenderTargetOutput,
                    BufferCount = 2u,
                    Scaling = Scaling.Stretch,
                    SwapEffect = SwapEffect.FlipDiscard,
                    AlphaMode = Silk.NET.DXGI.AlphaMode.Ignore,
                    Flags = 0u,
                };
                IDXGISwapChain1* swapChainPointer = null;
                SilkMarshal.ThrowHResult(factory.CreateSwapChainForHwnd(
                    (IUnknown*)_graphicsDevice.Device,
                    _windowHandle,
                    &description,
                    (SwapChainFullscreenDesc*)null,
                    (IDXGIOutput*)null,
                    &swapChainPointer));
                _swapChain = new ComPtr<IDXGISwapChain1>(swapChainPointer);
                _bufferWidth = width;
                _bufferHeight = height;
                _bufferFormatTag = formatTag;
            }
            finally
            {
                factory.Dispose();
            }
        }

        public void Present(IntPtr texturePointer, int width, int height)
        {
            if (_swapChain.Handle == null) return;

            if (texturePointer != IntPtr.Zero && width > 0 && height > 0)
            {
                // format が変わったらスワップチェーンごと作り直す (family 違いはコピー不可)。
                // 作り直した直後のフレームは skip し、次フレームでコピーする。
                if (_receivedFormatTag != _bufferFormatTag)
                {
                    CreateSwapChain(width, height, _receivedFormatTag);
                    return;
                }
                // サイズ収束: 一致するまではコピーせず次フレームに回す
                if (_bufferWidth != width || _bufferHeight != height)
                {
                    SilkMarshal.ThrowHResult(_swapChain.ResizeBuffers(
                        0u, (uint)width, (uint)height, Format.FormatUnknown, 0u));
                    _bufferWidth = width;
                    _bufferHeight = height;
                    return;
                }

                ComPtr<ID3D11Texture2D> backBuffer = default;
                SilkMarshal.ThrowHResult(_swapChain.GetBuffer(0u, out backBuffer));
                try
                {
                    if (backBuffer.Handle != null)
                    {
                        _graphicsDevice.ImmediateContext->CopySubresourceRegion(
                            (ID3D11Resource*)backBuffer.Handle, 0u, 0u, 0u, 0u,
                            (ID3D11Resource*)texturePointer, 0u, null);
                    }
                    else if (!_copyFailureReported)
                    {
                        _copyFailureReported = true;
                        Console.Error.WriteLine("D3D11FrameRenderer: バックバッファを取得できませんでした");
                    }
                }
                finally
                {
                    backBuffer.Dispose();
                }
            }

            // vsync 待ち。mac の CAMetalLayer displaySync に相当するフレームペーシングを担う
            _swapChain.Present(1u, 0u);
        }

        public void Dispose()
        {
            _swapChain.Dispose();
            _swapChain = default;
            _dxgi.Dispose();
        }
    }
}
