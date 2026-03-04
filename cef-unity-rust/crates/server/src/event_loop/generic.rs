// Generic event loop (Linux/Windows): thread::sleep ベースのポーリング。

use std::time::Duration;

use ipc_channel::TryRecvError;

use cef_unity_ipc::Command;

use super::ServerState;

fn log(msg: &str) {
    crate::log(msg);
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
            std::thread::sleep(Duration::from_millis(4));
        }
    }

    state
}

fn tick(state: &mut ServerState) {
    cef::do_message_loop_work();
    state.pump_count += 1;

    loop {
        match state.cmd_rx.try_recv() {
            Ok(cmd) => {
                let is_shutdown = matches!(cmd, Command::Shutdown);
                let needs_response = cmd.needs_response();
                if needs_response {
                    log(&format!("received command: {:?}", cmd));
                }
                let resp = state.cef_server.handle_command(cmd);
                if needs_response {
                    if let Err(e) = state.resp_tx.send(resp) {
                        log(&format!("send error: {}", e));
                        state.running = false;
                        break;
                    }
                }
                if is_shutdown {
                    state.running = false;
                    break;
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::IpcError(e)) => {
                log(&format!("client disconnected: {}", e));
                state.running = false;
                break;
            }
        }
    }
}
