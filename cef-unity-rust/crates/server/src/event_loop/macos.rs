// macOS event loop: CFRunLoopTimer for periodic CEF pump + IPC polling.

use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
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

/// tick panic 後の停止要求。panic 直後に SERVER_STATE への `&mut` を再作成しない
/// ための伝達路 (次の tick 冒頭でこれを見てループを止める)。
static PANICKED: AtomicBool = AtomicBool::new(false);

// tick 再入検出。CEF がネストした run loop (モーダル等) を回すと timer が
// 再発火し得るため、その場合は SERVER_STATE への `&mut` を二重に作らず戻す。
thread_local! {
    static IN_TICK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

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
    if IN_TICK.with(|f| f.replace(true)) {
        return; // 再入 (ネスト run loop からの再発火) — &mut の二重作成を避ける
    }
    let result = std::panic::catch_unwind(|| {
        timer_callback_inner();
    });
    IN_TICK.with(|f| f.set(false));
    if let Err(payload) = result {
        // payload を捨てず原因をログに残す (&str / String 以外は型名不明のため定型文)
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        log(&format!("timer_callback panicked: {}", msg));
        PANICKED.store(true, Ordering::Release);
        unsafe {
            CFRunLoopStop(CFRunLoopGetMain());
        }
    }
}

fn timer_callback_inner() {
    if PANICKED.load(Ordering::Acquire) {
        // panic 後に stop 前へ滑り込んだ tick — 状態には触れず止め直すだけ
        unsafe {
            CFRunLoopStop(CFRunLoopGetMain());
        }
        return;
    }
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

    // server-side flush: 保留中の BeginFrame#2 (flush) を発行時刻が来ていれば撃つ。
    // do_message_loop_work の前に行い、同じ pump サイクルで compositor が draw する。
    state.cef_server.process_pending_flushes();

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
        // Short fallback interval for responsive JS execution + low BeginFrame→paint latency.
        // CEF also controls timing via schedule_pump() for immediate work.
        // 1ms (1000Hz) は External BeginFrame モードで Unity の同フレーム取得 (0 遅延) を狙う際に必要。
        // CFRunLoopTimer は 1ms の精度を持つので CPU 負荷上昇は限定的。
        let timer = CFRunLoopTimerCreate(
            std::ptr::null(),
            CFAbsoluteTimeGetCurrent(),
            0.001, // 1ms fallback interval (~1000Hz)
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
