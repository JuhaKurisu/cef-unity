using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="ScrollResampler" /> の単体テスト。合成イベント列で
    ///     「per-frame 均一化・補間/外挿境界・momentum 終端の即時停止・総量保存」を検証する。
    ///     設計: docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md
    /// </summary>
    public class ScrollResamplerTests
    {
        private const double E = 1.0 / 120.0; // 120Hz イベント間隔
        private const double F = 1.0 / 60.0;  // 60fps フレーム間隔

        private static ScrollInputEvent Ev(double t, float dy, ScrollPhase phase = ScrollPhase.MomentumChanged)
            => new ScrollInputEvent { Timestamp = t, DyPx = dy, Precise = true, Phase = phase };

        // ---- イベント 1 点では補間せず即時排出 (低遅延スタート) ----

        [Test]
        public void SingleEvent_EmitsImmediately()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.0, 30f));
            r.Tick(0.006, out _, out var dy);
            Assert.AreEqual(30, dy);
        }

        // ---- 均一 120Hz ストリーム → 追いつき後は毎フレームちょうど均一排出 ----

        [Test]
        public void SteadyStream_UniformPerFrameOutput()
        {
            var r = new ScrollResampler();
            var emitted = new System.Collections.Generic.List<int>();
            var eventIndex = 0;
            for (var k = 0; k < 30; k++)
            {
                var now = 0.02 + k * F;
                while (eventIndex * E <= now)
                {
                    r.AddEvent(Ev(eventIndex * E, 5f));
                    eventIndex++;
                }
                r.Tick(now, out _, out var dy);
                emitted.Add(dy);
            }
            // 600px/s の均一入力 → 初回フレームの追いつき以降は毎フレーム 10px ちょうど
            for (var k = 1; k < emitted.Count; k++)
                Assert.AreEqual(10, emitted[k], $"frame {k}");
        }

        // ---- イベントタイミングに ±3ms のジッター → 排出は準均一のまま ----

        [Test]
        public void JitteredTimestamps_OutputStaysNearUniform()
        {
            var r = new ScrollResampler();
            double[] jitter = { 0, +0.003, -0.002, +0.001, -0.003, +0.002 };
            var emitted = new System.Collections.Generic.List<int>();
            var eventIndex = 0;
            for (var k = 0; k < 24; k++)
            {
                var now = 0.02 + k * F;
                while (true)
                {
                    var t = eventIndex * E + jitter[eventIndex % jitter.Length];
                    if (t > now) break;
                    r.AddEvent(Ev(t, 5f));
                    eventIndex++;
                }
                r.Tick(now, out _, out var dy);
                emitted.Add(dy);
            }
            for (var k = 2; k < emitted.Count; k++)
                Assert.That(emitted[k], Is.InRange(5, 15), $"frame {k}");
        }

        // ---- momentum 終端: 残差を即時排出して停止 (浮遊しない) ----

        [Test]
        public void MomentumEnded_FlushesResidualThenStops()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.000, 10f));
            r.AddEvent(Ev(E, 10f));
            r.Tick(E + 0.005, out _, out var d1);
            r.AddEvent(Ev(2 * E, 0f, ScrollPhase.MomentumEnded));
            r.Tick(2 * E + 0.005, out _, out var d2);
            Assert.AreEqual(20, d1 + d2, "終端までの総量が排出される");
            Assert.IsFalse(r.IsActive);
            r.Tick(2 * E + 0.005 + F, out _, out var d3);
            Assert.AreEqual(0, d3, "終端後は排出なし");
        }

        // ---- 終端イベント取り逃し → 100ms グレースで残差排出 ----

        [Test]
        public void GraceTimeout_FlushesResidual()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.000, 10f));
            r.AddEvent(Ev(E, 10f));
            r.Tick(0.005 + E * 0.5, out _, out var d1); // sample=E/2 → 補間で 15 排出
            r.Tick(E + 0.150, out _, out var d2);        // グレース超過 → 残差 5
            Assert.AreEqual(20, d1 + d2);
            Assert.IsFalse(r.IsActive);
        }

        // ---- 外挿は 8ms で頭打ち、終端フラッシュで巻き戻さない ----

        [Test]
        public void Extrapolation_CappedAndNoBackwardFlush()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.000, 10f));
            r.AddEvent(Ev(E, 10f)); // 速度 1200px/s
            r.Tick(E + 0.005 + 0.050, out _, out var d1);
            // 外挿は cap=8ms まで: 20 + 1200*0.008 = 29.6 → 30
            Assert.AreEqual(30, d1);
            r.Tick(E + 0.200, out _, out var d2);
            // サンプルは最終位置 20 を追い越している → 巻き戻さない
            Assert.AreEqual(0, d2);
            Assert.IsFalse(r.IsActive);
        }

        // ---- 方向反転でも正味総量が保存される ----

        [Test]
        public void Reversal_NetTotalConserved()
        {
            var r = new ScrollResampler();
            var t = 0.0;
            for (var i = 0; i < 6; i++) { r.AddEvent(Ev(t, +10f)); t += E; }
            for (var i = 0; i < 6; i++) { r.AddEvent(Ev(t, -20f)); t += E; }
            r.AddEvent(Ev(t, 0f, ScrollPhase.MomentumEnded));
            var total = 0;
            for (var k = 0; k < 20; k++)
            {
                r.Tick(0.02 + k * F, out _, out var dy);
                total += dy;
            }
            Assert.AreEqual(-60, total, "正味 +60-120 = -60");
            Assert.IsFalse(r.IsActive);
        }

        // ---- 小数 delta の端数繰り越しで総量保存 ----

        [Test]
        public void FractionCarry_ConservesFractionalDeltas()
        {
            var r = new ScrollResampler();
            var t = 0.0;
            for (var i = 0; i < 10; i++) { r.AddEvent(Ev(t, 1.5f)); t += E; }
            r.AddEvent(Ev(t, 0f, ScrollPhase.MomentumEnded));
            var total = 0;
            for (var k = 0; k < 10; k++) { r.Tick(0.02 + k * F, out _, out var dy); total += dy; }
            Assert.AreEqual(15, total);
        }

        // ---- momentum 終端の Tick を挟まず新ジェスチャ開始 → 残差は引き継がれる ----

        [Test]
        public void NewGestureAfterMomentumEnd_ContinuesCleanly()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.0, 10f));
            r.AddEvent(Ev(E, 10f));
            r.AddEvent(Ev(2 * E, 0f, ScrollPhase.MomentumEnded));
            r.AddEvent(Ev(3 * E, 7f, ScrollPhase.GestureBegan));
            var total = 0;
            for (var k = 0; k < 10; k++) { r.Tick(0.03 + k * F, out _, out var dy); total += dy; }
            r.AddEvent(Ev(0.2, 0f, ScrollPhase.MomentumEnded));
            r.Tick(0.25, out _, out var last);
            Assert.AreEqual(27, total + last, "旧 20 + 新 7 が全て排出される");
        }

        // ---- Reset で全状態破棄 ----

        [Test]
        public void Reset_ClearsState()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.0, 100f));
            r.Reset();
            Assert.IsFalse(r.IsActive);
            r.Tick(0.05, out _, out var dy);
            Assert.AreEqual(0, dy);
        }
    }
}
