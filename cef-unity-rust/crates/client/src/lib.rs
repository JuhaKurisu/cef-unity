// FFI layer for Unity C# interop.
//
// This is a pure IPC client — no CEF dependency.
// Communicates with cef-unity-server via ipc-channel + shared memory.

#[cfg(target_os = "windows")]
mod d3d11;
#[cfg(target_os = "windows")]
mod d3d12;

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
}

fn handle_to_ref<'a>(handle: *mut CefUnityBrowser) -> &'a mut ClientBrowserInstance {
    unsafe { &mut *(handle as *mut ClientBrowserInstance) }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static IOSURFACE_CONNECTED: AtomicBool = AtomicBool::new(false);
/// GPU (accelerated paint) を使うか。Init 時にセットされ、以降は不変。
/// false の場合は server が software paint で動作し、client 側でも
/// is_*_connected getter が 0 を返して C# が software 経路に入る。
static USE_GPU_MODE: AtomicBool = AtomicBool::new(true);
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
/// `use_gpu`: 非 0 で accelerated paint (GPU 共有テクスチャ / IOSurface) を使う。
/// 0 で software paint (CPU 経由の shm BGRA 転送) を強制する。
/// Returns 0 on success, non-zero on failure.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_init(use_gpu: i32) -> i32 {
    if INITIALIZED.load(Ordering::SeqCst) {
        return 0;
    }

    let use_gpu_bool = use_gpu != 0;
    USE_GPU_MODE.store(use_gpu_bool, Ordering::SeqCst);
    log_to_file(&format!(
        "---- cef_unity_init(use_gpu={}) called (IPC client mode) ----",
        use_gpu_bool
    ));

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

    // Launch server process with --ipc-server argument。
    // Windows では D3D11 共有テクスチャを DuplicateHandle で渡すために
    // クライアント PID も渡す。
    let client_pid = std::process::id();
    match std::process::Command::new(&server_app)
        .arg("--ipc-server")
        .arg(&server_name)
        .arg("--client-pid")
        .arg(client_pid.to_string())
        .arg("--use-gpu")
        .arg(if use_gpu_bool { "1" } else { "0" })
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

    // Connect to server's Mach IOSurface port service (macOS only)
    // GPU モード時のみ接続する。CPU モードでは IOSURFACE_CONNECTED は false のまま。
    #[cfg(target_os = "macos")]
    if use_gpu_bool {
        let service_name = cef_unity_ipc::iosurface_service_name(bootstrap.server_pid);
        log_to_file(&format!("connecting to Mach IOSurface service: {}", service_name));
        let cname = std::ffi::CString::new(service_name.as_str()).unwrap();
        let ret = unsafe { mach_iosurface_client_connect(cname.as_ptr()) };
        if ret == 0 {
            IOSURFACE_CONNECTED.store(true, Ordering::SeqCst);
            log_to_file("Mach IOSurface service connected");
        } else {
            log_to_file(&format!("Mach IOSurface service connect failed: {}", ret));
        }
    }

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
        // fire-and-forget: server プロセスが無応答でも Unity main thread を
        // 永久ブロックさせないため、応答は待たない。server 側は
        // expects_response=false でも Shutdown を正しく処理して running=false にする
        // (event_loop/generic.rs の drain_commands 参照)。
        send_command_no_wait(&conn, Command::Shutdown);
        // Server が Shutdown を処理して cef::shutdown() を呼び終わるまで少し待つ。
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    INITIALIZED.store(false, Ordering::SeqCst);
    IOSURFACE_CONNECTED.store(false, Ordering::SeqCst);
    USE_GPU_MODE.store(true, Ordering::SeqCst);
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
            d3d11_fence_handle,
        } => {
            log_to_file(&format!(
                "browser created: id={}, shm={}, fence_handle=0x{:x}",
                browser_id, shm_flink, d3d11_fence_handle
            ));
            let shm = match ShmReader::open(&shm_flink) {
                Ok(s) => s,
                Err(e) => {
                    log_to_file(&format!("shm_open failed: {}", e));
                    return std::ptr::null_mut();
                }
            };
            #[cfg(target_os = "windows")]
            {
                if d3d11_fence_handle != 0 {
                    // Unity の graphics backend に応じて開ける方を試す。
                    // D3D11/D3D12 双方無接続でも fence_handle 自体は同じ NT 共有 HANDLE。
                    if d3d11::is_connected() {
                        if let Err(e) = d3d11::open_fence(d3d11_fence_handle) {
                            log_to_file(&format!("d3d11::open_fence failed: {}", e));
                        }
                    }
                    if d3d12::is_connected() {
                        if let Err(e) = d3d12::open_fence(d3d11_fence_handle) {
                            log_to_file(&format!("d3d12::open_fence failed: {}", e));
                        }
                    }
                }
            }
            #[cfg(not(target_os = "windows"))]
            let _ = d3d11_fence_handle;
            let instance = Box::new(ClientBrowserInstance { browser_id, shm });
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
        send_command_no_wait(
            conn,
            Command::MouseMove {
                browser_id: instance.browser_id,
                x,
                y,
                modifiers,
            },
        );
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
        send_command_no_wait(
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
        );
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
        send_command_no_wait(
            conn,
            Command::MouseWheel {
                browser_id: instance.browser_id,
                x,
                y,
                modifiers,
                delta_x,
                delta_y,
            },
        );
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

/// Execute an editing command (copy, paste, cut, select_all, undo, redo).
/// command: 0=Copy, 1=Paste, 2=Cut, 3=SelectAll, 4=Undo, 5=Redo
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_edit_command(handle: *mut CefUnityBrowser, command: u8) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(
            conn,
            Command::EditCommand {
                browser_id: instance.browser_id,
                command,
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

    match instance.shm.get_active_buffer_ptr() {
        Some((ptr, w, h)) => {
            PAINT_COUNT.fetch_add(1, Ordering::Relaxed);
            unsafe {
                *out_buffer = ptr;
                *out_width = w as i32;
                *out_height = h as i32;
            }
            1
        }
        None => {
            unsafe {
                *out_buffer = std::ptr::null();
                *out_width = 0;
                *out_height = 0;
            }
            0
        }
    }
}

/// Read the IME caret rect from shared memory.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_ime_caret(
    handle: *mut CefUnityBrowser,
    out_x: *mut i32,
    out_y: *mut i32,
    out_w: *mut i32,
    out_h: *mut i32,
) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);
    let (x, y, w, h) = instance.shm.read_ime_caret();
    unsafe {
        *out_x = x;
        *out_y = y;
        *out_w = w;
        *out_h = h;
    }
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

