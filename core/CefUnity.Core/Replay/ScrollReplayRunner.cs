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
        public IReadOnlyList<string> OutLines = Array.Empty<string>();
    }

    /// <summary>
    ///   cef_scroll_record の S/E/T CSV を ScrollResampler(interp + predictive)へ
    ///   オフラインリプレイし、録画時の live 実排出との忠実度(mismatches)を測る純ロジック。
    ///   Editor/Harness は本クラスを呼び、I/O・終了コード・ログのみ担当する。
    /// </summary>
    public static class ScrollReplayRunner
    {
        public static ScrollReplayResult Run(IEnumerable<string> csvLines)
        {
            var interp = new ScrollResampler();
            var pred = new ScrollResampler { Predictive = true };
            var outLines = new List<string>();
            var scale = 1f;                       // S 行が無い旧録画は scale=1
            int events = 0, ticks = 0, mismatches = 0, lineNo = 0;
            foreach (var line in csvLines)
            {
                lineNo++;
                if (line.Length == 0) continue;
                try
                {
                    var c = line.Split(',');
                    if (c.Length >= 2 && c[0] == "S")
                    {
                        scale = float.Parse(c[1], CultureInfo.InvariantCulture);
                    }
                    else if (c.Length >= 6 && c[0] == "E")
                    {
                        if (c.Length >= 7 && c[6] != "1") continue;   // live 未転送は投入しない
                        var e = new ScrollInputEvent
                        {
                            Timestamp = double.Parse(c[1], CultureInfo.InvariantCulture),
                            DxPx = float.Parse(c[2], CultureInfo.InvariantCulture) * scale,
                            DyPx = float.Parse(c[3], CultureInfo.InvariantCulture) * scale,
                            Phase = (ScrollPhase)byte.Parse(c[4], CultureInfo.InvariantCulture),
                            Precise = c[5] == "1",
                        };
                        if (!e.Precise) continue;                     // precise のみ
                        interp.AddEvent(in e);
                        pred.AddEvent(in e);
                        events++;
                    }
                    else if (c.Length >= 5 && c[0] == "T")
                    {
                        var now = double.Parse(c[1], CultureInfo.InvariantCulture);
                        interp.Tick(now, out var idx, out var idy);
                        pred.Tick(now, out var pdx, out var pdy);
                        outLines.Add($"{c[1]},{c[2]},{c[3]},{idx},{idy},{pdx},{pdy}");
                        var wasPredictive = c[4] == "1";
                        var liveDx = int.Parse(c[2], CultureInfo.InvariantCulture);
                        var liveDy = int.Parse(c[3], CultureInfo.InvariantCulture);
                        if ((wasPredictive ? pdx : idx) != liveDx || (wasPredictive ? pdy : idy) != liveDy)
                            mismatches++;
                        ticks++;
                    }
                }
                catch (Exception ex)
                {
                    return new ScrollReplayResult { Ok = false, Error = $"parse error at line {lineNo}: \"{line}\" ({ex.Message})" };
                }
            }
            if (ticks == 0)
                return new ScrollReplayResult { Ok = false, Error = "no T lines (録画が空)" };
            return new ScrollReplayResult
            {
                Ok = true, Events = events, Ticks = ticks, Mismatches = mismatches, OutLines = outLines,
            };
        }
    }
}
