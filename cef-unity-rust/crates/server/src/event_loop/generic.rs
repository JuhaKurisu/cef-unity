// Generic event loop (Linux/Windows): condvar ベースのポーリング。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use super::ServerState;

// ---------------------------------------------------------------------------
// Global schedule signal
// ---------------------------------------------------------------------------

static SCHEDULE_DELAY_MILLISECONDS: AtomicI64 = AtomicI64::new(4);
static WAKE_MUTEX: Mutex<()> = Mutex::new(());
static WAKE_CONDVAR: Condvar = Condvar::new();

/// Called from BrowserProcessHandler::on_schedule_message_pump_work.
pub fn schedule_pump(delay_milliseconds: i64) {
    SCHEDULE_DELAY_MILLISECONDS.store(delay_milliseconds.max(0), Ordering::Release);
    // Wake the sleeping event loop
    let _guard = WAKE_MUTEX.lock().unwrap();
    WAKE_CONDVAR.notify_one();
}

fn log(message: &str) {
    crate::log(message);
}

pub fn run_event_loop(mut state: ServerState) -> ServerState {
    log("entering generic event loop");

    while state.running {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tick(&mut state);
        }));
        if result.is_err() {
            log("event loop tick panicked, shutting down");
            state.running = false;
        }

        if state.running {
            let delay = SCHEDULE_DELAY_MILLISECONDS.load(Ordering::Acquire);
            let wait = Duration::from_millis(delay.max(1) as u64);
            let guard = WAKE_MUTEX.lock().unwrap();
            let _ = WAKE_CONDVAR.wait_timeout(guard, wait);
        }
    }

    state
}

fn tick(state: &mut ServerState) {
    // IPC コマンドを先に処理 → マウスイベント等が同じ pump サイクルで CEF に反映される
    drain_commands(state);

    if !state.running {
        return;
    }

    // server-side flush: 保留中の BeginFrame#2 (flush) を発行時刻が来ていれば撃つ。
    state.cef_server.process_pending_flushes();

    cef::do_message_loop_work();
    state.pump_count += 1;
}

fn drain_commands(state: &mut ServerState) {
    loop {
        match state.command_receiver.try_recv() {
            Ok(envelope) => {
                let is_shutdown = matches!(envelope.command, cef_unity_ipc::Command::Shutdown);
                if envelope.expects_response {
                    log(&format!("received command: {:?}", envelope.command));
                }
                let response = state.cef_server.handle_command(envelope.command);
                if envelope.expects_response {
                    if let Err(error) = state.response_sender.send(response) {
                        log(&format!("send error: {}", error));
                        state.running = false;
                        break;
                    }
                }
                if is_shutdown {
                    state.running = false;
                    break;
                }
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                log("IPC bridge disconnected");
                state.running = false;
                break;
            }
        }
    }
}
