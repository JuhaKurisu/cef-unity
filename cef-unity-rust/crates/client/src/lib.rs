// FFI layer for Unity C# interop.
//
// This is a pure IPC client — no CEF dependency.
// Communicates with cef-unity-server via ipc-channel + shared memory.

#[cfg(target_os = "windows")]
mod d3d11;
#[cfg(target_os = "windows")]
mod d3d12;

#[cfg(target_os = "macos")]
mod audio_ring;
#[cfg(target_os = "macos")]
mod au_output;
#[cfg(target_os = "macos")]
mod native_voice;
#[cfg(target_os = "macos")]
mod scroll_monitor;

use std::ffi::{CStr, c_char};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ipc_channel::ipc::{IpcOneShotServer, IpcReceiver, IpcSender};

use cef_unity_ipc::{AudioSharedMemoryReader, Bootstrap, Command, CommandEnvelope, Response, SharedMemoryReader};

// ---------------------------------------------------------------------------
// dylib location helpers
// ---------------------------------------------------------------------------

/// dylib/DLL 自身のディレクトリを返す。
fn dylib_directory() -> PathBuf {
    let info = dynamic_library_info().expect("failed to locate dylib/DLL");
    PathBuf::from(info).parent().unwrap().to_path_buf()
}

/// Unix: dladdr で共有ライブラリのパスを取得する。
#[cfg(unix)]
fn dynamic_library_info() -> Option<String> {
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
    let result = unsafe { dladdr(dylib_directory as *const u8, &mut info) };
    if result == 0 || info.dli_fname.is_null() {
        return None;
    }
    let c_string = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) };
    Some(c_string.to_str().ok()?.to_string())
}

/// Windows: GetModuleHandleExW + GetModuleFileNameW で DLL のパスを取得する。
#[cfg(windows)]
fn dynamic_library_info() -> Option<String> {
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

    let mut module_handle: HMODULE = std::ptr::null_mut();
    let flags =
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    let result = unsafe { GetModuleHandleExW(flags, dylib_directory as *const u16, &mut module_handle) };
    if result == 0 || module_handle.is_null() {
        return None;
    }

    let mut buffer = vec![0u16; 4096];
    let length = unsafe { GetModuleFileNameW(module_handle, buffer.as_mut_ptr(), buffer.len() as DWORD) };
    if length == 0 {
        return None;
    }
    let os_string = OsString::from_wide(&buffer[..length as usize]);
    os_string.into_string().ok()
}

/// サーバーバイナリのパスを返す。
#[cfg(target_os = "macos")]
fn server_binary_path(plugin_directory: &std::path::Path) -> PathBuf {
    plugin_directory.join("cef-unity-server.app/Contents/MacOS/cef-unity-server")
}

#[cfg(target_os = "linux")]
fn server_binary_path(plugin_directory: &std::path::Path) -> PathBuf {
    plugin_directory.join("cef-unity-server")
}

#[cfg(target_os = "windows")]
fn server_binary_path(plugin_directory: &std::path::Path) -> PathBuf {
    plugin_directory.join("cef-unity-server.exe")
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
    shared_memory: SharedMemoryReader,
    /// 音声リングバッファのリーダー。サーバーが flink を返さなかった場合や
    /// open に失敗した場合は None (音声無効)。
    audio_shared_memory: Option<AudioSharedMemoryReader>,
    /// 音声リングの flink。NativeVoice が独立カーソルの自前リーダーを開くのに使う。
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    audio_flink: String,
    /// CRI 方式ネイティブ音声出力 (macOS)。Unity ミキサを迂回して AudioUnit で再生。
    #[cfg(target_os = "macos")]
    native_voice: Option<native_voice::NativeVoice>,
}

fn handle_to_reference<'a>(handle: *mut CefUnityBrowser) -> &'a mut ClientBrowserInstance {
    unsafe { &mut *(handle as *mut ClientBrowserInstance) }
}

/// ネイティブ音声を停止する (排水待ち)。destroy の先頭で呼ぶこと —
/// NativeVoice は自前 reader/Shmem を持ち instance と参照関係がないため、
/// stop (排水待ち) さえ済めば以降の解放順序で UAF は構造的に起きない。
fn stop_native_voice(instance: &mut ClientBrowserInstance) {
    #[cfg(target_os = "macos")]
    {
        instance.native_voice.take();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = instance;
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static INITIALIZED: AtomicBool = AtomicBool::new(false);
/// ログ出力の有効/無効。Init で Unity 側のマスターフラグに従って設定される。
/// false の場合 log_to_file は何もしない (ファイルログ抑制)。
static LOG_ENABLED: AtomicBool = AtomicBool::new(false);
static IOSURFACE_CONNECTED: AtomicBool = AtomicBool::new(false);
/// GPU (accelerated paint) を使うか。Init 時にセットされ、以降は不変。
/// false の場合は server が software paint で動作し、client 側でも
/// is_*_connected getter が 0 を返して C# が software 経路に入る。
static USE_GPU_MODE: AtomicBool = AtomicBool::new(true);
static PAINT_COUNT: AtomicU64 = AtomicU64::new(0);
static PUMP_COUNT: AtomicU64 = AtomicU64::new(0);

struct ServerConnection {
    command_sender: IpcSender<CommandEnvelope>,
    response_receiver: IpcReceiver<Response>,
    /// server プロセスのハンドル。保持しないと終了後にゾンビとして残る
    /// (Editor は長寿命なので Play/Stop の繰り返しで蓄積する)。
    child: std::process::Child,
}

static CONNECTION: Mutex<Option<ServerConnection>> = Mutex::new(None);

fn log_to_file(message: &str) {
    if !LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let path = std::env::temp_dir().join("cef_unity_debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "[{:?}] {}", std::time::SystemTime::now(), message);
    }
}

/// FFI 境界のパニックガード。extern "C" 越しの unwind は edition 2024 で即 abort
/// (= Unity Editor ごと落ちる) ため、全エントリポイントの本体をこれで包む。
/// panic 時は default を返し、原因を試行ログに残す (ログ自体の失敗は無視)。
fn ffi_guard<T>(default: T, function: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(function)) {
        Ok(value) => value,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|string| string.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            let _ = std::panic::catch_unwind(|| log_to_file(&format!("FFI panic: {}", message)));
            default
        }
    }
}

