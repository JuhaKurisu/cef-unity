using System;

namespace CefUnity.Runtime
{
    /// <summary>レンダラープロセスの終了理由。値は CEF の <c>cef_termination_status_t</c> と揃える。</summary>
    public enum CefRenderProcessTerminationStatus
    {
        /// <summary>0 以外の終了コードで終了した。</summary>
        AbnormalTermination = 0,
        /// <summary>SIGKILL やタスクマネージャーで終了させられた (無応答で強制終了した場合も含む)。</summary>
        ProcessWasKilled = 1,
        /// <summary>セグメンテーション違反など。</summary>
        ProcessCrashed = 2,
        /// <summary>メモリ不足。プラットフォームによっては <see cref="ProcessCrashed" /> になる。</summary>
        ProcessOutOfMemory = 3,
        /// <summary>プロセスを起動できなかった。</summary>
        LaunchFailed = 4,
        /// <summary>Windows のコード整合性チェックで終了させられた。</summary>
        IntegrityFailure = 5
    }

    /// <summary>ブラウザの障害と復旧の状態。回数はどれもブラウザ作成以降の累計で、減らない。</summary>
    public readonly struct CefBrowserRecoveryStatus
    {
        /// <summary>レンダラーが終了した回数。</summary>
        public readonly uint RenderProcessTerminationCount;

        public readonly CefRenderProcessTerminationStatus LastRenderProcessTerminationStatus;

        /// <summary>
        ///     短時間にレンダラーのクラッシュが続いたため、自動再読み込みを止めているか。
        ///     止めている間ページは空白のまま。LoadUrl で別のページを開くと解除される。
        /// </summary>
        public readonly bool RenderProcessReloadSuppressed;

        /// <summary>
        ///     ブラウザを作り直した回数 (server の再起動、GPU プロセスの再起動による)。
        ///     作り直すとページは読み込み直しになる。
        /// </summary>
        public readonly uint RecreationCount;

        public CefBrowserRecoveryStatus(uint renderProcessTerminationCount,
            CefRenderProcessTerminationStatus lastRenderProcessTerminationStatus, bool renderProcessReloadSuppressed,
            uint recreationCount)
        {
            RenderProcessTerminationCount = renderProcessTerminationCount;
            LastRenderProcessTerminationStatus = lastRenderProcessTerminationStatus;
            RenderProcessReloadSuppressed = renderProcessReloadSuppressed;
            RecreationCount = recreationCount;
        }

        public override string ToString() =>
            $"terminations={RenderProcessTerminationCount} last={LastRenderProcessTerminationStatus} " +
            $"reloadSuppressed={RenderProcessReloadSuppressed} recreations={RecreationCount}";
    }

    /// <summary>前回の観測以降にブラウザで起きた出来事。</summary>
    [Flags]
    public enum CefBrowserTransitions
    {
        None = 0,
        RenderProcessTerminated = 1,
        Recreated = 2
    }

    /// <summary>回数の増分から、前回の観測以降に起きた出来事を求める。</summary>
    public sealed class CefBrowserRecoveryTracker
    {
        private uint _observedTerminationCount;
        private uint _observedRecreationCount;

        public CefBrowserTransitions Observe(CefBrowserRecoveryStatus current)
        {
            var transitions = CefBrowserTransitions.None;
            if (current.RenderProcessTerminationCount > _observedTerminationCount)
                transitions |= CefBrowserTransitions.RenderProcessTerminated;
            if (current.RecreationCount > _observedRecreationCount)
                transitions |= CefBrowserTransitions.Recreated;
            _observedTerminationCount = current.RenderProcessTerminationCount;
            _observedRecreationCount = current.RecreationCount;
            return transitions;
        }
    }
}
