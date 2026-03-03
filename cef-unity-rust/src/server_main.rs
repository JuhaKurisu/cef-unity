// CEF Server entry point.
//
// Runs CEF in its own process, communicates with Unity via Unix domain socket + shared memory.
// Uses CFRunLoopTimer for periodic CEF pump + socket polling on macOS.

use std::io::Write;
use std::os::unix::net::UnixListener;

use cef_unity_rust::ipc::Command;
use cef_unity_rust::server;

fn log(msg: &str) {
    let path = std::env::temp_dir().join("cef_unity_server.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{:?}] {}", std::time::SystemTime::now(), msg);
    }
}

// ---------------------------------------------------------------------------
// CoreFoundation FFI for macOS run loop
// ---------------------------------------------------------------------------

#[allow(non_camel_case_types)]
type CFRunLoopRef = *mut std::ffi::c_void;
#[allow(non_camel_case_types)]
type CFRunLoopTimerRef = *mut std::ffi::c_void;
#[allow(non_camel_case_types)]
type CFStringRef = *const std::ffi::c_void;
#[allow(non_camel_case_types)]
type CFTimeInterval = f64;
#[allow(non_camel_case_types)]
type CFAbsoluteTime = f64;
#[allow(non_camel_case_types)]
type CFIndex = isize;
#[allow(non_camel_case_types)]
type CFOptionFlags = u64;
#[allow(non_camel_case_types)]
type CFAllocatorRef = *const std::ffi::c_void;
#[allow(non_camel_case_types)]
type CFRunLoopTimerCallBack =
    unsafe extern "C" fn(timer: CFRunLoopTimerRef, info: *mut std::ffi::c_void);

#[repr(C)]
#[allow(non_camel_case_types)]
struct CFRunLoopTimerContext {
    version: CFIndex,
    info: *mut std::ffi::c_void,
    retain: *const std::ffi::c_void,
    release: *const std::ffi::c_void,
    copy_description: *const std::ffi::c_void,
}

unsafe extern "C" {
    static kCFRunLoopDefaultMode: CFStringRef;
    fn CFRunLoopGetMain() -> CFRunLoopRef;
    fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    fn CFRunLoopTimerCreate(
        allocator: CFAllocatorRef,
        fire_date: CFAbsoluteTime,
        interval: CFTimeInterval,
        flags: CFOptionFlags,
        order: CFIndex,
        callout: CFRunLoopTimerCallBack,
        context: *mut CFRunLoopTimerContext,
    ) -> CFRunLoopTimerRef;
    fn CFAbsoluteTimeGetCurrent() -> CFAbsoluteTime;
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
}

// ---------------------------------------------------------------------------
// Global mutable state for the timer callback
// ---------------------------------------------------------------------------

struct ServerState {
    cef_server: server::CefServer,
    listener: UnixListener,
    client: Option<server::ClientConnection>,
    running: bool,
    pump_count: u64,
    socket_path: String,
}

static mut SERVER_STATE: *mut ServerState = std::ptr::null_mut();

unsafe extern "C" fn timer_callback(_timer: CFRunLoopTimerRef, _info: *mut std::ffi::c_void) {
    let state = unsafe { &mut *SERVER_STATE };

    // Pump CEF message loop
    cef::do_message_loop_work();
    state.pump_count += 1;

    // Accept new connection
    if state.client.is_none() {
        match state.listener.accept() {
            Ok((stream, _)) => {
                log("client connected");
                match server::ClientConnection::new(stream) {
                    Ok(conn) => state.client = Some(conn),
                    Err(e) => log(&format!("failed to setup client: {}", e)),
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => log(&format!("accept error: {}", e)),
        }
    }

    // Process commands from connected client
    if let Some(ref mut conn) = state.client {
        loop {
            match conn.try_recv() {
                Ok(Some(cmd)) => {
                    let is_shutdown = matches!(cmd, Command::Shutdown);
                    log(&format!("received command: {:?}", cmd));
                    let resp = state.cef_server.handle_command(cmd);
                    if let Err(e) = conn.send(&resp) {
                        log(&format!("send error: {}", e));
                        state.client = None;
                        break;
                    }
                    if is_shutdown {
                        state.running = false;
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    log(&format!("client disconnected: {}", e));
                    state.client = None;
                    state.running = false;
                    break;
                }
            }
        }
    }

    if !state.running {
        unsafe {
            CFRunLoopStop(CFRunLoopGetMain());
        }
    }
}

fn main() {
    let _ = std::fs::write(std::env::temp_dir().join("cef_unity_server.log"), "");
    log(&format!("server started, pid={}", std::process::id()));

    // Parse --socket-path argument
    let socket_path = std::env::args()
        .skip_while(|a| a != "--socket-path")
        .nth(1)
        .expect("--socket-path argument required");
    log(&format!("socket_path = {}", socket_path));

    // Remove stale socket file
    let _ = std::fs::remove_file(&socket_path);

    // Initialize CEF
    let cef_server = server::CefServer::new();
    if !cef_server.init_cef() {
        log("CEF initialization failed");
        std::process::exit(1);
    }
    log("CEF initialized successfully");

    // Bind Unix domain socket
    let listener = UnixListener::bind(&socket_path).expect("failed to bind socket");
    listener.set_nonblocking(true).unwrap();
    log("socket bound, waiting for connections");

    // Set up global state for timer callback
    let state = Box::new(ServerState {
        cef_server,
        listener,
        client: None,
        running: true,
        pump_count: 0,
        socket_path: socket_path.clone(),
    });
    unsafe {
        SERVER_STATE = Box::into_raw(state);
    }

    // Create a CFRunLoopTimer that fires every 4ms (~250Hz)
    unsafe {
        let mut ctx = CFRunLoopTimerContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        };
        let timer = CFRunLoopTimerCreate(
            std::ptr::null(),
            CFAbsoluteTimeGetCurrent(),
            0.004, // 4ms interval
            0,
            0,
            timer_callback,
            &mut ctx,
        );
        CFRunLoopAddTimer(CFRunLoopGetMain(), timer, kCFRunLoopDefaultMode);
    }

    log("entering CFRunLoop");
    unsafe {
        CFRunLoopRun();
    }

    // Cleanup
    let state = unsafe { Box::from_raw(SERVER_STATE) };
    log(&format!("shutting down after {} pumps", state.pump_count));
    let mut cef_server = state.cef_server;
    cef_server.shutdown();

    let _ = std::fs::remove_file(&state.socket_path);
    log("server exit");
}
