using System;
using System.Collections.Generic;
using System.Globalization;

namespace CefUnity.Runtime
{
    public sealed class ScrollReplayResult
    {
        public bool Ok;
        public string? Error;
        public int Events;
        public int Ticks;
        public int Mismatches;
        public IReadOnlyList<string> OutputLines = Array.Empty<string>();
    }

    /// <summary>
    ///   cef_scroll_record の S/E/T CSV を ScrollResampler(interpolating + predictive)へ
    ///   オフラインリプレイし、録画時の live 実排出との忠実度(mismatches)を測る純ロジック。
    ///   Editor/Harness は本クラスを呼び、I/O・終了コード・ログのみ担当する。
    /// </summary>
    public static class ScrollReplayRunner
    {
        public static ScrollReplayResult Run(IEnumerable<string> csvLines)
        {
            var interpolatingResampler = new ScrollResampler();
            var predictiveResampler = new ScrollResampler { Predictive = true };
            var outputLines = new List<string>();
            var scale = 1f;                       // S 行が無い旧録画は scale=1
            int events = 0, ticks = 0, mismatches = 0, lineNumber = 0;
            foreach (var line in csvLines)
            {
                lineNumber++;
                if (line.Length == 0) continue;
                try
                {
                    var columns = line.Split(',');
                    if (columns.Length >= 2 && columns[0] == "S")
                    {
                        scale = float.Parse(columns[1], CultureInfo.InvariantCulture);
                    }
                    else if (columns.Length >= 6 && columns[0] == "E")
                    {
                        if (columns.Length >= 7 && columns[6] != "1") continue;   // live 未転送は投入しない
                        var inputEvent = new ScrollInputEvent
                        {
                            Timestamp = double.Parse(columns[1], CultureInfo.InvariantCulture),
                            DeltaXPixels = float.Parse(columns[2], CultureInfo.InvariantCulture) * scale,
                            DeltaYPixels = float.Parse(columns[3], CultureInfo.InvariantCulture) * scale,
                            Phase = (ScrollPhase)byte.Parse(columns[4], CultureInfo.InvariantCulture),
                            Precise = columns[5] == "1",
                        };
                        if (!inputEvent.Precise) continue;                     // precise のみ
                        interpolatingResampler.AddEvent(in inputEvent);
                        predictiveResampler.AddEvent(in inputEvent);
                        events++;
                    }
                    else if (columns.Length >= 5 && columns[0] == "T")
                    {
                        var now = double.Parse(columns[1], CultureInfo.InvariantCulture);
                        interpolatingResampler.Tick(now, out var interpolatedDeltaX, out var interpolatedDeltaY);
                        predictiveResampler.Tick(now, out var predictedDeltaX, out var predictedDeltaY);
                        outputLines.Add($"{columns[1]},{columns[2]},{columns[3]},{interpolatedDeltaX},{interpolatedDeltaY},{predictedDeltaX},{predictedDeltaY}");
                        var wasPredictive = columns[4] == "1";
                        var liveDeltaX = int.Parse(columns[2], CultureInfo.InvariantCulture);
                        var liveDeltaY = int.Parse(columns[3], CultureInfo.InvariantCulture);
                        if ((wasPredictive ? predictedDeltaX : interpolatedDeltaX) != liveDeltaX ||
                            (wasPredictive ? predictedDeltaY : interpolatedDeltaY) != liveDeltaY)
                            mismatches++;
                        ticks++;
                    }
                }
                catch (Exception exception)
                {
                    return new ScrollReplayResult { Ok = false, Error = $"parse error at line {lineNumber}: \"{line}\" ({exception.Message})" };
                }
            }
            if (ticks == 0)
                return new ScrollReplayResult { Ok = false, Error = "no T lines (録画が空)" };
            return new ScrollReplayResult
            {
                Ok = true, Events = events, Ticks = ticks, Mismatches = mismatches, OutputLines = outputLines,
            };
        }
    }
}
