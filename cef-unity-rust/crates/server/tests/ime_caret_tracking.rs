//! 統合テスト: IME キャレット位置 (SHM ime_caret) がユーザー操作に追従するか検証する。
//!
//! 背景: OSR では OS の IME 候補ウィンドウ位置を Unity 側が
//! `Input.compositionCursorPos` で指定する必要があり、その座標源は
//! SHM ヘッダの ime_caret (server が書き込む) のみ。
//! 書き込み元は 2 つ:
//!   1. on_ime_composition_range_changed (composition 中のみ)
//!   2. ページに注入する CARET_TRACKING_JS (click / selectionchange など)
//! `window.getSelection()` は input/textarea 内部のキャレットを返さないため、
//! JS 側にテキストコントロール対応がないとクリック直後の初回 IME 表示位置が
//! 古い位置のままになる (= 「IME の位置があっていない」問題)。
//!
//! 事前準備:
//!   cargo build
//!   bash build-server-sandbox.sh target/debug/test-bundle
//!
//! 実行:
//!   cargo test -p cef-unity-server --test ime_caret_tracking -- --ignored --test-threads=1

use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ipc_channel::ipc::{self, IpcOneShotServer};

use cef_unity_ipc::{Bootstrap, Command, CommandEnvelope, Response, ShmReader};

// ---------------------------------------------------------------------------
// CEF サーバーは同時に 1 つしか起動できないため、テストを直列化する。
// ---------------------------------------------------------------------------

static TEST_MUTEX: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// ローカル HTTP サーバー (テストページ配信のみ)
// ---------------------------------------------------------------------------

/// レイアウトを固定したテストページ。
/// - #t1: input (上部, top=0)
/// - #t2: input (中央, top=300)
/// - #ce: contenteditable (下部, top=500, テキスト入り)
const TEST_HTML: &str = r#"<!DOCTYPE html>
<html><body style="margin:0">
<input id="t1" type="text" style="position:absolute;left:0;top:0;width:400px;height:36px;margin:0;font-size:24px;box-sizing:border-box;">
<input id="t2" type="text" style="position:absolute;left:0;top:300px;width:400px;height:36px;margin:0;font-size:24px;box-sizing:border-box;">
<div id="ce" contenteditable="true" style="position:absolute;left:0;top:500px;width:400px;height:36px;margin:0;font-size:24px;">あいうえお</div>
</body></html>"#;

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
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
                        TEST_HTML.len(),
                        TEST_HTML
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

    /// ブラウザ作成 → ページロード待ち。SHM リーダーも開く。
    ///
    /// external_begin_frame_enabled=1 で作られるため、Unity と同様に
    /// SendExternalBeginFrame を定期送信しないと rAF アラインの入力
    /// (マウスクリック等) がレンダラーでディスパッチされない。
    /// ここで 60Hz のポンプスレッドを起動して実環境を模す。
    fn setup_browser(&self, url: &str) -> (u32, ShmReader) {
        let (browser_id, shm_flink) = match self.send(Command::CreateBrowser {
            width: 800,
            height: 600,
            url: url.to_string(),
        }) {
            Response::BrowserCreated {
                browser_id,
                shm_flink,
                ..
            } => (browser_id, shm_flink),
            other => panic!("expected BrowserCreated, got {:?}", other),
        };

        // BeginFrame ポンプ (Unity の毎フレーム送信を模す)。
        // Shutdown 後は send が失敗してスレッドが自然終了する。
        let tx = self.cmd_tx.clone();
        thread::spawn(move || {
            let mut frame: u64 = 1;
            loop {
                let env = CommandEnvelope {
                    command: Command::SendExternalBeginFrame {
                        browser_id,
                        unity_frame: frame,
                    },
                    expects_response: false,
                };
                if tx.send(env).is_err() {
                    break;
                }
                frame += 1;
                thread::sleep(Duration::from_millis(16));
            }
        });

        // ページロード + CARET_TRACKING_JS 注入 (on_load_end) 待ち
        thread::sleep(Duration::from_secs(3));

        let shm = ShmReader::open(&shm_flink).expect("open shm");
        (browser_id, shm)
    }

    /// 指定座標をクリック (down→up)。
    fn click(&self, browser_id: u32, x: i32, y: i32) {
        self.fire(Command::MouseClick {
            browser_id,
            x,
            y,
            modifiers: 0,
            button: 0,
            mouse_up: false,
            click_count: 1,
        });
        thread::sleep(Duration::from_millis(50));
        self.fire(Command::MouseClick {
            browser_id,
            x,
            y,
            modifiers: 0,
            button: 0,
            mouse_up: true,
            click_count: 1,
        });
        // click ハンドラ (setTimeout 0) → console.log → on_console_message → shm 書込 待ち
        thread::sleep(Duration::from_millis(700));
    }

    /// JS 式を評価して結果 (文字列化) を HTTP サーバーへ POST させ、その値を返す。
    /// ページ内の状態 (activeElement, selection など) の観測用。
    fn eval_js(&self, browser_id: u32, http: &TestHttpServer, expr: &str) -> String {
        let js = format!(
            r#"(function(){{
                var v;
                try {{ v = String({expr}); }} catch(e) {{ v = "ERR:" + e; }}
                var xhr = new XMLHttpRequest();
                xhr.open('POST', 'http://127.0.0.1:{}/value', false);
                xhr.send(v);
            }})()"#,
            http.port,
        );
        self.fire(Command::ExecuteJavaScript {
            browser_id,
            code: js,
        });
        thread::sleep(Duration::from_millis(500));
        http.take_value().unwrap_or_else(|| "<no response>".to_string())
    }

    /// 明示的な終了。実際の後始末は Drop が行う。
    fn shutdown(self) {}
}

