// FFI layer for Unity C# interop.
//
// This is a pure IPC client — no CEF dependency.
// Communicates with cef-unity-server via ipc-channel + shared memory.

use std::ffi::{CStr, c_char};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ipc_channel::ipc::{IpcOneShotServer, IpcReceiver, IpcSender};

use cef_unity_ipc::{Bootstrap, Command, CommandEnvelope, Response, ShmReader};

// ---------------------------------------------------------------------------
// dylib location helpers
// ---------------------------------------------------------------------------

/// dylib/DLL 自身のディレクトリを返す。
fn dylib_dir() -> PathBuf {
    let info = dl_info().expect("failed to locate dylib/DLL");
    PathBuf::from(info).parent().unwrap().to_path_buf()
}

/// Unix: dladdr で共有ライブラリのパスを取得する。
#[cfg(unix)]
fn dl_info() -> Option<String> {
    unsafe extern "C" {
        fn dladdr(addr: *const u8, info: *mut DlInfo) -> i32;
    }
    #[repr(C)]
    struct DlInfo {
        dli_fname: *const std::ffi::c_char,
        dli_fbase: *const u8,
        dli_sname: *const std::ffi::c_char,
        dli_saddr: *const u8,
    }
    let mut info: DlInfo = unsafe { std::mem::zeroed() };
    let ret = unsafe { dladdr(dylib_dir as *const u8, &mut info) };
    if ret == 0 || info.dli_fname.is_null() {
        return None;
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) };
    Some(cstr.to_str().ok()?.to_string())
}

/// Windows: GetModuleHandleExW + GetModuleFileNameW で DLL のパスを取得する。
#[cfg(windows)]
fn dl_info() -> Option<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x00000004;
    const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x00000002;

    type HMODULE = *mut std::ffi::c_void;
    type BOOL = i32;
    type DWORD = u32;
    type LPCWSTR = *const u16;
    type LPWSTR = *mut u16;

    unsafe extern "system" {
        fn GetModuleHandleExW(
            dwFlags: DWORD,
            lpModuleName: LPCWSTR,
            phModule: *mut HMODULE,
        ) -> BOOL;
        fn GetModuleFileNameW(hModule: HMODULE, lpFilename: LPWSTR, nSize: DWORD) -> DWORD;
    }

    let mut hmodule: HMODULE = std::ptr::null_mut();
    let flags =
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    let ret = unsafe { GetModuleHandleExW(flags, dylib_dir as *const u16, &mut hmodule) };
    if ret == 0 || hmodule.is_null() {
        return None;
    }

    let mut buf = vec![0u16; 4096];
    let len = unsafe { GetModuleFileNameW(hmodule, buf.as_mut_ptr(), buf.len() as DWORD) };
    if len == 0 {
        return None;
    }
    let os_str = OsString::from_wide(&buf[..len as usize]);
    os_str.into_string().ok()
}

/// サーバーバイナリのパスを返す。
#[cfg(target_os = "macos")]
fn server_binary_path(plugin_dir: &std::path::Path) -> PathBuf {
    plugin_dir.join("cef-unity-server.app/Contents/MacOS/cef-unity-server")
}

#[cfg(target_os = "linux")]
fn server_binary_path(plugin_dir: &std::path::Path) -> PathBuf {
    plugin_dir.join("cef-unity-server")
}

#[cfg(target_os = "windows")]
fn server_binary_path(plugin_dir: &std::path::Path) -> PathBuf {
    plugin_dir.join("cef-unity-server.exe")
}

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
    cmd_tx: IpcSender<CommandEnvelope>,
    resp_rx: IpcReceiver<Response>,
}

static CONNECTION: Mutex<Option<ServerConnection>> = Mutex::new(None);

fn log_to_file(msg: &str) {
    let path = std::env::temp_dir().join("cef_unity_debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[{:?}] {}", std::time::SystemTime::now(), msg);
    }
}

// ---------------------------------------------------------------------------
// IPC helpers
// ---------------------------------------------------------------------------

