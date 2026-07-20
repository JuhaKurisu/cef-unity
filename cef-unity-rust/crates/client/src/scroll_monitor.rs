//! scroll_monitor.m (NSEvent ローカルモニタ) の Rust バインディング。
//! Unity メインスレッド == AppKit メインスレッドで poll する前提 (ロック無し)。
//! FFI 公開は lib.rs 側 (csbindgen が lib.rs のみを走査するため)。

/// scroll_monitor.m の scroll_event_t / lib.rs の CefScrollEvent と同一レイアウト。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawScrollEvent {
    pub timestamp: f64,
    pub dx: f32,
    pub dy: f32,
    pub phase: u8,
    pub precise: u8,
}

unsafe extern "C" {
    pub fn cef_scroll_monitor_start_impl() -> i32;
    pub fn cef_scroll_monitor_stop_impl();
    pub fn cef_scroll_monitor_poll_impl(out: *mut RawScrollEvent, max: i32) -> i32;
    pub fn cef_scroll_monitor_now_impl() -> f64;
}
