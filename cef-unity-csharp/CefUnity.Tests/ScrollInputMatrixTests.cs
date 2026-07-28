using System;
using System.Collections.Generic;
using CefUnity.Runtime;
using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ScrollInputMatrixTests
    {
        /// <summary>時刻を手動で進められる fake ソース。</summary>
        private sealed class FakeScrollSource : IScrollEventSource
        {
            public readonly List<ScrollInputEvent> Pending = new();
            public double CurrentTime;
            public bool Start() => true;
            public double Now => CurrentTime;
            public int Poll(ScrollInputEvent[] buffer)
            {
                var count = Math.Min(Pending.Count, buffer.Length);
                for (var index = 0; index < count; index++) buffer[index] = Pending[index];
                Pending.RemoveRange(0, count);
                return count;
            }
            public void Dispose() { }
        }

        [Test]
        public void RawMode_WheelSteps_EmitOncePerFrame()
        {
            using var matrix = new ScrollInputMatrix();
            matrix.SetMode(ScrollMode.Raw);
            matrix.AddWheelSteps(0f, -1f);
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var primaryDeltaY, out _, out var secondaryDeltaY);
            Assert.That(primaryDeltaY, Is.EqualTo((int)(-1f * ScrollInputPipeline.WheelPixelsPerStep)));
            Assert.That(secondaryDeltaY, Is.EqualTo(0));
            // 消費済み: 次フレームは 0
            matrix.TickFrame(0.016f, overBrowser: true, out _, out primaryDeltaY, out _, out _);
            Assert.That(primaryDeltaY, Is.EqualTo(0));
        }

        [Test]
        public void SmootherMode_WheelSteps_GlideOverFrames()
        {
            using var matrix = new ScrollInputMatrix();
            matrix.SetMode(ScrollMode.Smoother);
            matrix.AddWheelSteps(0f, -1f);
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var firstDeltaY, out _, out _);
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var secondDeltaY, out _, out _);
            Assert.That(firstDeltaY, Is.Not.EqualTo(0));
            Assert.That(Math.Abs(firstDeltaY), Is.LessThan(60)); // 一括 60px でなく分散排出
            Assert.That(secondDeltaY, Is.Not.EqualTo(0));
        }

        [Test]
        public void ResamplerMode_PreciseEvents_FlowThroughResampler()
        {
            using var matrix = new ScrollInputMatrix();
            var source = new FakeScrollSource();
            matrix.AttachSource(source);
            matrix.SetMode(ScrollMode.Resampler);
            // 8ms 間隔の precise イベント 3 連 (60Hz NSEvent 相当)
            for (var index = 0; index < 3; index++)
                source.Pending.Add(new ScrollInputEvent
                {
                    Timestamp = index * 0.008, DeltaYPixels = -10f, Precise = true, Phase = ScrollPhase.GestureChanged,
                });
            source.CurrentTime = 0.030;
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var primaryDeltaY, out _, out _);
            source.CurrentTime = 0.046;
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var nextDeltaY, out _, out _);
            Assert.That(primaryDeltaY + nextDeltaY, Is.LessThan(0)); // 下方向の排出が発生
        }

        [Test]
        public void ResamplerMode_WindowWheelSteps_Ignored()
        {
            using var matrix = new ScrollInputMatrix();
            matrix.AttachSource(new FakeScrollSource());
            matrix.SetMode(ScrollMode.Resampler);
            matrix.AddWheelSteps(0f, -1f); // native と二重計上しない
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var primaryDeltaY, out _, out var secondaryDeltaY);
            Assert.That(primaryDeltaY, Is.EqualTo(0));
            Assert.That(secondaryDeltaY, Is.EqualTo(0));
        }

        [Test]
        public void SetMode_ClearsPendingState()
        {
            using var matrix = new ScrollInputMatrix();
            matrix.SetMode(ScrollMode.Raw);
            matrix.AddWheelSteps(0f, -5f);
            matrix.SetMode(ScrollMode.Smoother); // 切替で raw 蓄積を破棄
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var primaryDeltaY, out _, out _);
            Assert.That(primaryDeltaY, Is.EqualTo(0));
        }
    }
}