// ---------------------------------------------------------------------------
// IPC helpers
// ---------------------------------------------------------------------------

fn send_command(connection: &ServerConnection, command: Command) -> Result<Response, String> {
    connection.command_sender
        .send(CommandEnvelope {
            command,
            expects_response: true,
        })
        .map_err(|error| format!("send: {}", error))?;
    connection.response_receiver.recv().map_err(|error| format!("recv: {}", error))
}

/// Fire-and-forget: send only, don't wait for response.
fn send_command_no_wait(connection: &ServerConnection, command: Command) {
    let _ = connection.command_sender.send(CommandEnvelope {
        command,
        expects_response: false,
    });
}

// ---------------------------------------------------------------------------
// Global functions
// ---------------------------------------------------------------------------

/// Initialize: launch CEF server process and connect via ipc-channel.
/// `use_gpu`: 非 0 で accelerated paint (GPU 共有テクスチャ / IOSurface) を使う。
/// 0 で software paint (CPU 経由の shm BGRA 転送) を強制する。
/// `enable_log`: 非 0 で client/server のファイルログを有効にする。0 で全ログ抑制。
/// Unity 側のマスターログフラグから渡す。
/// Returns 0 on success, non-zero on failure.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_initialize(use_gpu: i32, enable_log: i32) -> i32 {
    ffi_guard(-1, || {
        // ログ有効/無効を最初に確定させる (以降の log_to_file がこれに従う)。
        LOG_ENABLED.store(enable_log != 0, Ordering::SeqCst);

        if INITIALIZED.load(Ordering::SeqCst) {
            return 0;
        }

        let use_gpu_bool = use_gpu != 0;
        USE_GPU_MODE.store(use_gpu_bool, Ordering::SeqCst);
        log_to_file(&format!(
            "---- cef_unity_initialize(use_gpu={}) called (IPC client mode) ----",
            use_gpu_bool
        ));

        // Find server binary next to dylib
        let plugin_directory = dylib_directory();
        let server_app = server_binary_path(&plugin_directory);
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
            Err(error) => {
                log_to_file(&format!("failed to create one-shot server: {}", error));
                return -4;
            }
        };
        log_to_file(&format!("one-shot server name = {}", server_name));

        // Launch server process with --ipc-server argument。
        // Windows では D3D11 共有テクスチャを DuplicateHandle で渡すために
        // クライアント PID も渡す。
        let client_pid = std::process::id();
        let mut child = match std::process::Command::new(&server_app)
            .arg("--ipc-server")
            .arg(&server_name)
            .arg("--client-pid")
            .arg(client_pid.to_string())
            .arg("--use-gpu")
            .arg(if use_gpu_bool { "1" } else { "0" })
            .arg("--logging")
            .arg(if enable_log != 0 { "1" } else { "0" })
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                log_to_file(&format!("failed to spawn server: {}", error));
                return -4;
            }
        };
        log_to_file("server spawned");

        // Wait for server to connect and send bootstrap.
        // accept() 自体は無期限ブロックするため別スレッドで行い、server の早期死亡
        // (codesign 不備・framework 欠落・CEF 初期化失敗等) とタイムアウトを監視する。
        // これがないと server 起動失敗時に Unity main thread が永久フリーズする。
        let (bootstrap_sender, bootstrap_receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = bootstrap_sender.send(oneshot_server.accept());
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let bootstrap = loop {
            match bootstrap_receiver.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(Ok((_receiver, bootstrap))) => break bootstrap,
                Ok(Err(error)) => {
                    log_to_file(&format!("failed to accept bootstrap: {}", error));
                    let _ = child.kill();
                    let _ = child.wait();
                    return -5;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        log_to_file(&format!("server exited during init: {}", status));
                        return -6;
                    }
                    if std::time::Instant::now() >= deadline {
                        log_to_file("bootstrap accept timed out (15s); killing server");
                        let _ = child.kill();
                        let _ = child.wait();
                        return -7;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    log_to_file("bootstrap accept thread terminated unexpectedly");
                    let _ = child.kill();
                    let _ = child.wait();
                    return -5;
                }
            }
        };
        log_to_file("bootstrap received from server");

        *CONNECTION.lock().unwrap_or_else(PoisonError::into_inner) = Some(ServerConnection {
            command_sender: bootstrap.command_sender,
            response_receiver: bootstrap.response_receiver,
            child,
        });

        // Connect to server's Mach IOSurface port service (macOS only)
        // GPU モード時のみ接続する。CPU モードでは IOSURFACE_CONNECTED は false のまま。
        #[cfg(target_os = "macos")]
        if use_gpu_bool {
            let service_name = cef_unity_ipc::iosurface_service_name(bootstrap.server_pid);
            log_to_file(&format!("connecting to Mach IOSurface service: {}", service_name));
            if let Ok(c_service_name) = std::ffi::CString::new(service_name.as_str()) {
                let result = unsafe { mach_iosurface_client_connect(c_service_name.as_ptr()) };
                if result == 0 {
                    IOSURFACE_CONNECTED.store(true, Ordering::SeqCst);
                    log_to_file("Mach IOSurface service connected");
                } else {
                    log_to_file(&format!("Mach IOSurface service connect failed: {}", result));
                }
            } else {
                // service name に NUL が混入した場合のみ。接続失敗と同じく非致命 (CPU 経路へ)。
                log_to_file("iosurface service name contained NUL; skipping connect");
            }
        }

        INITIALIZED.store(true, Ordering::SeqCst);
        log_to_file("initialized successfully (IPC client)");
        0
    })
}

/// Pump CEF message loop — no-op in IPC mode (server has its own loop).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_pump() {
    ffi_guard((), || {
        PUMP_COUNT.fetch_add(1, Ordering::Relaxed);
    })
}

/// Returns the number of on_paint calls (tracked per-frame reads in IPC mode).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_paint_count() -> u64 {
    ffi_guard(0, || {
        PAINT_COUNT.load(Ordering::Relaxed)
    })
}

