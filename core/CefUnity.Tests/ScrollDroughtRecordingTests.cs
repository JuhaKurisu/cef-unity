using System.Globalization;
using NUnit.Framework;

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
            var path = System.IO.Path.Combine(
                System.AppContext.BaseDirectory, "fixtures", "cef_scroll_events_nozerowait.csv");
            if (!System.IO.File.Exists(path))
                Assert.Ignore($"録画が無い環境ではスキップ: {path}");

            var resampler = new ScrollResampler { Predictive = true };
            var scale = 1f;
            // tick ごとの (排出, live 排出) を収集
            var newDeltaYs = new System.Collections.Generic.List<int>();
            var liveDeltaYs = new System.Collections.Generic.List<int>();
            long newTotal = 0, liveTotal = 0;
            foreach (var line in System.IO.File.ReadLines(path))
            {
                if (line.Length == 0) continue;
                var columns = line.Split(',');
                if (columns[0] == "S" && columns.Length >= 2)
                {
                    scale = float.Parse(columns[1], CultureInfo.InvariantCulture);
                }
                else if (columns[0] == "E" && columns.Length >= 7)
                {
                    if (columns[6] != "1") continue; // live で未転送 (ブラウザ外) は投入しない
                    var inputEvent = new ScrollInputEvent
                    {
                        Timestamp = double.Parse(columns[1], CultureInfo.InvariantCulture),
                        DeltaXPixels = float.Parse(columns[2], CultureInfo.InvariantCulture) * scale,
                        DeltaYPixels = float.Parse(columns[3], CultureInfo.InvariantCulture) * scale,
                        Phase = (ScrollPhase)byte.Parse(columns[4], CultureInfo.InvariantCulture),
                        Precise = columns[5] == "1",
                    };
                    if (!inputEvent.Precise) continue;
                    resampler.AddEvent(in inputEvent);
                }
                else if (columns[0] == "T" && columns.Length >= 5)
                {
                    var now = double.Parse(columns[1], CultureInfo.InvariantCulture);
                    resampler.Tick(now, out _, out var deltaY);
                    newDeltaYs.Add(deltaY);
                    newTotal += deltaY;
                    var live = int.Parse(columns[3], CultureInfo.InvariantCulture);
                    liveDeltaYs.Add(live);
                    liveTotal += live;
                }
            }
            Assert.Greater(liveDeltaYs.Count, 100, "録画に十分な Tick がある");

            // 動きの最中 (前後 6 tick 以内に排出あり) の排出ゼロ tick が live より増えない
            var liveZero = CountMidMotionZeros(liveDeltaYs);
            var newZero = CountMidMotionZeros(newDeltaYs);
            Assert.LessOrEqual(newZero.zeros, liveZero.zeros,
                $"動き中の排出ゼロが退行しない: live={liveZero.zeros}/{liveZero.active} → new={newZero.zeros}/{newZero.active}");

            // 総移動量は録画時の実排出と同水準 (±2%)
            var diff = System.Math.Abs(newTotal - liveTotal) / (double)System.Math.Max(1, System.Math.Abs(liveTotal));
            Assert.Less(diff, 0.02, $"総量が保存される: live={liveTotal} new={newTotal}");
        }

        private static (int zeros, int active) CountMidMotionZeros(System.Collections.Generic.List<int> deltaYs)
        {
            var zeros = 0;
            var active = 0;
            for (var index = 0; index < deltaYs.Count; index++)
            {
                // 前後 6 tick 以内に排出がある = 動きの最中
                var near = false;
                for (var neighborIndex = System.Math.Max(0, index - 6); neighborIndex <= System.Math.Min(deltaYs.Count - 1, index + 6); neighborIndex++)
                    if (neighborIndex != index && deltaYs[neighborIndex] != 0) { near = true; break; }
                if (!near) continue;
                active++;
                if (deltaYs[index] == 0) zeros++;
            }
            return (zeros, active);
        }
    }
}
