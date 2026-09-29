using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="CefServerStatusTracker" /> と <see cref="CefBrowserRecoveryTracker" /> の単体テスト。
    ///     Pump ごとの観測から、復旧イベントを重複も取りこぼしもなく一度ずつ発行できること。
    /// </summary>
    public class CrashRecoveryTrackerTests
    {
        private static CefServerStatus Status(CefServerState state, uint losses, uint recoveries) =>
            new CefServerStatus(state,
                losses == 0 ? CefServerLossReason.None : CefServerLossReason.Exited, losses, recoveries);

        [Test]
        public void Running_WithoutChanges_RaisesNothing()
        {
            var tracker = new CefServerStatusTracker();
            tracker.Reset(Status(CefServerState.Running, 0, 0));
            Assert.AreEqual(CefServerTransitions.None, tracker.Observe(Status(CefServerState.Running, 0, 0)));
        }

        [Test]
        public void LossThenRecovery_RaisesEachOnce()
        {
            var tracker = new CefServerStatusTracker();
            tracker.Reset(Status(CefServerState.Running, 0, 0));

            Assert.AreEqual(CefServerTransitions.Lost, tracker.Observe(Status(CefServerState.Recovering, 1, 0)));
            Assert.AreEqual(CefServerTransitions.None, tracker.Observe(Status(CefServerState.Recovering, 1, 0)),
                "復旧を待つ間は何も起きない");
            Assert.AreEqual(CefServerTransitions.Recovered, tracker.Observe(Status(CefServerState.Running, 1, 1)));
            Assert.AreEqual(CefServerTransitions.None, tracker.Observe(Status(CefServerState.Running, 1, 1)));
        }

        /// <summary>観測の合間に「失って、復旧して、また失った」場合も全部拾う。</summary>
        [Test]
        public void SeveralEventsBetweenObservations_AreAllReported()
        {
            var tracker = new CefServerStatusTracker();
            tracker.Reset(Status(CefServerState.Running, 0, 0));
            Assert.AreEqual(CefServerTransitions.Lost | CefServerTransitions.Recovered,
                tracker.Observe(Status(CefServerState.Recovering, 2, 1)));
        }

        [Test]
        public void GivingUp_RaisesRecoveryFailedOnce()
        {
            var tracker = new CefServerStatusTracker();
            tracker.Reset(Status(CefServerState.Running, 3, 3));

            Assert.AreEqual(CefServerTransitions.Lost | CefServerTransitions.RecoveryFailed,
                tracker.Observe(Status(CefServerState.Failed, 4, 3)));
            Assert.AreEqual(CefServerTransitions.None, tracker.Observe(Status(CefServerState.Failed, 4, 3)));
        }

        /// <summary>Shutdown → Initialize で回数が 0 に戻っても、Reset すれば誤検出しない。</summary>
        [Test]
        public void Reset_AfterReinitialize_StartsFromZeroAgain()
        {
            var tracker = new CefServerStatusTracker();
            tracker.Reset(Status(CefServerState.Running, 0, 0));
            tracker.Observe(Status(CefServerState.Running, 2, 2));

            tracker.Reset(Status(CefServerState.Running, 0, 0));
            Assert.AreEqual(CefServerTransitions.Lost, tracker.Observe(Status(CefServerState.Recovering, 1, 0)));
        }

        [Test]
        public void BrowserEvents_AreReportedOncePerIncrease()
        {
            var tracker = new CefBrowserRecoveryTracker();
            CefBrowserRecoveryStatus Status(uint terminations, uint recreations) =>
                new CefBrowserRecoveryStatus(terminations, CefRenderProcessTerminationStatus.ProcessCrashed, false,
                    recreations);

            Assert.AreEqual(CefBrowserTransitions.None, tracker.Observe(Status(0, 0)));
            Assert.AreEqual(CefBrowserTransitions.RenderProcessTerminated, tracker.Observe(Status(1, 0)));
            Assert.AreEqual(CefBrowserTransitions.None, tracker.Observe(Status(1, 0)), "同じ出来事を二度報告しない");
            Assert.AreEqual(CefBrowserTransitions.Recreated, tracker.Observe(Status(1, 1)));
            Assert.AreEqual(CefBrowserTransitions.RenderProcessTerminated | CefBrowserTransitions.Recreated,
                tracker.Observe(Status(2, 2)));
        }
    }
}
