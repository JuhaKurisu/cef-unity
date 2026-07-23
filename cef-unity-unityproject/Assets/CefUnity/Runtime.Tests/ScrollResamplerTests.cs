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

        // ---- 60Hz イベント (フレームと同率) でもビートジッターが出ない (適応オフセット+履歴4点) ----

        [Test]
        public void SteadyStream60Hz_NoBeatJitter()
        {
            var r = new ScrollResampler();
            var emitted = new System.Collections.Generic.List<int>();
            var eventIndex = 0;
            for (var k = 0; k < 40; k++)
            {
                var now = 0.05 + k * F;
                while (eventIndex * F <= now) { r.AddEvent(Ev(eventIndex * F, 10f)); eventIndex++; }
                r.Tick(now, out _, out var dy);
                emitted.Add(dy);
            }
            // 適応オフセット収束中も偏差は丸め ±1px に収まる (欠陥時は ±2〜4px のビートが交互に出る)
            for (var k = 1; k < emitted.Count; k++)
                Assert.That(emitted[k], Is.InRange(9, 11), $"frame {k}");
        }

        // ---- 予測モード: 60Hz 定常で均一かつ追従遅れが ~5ms 相当に縮む ----

        [Test]
        public void Predictive_SteadyStream60Hz_UniformAndLowLatency()
        {
            var r = new ScrollResampler { Predictive = true };
            var emitted = new System.Collections.Generic.List<int>();
            var total = 0;
            var eventIndex = 0;
            var lastNow = 0.0;
            for (var k = 0; k < 40; k++)
            {
                var now = 0.05 + k * F;
                lastNow = now;
                while (eventIndex * F <= now) { r.AddEvent(Ev(eventIndex * F, 10f)); eventIndex++; }
                r.Tick(now, out _, out var dy);
                emitted.Add(dy);
                total += dy;
            }
            for (var k = 1; k < emitted.Count; k++)
                Assert.That(emitted[k], Is.InRange(9, 11), $"frame {k}");
            // P(t) = 600t + 10 の直線。予測モードの追従遅れは ~5ms (3px)。
            // 補間モード (遅れ ~21ms ≈ 12.5px) では下限を割る → モード差を検証。
            var pNow = 600.0 * lastNow + 10.0;
            Assert.GreaterOrEqual(total, pNow - 6.0, "追従遅れが ~5ms 相当 (予測が効いている)");
        }

        // ---- 予測モード: 急停止でも巻き戻し (負の排出) が出ない ----

        [Test]
        public void Predictive_AbruptStop_NoBacktrack()
        {
            var r = new ScrollResampler { Predictive = true };
            var emitted = new System.Collections.Generic.List<int>();
            var total = 0;
            // 600px/s で 4 イベント → 以後 delta 0 (急停止)
            for (var k = 0; k < 10; k++)
            {
                var now = k * F + 0.006;
                if (k < 4) r.AddEvent(Ev(k * F, 10f));
                else if (k < 7) r.AddEvent(Ev(k * F, 0f));
                r.Tick(now, out _, out var dy);
                emitted.Add(dy);
                total += dy;
            }
            foreach (var dy in emitted)
                Assert.GreaterOrEqual(dy, 0, "巻き戻し (負の排出) は出ない");
            // 入力合計 40px。外挿オーバーシュートの保持分 (+1px 程度) までは許容
            Assert.That(total, Is.InRange(40, 42));
        }

        // ---- 予測モード: 急停止のオーバーシュート残差が次ジェスチャ開始時に飛びとして出ない ----

        [Test]
        public void Predictive_OvershootResidual_NotEmittedOnNextGesture()
        {
            var r = new ScrollResampler { Predictive = true };
            // 1800px/s で 4 イベント → 外挿がサンプルを先行させる
            for (var k = 0; k < 4; k++) r.AddEvent(Ev(k * F, 30f));
            r.Tick(3 * F + 0.012, out _, out _);
            // 急停止 (delta=0 の終端イベント → 直近セグメントの傾きは 0)
            r.AddEvent(Ev(4 * F, 0f, ScrollPhase.MomentumEnded));
            r.Tick(4 * F + 0.006, out _, out var d2);
            Assert.GreaterOrEqual(d2, 0, "終端フラッシュで負の排出をしない");
            Assert.IsFalse(r.IsActive);
            // 新ジェスチャ (同方向 5px): 滞留残差による飛びが出ない
            r.AddEvent(Ev(0.2, 5f, ScrollPhase.GestureBegan));
            r.Tick(0.2 + 0.006, out _, out var d3);
            Assert.That(d3, Is.InRange(0, 6), "次ジェスチャ開始時に位置が飛ばない");
        }

        // ---- 予測モード: phase 遷移の近接イベント (0.2ms 差) で外挿傾きが発散しない ----

        [Test]
        public void Predictive_NearSimultaneousPhaseTransition_NoSpike()
        {
            var r = new ScrollResampler { Predictive = true };
            var t = 0.0;
            // 低速ジェスチャ (60Hz, -2px)
            for (var k = 0; k < 6; k++) { r.AddEvent(Ev(t, -2f, ScrollPhase.GestureChanged)); t += F; }
            // 遷移: GestureEnded (dy=0) の 0.2ms 後に MomentumBegan (-15px) — 実録画のパターン
            r.AddEvent(Ev(t, 0f, ScrollPhase.GestureEnded));
            r.AddEvent(Ev(t + 0.0002, -15f, ScrollPhase.MomentumBegan));
            // 直後の Tick 群でスパイクが出ない (修正前は数百〜数千 px)
            var worst = 0;
            for (var k = 0; k < 6; k++)
            {
                r.Tick(t + 0.004 + k * F, out _, out var dy);
                if (System.Math.Abs(dy) > System.Math.Abs(worst)) worst = dy;
            }
            Assert.LessOrEqual(System.Math.Abs(worst), 60, $"外挿スパイクが出ない (worst={worst})");
        }

        // ---- 予測モード: 慣性中のイベント欠落 (OS コアレッシング) の橋渡し ----
        // 実測 (2026-07-23): メインスレッドブロック起因で慣性中に最大 66ms (4F) の
        // イベント欠落 + 欠落明けに溜め分の巨大イベントが届く。橋渡しが無いと
        // 「数フレーム停止 → ジャンプ」のガタつきになる。

        [Test]
        public void Predictive_MomentumDrought_BridgesAtEstablishedVelocity()
        {
            var r = new ScrollResampler { Predictive = true };
            // 600px/s の momentum ストリームで速度確立 (Ev の既定 phase = MomentumChanged)
            for (var k = 0; k <= 5; k++) r.AddEvent(Ev(k * F, 10f));
            r.Tick(5 * F + 0.006, out _, out _);
            // 66ms 欠落: イベント無しのまま 3 フレーム Tick — 橋渡しで排出が途切れない
            for (var k = 6; k <= 8; k++)
            {
                r.Tick(k * F + 0.006, out _, out var dy);
                Assert.Greater(dy, 0, $"欠落中フレーム {k} でも確立済み速度で排出が続く");
            }
        }

        [Test]
        public void Predictive_MomentumDrought_CoalescedSpikeDoesNotJump()
        {
            var r = new ScrollResampler { Predictive = true };
            var total = 0;
            var maxDy = 0;
            // 実運用同様、イベントと Tick を毎フレーム進める (初回キャッチアップを避ける)
            for (var k = 0; k <= 8; k++)
            {
                if (k <= 5) r.AddEvent(Ev(k * F, 10f));
                r.Tick(k * F + 0.006, out _, out var dy);
                total += dy;
                if (k >= 6 && dy > maxDy) maxDy = dy; // 欠落区間以降のみ評価
            }
            // 欠落明け: 溜め分 40px がコアレッシングされた 1 イベントで届く (実録画のパターン)
            r.AddEvent(Ev(9 * F, 40f));
            r.Tick(9 * F + 0.006, out _, out var dSpike);
            total += dSpike;
            if (dSpike > maxDy) maxDy = dSpike;
            // 橋渡しで先払いした分が差し引かれ、スパイクは 1 フレームに集中しない
            // (橋渡し無しの旧実装ではこのフレームに ~38px が出る)
            Assert.LessOrEqual(maxDy, 16, $"欠落明けの溜め分が 1 フレームに集中しない (max={maxDy})");
            // 終端で総量保存 (no-backtrack による先行分の切り捨ては ±2px 許容)
            r.AddEvent(Ev(10 * F, 0f, ScrollPhase.MomentumEnded));
            r.Tick(10 * F + 0.006, out _, out var dEnd);
            total += dEnd;
            Assert.That(total, Is.InRange(98, 102), "橋渡しを挟んでも総移動量が保存される");
            Assert.IsFalse(r.IsActive, "MomentumEnded の即時停止は橋渡しより優先");
        }

        [Test]
        public void Predictive_FingerDownDrought_NotBridged()
        {
            var r = new ScrollResampler { Predictive = true };
            // 指が接触したままの欠落 = ユーザーが指を止めた可能性がある。橋渡しすると
            // 「幽霊スクロール」になるため従来どおり停止する (Chromium と同じ判断)。
            for (var k = 0; k <= 5; k++) r.AddEvent(Ev(k * F, 10f, ScrollPhase.GestureChanged));
            r.Tick(5 * F + 0.006, out _, out _);
            var sawZero = false;
            for (var k = 6; k <= 8; k++)
            {
                r.Tick(k * F + 0.006, out _, out var dy);
                if (dy == 0) sawZero = true;
            }
            Assert.IsTrue(sawZero, "指接触中の欠落は外挿上限で止まる (幽霊スクロール防止)");
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
