using System.Globalization;
using CefUnity.Runtime;
using UnityEditor;
using UnityEngine;

namespace CefUnity.Editor
{
    /// <summary>
    ///     開発用: cef_scroll_record で録画した生スクロールイベント列を、本物の
    ///     ScrollResampler にオフラインでリプレイする。実機のユーザー操作なしで
    ///     「低速時の飛び」等の入力→排出変換の不具合を再現・修正検証するための基盤。
    ///     batchmode: Unity -batchmode -quit -executeMethod CefUnity.Editor.ScrollReplay.Run
    ///     入力: $TMPDIR/cef_scroll_events.csv (E/T 行)
    ///     出力: $TMPDIR/cef_scroll_replay.csv — now,liveDx,liveDy,interpDx,interpDy,predDx,predDy
    ///     (live は録画時の実排出。interp/pred は補間/予測モードでのリプレイ結果。
    ///      録画時と同モードの列が live と一致すればリプレイは忠実)
    /// </summary>
    public static class ScrollReplay
    {
        public static void Run()
        {
            var tmp = System.IO.Path.GetTempPath();
            var src = System.IO.Path.Combine(tmp, "cef_scroll_events.csv");
            var dst = System.IO.Path.Combine(tmp, "cef_scroll_replay.csv");
            var interp = new ScrollResampler();
            var pred = new ScrollResampler { Predictive = true };
            var outLines = new System.Collections.Generic.List<string>();
            var events = 0;
            var ticks = 0;
            foreach (var line in System.IO.File.ReadLines(src))
            {
                var c = line.Split(',');
                if (c.Length >= 6 && c[0] == "E")
                {
                    var e = new ScrollInputEvent
                    {
                        Timestamp = double.Parse(c[1], CultureInfo.InvariantCulture),
                        DxPx = float.Parse(c[2], CultureInfo.InvariantCulture),
                        DyPx = float.Parse(c[3], CultureInfo.InvariantCulture),
                        Phase = (ScrollPhase)byte.Parse(c[4], CultureInfo.InvariantCulture),
                        Precise = c[5] == "1",
                    };
                    if (!e.Precise) continue; // 本番同様、リサンプラには precise のみ
                    interp.AddEvent(in e);
                    pred.AddEvent(in e);
                    events++;
                }
                else if (c.Length >= 5 && c[0] == "T")
                {
                    var now = double.Parse(c[1], CultureInfo.InvariantCulture);
                    interp.Tick(now, out var idx, out var idy);
                    pred.Tick(now, out var pdx, out var pdy);
                    outLines.Add(
                        $"{c[1]},{c[2]},{c[3]},{idx},{idy},{pdx},{pdy}");
                    ticks++;
                }
            }
            System.IO.File.WriteAllText(dst, string.Join("\n", outLines) + "\n");
            Debug.Log($"[ScrollReplay] events={events} ticks={ticks} out={dst}");
        }
    }
}
