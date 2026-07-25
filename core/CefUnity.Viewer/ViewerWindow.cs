using Silk.NET.Input;
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
        private IInputContext? _input;
        private IMouse? _mouse;
        private readonly ClickCounter _clickCounter = new ClickCounter();
        private int _mouseX;
        private int _mouseY;
        private double _elapsedSeconds;

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
            _input = _window.CreateInput();
            _mouse = _input.Mice.Count > 0 ? _input.Mice[0] : null;
            if (_mouse != null)
            {
                _mouse.MouseMove += OnMouseMove;
                _mouse.MouseDown += OnMouseDown;
                _mouse.MouseUp += OnMouseUp;
                _mouse.Scroll += OnMouseScroll;
            }
        }

        private void OnRender(double deltaSeconds)
        {
            _elapsedSeconds += deltaSeconds;
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
            _input?.Dispose();
            _renderer.Dispose();
            _window.Dispose();
        }

        private void OnMouseMove(IMouse mouse, System.Numerics.Vector2 position)
        {
            _mouseX = (int)position.X;
            _mouseY = (int)position.Y;
            _frameSource.Browser.SendMouseMove(_mouseX, _mouseY);
        }

        private static CefUnity.Interop.MouseButton ToCefMouseButton(Silk.NET.Input.MouseButton button) => button switch
        {
            Silk.NET.Input.MouseButton.Right => CefUnity.Interop.MouseButton.Right,
            Silk.NET.Input.MouseButton.Middle => CefUnity.Interop.MouseButton.Middle,
            _ => CefUnity.Interop.MouseButton.Left,
        };

        private void OnMouseDown(IMouse mouse, Silk.NET.Input.MouseButton button)
        {
            var clickCount = _clickCounter.OnMouseDown(_elapsedSeconds, _mouseX, _mouseY);
            _frameSource.Browser.SendMouseClick(_mouseX, _mouseY, ToCefMouseButton(button), mouseUp: false, clickCount);
        }

        private void OnMouseUp(IMouse mouse, Silk.NET.Input.MouseButton button)
        {
            _frameSource.Browser.SendMouseClick(_mouseX, _mouseY, ToCefMouseButton(button), mouseUp: true);
        }

        private void OnMouseScroll(IMouse mouse, ScrollWheel wheel)
        {
            // Task 6 で ScrollInputMatrix に置き換える。まずは raw 直送 (Unity 旧経路相当)
            _frameSource.Browser.SendMouseWheel(_mouseX, _mouseY,
                (int)(wheel.X * CefUnity.Runtime.ScrollInputPipeline.WheelPixelsPerStep),
                (int)(wheel.Y * CefUnity.Runtime.ScrollInputPipeline.WheelPixelsPerStep));
        }
    }
}
