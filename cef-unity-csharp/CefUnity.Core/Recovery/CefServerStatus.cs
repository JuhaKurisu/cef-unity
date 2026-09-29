using System;

namespace CefUnity.Runtime
{
    /// <summary>CEF server プロセスの状態。値は Rust 側 (<c>CEF_UNITY_SERVER_STATE_*</c>) と揃える。</summary>
    public enum CefServerState
    {
        /// <summary>Initialize 前、または Shutdown 後。</summary>
        NotStarted = 0,
        Running = 1,
        /// <summary>server を失い、再起動してブラウザを作り直している。</summary>
        Recovering = 2,
        /// <summary>短時間に落ち続けた、または起動できなかったため復旧を諦めた。</summary>
        Failed = 3
    }

    /// <summary>server を失った理由。値は Rust 側 (<c>ServerLossReason</c>) と揃える。</summary>
    public enum CefServerLossReason
    {
        None = 0,
        /// <summary>プロセスが終了した (クラッシュ・外部からの kill)。</summary>
        Exited = 1,
        /// <summary>応答しなくなった (heartbeat の停止・IPC 応答のタイムアウト)。</summary>
        Unresponsive = 2,
        /// <summary>IPC の送受信に失敗した。</summary>
        ConnectionLost = 3
    }

    public readonly struct CefServerStatus
    {
        public readonly CefServerState State;
        public readonly CefServerLossReason LastLossReason;

        /// <summary>Initialize 以降に server を失った回数。</summary>
        public readonly uint LossCount;

        /// <summary>Initialize 以降に復旧できた回数。</summary>
        public readonly uint RecoveryCount;

        public CefServerStatus(CefServerState state, CefServerLossReason lastLossReason, uint lossCount, uint recoveryCount)
        {
            State = state;
            LastLossReason = lastLossReason;
            LossCount = lossCount;
            RecoveryCount = recoveryCount;
        }

        public override string ToString() =>
            $"{State} (lastLossReason={LastLossReason}, losses={LossCount}, recoveries={RecoveryCount})";
    }

    /// <summary>Pump の間に起きた server の出来事。</summary>
    [Flags]
    public enum CefServerTransitions
    {
        None = 0,
        Lost = 1,
        Recovered = 2,
        RecoveryFailed = 4
    }

    /// <summary>
    ///     前回観測した <see cref="CefServerStatus" /> と比べて、その間に起きた出来事を求める。
    ///     1 回の観測の間に複数の出来事が起き得る (失ってすぐ復旧した等) ので、回数の増分で判定する。
    /// </summary>
    public sealed class CefServerStatusTracker
    {
        private CefServerStatus _previous;

        /// <summary>基準を置き直す (Initialize 直後に呼ぶ。回数は Initialize で 0 に戻る)。</summary>
        public void Reset(CefServerStatus current)
        {
            _previous = current;
        }

        public CefServerTransitions Observe(CefServerStatus current)
        {
            var transitions = CefServerTransitions.None;
            if (current.LossCount > _previous.LossCount) transitions |= CefServerTransitions.Lost;
            if (current.RecoveryCount > _previous.RecoveryCount) transitions |= CefServerTransitions.Recovered;
            if (current.State == CefServerState.Failed && _previous.State != CefServerState.Failed)
                transitions |= CefServerTransitions.RecoveryFailed;
            _previous = current;
            return transitions;
        }
    }
}
