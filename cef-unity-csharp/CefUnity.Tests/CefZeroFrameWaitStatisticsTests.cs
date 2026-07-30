using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="CefZeroFrameWaitStatistics" /> の単体テスト。
    ///     issue #11 の指摘②「計装カウンタが待機頻度を測れない」の回帰テストを含む:
    ///     旧実装は block_avg の分母に fresh+fallback を使っており、ブロックしない
    ///     抑止スキップパスでもこれらが加算されるため、実測で 3〜15 倍の過小評価になっていた。
    /// </summary>
    public class CefZeroFrameWaitStatisticsTests
    {
        [Test]
        public void NonWaitingPaths_DoNotCountAsWaitEntered()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordNoWaitReceive(receivedFreshPaint: true);
            statistics.RecordIdleSkip(receivedFreshPaint: false);
            statistics.RecordSuppressedSkip(receivedFreshPaint: true);

            Assert.AreEqual(0, statistics.WaitEnteredCount, "ブロックしないパスは待機回数に入らない");
            Assert.AreEqual(0.0, statistics.SpinTotalMilliseconds, "spin 時間も増えない");
            Assert.AreEqual(1, statistics.NoWaitCount);
            Assert.AreEqual(1, statistics.IdleSkipCount);
            Assert.AreEqual(1, statistics.SuppressedSkipCount);
        }

        [Test]
        public void BlockAverage_UsesWaitEnteredCountAsDenominator()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 6.0);
            statistics.RecordWaitCompleted(receivedFreshPaint: false, spinMilliseconds: 8.0);

            Assert.AreEqual(2, statistics.WaitEnteredCount);
            Assert.AreEqual(14.0, statistics.SpinTotalMilliseconds, 1e-9);
            Assert.AreEqual(7.0, statistics.BlockAverageMilliseconds, 1e-9, "spin 合計 ÷ 待機回数");
            Assert.AreEqual(8.0, statistics.SpinMaximumMilliseconds, 1e-9, "最大値を保持する");
        }

        [Test]
        public void BlockAverage_IsNotDilutedBySuppressedSkips()
        {
            // 実測条件の再現: 60 フレーム中 42 が抑止スキップ、18 が実待機で 1 回 7ms。
            var statistics = new CefZeroFrameWaitStatistics();
            for (var frameIndex = 0; frameIndex < 42; frameIndex++)
                statistics.RecordSuppressedSkip(receivedFreshPaint: true);
            for (var frameIndex = 0; frameIndex < 18; frameIndex++)
                statistics.RecordWaitCompleted(receivedFreshPaint: false, spinMilliseconds: 7.0);

            Assert.AreEqual(7.0, statistics.BlockAverageMilliseconds, 1e-9,
                "抑止スキップは分母に入れない (旧実装は 126/60 = 2.1ms と表示していた)");
        }

        [Test]
        public void BlockAverage_IsZeroWhenNeverWaited()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordNoWaitReceive(receivedFreshPaint: true);
            Assert.AreEqual(0.0, statistics.BlockAverageMilliseconds, "0 除算せず 0 を返す");
        }

        [Test]
        public void ReceiveTotals_MatchPathTotals()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordNoWaitReceive(receivedFreshPaint: true);
            statistics.RecordIdleSkip(receivedFreshPaint: true);
            statistics.RecordSuppressedSkip(receivedFreshPaint: false);
            statistics.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 3.0);

            var receiveTotal = statistics.FreshCount + statistics.FallbackCount;
            var pathTotal = statistics.NoWaitCount + statistics.IdleSkipCount
                          + statistics.SuppressedSkipCount + statistics.WaitEnteredCount;
            Assert.AreEqual(4, receiveTotal, "全パスで受信の成否を数える");
            Assert.AreEqual(receiveTotal, pathTotal, "2 つの軸の合計はどちらもフレーム数");
            Assert.AreEqual(3, statistics.FreshCount);
            Assert.AreEqual(1, statistics.FallbackCount);
        }

        [Test]
        public void Add_AccumulatesWindowsIntoTotals()
        {
            var window = new CefZeroFrameWaitStatistics();
            window.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 4.0);
            window.RecordSuppressedSkip(receivedFreshPaint: false);

            var totals = new CefZeroFrameWaitStatistics();
            totals.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 9.0);
            totals.Add(window);

            Assert.AreEqual(2, totals.WaitEnteredCount);
            Assert.AreEqual(1, totals.SuppressedSkipCount);
            Assert.AreEqual(13.0, totals.SpinTotalMilliseconds, 1e-9);
            Assert.AreEqual(9.0, totals.SpinMaximumMilliseconds, 1e-9, "最大値は大きい方が残る");
            Assert.AreEqual(2, totals.FreshCount);
            Assert.AreEqual(1, totals.FallbackCount);
        }

        [Test]
        public void Reset_ClearsEveryCounter()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 5.0);
            statistics.RecordIdleSkip(receivedFreshPaint: false);
            statistics.Reset();

            Assert.AreEqual(0, statistics.FreshCount);
            Assert.AreEqual(0, statistics.FallbackCount);
            Assert.AreEqual(0, statistics.NoWaitCount);
            Assert.AreEqual(0, statistics.IdleSkipCount);
            Assert.AreEqual(0, statistics.SuppressedSkipCount);
            Assert.AreEqual(0, statistics.WaitEnteredCount);
            Assert.AreEqual(0.0, statistics.SpinTotalMilliseconds);
            Assert.AreEqual(0.0, statistics.SpinMaximumMilliseconds);
        }

        [Test]
        public void FormatLine_ContainsSeparatedCounters()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordSuppressedSkip(receivedFreshPaint: true);
            statistics.RecordWaitCompleted(receivedFreshPaint: false, spinMilliseconds: 7.0);

            var line = statistics.FormatLine();
            StringAssert.Contains("wait_entered=1", line);
            StringAssert.Contains("suppressed_skip=1", line);
            StringAssert.Contains("block_avg=7.00ms", line);
        }
    }
}
