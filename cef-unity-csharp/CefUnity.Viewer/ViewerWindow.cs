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
        private readonly Sdl _sdl;
        private bool _firstFrameShown;
        private bool _applicationActivated;
        private IInputContext? _input;
        private IMouse? _mouse;
        private IKeyboard? _keyboard;
        private readonly ClickCounter _clickCounter = new ClickCounter();
        private int _mouseX;
        private int _mouseY;
        private double _elapsedSeconds;
        private int _lastSentDeltaX;
        private int _lastSentDeltaY;
        private int _lastClickCount = 1;
        // 所有権は Program (using StatisticsRecorder で管理)
        private readonly StatisticsRecorder? _statistics;
        private long _statisticsFrameIndex;
        private readonly CefUnity.Runtime.ScrollReplaySource? _replaySource;
        private ImeBridge? _imeBridge;
        private static ViewerWindow? _eventWatchInstance; // SDL コールバックは static のため
        private int _caretX = -1;
        private int _caretY = -1;
        private bool _replayFinishedShown; // Item I: リプレイ完了表示フラグ

        /// <param name="rendererFactory">
        ///     表示バックエンドの生成関数。MetalFrameRenderer は Sdl インスタンスを要求し、
        ///     それはウィンドウ生成後にしか取れないため、インスタンスではなくファクトリを受け取る。
        /// </param>
        public ViewerWindow(ViewerOptions options, CefFrameSource frameSource, ScrollInputMatrix scrollMatrix, Func<Sdl, IFrameRenderer> rendererFactory, StatisticsRecorder? statistics, CefUnity.Runtime.ScrollReplaySource? replaySource = null)
        {
            _options = options;
            _frameSource = frameSource;
            _scrollMatrix = scrollMatrix;
            _statistics = statistics;
            _replaySource = replaySource;
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
            _sdl = SdlWindowing.GetExistingApi(_window)
                   ?? throw new InvalidOperationException("SDL backend not active");
            _renderer = rendererFactory(_sdl);
            _window.Load += OnLoad;
            _window.Render += OnRender;
            _window.Resize += OnWindowResize;
            _window.FocusChanged += OnFocusChanged;
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
            // デバイスが 0 件だと入力が一切効かない (原因が見えにくいので起動時に必ず出す)
            Console.WriteLine($"input devices: mice={_input.Mice.Count} keyboards={_input.Keyboards.Count}");
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
            var effectiveMode = _options.Mode;
            if (_options.ReplayPath == null)
            {
                var startResult = _scrollMatrix.StartNativeSource(out var startError);
                if (startResult != NativeScrollSourceStart.Started)
                {
                    Console.WriteLine($"native scroll source: {startResult} {startError?.Message} — フォールバック (窓 wheel イベント)");
                    // Resampler モードは窓 wheel を無視する (native ソースとの二重計上防止) ため、
                    // native ソースが無いままだとスクロールが一切効かない。Smoother に落とす。
                    // Windows は native ソース未対応なので常にこの経路を通る。
                    if (effectiveMode == ScrollMode.Resampler)
                    {
                        effectiveMode = ScrollMode.Smoother;
                        Console.WriteLine("scroll mode: Resampler は native ソースを要するため Smoother で起動する (F1/F2/F3 で切替可)");
                    }
                }
            }
            _scrollMatrix.SetMode(effectiveMode);
            _scrollMatrix.RecordingEnabled = _options.Record;
            _imeBridge = new ImeBridge(new BrowserImeSink(_frameSource.Browser));
            _eventWatchInstance = this;
            unsafe { _sdl.StartTextInput(); _sdl.AddEventWatch(new PfnEventFilter(ImeEventWatch), null); }
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
            // Item C: 修飾キー状態をスクロールイベントに反映する
            var currentModifiers = CurrentModifiers();
            if (primaryDeltaX != 0 || primaryDeltaY != 0)
                _frameSource.Browser.SendMouseWheel(_mouseX, _mouseY, primaryDeltaX, primaryDeltaY, currentModifiers);
            if (secondaryDeltaX != 0 || secondaryDeltaY != 0)
                _frameSource.Browser.SendMouseWheel(_mouseX, _mouseY, secondaryDeltaX, secondaryDeltaY, currentModifiers);
            _lastSentDeltaX = primaryDeltaX + secondaryDeltaX;
            _lastSentDeltaY = primaryDeltaY + secondaryDeltaY; // Task 8 の統計用
            if (_frameSource.TickFrame(out var texturePointer, out var textureWidth, out var textureHeight, out var textureFormat))
            {
                // D3D11 は BGRA/RGBA で スワップチェーンの format family が変わるため受信値を伝える
                if (_renderer is D3D11FrameRenderer d3d11Renderer) d3d11Renderer.SetReceivedFormat(textureFormat);
                _renderer.Present(texturePointer, textureWidth, textureHeight);
                if (!_firstFrameShown)
                {
                    _firstFrameShown = true;
                    _replaySource?.Start();
                    UpdateTitle();
                }
            }
            else
            {
                // まだ 1 枚も来ていない: drawable だけ回す (黒画面)
                _renderer.Present(IntPtr.Zero, _options.Width, _options.Height);
            }
            _statistics?.RecordFrame(_statisticsFrameIndex++, deltaSeconds * 1000.0,
                _frameSource.Browser.PeekAcceleratedFrameId(), _lastSentDeltaX, _lastSentDeltaY, _scrollMatrix.Mode);
            // Item I: リプレイ完了時にタイトル更新 (一度だけ)
            if (_replaySource != null && _replaySource.Finished && !_replayFinishedShown)
            {
                _replayFinishedShown = true;
                UpdateTitle();
            }
            _frameSource.Browser.GetImeCaret(out var caretX, out var caretY, out var caretWidth, out var caretHeight);
            if ((caretX != _caretX || caretY != _caretY) && caretWidth >= 0)
            {
                _caretX = caretX;
                _caretY = caretY;
                var caretRectangle = new Rectangle<int>(caretX, caretY, System.Math.Max(caretWidth, 1), System.Math.Max(caretHeight, 16));
                unsafe { _sdl.SetTextInputRect(&caretRectangle); }
            }
        }

        private void UpdateTitle()
        {
            var recording = _scrollMatrix.RecordingEnabled ? " REC" : "";
            var replayDone = _replaySource is { Finished: true } ? " REPLAY done" : "";
            _window.Title = $"CefUnity.Viewer [{_scrollMatrix.Mode}]{recording}{replayDone}";
        }

        public void Dispose()
        {
            _eventWatchInstance = null;
            // Item H: _statistics の所有権は Program (using) にある。ここでは Dispose しない。
            _input?.Dispose();
            _renderer.Dispose();
            _window.Dispose();
        }

        private void OnWindowResize(Vector2D<int> newSize)
        {
            if (newSize.X <= 0 || newSize.Y <= 0) return;
            _frameSource.Resize(newSize.X, newSize.Y);
        }

        private void OnFocusChanged(bool focused)
        {
            if (!focused) _imeBridge?.OnFocusLost();
        }

        private void OnMouseMove(IMouse mouse, System.Numerics.Vector2 position)
        {
            _mouseX = (int)position.X;
            _mouseY = (int)position.Y;
            // Item C: 修飾キー状態をマウス移動に反映する
            _frameSource.Browser.SendMouseMove(_mouseX, _mouseY, CurrentModifiers());
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
            // Item C: 修飾キー状態をクリックに反映する
            _frameSource.Browser.SendMouseClick(_mouseX, _mouseY, ToCefMouseButton(button), mouseUp: false, _lastClickCount, CurrentModifiers());
        }

        private void OnMouseUp(IMouse mouse, Silk.NET.Input.MouseButton button)
        {
            // Item C: 修飾キー状態をクリックに反映する
            _frameSource.Browser.SendMouseClick(_mouseX, _mouseY, ToCefMouseButton(button), mouseUp: true, _lastClickCount, CurrentModifiers());
        }

        private void OnMouseScroll(IMouse mouse, ScrollWheel wheel)
        {
            _scrollMatrix.AddWheelSteps(wheel.X, wheel.Y);
        }

        /// <summary>Item C: 現在の修飾キー状態を CefEventFlags (uint) として返す。</summary>
        private uint CurrentModifiers() => _keyboard != null ? SilkKeyboardMapper.BuildModifiers(_keyboard) : 0u;

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
            // Item G: F1/F2/F3/F5 の KeyUp もここで消費し、ブラウザに届けない
            switch (key)
            {
                case Key.F1:
                case Key.F2:
                case Key.F3:
                case Key.F5:
                    return;
            }
            if (!SilkKeyboardMapper.TryMap(key, out var code)) return;
            _frameSource.Browser.SendKeyEvent(KeyEventType.KeyUp, code, SilkKeyboardMapper.BuildModifiers(keyboard));
        }

        private static unsafe int ImeEventWatch(void* userData, Event* sdlEvent)
        {
            var instance = _eventWatchInstance;
            if (instance == null) return 1;
            try
            {
                switch ((EventType)sdlEvent->Type)
                {
                    case EventType.TexteditingExt:
                        // Item A: SDL_IME_SUPPORT_EXTENDED_TEXT=1 が有効な場合、長い変換文字列は
                        // TEXTEDITING_EXT として届く (heap 上の NUL 終端 UTF-8 ポインタ)。
                        // Silk.NET 自身のイベントループはこのイベント型を無視して Free しないため、
                        // 受信側 (ここ) が必ず SDL_free で解放しなければならない。
                        var extText = ReadHeapUtf8(sdlEvent->EditExt.Text);
                        instance._imeBridge?.OnTextEditing(extText, sdlEvent->EditExt.Start);
                        instance._sdl.Free(sdlEvent->EditExt.Text);
                        break;
                    case EventType.Textediting:
                        // 通常の TEXTEDITING (32 バイト固定バッファ、SDL_IME_SUPPORT_EXTENDED_TEXT が
                        // 未サポートの SDL ビルドや短い文字列のフォールバックとして機能する)
                        instance._imeBridge?.OnTextEditing(ReadFixedUtf8(sdlEvent->Edit.Text, 32), sdlEvent->Edit.Start);
                        break;
                    case EventType.Textinput:
                        // 注: TEXTINPUT も 32 バイト固定バッファのため、>31 バイトの一括確定は断片化する
                        // (完全対応は後日)。通常の日本語入力は IME が分割して送るため実用上は問題ない。
                        instance._imeBridge?.OnTextInput(ReadFixedUtf8(sdlEvent->Text.Text, 32));
                        break;
                }
            }
            catch (Exception exception)
            {
                Console.Error.WriteLine($"[IME] {exception}");
            }
            return 1;
        }

        /// <summary>Item A: heap 上の NUL 終端 UTF-8 文字列を読み取る (TextEditingExtEvent.Text 用)。</summary>
        private static unsafe string ReadHeapUtf8(byte* pointer)
        {
            if (pointer == null) return string.Empty;
            const int maximumBytes = 4096; // 防御的上限
            var length = 0;
            while (length < maximumBytes && pointer[length] != 0) length++;
            return System.Text.Encoding.UTF8.GetString(pointer, length);
        }

        private static unsafe string ReadFixedUtf8(byte* bytes, int capacity)
        {
            var length = 0;
            while (length < capacity && bytes[length] != 0) length++;
            return System.Text.Encoding.UTF8.GetString(bytes, length);
        }
    }

    /// <summary>IImeSink → Browser の橋 (実行時配線)。</summary>
    internal sealed class BrowserImeSink : IImeSink
    {
        private readonly CefUnity.Interop.Browser _browser;

        public BrowserImeSink(CefUnity.Interop.Browser browser)
        {
            _browser = browser;
        }

        public void SetComposition(string text, uint cursorPosition) => _browser.ImeSetComposition(text, cursorPosition, cursorPosition);
        public void CommitText(string text) => _browser.ImeCommitText(text);
        public void SendCharacter(char character) => _browser.SendCharEvent(character);
        public void FinishComposition() => _browser.ImeFinishComposingText();
        public void CancelComposition() => _browser.ImeCancelComposition();
    }
}
