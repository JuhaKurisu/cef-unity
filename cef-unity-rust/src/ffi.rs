// FFI layer for Unity C# interop.
//
// This is now a pure IPC client — no CEF dependency.
// Communicates with cef-unity-server via Unix domain socket + shared memory.

use std::ffi::{CStr, c_char};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::ipc::{self, Command, Response, ShmReader};

// ---------------------------------------------------------------------------
// Opaque handle type (becomes IntPtr in C#)
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct CefUnityBrowser {
    _opaque: u8,
}

// ---------------------------------------------------------------------------
// Per-browser client state
// ---------------------------------------------------------------------------

struct ClientBrowserInstance {
    browser_id: u32,
    shm: ShmReader,
    front: Vec<u8>,
    front_w: i32,
    front_h: i32,
}

fn handle_to_ref<'a>(handle: *mut CefUnityBrowser) -> &'a mut ClientBrowserInstance {
    unsafe { &mut *(handle as *mut ClientBrowserInstance) }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static PAINT_COUNT: AtomicU64 = AtomicU64::new(0);
static PUMP_COUNT: AtomicU64 = AtomicU64::new(0);

struct ServerConnection {
    stream: UnixStream,
    socket_path: String,
}

static CONNECTION: Mutex<Option<ServerConnection>> = Mutex::new(None);

fn log_to_file(msg: &str) {
    let path = std::env::temp_dir().join("cef_unity_debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{:?}] {}", std::time::SystemTime::now(), msg);
    }
}

// ---------------------------------------------------------------------------
// IPC helpers
// ---------------------------------------------------------------------------

fn send_command(stream: &mut UnixStream, cmd: &Command) -> std::io::Result<Response> {
    let payload = cmd.serialize();
    ipc::send_message(stream, &payload)?;
    let resp_data = ipc::recv_message(stream)?;
    Response::deserialize(&resp_data)
}

// ---------------------------------------------------------------------------
// Global functions
// ---------------------------------------------------------------------------

/// Initialize: launch CEF server process and connect via Unix domain socket.
/// Returns 0 on success, non-zero on failure.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_init() -> i32 {
    if INITIALIZED.load(Ordering::SeqCst) {
        return 0;
    }

    let _ = std::fs::write(std::env::temp_dir().join("cef_unity_debug.log"), "");
    log_to_file("cef_unity_init() called (IPC client mode)");

    // Find server .app next to dylib
    let plugin_dir = crate::dylib_dir();
    let server_app = plugin_dir.join("cef-unity-server.app/Contents/MacOS/cef-unity-server");
    if !server_app.exists() {
        log_to_file(&format!("server binary not found: {}", server_app.display()));
        return -3;
    }
    log_to_file(&format!("server binary: {}", server_app.display()));

    // Socket path in temp dir
    let socket_path = std::env::temp_dir()
        .join(format!("cef-unity-{}.sock", std::process::id()))
        .to_str()
        .unwrap()
        .to_string();
    log_to_file(&format!("socket_path = {}", socket_path));

    // Launch server process directly
    match std::process::Command::new(&server_app)
        .arg("--socket-path")
        .arg(&socket_path)
        .spawn()
    {
        Ok(_child) => {
            // Server runs independently; we don't track its PID
        }
        Err(e) => {
            log_to_file(&format!("failed to spawn server: {}", e));
            return -4;
        }
    }
    log_to_file("server spawned");

    // Retry loop to connect to server socket
    let mut stream = None;
    for attempt in 0..100 {
        match UnixStream::connect(&socket_path) {
            Ok(s) => {
                log_to_file(&format!("connected to server on attempt {}", attempt));
                stream = Some(s);
                break;
            }
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    let stream = match stream {
        Some(s) => s,
        None => {
            log_to_file("failed to connect to server after retries");
            return -5;
        }
    };

    *CONNECTION.lock().unwrap() = Some(ServerConnection {
        stream,
        socket_path,
    });

    INITIALIZED.store(true, Ordering::SeqCst);
    log_to_file("initialized successfully (IPC client)");
    0
}

/// Pump CEF message loop — no-op in IPC mode (server has its own loop).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_pump() {
    PUMP_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Returns the number of on_paint calls (tracked per-frame reads in IPC mode).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_paint_count() -> u64 {
    PAINT_COUNT.load(Ordering::Relaxed)
}

/// Returns the number of pump iterations.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_pump_count() -> u64 {
    PUMP_COUNT.load(Ordering::Relaxed)
}

