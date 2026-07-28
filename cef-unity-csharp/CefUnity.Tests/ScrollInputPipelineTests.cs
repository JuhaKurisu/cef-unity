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
        private const double EventInterval = 1.0 / 120.0; // 120Hz イベント間隔
        private const float FrameInterval = 1.0f / 60.0f; // 60fps フレーム間隔

        /// <summary>テスト用の合成イベントソース。Poll でキュー内容を排出する。</summary>
        private sealed class FakeSource : IScrollEventSource
        {
            private readonly Queue<ScrollInputEvent> _queue = new();
            public double Now { get; set; }

            public bool Start() => true;

            public int Poll(ScrollInputEvent[] buffer)
            {
                var count = 0;
                while (count < buffer.Length && _queue.Count > 0) buffer[count++] = _queue.Dequeue();
                return count;
            }

            public void Dispose() { }

            public void Enqueue(double timestamp, float deltaY, bool precise,
                ScrollPhase phase = ScrollPhase.MomentumChanged)
                => _queue.Enqueue(new ScrollInputEvent
                    { Timestamp = timestamp, DeltaYPixels = deltaY, Precise = precise, Phase = phase });
        }

        /// <summary>入力途絶までリサンプラ排出を合計する (終端スナップ含む)。</summary>
        private static int DrainResampler(
            ScrollInputPipeline pipeline, FakeSource source, double start, int frames = 60, float scale = 1f)
        {
            var total = 0;
            for (var frameIndex = 0; frameIndex < frames; frameIndex++)
            {
                source.Now = start + frameIndex * FrameInterval;
                pipeline.Drain(true, scale);
                pipeline.TickResampler(out _, out var deltaY);
                total += deltaY;
            }
            return total;
        }

        /// <summary>スムーザが非アクティブになるまで排出を合計する。</summary>
        private static int DrainSmoother(ScrollInputPipeline pipeline)
        {
            var total = 0;
            for (var frameIndex = 0; frameIndex < 600 && pipeline.TickSmoother(FrameInterval, out _, out var deltaY); frameIndex++) total += deltaY;
            return total;
        }

        // ---- native ソースなし: TickResampler は false、フォールバック蓄積は動く ----

        [Test]
        public void WithoutSource_TickResamplerReturnsFalse()
        {
            var pipeline = new ScrollInputPipeline();
            Assert.IsFalse(pipeline.HasNativeSource);
            Assert.IsFalse(pipeline.TickResampler(out var deltaX, out var deltaY));
            Assert.AreEqual(0, deltaX);
            Assert.AreEqual(0, deltaY);
        }

        [Test]
        public void AddWheelSteps_DrainsStepsTimesPixelsPerStep()
        {
            var pipeline = new ScrollInputPipeline();
            pipeline.AddWheelSteps(0f, 2f, 1f); // 2 ステップ = 120px
            Assert.AreEqual(120, DrainSmoother(pipeline), "総量保存: 2×60px が全て排出される");
        }

        [Test]
        public void AddWheelSteps_AppliesResolutionScale()
        {
            var pipeline = new ScrollInputPipeline();
            pipeline.AddWheelSteps(0f, 1f, 2f); // 1 ステップ × scale2 = 120px
            Assert.AreEqual(120, DrainSmoother(pipeline));
        }

        // ---- ルーティング: precise はリサンプラ、非 precise はスムーザ ----

        [Test]
        public void PreciseEvents_RouteToResampler()
        {
            var pipeline = new ScrollInputPipeline();
            var source = new FakeSource();
            pipeline.AttachSource(source);
            Assert.IsTrue(pipeline.HasNativeSource);

            // 実運用同様、ジェスチャは MomentumEnded で閉じる (終端なしで途切れる列は
            // 外挿上限分のオーバーシュートが仕様上残るため、総量比較には使えない)。
            for (var eventIndex = 0; eventIndex < 6; eventIndex++) source.Enqueue(eventIndex * EventInterval, 10f, precise: true);
            source.Enqueue(6 * EventInterval, 0f, precise: true, ScrollPhase.MomentumEnded);
            var total = DrainResampler(pipeline, source, 0.06);
            Assert.AreEqual(60, total, "precise 6 イベント ×10px が終端スナップで全量排出");
            Assert.IsFalse(pipeline.TickSmoother(FrameInterval, out _, out _), "スムーザ側には入らない");
        }

        [Test]
        public void NotchEvents_RouteToSmootherWithPixelsPerStep()
        {
            var pipeline = new ScrollInputPipeline();
            var source = new FakeSource();
            pipeline.AttachSource(source);

            source.Enqueue(0.0, 1f, precise: false); // 1 ノッチ = 60px
            source.Now = 0.01;
            pipeline.Drain(true, 1f);
            pipeline.TickResampler(out _, out var resamplerDeltaY);
            Assert.AreEqual(0, resamplerDeltaY, "リサンプラ側には入らない");
            Assert.AreEqual(60, DrainSmoother(pipeline));
        }

        [Test]
        public void PreciseEvents_ScaledByResolutionScale()
        {
            var pipeline = new ScrollInputPipeline();
            var source = new FakeSource();
            pipeline.AttachSource(source);

            // 終端で閉じた列 (PreciseEvents_RouteToResampler と同じ理由)。
            for (var eventIndex = 0; eventIndex < 6; eventIndex++) source.Enqueue(eventIndex * EventInterval, 10f, precise: true);
            source.Enqueue(6 * EventInterval, 0f, precise: true, ScrollPhase.MomentumEnded);
            var total = DrainResampler(pipeline, source, 0.06, scale: 2f); // scale=2 → 各イベント 20px
            Assert.AreEqual(120, total, "scale=2 で総量も 2 倍");
        }

        // ---- overBrowser=false のイベントは転送されない ----

        [Test]
        public void EventsOutsideBrowser_AreDropped()
        {
            var pipeline = new ScrollInputPipeline();
            var source = new FakeSource();
            pipeline.AttachSource(source);

            for (var eventIndex = 0; eventIndex < 6; eventIndex++) source.Enqueue(eventIndex * EventInterval, 10f, precise: true);
            source.Now = 0.02;
            pipeline.Drain(false, 1f); // カーソルがブラウザ外
            var total = DrainResampler(pipeline, source, 0.02 + FrameInterval);
            Assert.AreEqual(0, total);
            Assert.IsFalse(pipeline.TickSmoother(FrameInterval, out _, out _));
        }

        // ---- Reset は両経路の残距離/履歴を消す ----

        [Test]
        public void Reset_ClearsBothPaths()
        {
            var pipeline = new ScrollInputPipeline();
            var source = new FakeSource();
            pipeline.AttachSource(source);

            source.Enqueue(0.0, 100f, precise: true);
            source.Enqueue(0.0, 3f, precise: false);
            source.Now = 0.005;
            pipeline.Drain(true, 1f);
            pipeline.Reset();

            var total = DrainResampler(pipeline, source, 0.01);
            Assert.AreEqual(0, total, "リサンプラ履歴が消えている");
            Assert.IsFalse(pipeline.TickSmoother(FrameInterval, out _, out _), "スムーザ残距離が消えている");
        }

        // ---- Predictive プロパティがリサンプラへ届く (既定 true) ----

        [Test]
        public void Predictive_DefaultsTrue_AndIsSettable()
        {
            var pipeline = new ScrollInputPipeline();
            Assert.IsTrue(pipeline.Predictive, "予測モードが既定 (2026-07-22 採用)");
            pipeline.Predictive = false;
            Assert.IsFalse(pipeline.Predictive);
        }
    }
}
