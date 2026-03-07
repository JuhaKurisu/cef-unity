// CEF Server entry point.
//
// Runs CEF in its own process, communicates with Unity via ipc-channel + shared memory.
// Platform-specific event loop is in the event_loop module.

mod event_loop;
mod server;

use std::io::Write;

use ipc_channel::ipc::{self as ipc_ch, IpcSender};

use cef_unity_ipc::{Bootstrap, CommandEnvelope, Response};

fn log(msg: &str) {
    let path = std::env::temp_dir().join("cef_unity_server.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[{:?}] {}", std::time::SystemTime::now(), msg);
    }
}

fn main() {
    let _ = std::fs::write(std::env::temp_dir().join("cef_unity_server.log"), "");
    log(&format!("server started, pid={}", std::process::id()));

    // Parse --ipc-server argument
    let ipc_server_name = std::env::args()
        .skip_while(|a| a != "--ipc-server")
        .nth(1)
        .expect("--ipc-server argument required");
    log(&format!("ipc_server_name = {}", ipc_server_name));

    // Initialize CEF first (server must be ready before accepting connections)
    let cef_server = server::CefServer::new();
    if !cef_server.init_cef() {
        log("CEF initialization failed");
        std::process::exit(1);
    }
    log("CEF initialized successfully");

    // Create bidirectional channels
    let (cmd_tx, cmd_rx) =
        ipc_ch::channel::<CommandEnvelope>().expect("failed to create cmd channel");
    let (resp_tx, resp_rx) = ipc_ch::channel::<Response>().expect("failed to create resp channel");

    // Connect to client's one-shot server and send bootstrap
    let bootstrap_tx =
        IpcSender::connect(ipc_server_name).expect("failed to connect to client one-shot server");
    bootstrap_tx
        .send(Bootstrap { cmd_tx, resp_rx })
        .expect("failed to send bootstrap");
    log("bootstrap sent to client");

    // IPC → mpsc ブリッジスレッド: IPC recv をブロッキング待ちし、
    // コマンド到着時に即座にイベントループを起こす。
    let (mpsc_tx, mpsc_rx) = std::sync::mpsc::channel::<CommandEnvelope>();
    std::thread::spawn(move || {
        loop {
            match cmd_rx.recv() {
                Ok(env) => {
                    if mpsc_tx.send(env).is_err() {
                        break;
                    }
                    event_loop::schedule_pump(0);
                }
                Err(_) => break,
            }
        }
    });

    // Run platform-specific event loop
    let state = event_loop::ServerState {
        cef_server,
        cmd_rx: mpsc_rx,
        resp_tx,
        running: true,
        pump_count: 0,
    };

    let state = event_loop::run_event_loop(state);

    // Cleanup
    log(&format!("shutting down after {} pumps", state.pump_count));
    let mut cef_server = state.cef_server;
    cef_server.shutdown();

    log("server exit");
}
