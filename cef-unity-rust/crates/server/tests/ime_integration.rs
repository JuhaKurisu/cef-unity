//! 統合テスト: IME API が実際に CEF ブラウザの input フィールドにテキストを入力できるか検証する。
//!
//! 事前準備:
//!   cargo build
//!   bash build-server-sandbox.sh target/debug/test-bundle
//!
//! 実行:
//!   cargo test -p cef-unity-server --test ime_integration -- --ignored

use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ipc_channel::ipc::{self, IpcOneShotServer};

use cef_unity_ipc::{Bootstrap, Command, CommandEnvelope, Response};

// ---------------------------------------------------------------------------
// ローカル HTTP サーバー
// ---------------------------------------------------------------------------

/// テスト用の最小 HTTP サーバー。
/// GET / → input フィールドを持つ HTML
/// GET /value → JS から POST された input の値を返す
struct TestHttpServer {
    port: u16,
    value: Arc<Mutex<Option<String>>>,
    _handle: thread::JoinHandle<()>,
}

impl TestHttpServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let value: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let value_clone = Arc::clone(&value);

        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }

                // Content-Length を読む
                let mut content_length = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                    if let Some(val) = header.strip_prefix("Content-Length: ") {
                        content_length = val.trim().parse().unwrap_or(0);
                    }
                }

                if request_line.starts_with("GET / ") {
                    let html = r#"<!DOCTYPE html>
<html><body>
<input id="t" type="text" style="font-size:24px;width:400px;">
</body></html>"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
                        html.len(), html
                    );
                    let _ = stream.write_all(resp.as_bytes());
                } else if request_line.starts_with("POST /value") {
                    let mut body = vec![0u8; content_length];
                    let _ = reader.read_exact(&mut body);
                    let body_str = String::from_utf8_lossy(&body).to_string();
                    *value_clone.lock().unwrap() = Some(body_str);
                    let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nAccess-Control-Allow-Origin: *\r\n\r\nOK";
                    let _ = stream.write_all(resp.as_bytes());
                } else {
                    let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
                    let _ = stream.write_all(resp.as_bytes());
                }
            }
        });

        TestHttpServer {
            port,
            value,
            _handle: handle,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    fn take_value(&self) -> Option<String> {
        self.value.lock().unwrap().take()
    }
}

// ---------------------------------------------------------------------------
// CEF サーバーヘルパー
// ---------------------------------------------------------------------------

struct TestCefServer {
    cmd_tx: ipc::IpcSender<CommandEnvelope>,
    resp_rx: ipc::IpcReceiver<Response>,
    child: std::process::Child,
}

impl TestCefServer {
    fn start() -> Self {
        let server_app = find_server_app().expect(
            "バンドル済みサーバーが見つかりません。\n\
             以下を実行してください:\n  \
             cargo build\n  \
             bash build-server-sandbox.sh target/debug/test-bundle",
        );
        let server_bin = server_app.join("Contents/MacOS/cef-unity-server");
        assert!(server_bin.exists(), "{}", server_bin.display());

        let (oneshot_server, server_name) =
            IpcOneShotServer::<Bootstrap>::new().expect("one-shot server");

        let child = std::process::Command::new(&server_bin)
            .arg("--ipc-server")
            .arg(&server_name)
            .spawn()
            .unwrap_or_else(|e| panic!("{}: {}", server_bin.display(), e));

        let (_rx, bootstrap) = oneshot_server.accept().expect("bootstrap");

        TestCefServer {
            cmd_tx: bootstrap.cmd_tx,
            resp_rx: bootstrap.resp_rx,
            child,
        }
    }

    fn send(&self, cmd: Command) -> Response {
        self.cmd_tx
            .send(CommandEnvelope {
                command: cmd,
                expects_response: true,
            })
            .unwrap();
        self.resp_rx.recv().unwrap()
    }

    fn fire(&self, cmd: Command) {
        self.cmd_tx
            .send(CommandEnvelope {
                command: cmd,
                expects_response: false,
            })
            .unwrap();
    }

    /// ブラウザ作成 → ページロード → クリックフォーカス
    fn setup_browser(&self, url: &str) -> u32 {
        let browser_id = match self.send(Command::CreateBrowser {
            width: 800,
            height: 600,
            url: url.to_string(),
        }) {
            Response::BrowserCreated { browser_id, .. } => browser_id,
            other => panic!("expected BrowserCreated, got {:?}", other),
        };

        // ページロード + 描画待ち
        thread::sleep(Duration::from_secs(3));

        // input をクリックしてフォーカス
        self.fire(Command::MouseClick {
            browser_id, x: 200, y: 20, modifiers: 0,
            button: 0, mouse_up: false, click_count: 1,
        });
        thread::sleep(Duration::from_millis(50));
        self.fire(Command::MouseClick {
            browser_id, x: 200, y: 20, modifiers: 0,
            button: 0, mouse_up: true, click_count: 1,
        });
        thread::sleep(Duration::from_millis(500));

        browser_id
    }

