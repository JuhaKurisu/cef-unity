// server プロセスの監視スレッド。
//
// プロセスの終了と heartbeat の停止を見張り、見つけたら復旧を要求する。
// 復旧そのもの (再起動・ブラウザの作り直し) はメインスレッド (`cef_unity_pump`) が
// 行う: ブラウザのハンドルはメインスレッドが無ロックで触っているため、別スレッドから
// 差し替えられない。ここは固まった server を kill するところまでを担う。kill すれば
// その server へ送信中のメインスレッドも IPC エラーで抜けられる。

use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use cef_unity_ipc::ServerStatusReader;

use crate::recovery_policy::{HEARTBEAT_STALL_POLLS, HeartbeatMonitor, WATCHDOG_POLL_INTERVAL};
use crate::{ServerLossReason, log_to_file, report_server_lost};

pub struct Watchdog {
    stop_requested: Arc<AtomicBool>,
    thread: JoinHandle<Option<ServerStatusReader>>,
}

impl Watchdog {
    /// `generation`: 監視する server の世代。古い世代の報告が新しい server を
    /// 巻き込まないよう、報告に添える。
    pub fn start(
        process: Arc<Mutex<Child>>,
        status_reader: Option<ServerStatusReader>,
        generation: u64,
    ) -> Watchdog {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let thread_stop_requested = Arc::clone(&stop_requested);
        let thread = std::thread::Builder::new()
            .name("cef-unity-watchdog".to_string())
            .spawn(move || watch(process, status_reader, generation, thread_stop_requested))
            .expect("failed to spawn watchdog thread");
        Watchdog {
            stop_requested,
            thread,
        }
    }

    /// 監視をやめる。server status の reader を返す (server が異常終了していたら
    /// 呼び出し側が所有権を引き取って後始末する)。
    pub fn stop(self) -> Option<ServerStatusReader> {
        self.stop_requested.store(true, Ordering::Release);
        // 待機中の監視スレッドをすぐ起こす (間隔いっぱい待たせるとメインスレッドが止まる)。
        self.thread.thread().unpark();
        self.thread.join().ok().flatten()
    }
}

fn watch(
    process: Arc<Mutex<Child>>,
    status_reader: Option<ServerStatusReader>,
    generation: u64,
    stop_requested: Arc<AtomicBool>,
) -> Option<ServerStatusReader> {
    let mut heartbeat_monitor = HeartbeatMonitor::new(HEARTBEAT_STALL_POLLS);
    loop {
        // park は stop の unpark で早く戻る。偽の起床もあり得るが、監視を 1 回
        // 早めるだけなので問題ない。
        std::thread::park_timeout(WATCHDOG_POLL_INTERVAL);
        if stop_requested.load(Ordering::Acquire) {
            return status_reader;
        }

        let exit_status = process
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_wait()
            .ok()
            .flatten();
        if let Some(exit_status) = exit_status {
            log_to_file(&format!("watchdog: server exited ({})", exit_status));
            report_server_lost(generation, ServerLossReason::Exited);
            return status_reader;
        }

        if let Some(reader) = status_reader.as_ref()
            && heartbeat_monitor.observe(reader.heartbeat())
        {
            log_to_file(&format!(
                "watchdog: server heartbeat stalled for {:?}; killing it",
                WATCHDOG_POLL_INTERVAL * HEARTBEAT_STALL_POLLS
            ));
            // kill より先に報告する。kill した瞬間からメインスレッドの IPC 送信も
            // 失敗し始めるので、後に回すと原因が「接続断」「終了」として先に記録される。
            report_server_lost(generation, ServerLossReason::Unresponsive);
            kill_server(&process);
            return status_reader;
        }
    }
}

/// server を強制終了して回収する。helper プロセス群は親の終了を検知して自分で終わる。
pub fn kill_server(process: &Mutex<Child>) {
    let mut child = process.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = child.kill();
    let _ = child.wait();
}