/// Returns the number of pump iterations.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_pump_count() -> u64 {
    ffi_guard(0, || {
        PUMP_COUNT.load(Ordering::Relaxed)
    })
}

/// Shut down: send Shutdown command and wait for server to exit.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_shutdown() {
    ffi_guard((), || {
        if !INITIALIZED.load(Ordering::SeqCst) {
            return;
        }
        log_to_file("cef_unity_shutdown()");

        // 先に take してガードを解放する: 保持したまま 500ms sleep すると
        // 他スレッドの全 FFI 呼び出しがその間ロック待ちになる。
        let connection = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(mut connection) = connection {
            // fire-and-forget: server プロセスが無応答でも Unity main thread を
            // 永久ブロックさせないため、応答は待たない。server 側は
            // expects_response=false でも Shutdown を正しく処理して running=false にする
            // (event_loop/generic.rs の drain_commands 参照)。
            send_command_no_wait(&connection, Command::Shutdown);
            // Server が Shutdown を処理して cef::shutdown() を呼び終わるまで少し待つ。
            std::thread::sleep(std::time::Duration::from_millis(500));
            // 終了済みならここで回収してゾンビ化を防ぐ (未終了なら従来どおり放置)。
            let _ = connection.child.try_wait();
        }

        INITIALIZED.store(false, Ordering::SeqCst);
        IOSURFACE_CONNECTED.store(false, Ordering::SeqCst);
        USE_GPU_MODE.store(true, Ordering::SeqCst);
        log_to_file("shutdown complete");
    })
}

/// 生スクロールイベント (scroll_monitor.m / C# 側と同一レイアウト)。
/// phase: 0=None 1=GestureBegan 2=GestureChanged 3=GestureEnded
///        4=MomentumBegan 5=MomentumChanged 6=MomentumEnded 7=Cancelled
#[repr(C)]
pub struct CefScrollEvent {
    pub timestamp: f64,
    pub delta_x: f32,
    pub delta_y: f32,
    pub phase: u8,
    pub precise: u8,
}

/// NSEvent スクロールモニタを開始する。1=成功 0=失敗 (ヘッドレス等)。
/// macOS 以外は常に 0 (呼び出し側がフォールバックする)。
#[unsafe(no_mangle)]
pub extern "C" fn cef_scroll_monitor_start() -> i32 {
    ffi_guard(0, || {
        #[cfg(target_os = "macos")]
        {
            unsafe { scroll_monitor::cef_scroll_monitor_start_impl() }
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cef_scroll_monitor_stop() {
    ffi_guard((), || {
        #[cfg(target_os = "macos")]
        unsafe {
            scroll_monitor::cef_scroll_monitor_stop_impl()
        }
    })
}

/// 新着イベントを out に書き、件数を返す。毎フレーム呼ぶこと (リング鮮度維持)。
#[unsafe(no_mangle)]
pub extern "C" fn cef_scroll_monitor_poll(out: *mut CefScrollEvent, max: i32) -> i32 {
    ffi_guard(0, || {
        if out.is_null() || max <= 0 {
            return 0; // 負の max は .m 側 memcpy の size_t ラップ = 巨大コピーになる
        }
        #[cfg(target_os = "macos")]
        {
            unsafe {
                scroll_monitor::cef_scroll_monitor_poll_impl(
                    out as *mut scroll_monitor::RawScrollEvent,
                    max,
                )
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (out, max);
            0
        }
    })
}

/// イベント timestamp と同一クロック (起動からの秒) の現在時刻。リサンプル基準用。
#[unsafe(no_mangle)]
pub extern "C" fn cef_scroll_monitor_now() -> f64 {
    ffi_guard(0.0, || {
        #[cfg(target_os = "macos")]
        {
            unsafe { scroll_monitor::cef_scroll_monitor_now_impl() }
        }
        #[cfg(not(target_os = "macos"))]
        {
            0.0
        }
    })
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
    ffi_guard(std::ptr::null_mut(), || {
        if !INITIALIZED.load(Ordering::SeqCst) || url.is_null() {
            return std::ptr::null_mut();
        }

        let url_string = unsafe { CStr::from_ptr(url) }.to_str().unwrap_or("");
        log_to_file(&format!(
            "cef_unity_create_browser({}x{}, {})",
            width, height, url_string
        ));

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        let connection = match guard.as_ref() {
            Some(connection) => connection,
            None => return std::ptr::null_mut(),
        };

        let command = Command::CreateBrowser {
            width,
            height,
            url: url_string.to_string(),
        };
        let response = match send_command(connection, command) {
            Ok(response) => response,
            Err(error) => {
                log_to_file(&format!("create_browser IPC error: {}", error));
                return std::ptr::null_mut();
            }
        };

        match response {
            Response::BrowserCreated {
                browser_id,
                shared_memory_flink,
                d3d11_fence_handle,
                audio_shared_memory_flink,
            } => {
                log_to_file(&format!(
                    "browser created: id={}, shm={}, fence_handle=0x{:x}, audio_shm={}",
                    browser_id, shared_memory_flink, d3d11_fence_handle, audio_shared_memory_flink
                ));
                let shared_memory = match SharedMemoryReader::open(&shared_memory_flink) {
                    Ok(reader) => reader,
                    Err(error) => {
                        log_to_file(&format!("shm_open failed: {}", error));
                        return std::ptr::null_mut();
                    }
                };
                let audio_shared_memory = match AudioSharedMemoryReader::open(&audio_shared_memory_flink) {
                    Ok(reader) => Some(reader),
                    Err(error) => {
                        // 音声は必須ではないので open 失敗時は警告のみ。
                        log_to_file(&format!("audio_shm_open failed (audio disabled): {}", error));
                        None
                    }
                };
                #[cfg(target_os = "windows")]
                {
                    if d3d11_fence_handle != 0 {
                        // Unity の graphics backend に応じて開ける方を試す。
                        // D3D11/D3D12 双方無接続でも fence_handle 自体は同じ NT 共有 HANDLE。
                        if d3d11::is_connected() {
                            if let Err(error) = d3d11::open_fence(d3d11_fence_handle) {
                                log_to_file(&format!("d3d11::open_fence failed: {}", error));
                            }
                        }
                        if d3d12::is_connected() {
                            if let Err(error) = d3d12::open_fence(d3d11_fence_handle) {
                                log_to_file(&format!("d3d12::open_fence failed: {}", error));
                            }
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                let _ = d3d11_fence_handle;
                let instance = Box::new(ClientBrowserInstance {
                    browser_id,
                    shared_memory,
                    audio_shared_memory,
                    audio_flink: audio_shared_memory_flink.clone(),
                    #[cfg(target_os = "macos")]
                    native_voice: None,
                });
                Box::into_raw(instance) as *mut CefUnityBrowser
            }
            Response::Error { message } => {
                log_to_file(&format!("create_browser error: {}", message));
                std::ptr::null_mut()
            }
            _ => {
                log_to_file("unexpected response to CreateBrowser");
                std::ptr::null_mut()
            }
        }
    })
}

/// Destroy a browser instance.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_destroy_browser(handle: *mut CefUnityBrowser) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let mut instance = unsafe { Box::from_raw(handle as *mut ClientBrowserInstance) };
        stop_native_voice(&mut instance);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            let command = Command::DestroyBrowser {
                browser_id: instance.browser_id,
            };
            send_command_no_wait(connection, command);
        }
        drop(instance);
    })
}

/// Load a URL in the browser.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_load_url(handle: *mut CefUnityBrowser, url: *const c_char) {
    ffi_guard((), || {
        if handle.is_null() || url.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);
        let url_string = unsafe { CStr::from_ptr(url) }.to_str().unwrap_or("");

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            let command = Command::LoadUrl {
                browser_id: instance.browser_id,
                url: url_string.to_string(),
            };
            send_command_no_wait(connection, command);
        }
    })
}

/// Resize the browser viewport.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_resize(handle: *mut CefUnityBrowser, width: i32, height: i32) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            let command = Command::Resize {
                browser_id: instance.browser_id,
                width,
                height,
            };
            send_command_no_wait(connection, command);
        }
    })
}

