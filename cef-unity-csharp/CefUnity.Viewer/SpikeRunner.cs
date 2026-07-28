using CefUnity.Runtime;
using Silk.NET.Maths;
using Silk.NET.SDL;
using Silk.NET.Windowing;
using Silk.NET.Windowing.Sdl;
using SilkWindow = Silk.NET.Windowing.Window;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     spec スパイク S1/S2/S3 の一括検証 (CEF なし)。
    ///     S1: MacNativeScrollSource (NSEvent monitor) が SDL イベントループ下で発火するか
    ///         (macOS のみ。Windows は native スクロールソース未対応のため skip)
    ///     S2: SDL API 取得 + AddEventWatch で TEXTEDITING/TEXTINPUT が見えるか
    ///     S3: 表示バックエンドの present が動くか
    ///         (macOS: SDL_Metal_CreateView → CAMetalLayer / Windows: D3D11 + DXGI スワップチェーン)
    /// </summary>
    internal static class SpikeRunner
    {
        private static int _textEvents;

        public static unsafe int Run()
        {
            SdlWindowing.Use();
            var windowOptions = WindowOptions.Default with
            {
                API = GraphicsAPI.None,
                Size = new Vector2D<int>(640, 480),
                Title = "CefUnity.Viewer spike",
                FramesPerSecond = 0,
                UpdatesPerSecond = 0,
                VSync = false,
            };
            using var window = SilkWindow.Create(windowOptions);
            var sdl = SdlWindowing.GetExistingApi(window)
                      ?? throw new InvalidOperationException("SDL backend not active (S2 FAIL)");

            var rendererKind = FrameRendererFactory.SelectKind();
            if (rendererKind == FrameRendererKind.Unsupported)
            {
                Console.Error.WriteLine("このプラットフォームには表示バックエンドがありません (macOS / Windows のみ対応)");
                return 1;
            }
            var graphicsDevice = rendererKind == FrameRendererKind.Direct3D11
                ? new D3D11GraphicsDevice() : null;
            IFrameRenderer renderer = graphicsDevice != null
                ? new D3D11FrameRenderer(graphicsDevice)
                : new MetalFrameRenderer(sdl);
            // native スクロールソースは macOS のみ (Windows は SDL wheel にフォールバックする設計)
            var scrollSource = rendererKind == FrameRendererKind.Metal
                ? new MacNativeScrollSource() : null;
            var scrollEvents = new ScrollInputEvent[256];
            var scrollCount = 0;
            var frames = 0;

            window.Load += () =>
            {
                // SDL は生成後に Retina スケーリングやウィンドウ保存サイズを適用することがあるため
                // Load コールバック内で明示的にサイズを再設定する。
                window.Size = new Vector2D<int>(640, 480);
                renderer.Initialize(window);
                sdl.StartTextInput();
                sdl.AddEventWatch(new PfnEventFilter(EventWatch), null);
                // NSApp は SDL が作成済みのこの時点で開始する (scroll_monitor.m の前提)
                if (scrollSource != null)
                    Console.WriteLine($"S1 scroll monitor started: {scrollSource.Start()}");
                else
                    Console.WriteLine("S1 skipped (Windows: native scroll source 未対応)");
            };
            window.Render += _ =>
            {
                if (scrollSource != null) scrollCount += scrollSource.Poll(scrollEvents);
                renderer.Present(IntPtr.Zero, 640, 480);
                if (++frames >= 300) window.Close();
            };
            window.Run();
            scrollSource?.Dispose();
            renderer.Dispose();
            graphicsDevice?.Dispose();

            Console.WriteLine($"SPIKE frames={frames} scrollEvents={scrollCount} textEvents={_textEvents}");
            Console.WriteLine($"S3 OK (present 300 frames, no exception) backend={rendererKind}");
            if (scrollSource != null)
                Console.WriteLine(scrollCount > 0 ? "S1 OK (scroll events observed)" : "S1 NG? — 窓上でトラックパッドスクロールしたか確認");
            Console.WriteLine(_textEvents > 0 ? "S2 OK (text events observed)" : "S2 NG? — 日本語入力でタイプしたか確認");
            return 0;
        }

        private static unsafe int EventWatch(void* userData, Event* sdlEvent)
        {
            var type = (EventType)sdlEvent->Type;
            if (type == EventType.Textediting || type == EventType.Textinput) _textEvents++;
            return 1;
        }
    }
}