/// Set IME composition text (preedit).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_ime_set_composition(
    handle: *mut CefUnityBrowser,
    text: *const c_char,
    selection_start: u32,
    selection_end: u32,
) {
    if handle.is_null() || text.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);
    let text_str = unsafe { CStr::from_ptr(text) }.to_str().unwrap_or("");

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(
            conn,
            Command::ImeSetComposition {
                browser_id: instance.browser_id,
                text: text_str.to_string(),
                selection_start,
                selection_end,
            },
        );
    }
}

/// Commit IME text (finalize composition and insert text).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_ime_commit_text(handle: *mut CefUnityBrowser, text: *const c_char) {
    if handle.is_null() || text.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);
    let text_str = unsafe { CStr::from_ptr(text) }.to_str().unwrap_or("");

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(
            conn,
            Command::ImeCommitText {
                browser_id: instance.browser_id,
                text: text_str.to_string(),
            },
        );
    }
}

/// Finish composing text (apply current composition).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_ime_finish_composing_text(
    handle: *mut CefUnityBrowser,
    keep_selection: i32,
) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(
            conn,
            Command::ImeFinishComposingText {
                browser_id: instance.browser_id,
                keep_selection: keep_selection != 0,
            },
        );
    }
}

/// Cancel the current IME composition.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_ime_cancel_composition(handle: *mut CefUnityBrowser) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);

    let guard = CONNECTION.lock().unwrap();
    if let Some(conn) = guard.as_ref() {
        send_command_no_wait(
            conn,
            Command::ImeCancelComposition {
                browser_id: instance.browser_id,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// IOSurface / Metal texture (macOS)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn cef_unity_create_metal_texture_objc(
        surface_id: u32,
        width: i32,
        height: i32,
        format: u32,
    ) -> *mut std::ffi::c_void;

    fn cef_unity_release_metal_texture_objc(texture_ptr: *mut std::ffi::c_void);

    fn mach_iosurface_client_connect(service_name: *const std::ffi::c_char) -> i32;

    fn mach_iosurface_recv_texture(
        out_width: *mut i32,
        out_height: *mut i32,
        out_format: *mut u32,
    ) -> *mut std::ffi::c_void;

}

/// Check if a new accelerated paint frame is available via IOSurface.
/// Returns 1 if new info, 0 if unchanged. Writes surface_id, width, height, format to out params.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_iosurface_info(
    handle: *mut CefUnityBrowser,
    out_surface_id: *mut u32,
    out_width: *mut i32,
    out_height: *mut i32,
    out_format: *mut u32,
) -> i32 {
    static ACCEL_LOG_COUNT: AtomicU64 = AtomicU64::new(0);

    if handle.is_null()
        || out_surface_id.is_null()
        || out_width.is_null()
        || out_height.is_null()
        || out_format.is_null()
    {
        return 0;
    }
    let instance = handle_to_ref(handle);

    match instance.shm.get_iosurface_info() {
        Some((surface_id, w, h, format)) => {
            let count = ACCEL_LOG_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            if count <= 5 || count % 100 == 0 {
                log_to_file(&format!(
                    "get_iosurface_info #{}: surface_id={} {}x{} fmt={}",
                    count, surface_id, w, h, format
                ));
            }
            unsafe {
                *out_surface_id = surface_id;
                *out_width = w as i32;
                *out_height = h as i32;
                *out_format = format;
            }
            1
        }
        None => 0,
    }
}