/// Send a mouse move event.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_mouse_move(
    handle: *mut CefUnityBrowser,
    x: i32,
    y: i32,
    modifiers: u32,
) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
                Command::MouseMove {
                    browser_id: instance.browser_id,
                    x,
                    y,
                    modifiers,
                },
            );
        }
    })
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
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
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
    })
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
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
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
    })
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
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
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
    })
}

/// Execute JavaScript in the browser's main frame.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_execute_javascript(handle: *mut CefUnityBrowser, code: *const c_char) {
    ffi_guard((), || {
        if handle.is_null() || code.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);
        let code_string = unsafe { CStr::from_ptr(code) }.to_str().unwrap_or("");

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
                Command::ExecuteJavaScript {
                    browser_id: instance.browser_id,
                    code: code_string.to_string(),
                },
            );
        }
    })
}

/// Execute an editing command (copy, paste, cut, select_all, undo, redo).
/// command: 0=Copy, 1=Paste, 2=Cut, 3=SelectAll, 4=Undo, 5=Redo
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_edit_command(handle: *mut CefUnityBrowser, command: u8) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
                Command::EditCommand {
                    browser_id: instance.browser_id,
                    command,
                },
            );
        }
    })
}

/// Get the browser's current main-frame URL as UTF-8 bytes.
/// Returns the required buffer size including the trailing NUL terminator.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_url(
    handle: *mut CefUnityBrowser,
    buffer: *mut u8,
    buffer_length: i32,
) -> i32 {
    ffi_guard(0, || {
        if handle.is_null() {
            return 0;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(connection) = guard.as_ref() else {
            return 0;
        };

        let url = match send_command(
            connection,
            Command::GetCurrentUrl {
                browser_id: instance.browser_id,
            },
        ) {
            Ok(Response::CurrentUrl { url }) => url,
            Ok(Response::Error { message }) => {
                log_to_file(&format!("get_url error: {}", message));
                return 0;
            }
            Ok(other) => {
                log_to_file(&format!("get_url unexpected response: {:?}", other));
                return 0;
            }
            Err(error) => {
                log_to_file(&format!("get_url IPC error: {}", error));
                return 0;
            }
        };

        let bytes = url.as_bytes();
        let required = bytes.len() + 1;
        if !buffer.is_null() && buffer_length as usize >= required {
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len());
                *buffer.add(bytes.len()) = 0;
            }
        }

        required as i32
    })
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
    ffi_guard(0, || {
        if handle.is_null() || out_buffer.is_null() || out_width.is_null() || out_height.is_null() {
            return 0;
        }
        let instance = handle_to_reference(handle);

        match instance.shared_memory.get_active_buffer_pointer() {
            Some((pointer, width, height)) => {
                PAINT_COUNT.fetch_add(1, Ordering::Relaxed);
                unsafe {
                    *out_buffer = pointer;
                    *out_width = width as i32;
                    *out_height = height as i32;
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
    })
}

/// Read the IME caret rect from shared memory.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_ime_caret(
    handle: *mut CefUnityBrowser,
    out_x: *mut i32,
    out_y: *mut i32,
    out_width: *mut i32,
    out_height: *mut i32,
) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);
        let (x, y, width, height) = instance.shared_memory.read_ime_caret();
        unsafe {
            *out_x = x;
            *out_y = y;
            *out_width = width;
            *out_height = height;
        }
    })
}

// ---------------------------------------------------------------------------
// Audio: CEF → Unity PCM ストリーム
// ---------------------------------------------------------------------------

