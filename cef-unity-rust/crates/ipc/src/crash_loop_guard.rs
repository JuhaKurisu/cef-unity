// クラッシュループの判定。
//
// 自動復旧 (レンダラーの再読み込み、server の再起動) は、同じ原因で落ち続けると
// 復旧と障害を無限に繰り返す。一定時間内の障害回数が上限を超えたら復旧を諦める。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub struct CrashLoopGuard {
    /// `window` 内で許す復旧の回数。これを超えた障害では復旧しない。
    maximum_recoveries: usize,
    window: Duration,
    /// `window` 内に起きた障害の時刻 (古い順)。
    recent_failures: VecDeque<Instant>,
}

impl CrashLoopGuard {
    pub const fn new(maximum_recoveries: usize, window: Duration) -> Self {
        CrashLoopGuard {
            maximum_recoveries,
            window,
            recent_failures: VecDeque::new(),
        }
    }

    /// 障害を記録し、復旧を試みてよければ true を返す。
    pub fn record_failure(&mut self, now: Instant) -> bool {
        while let Some(&oldest) = self.recent_failures.front() {
            if now.saturating_duration_since(oldest) > self.window {
                self.recent_failures.pop_front();
            } else {
                break;
            }
        }
        self.recent_failures.push_back(now);
        self.recent_failures.len() <= self.maximum_recoveries
    }

    /// 履歴を消す。利用者が明示的に別のページへ移ったときなど、過去の障害と
    /// 無関係になった時点で呼ぶ。
    pub fn reset(&mut self) {
        self.recent_failures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(60);

    #[test]
    fn allows_recoveries_up_to_the_limit() {
        let mut guard = CrashLoopGuard::new(2, WINDOW);
        let start = Instant::now();
        assert!(guard.record_failure(start));
        assert!(guard.record_failure(start + Duration::from_secs(1)));
        assert!(!guard.record_failure(start + Duration::from_secs(2)), "3 回目は諦める");
        assert!(!guard.record_failure(start + Duration::from_secs(3)), "諦めた後も続けて諦める");
    }

    #[test]
    fn failures_outside_the_window_are_forgotten() {
        let mut guard = CrashLoopGuard::new(2, WINDOW);
        let start = Instant::now();
        assert!(guard.record_failure(start));
        assert!(guard.record_failure(start + Duration::from_secs(1)));
        // 最初の 2 回が窓の外へ出れば、また復旧してよい。
        assert!(guard.record_failure(start + Duration::from_secs(62)));
    }

    #[test]
    fn reset_clears_history() {
        let mut guard = CrashLoopGuard::new(1, WINDOW);
        let start = Instant::now();
        assert!(guard.record_failure(start));
        assert!(!guard.record_failure(start + Duration::from_secs(1)));
        guard.reset();
        assert!(guard.record_failure(start + Duration::from_secs(2)));
    }
}
