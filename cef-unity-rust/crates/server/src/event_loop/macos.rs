// macOS event loop: CFRunLoopTimer for periodic CEF pump + IPC polling.

use ipc_channel::TryRecvError;

use cef_unity_ipc::Command;

use super::ServerState;

// ---------------------------------------------------------------------------
// CoreFoundation FFI
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
// Global mutable state for timer callback
// ---------------------------------------------------------------------------

static mut SERVER_STATE: *mut ServerState = std::ptr::null_mut();

fn log(msg: &str) {
    crate::log(msg);
}

unsafe extern "C" fn timer_callback(_timer: CFRunLoopTimerRef, _info: *mut std::ffi::c_void) {
    // catch_unwind で保護: extern "C" コールバック内の panic は abort を引き起こすため
    let result = std::panic::catch_unwind(|| {
        timer_callback_inner();
    });
    if result.is_err() {
        log("timer_callback panicked, stopping run loop");
        let state = unsafe { &mut *SERVER_STATE };
        state.running = false;
        unsafe { CFRunLoopStop(CFRunLoopGetMain()); }
    }
}

fn timer_callback_inner() {
    let state = unsafe { &mut *SERVER_STATE };

    if !state.running {
        unsafe { CFRunLoopStop(CFRunLoopGetMain()); }
        return;
    }

    cef::do_message_loop_work();
    state.pump_count += 1;

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

pub fn run_event_loop(state: ServerState) -> ServerState {
    let boxed = Box::new(state);
    unsafe {
        SERVER_STATE = Box::into_raw(boxed);
    }

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

    unsafe { *Box::from_raw(SERVER_STATE) }
}