/// 現在の音声ストリームフォーマットを取得する。
/// 戻り値: ストリーム再生中なら 1、停止中/音声無効なら 0。
/// `out_sample_rate` / `out_channels` には最新のフォーマットを書き込む
/// (停止中でも直近の値が残る)。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_audio_format(
    handle: *mut CefUnityBrowser,
    out_sample_rate: *mut u32,
    out_channels: *mut u32,
) -> i32 {
    ffi_guard(0, || {
        if handle.is_null() {
            return 0;
        }
        let instance = handle_to_reference(handle);
        let Some(reader) = instance.audio_shared_memory.as_ref() else {
            return 0;
        };
        let (sample_rate, channels, active) = reader.format();
        unsafe {
            if !out_sample_rate.is_null() {
                *out_sample_rate = sample_rate;
            }
            if !out_channels.is_null() {
                *out_channels = channels;
            }
        }
        if active { 1 } else { 0 }
    })
}

/// 音声リングバッファから未読の PCM を読み出す。
///
/// `out_samples` には interleaved な f32 サンプル (LRLR... 順) を書き込む。
/// バッファは少なくとも `max_frames * channels` 個の f32 を保持できること
/// (channels は最大 [`cef_unity_ipc::AUDIO_MAX_CHANNELS`])。安全のため呼び出し側は
/// `max_frames * AUDIO_MAX_CHANNELS` 確保することを推奨。
///
/// `out_channels` には実際のチャネル数を書き込む。
/// 戻り値: 実際に読み出したフレーム数 (新規データが無ければ 0)。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_read_audio(
    handle: *mut CefUnityBrowser,
    out_samples: *mut f32,
    max_frames: i32,
    out_channels: *mut u32,
) -> i32 {
    ffi_guard(0, || {
        if handle.is_null() || out_samples.is_null() || max_frames <= 0 {
            return 0;
        }
        let instance = handle_to_reference(handle);
        let Some(reader) = instance.audio_shared_memory.as_mut() else {
            return 0;
        };
        let max_frames = max_frames as usize;
        // 出力スライス長は最悪ケース (AUDIO_MAX_CHANNELS) を仮定して構築する。
        // read() は実 channels に基づき max_frames*channels までしか書き込まない。
        let out = unsafe {
            std::slice::from_raw_parts_mut(out_samples, max_frames * cef_unity_ipc::AUDIO_MAX_CHANNELS)
        };
        let (frames, channels) = reader.read(out, max_frames);
        unsafe {
            if !out_channels.is_null() {
                *out_channels = channels as u32;
            }
        }
        frames as i32
    })
}

// ---------------------------------------------------------------------------
// Audio: ネイティブ出力 (CRI 方式)。Unity ミキサを迂回して OS オーディオ API に直結。
// 現状 macOS (AudioUnit) のみ。非対応 OS では start が -1 を返す。
// ---------------------------------------------------------------------------

/// ネイティブ音声出力を開始する。
/// 既存の `cef_unity_read_audio` (録画 tap) とはリングカーソルが独立しており併用可。
/// CefAudioOutput (Unity ミキサ再生) と同時に有効にすると二重再生になる。
///
/// `target_milliseconds`: jitter buffer の目標滞留量 (推奨 15)。
/// `io_frames`: CoreAudio IO バッファフレーム数 (推奨 128 ≈ 2.9ms)。0 以下は 128。
/// 戻り値: 0=成功 (既に再生中も 0)、-1=失敗 (音声無効・フォーマット未確定・
/// AU 起動失敗・非対応 OS)。フォーマット未確定で失敗するため、呼び出し側は
/// `cef_unity_get_audio_format` が 1 を返してから呼ぶこと。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_audio_native_start(
    handle: *mut CefUnityBrowser,
    target_milliseconds: f32,
    io_frames: i32,
) -> i32 {
    ffi_guard(-1, || {
        if handle.is_null() {
            return -1;
        }
        #[cfg(target_os = "macos")]
        {
            let instance = handle_to_reference(handle);
            if instance.native_voice.is_some() {
                return 0;
            }
            if instance.audio_flink.is_empty() {
                return -1;
            }
            match native_voice::NativeVoice::start(&instance.audio_flink, target_milliseconds, io_frames) {
                Ok(voice) => {
                    instance.native_voice = Some(voice);
                    log_to_file(&format!(
                        "native audio started (target={}ms io_frames={})",
                        target_milliseconds, io_frames
                    ));
                    0
                }
                Err(error) => {
                    log_to_file(&format!("native audio start failed: {}", error));
                    -1
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (target_milliseconds, io_frames);
            -1
        }
    })
}

/// ネイティブ音声出力を停止する (排水待ちして返る)。未開始なら何もしない。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_audio_native_stop(handle: *mut CefUnityBrowser) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            let instance = handle_to_reference(handle);
            if instance.native_voice.take().is_some() {
                log_to_file("native audio stopped");
            }
        }
    })
}

/// ネイティブ音声出力の音量 (0.0〜)。callback 内で乗算される。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_audio_native_set_volume(handle: *mut CefUnityBrowser, volume: f32) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            let instance = handle_to_reference(handle);
            if let Some(voice) = instance.native_voice.as_ref() {
                voice.set_volume(volume);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = volume;
        }
    })
}

/// ネイティブ音声出力の診断値を取得する。
/// `out_occupancy_milliseconds`: jitter buffer の滞留量 (ms)。
/// `out_underrun_frames` / `out_overflow_frames`: 累積フレーム数。
/// 戻り値: 0=再生中、-1=停止中/非対応 OS (out には書き込まない)。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_audio_native_statistics(
    handle: *mut CefUnityBrowser,
    out_occupancy_milliseconds: *mut f32,
    out_underrun_frames: *mut u64,
    out_overflow_frames: *mut u64,
) -> i32 {
    ffi_guard(-1, || {
        if handle.is_null() {
            return -1;
        }
        #[cfg(target_os = "macos")]
        {
            let instance = handle_to_reference(handle);
            let Some(voice) = instance.native_voice.as_ref() else {
                return -1;
            };
            let (occupancy_milliseconds, underrun, overflow) = voice.statistics();
            unsafe {
                if !out_occupancy_milliseconds.is_null() {
                    *out_occupancy_milliseconds = occupancy_milliseconds;
                }
                if !out_underrun_frames.is_null() {
                    *out_underrun_frames = underrun;
                }
                if !out_overflow_frames.is_null() {
                    *out_overflow_frames = overflow;
                }
            }
            0
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (out_occupancy_milliseconds, out_underrun_frames, out_overflow_frames);
            -1
        }
    })
}