/// Create a Metal texture backed by an IOSurface.
/// Uses the system default Metal device internally.
/// Returns an opaque MTLTexture pointer, or null on failure.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_create_metal_texture(
    surface_id: u32,
    width: i32,
    height: i32,
    format: u32,
) -> *mut std::ffi::c_void {
    #[cfg(target_os = "macos")]
    {
        unsafe { cef_unity_create_metal_texture_objc(surface_id, width, height, format) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (surface_id, width, height, format);
        std::ptr::null_mut()
    }
}

/// Receive the latest IOSurface from the server via Mach port and create a Metal texture.
/// Returns an opaque MTLTexture pointer, or null if no new frame.
/// The caller must release the returned texture with cef_unity_release_metal_texture.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_recv_iosurface_texture(
    out_width: *mut i32,
    out_height: *mut i32,
    out_format: *mut u32,
) -> *mut std::ffi::c_void {
    if out_width.is_null() || out_height.is_null() || out_format.is_null() {
        return std::ptr::null_mut();
    }
    #[cfg(target_os = "macos")]
    {
        if !IOSURFACE_CONNECTED.load(Ordering::SeqCst) {
            return std::ptr::null_mut();
        }
        unsafe {
            mach_iosurface_recv_texture(out_width, out_height, out_format)
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (out_width, out_height, out_format);
        std::ptr::null_mut()
    }
}


/// Returns 1 if the Mach IOSurface port channel is connected, 0 otherwise.
/// CPU モード (Init で use_gpu=0) のときは常に 0 を返す。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_is_iosurface_connected() -> i32 {
    if !USE_GPU_MODE.load(Ordering::SeqCst) {
        return 0;
    }
    if IOSURFACE_CONNECTED.load(Ordering::SeqCst) { 1 } else { 0 }
}

/// Release a Metal texture previously created by cef_unity_create_metal_texture.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_release_metal_texture(texture: *mut std::ffi::c_void) {
    #[cfg(target_os = "macos")]
    {
        unsafe {
            cef_unity_release_metal_texture_objc(texture);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = texture;
    }
}

