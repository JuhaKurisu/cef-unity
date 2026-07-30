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
    fn CFRunLoopAddTimer(run_loop: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
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
    fn CFRunLoopStop(run_loop: CFRunLoopRef);
    fn CFRunLoopWakeUp(run_loop: CFRunLoopRef);
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
/// Adjusts the timer to fire after `delay_milliseconds` and wakes the run loop.
pub fn schedule_pump(delay_milliseconds: i64) {
    let timer = TIMER.load(Ordering::Acquire);
    if timer.is_null() {
        return;
    }
    unsafe {
        let now = CFAbsoluteTimeGetCurrent();
        let delay = if delay_milliseconds <= 0 {
            0.0
        } else {
            delay_milliseconds as f64 / 1000.0
        };
        CFRunLoopTimerSetNextFireDate(timer, now + delay);
        CFRunLoopWakeUp(CFRunLoopGetMain());
    }
}

fn log(message: &str) {
    crate::log(message);
}

unsafe extern "C" fn timer_callback(_timer: CFRunLoopTimerRef, _info: *mut std::ffi::c_void) {
    if IN_TICK.with(|flag| flag.replace(true)) {
        return; // 再入 (ネスト run loop からの再発火) — &mut の二重作成を避ける
    }
    let result = std::panic::catch_unwind(|| {
        timer_callback_inner();
    });
    IN_TICK.with(|flag| flag.set(false));
    if let Err(payload) = result {
        // payload を捨てず原因をログに残す (&str / String 以外は型名不明のため定型文)
        let message = payload
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        log(&format!("timer_callback panicked: {}", message));
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

    // 1 秒窓の paint 統計 (pump tick 数 / paint 数 / GPU コピー待ち) を出す。
    // tick から呼ぶため、コピー待ちで pump が凍結した分は窓長の伸びとして現れる。
    crate::server::report_paint_statistics(state.pump_count);
}

/// mpsc チャネルからコマンドを全て取り出して処理する。
///
/// issue #13: キューが空になるまで無制限に drain するため、pump が止まっていた間に
/// 溜まった BeginFrame がまとめて CEF へ送られる。1 tick 分の drain 数を計測する。
fn drain_commands(state: &mut ServerState) {
    let mut command_count: u64 = 0;
    let mut begin_frame_count: u64 = 0;
    loop {
        match state.command_receiver.try_recv() {
            Ok(envelope) => {
                command_count += 1;
                if matches!(
                    envelope.command,
                    cef_unity_ipc::Command::SendExternalBeginFrame { .. }
                ) {
                    begin_frame_count += 1;
                }
                let is_shutdown = matches!(envelope.command, cef_unity_ipc::Command::Shutdown);
                if envelope.expects_response {
                    log(&format!("received command: {:?}", envelope.command));
                }
                let response = state.cef_server.handle_command(envelope.command);
                if envelope.expects_response
                    && let Err(error) = state.response_sender.send(response) {
                        log(&format!("send error: {}", error));
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
    crate::server::record_drain_burst(command_count, begin_frame_count);
}

pub fn run_event_loop(state: ServerState) -> ServerState {
    let boxed = Box::new(state);
    unsafe {
        SERVER_STATE = Box::into_raw(boxed);
    }

    unsafe {
        let mut context = CFRunLoopTimerContext {
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
        // issue #12 の A/B 用に fallback 間隔を環境変数で上書きできるようにする
        // (既定は従来どおり 1ms)。CEF 要求駆動 (schedule_pump) は常に併存する。
        let interval_milliseconds: f64 = std::env::var("CEF_UNITY_PUMP_INTERVAL_MILLISECONDS")
            .ok()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| *value > 0.0)
            .unwrap_or(1.0);
        log(&format!(
            "pump fallback interval = {}ms",
            interval_milliseconds
        ));
        let timer = CFRunLoopTimerCreate(
            std::ptr::null(),
            CFAbsoluteTimeGetCurrent(),
            interval_milliseconds / 1000.0,
            0,
            0,
            timer_callback,
            &mut context,
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