fn send_command(conn: &ServerConnection, cmd: Command) -> Result<Response, String> {
    conn.cmd_tx
        .send(CommandEnvelope {
            command: cmd,
            expects_response: true,
        })
        .map_err(|e| format!("send: {}", e))?;
    conn.resp_rx.recv().map_err(|e| format!("recv: {}", e))
}

/// Fire-and-forget: send only, don't wait for response.
fn send_command_no_wait(conn: &ServerConnection, cmd: Command) {
    let _ = conn.cmd_tx.send(CommandEnvelope {
        command: cmd,
        expects_response: false,
    });
}

// ---------------------------------------------------------------------------
// Global functions
// ---------------------------------------------------------------------------

/// Initialize: launch CEF server process and connect via ipc-channel.
/// Returns 0 on success, non-zero on failure.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_init() -> i32 {
    if INITIALIZED.load(Ordering::SeqCst) {
        return 0;
    }

    let _ = std::fs::write(std::env::temp_dir().join("cef_unity_debug.log"), "");
    log_to_file("cef_unity_init() called (IPC client mode)");

    // Find server binary next to dylib
    let plugin_dir = dylib_dir();
    let server_app = server_binary_path(&plugin_dir);
    if !server_app.exists() {
        log_to_file(&format!(
            "server binary not found: {}",
            server_app.display()
        ));
        return -3;
    }
    log_to_file(&format!("server binary: {}", server_app.display()));

    // Create one-shot server for bootstrap
    let (oneshot_server, server_name) = match IpcOneShotServer::<Bootstrap>::new() {
        Ok(pair) => pair,
        Err(e) => {
            log_to_file(&format!("failed to create one-shot server: {}", e));
            return -4;
        }
    };
    log_to_file(&format!("one-shot server name = {}", server_name));

    // Launch server process with --ipc-server argument
    match std::process::Command::new(&server_app)
        .arg("--ipc-server")
        .arg(&server_name)
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

    // Wait for server to connect and send bootstrap (blocking)
    let bootstrap = match oneshot_server.accept() {
        Ok((_rx, bootstrap)) => bootstrap,
        Err(e) => {
            log_to_file(&format!("failed to accept bootstrap: {}", e));
            return -5;
        }
    };
    log_to_file("bootstrap received from server");

    *CONNECTION.lock().unwrap() = Some(ServerConnection {
        cmd_tx: bootstrap.cmd_tx,
        resp_rx: bootstrap.resp_rx,
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

    if let Some(conn) = CONNECTION.lock().unwrap().take() {
        let _ = send_command(&conn, Command::Shutdown);
        // Server will exit after receiving Shutdown; give it a moment
        std::thread::sleep(std::time::Duration::from_millis(500));
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

    let guard = CONNECTION.lock().unwrap();
    let conn = match guard.as_ref() {
        Some(c) => c,
        None => return std::ptr::null_mut(),
    };

    let cmd = Command::CreateBrowser {
        width,
        height,
        url: url_str.to_string(),
    };
    let resp = match send_command(conn, cmd) {
        Ok(r) => r,
        Err(e) => {
            log_to_file(&format!("create_browser IPC error: {}", e));
            return std::ptr::null_mut();
        }
    };

    match resp {
        Response::BrowserCreated {
            browser_id,
            shm_flink,
        } => {
            log_to_file(&format!(
                "browser created: id={}, shm={}",
                browser_id, shm_flink
            ));
            let shm = match ShmReader::open(&shm_flink) {
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

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        let cmd = Command::DestroyBrowser {
            browser_id: instance.browser_id,
        };
        send_command_no_wait(conn, cmd);
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

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        let cmd = Command::LoadUrl {
            browser_id: instance.browser_id,
            url: url_str.to_string(),
        };
        send_command_no_wait(conn, cmd);
    }
}

/// Resize the browser viewport.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_resize(handle: *mut CefUnityBrowser, width: i32, height: i32) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        let cmd = Command::Resize {
            browser_id: instance.browser_id,
            width,
            height,
        };
        send_command_no_wait(conn, cmd);
    }
}

/// Send a mouse move event.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_mouse_move(
    handle: *mut CefUnityBrowser,
    x: i32,
    y: i32,
    modifiers: u32,
) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(conn, Command::MouseMove {
            browser_id: instance.browser_id,
            x,
            y,
            modifiers,
        });
    }
}

