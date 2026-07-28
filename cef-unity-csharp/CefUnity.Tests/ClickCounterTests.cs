using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ClickCounterTests
    {
        [Test]
        public void OnMouseDown_QuickSamePlace_IncrementsClickCount()
        {
            var counter = new ClickCounter();
            Assert.That(counter.OnMouseDown(0.0, 100, 100), Is.EqualTo(1));
            Assert.That(counter.OnMouseDown(0.3, 101, 101), Is.EqualTo(2));
            Assert.That(counter.OnMouseDown(0.6, 100, 102), Is.EqualTo(3));
        }

        [Test]
        public void OnMouseDown_TooSlow_ResetsToSingle()
        {
            var counter = new ClickCounter();
            counter.OnMouseDown(0.0, 100, 100);
            Assert.That(counter.OnMouseDown(0.6, 100, 100), Is.EqualTo(1)); // 500ms 超
        }

        [Test]
        public void OnMouseDown_TooFar_ResetsToSingle()
        {
            var counter = new ClickCounter();
            counter.OnMouseDown(0.0, 100, 100);
            Assert.That(counter.OnMouseDown(0.1, 200, 100), Is.EqualTo(1)); // 4px 超
        }
    }
}
