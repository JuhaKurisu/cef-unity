//! Windows の生スクロール入力 (Raw Input) モニタ。
//!
//! macOS の `scroll_monitor.m` (NSEvent ローカルモニタ) に対応する Windows 実装。
//! Unity の `Input.mouseScrollDelta` はフレーム境界で量子化されるため、
//! リサンプラ (`ScrollResampler`) に渡す生イベントをここで拾う。
//!
//! # 単位規約 (重要)
//!
//! `WM_INPUT` の `RI_MOUSE_WHEEL` は 120 = 1 ノッチ。既存の `ScrollInputEvent` は
//! macOS 由来で「precise = CSS ピクセル / 非 precise = ノッチ数」という規約なので、
//! **Windows は非 precise (`precise = 0`) として `値 / 120.0` をノッチ数で渡す**。
//! `WheelPixelsPerStep` の乗算は呼び出し側 (`ScrollResampler`) が行うため、
//! ここで掛けてはいけない (掛けると 120 倍のスクロールになる)。
//! 高精度ホイール (120 未満の刻みを送るデバイス) もそのまま比率で表現される。
//!
//! # フェーズ
//!
//! Windows の標準ホイールには macOS のようなジェスチャフェーズが無いので
//! `phase = 0` (None) 固定。フェーズ非対応プラットフォームはパイプライン側の
//! GraceTimeout で扱われる。
//!
//! # スレッド構成
//!
//! Raw Input はウィンドウメッセージで届くため、専用スレッドにメッセージ専用
//! ウィンドウ (`HWND_MESSAGE` の子) を作ってそこで受ける。受けたイベントは
//! Mutex 付きリングバッファに積み、Unity のメインスレッドが `poll` で drain する。

#![cfg(target_os = "windows")]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
    RIDEV_INPUTSINK, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
    GetMessageW, GetWindowThreadProcessId, HWND_MESSAGE, MSG, PostThreadMessageW, RegisterClassW,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_QUIT, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

use crate::CefScrollEvent;

/// リングバッファに保持する最大件数。60fps で 1 秒分を大きく超える余裕を持たせる。
/// 溢れた場合は最古から捨てる (最新の入力を優先する)。
pub const SCROLL_EVENT_CAPACITY: usize = 256;

/// Raw Input が報告するホイール 1 ノッチ分の値 (WHEEL_DELTA)。
const WHEEL_DELTA: f32 = 120.0;

/// `RAWMOUSE.usButtonFlags` のホイールビット (winuser.h)。
/// windows-rs のモジュールパスが版によって動くため、ここで定義して依存を避ける。
const RI_MOUSE_WHEEL: u16 = 0x0400;
const RI_MOUSE_HWHEEL: u16 = 0x0800;

// ---- リングバッファ ----

pub struct ScrollEventBuffer {
    events: Mutex<VecDeque<CefScrollEvent>>,
}

impl ScrollEventBuffer {
    pub const fn new() -> Self {
        Self {
            events: Mutex::new(VecDeque::new()),
        }
    }

    /// イベントを積む。容量を超えたら最古を捨てる。
    pub fn push(&self, event: CefScrollEvent) {
        let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        if events.len() >= SCROLL_EVENT_CAPACITY {
            events.pop_front();
        }
        events.push_back(event);
    }

    /// 積まれたイベントを FIFO 順に `destination` へ書き出し、書いた件数を返す。
    /// 書いた分はバッファから取り除く。
    pub fn drain_into(&self, destination: &mut [CefScrollEvent]) -> usize {
        let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        let count = destination.len().min(events.len());
        for slot in destination.iter_mut().take(count) {
            // count 件は上の min で保証済みなので unwrap しない形で取り出す
            if let Some(event) = events.pop_front() {
                *slot = event;
            }
        }
        count
    }

