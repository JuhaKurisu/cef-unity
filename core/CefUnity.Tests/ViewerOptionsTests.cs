using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ViewerOptionsTests
    {
        [Test]
        public void Parse_NoArguments_ReturnsDefaults()
        {
            var options = ViewerOptions.Parse(new string[0]);
            Assert.That(options, Is.Not.Null);
            Assert.That(options!.Url, Is.EqualTo("https://example.com"));
            Assert.That(options.Width, Is.EqualTo(1280));
            Assert.That(options.Height, Is.EqualTo(720));
            Assert.That(options.Mode, Is.EqualTo(ScrollMode.Resampler));
            Assert.That(options.Record, Is.False);
            Assert.That(options.ReplayPath, Is.Null);
            Assert.That(options.StatisticsPath, Is.Null);
            Assert.That(options.AnalyzePath, Is.Null);
        }

        [Test]
        public void Parse_AllArguments_ParsesEveryField()
        {
            var options = ViewerOptions.Parse(new[]
            {
                "--url", "https://ja.wikipedia.org", "--size", "1920x1080",
                "--scroll-mode", "smoother", "--record",
                "--replay", "/tmp/replay.csv", "--statistics", "/tmp/statistics.csv",
            });
            Assert.That(options, Is.Not.Null);
            Assert.That(options!.Url, Is.EqualTo("https://ja.wikipedia.org"));
            Assert.That(options.Width, Is.EqualTo(1920));
            Assert.That(options.Height, Is.EqualTo(1080));
            Assert.That(options.Mode, Is.EqualTo(ScrollMode.Smoother));
            Assert.That(options.Record, Is.True);
            Assert.That(options.ReplayPath, Is.EqualTo("/tmp/replay.csv"));
            Assert.That(options.StatisticsPath, Is.EqualTo("/tmp/statistics.csv"));
        }

        [TestCase("raw", ScrollMode.Raw)]
        [TestCase("smoother", ScrollMode.Smoother)]
        [TestCase("resampler", ScrollMode.Resampler)]
        public void Parse_ScrollMode_MapsName(string name, ScrollMode expected)
        {
            var options = ViewerOptions.Parse(new[] { "--scroll-mode", name });
            Assert.That(options!.Mode, Is.EqualTo(expected));
        }

        [TestCase("--size", "abc")]
        [TestCase("--size", "100")]
        [TestCase("--scroll-mode", "unknown")]
        public void Parse_InvalidValue_ReturnsNull(string flag, string value)
        {
            Assert.That(ViewerOptions.Parse(new[] { flag, value }), Is.Null);
        }

        [Test]
        public void Parse_UnknownFlag_ReturnsNull()
        {
            Assert.That(ViewerOptions.Parse(new[] { "--frobnicate" }), Is.Null);
        }

        [Test]
        public void Parse_Analyze_ParsesPath()
        {
            var options = ViewerOptions.Parse(new[] { "--analyze", "/tmp/statistics.csv" });
            Assert.That(options!.AnalyzePath, Is.EqualTo("/tmp/statistics.csv"));
        }
    }
}
