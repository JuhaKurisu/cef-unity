// CEF Server entry point.
//
// Runs CEF in its own process, communicates with Unity via ipc-channel + shared memory.
// Platform-specific event loop is in the event_loop module.

mod d3d11_pool;
mod event_loop;
mod server;

use std::io::Write;

use ipc_channel::ipc::{self as ipc, IpcSender};

use cef_unity_ipc::{Bootstrap, CommandEnvelope, Response};

/// main 内ローカルログの有効/無効。--logging で設定。server::log とは別系統だが
/// 同じフラグに従わせる。
static MAIN_LOG_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn log(message: &str) {
    if !MAIN_LOG_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let path = std::env::temp_dir().join("cef_unity_server.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "[{:?}] {}", std::time::SystemTime::now(), message);
    }
}

fn main() {
    // 最初に --logging を確定させ、以降の log() (main / server 双方) を従わせる。
    let logging: bool = std::env::args()
        .skip_while(|argument| argument != "--logging")
        .nth(1)
        .and_then(|text| text.parse::<i32>().ok())
        .map(|value| value != 0)
        .unwrap_or(false);
    MAIN_LOG_ENABLED.store(logging, std::sync::atomic::Ordering::Relaxed);
    server::set_logging(logging);

    if logging {
        let _ = std::fs::write(std::env::temp_dir().join("cef_unity_server.log"), "");
    }
    log(&format!("server started, pid={}", std::process::id()));

    // Parse --ipc-server argument
    let ipc_server_name = std::env::args()
        .skip_while(|argument| argument != "--ipc-server")
        .nth(1)
        .expect("--ipc-server argument required");
    log(&format!("ipc_server_name = {}", ipc_server_name));

    // Parse --client-pid (optional; Windows D3D11 共有のために使う)
    let client_pid: Option<u32> = std::env::args()
        .skip_while(|argument| argument != "--client-pid")
        .nth(1)
        .and_then(|text| text.parse().ok());
    log(&format!("client_pid = {:?}", client_pid));

    // Parse --use-gpu (optional; default 1 = GPU). 0 で software paint を強制する。
    let use_gpu: bool = std::env::args()
        .skip_while(|argument| argument != "--use-gpu")
        .nth(1)
        .and_then(|text| text.parse::<i32>().ok())
        .map(|value| value != 0)
        .unwrap_or(true);
    log(&format!("use_gpu = {}", use_gpu));

    // Initialize CEF first (server must be ready before accepting connections)
    let cef_server = server::CefServer::new(client_pid, use_gpu);
    if !cef_server.initialize_cef() {
        log("CEF initialization failed");
        std::process::exit(1);
    }
    log("CEF initialized successfully");

    // Initialize Mach IOSurface port service (macOS only, GPU モード時のみ)
    #[cfg(target_os = "macos")]
    if use_gpu {
        let service_name = cef_unity_ipc::iosurface_service_name(std::process::id());
        let c_service_name = std::ffi::CString::new(service_name.as_str()).unwrap();
        unsafe extern "C" {
            fn mach_iosurface_server_init(service_name: *const std::ffi::c_char) -> i32;
        }
        let result = unsafe { mach_iosurface_server_init(c_service_name.as_ptr()) };
        if result == 0 {
            log(&format!("Mach IOSurface service registered: {}", service_name));
        } else {
            log(&format!("Mach IOSurface service init failed: {}", result));
        }
    }

    // Create bidirectional channels
    let (command_sender, command_receiver) =
        ipc::channel::<CommandEnvelope>().expect("failed to create cmd channel");
    let (response_sender, response_receiver) = ipc::channel::<Response>().expect("failed to create resp channel");

    // Connect to client's one-shot server and send bootstrap
    let bootstrap_sender =
        IpcSender::connect(ipc_server_name).expect("failed to connect to client one-shot server");
    bootstrap_sender
        .send(Bootstrap {
            command_sender,
            response_receiver,
            server_pid: std::process::id(),
        })
        .expect("failed to send bootstrap");
    log("bootstrap sent to client");

    // IPC → mpsc ブリッジスレッド: IPC recv をブロッキング待ちし、
    // コマンド到着時に即座にイベントループを起こす。
    let (mpsc_sender, mpsc_receiver) = std::sync::mpsc::channel::<CommandEnvelope>();
    std::thread::spawn(move || {
        loop {
            match command_receiver.recv() {
                Ok(envelope) => {
                    if mpsc_sender.send(envelope).is_err() {
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
        command_receiver: mpsc_receiver,
        response_sender,
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
