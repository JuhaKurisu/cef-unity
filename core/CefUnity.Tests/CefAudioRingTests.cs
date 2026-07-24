using System;
using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="CefAudioRing" /> の単体テスト。
    ///     CEF(producer)→ Unity 出力(consumer)のレート変換つきリングが
    ///     アンダーラン・オーバーフロー・不連続(クリック=ぶつ切り)を起こさず
    ///     滞留量を目標へ収束させることを検証する。
    /// </summary>
    public class CefAudioRingTests
    {
        private const int SrcRate = 48000;
        private const int OutRate = 44100;
        private const int Channels = 2;

        private static CefAudioRing MakeRing(float bufSec = 0.5f, float targetSec = 0.08f, double adjust = 0.01)
        {
            int cap = (int)Math.Ceiling(bufSec * SrcRate);
            int target = (int)Math.Ceiling(targetSec * SrcRate);
            return new CefAudioRing(cap, Channels, target, adjust);
        }

        // 440Hz サイン波を interleaved で frameCount フレーム生成する。phase は継続用に更新。
        private static float[] MakeSine(int frameCount, ref double phase, double freq = 440.0, float amp = 0.2f)
        {
            var buf = new float[frameCount * Channels];
            double dphi = 2.0 * Math.PI * freq / SrcRate;
            for (int f = 0; f < frameCount; f++)
            {
                float s = (float)(Math.Sin(phase) * amp);
                for (int c = 0; c < Channels; c++) buf[f * Channels + c] = s;
                phase += dphi;
            }

            return buf;
        }

        // ----- 基本正当性 -----

        [Test]
        public void Read_WithUnitStep_ReturnsWrittenSamplesInOrder()
        {
            // baseStep=1.0 (リサンプルなし), 1ch でランプを書くとそのまま順に出る (frac=0)。
            var ring = new CefAudioRing(1000, 1, targetFrames: 4, maxRateAdjust: 0.0);
            var ramp = new float[100];
            for (int i = 0; i < ramp.Length; i++) ramp[i] = i;
            ring.Write(ramp, 0, 100); // 目標(4)以上溜まっている → プライミング即完了

            var outBuf = new float[10];
            ring.Read(outBuf, 10, baseStep: 1.0);

            for (int i = 0; i < 10; i++)
                Assert.AreEqual((float)i, outBuf[i], 1e-4f, $"index {i}");
            Assert.AreEqual(0, ring.UnderrunFrames);
            Assert.AreEqual(0, ring.OverflowDropFrames);
        }

        [Test]
        public void Read_BeforeTargetReached_OutputsSilenceAndCountsUnderrun()
        {
            var ring = new CefAudioRing(1000, 1, targetFrames: 50, maxRateAdjust: 0.0);
            var few = new float[10];
            for (int i = 0; i < few.Length; i++) few[i] = 1f;
            ring.Write(few, 0, 10); // 目標 50 未満 → まだプライミングしない

            var outBuf = new float[8];
            ring.Read(outBuf, 8, baseStep: 1.0);

            foreach (var s in outBuf) Assert.AreEqual(0f, s, "プライミング前は無音であるべき");
            Assert.AreEqual(8, ring.UnderrunFrames);
        }

        [Test]
        public void Write_BeyondCapacity_DropsOldestAndCountsOverflow()
        {
            var ring = new CefAudioRing(100, 1, targetFrames: 10, maxRateAdjust: 0.0);
            var big = new float[500];
            for (int i = 0; i < big.Length; i++) big[i] = i;
            ring.Write(big, 0, 500); // 容量 100 を大きく超える

            Assert.Greater(ring.OverflowDropFrames, 0, "容量超過で破棄が記録されるべき");
            Assert.LessOrEqual(ring.OccupancyFrames, 100, "滞留量は容量以内に収まるべき");
        }

        // ----- 連続性 (ぶつ切り = クリック検出) -----

        [Test]
        public void SteadyState_MatchedClocks_NoUnderrunNoOverflow_ContinuousOutput()
        {
            RunStreamingScenario(
                produceFramesPerTick: 480, // 48000 * 10ms
                consumeFramesPerTick: 441, // 44100 * 10ms
                ticks: 500,
                out long underAfterPrime, out long overflow, out float maxDiscontinuity);

            Assert.AreEqual(0, underAfterPrime, "プライミング後のアンダーランは 0 であるべき (ぶつ切りなし)");
            Assert.AreEqual(0, overflow, "オーバーフロー破棄は 0 であるべき");
            // 440Hz/0.2amp の隣接サンプル差は最大 ~0.0125。クリック(チャンク破棄)なら ~0.4 跳ぶ。
            Assert.Less(maxDiscontinuity, 0.05f, "出力に不連続(クリック)があってはならない");
        }

        [Test]
        public void ProducerSlightlyFaster_SteeringAbsorbs_NoOverflow_NoUnderrun()
        {
            // producer が consumer よりわずかに速い (≈+0.4%)。steering(±1%)で吸収できるはず。
            RunStreamingScenario(
                produceFramesPerTick: 482,
                consumeFramesPerTick: 441,
                ticks: 800,
                out long underAfterPrime, out long overflow, out float maxDiscontinuity);

            Assert.AreEqual(0, overflow, "速い producer でも steering が消費を速めてオーバーフローを防ぐべき");
            Assert.AreEqual(0, underAfterPrime, "アンダーランは発生しないべき");
            Assert.Less(maxDiscontinuity, 0.05f, "出力は連続であるべき");
        }

        [Test]
        public void ProducerSlightlySlower_SteeringAbsorbs_NoUnderrun_NoOverflow()
        {
            // producer がわずかに遅い (≈-0.4%)。steering が消費を緩めてアンダーランを防ぐ。
            RunStreamingScenario(
                produceFramesPerTick: 478,
                consumeFramesPerTick: 441,
                ticks: 800,
                out long underAfterPrime, out long overflow, out float maxDiscontinuity);

            Assert.AreEqual(0, underAfterPrime, "遅い producer でも steering が消費を緩めてアンダーランを防ぐべき");
            Assert.AreEqual(0, overflow, "オーバーフローは発生しないべき");
            Assert.Less(maxDiscontinuity, 0.05f, "出力は連続であるべき");
        }

        [Test]
        public void SteadyState_OccupancyConvergesNearTarget()
        {
            // 定常運転後、滞留量が目標近傍(±25%)に収束していること。
            var ring = MakeRing();
            double baseStep = (double)SrcRate / OutRate;
            double phase = 0;
            int produced = 0, consumed = 0;
            var outBuf = new float[441 * Channels];

            for (int t = 0; t < 600; t++)
            {
                int prod = (int)Math.Round((t + 1) * 480.0) - produced;
                var sine = MakeSine(prod, ref phase);
                ring.Write(sine, 0, prod);
                produced += prod;

                int cons = (int)Math.Round((t + 1) * 441.0) - consumed;
                if (cons > outBuf.Length / Channels) cons = outBuf.Length / Channels;
                ring.Read(outBuf, cons, baseStep);
                consumed += cons;
            }

            double occ = ring.OccupancyFrames;
            float target = ring.TargetFrames;
            Assert.That(occ, Is.InRange(target * 0.5, target * 1.5),
                $"滞留量({occ:F0})は目標({target})近傍へ収束すべき");
        }

        // ----- 共通シナリオ実行器 -----

        // producer/consumer を tick 単位で交互に動かし、プライミング後のアンダーラン数・
        // オーバーフロー数・出力の最大不連続量(隣接サンプル差の最大)を返す。
        private void RunStreamingScenario(
            int produceFramesPerTick, int consumeFramesPerTick, int ticks,
            out long underAfterPrime, out long overflow, out float maxDiscontinuity)
        {
            var ring = MakeRing();
            double baseStep = (double)SrcRate / OutRate;
            double phase = 0;

            var outBuf = new float[(consumeFramesPerTick + 8) * Channels];
            maxDiscontinuity = 0f;
            long underAtPrimeWindow = -1;
            // プライミング完了とみなす tick (目標 80ms ≒ 8 tick + 余裕)。これ以降のアンダーランを評価。
            const int primeWindowTicks = 20;
            // 連続性は安定後 (priming 直後の開始トランジェントを除く) のみ評価。
            const int continuityFromTick = 25;

            float[] prevFrame = null;

            for (int t = 0; t < ticks; t++)
            {
                var sine = MakeSine(produceFramesPerTick, ref phase);
                ring.Write(sine, 0, produceFramesPerTick);

                ring.Read(outBuf, consumeFramesPerTick, baseStep);

                if (t == primeWindowTicks) underAtPrimeWindow = ring.UnderrunFrames;

                if (t >= continuityFromTick)
                {
                    for (int f = 0; f < consumeFramesPerTick; f++)
                    {
                        if (prevFrame != null)
                        {
                            for (int c = 0; c < Channels; c++)
                            {
                                float d = Math.Abs(outBuf[f * Channels + c] - prevFrame[c]);
                                if (d > maxDiscontinuity) maxDiscontinuity = d;
                            }
                        }
                        else
                        {
                            prevFrame = new float[Channels];
                        }

                        for (int c = 0; c < Channels; c++) prevFrame[c] = outBuf[f * Channels + c];
                    }
                }
            }

            overflow = ring.OverflowDropFrames;
            underAfterPrime = underAtPrimeWindow < 0 ? ring.UnderrunFrames : ring.UnderrunFrames - underAtPrimeWindow;
        }
    }
}
