using CefUnity.Interop;
using CefUnity.Runtime;
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
        private readonly ScrollInputMatrix _scrollMatrix;
        private readonly IWindow _window;
        private readonly IFrameRenderer _renderer;
        private bool _firstFrameShown;
        private bool _applicationActivated;
        private IInputContext? _input;
        private IMouse? _mouse;
        private IKeyboard? _keyboard;
        private readonly ClickCounter _clickCounter = new ClickCounter();
        private int _mouseX;
        private int _mouseY;
        private double _elapsedSeconds;
        private int _lastSentDeltaY;
        private int _lastClickCount = 1;

        public ViewerWindow(ViewerOptions options, CefFrameSource frameSource, ScrollInputMatrix scrollMatrix)
        {
            _options = options;
            _frameSource = frameSource;
            _scrollMatrix = scrollMatrix;
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
            _window.Resize += OnWindowResize;
        }

        public void Run() => _window.Run();

        private void OnLoad()
        {
            // SDL は生成後に Retina スケーリングやウィンドウ保存サイズを適用することがあるため
            // Load コールバック内で明示的にサイズを再設定する (Defect A 修正)。
            _window.Size = new Vector2D<int>(_options.Width, _options.Height);
            // 実際のウィンドウサイズが options と異なる場合はブラウザ側を合わせる (Defect B 修正)。
            if (_window.Size.X != _options.Width || _window.Size.Y != _options.Height)
                _frameSource.Resize(_window.Size.X, _window.Size.Y);
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
            _keyboard = _input.Keyboards.Count > 0 ? _input.Keyboards[0] : null;
            if (_keyboard != null)
            {
                _keyboard.KeyDown += OnKeyDown;
                _keyboard.KeyUp += OnKeyUp;
            }
            if (_options.ReplayPath == null)
            {
                var startResult = _scrollMatrix.StartNativeSource(out var startError);
                if (startResult != NativeScrollSourceStart.Started)
                    Console.WriteLine($"native scroll source: {startResult} {startError?.Message} — フォールバック (窓 wheel イベント)");
            }
            _scrollMatrix.SetMode(_options.Mode);
            _scrollMatrix.RecordingEnabled = _options.Record;
        }

        private void OnRender(double deltaSeconds)
        {
            if (!_applicationActivated)
            {
                _applicationActivated = true;
                MacApplicationActivator.ActivateCurrentApplication();
            }
            _elapsedSeconds += deltaSeconds;
            var overBrowser = _mouseX >= 0 && _mouseY >= 0
                              && _mouseX < _window.Size.X && _mouseY < _window.Size.Y;
            _scrollMatrix.TickFrame((float)deltaSeconds, overBrowser,
                out var primaryDeltaX, out var primaryDeltaY, out var secondaryDeltaX, out var secondaryDeltaY);
            if (primaryDeltaX != 0 || primaryDeltaY != 0)
                _frameSource.Browser.SendMouseWheel(_mouseX, _mouseY, primaryDeltaX, primaryDeltaY);
            if (secondaryDeltaX != 0 || secondaryDeltaY != 0)
                _frameSource.Browser.SendMouseWheel(_mouseX, _mouseY, secondaryDeltaX, secondaryDeltaY);
            _lastSentDeltaY = primaryDeltaY + secondaryDeltaY; // Task 8 の統計用
            if (_frameSource.TickFrame(out var texturePointer, out var textureWidth, out var textureHeight))
            {
                _renderer.Present(texturePointer, textureWidth, textureHeight);
                if (!_firstFrameShown)
                {
                    _firstFrameShown = true;
                    UpdateTitle();
                }
            }
            else
            {
                // まだ 1 枚も来ていない: drawable だけ回す (黒画面)
                _renderer.Present(IntPtr.Zero, _options.Width, _options.Height);
            }
        }

        private void UpdateTitle()
        {
            var recording = _scrollMatrix.RecordingEnabled ? " REC" : "";
            _window.Title = $"CefUnity.Viewer [{_scrollMatrix.Mode}]{recording}";
        }

        public void Dispose()
        {
            _input?.Dispose();
            _renderer.Dispose();
            _window.Dispose();
        }

        private void OnWindowResize(Vector2D<int> newSize)
        {
            if (newSize.X <= 0 || newSize.Y <= 0) return;
            _frameSource.Resize(newSize.X, newSize.Y);
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
            _lastClickCount = _clickCounter.OnMouseDown(_elapsedSeconds, _mouseX, _mouseY);
            _frameSource.Browser.SendMouseClick(_mouseX, _mouseY, ToCefMouseButton(button), mouseUp: false, _lastClickCount);
        }

        private void OnMouseUp(IMouse mouse, Silk.NET.Input.MouseButton button)
        {
            _frameSource.Browser.SendMouseClick(_mouseX, _mouseY, ToCefMouseButton(button), mouseUp: true, _lastClickCount);
        }

        private void OnMouseScroll(IMouse mouse, ScrollWheel wheel)
        {
            _scrollMatrix.AddWheelSteps(wheel.X, wheel.Y);
        }

        private void OnKeyDown(IKeyboard keyboard, Key key, int scanCode)
        {
            switch (key)
            {
                case Key.F1: _scrollMatrix.SetMode(ScrollMode.Raw); UpdateTitle(); return;
                case Key.F2: _scrollMatrix.SetMode(ScrollMode.Smoother); UpdateTitle(); return;
                case Key.F3: _scrollMatrix.SetMode(ScrollMode.Resampler); UpdateTitle(); return;
                case Key.F5:
                    _scrollMatrix.RecordingEnabled = !_scrollMatrix.RecordingEnabled;
                    UpdateTitle();
                    return;
            }
            if (!SilkKeyboardMapper.TryMap(key, out var code)) return;
            _frameSource.Browser.SendKeyEvent(KeyEventType.RawKeyDown, code, SilkKeyboardMapper.BuildModifiers(keyboard));
        }

        private void OnKeyUp(IKeyboard keyboard, Key key, int scanCode)
        {
            if (!SilkKeyboardMapper.TryMap(key, out var code)) return;
            _frameSource.Browser.SendKeyEvent(KeyEventType.KeyUp, code, SilkKeyboardMapper.BuildModifiers(keyboard));
        }
    }
}