/// Shut down: send Shutdown command and wait for server to exit.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_shutdown() {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return;
    }
    log_to_file("cef_unity_shutdown()");

    if let Some(mut conn) = CONNECTION.lock().unwrap().take() {
        let _ = send_command(&mut conn.stream, &Command::Shutdown);
        // Server will exit after receiving Shutdown; give it a moment
        std::thread::sleep(std::time::Duration::from_millis(500));
        let _ = std::fs::remove_file(&conn.socket_path);
    }

    INITIALIZED.store(false, Ordering::SeqCst);
    log_to_file("shutdown complete");
}

// ---------------------------------------------------------------------------
// Per-browser functions
// ---------------------------------------------------------------------------

/// Create a browser instance via IPC.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_create_browser(
    width: i32,
    height: i32,
    url: *const c_char,
) -> *mut CefUnityBrowser {
    if !INITIALIZED.load(Ordering::SeqCst) || url.is_null() {
        return std::ptr::null_mut();
    }

    let url_str = unsafe { CStr::from_ptr(url) }.to_str().unwrap_or("");
    log_to_file(&format!(
        "cef_unity_create_browser({}x{}, {})",
        width, height, url_str
    ));

    let mut guard = CONNECTION.lock().unwrap();
    let conn = match guard.as_mut() {
        Some(c) => c,
        None => return std::ptr::null_mut(),
    };

    let cmd = Command::CreateBrowser {
        width,
        height,
        url: url_str.to_string(),
    };
    let resp = match send_command(&mut conn.stream, &cmd) {
        Ok(r) => r,
        Err(e) => {
            log_to_file(&format!("create_browser IPC error: {}", e));
            return std::ptr::null_mut();
        }
    };

    match resp {
        Response::BrowserCreated {
            browser_id,
            shm_name,
        } => {
            log_to_file(&format!(
                "browser created: id={}, shm={}",
                browser_id, shm_name
            ));
            let shm = match ShmReader::open(&shm_name) {
                Ok(s) => s,
                Err(e) => {
                    log_to_file(&format!("shm_open failed: {}", e));
                    return std::ptr::null_mut();
                }
            };
            let instance = Box::new(ClientBrowserInstance {
                browser_id,
                shm,
                front: Vec::new(),
                front_w: 0,
                front_h: 0,
            });
            Box::into_raw(instance) as *mut CefUnityBrowser
        }
        Response::Error { msg } => {
            log_to_file(&format!("create_browser error: {}", msg));
            std::ptr::null_mut()
        }
        _ => {
            log_to_file("unexpected response to CreateBrowser");
            std::ptr::null_mut()
        }
    }
}

/// Destroy a browser instance.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_destroy_browser(handle: *mut CefUnityBrowser) {
    if handle.is_null() {
        return;
    }
    let instance = unsafe { Box::from_raw(handle as *mut ClientBrowserInstance) };

    let mut guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_mut() {
        let cmd = Command::DestroyBrowser {
            browser_id: instance.browser_id,
        };
        let _ = send_command(&mut conn.stream, &cmd);
    }
    drop(instance);
}

/// Load a URL in the browser.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_load_url(handle: *mut CefUnityBrowser, url: *const c_char) {
    if handle.is_null() || url.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);
    let url_str = unsafe { CStr::from_ptr(url) }.to_str().unwrap_or("");

    let mut guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_mut() {
        let cmd = Command::LoadUrl {
            browser_id: instance.browser_id,
            url: url_str.to_string(),
        };
        let _ = send_command(&mut conn.stream, &cmd);
    }
}

/// Resize the browser viewport.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_resize(handle: *mut CefUnityBrowser, width: i32, height: i32) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);

    let mut guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_mut() {
        let cmd = Command::Resize {
            browser_id: instance.browser_id,
            width,
            height,
        };
        let _ = send_command(&mut conn.stream, &cmd);
    }
}

/// Get the latest frame buffer from shared memory.
/// Returns 1 if a new frame is available, 0 if unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_buffer(
    handle: *mut CefUnityBrowser,
    out_buffer: *mut *const u8,
    out_width: *mut i32,
    out_height: *mut i32,
) -> i32 {
    if handle.is_null() || out_buffer.is_null() || out_width.is_null() || out_height.is_null() {
        return 0;
    }
    let instance = handle_to_ref(handle);

    let new_frame = instance.shm.read_frame(&mut instance.front);
    if let Some((w, h)) = new_frame {
        instance.front_w = w as i32;
        instance.front_h = h as i32;
        PAINT_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    unsafe {
        *out_buffer = instance.front.as_ptr();
        *out_width = instance.front_w;
        *out_height = instance.front_h;
    }
    new_frame.is_some() as i32
}
