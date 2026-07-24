using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     macOS の NSEvent ローカルモニタ (client dylib 内 scroll_monitor.m) から
    ///     生スクロールイベントを取得する <see cref="IScrollEventSource" /> 実装。
    ///     Unity メインスレッド == AppKit メインスレッドで Poll する前提。
    /// </summary>
    public sealed class MacNativeScrollSource : IScrollEventSource
    {
        // NSEvent.scrollingDelta の符号は現行 Input.mouseScrollDelta 経路と同じ想定。
        // 実機検証 (Task 4) で逆だった場合はここを -1 にする。
        private const float SignX = 1f;
        private const float SignY = 1f;

        private bool _started;
        private readonly CefScrollEvent[] _native = new CefScrollEvent[256];

        public bool Start()
        {
            _started = NativeMethods.cef_scroll_monitor_start() != 0;
            return _started;
        }

        public unsafe int Poll(ScrollInputEvent[] buffer)
        {
            if (!_started) return 0;
            int n;
            fixed (CefScrollEvent* p = _native)
            {
                n = NativeMethods.cef_scroll_monitor_poll(p, Math.Min(_native.Length, buffer.Length));
            }
            for (var i = 0; i < n; i++)
            {
                buffer[i] = new ScrollInputEvent
                {
                    Timestamp = _native[i].timestamp,
                    DxPx = _native[i].dx * SignX,
                    DyPx = _native[i].dy * SignY,
                    Precise = _native[i].precise != 0,
                    Phase = (ScrollPhase)_native[i].phase,
                };
            }
            return n;
        }

        public double Now => NativeMethods.cef_scroll_monitor_now();

        public void Dispose()
        {
            if (!_started) return;
            _started = false;
            NativeMethods.cef_scroll_monitor_stop();
        }
    }
}
