using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     cef_scroll_record の E 行 (over=1) を録画タイムラインどおりにライブ再生する
    ///     IScrollEventSource。Unity で録った入力を Viewer の生きた CEF に流し、
    ///     ホスト間比較 (spec の実験プロトコル) を行うための供給源。
    ///     ScrollReplayRunner (オフライン忠実度照合) とは役割が異なる。
    /// </summary>
    public sealed class ScrollReplaySource : IScrollEventSource
    {
        private readonly List<ScrollInputEvent> _events = new List<ScrollInputEvent>();
        private readonly Func<double> _clock;
        private double _clockStart;
        private double _timelineStart;
        private int _cursor;
        private bool _started;

        public ScrollReplaySource(IEnumerable<string> csvLines, Func<double>? clock = null)
        {
            _clock = clock ?? DefaultClock;
            var scale = 1f;
            foreach (var line in csvLines)
            {
                if (line.Length == 0) continue;
                var columns = line.Split(',');
                if (columns.Length >= 2 && columns[0] == "S")
                {
                    scale = float.Parse(columns[1], CultureInfo.InvariantCulture);
                }
                else if (columns.Length >= 7 && columns[0] == "E" && columns[6] == "1")
                {
                    _events.Add(new ScrollInputEvent
                    {
                        Timestamp = double.Parse(columns[1], CultureInfo.InvariantCulture),
                        DeltaXPixels = float.Parse(columns[2], CultureInfo.InvariantCulture) * scale,
                        DeltaYPixels = float.Parse(columns[3], CultureInfo.InvariantCulture) * scale,
                        Phase = (ScrollPhase)byte.Parse(columns[4], CultureInfo.InvariantCulture),
                        Precise = columns[5] == "1",
                    });
                }
            }
        }

        public int TotalEvents => _events.Count;

        public bool Finished => _cursor >= _events.Count;

        public bool Start()
        {
            if (_events.Count == 0) return false;
            _clockStart = _clock();
            _timelineStart = _events[0].Timestamp;
            _started = true;
            return true;
        }

        public double Now => _timelineStart + (_clock() - _clockStart);

        public int Poll(ScrollInputEvent[] buffer)
        {
            if (!_started) return 0;
            var now = Now;
            var count = 0;
            while (_cursor < _events.Count && count < buffer.Length && _events[_cursor].Timestamp <= now)
                buffer[count++] = _events[_cursor++];
            return count;
        }

        public void Dispose() { }

        private static double DefaultClock() => Stopwatch.GetTimestamp() / (double)Stopwatch.Frequency;
    }
}
