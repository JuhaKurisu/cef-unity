using Silk.NET.Windowing;

namespace CefUnity.Viewer
{
    /// <summary>表示抽象。将来の Windows D3D11FrameRenderer の継ぎ目 (spec 参照)。</summary>
    internal interface IFrameRenderer : IDisposable
    {
        void Initialize(IView view);

        /// <summary>
        ///     受信テクスチャをウィンドウへ表示する。texturePointer が IntPtr.Zero の場合は
        ///     blit せず drawable を回すだけ (起動直後・スパイク用)。
        ///     drawableSize はテクスチャサイズに追従し、リサイズ中は旧サイズの絵が
        ///     レイヤー境界にスケール表示される (spec のリサイズ節)。spec のインターフェース案に
        ///     あった Resize はこのサイズ追従に統合した (意図的な簡約)。
        /// </summary>
        void Present(IntPtr texturePointer, int width, int height);
    }
}
