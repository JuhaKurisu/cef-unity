using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Tests
{
    public class ScrollReplayRunnerTests
    {
        [Test]
        public void ValidCsv_ParsesEventsAndTicks()
        {
            // S=scale, E=timestamp,dx,dy,phase,precise[,over], T=now,liveDx,liveDy,mode
            var lines = new[]
            {
                "S,1",
                "E,0.000,0,-8,2,1",
                "E,0.016,0,-8,2,1",
                "T,0.016,0,-4,0",
                "T,0.032,0,-4,0",
            };
            var result = ScrollReplayRunner.Run(lines);
            Assert.That(result.Ok, Is.True, result.Error);
            Assert.That(result.Events, Is.EqualTo(2));
            Assert.That(result.Ticks, Is.EqualTo(2));
            Assert.That(result.OutputLines.Count, Is.EqualTo(2));
        }

        [Test]
        public void NoTickLines_Fails()
        {
            var result = ScrollReplayRunner.Run(new[] { "S,1", "E,0.0,0,-8,2,1" });
            Assert.That(result.Ok, Is.False);
        }
    }
}