// ---------------------------------------------------------------------------
// Blocking variants — wait for server response, return 0=ok / -1=error.
// ---------------------------------------------------------------------------

/// Helper: send a command and wait for Response. Returns 0 on Ok, -1 on error.
fn blocking_simple(connection: &ServerConnection, command: Command) -> i32 {
    match send_command(connection, command) {
        Ok(Response::Ok) => 0,
        Ok(Response::Error { message }) => {
            log_to_file(&format!("blocking command error: {}", message));
            -1
        }
        Ok(_) => 0,
        Err(error) => {
            log_to_file(&format!("blocking command IPC error: {}", error));
            -1
        }
    }
}

/// Destroy a browser instance (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_destroy_browser_blocking(handle: *mut CefUnityBrowser) -> i32 {
    ffi_guard(-1, || {
        if handle.is_null() {
            return -1;
        }
        let mut instance = unsafe { Box::from_raw(handle as *mut ClientBrowserInstance) };
        stop_native_voice(&mut instance);
        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        let result = if let Some(connection) = guard.as_ref() {
            blocking_simple(
                connection,
                Command::DestroyBrowser {
                    browser_id: instance.browser_id,
                },
            )
        } else {
            -1
        };
        drop(instance);
        result
    })
}

/// Load a URL in the browser (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_load_url_blocking(
    handle: *mut CefUnityBrowser,
    url: *const c_char,
) -> i32 {
    ffi_guard(-1, || {
        if handle.is_null() || url.is_null() {
            return -1;
        }
        let instance = handle_to_reference(handle);
        let url_string = unsafe { CStr::from_ptr(url) }.to_str().unwrap_or("");
        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            blocking_simple(
                connection,
                Command::LoadUrl {
                    browser_id: instance.browser_id,
                    url: url_string.to_string(),
                },
            )
        } else {
            -1
        }
    })
}

/// Resize the browser viewport (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_resize_blocking(
    handle: *mut CefUnityBrowser,
    width: i32,
    height: i32,
) -> i32 {
    ffi_guard(-1, || {
        if handle.is_null() {
            return -1;
        }
        let instance = handle_to_reference(handle);
        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            blocking_simple(
                connection,
                Command::Resize {
                    browser_id: instance.browser_id,
                    width,
                    height,
                },
            )
        } else {
            -1
        }
    })
}

/// Send a mouse move event (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_mouse_move_blocking(
    handle: *mut CefUnityBrowser,
    x: i32,
    y: i32,
    modifiers: u32,
) -> i32 {
    ffi_guard(-1, || {
        if handle.is_null() {
            return -1;
        }
        let instance = handle_to_reference(handle);
        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            blocking_simple(
                connection,
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
    })
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
    ffi_guard(-1, || {
        if handle.is_null() {
            return -1;
        }
        let instance = handle_to_reference(handle);
        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            blocking_simple(
                connection,
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
    })
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
    ffi_guard(-1, || {
        if handle.is_null() {
            return -1;
        }
        let instance = handle_to_reference(handle);
        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            blocking_simple(
                connection,
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
    })
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
    ffi_guard(-1, || {
        if handle.is_null() {
            return -1;
        }
        let instance = handle_to_reference(handle);
        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            blocking_simple(
                connection,
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
    })
}

/// Set IME composition text (preedit).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_ime_set_composition(
    handle: *mut CefUnityBrowser,
    text: *const c_char,
    selection_start: u32,
    selection_end: u32,
) {
    ffi_guard((), || {
        if handle.is_null() || text.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);
        let text_string = unsafe { CStr::from_ptr(text) }.to_str().unwrap_or("");

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
                Command::ImeSetComposition {
                    browser_id: instance.browser_id,
                    text: text_string.to_string(),
                    selection_start,
                    selection_end,
                },
            );
        }
    })
}

/// Commit IME text (finalize composition and insert text).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_ime_commit_text(handle: *mut CefUnityBrowser, text: *const c_char) {
    ffi_guard((), || {
        if handle.is_null() || text.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);
        let text_string = unsafe { CStr::from_ptr(text) }.to_str().unwrap_or("");

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
                Command::ImeCommitText {
                    browser_id: instance.browser_id,
                    text: text_string.to_string(),
                },
            );
        }
    })
}

/// Finish composing text (apply current composition).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_ime_finish_composing_text(
    handle: *mut CefUnityBrowser,
    keep_selection: i32,
) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
                Command::ImeFinishComposingText {
                    browser_id: instance.browser_id,
                    keep_selection: keep_selection != 0,
                },
            );
        }
    })
}

/// External BeginFrame: CEF Viz Compositor に次フレームの描画許可を出す。
/// Unity の Update 冒頭で呼ぶ。Init 時に WindowInfo::external_begin_frame_enabled=1
/// が立っているブラウザに対してのみ意味を持つ。fire-and-forget。
/// `unity_frame` には Time.frameCount を渡す。on_accelerated_paint 経由で shm に転送され、
/// `cef_unity_get_accelerated_paint_unity_frame` で読み取ることで end-to-end の遅延フレーム数を測れる。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_send_external_begin_frame(
    handle: *mut CefUnityBrowser,
    unity_frame: u64,
) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
                Command::SendExternalBeginFrame {
                    browser_id: instance.browser_id,
                    unity_frame,
                },
            );
        }
    })
}

/// 最後の on_accelerated_paint に対応する SendExternalBeginFrame 発行時の Unity frame
/// 番号を返す。Unity 側は `Time.frameCount - 戻り値` で end-to-end の遅延フレーム数
/// (BeginFrame 発行から実際にテクスチャが使えるようになるまで) を計算できる。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_accelerated_paint_unity_frame(handle: *mut CefUnityBrowser) -> u64 {
    ffi_guard(0, || {
        if handle.is_null() {
            return 0;
        }
        let instance = handle_to_reference(handle);
        instance.shared_memory.read_paint_unity_frame()
    })
}

