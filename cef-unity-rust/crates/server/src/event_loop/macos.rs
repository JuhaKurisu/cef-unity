// macOS event loop: CFRunLoopTimer for periodic CEF pump + IPC polling.

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc;

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
    fn CFRunLoopTimerSetNextFireDate(timer: CFRunLoopTimerRef, fire_date: CFAbsoluteTime);
    fn CFAbsoluteTimeGetCurrent() -> CFAbsoluteTime;
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRunLoopWakeUp(rl: CFRunLoopRef);
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static mut SERVER_STATE: *mut ServerState = std::ptr::null_mut();

/// Global timer ref so BrowserProcessHandler can adjust it from any thread.
static TIMER: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Called from BrowserProcessHandler::on_schedule_message_pump_work.
/// Adjusts the timer to fire after `delay_ms` and wakes the run loop.
pub fn schedule_pump(delay_ms: i64) {
    let timer = TIMER.load(Ordering::Acquire);
    if timer.is_null() {
        return;
    }
    unsafe {
        let now = CFAbsoluteTimeGetCurrent();
        let delay = if delay_ms <= 0 {
            0.0
        } else {
            delay_ms as f64 / 1000.0
        };
        CFRunLoopTimerSetNextFireDate(timer, now + delay);
        CFRunLoopWakeUp(CFRunLoopGetMain());
    }
}

fn log(msg: &str) {
    crate::log(msg);
}

unsafe extern "C" fn timer_callback(_timer: CFRunLoopTimerRef, _info: *mut std::ffi::c_void) {
    let result = std::panic::catch_unwind(|| {
        timer_callback_inner();
    });
    if result.is_err() {
        log("timer_callback panicked, stopping run loop");
        let state = unsafe { &mut *SERVER_STATE };
        state.running = false;
        unsafe {
            CFRunLoopStop(CFRunLoopGetMain());
        }
    }
}

fn timer_callback_inner() {
    let state = unsafe { &mut *SERVER_STATE };

    if !state.running {
        unsafe {
            CFRunLoopStop(CFRunLoopGetMain());
        }
        return;
    }

    // IPC コマンドを先に処理 → マウスイベント等が同じ pump サイクルで CEF に反映される
    drain_commands(state);

    if !state.running {
        unsafe {
            CFRunLoopStop(CFRunLoopGetMain());
        }
        return;
    }

    cef::do_message_loop_work();
    state.pump_count += 1;
}

/// mpsc チャネルからコマンドを全て取り出して処理する。
fn drain_commands(state: &mut ServerState) {
    loop {
        match state.cmd_rx.try_recv() {
            Ok(env) => {
                let is_shutdown = matches!(env.command, cef_unity_ipc::Command::Shutdown);
                if env.expects_response {
                    log(&format!("received command: {:?}", env.command));
                }
                let resp = state.cef_server.handle_command(env.command);
                if env.expects_response
                    && let Err(e) = state.resp_tx.send(resp) {
                        log(&format!("send error: {}", e));
                        state.running = false;
                        break;
                    }
                if is_shutdown {
                    state.running = false;
                    break;
                }
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                log("IPC bridge disconnected");
                state.running = false;
                break;
            }
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
        // Short fallback interval for responsive JS execution.
        // CEF also controls timing via schedule_pump() for immediate work.
        let timer = CFRunLoopTimerCreate(
            std::ptr::null(),
            CFAbsoluteTimeGetCurrent(),
            0.004, // 4ms fallback interval (~250Hz)
            0,
            0,
            timer_callback,
            &mut ctx,
        );
        TIMER.store(timer, Ordering::Release);
        CFRunLoopAddTimer(CFRunLoopGetMain(), timer, kCFRunLoopDefaultMode);
    }

    log("entering CFRunLoop");
    unsafe {
        CFRunLoopRun();
    }

    TIMER.store(std::ptr::null_mut(), Ordering::Release);
    unsafe { *Box::from_raw(SERVER_STATE) }
}