// ---------------------------------------------------------------------------
// Log retrieval
// ---------------------------------------------------------------------------

static CACHED_LOGS: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// Retrieve server logs as NUL-separated UTF-8 entries.
/// If buffer is null, sends GetLogs via IPC, caches result, and returns required size.
/// If buffer is non-null, copies cached data into buffer and clears the cache.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_logs(buffer: *mut u8, buffer_len: i32) -> i32 {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return 0;
    }

    if buffer.is_null() {
        // Phase 1: fetch from server and cache
        let guard = CONNECTION.lock().unwrap();
        let Some(conn) = guard.as_ref() else {
            return 0;
        };

        let entries = match send_command(conn, Command::GetLogs) {
            Ok(Response::Logs { entries }) => entries,
            _ => return 0,
        };

        if entries.is_empty() {
            *CACHED_LOGS.lock().unwrap() = None;
            return 0;
        }

        // Encode as "msg1\0msg2\0" (trailing NUL included)
        let mut encoded = Vec::new();
        for entry in &entries {
            encoded.extend_from_slice(entry.as_bytes());
            encoded.push(0);
        }

        let size = encoded.len() as i32;
        *CACHED_LOGS.lock().unwrap() = Some(encoded);
        size
    } else {
        // Phase 2: copy cached data to buffer
        let mut cache = CACHED_LOGS.lock().unwrap();
        let Some(data) = cache.take() else {
            return 0;
        };

        let copy_len = data.len().min(buffer_len as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), buffer, copy_len);
        }
        copy_len as i32
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

// ---------------------------------------------------------------------------
// Windows: D3D11 共有テクスチャ受信
// ---------------------------------------------------------------------------

/// Unity Native Plugin Interface のエントリポイント。Unity が DLL ロード時に呼ぶ。
/// IUnityGraphicsD3D11 / IUnityGraphicsD3D12v5 経由で Unity の Device を取得し保持する。
/// 両方を試して、Unity の graphics backend に応じて生きている方が使われる。
/// 非 Windows プラットフォームでは何もしない。
#[unsafe(no_mangle)]
pub extern "C" fn UnityPluginLoad(unity_interfaces: *mut std::ffi::c_void) {
    log_to_file(&format!(
        "UnityPluginLoad called (interfaces={:p})",
        unity_interfaces
    ));
    #[cfg(target_os = "windows")]
    {
        d3d11::set_unity_interfaces(unity_interfaces as *mut d3d11::IUnityInterfaces);
        d3d12::set_unity_interfaces(unity_interfaces);
        log_to_file(&format!(
            "UnityPluginLoad: d3d11_connected={} d3d12_connected={}",
            d3d11::is_connected(),
            d3d12::is_connected()
        ));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = unity_interfaces;
    }
}

/// Unity Native Plugin Interface のアンロード。Unity が DLL アンロード時に呼ぶ。
#[unsafe(no_mangle)]
pub extern "C" fn UnityPluginUnload() {
    #[cfg(target_os = "windows")]
    {
        d3d11::clear_unity_interfaces();
        d3d12::clear_unity_interfaces();
    }
}

/// Windows: Unity の D3D11 device に接続済みなら 1 を返す。
/// CPU モード (Init で use_gpu=0) のときは常に 0 を返す。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_is_d3d11_connected() -> i32 {
    if !USE_GPU_MODE.load(Ordering::SeqCst) {
        return 0;
    }
    #[cfg(target_os = "windows")]
    {
        if d3d11::is_connected() { 1 } else { 0 }
    }
    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

/// Windows: Unity の D3D12 device に接続済みなら 1 を返す。
/// C# 側はこちらが 1 のとき `cef_unity_recv_d3d12_texture` を呼ぶ。
/// CPU モード (Init で use_gpu=0) のときは常に 0 を返す。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_is_d3d12_connected() -> i32 {
    if !USE_GPU_MODE.load(Ordering::SeqCst) {
        return 0;
    }
    #[cfg(target_os = "windows")]
    {
        if d3d12::is_connected() { 1 } else { 0 }
    }
    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

