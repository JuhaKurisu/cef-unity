//! 2160px 超ビューポートの回帰テスト (2026-07-23 のテクスチャ縦伸びリグレッション)。
//!
//! server が viewport を MAX_W/MAX_H (software shared_memory の制約) に clamp すると、
//! GPU (IOSurface) 経路でもクライアント申告サイズと乖離し、Unity 側で
//! テクスチャ引き伸ばし + マウス座標ズレになる (Retina の縦長 Game view は
//! 2160px を超える — 実セッションで CreateBrowser 1706x2762 を確認)。
//! GPU 経路は任意サイズを受理し、accel paint の寸法 (shared_memory の accelerated_width/height)
//! が申告サイズにそのまま一致することを検証する。
//!
//! 実行 (バンドル済みサーバー必須・CEF は同時 1 インスタンスのみ):
//!   CEF_SERVER_APP=../cef-unity-unityproject/Assets/CefUnity/Plugins/osx-arm64/cef-unity-server.app \
//!     cargo test -p cef-unity-server --test oversize_viewport -- --ignored

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use ipc_channel::ipc::{self, IpcOneShotServer};

use cef_unity_ipc::{Bootstrap, Command, CommandEnvelope, Response, SharedMemoryReader};

struct TestCefServer {
    command_sender: ipc::IpcSender<CommandEnvelope>,
    response_receiver: ipc::IpcReceiver<Response>,
    child: std::process::Child,
}

impl TestCefServer {
    fn start() -> Self {
        let server_app = find_server_app().expect(
            "バンドル済みサーバーが見つかりません (CEF_SERVER_APP で指定可)",
        );
        let server_bin = server_app.join("Contents/MacOS/cef-unity-server");
        assert!(server_bin.exists(), "{}", server_bin.display());

        let (oneshot_server, server_name) =
            IpcOneShotServer::<Bootstrap>::new().expect("one-shot server");

        let child = std::process::Command::new(&server_bin)
            .arg("--ipc-server")
            .arg(&server_name)
            .spawn()
            .unwrap_or_else(|error| panic!("{}: {}", server_bin.display(), error));

        let (_receiver, bootstrap) = oneshot_server.accept().expect("bootstrap");

        TestCefServer {
            command_sender: bootstrap.command_sender,
            response_receiver: bootstrap.response_receiver,
            child,
        }
    }

    fn send(&self, command: Command) -> Response {
        self.command_sender
            .send(CommandEnvelope {
                command: command,
                expects_response: true,
            })
            .unwrap();
        self.response_receiver.recv().unwrap()
    }

    fn fire(&self, command: Command) {
        self.command_sender
            .send(CommandEnvelope {
                command: command,
                expects_response: false,
            })
            .unwrap();
    }
}

/// panic 時にもサーバーを確実に終了させる (CEF シングルトンロック残留防止)。
impl Drop for TestCefServer {
    fn drop(&mut self) {
        let _ = self.command_sender.send(CommandEnvelope {
            command: Command::Shutdown,
            expects_response: false,
        });
        thread::sleep(Duration::from_millis(500));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find_server_app() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CEF_SERVER_APP") {
        let path = PathBuf::from(&path);
        if path.exists() {
            return Some(path);
        }
    }
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    let candidates = [
        workspace_root.join("target/debug/test-bundle/cef-unity-server.app"),
        workspace_root
            .parent()?
            .join("cef-unity-unityproject/Assets/CefUnity/Plugins/osx-arm64/cef-unity-server.app"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

/// accelerated_width/height が (want_width, want_height) になるまで BeginFrame を送りつつ待つ。
fn wait_for_accelerated_dimensions(
    server: &TestCefServer,
    shared_memory: &SharedMemoryReader,
    browser_id: u32,
    want_width: u32,
    want_height: u32,
    timeout: Duration,
) -> (u32, u32) {
    let deadline = Instant::now() + timeout;
    let mut frame: u64 = 1;
    let mut last = (0u32, 0u32);
    while Instant::now() < deadline {
        server.fire(Command::SendExternalBeginFrame {
            browser_id,
            unity_frame: frame,
        });
        frame += 1;
        let (width, height) = shared_memory.read_accelerated_dimensions();
        last = (width, height);
        if width == want_width && height == want_height {
            return last;
        }
        thread::sleep(Duration::from_millis(16));
    }
    last
}

#[test]
#[ignore] // バンドル済みサーバー + CEF シングルトン制約のため明示実行のみ
fn oversize_viewport_paints_at_full_size() {
    let server = TestCefServer::start();

    // 縦 2400px (> MAX_H 2160) で作成 — GPU 経路は clamp してはならない
    let (browser_id, shared_memory_flink) = match server.send(Command::CreateBrowser {
        width: 1200,
        height: 2400,
        url: "about:blank".to_string(),
    }) {
        Response::BrowserCreated {
            browser_id,
            shared_memory_flink,
            ..
        } => (browser_id, shared_memory_flink),
        other => panic!("expected BrowserCreated, got {:?}", other),
    };
    let shared_memory = SharedMemoryReader::open(&shared_memory_flink).expect("open shared_memory");

    let (width, height) = wait_for_accelerated_dimensions(&server, &shared_memory, browser_id, 1200, 2400, Duration::from_secs(15));
    assert_eq!(
        (width, height),
        (1200, 2400),
        "accel paint 寸法が申告 viewport と一致すべき (clamp があると 2160 になる)"
    );

    // Resize でも同様 (実バグの再現経路: Play 中のウィンドウリサイズ)
    server.fire(Command::Resize {
        browser_id,
        width: 1000,
        height: 2600,
    });
    let (width, height) = wait_for_accelerated_dimensions(&server, &shared_memory, browser_id, 1000, 2600, Duration::from_secs(15));
    assert_eq!(
        (width, height),
        (1000, 2600),
        "Resize 後の accel paint 寸法も申告サイズに追従すべき"
    );

    server.send(Command::Shutdown);
}
