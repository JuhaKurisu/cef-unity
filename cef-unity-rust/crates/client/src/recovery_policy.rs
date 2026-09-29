// server 自動復旧の判断ロジック (副作用なし)。
//
// 監視スレッドと復旧の駆動 (lib.rs) はここの判断に従うだけにして、閾値と
// 再試行の組み立てを単体テストで固定する。

use std::time::Duration;

/// 監視スレッドが server を見に行く間隔。
pub const WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// heartbeat が何回続けて進まなければ server が固まったとみなすか
/// (`WATCHDOG_POLL_INTERVAL` × これ = 10 秒)。
///
/// 経過時間ではなく観測回数で数える。PC のスリープ復帰直後は経過時間が
/// 一気に伸びるが、server もすぐ動き出すので観測回数なら誤検出しない。
pub const HEARTBEAT_STALL_POLLS: u32 = 40;

/// 応答を待つ IPC 呼び出しの上限。これを超えたら server が固まったとみなす。
/// 上限がないと、固まった server への呼び出しで Unity のメインスレッドが永久に止まる。
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// server を失ってから復旧を諦めるまでの回数と窓。落ち続ける server を
/// 再起動し続けないため。
pub const SERVER_RECOVERIES_PER_WINDOW: usize = 3;
pub const SERVER_RECOVERY_WINDOW: Duration = Duration::from_secs(300);

/// 1 回の障害に対する起動の試行間隔。要素数が試行回数の上限。
/// 1 回目はすぐ起動する。
const LAUNCH_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(0),
    Duration::from_secs(1),
    Duration::from_secs(3),
];

/// heartbeat の停止を判定する。
pub struct HeartbeatMonitor {
    last_heartbeat: Option<u64>,
    unchanged_polls: u32,
    stall_polls: u32,
}

impl HeartbeatMonitor {
    pub fn new(stall_polls: u32) -> Self {
        HeartbeatMonitor {
            last_heartbeat: None,
            unchanged_polls: 0,
            stall_polls,
        }
    }

    /// 観測した heartbeat を渡す。固まったと判断したら true。
    pub fn observe(&mut self, heartbeat: u64) -> bool {
        if self.last_heartbeat == Some(heartbeat) {
            self.unchanged_polls += 1;
        } else {
            self.last_heartbeat = Some(heartbeat);
            self.unchanged_polls = 0;
        }
        self.unchanged_polls >= self.stall_polls
    }
}

/// `attempt` 回目 (0 始まり) の起動をいつ行うか。試行を使い切ったら None。
pub fn launch_retry_delay(attempt: usize) -> Option<Duration> {
    LAUNCH_RETRY_DELAYS.get(attempt).copied()
}

/// `attempt` 回目の起動でキャッシュを消すか。1 回目は消さない
/// (ログイン状態などを保つ)。1 回目で起動できなかったのはキャッシュの破損が
/// 原因のことがある (強制終了の繰り返しで起動がハングした実例がある) ので、
/// 2 回目以降は消してから起動する。
pub fn should_reset_cache(attempt: usize) -> bool {
    attempt >= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_stall_needs_consecutive_unchanged_polls() {
        let mut monitor = HeartbeatMonitor::new(3);
        assert!(!monitor.observe(5), "初回は基準値の記録だけ");
        assert!(!monitor.observe(5));
        assert!(!monitor.observe(5));
        assert!(monitor.observe(5), "3 回続けて進まなければ停止");
    }

    #[test]
    fn heartbeat_progress_resets_the_count() {
        let mut monitor = HeartbeatMonitor::new(3);
        monitor.observe(1);
        monitor.observe(1);
        monitor.observe(1);
        assert!(!monitor.observe(2), "進んだら数え直す");
        assert!(!monitor.observe(2));
        assert!(!monitor.observe(2));
        assert!(monitor.observe(2));
    }

    #[test]
    fn launch_retries_are_bounded() {
        assert_eq!(launch_retry_delay(0), Some(Duration::ZERO), "1 回目はすぐ起動する");
        assert!(launch_retry_delay(1).unwrap() < launch_retry_delay(2).unwrap(), "間隔は伸びる");
        assert_eq!(launch_retry_delay(LAUNCH_RETRY_DELAYS.len()), None);
    }

    #[test]
    fn cache_is_reset_only_on_retries() {
        assert!(!should_reset_cache(0));
        assert!(should_reset_cache(1));
        assert!(should_reset_cache(2));
    }
}