/// Windows: 共有メモリから最新の D3D11 共有 HANDLE を読み出し、
/// Unity の D3D11Device で OpenSharedResource1 した ID3D11Texture2D* を返す。
/// 新フレームが無い場合は null。
///
/// 戻り値ポインタは内部で AddRef 済みのキャッシュであり、次に handle が変わるか
/// プラグイン unload までは Unity 側で再 AddRef せずに使ってよい。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_recv_d3d11_texture(
    handle: *mut CefUnityBrowser,
    out_width: *mut i32,
    out_height: *mut i32,
    out_format: *mut u32,
) -> *mut std::ffi::c_void {
    if handle.is_null() || out_width.is_null() || out_height.is_null() || out_format.is_null() {
        return std::ptr::null_mut();
    }

    #[cfg(target_os = "windows")]
    {
        let instance = handle_to_ref(handle);
        let Some((handle_value, w, h, format, fence_value)) = instance.shm.get_d3d11_handle()
        else {
            // 新フレーム無し: 前回開いたテクスチャを Unity 側で使い続けてもらう (null 返却)。
            return std::ptr::null_mut();
        };
        // GPU-side wait: Unity の immediate context に fence_value 到達待ちを発行する。
        // CPU はブロックせず、Unity の以降の描画コマンドが GPU 上で server.Copy 完了を待つ。
        if let Err(e) = d3d11::wait_fence(fence_value) {
            log_to_file(&format!("d3d11::wait_fence({}) failed: {}", fence_value, e));
        }
        let Some((tex_ptr, w_out, h_out)) = d3d11::open_or_cached(handle_value, w, h) else {
            return std::ptr::null_mut();
        };
        unsafe {
            *out_width = w_out as i32;
            *out_height = h_out as i32;
            *out_format = format;
        }
        PAINT_COUNT.fetch_add(1, Ordering::Relaxed);
        tex_ptr
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (handle, out_width, out_height, out_format);
        std::ptr::null_mut()
    }
}

/// Windows: 共有メモリから最新の D3D11 共有 HANDLE を読み出し、
/// Unity の D3D12Device で OpenSharedHandle した ID3D12Resource* を返す。
/// KeyedMutex で server との排他とキャッシュコヒーレンスを取り、
/// 初回のみ COMMON → PIXEL_SHADER_RESOURCE 状態遷移を Unity に宣言する。
/// 新フレームが無い場合は null。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_recv_d3d12_texture(
    handle: *mut CefUnityBrowser,
    out_width: *mut i32,
    out_height: *mut i32,
    out_format: *mut u32,
) -> *mut std::ffi::c_void {
    if handle.is_null() || out_width.is_null() || out_height.is_null() || out_format.is_null() {
        return std::ptr::null_mut();
    }

    #[cfg(target_os = "windows")]
    {
        let instance = handle_to_ref(handle);
        let Some((handle_value, w, h, format, fence_value)) = instance.shm.get_d3d11_handle()
        else {
            // 新フレーム無し: 前回開いたテクスチャを Unity 側で使い続けてもらう (null 返却)。
            return std::ptr::null_mut();
        };
        // GPU-side wait: Unity の D3D12 queue に fence_value 到達待ちを発行する。
        // CPU はブロックせず、Unity の以降の queue 操作が GPU 上で server.Copy 完了を待つ。
        if let Err(e) = d3d12::wait_fence(fence_value) {
            log_to_file(&format!("d3d12::wait_fence({}) failed: {}", fence_value, e));
        }
        let Some((res_ptr, w_out, h_out)) = d3d12::open_or_cached(handle_value, w, h) else {
            return std::ptr::null_mut();
        };
        unsafe {
            *out_width = w_out as i32;
            *out_height = h_out as i32;
            *out_format = format;
        }
        PAINT_COUNT.fetch_add(1, Ordering::Relaxed);
        res_ptr
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (handle, out_width, out_height, out_format);
        std::ptr::null_mut()
    }
}
