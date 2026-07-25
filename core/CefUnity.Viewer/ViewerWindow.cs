using Silk.NET.Maths;
using Silk.NET.SDL;
using Silk.NET.Windowing;
using Silk.NET.Windowing.Sdl;
using SilkWindow = Silk.NET.Windowing.Window;

namespace CefUnity.Viewer
{
    /// <summary>SDL 窓とフレームループの所有者。入力配線は後続タスクでこのクラスに追記する。</summary>
    internal sealed class ViewerWindow : IDisposable
    {
        private readonly ViewerOptions _options;
        private readonly CefFrameSource _frameSource;
        private readonly IWindow _window;
        private readonly IFrameRenderer _renderer;
        private bool _firstFrameShown;

        public ViewerWindow(ViewerOptions options, CefFrameSource frameSource)
        {
            _options = options;
            _frameSource = frameSource;
            SdlWindowing.Use();
            _window = SilkWindow.Create(WindowOptions.Default with
            {
                API = GraphicsAPI.None,
                Size = new Vector2D<int>(options.Width, options.Height),
                Title = "CefUnity.Viewer (loading)",
                // ペーシングはタイマーではなく CAMetalLayer displaySync + nextDrawable ブロックに任せる
                FramesPerSecond = 0,
                UpdatesPerSecond = 0,
                VSync = false,
            });
            var sdl = SdlWindowing.GetExistingApi(_window)
                      ?? throw new InvalidOperationException("SDL backend not active");
            _renderer = new MetalFrameRenderer(sdl);
            _window.Load += OnLoad;
            _window.Render += OnRender;
        }

        public void Run() => _window.Run();

        private void OnLoad()
        {
            _renderer.Initialize(_window);
        }

        private void OnRender(double deltaSeconds)
        {
            if (_frameSource.TickFrame(out var texturePointer, out var textureWidth, out var textureHeight))
            {
                _renderer.Present(texturePointer, textureWidth, textureHeight);
                if (!_firstFrameShown)
                {
                    _firstFrameShown = true;
                    _window.Title = "CefUnity.Viewer";
                }
            }
            else
            {
                // まだ 1 枚も来ていない: drawable だけ回す (黒画面)
                _renderer.Present(IntPtr.Zero, _options.Width, _options.Height);
            }
        }

        public void Dispose()
        {
            _renderer.Dispose();
            _window.Dispose();
        }
    }
}
