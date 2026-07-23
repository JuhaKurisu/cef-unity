using System.Globalization;
using NUnit.Framework;
using UnityEngine;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     実録画によるリサンプラの非退行テスト。2026-07-23 の実トラックパッドスクロール録画
    ///     (慣性中の入力欠落 18 回を含むがフレームは健全な session) を新リサンプラに通し、
    ///     録画時の実排出 (T 行の live 列) と比較する。
    ///     この録画の欠落は 27〜34ms と短く既存の外挿上限内で処理されるため、momentum
    ///     橋渡し (MomentumBridgeCap、対象は 35ms 超の長欠落 — 合成テストで検証) は
    ///     発火しない。よってここでは「実データで挙動が退行しない (排出の途切れが増えず
    ///     総量が保存される)」ことを固定する。
    ///     録画: test-results/scroll-drought-2026-07-23/ (リポジトリ同梱、開発リポジトリ専用)。
    /// </summary>
    public class ScrollDroughtRecordingTests
    {
        [Test]
        public void RealRecordingReplay_NoRegression_GapsNotWorse_TotalConserved()
        {
            // nozerowait 録画を使う: ベースライン録画は欠落がフレームストールと同時
            // 発生しており Tick 自体が止まっている (橋渡しの出番がない)。nozerowait は
            // フレームが健全 (tick 間隔 >25ms = 2.2%) なまま慣性中の入力欠落 18 回を
            // 含み、橋渡しの効果を分離検証できる。
            var path = System.IO.Path.GetFullPath(
                System.IO.Path.Combine(Application.dataPath, "..", "..",
                    "test-results/scroll-drought-2026-07-23/cef_scroll_events_nozerowait.csv"));
            if (!System.IO.File.Exists(path))
                Assert.Ignore($"録画が無い環境ではスキップ: {path}");

            var r = new ScrollResampler { Predictive = true };
            var scale = 1f;
            // tick ごとの (排出, live 排出) を収集
            var newDys = new System.Collections.Generic.List<int>();
            var liveDys = new System.Collections.Generic.List<int>();
            long newTotal = 0, liveTotal = 0;
            foreach (var line in System.IO.File.ReadLines(path))
            {
                if (line.Length == 0) continue;
                var c = line.Split(',');
                if (c[0] == "S" && c.Length >= 2)
                {
                    scale = float.Parse(c[1], CultureInfo.InvariantCulture);
                }
                else if (c[0] == "E" && c.Length >= 7)
                {
                    if (c[6] != "1") continue; // live で未転送 (ブラウザ外) は投入しない
                    var e = new ScrollInputEvent
                    {
                        Timestamp = double.Parse(c[1], CultureInfo.InvariantCulture),
                        DxPx = float.Parse(c[2], CultureInfo.InvariantCulture) * scale,
                        DyPx = float.Parse(c[3], CultureInfo.InvariantCulture) * scale,
                        Phase = (ScrollPhase)byte.Parse(c[4], CultureInfo.InvariantCulture),
                        Precise = c[5] == "1",
                    };
                    if (!e.Precise) continue;
                    r.AddEvent(in e);
                }
                else if (c[0] == "T" && c.Length >= 5)
                {
                    var now = double.Parse(c[1], CultureInfo.InvariantCulture);
                    r.Tick(now, out _, out var dy);
                    newDys.Add(dy);
                    newTotal += dy;
                    var live = int.Parse(c[3], CultureInfo.InvariantCulture);
                    liveDys.Add(live);
                    liveTotal += live;
                }
            }
            Assert.Greater(liveDys.Count, 100, "録画に十分な Tick がある");

            // 動きの最中 (前後 6 tick 以内に排出あり) の排出ゼロ tick が live より増えない
            var liveZero = CountMidMotionZeros(liveDys);
            var newZero = CountMidMotionZeros(newDys);
            Assert.LessOrEqual(newZero.zeros, liveZero.zeros,
                $"動き中の排出ゼロが退行しない: live={liveZero.zeros}/{liveZero.active} → new={newZero.zeros}/{newZero.active}");

            // 総移動量は録画時の実排出と同水準 (±2%)
            var diff = System.Math.Abs(newTotal - liveTotal) / (double)System.Math.Max(1, System.Math.Abs(liveTotal));
            Assert.Less(diff, 0.02, $"総量が保存される: live={liveTotal} new={newTotal}");
        }

        private static (int zeros, int active) CountMidMotionZeros(System.Collections.Generic.List<int> dys)
        {
            var zeros = 0;
            var active = 0;
            for (var i = 0; i < dys.Count; i++)
            {
                // 前後 6 tick 以内に排出がある = 動きの最中
                var near = false;
                for (var j = System.Math.Max(0, i - 6); j <= System.Math.Min(dys.Count - 1, i + 6); j++)
                    if (j != i && dys[j] != 0) { near = true; break; }
                if (!near) continue;
                active++;
                if (dys[i] == 0) zeros++;
            }
            return (zeros, active);
        }
    }
}