/// Send a mouse click event.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_mouse_click(
    handle: *mut CefUnityBrowser,
    x: i32,
    y: i32,
    modifiers: u32,
    button: u8,
    mouse_up: i32,
    click_count: i32,
) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(conn, Command::MouseClick {
            browser_id: instance.browser_id,
            x,
            y,
            modifiers,
            button,
            mouse_up: mouse_up != 0,
            click_count,
        });
    }
}

/// Send a mouse wheel event.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_mouse_wheel(
    handle: *mut CefUnityBrowser,
    x: i32,
    y: i32,
    modifiers: u32,
    delta_x: i32,
    delta_y: i32,
) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(conn, Command::MouseWheel {
            browser_id: instance.browser_id,
            x,
            y,
            modifiers,
            delta_x,
            delta_y,
        });
    }
}

/// Send a key event.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_key_event(
    handle: *mut CefUnityBrowser,
    event_type: u8,
    modifiers: u32,
    windows_key_code: i32,
    native_key_code: i32,
    character: u16,
    unmodified_character: u16,
    is_system_key: i32,
    focus_on_editable_field: i32,
) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(
            conn,
            Command::KeyEvent {
                browser_id: instance.browser_id,
                event_type,
                modifiers,
                windows_key_code,
                native_key_code,
                character,
                unmodified_character,
                is_system_key,
                focus_on_editable_field,
            },
        );
    }
}

/// Execute JavaScript in the browser's main frame.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_execute_javascript(handle: *mut CefUnityBrowser, code: *const c_char) {
    if handle.is_null() || code.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);
    let code_str = unsafe { CStr::from_ptr(code) }.to_str().unwrap_or("");

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(
            conn,
            Command::ExecuteJavaScript {
                browser_id: instance.browser_id,
                code: code_str.to_string(),
            },
        );
    }
}

/// Get the browser's current main-frame URL as UTF-8 bytes.
/// Returns the required buffer size including the trailing NUL terminator.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_url(
    handle: *mut CefUnityBrowser,
    buffer: *mut u8,
    buffer_len: i32,
) -> i32 {
    if handle.is_null() {
        return 0;
    }
    let instance = handle_to_ref(handle);

    let guard = CONNECTION.lock().unwrap();
    let Some(conn) = guard.as_ref() else {
        return 0;
    };

    let url = match send_command(
        conn,
        Command::GetCurrentUrl {
            browser_id: instance.browser_id,
        },
    ) {
        Ok(Response::CurrentUrl { url }) => url,
        Ok(Response::Error { msg }) => {
            log_to_file(&format!("get_url error: {}", msg));
            return 0;
        }
        Ok(other) => {
            log_to_file(&format!("get_url unexpected response: {:?}", other));
            return 0;
        }
        Err(e) => {
            log_to_file(&format!("get_url IPC error: {}", e));
            return 0;
        }
    };

    let bytes = url.as_bytes();
    let required = bytes.len() + 1;
    if !buffer.is_null() && buffer_len as usize >= required {
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len());
            *buffer.add(bytes.len()) = 0;
        }
    }

    required as i32
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

// ---------------------------------------------------------------------------
// Blocking variants — wait for server response, return 0=ok / -1=error.
// ---------------------------------------------------------------------------

/// Helper: send a command and wait for Response. Returns 0 on Ok, -1 on error.
fn blocking_simple(conn: &ServerConnection, cmd: Command) -> i32 {
    match send_command(conn, cmd) {
        Ok(Response::Ok) => 0,
        Ok(Response::Error { msg }) => {
            log_to_file(&format!("blocking command error: {}", msg));
            -1
        }
        Ok(_) => 0,
        Err(e) => {
            log_to_file(&format!("blocking command IPC error: {}", e));
            -1
        }
    }
}