    pub fn clear(&self) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

static BUFFER: ScrollEventBuffer = ScrollEventBuffer::new();

/// タイムスタンプの基準時刻。`start` で 1 度だけ設定し、以後は同じクロックを使う
/// (イベントの timestamp と `now()` が同一クロックである必要がある)。
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Raw Input スレッドの実行状態。
static RUNNING: AtomicBool = AtomicBool::new(false);
/// Raw Input スレッドの OS スレッド ID (停止時に WM_QUIT を送る宛先)。
static MONITOR_THREAD_ID: AtomicU32 = AtomicU32::new(0);

static MONITOR_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

fn elapsed_seconds() -> f64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// イベント timestamp と同一クロックの現在時刻 (秒)。
pub fn now() -> f64 {
    elapsed_seconds()
}

// ---- Raw Input ウィンドウ ----

/// 前景ウィンドウが自プロセスのものか。
///
/// `RIDEV_INPUTSINK` を付けると非前景でも入力が届く。付けないとメッセージ専用
/// ウィンドウにはそもそも届かない (Raw Input はキーボードフォーカスを持つ
/// ウィンドウへ配送されるため) ので INPUTSINK は必須だが、そのままだと
/// Unity が背面にいるときのスクロールまで拾ってしまう。ここで front を確認して弾く。
fn foreground_window_is_ours() -> bool {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.0.is_null() {
            return false;
        }
        let mut process_id: u32 = 0;
        GetWindowThreadProcessId(foreground, Some(&mut process_id));
        process_id != 0 && process_id == GetCurrentProcessId()
    }
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_INPUT {
        handle_raw_input(lparam);
        // WM_INPUT は DefWindowProc にも渡す必要がある (クリーンアップのため)。
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn handle_raw_input(lparam: LPARAM) {
    if !foreground_window_is_ours() {
        return;
    }
    unsafe {
        let mut raw_input = RAWINPUT::default();
        let mut size = std::mem::size_of::<RAWINPUT>() as u32;
        let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let written = GetRawInputData(
            HRAWINPUT(lparam.0 as *mut _),
            RID_INPUT,
            Some(&mut raw_input as *mut _ as *mut _),
            &mut size,
            header_size,
        );
        // 失敗時は u32::MAX が返る
        if written == u32::MAX || written == 0 {
            return;
        }
        if raw_input.header.dwType != RIM_TYPEMOUSE.0 {
            return;
        }
        let mouse = &raw_input.data.mouse;
        let button_flags = mouse.Anonymous.Anonymous.usButtonFlags;
        let is_vertical = button_flags & RI_MOUSE_WHEEL != 0;
        let is_horizontal = button_flags & RI_MOUSE_HWHEEL != 0;
        if !is_vertical && !is_horizontal {
            return;
        }
        // usButtonData は符号付き 16bit (WHEEL_DELTA の倍数、下方向/左方向が負)
        let raw_delta = mouse.Anonymous.Anonymous.usButtonData as i16 as f32 / WHEEL_DELTA;
        let (delta_x, delta_y) = if is_vertical {
            (0.0, raw_delta)
        } else {
            (raw_delta, 0.0)
        };
        BUFFER.push(CefScrollEvent {
            timestamp: elapsed_seconds(),
            delta_x,
            delta_y,
            phase: 0,   // Windows の標準ホイールにジェスチャフェーズは無い
            precise: 0, // ノッチ数単位 (CSS ピクセルではない)
        });
    }
}

const WINDOW_CLASS_NAME: PCWSTR = w!("CefUnityScrollMonitorWindow");

/// メッセージ専用ウィンドウを作り、Raw Input を購読してメッセージループを回す。
/// `WM_QUIT` を受けるまで戻らない。
fn monitor_thread_main() {
    unsafe {
        let instance = match GetModuleHandleW(None) {
            Ok(handle) => handle,
            Err(_) => return,
        };

        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_procedure),
            hInstance: instance.into(),
            lpszClassName: WINDOW_CLASS_NAME,
            ..Default::default()
        };
        // 既に登録済み (2 回目の start) でも失敗するだけなので戻り値は見ない。
        RegisterClassW(&window_class);

        let window = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            WINDOW_CLASS_NAME,
            w!("CefUnityScrollMonitor"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        ) {
            Ok(window) => window,
            Err(_) => return,
        };

        // generic mouse (usage page 0x01 / usage 0x02) を購読する。
        let devices = [RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window,
        }];
        if RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_err()
        {
            let _ = DestroyWindow(window);
            return;
        }

        MONITOR_THREAD_ID.store(
            windows::Win32::System::Threading::GetCurrentThreadId(),
            Ordering::Release,
        );
        RUNNING.store(true, Ordering::Release);

        let mut message = MSG::default();
        // GetMessageW は WM_QUIT で 0 を返す
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        let _ = DestroyWindow(window);
        RUNNING.store(false, Ordering::Release);
    }
}