/// accelerated paint の単調増加カウンタ (accelerated_frame_id) を消費せずに返す。
/// double-pump 同期に使う: flush BeginFrame の直前にこの値を記録し、flush 後に
/// この値を超えるまで待てば、flush が生成した最新 paint の IOSurface が
/// 受信ポートに届いていることが保証される (server は Mach 送信完了後に +1 する)。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_peek_accelerated_frame_id(handle: *mut CefUnityBrowser) -> u64 {
    ffi_guard(0, || {
        if handle.is_null() {
            return 0;
        }
        let instance = handle_to_reference(handle);
        instance.shared_memory.peek_accelerated_frame_id()
    })
}

/// Cancel the current IME composition.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_ime_cancel_composition(handle: *mut CefUnityBrowser) {
    ffi_guard((), || {
        if handle.is_null() {
            return;
        }
        let instance = handle_to_reference(handle);

        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            send_command_no_wait(
                connection,
                Command::ImeCancelComposition {
                    browser_id: instance.browser_id,
                },
            );
        }
    })
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

    fn cef_unity_release_metal_texture_objc(texture_pointer: *mut std::ffi::c_void);

    fn mach_iosurface_client_connect(service_name: *const std::ffi::c_char) -> i32;

    fn mach_iosurface_receive_texture(
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
    ffi_guard(0, || {
        static ACCELERATED_LOG_COUNT: AtomicU64 = AtomicU64::new(0);

        if handle.is_null()
            || out_surface_id.is_null()
            || out_width.is_null()
            || out_height.is_null()
            || out_format.is_null()
        {
            return 0;
        }
        let instance = handle_to_reference(handle);

        match instance.shared_memory.get_iosurface_info() {
            Some((surface_id, width, height, format)) => {
                let count = ACCELERATED_LOG_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                if count <= 5 || count % 100 == 0 {
                    log_to_file(&format!(
                        "get_iosurface_info #{}: surface_id={} {}x{} fmt={}",
                        count, surface_id, width, height, format
                    ));
                }
                unsafe {
                    *out_surface_id = surface_id;
                    *out_width = width as i32;
                    *out_height = height as i32;
                    *out_format = format;
                }
                1
            }
            None => 0,
        }
    })
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
    ffi_guard(std::ptr::null_mut(), || {
        #[cfg(target_os = "macos")]
        {
            unsafe { cef_unity_create_metal_texture_objc(surface_id, width, height, format) }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (surface_id, width, height, format);
            std::ptr::null_mut()
        }
    })
}

/// Receive the latest IOSurface from the server via Mach port and create a Metal texture.
/// Returns an opaque MTLTexture pointer, or null if no new frame.
/// The caller must release the returned texture with cef_unity_release_metal_texture.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_receive_iosurface_texture(
    out_width: *mut i32,
    out_height: *mut i32,
    out_format: *mut u32,
) -> *mut std::ffi::c_void {
    ffi_guard(std::ptr::null_mut(), || {
        if out_width.is_null() || out_height.is_null() || out_format.is_null() {
            return std::ptr::null_mut();
        }
        #[cfg(target_os = "macos")]
        {
            if !IOSURFACE_CONNECTED.load(Ordering::SeqCst) {
                return std::ptr::null_mut();
            }
            unsafe {
                mach_iosurface_receive_texture(out_width, out_height, out_format)
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (out_width, out_height, out_format);
            std::ptr::null_mut()
        }
    })
}


/// Returns 1 if the Mach IOSurface port channel is connected, 0 otherwise.
/// CPU モード (Init で use_gpu=0) のときは常に 0 を返す。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_is_iosurface_connected() -> i32 {
    ffi_guard(0, || {
        if !USE_GPU_MODE.load(Ordering::SeqCst) {
            return 0;
        }
        if IOSURFACE_CONNECTED.load(Ordering::SeqCst) { 1 } else { 0 }
    })
}

/// Release a Metal texture previously created by cef_unity_create_metal_texture.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_release_metal_texture(texture: *mut std::ffi::c_void) {
    ffi_guard((), || {
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
    })
}

// ---------------------------------------------------------------------------
// Log retrieval
// ---------------------------------------------------------------------------

static CACHED_LOGS: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// Retrieve server logs as NUL-separated UTF-8 entries.
/// If buffer is null, sends GetLogs via IPC, caches result, and returns required size.
/// If buffer is non-null, copies cached data into buffer and clears the cache.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_logs(buffer: *mut u8, buffer_length: i32) -> i32 {
    ffi_guard(0, || {
        if !INITIALIZED.load(Ordering::SeqCst) {
            return 0;
        }

        if buffer.is_null() {
            // Phase 1: fetch from server and cache
            let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(connection) = guard.as_ref() else {
                return 0;
            };

            let entries = match send_command(connection, Command::GetLogs) {
                Ok(Response::Logs { entries }) => entries,
                _ => return 0,
            };

            if entries.is_empty() {
                *CACHED_LOGS.lock().unwrap_or_else(PoisonError::into_inner) = None;
                return 0;
            }

            // Encode as "msg1\0msg2\0" (trailing NUL included)
            let mut encoded = Vec::new();
            for entry in &entries {
                encoded.extend_from_slice(entry.as_bytes());
                encoded.push(0);
            }

            let size = encoded.len() as i32;
            *CACHED_LOGS.lock().unwrap_or_else(PoisonError::into_inner) = Some(encoded);
            size
        } else {
            // Phase 2: copy cached data to buffer
            let mut cache = CACHED_LOGS.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(data) = cache.take() else {
                return 0;
            };

            let copy_length = data.len().min(buffer_length as usize);
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), buffer, copy_length);
            }
            copy_length as i32
        }
    })
}

/// Execute JavaScript in the browser's main frame (blocking).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_execute_javascript_blocking(
    handle: *mut CefUnityBrowser,
    code: *const c_char,
) -> i32 {
    ffi_guard(-1, || {
        if handle.is_null() || code.is_null() {
            return -1;
        }
        let instance = handle_to_reference(handle);
        let code_string = unsafe { CStr::from_ptr(code) }.to_str().unwrap_or("");
        let guard = CONNECTION.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(connection) = guard.as_ref() {
            blocking_simple(
                connection,
                Command::ExecuteJavaScript {
                    browser_id: instance.browser_id,
                    code: code_string.to_string(),
                },
            )
        } else {
            -1
        }
    })
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
    ffi_guard((), || {
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
    })
}