    /// JS で input の値を HTTP サーバーへ POST する。
    fn post_input_value(&self, browser_id: u32, http_port: u16) {
        let js = format!(
            r#"(function(){{
                var v = document.getElementById('t').value;
                var xhr = new XMLHttpRequest();
                xhr.open('POST', 'http://127.0.0.1:{}/value', false);
                xhr.send(v);
            }})()"#,
            http_port
        );
        self.fire(Command::ExecuteJavaScript {
            browser_id,
            code: js,
        });
        thread::sleep(Duration::from_millis(500));
    }

    fn shutdown(mut self) {
        let _ = self.send(Command::Shutdown);
        thread::sleep(Duration::from_millis(500));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find_server_app() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CEF_SERVER_APP") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Some(p);
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
            .join("cef-unity-csharp/Interop/cef-unity-server.app"),
    ];

    candidates.into_iter().find(|p| p.exists())
}

// ---------------------------------------------------------------------------
// テストケース
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn ime_set_composition_then_commit() {
    let http = TestHttpServer::start();
    let cef = TestCefServer::start();
    let bid = cef.setup_browser(&http.url());

    cef.fire(Command::ImeSetComposition {
        browser_id: bid, text: "漢字".to_string(),
        selection_start: 0, selection_end: 2,
    });
    thread::sleep(Duration::from_millis(200));

    cef.fire(Command::ImeCommitText {
        browser_id: bid, text: "漢字".to_string(),
    });
    thread::sleep(Duration::from_millis(500));

    cef.post_input_value(bid, http.port);
    let value = http.take_value().unwrap_or_default();
    assert_eq!(value, "漢字", "SetComposition → CommitText");

    cef.shutdown();
}

#[test]
#[ignore]
fn ime_set_composition_then_finish() {
    let http = TestHttpServer::start();
    let cef = TestCefServer::start();
    let bid = cef.setup_browser(&http.url());

    cef.fire(Command::ImeSetComposition {
        browser_id: bid, text: "テスト".to_string(),
        selection_start: 0, selection_end: 3,
    });
    thread::sleep(Duration::from_millis(200));

    cef.fire(Command::ImeFinishComposingText {
        browser_id: bid, keep_selection: false,
    });
    thread::sleep(Duration::from_millis(500));

    cef.post_input_value(bid, http.port);
    let value = http.take_value().unwrap_or_default();
    assert_eq!(value, "テスト", "SetComposition → FinishComposingText");

    cef.shutdown();
}

#[test]
#[ignore]
fn ime_set_composition_then_cancel() {
    let http = TestHttpServer::start();
    let cef = TestCefServer::start();
    let bid = cef.setup_browser(&http.url());

    cef.fire(Command::ImeSetComposition {
        browser_id: bid, text: "入力中".to_string(),
        selection_start: 0, selection_end: 3,
    });
    thread::sleep(Duration::from_millis(200));

    cef.fire(Command::ImeCancelComposition { browser_id: bid });
    thread::sleep(Duration::from_millis(500));

    cef.post_input_value(bid, http.port);
    let value = http.take_value().unwrap_or_default();
    assert_eq!(value, "", "cancel should leave input empty");

    cef.shutdown();
}

#[test]
#[ignore]
fn ime_commit_text_standalone() {
    let http = TestHttpServer::start();
    let cef = TestCefServer::start();
    let bid = cef.setup_browser(&http.url());

    cef.fire(Command::ImeCommitText {
        browser_id: bid, text: "直接入力".to_string(),
    });
    thread::sleep(Duration::from_millis(500));

    cef.post_input_value(bid, http.port);
    let value = http.take_value().unwrap_or_default();
    assert_eq!(value, "直接入力", "standalone CommitText");

    cef.shutdown();
}

#[test]
#[ignore]
fn ime_sequential_inputs() {
    let http = TestHttpServer::start();
    let cef = TestCefServer::start();
    let bid = cef.setup_browser(&http.url());

    // 1回目
    cef.fire(Command::ImeSetComposition {
        browser_id: bid, text: "東".to_string(),
        selection_start: 0, selection_end: 1,
    });
    thread::sleep(Duration::from_millis(100));
    cef.fire(Command::ImeCommitText {
        browser_id: bid, text: "東".to_string(),
    });
    thread::sleep(Duration::from_millis(300));

    // 2回目
    cef.fire(Command::ImeSetComposition {
        browser_id: bid, text: "京".to_string(),
        selection_start: 0, selection_end: 1,
    });
    thread::sleep(Duration::from_millis(100));
    cef.fire(Command::ImeCommitText {
        browser_id: bid, text: "京".to_string(),
    });
    thread::sleep(Duration::from_millis(500));

    cef.post_input_value(bid, http.port);
    let value = http.take_value().unwrap_or_default();
    assert_eq!(value, "東京", "sequential inputs should concatenate");

    cef.shutdown();
}
