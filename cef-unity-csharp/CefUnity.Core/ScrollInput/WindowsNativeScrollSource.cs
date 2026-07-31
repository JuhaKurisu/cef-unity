using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     Windows のメッセージフック (client dll 内 scroll_monitor_windows.rs) から生スクロール
    ///     イベントを取得する <see cref="IScrollEventSource" /> 実装。
    ///     <para>
    ///         ネイティブ側は Start を呼んだスレッドに WH_GETMESSAGE フックを張り、メッセージ
    ///         ポンプが取り出す WM_MOUSEWHEEL / WM_MOUSEHWHEEL を観測してリングバッファに積む。
    ///         Raw Input は登録しない (登録はプロセス単位の後勝ちで、Unity Input System の
    ///         Mouse.delta 配送を奪ってしまうため)。Start / Poll とも Unity のメインスレッド
    ///         から呼ぶこと (フックは呼び出しスレッドにのみ張られる)。
    ///     </para>
    ///     <para>
    ///         単位: Windows のホイールは 120 = 1 ノッチ。ネイティブ側が 120 で割った
    ///         「ノッチ数」を <see cref="ScrollInputEvent.DeltaYPixels" /> に入れ、
    ///         <see cref="ScrollInputEvent.Precise" /> は false になる。CSS ピクセルへの換算
    ///         (WheelPixelsPerStep) は <c>ScrollResampler</c> 側の責務。
    ///     </para>
    /// </summary>
    public sealed class WindowsNativeScrollSource : IScrollEventSource
    {
        // WM_MOUSEWHEEL のホイール符号は「手前から奥 = 正」。
        // 現行の Input.mouseScrollDelta 経路と同じ向きなので変換は不要。
        // 実機で逆だった場合はここを -1 にする。
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
            int count;
            fixed (CefScrollEvent* pointer = _native)
            {
                count = NativeMethods.cef_scroll_monitor_poll(pointer, Math.Min(_native.Length, buffer.Length));
            }
            for (var index = 0; index < count; index++)
            {
                buffer[index] = new ScrollInputEvent
                {
                    Timestamp = _native[index].timestamp,
                    DeltaXPixels = _native[index].delta_x * SignX,
                    DeltaYPixels = _native[index].delta_y * SignY,
                    Precise = _native[index].precise != 0,
                    Phase = (ScrollPhase)_native[index].phase,
                };
            }
            return count;
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
