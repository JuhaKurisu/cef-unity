#![cfg_attr(windows, windows_subsystem = "windows")]

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

/// `--name=value` と `--name value` の両形式から値を取り出す。
///
/// クライアントは Chromium と同じ `--name=value` 形式で渡す。Chromium は `--name value`
/// を「値なしスイッチ + 位置引数」と解釈するため、CEF が自分を起動し直す経路
/// (Windows の非昇格化など) で値が位置引数へ分離され、`--ipc-server` の値として
/// 次のスイッチ `--client-pid` を読んでしまっていた。空白区切りの形式は古い
/// クライアントとテスト向けに受け付けるが、次の要素がスイッチなら値なしとみなす。
fn argument_value(arguments: &[String], name: &str) -> Option<String> {
    let prefix = format!("{}=", name);
    for (index, argument) in arguments.iter().enumerate() {
        if let Some(value) = argument.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
        if argument == name {
            return arguments
                .get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .cloned();
        }
    }
    None
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();

    // 最初に --logging を確定させ、以降の log() (main / server 双方) を従わせる。
    let logging: bool = argument_value(&arguments, "--logging")
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
    let Some(ipc_server_name) = argument_value(&arguments, "--ipc-server") else {
        // Unity 以外から起動された (または引数が壊れた) 場合。GUI サブシステムで
        // panic メッセージは誰にも見えないので、ログへ残して終了する。
        log(&format!("--ipc-server argument required, arguments = {:?}", arguments));
        eprintln!("--ipc-server argument required");
        std::process::exit(2);
    };
    log(&format!("ipc_server_name = {}", ipc_server_name));

    // Parse --client-pid (optional; Windows D3D11 共有のために使う)
    let client_pid: Option<u32> =
        argument_value(&arguments, "--client-pid").and_then(|text| text.parse().ok());
    log(&format!("client_pid = {:?}", client_pid));

    // Parse --use-gpu (optional; default 1 = GPU). 0 で software paint を強制する。
    let use_gpu: bool = argument_value(&arguments, "--use-gpu")
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

#[cfg(test)]
mod tests {
    use super::argument_value;

    fn arguments(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn reads_equals_form() {
        let list = arguments("server --ipc-server=abc --client-pid=42");
        assert_eq!(argument_value(&list, "--ipc-server").as_deref(), Some("abc"));
        assert_eq!(argument_value(&list, "--client-pid").as_deref(), Some("42"));
    }

    #[test]
    fn reads_space_form() {
        let list = arguments("server --ipc-server abc --client-pid 42");
        assert_eq!(argument_value(&list, "--ipc-server").as_deref(), Some("abc"));
        assert_eq!(argument_value(&list, "--client-pid").as_deref(), Some("42"));
    }

    /// Chromium が組み直した引数 (値が末尾の位置引数へ分離される) では、
    /// 次のスイッチを値として読まない。
    #[test]
    fn does_not_take_next_switch_as_value() {
        let list = arguments(
            "server --ipc-server --client-pid --use-gpu --logging --do-not-de-elevate abc 42 1 0",
        );
        assert_eq!(argument_value(&list, "--ipc-server"), None);
        assert_eq!(argument_value(&list, "--client-pid"), None);
    }

    #[test]
    fn keeps_equals_form_after_chromium_rebuild() {
        let list = arguments("server --ipc-server=abc --client-pid=42 --no-sandbox --do-not-de-elevate");
        assert_eq!(argument_value(&list, "--ipc-server").as_deref(), Some("abc"));
        assert_eq!(argument_value(&list, "--client-pid").as_deref(), Some("42"));
    }

    #[test]
    fn missing_argument_is_none() {
        let list = arguments("server --client-pid=42");
        assert_eq!(argument_value(&list, "--ipc-server"), None);
    }
}
