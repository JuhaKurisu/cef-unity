//! Windows の生スクロール入力モニタ (メッセージフック観測型)。
//!
//! macOS の `scroll_monitor.m` (NSEvent ローカルモニタ) に対応する Windows 実装。
//! Unity の `Input.mouseScrollDelta` はフレーム境界で量子化されるため、
//! パイプライン (`ScrollInputPipeline`) に渡す生イベントをここで拾う。
//!
//! # 実装方式: WH_GETMESSAGE フック (観測型) — Raw Input は使わないこと
//!
//! 旧実装 (v0.6.0 まで) は専用スレッドのメッセージ専用ウィンドウに
//! `RegisterRawInputDevices(RIDEV_INPUTSINK)` で generic mouse を購読していたが、
//! **Windows の Raw Input 登録はプロセス単位・同一デバイスクラスにつき
//! 「最後に登録した 1 ウィンドウ」だけが受領する仕様**のため、Unity Input System
//! の登録を上書きしてしまい、`Mouse.delta` (WM_INPUT の RAWMOUSE 由来) が全て
//! 0 になる不具合を起こした (カメラ回転が死ぬ)。MSDN も「ライブラリから
//! RegisterRawInputDevices を呼ぶとホストアプリの Raw Input 処理を妨げる」と
//! この構成を明示的に警告している。
//!
//! 現実装は `start()` を呼んだスレッド (= Unity のメインスレッド) に
//! `WH_GETMESSAGE` のスレッドスコープフックを張り、メッセージポンプが取り出す
//! `WM_MOUSEWHEEL` / `WM_MOUSEHWHEEL` を覗くだけの観測型。何も登録せず、
//! メッセージは `CallNextHookEx` でそのまま流すため、Unity の入力配送には
//! 一切影響しない (macOS の NSEvent ローカルモニタと対称の設計)。
//!
//! 旧実装にあった前景プロセス判定は不要になった: フックに流れてくるのは
//! 自プロセスのウィンドウへ OS が配送したメッセージだけで、他アプリ上の
//! スクロールが混入する経路が存在しない (INPUTSINK 固有の問題だった)。
//! アプリ内での位置ゲートは従来どおり C# 側 (`Drain` の overBrowser) が行う。
//!
//! # 呼び出しスレッド規約 (重要)
//!
//! `start()` は**対象ウィンドウを所有しメッセージポンプを回すスレッド**
//! (Unity のメインスレッド / Viewer のウィンドウスレッド) から呼ぶこと。
//! フックは呼び出しスレッドにのみ張られるため、別スレッドから呼ぶと
//! イベントが観測されない。`poll` は同スレッドから毎フレーム呼ぶ。
//!
//! # 単位規約 (重要)
//!
//! `WM_MOUSEWHEEL` の wParam 上位 16bit は 120 = 1 ノッチ。既存の
//! `ScrollInputEvent` は macOS 由来で「precise = CSS ピクセル / 非 precise =
//! ノッチ数」という規約なので、**Windows は非 precise (`precise = 0`) として
//! `値 / 120.0` をノッチ数で渡す**。`WheelPixelsPerStep` の乗算は呼び出し側
//! (`ScrollSmoother` 経路) が行うため、ここで掛けてはいけない (掛けると
//! 120 倍のスクロールになる)。高精度ホイール (120 未満の刻みを送るデバイス)
//! もそのまま比率で表現される。
//!
//! # フェーズ
//!
//! Windows の標準ホイールには macOS のようなジェスチャフェーズが無いので
//! `phase = 0` (None) 固定。フェーズ非対応プラットフォームはパイプライン側の
//! GraceTimeout で扱われる。

#![cfg(target_os = "windows")]

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Instant;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, HHOOK, MSG, PM_REMOVE, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_GETMESSAGE, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
};

use crate::CefScrollEvent;

/// リングバッファに保持する最大件数。60fps で 1 秒分を大きく超える余裕を持たせる。
/// 溢れた場合は最古から捨てる (最新の入力を優先する)。
pub const SCROLL_EVENT_CAPACITY: usize = 256;

/// ホイール 1 ノッチ分の値 (winuser.h の WHEEL_DELTA)。
const WHEEL_DELTA: f32 = 120.0;

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

/// インストール済みフックの HHOOK 値。`HHOOK` は `Send` でないため
/// 生ポインタ値 (isize) で保持する。`None` = 未インストール。
static INSTALLED_HOOK: Mutex<Option<isize>> = Mutex::new(None);

fn elapsed_seconds() -> f64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// イベント timestamp と同一クロックの現在時刻 (秒)。
pub fn now() -> f64 {
    elapsed_seconds()
}

// ---- メッセージフック ----

/// wParam の上位 16bit (符号付き WHEEL_DELTA 倍数) をノッチ数へ換算する。
/// 120 = 1 ノッチ、下方向/左方向が負。CSS ピクセル換算は C# 側の責務。
fn wheel_notches(wheel_parameter: usize) -> f32 {
    ((wheel_parameter >> 16) & 0xFFFF) as u16 as i16 as f32 / WHEEL_DELTA
}