/// Destroy a browser instance (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_destroy_browser_blocking(handle: *mut CefUnityBrowser) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let instance = unsafe { Box::from_raw(handle as *mut ClientBrowserInstance) };
    let guard = CONNECTION.lock().unwrap();
    let result = if let Some(conn) = guard.as_ref() {
        blocking_simple(
            conn,
            Command::DestroyBrowser {
                browser_id: instance.browser_id,
            },
        )
    } else {
        -1
    };
    drop(instance);
    result
}

/// Load a URL in the browser (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_load_url_blocking(
    handle: *mut CefUnityBrowser,
    url: *const c_char,
) -> i32 {
    if handle.is_null() || url.is_null() {
        return -1;
    }
    let instance = handle_to_ref(handle);
    let url_str = unsafe { CStr::from_ptr(url) }.to_str().unwrap_or("");
    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        blocking_simple(
            conn,
            Command::LoadUrl {
                browser_id: instance.browser_id,
                url: url_str.to_string(),
            },
        )
    } else {
        -1
    }
}

/// Resize the browser viewport (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_resize_blocking(
    handle: *mut CefUnityBrowser,
    width: i32,
    height: i32,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let instance = handle_to_ref(handle);
    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        blocking_simple(
            conn,
            Command::Resize {
                browser_id: instance.browser_id,
                width,
                height,
            },
        )
    } else {
        -1
    }
}

/// Send a mouse move event (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_mouse_move_blocking(
    handle: *mut CefUnityBrowser,
    x: i32,
    y: i32,
    modifiers: u32,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let instance = handle_to_ref(handle);
    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        blocking_simple(
            conn,
            Command::MouseMove {
                browser_id: instance.browser_id,
                x,
                y,
                modifiers,
            },
        )
    } else {
        -1
    }
}

/// Send a mouse click event (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_mouse_click_blocking(
    handle: *mut CefUnityBrowser,
    x: i32,
    y: i32,
    modifiers: u32,
    button: u8,
    mouse_up: i32,
    click_count: i32,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let instance = handle_to_ref(handle);
    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        blocking_simple(
            conn,
            Command::MouseClick {
                browser_id: instance.browser_id,
                x,
                y,
                modifiers,
                button,
                mouse_up: mouse_up != 0,
                click_count,
            },
        )
    } else {
        -1
    }
}

/// Send a mouse wheel event (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_mouse_wheel_blocking(
    handle: *mut CefUnityBrowser,
    x: i32,
    y: i32,
    modifiers: u32,
    delta_x: i32,
    delta_y: i32,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let instance = handle_to_ref(handle);
    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        blocking_simple(
            conn,
            Command::MouseWheel {
                browser_id: instance.browser_id,
                x,
                y,
                modifiers,
                delta_x,
                delta_y,
            },
        )
    } else {
        -1
    }
}

/// Send a key event (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_key_event_blocking(
    handle: *mut CefUnityBrowser,
    event_type: u8,
    modifiers: u32,
    windows_key_code: i32,
    native_key_code: i32,
    character: u16,
    unmodified_character: u16,
    is_system_key: i32,
    focus_on_editable_field: i32,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    let instance = handle_to_ref(handle);
    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        blocking_simple(
            conn,
            Command::KeyEvent {
                browser_id: instance.browser_id,
                event_type,
                modifiers,
                windows_key_code,
                native_key_code,
                character,
                unmodified_character,
                is_system_key,
                focus_on_editable_field,
            },
        )
    } else {
        -1
    }
}

/// Execute JavaScript in the browser's main frame (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_execute_javascript_blocking(
    handle: *mut CefUnityBrowser,
    code: *const c_char,
) -> i32 {
    if handle.is_null() || code.is_null() {
        return -1;
    }
    let instance = handle_to_ref(handle);
    let code_str = unsafe { CStr::from_ptr(code) }.to_str().unwrap_or("");
    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        blocking_simple(
            conn,
            Command::ExecuteJavaScript {
                browser_id: instance.browser_id,
                code: code_str.to_string(),
            },
        )
    } else {
        -1
    }
}
