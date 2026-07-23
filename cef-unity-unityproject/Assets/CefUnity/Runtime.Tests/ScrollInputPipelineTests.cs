using System.Collections.Generic;
using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="ScrollInputPipeline" /> の単体テスト。ルーティング規則
    ///     (precise→リサンプラ / 非 precise→スムーザ×WheelPixelsPerStep)、
    ///     resolutionScale 適用、overBrowser ゲート、Reset、フォールバック経路を検証する。
    ///     リサンプラ/スムーザ自体のアルゴリズムは各単体テストが担う。
    /// </summary>
    public class ScrollInputPipelineTests
    {
        private const double E = 1.0 / 120.0; // 120Hz イベント間隔
        private const float F = 1.0f / 60.0f; // 60fps フレーム間隔

        /// <summary>テスト用の合成イベントソース。Poll でキュー内容を排出する。</summary>
        private sealed class FakeSource : IScrollEventSource
        {
            private readonly Queue<ScrollInputEvent> _queue = new();
            public double Now { get; set; }

            public bool Start() => true;

            public int Poll(ScrollInputEvent[] buffer)
            {
                var n = 0;
                while (n < buffer.Length && _queue.Count > 0) buffer[n++] = _queue.Dequeue();
                return n;
            }

            public void Dispose() { }

            public void Enqueue(double t, float dy, bool precise,
                ScrollPhase phase = ScrollPhase.MomentumChanged)
                => _queue.Enqueue(new ScrollInputEvent
                    { Timestamp = t, DyPx = dy, Precise = precise, Phase = phase });
        }

        /// <summary>入力途絶までリサンプラ排出を合計する (終端スナップ含む)。</summary>
        private static int DrainResampler(
            ScrollInputPipeline p, FakeSource src, double start, int frames = 60, float scale = 1f)
        {
            var total = 0;
            for (var k = 0; k < frames; k++)
            {
                src.Now = start + k * F;
                p.Drain(true, scale);
                p.TickResampler(out _, out var dy);
                total += dy;
            }
            return total;
        }

        /// <summary>スムーザが非アクティブになるまで排出を合計する。</summary>
        private static int DrainSmoother(ScrollInputPipeline p)
        {
            var total = 0;
            for (var k = 0; k < 600 && p.TickSmoother(F, out _, out var dy); k++) total += dy;
            return total;
        }

        // ---- native ソースなし: TickResampler は false、フォールバック蓄積は動く ----

        [Test]
        public void WithoutSource_TickResamplerReturnsFalse()
        {
            var p = new ScrollInputPipeline();
            Assert.IsFalse(p.HasNativeSource);
            Assert.IsFalse(p.TickResampler(out var dx, out var dy));
            Assert.AreEqual(0, dx);
            Assert.AreEqual(0, dy);
        }

        [Test]
        public void AddWheelSteps_DrainsStepsTimesPixelsPerStep()
        {
            var p = new ScrollInputPipeline();
            p.AddWheelSteps(0f, 2f, 1f); // 2 ステップ = 120px
            Assert.AreEqual(120, DrainSmoother(p), "総量保存: 2×60px が全て排出される");
        }

        [Test]
        public void AddWheelSteps_AppliesResolutionScale()
        {
            var p = new ScrollInputPipeline();
            p.AddWheelSteps(0f, 1f, 2f); // 1 ステップ × scale2 = 120px
            Assert.AreEqual(120, DrainSmoother(p));
        }

        // ---- ルーティング: precise はリサンプラ、非 precise はスムーザ ----

        [Test]
        public void PreciseEvents_RouteToResampler()
        {
            var p = new ScrollInputPipeline();
            var src = new FakeSource();
            p.AttachSource(src);
            Assert.IsTrue(p.HasNativeSource);

            // 実運用同様、ジェスチャは MomentumEnded で閉じる (終端なしで途切れる列は
            // 外挿上限分のオーバーシュートが仕様上残るため、総量比較には使えない)。
            for (var i = 0; i < 6; i++) src.Enqueue(i * E, 10f, precise: true);
            src.Enqueue(6 * E, 0f, precise: true, ScrollPhase.MomentumEnded);
            var total = DrainResampler(p, src, 0.06);
            Assert.AreEqual(60, total, "precise 6 イベント ×10px が終端スナップで全量排出");
            Assert.IsFalse(p.TickSmoother(F, out _, out _), "スムーザ側には入らない");
        }

        [Test]
        public void NotchEvents_RouteToSmootherWithPixelsPerStep()
        {
            var p = new ScrollInputPipeline();
            var src = new FakeSource();
            p.AttachSource(src);

            src.Enqueue(0.0, 1f, precise: false); // 1 ノッチ = 60px
            src.Now = 0.01;
            p.Drain(true, 1f);
            p.TickResampler(out _, out var rdy);
            Assert.AreEqual(0, rdy, "リサンプラ側には入らない");
            Assert.AreEqual(60, DrainSmoother(p));
        }

        [Test]
        public void PreciseEvents_ScaledByResolutionScale()
        {
            var p = new ScrollInputPipeline();
            var src = new FakeSource();
            p.AttachSource(src);

            // 終端で閉じた列 (PreciseEvents_RouteToResampler と同じ理由)。
            for (var i = 0; i < 6; i++) src.Enqueue(i * E, 10f, precise: true);
            src.Enqueue(6 * E, 0f, precise: true, ScrollPhase.MomentumEnded);
            var total = DrainResampler(p, src, 0.06, scale: 2f); // scale=2 → 各イベント 20px
            Assert.AreEqual(120, total, "scale=2 で総量も 2 倍");
        }

        // ---- overBrowser=false のイベントは転送されない ----

        [Test]
        public void EventsOutsideBrowser_AreDropped()
        {
            var p = new ScrollInputPipeline();
            var src = new FakeSource();
            p.AttachSource(src);

            for (var i = 0; i < 6; i++) src.Enqueue(i * E, 10f, precise: true);
            src.Now = 0.02;
            p.Drain(false, 1f); // カーソルがブラウザ外
            var total = DrainResampler(p, src, 0.02 + F);
            Assert.AreEqual(0, total);
            Assert.IsFalse(p.TickSmoother(F, out _, out _));
        }

        // ---- Reset は両経路の残距離/履歴を消す ----

        [Test]
        public void Reset_ClearsBothPaths()
        {
            var p = new ScrollInputPipeline();
            var src = new FakeSource();
            p.AttachSource(src);

            src.Enqueue(0.0, 100f, precise: true);
            src.Enqueue(0.0, 3f, precise: false);
            src.Now = 0.005;
            p.Drain(true, 1f);
            p.Reset();

            var total = DrainResampler(p, src, 0.01);
            Assert.AreEqual(0, total, "リサンプラ履歴が消えている");
            Assert.IsFalse(p.TickSmoother(F, out _, out _), "スムーザ残距離が消えている");
        }

        // ---- Predictive プロパティがリサンプラへ届く (既定 true) ----

        [Test]
        public void Predictive_DefaultsTrue_AndIsSettable()
        {
            var p = new ScrollInputPipeline();
            Assert.IsTrue(p.Predictive, "予測モードが既定 (2026-07-22 採用)");
            p.Predictive = false;
            Assert.IsFalse(p.Predictive);
        }
    }
}
