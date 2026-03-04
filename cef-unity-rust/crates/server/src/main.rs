// CEF Server entry point.
//
// Runs CEF in its own process, communicates with Unity via ipc-channel + shared memory.
// Uses CFRunLoopTimer for periodic CEF pump + IPC polling on macOS.

mod server;

use std::io::Write;

use ipc_channel::ipc::{self as ipc_ch, IpcReceiver, IpcSender};
use ipc_channel::TryRecvError;

use cef_unity_ipc::{Bootstrap, Command, Response};

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
    cmd_rx: IpcReceiver<Command>,
    resp_tx: IpcSender<Response>,
    running: bool,
    pump_count: u64,
}

static mut SERVER_STATE: *mut ServerState = std::ptr::null_mut();

unsafe extern "C" fn timer_callback(_timer: CFRunLoopTimerRef, _info: *mut std::ffi::c_void) {
    let state = unsafe { &mut *SERVER_STATE };

    // Pump CEF message loop
    cef::do_message_loop_work();
    state.pump_count += 1;

    // Process commands from client
    loop {
        match state.cmd_rx.try_recv() {
            Ok(cmd) => {
                let is_shutdown = matches!(cmd, Command::Shutdown);
                log(&format!("received command: {:?}", cmd));
                let resp = state.cef_server.handle_command(cmd);
                if let Err(e) = state.resp_tx.send(resp) {
                    log(&format!("send error: {}", e));
                    state.running = false;
                    break;
                }
                if is_shutdown {
                    state.running = false;
                    break;
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::IpcError(e)) => {
                log(&format!("client disconnected: {}", e));
                state.running = false;
                break;
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

    // Parse --ipc-server argument
    let ipc_server_name = std::env::args()
        .skip_while(|a| a != "--ipc-server")
        .nth(1)
        .expect("--ipc-server argument required");
    log(&format!("ipc_server_name = {}", ipc_server_name));

    // Initialize CEF first (server must be ready before accepting connections)
    let cef_server = server::CefServer::new();
    if !cef_server.init_cef() {
        log("CEF initialization failed");
        std::process::exit(1);
    }
    log("CEF initialized successfully");

    // Create bidirectional channels
    let (cmd_tx, cmd_rx) = ipc_ch::channel::<Command>().expect("failed to create cmd channel");
    let (resp_tx, resp_rx) =
        ipc_ch::channel::<Response>().expect("failed to create resp channel");

    // Connect to client's one-shot server and send bootstrap
    let bootstrap_tx =
        IpcSender::connect(ipc_server_name).expect("failed to connect to client one-shot server");
    bootstrap_tx
        .send(Bootstrap {
            cmd_tx,
            resp_rx,
        })
        .expect("failed to send bootstrap");
    log("bootstrap sent to client");

    // Set up global state for timer callback
    let state = Box::new(ServerState {
        cef_server,
        cmd_rx,
        resp_tx,
        running: true,
        pump_count: 0,
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

    log("server exit");
}
