using System.Runtime.InteropServices;

namespace CefUnity.Viewer
{
    /// <summary>表示バックエンドの種別。</summary>
    public enum FrameRendererKind
    {
        /// <summary>macOS: CAMetalLayer への blit (MetalFrameRenderer)。</summary>
        Metal,

        /// <summary>Windows: DXGI スワップチェーンへのコピー (D3D11FrameRenderer)。</summary>
        Direct3D11,

        /// <summary>表示バックエンドが無いプラットフォーム。</summary>
        Unsupported,
    }

    /// <summary>
    ///     実行中のプラットフォームに応じた表示バックエンドを選ぶ。
    ///     プラットフォーム分岐を呼び出し側に散らさないための単一の判定点
    ///     (ScrollInputPipeline.StartNativeSource と同じ流儀)。
    ///     判定ロジックをテスト可能にするため OS 判定は引数で受ける。
    /// </summary>
    public static class FrameRendererFactory
    {
        public static FrameRendererKind SelectKind(bool isMacOS, bool isWindows)
        {
            if (isMacOS) return FrameRendererKind.Metal;
            if (isWindows) return FrameRendererKind.Direct3D11;
            return FrameRendererKind.Unsupported;
        }

        public static FrameRendererKind SelectKind()
            => SelectKind(
                RuntimeInformation.IsOSPlatform(OSPlatform.OSX),
                RuntimeInformation.IsOSPlatform(OSPlatform.Windows));
    }
}