/// Unity Native Plugin Interface のアンロード。Unity が DLL アンロード時に呼ぶ。
#[unsafe(no_mangle)]
pub extern "C" fn UnityPluginUnload() {
    ffi_guard((), || {
        #[cfg(target_os = "windows")]
        {
            d3d11::clear_unity_interfaces();
            d3d12::clear_unity_interfaces();
        }
    })
}

/// Windows: 外部ホストの ID3D11Device を注入する (Unity 以外のホスト用)。
///
/// **`cef_unity_create_browser` より前に呼ぶこと。** browser 生成時に共有 fence を開く判定
/// (`is_d3d11_connected`) が走るため、後から注入すると GPU 同期が張られない。
///
/// デバイスの所有権は呼び出し側。こちら側は AddRef せず借用するだけなので、
/// `cef_unity_shutdown` まで生存させること。非 Windows では何もしない。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_set_external_d3d11_device(device: *mut std::ffi::c_void) {
    ffi_guard((), || {
        #[cfg(target_os = "windows")]
        {
            d3d11::set_external_device(device);
            log_to_file(&format!("external d3d11 device set: {:p}", device));
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = device;
        }
    })
}

/// Windows: Unity の D3D11 device に接続済みなら 1 を返す。
/// CPU モード (Init で use_gpu=0) のときは常に 0 を返す。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_is_d3d11_connected() -> i32 {
    ffi_guard(0, || {
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
    })
}

/// Windows: Unity の D3D12 device に接続済みなら 1 を返す。
/// C# 側はこちらが 1 のとき `cef_unity_receive_d3d12_texture` を呼ぶ。
/// CPU モード (Init で use_gpu=0) のときは常に 0 を返す。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_is_d3d12_connected() -> i32 {
    ffi_guard(0, || {
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
    })
}

/// Windows: 共有メモリから最新の D3D11 共有 HANDLE を読み出し、
/// Unity の D3D11Device で OpenSharedResource1 した ID3D11Texture2D* を返す。
/// 新フレームが無い場合は null。
///
/// 戻り値ポインタは内部で AddRef 済みのキャッシュであり、次に handle が変わるか
/// プラグイン unload までは Unity 側で再 AddRef せずに使ってよい。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_receive_d3d11_texture(
    handle: *mut CefUnityBrowser,
    out_width: *mut i32,
    out_height: *mut i32,
    out_format: *mut u32,
) -> *mut std::ffi::c_void {
    ffi_guard(std::ptr::null_mut(), || {
        if handle.is_null() || out_width.is_null() || out_height.is_null() || out_format.is_null() {
            return std::ptr::null_mut();
        }

        #[cfg(target_os = "windows")]
        {
            let instance = handle_to_reference(handle);
            let Some((handle_value, width, height, format, fence_value)) = instance.shared_memory.get_d3d11_handle()
            else {
                // 新フレーム無し: 前回開いたテクスチャを Unity 側で使い続けてもらう (null 返却)。
                return std::ptr::null_mut();
            };
            // GPU-side wait: Unity の immediate context に fence_value 到達待ちを発行する。
            // CPU はブロックせず、Unity の以降の描画コマンドが GPU 上で server.Copy 完了を待つ。
            if let Err(error) = d3d11::wait_fence(fence_value) {
                log_to_file(&format!("d3d11::wait_fence({}) failed: {}", fence_value, error));
            }
            let Some((texture_pointer, opened_width, opened_height)) = d3d11::open_or_cached(handle_value, width, height) else {
                return std::ptr::null_mut();
            };
            unsafe {
                *out_width = opened_width as i32;
                *out_height = opened_height as i32;
                *out_format = format;
            }
            PAINT_COUNT.fetch_add(1, Ordering::Relaxed);
            texture_pointer
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (handle, out_width, out_height, out_format);
            std::ptr::null_mut()
        }
    })
}

/// Windows: 共有メモリから最新の D3D11 共有 HANDLE を読み出し、
/// Unity の D3D12Device で OpenSharedHandle した ID3D12Resource* を返す。
/// KeyedMutex で server との排他とキャッシュコヒーレンスを取り、
/// 初回のみ COMMON → PIXEL_SHADER_RESOURCE 状態遷移を Unity に宣言する。
/// 新フレームが無い場合は null。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_receive_d3d12_texture(
    handle: *mut CefUnityBrowser,
    out_width: *mut i32,
    out_height: *mut i32,
    out_format: *mut u32,
) -> *mut std::ffi::c_void {
    ffi_guard(std::ptr::null_mut(), || {
        if handle.is_null() || out_width.is_null() || out_height.is_null() || out_format.is_null() {
            return std::ptr::null_mut();
        }

        #[cfg(target_os = "windows")]
        {
            let instance = handle_to_reference(handle);
            let Some((handle_value, width, height, format, fence_value)) = instance.shared_memory.get_d3d11_handle()
            else {
                // 新フレーム無し: 前回開いたテクスチャを Unity 側で使い続けてもらう (null 返却)。
                return std::ptr::null_mut();
            };
            // GPU-side wait: Unity の D3D12 queue に fence_value 到達待ちを発行する。
            // CPU はブロックせず、Unity の以降の queue 操作が GPU 上で server.Copy 完了を待つ。
            if let Err(error) = d3d12::wait_fence(fence_value) {
                log_to_file(&format!("d3d12::wait_fence({}) failed: {}", fence_value, error));
            }
            let Some((resource_pointer, opened_width, opened_height)) = d3d12::open_or_cached(handle_value, width, height) else {
                return std::ptr::null_mut();
            };
            unsafe {
                *out_width = opened_width as i32;
                *out_height = opened_height as i32;
                *out_format = format;
            }
            PAINT_COUNT.fetch_add(1, Ordering::Relaxed);
            resource_pointer
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (handle, out_width, out_height, out_format);
            std::ptr::null_mut()
        }
    })
}