/// ポンプが取り出した 1 メッセージを観測し、ホイールならリングバッファに積む。
fn handle_retrieved_message(message_id: u32, wheel_parameter: usize) {
    let is_vertical = message_id == WM_MOUSEWHEEL;
    let is_horizontal = message_id == WM_MOUSEHWHEEL;
    if !is_vertical && !is_horizontal {
        return;
    }
    let notches = wheel_notches(wheel_parameter);
    let (delta_x, delta_y) = if is_vertical {
        (0.0, notches)
    } else {
        (notches, 0.0)
    };
    BUFFER.push(CefScrollEvent {
        timestamp: elapsed_seconds(),
        delta_x,
        delta_y,
        phase: 0,   // Windows の標準ホイールにジェスチャフェーズは無い
        precise: 0, // ノッチ数単位 (CSS ピクセルではない)
    });
}

/// WH_GETMESSAGE フック本体。PM_REMOVE (実際に取り出された) のときだけ観測する
/// (PM_NOREMOVE の覗き見も通知されるため、見るだけだと二重計上になる)。
unsafe extern "system" fn get_message_hook_procedure(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code == HC_ACTION as i32 && wparam.0 == PM_REMOVE.0 as usize && lparam.0 != 0 {
        let message = unsafe { &*(lparam.0 as *const MSG) };
        handle_retrieved_message(message.message, message.wParam.0);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// スクロールモニタを開始する。1 = 成功 / 0 = 失敗。
///
/// 呼び出しスレッドに WH_GETMESSAGE フックを張るため、対象ウィンドウを所有し
/// メッセージポンプを回すスレッド (Unity のメインスレッド) から呼ぶこと。
pub fn start() -> i32 {
    let mut guard = INSTALLED_HOOK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if guard.is_some() {
        return 1; // 既に動作中
    }
    // タイムスタンプ基準を先に確定させる (フック前に now() が呼ばれても揃うように)
    let _ = EPOCH.get_or_init(Instant::now);
    BUFFER.clear();

    let hook = unsafe {
        SetWindowsHookExW(
            WH_GETMESSAGE,
            Some(get_message_hook_procedure),
            None, // 自プロセスのスレッドフックなのでモジュールハンドル不要
            GetCurrentThreadId(),
        )
    };
    match hook {
        Ok(handle) => {
            *guard = Some(handle.0 as isize);
            crate::logging::write("scroll", "message hook monitor started");
            1
        }
        Err(_) => {
            crate::logging::write("scroll", "message hook monitor failed to start");
            0
        }
    }
}

/// スクロールモニタを停止する (フックを外す)。
pub fn stop() {
    let mut guard = INSTALLED_HOOK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let Some(hook_handle_value) = guard.take() else {
        return;
    };
    unsafe {
        let _ = UnhookWindowsHookEx(HHOOK(hook_handle_value as *mut _));
    }
    BUFFER.clear();
    crate::logging::write("scroll", "message hook monitor stopped");
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

    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::{GetRegisteredRawInputDevices, RAWINPUTDEVICE};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_MESSAGE,
        PM_NOREMOVE, PeekMessageW, PostMessageW, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE,
        WINDOW_STYLE, WM_APP, WNDCLASSW,
    };
    use windows::core::w;

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
    /// C# 側 (WheelPixelsPerStep) の責務なのでここでは掛けない。
    #[test]
    fn wheel_parameter_converts_to_notches() {
        assert_eq!(wheel_notches(make_wheel_parameter(120)), 1.0, "120 は 1 ノッチ");
        assert_eq!(
            wheel_notches(make_wheel_parameter(-240)),
            -2.0,
            "負値は逆方向"
        );
        assert_eq!(
            wheel_notches(make_wheel_parameter(60)),
            0.5,
            "高精度ホイールは比率で表現"
        );
    }

    /// wParam 下位 16bit (修飾キーフラグ) はノッチ換算に影響しないこと。
    #[test]
    fn wheel_parameter_ignores_low_word_key_flags() {
        let with_key_flags = make_wheel_parameter(120) | 0x0008; // MK_CONTROL
        assert_eq!(wheel_notches(with_key_flags), 1.0);
    }

    /// WM_MOUSEWHEEL の wParam を組み立てる (上位 16bit = 符号付き delta)。
    fn make_wheel_parameter(delta: i16) -> usize {
        ((delta as u16 as usize) << 16) & 0xFFFF_0000
    }

    /// テスト用ウィンドウの window procedure (既定処理へ流すだけ)。
    /// `DefWindowProcW` は windows-rs では Rust ABI ラッパーなので直接
    /// `lpfnWndProc` に渡せず、extern "system" で包む必要がある。
    unsafe extern "system" fn test_window_procedure(
        window: HWND,
        message_id: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(window, message_id, wparam, lparam) }
    }

    /// テスト用のメッセージ専用ウィンドウ (フック対象スレッドの投函先)。
    fn create_message_window() -> HWND {
        unsafe {
            let instance = GetModuleHandleW(None).expect("GetModuleHandleW");
            let class_name = w!("CefUnityScrollMonitorTestWindow");
            let window_class = WNDCLASSW {
                lpfnWndProc: Some(test_window_procedure),
                hInstance: instance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            // 既に登録済み (別テスト実行後) でも失敗するだけなので戻り値は見ない。
            RegisterClassW(&window_class);
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!("CefUnityScrollMonitorTest"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance.into()),
                None,
            )
            .expect("CreateWindowExW")
        }
    }

    /// 溜まっているメッセージを全て取り出してディスパッチする
    /// (PeekMessageW の PM_REMOVE 取り出しで WH_GETMESSAGE フックが発火する)。
    fn pump_pending_messages(window: HWND) {
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, Some(window), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    /// 自プロセスに登録されている Raw Input デバイス数。
    fn registered_raw_input_device_count() -> u32 {
        unsafe {
            let mut device_count: u32 = 0;
            let result = GetRegisteredRawInputDevices(
                None,
                &mut device_count,
                std::mem::size_of::<RAWINPUTDEVICE>() as u32,
            );
            assert_eq!(result, 0, "件数問い合わせ自体は成功すること");
            device_count
        }
    }

    /// フック監視の一連のシナリオ。グローバル状態 (フック + BUFFER) を使うため
    /// 並行実行で干渉しないよう 1 テストにまとめている。
    #[test]
    fn hook_monitor_observes_wheel_messages_without_registering_raw_input() {
        assert_eq!(start(), 1, "開始できること");
        assert_eq!(start(), 1, "二重開始は成功扱い (冪等) であること");

        // ★回帰テスト (カメラ回転バグの再発防止):
        // モニタ開始で Raw Input を一切登録しないこと。登録するとプロセス単位の
        // 後勝ちルールでホストアプリ (Unity) の WM_INPUT 配送を奪ってしまう。
        assert_eq!(
            registered_raw_input_device_count(),
            0,
            "RegisterRawInputDevices を呼んでいないこと"
        );

        let window = create_message_window();
        let mut destination = empty_destination(8);

        unsafe {
            // 縦ホイール 2 ノッチ下 (delta = -240)
            PostMessageW(
                Some(window),
                WM_MOUSEWHEEL,
                WPARAM(make_wheel_parameter(-240)),
                LPARAM(0),
            )
            .expect("PostMessageW vertical");
            // PM_NOREMOVE の覗き見では計上されないこと (二重計上防止) を先に確認
            let mut peeked = MSG::default();
            let _ = PeekMessageW(&mut peeked, Some(window), 0, 0, PM_NOREMOVE);
        }
        assert_eq!(
            BUFFER.drain_into(&mut destination),
            0,
            "PM_NOREMOVE の覗き見では計上しないこと"
        );

        pump_pending_messages(window);
        assert_eq!(
            BUFFER.drain_into(&mut destination),
            1,
            "取り出しで 1 件だけ計上されること (覗き見と合わせて二重計上しない)"
        );
        assert_eq!(destination[0].delta_y, -2.0, "縦は delta_y に入ること");
        assert_eq!(destination[0].delta_x, 0.0);
        assert_eq!(destination[0].precise, 0, "ノッチ数単位 (非 precise) であること");
        assert_eq!(destination[0].phase, 0);

        unsafe {
            // 横ホイール 1 ノッチ右 (delta = +120) と、無関係なメッセージ
            PostMessageW(
                Some(window),
                WM_MOUSEHWHEEL,
                WPARAM(make_wheel_parameter(120)),
                LPARAM(0),
            )
            .expect("PostMessageW horizontal");
            PostMessageW(Some(window), WM_APP, WPARAM(0), LPARAM(0)).expect("PostMessageW app");
        }
        pump_pending_messages(window);
        assert_eq!(
            BUFFER.drain_into(&mut destination),
            1,
            "ホイール以外のメッセージは計上しないこと"
        );
        assert_eq!(destination[0].delta_x, 1.0, "横は delta_x に入ること");
        assert_eq!(destination[0].delta_y, 0.0);

        stop();
        unsafe {
            PostMessageW(
                Some(window),
                WM_MOUSEWHEEL,
                WPARAM(make_wheel_parameter(120)),
                LPARAM(0),
            )
            .expect("PostMessageW after stop");
        }
        pump_pending_messages(window);
        assert_eq!(
            BUFFER.drain_into(&mut destination),
            0,
            "停止後は観測しないこと"
        );

        // 再開できること (Unity の Play サイクルで start/stop が繰り返される)
        assert_eq!(start(), 1, "停止後に再開できること");
        stop();

        unsafe {
            let _ = DestroyWindow(window);
        }
    }
}
