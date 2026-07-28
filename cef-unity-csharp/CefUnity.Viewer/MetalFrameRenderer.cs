using Silk.NET.SDL;
using Silk.NET.Windowing;
using SdlWindow = Silk.NET.SDL.Window;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     受信 MTLTexture を CAMetalLayer の drawable へ blit して表示する。
    ///     テクスチャは cef_unity_receive_iosurface_texture がシステム既定 Metal デバイスで
    ///     作るため、レイヤーにも MTLCreateSystemDefaultDevice を設定すれば同一デバイスで
    ///     blit できる (Apple Silicon は単一 GPU)。
    ///     色: CEF の BGRA バイトは sRGB エンコード済み。BGRA8Unorm どうしの blit は
    ///     変換なしでバイトがそのまま表示され、ウィンドウ既定色空間 (sRGB) で正しく見える。
    ///
    ///     drawableSize 収束方式:
    ///     SDL の Metal view はウィンドウ表示・リサイズ等のレイアウトイベントで
    ///     drawableSize を bounds×contentsScale (Retina では 2×) に上書きする。
    ///     そのため毎フレーム drawableSize を読み出し、テクスチャサイズと不一致なら
    ///     setDrawableSize: でテクスチャサイズに再設定して今フレームの blit を skip する。
    ///     次フレームでサイズが一致していれば blit を実行する (通常 1 フレームの skip で収束)。
    ///     drawableSize == テクスチャサイズ のとき CoreAnimation がレイヤーを窓全体に拡縮表示するため
    ///     Retina でのぼかしは生じるが全面表示になる。
    /// </summary>
    internal sealed unsafe class MetalFrameRenderer : IFrameRenderer
    {
        private static readonly IntPtr SelectorSetDevice = MetalNative.Selector("setDevice:");
        private static readonly IntPtr SelectorSetFramebufferOnly = MetalNative.Selector("setFramebufferOnly:");
        private static readonly IntPtr SelectorNewCommandQueue = MetalNative.Selector("newCommandQueue");
        private static readonly IntPtr SelectorNextDrawable = MetalNative.Selector("nextDrawable");
        private static readonly IntPtr SelectorTexture = MetalNative.Selector("texture");
        private static readonly IntPtr SelectorCommandBuffer = MetalNative.Selector("commandBuffer");
        private static readonly IntPtr SelectorBlitCommandEncoder = MetalNative.Selector("blitCommandEncoder");
        private static readonly IntPtr SelectorCopyTextureRegion = MetalNative.Selector(
            "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:");
        private static readonly IntPtr SelectorEndEncoding = MetalNative.Selector("endEncoding");
        private static readonly IntPtr SelectorPresentDrawable = MetalNative.Selector("presentDrawable:");
        private static readonly IntPtr SelectorCommit = MetalNative.Selector("commit");
        private static readonly IntPtr SelectorRelease = MetalNative.Selector("release");
        private static readonly IntPtr SelectorWidth = MetalNative.Selector("width");
        private static readonly IntPtr SelectorHeight = MetalNative.Selector("height");
        private static readonly IntPtr SelectorDrawableSize = MetalNative.Selector("drawableSize");
        private static readonly IntPtr SelectorSetDrawableSize = MetalNative.Selector("setDrawableSize:");

        private readonly Sdl _sdl;
        private void* _metalView;
        private IntPtr _layer;
        private IntPtr _commandQueue;

        public MetalFrameRenderer(Sdl sdl)
        {
            _sdl = sdl;
        }

        public void Initialize(IView view)
        {
            var window = (SdlWindow*)view.Native!.Sdl!.Value;
            _metalView = _sdl.MetalCreateView(window);
            if (_metalView == null) throw new InvalidOperationException("SDL_Metal_CreateView failed");
            _layer = (IntPtr)_sdl.MetalGetLayer(_metalView);
            if (_layer == IntPtr.Zero) throw new InvalidOperationException("SDL_Metal_GetLayer failed");
            var device = MetalNative.MTLCreateSystemDefaultDevice();
            MetalNative.VoidMessage(_layer, SelectorSetDevice, device);
            // blit の書き込み先にするため framebufferOnly を外す
            MetalNative.VoidBoolMessage(_layer, SelectorSetFramebufferOnly, false);
            _commandQueue = MetalNative.IntPtrMessage(device, SelectorNewCommandQueue);
            if (_commandQueue == IntPtr.Zero) throw new InvalidOperationException("newCommandQueue failed");
        }

        public void Present(IntPtr texturePointer, int width, int height)
        {
            // Rust スレッドと同じ罠: pool なしだと Metal オブジェクトが蓄積しフレームスパイクになる
            var pool = MetalNative.AutoreleasePoolPush();
            try
            {
                // drawableSize 収束: SDL がレイアウトイベントで drawableSize を上書きするため
                // 毎フレーム現在値を読み出し、テクスチャサイズと異なればテクスチャサイズへ再設定して
                // このフレームの blit を skip する。フィールドにキャッシュしないことで
                // SDL による上書きを次フレームで自動検出・再収束できる。
                if (texturePointer != IntPtr.Zero && width > 0 && height > 0)
                {
                    var currentDrawableSize = MetalNative.CGSizeMessage(_layer, SelectorDrawableSize);
                    if (currentDrawableSize.Width != width || currentDrawableSize.Height != height)
                    {
                        MetalNative.VoidCGSizeMessage(
                            _layer,
                            SelectorSetDrawableSize,
                            new MetalNative.CGSize { Width = width, Height = height });
                        // in-flight drawable が古いサイズを持つ可能性があるため今フレームは skip
                        return;
                    }
                }

                var drawable = MetalNative.IntPtrMessage(_layer, SelectorNextDrawable);
                if (drawable == IntPtr.Zero) return;
                var commandBuffer = MetalNative.IntPtrMessage(_commandQueue, SelectorCommandBuffer);
                if (commandBuffer == IntPtr.Zero) return;
                if (texturePointer != IntPtr.Zero)
                {
                    var drawableTexture = MetalNative.IntPtrMessage(drawable, SelectorTexture);
                    var sourceWidth = (nuint)MetalNative.NuintMessage(texturePointer, SelectorWidth);
                    var sourceHeight = (nuint)MetalNative.NuintMessage(texturePointer, SelectorHeight);
                    var drawableWidth = (nuint)MetalNative.NuintMessage(drawableTexture, SelectorWidth);
                    var drawableHeight = (nuint)MetalNative.NuintMessage(drawableTexture, SelectorHeight);

                    // drawableSize 収束済みなのでソースと drawable のサイズは一致しているはずだが、
                    // 安全のため最小値でクランプする
                    var copyWidth = Math.Min(sourceWidth, drawableWidth);
                    var copyHeight = Math.Min(sourceHeight, drawableHeight);

                    if (copyWidth > 0 && copyHeight > 0)
                    {
                        var blitEncoder = MetalNative.IntPtrMessage(commandBuffer, SelectorBlitCommandEncoder);
                        MetalNative.CopyTextureRegion(
                            blitEncoder, SelectorCopyTextureRegion,
                            sourceTexture: texturePointer,
                            sourceSlice: 0,
                            sourceLevel: 0,
                            sourceOrigin: new MetalNative.MTLOrigin { X = 0, Y = 0, Z = 0 },
                            sourceSize: new MetalNative.MTLSize { Width = copyWidth, Height = copyHeight, Depth = 1 },
                            destinationTexture: drawableTexture,
                            destinationSlice: 0,
                            destinationLevel: 0,
                            destinationOrigin: new MetalNative.MTLOrigin { X = 0, Y = 0, Z = 0 });
                        MetalNative.VoidMessage(blitEncoder, SelectorEndEncoding);
                    }
                }
                MetalNative.VoidMessage(commandBuffer, SelectorPresentDrawable, drawable);
                MetalNative.VoidMessage(commandBuffer, SelectorCommit);
            }
            finally
            {
                MetalNative.AutoreleasePoolPop(pool);
            }
        }

        public void Dispose()
        {
            if (_commandQueue != IntPtr.Zero)
            {
                MetalNative.VoidMessage(_commandQueue, SelectorRelease);
                _commandQueue = IntPtr.Zero;
            }
            if (_metalView != null)
            {
                _sdl.MetalDestroyView(_metalView);
                _metalView = null;
            }
        }
    }
}
