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

    public readonly struct CefRenderProcessStatus
    {
        /// <summary>ブラウザ作成以降にレンダラーが終了した累計回数 (server を再起動しても戻らない)。</summary>
        public readonly uint TerminationCount;

        public readonly CefRenderProcessTerminationStatus LastTerminationStatus;

        /// <summary>
        ///     短時間にクラッシュが続いたため、自動再読み込みを止めているか。
        ///     止めている間ページは空白のまま。LoadUrl で別のページを開くと解除される。
        /// </summary>
        public readonly bool ReloadSuppressed;

        public CefRenderProcessStatus(uint terminationCount, CefRenderProcessTerminationStatus lastTerminationStatus,
            bool reloadSuppressed)
        {
            TerminationCount = terminationCount;
            LastTerminationStatus = lastTerminationStatus;
            ReloadSuppressed = reloadSuppressed;
        }

        public override string ToString() =>
            $"terminations={TerminationCount} last={LastTerminationStatus} reloadSuppressed={ReloadSuppressed}";
    }

    /// <summary>レンダラーの終了回数の増分から、前回の観測以降に終了したかを判定する。</summary>
    public sealed class CefRenderProcessTerminationTracker
    {
        private uint _observedTerminationCount;

        public bool Observe(CefRenderProcessStatus current)
        {
            if (current.TerminationCount <= _observedTerminationCount) return false;
            _observedTerminationCount = current.TerminationCount;
            return true;
        }
    }
}
