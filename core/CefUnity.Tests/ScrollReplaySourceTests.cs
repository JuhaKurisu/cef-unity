using System;
using System.IO;
using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ScrollReplaySourceTests
    {
        private static readonly string[] SampleLines =
        {
            "S,1",
            "E,100.000,0,-10,2,1,1",
            "E,100.016,0,-12,2,1,1",
            "E,100.900,0,-99,2,1,0",   // over=0 → 除外
            "T,100.016,0,-10,1",        // T 行は無視
            "E,101.000,0,-3,5,1,1",
        };

        [Test]
        public void Poll_AdvancesWithClock_EmitsEventsInRecordedOrder()
        {
            var clockSeconds = 50.0;
            var source = new ScrollReplaySource(SampleLines, () => clockSeconds);
            Assert.That(source.Start(), Is.True);
            Assert.That(source.TotalEvents, Is.EqualTo(3));

            var buffer = new ScrollInputEvent[16];
            // 開始直後 (録画時刻 100.000 相当): 最初のイベントのみ
            Assert.That(source.Poll(buffer), Is.EqualTo(1));
            Assert.That(buffer[0].DeltaYPixels, Is.EqualTo(-10f));
            Assert.That(buffer[0].Timestamp, Is.EqualTo(100.000).Within(1e-9));

            clockSeconds = 50.020; // 録画時刻 100.020 相当
            Assert.That(source.Poll(buffer), Is.EqualTo(1));
            Assert.That(buffer[0].DeltaYPixels, Is.EqualTo(-12f));
            Assert.That(source.Finished, Is.False);

            clockSeconds = 51.100; // 録画時刻 101.100 相当 (over=0 は飛ばして最後の 1 件)
            Assert.That(source.Poll(buffer), Is.EqualTo(1));
            Assert.That(buffer[0].DeltaYPixels, Is.EqualTo(-3f));
            Assert.That(source.Finished, Is.True);
        }

        [Test]
        public void Now_MapsToRecordedTimeline()
        {
            var clockSeconds = 50.0;
            var source = new ScrollReplaySource(SampleLines, () => clockSeconds);
            source.Start();
            Assert.That(source.Now, Is.EqualTo(100.000).Within(1e-9));
            clockSeconds = 50.5;
            Assert.That(source.Now, Is.EqualTo(100.500).Within(1e-9));
        }

        [Test]
        public void ScaleRow_MultipliesDeltas()
        {
            var clockSeconds = 0.0;
            var source = new ScrollReplaySource(new[] { "S,2", "E,10.0,1,-10,2,1,1" }, () => clockSeconds);
            source.Start();
            var buffer = new ScrollInputEvent[4];
            source.Poll(buffer);
            Assert.That(buffer[0].DeltaXPixels, Is.EqualTo(2f));
            Assert.That(buffer[0].DeltaYPixels, Is.EqualTo(-20f));
        }

        [Test]
        public void EmptyRecording_StartReturnsFalse()
        {
            var source = new ScrollReplaySource(new[] { "T,1.0,0,0,1" }, () => 0.0);
            Assert.That(source.Start(), Is.False);
        }

        [Test]
        public void ExistingFixture_LoadsAllForwardedEvents()
        {
            var lines = File.ReadAllLines(Path.Combine(TestContext.CurrentContext.TestDirectory,
                "fixtures", "cef_scroll_events_nozerowait.csv"));
            var source = new ScrollReplaySource(lines, () => 0.0);
            Assert.That(source.Start(), Is.True);
            Assert.That(source.TotalEvents, Is.GreaterThan(100));
        }
    }
}