/// panic 時にもサーバーを確実に終了させる。プロセスが残ると CEF の
/// シングルトンロックと衝突し、次のテストの CEF 起動がハングする。
impl Drop for TestCefServer {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(CommandEnvelope {
            command: Command::Shutdown,
            expects_response: false,
        });
        thread::sleep(Duration::from_millis(500));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Mutex が前のテストの panic で poison されていても続行する。
fn lock_serial() -> std::sync::MutexGuard<'static, ()> {
    TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
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

/// 診断: パイプラインの各境界を個別に検証する。
///   1. CARET_TRACKING_JS が注入されているか
///   2. クリックがフォーカス/キャレット配置に効いているか
///   3. getSelection() の rect が取れているか
///   4. console.log("__CARET__:...") → on_console_message → SHM 書込みが機能するか
#[test]
#[ignore]
fn diagnose_caret_pipeline() {
    let _lock = lock_serial();
    let http = TestHttpServer::start();
    let cef = TestCefServer::start();
    let (bid, shm) = cef.setup_browser(&http.url());

    // 境界 1: トラッカー注入確認
    let injected = cef.eval_js(bid, &http, "window.__cefUnityCaretTracker");
    println!("[1] tracker injected: {}", injected);

    // 境界 4 (先に確認): console.log → SHM 経路
    cef.fire(Command::ExecuteJavaScript {
        browser_id: bid,
        code: "console.log('__CARET__:11:22:0:33');".to_string(),
    });
    thread::sleep(Duration::from_millis(700));
    let (x, y, w, h) = shm.read_ime_caret();
    println!("[4] console->shm: ({}, {}, {}, {})", x, y, w, h);

    // 境界 2: クリックでフォーカスが移るか
    cef.click(bid, 60, 518);
    let active = cef.eval_js(bid, &http, "document.activeElement && document.activeElement.id");
    println!("[2] activeElement after ce click: {}", active);

    // 境界 3: selection rect
    let sel = cef.eval_js(
        bid,
        &http,
        r#"(function(){
            var s = window.getSelection();
            if (!s || s.rangeCount === 0) return "no-range";
            var r = s.getRangeAt(0).cloneRange();
            r.collapse(false);
            var b = r.getBoundingClientRect();
            return b.x + "," + b.y + "," + b.width + "," + b.height;
        })()"#,
    );
    println!("[3] selection rect after ce click: {}", sel);

    let (x2, y2, w2, h2) = shm.read_ime_caret();
    println!("[final] shm caret: ({}, {}, {}, {})", x2, y2, w2, h2);

    cef.shutdown();
}

