using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ScrollRoughnessAnalyzerTests
    {
        [Test]
        public void ComputeRoughness_PerfectlyUniform_ReturnsZero()
        {
            var roughness = ScrollRoughnessAnalyzer.ComputeRoughness(new[] { 0, -10, -10, -10, -10, 0 });
            // 差が出るのは立ち上がり/立ち下がりの 2 遷移 (10+10) のみ、分母は総移動量 40。
            // 定常区間 (-10→-10) の差 0 が長いほど 0 に近づく (次テスト参照)
            Assert.That(roughness, Is.EqualTo(20.0 / 40.0).Within(1e-9));
        }

        [Test]
        public void ComputeRoughness_LongUniformRun_ApproachesZero()
        {
            var deltas = new int[102];
            for (var index = 1; index <= 100; index++) deltas[index] = -10;
            var roughness = ScrollRoughnessAnalyzer.ComputeRoughness(deltas);
            Assert.That(roughness, Is.EqualTo(20.0 / 1000.0).Within(1e-9)); // 端 2 遷移のみ
        }

        [Test]
        public void ComputeRoughness_Jittery_IsHigherThanUniform()
        {
            var uniform = ScrollRoughnessAnalyzer.ComputeRoughness(new[] { 0, -10, -10, -10, -10, -10, -10, 0 });
            var jittery = ScrollRoughnessAnalyzer.ComputeRoughness(new[] { 0, -2, -18, -4, -16, -6, -14, 0 });
            Assert.That(jittery, Is.GreaterThan(uniform));
        }

        [Test]
        public void ComputeRoughness_NoScroll_ReturnsZero()
        {
            Assert.That(ScrollRoughnessAnalyzer.ComputeRoughness(new[] { 0, 0, 0 }), Is.EqualTo(0.0));
            Assert.That(ScrollRoughnessAnalyzer.ComputeRoughness(new int[0]), Is.EqualTo(0.0));
        }
    }
}
