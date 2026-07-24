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
        private const double EventInterval = 1.0 / 120.0; // 120Hz イベント間隔
        private const double FrameInterval = 1.0 / 60.0;  // 60fps フレーム間隔

        private static ScrollInputEvent MakeEvent(double timestamp, float deltaY, ScrollPhase phase = ScrollPhase.MomentumChanged)
            => new ScrollInputEvent { Timestamp = timestamp, DeltaYPixels = deltaY, Precise = true, Phase = phase };

        // ---- イベント 1 点では補間せず即時排出 (低遅延スタート) ----

        [Test]
        public void SingleEvent_EmitsImmediately()
        {
            var resampler = new ScrollResampler();
            resampler.AddEvent(MakeEvent(0.0, 30f));
            resampler.Tick(0.006, out _, out var deltaY);
            Assert.AreEqual(30, deltaY);
        }

        // ---- 均一 120Hz ストリーム → 追いつき後は毎フレームちょうど均一排出 ----

        [Test]
        public void SteadyStream_UniformPerFrameOutput()
        {
            var resampler = new ScrollResampler();
            var emitted = new System.Collections.Generic.List<int>();
            var eventIndex = 0;
            for (var frameIndex = 0; frameIndex < 30; frameIndex++)
            {
                var now = 0.02 + frameIndex * FrameInterval;
                while (eventIndex * EventInterval <= now)
                {
                    resampler.AddEvent(MakeEvent(eventIndex * EventInterval, 5f));
                    eventIndex++;
                }
                resampler.Tick(now, out _, out var deltaY);
                emitted.Add(deltaY);
            }
            // 600px/s の均一入力 → 初回フレームの追いつき以降は毎フレーム 10px ちょうど
            for (var frameIndex = 1; frameIndex < emitted.Count; frameIndex++)
                Assert.AreEqual(10, emitted[frameIndex], $"frame {frameIndex}");
        }

        // ---- イベントタイミングに ±3ms のジッター → 排出は準均一のまま ----

        [Test]
        public void JitteredTimestamps_OutputStaysNearUniform()
        {
            var resampler = new ScrollResampler();
            double[] jitter = { 0, +0.003, -0.002, +0.001, -0.003, +0.002 };
            var emitted = new System.Collections.Generic.List<int>();
            var eventIndex = 0;
            for (var frameIndex = 0; frameIndex < 24; frameIndex++)
            {
                var now = 0.02 + frameIndex * FrameInterval;
                while (true)
                {
                    var timestamp = eventIndex * EventInterval + jitter[eventIndex % jitter.Length];
                    if (timestamp > now) break;
                    resampler.AddEvent(MakeEvent(timestamp, 5f));
                    eventIndex++;
                }
                resampler.Tick(now, out _, out var deltaY);
                emitted.Add(deltaY);
            }
            for (var frameIndex = 2; frameIndex < emitted.Count; frameIndex++)
                Assert.That(emitted[frameIndex], Is.InRange(5, 15), $"frame {frameIndex}");
        }

        // ---- momentum 終端: 残差を即時排出して停止 (浮遊しない) ----

        [Test]
        public void MomentumEnded_FlushesResidualThenStops()
        {
            var resampler = new ScrollResampler();
            resampler.AddEvent(MakeEvent(0.000, 10f));
            resampler.AddEvent(MakeEvent(EventInterval, 10f));
            resampler.Tick(EventInterval + 0.005, out _, out var deltaY1);
            resampler.AddEvent(MakeEvent(2 * EventInterval, 0f, ScrollPhase.MomentumEnded));
            resampler.Tick(2 * EventInterval + 0.005, out _, out var deltaY2);
            Assert.AreEqual(20, deltaY1 + deltaY2, "終端までの総量が排出される");
            Assert.IsFalse(resampler.IsActive);
            resampler.Tick(2 * EventInterval + 0.005 + FrameInterval, out _, out var deltaY3);
            Assert.AreEqual(0, deltaY3, "終端後は排出なし");
        }

        // ---- 終端イベント取り逃し → 100ms グレースで残差排出 ----

        [Test]
        public void GraceTimeout_FlushesResidual()
        {
            var resampler = new ScrollResampler();
            resampler.AddEvent(MakeEvent(0.000, 10f));
            resampler.AddEvent(MakeEvent(EventInterval, 10f));
            resampler.Tick(0.005 + EventInterval * 0.5, out _, out var deltaY1); // sample=EventInterval/2 → 補間で 15 排出
            resampler.Tick(EventInterval + 0.150, out _, out var deltaY2);        // グレース超過 → 残差 5
            Assert.AreEqual(20, deltaY1 + deltaY2);
            Assert.IsFalse(resampler.IsActive);
        }

        // ---- 外挿は 8ms で頭打ち、終端フラッシュで巻き戻さない ----

        [Test]
        public void Extrapolation_CappedAndNoBackwardFlush()
        {
            var resampler = new ScrollResampler();
            resampler.AddEvent(MakeEvent(0.000, 10f));
            resampler.AddEvent(MakeEvent(EventInterval, 10f)); // 速度 1200px/s
            resampler.Tick(EventInterval + 0.005 + 0.050, out _, out var deltaY1);
            // 外挿は cap=8ms まで: 20 + 1200*0.008 = 29.6 → 30
            Assert.AreEqual(30, deltaY1);
            resampler.Tick(EventInterval + 0.200, out _, out var deltaY2);
            // サンプルは最終位置 20 を追い越している → 巻き戻さない
            Assert.AreEqual(0, deltaY2);
            Assert.IsFalse(resampler.IsActive);
        }

        // ---- 方向反転でも正味総量が保存される ----

        [Test]
        public void Reversal_NetTotalConserved()
        {
            var resampler = new ScrollResampler();
            var timestamp = 0.0;
            for (var eventIndex = 0; eventIndex < 6; eventIndex++) { resampler.AddEvent(MakeEvent(timestamp, +10f)); timestamp += EventInterval; }
            for (var eventIndex = 0; eventIndex < 6; eventIndex++) { resampler.AddEvent(MakeEvent(timestamp, -20f)); timestamp += EventInterval; }
            resampler.AddEvent(MakeEvent(timestamp, 0f, ScrollPhase.MomentumEnded));
            var total = 0;
            for (var frameIndex = 0; frameIndex < 20; frameIndex++)
            {
                resampler.Tick(0.02 + frameIndex * FrameInterval, out _, out var deltaY);
                total += deltaY;
            }
            Assert.AreEqual(-60, total, "正味 +60-120 = -60");
            Assert.IsFalse(resampler.IsActive);
        }

        // ---- 小数 delta の端数繰り越しで総量保存 ----

        [Test]
        public void FractionCarry_ConservesFractionalDeltas()
        {
            var resampler = new ScrollResampler();
            var timestamp = 0.0;
            for (var eventIndex = 0; eventIndex < 10; eventIndex++) { resampler.AddEvent(MakeEvent(timestamp, 1.5f)); timestamp += EventInterval; }
            resampler.AddEvent(MakeEvent(timestamp, 0f, ScrollPhase.MomentumEnded));
            var total = 0;
            for (var frameIndex = 0; frameIndex < 10; frameIndex++) { resampler.Tick(0.02 + frameIndex * FrameInterval, out _, out var deltaY); total += deltaY; }
            Assert.AreEqual(15, total);
        }

        // ---- momentum 終端の Tick を挟まず新ジェスチャ開始 → 残差は引き継がれる ----

        [Test]
        public void NewGestureAfterMomentumEnd_ContinuesCleanly()
        {
            var resampler = new ScrollResampler();
            resampler.AddEvent(MakeEvent(0.0, 10f));
            resampler.AddEvent(MakeEvent(EventInterval, 10f));
            resampler.AddEvent(MakeEvent(2 * EventInterval, 0f, ScrollPhase.MomentumEnded));
            resampler.AddEvent(MakeEvent(3 * EventInterval, 7f, ScrollPhase.GestureBegan));
            var total = 0;
            for (var frameIndex = 0; frameIndex < 10; frameIndex++) { resampler.Tick(0.03 + frameIndex * FrameInterval, out _, out var deltaY); total += deltaY; }
            resampler.AddEvent(MakeEvent(0.2, 0f, ScrollPhase.MomentumEnded));
            resampler.Tick(0.25, out _, out var lastDeltaY);
            Assert.AreEqual(27, total + lastDeltaY, "旧 20 + 新 7 が全て排出される");
        }

        // ---- 60Hz イベント (フレームと同率) でもビートジッターが出ない (適応オフセット+履歴4点) ----

        [Test]
        public void SteadyStream60Hz_NoBeatJitter()
        {
            var resampler = new ScrollResampler();
            var emitted = new System.Collections.Generic.List<int>();
            var eventIndex = 0;
            for (var frameIndex = 0; frameIndex < 40; frameIndex++)
            {
                var now = 0.05 + frameIndex * FrameInterval;
                while (eventIndex * FrameInterval <= now) { resampler.AddEvent(MakeEvent(eventIndex * FrameInterval, 10f)); eventIndex++; }
                resampler.Tick(now, out _, out var deltaY);
                emitted.Add(deltaY);
            }
            // 適応オフセット収束中も偏差は丸め ±1px に収まる (欠陥時は ±2〜4px のビートが交互に出る)
            for (var frameIndex = 1; frameIndex < emitted.Count; frameIndex++)
                Assert.That(emitted[frameIndex], Is.InRange(9, 11), $"frame {frameIndex}");
        }

        // ---- 予測モード: 60Hz 定常で均一かつ追従遅れが ~5ms 相当に縮む ----

        [Test]
        public void Predictive_SteadyStream60Hz_UniformAndLowLatency()
        {
            var resampler = new ScrollResampler { Predictive = true };
            var emitted = new System.Collections.Generic.List<int>();
            var total = 0;
            var eventIndex = 0;
            var lastNow = 0.0;
            for (var frameIndex = 0; frameIndex < 40; frameIndex++)
            {
                var now = 0.05 + frameIndex * FrameInterval;
                lastNow = now;
                while (eventIndex * FrameInterval <= now) { resampler.AddEvent(MakeEvent(eventIndex * FrameInterval, 10f)); eventIndex++; }
                resampler.Tick(now, out _, out var deltaY);
                emitted.Add(deltaY);
                total += deltaY;
            }
            for (var frameIndex = 1; frameIndex < emitted.Count; frameIndex++)
                Assert.That(emitted[frameIndex], Is.InRange(9, 11), $"frame {frameIndex}");
            // P(t) = 600t + 10 の直線。予測モードの追従遅れは ~5ms (3px)。
            // 補間モード (遅れ ~21ms ≈ 12.5px) では下限を割る → モード差を検証。
            var positionNow = 600.0 * lastNow + 10.0;
            Assert.GreaterOrEqual(total, positionNow - 6.0, "追従遅れが ~5ms 相当 (予測が効いている)");
        }

        // ---- 予測モード: 急停止でも巻き戻し (負の排出) が出ない ----

        [Test]
        public void Predictive_AbruptStop_NoBacktrack()
        {
            var resampler = new ScrollResampler { Predictive = true };
            var emitted = new System.Collections.Generic.List<int>();
            var total = 0;
            // 600px/s で 4 イベント → 以後 delta 0 (急停止)
            for (var frameIndex = 0; frameIndex < 10; frameIndex++)
            {
                var now = frameIndex * FrameInterval + 0.006;
                if (frameIndex < 4) resampler.AddEvent(MakeEvent(frameIndex * FrameInterval, 10f));
                else if (frameIndex < 7) resampler.AddEvent(MakeEvent(frameIndex * FrameInterval, 0f));
                resampler.Tick(now, out _, out var deltaY);
                emitted.Add(deltaY);
                total += deltaY;
            }
            foreach (var deltaY in emitted)
                Assert.GreaterOrEqual(deltaY, 0, "巻き戻し (負の排出) は出ない");
            // 入力合計 40px。外挿オーバーシュートの保持分 (+1px 程度) までは許容
            Assert.That(total, Is.InRange(40, 42));
        }

        // ---- 予測モード: 急停止のオーバーシュート残差が次ジェスチャ開始時に飛びとして出ない ----

        [Test]
        public void Predictive_OvershootResidual_NotEmittedOnNextGesture()
        {
            var resampler = new ScrollResampler { Predictive = true };
            // 1800px/s で 4 イベント → 外挿がサンプルを先行させる
            for (var eventIndex = 0; eventIndex < 4; eventIndex++) resampler.AddEvent(MakeEvent(eventIndex * FrameInterval, 30f));
            resampler.Tick(3 * FrameInterval + 0.012, out _, out _);
            // 急停止 (delta=0 の終端イベント → 直近セグメントの傾きは 0)
            resampler.AddEvent(MakeEvent(4 * FrameInterval, 0f, ScrollPhase.MomentumEnded));
            resampler.Tick(4 * FrameInterval + 0.006, out _, out var deltaY2);
            Assert.GreaterOrEqual(deltaY2, 0, "終端フラッシュで負の排出をしない");
            Assert.IsFalse(resampler.IsActive);
            // 新ジェスチャ (同方向 5px): 滞留残差による飛びが出ない
            resampler.AddEvent(MakeEvent(0.2, 5f, ScrollPhase.GestureBegan));
            resampler.Tick(0.2 + 0.006, out _, out var deltaY3);
            Assert.That(deltaY3, Is.InRange(0, 6), "次ジェスチャ開始時に位置が飛ばない");
        }

        // ---- 予測モード: phase 遷移の近接イベント (0.2ms 差) で外挿傾きが発散しない ----

        [Test]
        public void Predictive_NearSimultaneousPhaseTransition_NoSpike()
        {
            var resampler = new ScrollResampler { Predictive = true };
            var timestamp = 0.0;
            // 低速ジェスチャ (60Hz, -2px)
            for (var eventIndex = 0; eventIndex < 6; eventIndex++) { resampler.AddEvent(MakeEvent(timestamp, -2f, ScrollPhase.GestureChanged)); timestamp += FrameInterval; }
            // 遷移: GestureEnded (dy=0) の 0.2ms 後に MomentumBegan (-15px) — 実録画のパターン
            resampler.AddEvent(MakeEvent(timestamp, 0f, ScrollPhase.GestureEnded));
            resampler.AddEvent(MakeEvent(timestamp + 0.0002, -15f, ScrollPhase.MomentumBegan));
            // 直後の Tick 群でスパイクが出ない (修正前は数百〜数千 px)
            var worst = 0;
            for (var frameIndex = 0; frameIndex < 6; frameIndex++)
            {
                resampler.Tick(timestamp + 0.004 + frameIndex * FrameInterval, out _, out var deltaY);
                if (System.Math.Abs(deltaY) > System.Math.Abs(worst)) worst = deltaY;
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
            var resampler = new ScrollResampler { Predictive = true };
            // 600px/s の momentum ストリームで速度確立 (MakeEvent の既定 phase = MomentumChanged)
            for (var frameIndex = 0; frameIndex <= 5; frameIndex++) resampler.AddEvent(MakeEvent(frameIndex * FrameInterval, 10f));
            resampler.Tick(5 * FrameInterval + 0.006, out _, out _);
            // 66ms 欠落: イベント無しのまま 3 フレーム Tick — 橋渡しで排出が途切れない
            for (var frameIndex = 6; frameIndex <= 8; frameIndex++)
            {
                resampler.Tick(frameIndex * FrameInterval + 0.006, out _, out var deltaY);
                Assert.Greater(deltaY, 0, $"欠落中フレーム {frameIndex} でも確立済み速度で排出が続く");
            }
        }

        [Test]
        public void Predictive_MomentumDrought_CoalescedSpikeDoesNotJump()
        {
            var resampler = new ScrollResampler { Predictive = true };
            var total = 0;
            var maxDeltaY = 0;
            // 実運用同様、イベントと Tick を毎フレーム進める (初回キャッチアップを避ける)
            for (var frameIndex = 0; frameIndex <= 8; frameIndex++)
            {
                if (frameIndex <= 5) resampler.AddEvent(MakeEvent(frameIndex * FrameInterval, 10f));
                resampler.Tick(frameIndex * FrameInterval + 0.006, out _, out var deltaY);
                total += deltaY;
                if (frameIndex >= 6 && deltaY > maxDeltaY) maxDeltaY = deltaY; // 欠落区間以降のみ評価
            }
            // 欠落明け: 溜め分 40px がコアレッシングされた 1 イベントで届く (実録画のパターン)
            resampler.AddEvent(MakeEvent(9 * FrameInterval, 40f));
            resampler.Tick(9 * FrameInterval + 0.006, out _, out var spikeDeltaY);
            total += spikeDeltaY;
            if (spikeDeltaY > maxDeltaY) maxDeltaY = spikeDeltaY;
            // 橋渡しで先払いした分が差し引かれ、スパイクは 1 フレームに集中しない
            // (橋渡し無しの旧実装ではこのフレームに ~38px が出る)
            Assert.LessOrEqual(maxDeltaY, 16, $"欠落明けの溜め分が 1 フレームに集中しない (max={maxDeltaY})");
            // 終端で総量保存 (no-backtrack による先行分の切り捨ては ±2px 許容)
            resampler.AddEvent(MakeEvent(10 * FrameInterval, 0f, ScrollPhase.MomentumEnded));
            resampler.Tick(10 * FrameInterval + 0.006, out _, out var endDeltaY);
            total += endDeltaY;
            Assert.That(total, Is.InRange(98, 102), "橋渡しを挟んでも総移動量が保存される");
            Assert.IsFalse(resampler.IsActive, "MomentumEnded の即時停止は橋渡しより優先");
        }

        [Test]
        public void Predictive_FingerDownDrought_NotBridged()
        {
            var resampler = new ScrollResampler { Predictive = true };
            // 指が接触したままの欠落 = ユーザーが指を止めた可能性がある。橋渡しすると
            // 「幽霊スクロール」になるため従来どおり停止する (Chromium と同じ判断)。
            for (var frameIndex = 0; frameIndex <= 5; frameIndex++) resampler.AddEvent(MakeEvent(frameIndex * FrameInterval, 10f, ScrollPhase.GestureChanged));
            resampler.Tick(5 * FrameInterval + 0.006, out _, out _);
            var sawZero = false;
            for (var frameIndex = 6; frameIndex <= 8; frameIndex++)
            {
                resampler.Tick(frameIndex * FrameInterval + 0.006, out _, out var deltaY);
                if (deltaY == 0) sawZero = true;
            }
            Assert.IsTrue(sawZero, "指接触中の欠落は外挿上限で止まる (幽霊スクロール防止)");
        }

        // ---- 予測モード: ジェスチャ開始の過渡でオーバーシュートしない ----
        // 実録画 (2026-07-23 build): ストローク開始の加速中、履歴が少ない状態での外挿が
        // 入力 -4,-30,-34 に対し排出 -4,-12,-58 の「ラグ→倍返し」を生んでいた。
        // 低速スクロール = 短いストロークの繰り返しなので毎回この過渡が出てガタつく。

        [Test]
        public void Predictive_GestureStartAcceleration_NoOvershoot()
        {
            var resampler = new ScrollResampler { Predictive = true };
            // 実録画のストローク開始パターンを再現 (17ms tick、加速する負方向入力)。
            // ガタつきの知覚対応量は排出の「フレーム間の跳び」— 入力自身の跳び
            // (加速) より大きく跳ねないことを検証する (旧実装: 排出 -12→-58 =
            // 跳び 46 の「1 拍遅れ → 一括放出」が出る。入力側の跳びは最大 26)。
            var inputSums = new System.Collections.Generic.List<int>();
            var outputs = new System.Collections.Generic.List<int>();
            void TickWith(double now, int inputSum)
            {
                resampler.Tick(now, out _, out var output);
                outputs.Add(System.Math.Abs(output));
                inputSums.Add(System.Math.Abs(inputSum));
            }

            resampler.AddEvent(MakeEvent(0.000, -4f, ScrollPhase.GestureBegan));
            TickWith(0.004, -4);
            resampler.AddEvent(MakeEvent(0.014, -7f, ScrollPhase.GestureChanged));
            resampler.AddEvent(MakeEvent(0.0225, -23f, ScrollPhase.GestureChanged));
            TickWith(0.0207, -30);
            resampler.AddEvent(MakeEvent(0.031, -34f, ScrollPhase.GestureChanged));
            TickWith(0.0374, -34);
            resampler.AddEvent(MakeEvent(0.048, -36f, ScrollPhase.GestureChanged));
            TickWith(0.0541, -36);

            int MaxJump(System.Collections.Generic.List<int> values)
            {
                var maxValue = 0;
                for (var index = 1; index < values.Count; index++)
                {
                    var jump = System.Math.Abs(values[index] - values[index - 1]);
                    if (jump > maxValue) maxValue = jump;
                }
                return maxValue;
            }

            var inputJump = MaxJump(inputSums);
            var outputJump = MaxJump(outputs);
            var bound = (int)(inputJump * ScrollResampler.CatchUpHeadroom) + 2;
            Assert.LessOrEqual(outputJump, bound,
                $"排出の跳びが入力の跳びを大きく超えない (outputJump={outputJump} inputJump={inputJump} bound={bound})");
        }

        // ---- Reset で全状態破棄 ----

        [Test]
        public void Reset_ClearsState()
        {
            var resampler = new ScrollResampler();
            resampler.AddEvent(MakeEvent(0.0, 100f));
            resampler.Reset();
            Assert.IsFalse(resampler.IsActive);
            resampler.Tick(0.05, out _, out var deltaY);
            Assert.AreEqual(0, deltaY);
        }
    }
}