/// ハーネス検証: テキスト入りの contenteditable クリックでキャレットが報告される
/// (getSelection() ベースの既存トラッカーで動くはずの経路)。
#[test]
#[ignore]
fn caret_reported_on_click_in_contenteditable() {
    let _lock = lock_serial();
    let http = TestHttpServer::start();
    let cef = TestCefServer::start();
    let (bid, shm) = cef.setup_browser(&http.url());

    // "あいうえお" の途中 (x=60) をクリック → キャレットは y∈[500,536] 付近のはず
    cef.click(bid, 60, 518);

    let (x, y, w, h) = shm.read_ime_caret();
    println!("contenteditable caret: x={} y={} w={} h={}", x, y, w, h);
    assert!(
        (500..=560).contains(&y) && h > 0,
        "contenteditable クリックでキャレットが報告されるべき: got ({}, {}, {}, {})",
        x,
        y,
        w,
        h
    );

    cef.shutdown();
}

/// 再現テスト 1: 空の input フィールドをクリックしたらキャレット位置が報告されるべき。
/// getSelection() はテキストコントロール内部のキャレットを返さないため、
/// テキストコントロール対応がないと SHM は (0,0,0,0) のまま。
#[test]
#[ignore]
fn caret_reported_on_click_in_input() {
    let _lock = lock_serial();
    let http = TestHttpServer::start();
    let cef = TestCefServer::start();
    let (bid, shm) = cef.setup_browser(&http.url());

    // 上部の input #t1 (top=0, height=36) をクリック
    cef.click(bid, 200, 18);

    let (x, y, w, h) = shm.read_ime_caret();
    println!("input caret: x={} y={} w={} h={}", x, y, w, h);
    assert!(
        !(x == 0 && y == 0 && w == 0 && h == 0),
        "input クリックでキャレットが報告されるべきだが SHM が未更新 (0,0,0,0)"
    );
    assert!(
        (0..=45).contains(&y) && h > 0,
        "キャレット y は input #t1 の範囲 [0,45] にあるべき: got ({}, {}, {}, {})",
        x,
        y,
        w,
        h
    );

    cef.shutdown();
}

/// 再現テスト 2 (ユーザー可視のバグそのもの): input #t1 で IME 入力・確定した後、
/// 別の input #t2 をクリックしたら、キャレット位置は #t2 に移動するべき。
/// 現状は on_ime_composition_range_changed が書いた #t1 の位置が残り続け、
/// 次の IME 候補ウィンドウが前のフィールド位置に表示される。
#[test]
#[ignore]
fn caret_follows_focus_change_to_second_input() {
    let _lock = lock_serial();
    let http = TestHttpServer::start();
    let cef = TestCefServer::start();
    let (bid, shm) = cef.setup_browser(&http.url());

    // #t1 をクリックして IME 入力 → 確定
    cef.click(bid, 200, 18);
    cef.fire(Command::ImeSetComposition {
        browser_id: bid,
        text: "かん".to_string(),
        selection_start: 2,
        selection_end: 2,
    });
    thread::sleep(Duration::from_millis(300));
    cef.fire(Command::ImeCommitText {
        browser_id: bid,
        text: "感".to_string(),
    });
    thread::sleep(Duration::from_millis(500));

    let (_, y1, _, _) = shm.read_ime_caret();
    println!("caret after composition in #t1: y={}", y1);

    // #t2 (top=300) をクリック → キャレットは #t2 へ移動するべき
    cef.click(bid, 200, 318);

    let (x, y, w, h) = shm.read_ime_caret();
    println!("caret after click on #t2: x={} y={} w={} h={}", x, y, w, h);
    assert!(
        (300..=345).contains(&y),
        "#t2 クリック後のキャレット y は [300,345] にあるべき (前のフィールド位置が残っている): got ({}, {}, {}, {})",
        x,
        y,
        w,
        h
    );

    cef.shutdown();
}