/// スクロールモニタを開始する。1 = 成功 / 0 = 失敗。
pub fn start() -> i32 {
    let mut guard = MONITOR_THREAD.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_some() {
        return 1; // 既に動作中
    }
    // タイムスタンプ基準を先に確定させる (スレッド開始前に now() が呼ばれても揃うように)
    let _ = EPOCH.get_or_init(Instant::now);
    BUFFER.clear();

    let handle = std::thread::Builder::new()
        .name("cef-scroll-monitor".to_string())
        .spawn(monitor_thread_main);
    let handle = match handle {
        Ok(handle) => handle,
        Err(_) => return 0,
    };

    // スレッドがウィンドウを作って RUNNING を立てるまで少し待つ。
    // 失敗 (RegisterRawInputDevices エラー等) の場合はスレッドが即終了する。
    for _ in 0..200 {
        if RUNNING.load(Ordering::Acquire) {
            *guard = Some(handle);
            crate::logging::write("scroll", "raw input monitor started");
            return 1;
        }
        if handle.is_finished() {
            crate::logging::write("scroll", "raw input monitor failed to start");
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // 立ち上がりを確認できなかった場合もハンドルは保持しておく (stop で回収する)
    *guard = Some(handle);
    crate::logging::write("scroll", "raw input monitor start timed out (kept running)");
    0
}

/// スクロールモニタを停止する。
pub fn stop() {
    let mut guard = MONITOR_THREAD.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(handle) = guard.take() else {
        return;
    };
    let thread_id = MONITOR_THREAD_ID.load(Ordering::Acquire);
    if thread_id != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
    let _ = handle.join();
    MONITOR_THREAD_ID.store(0, Ordering::Release);
    RUNNING.store(false, Ordering::Release);
    BUFFER.clear();
    crate::logging::write("scroll", "raw input monitor stopped");
}

/// 新着イベントを `out` へ書き、件数を返す。
///
/// # Safety
/// `out` は `max` 件を書ける有効なバッファであること。
pub unsafe fn poll(out: *mut CefScrollEvent, max: i32) -> i32 {
    if out.is_null() || max <= 0 {
        return 0;
    }
    let destination = unsafe { std::slice::from_raw_parts_mut(out, max as usize) };
    BUFFER.drain_into(destination) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(timestamp: f64, delta_y: f32) -> CefScrollEvent {
        CefScrollEvent {
            timestamp,
            delta_x: 0.0,
            delta_y,
            phase: 0,
            precise: 0,
        }
    }

    fn empty_destination(length: usize) -> Vec<CefScrollEvent> {
        (0..length).map(|_| event(0.0, 0.0)).collect()
    }

    #[test]
    fn drains_in_fifo_order_and_empties() {
        let buffer = ScrollEventBuffer::new();
        buffer.push(event(1.0, 1.0));
        buffer.push(event(2.0, -2.0));

        let mut destination = empty_destination(8);
        let count = buffer.drain_into(&mut destination);

        assert_eq!(count, 2, "積んだ件数が drain されること");
        assert_eq!(destination[0].timestamp, 1.0, "FIFO 順であること");
        assert_eq!(destination[1].delta_y, -2.0);
        assert_eq!(
            buffer.drain_into(&mut destination),
            0,
            "drain 後は空になること"
        );
    }

    #[test]
    fn drops_oldest_when_capacity_exceeded() {
        let buffer = ScrollEventBuffer::new();
        for index in 0..(SCROLL_EVENT_CAPACITY + 2) {
            buffer.push(event(index as f64, 1.0));
        }
        let mut destination = empty_destination(SCROLL_EVENT_CAPACITY);
        let count = buffer.drain_into(&mut destination);

        assert_eq!(count, SCROLL_EVENT_CAPACITY, "容量を超えて保持しないこと");
        assert_eq!(
            destination[0].timestamp, 2.0,
            "最古の 2 件が捨てられていること"
        );
    }

    #[test]
    fn drain_respects_destination_length() {
        let buffer = ScrollEventBuffer::new();
        for index in 0..5 {
            buffer.push(event(index as f64, 1.0));
        }
        let mut destination = empty_destination(2);
        assert_eq!(buffer.drain_into(&mut destination), 2, "宛先の長さで止まること");

        let mut rest = empty_destination(8);
        assert_eq!(buffer.drain_into(&mut rest), 3, "残りは次回に取れること");
        assert_eq!(rest[0].timestamp, 2.0, "続きから取れること");
    }

    /// ホイール値の単位換算。120 = 1 ノッチで、CSS ピクセル換算は
    /// 呼び出し側 (ScrollResampler) の責務なのでここでは掛けない。
    #[test]
    fn wheel_delta_converts_to_notches() {
        assert_eq!(120i16 as f32 / WHEEL_DELTA, 1.0, "120 は 1 ノッチ");
        assert_eq!(-240i16 as f32 / WHEEL_DELTA, -2.0, "負値は逆方向");
        assert_eq!(60i16 as f32 / WHEEL_DELTA, 0.5, "高精度ホイールは比率で表現");
    }
}
