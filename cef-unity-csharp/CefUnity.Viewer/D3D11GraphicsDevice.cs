using Silk.NET.Core.Native;
using Silk.NET.Direct3D11;
using Silk.NET.DXGI;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     Viewer が所有する ID3D11Device と immediate context。ウィンドウには依存しない
    ///     (スワップチェーンは D3D11FrameRenderer が持つ)。
    ///
    ///     native 側 (crates/client/src/d3d11.rs) はこのデバイスを AddRef せず借用するだけなので、
    ///     CEF shutdown まで Dispose してはならない。
    ///
    ///     デバイス生成条件は server 側 D3D11Pool (crates/server/src/d3d11_pool.rs) と揃える
    ///     — 既定アダプタ / Hardware / BGRA_SUPPORT。揃えないと共有テクスチャを開けない
    ///     アダプタになりうる。
    /// </summary>
    internal sealed unsafe class D3D11GraphicsDevice : IDisposable
    {
        private readonly D3D11 _d3d11;
        private ComPtr<ID3D11Device> _device;
        private ComPtr<ID3D11DeviceContext> _immediateContext;

        public D3D11GraphicsDevice()
        {
            _d3d11 = D3D11.GetApi(null);
            D3DFeatureLevel featureLevel = default;
            SilkMarshal.ThrowHResult(_d3d11.CreateDevice(
                default(ComPtr<IDXGIAdapter>),
                D3DDriverType.Hardware,
                default(nint),
                (uint)CreateDeviceFlag.BgraSupport,
                null,
                0u,
                D3D11.SdkVersion,
                ref _device,
                ref featureLevel,
                ref _immediateContext));
        }

        public ID3D11Device* Device => _device;

        /// <summary>
        ///     immediate context。native 側 wait_fence がこの context に Wait を積むため、
        ///     受信テクスチャのコピーも必ずこの context で行うこと。
        /// </summary>
        public ID3D11DeviceContext* ImmediateContext => _immediateContext;

        public IntPtr DevicePointer => (IntPtr)_device.Handle;

        public void Dispose()
        {
            _immediateContext.Dispose();
            _device.Dispose();
            _d3d11.Dispose();
        }
    }
}
