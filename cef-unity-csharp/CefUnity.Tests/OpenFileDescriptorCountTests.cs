using System.IO;
using System.Runtime.InteropServices;
using CefUnity.Harness;
using NUnit.Framework;

namespace CefUnity.Tests
{
    /// <summary>
    ///     Linux の file descriptor リーク検出。dmabuf の fd 転送は静かに壊れるため、
    ///     lifecycle で機械的に検出できるようにしてある。
    /// </summary>
    [TestFixture]
    public class OpenFileDescriptorCountTests
    {
        [Test]
        public void CountOpenFileDescriptors_Linux_IncreasesWhenFileOpened()
        {
            if (!RuntimeInformation.IsOSPlatform(OSPlatform.Linux))
            {
                Assert.Ignore("Linux 以外では /proc/self/fd が無い");
            }
            var before = OpenFileDescriptorCounter.Count();
            using var file = File.Open("/dev/null", FileMode.Open);
            var after = OpenFileDescriptorCounter.Count();
            Assert.That(after, Is.GreaterThan(before));
        }

        [Test]
        public void CountOpenFileDescriptors_NonLinux_ReturnsNegativeOne()
        {
            if (RuntimeInformation.IsOSPlatform(OSPlatform.Linux))
            {
                Assert.Ignore("Linux では実数が返る");
            }
            Assert.That(OpenFileDescriptorCounter.Count(), Is.EqualTo(-1));
        }
    }
}
