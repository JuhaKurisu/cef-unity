// Windows: Unity の ID3D11Device を IUnityGraphicsD3D11 経由で取得し、
// サーバが共有してきた NT 共有 HANDLE を OpenSharedResource1 で開く。
// 開いた ID3D11Texture2D* を Unity の Texture2D.CreateExternalTexture に渡す。
//
// 同期: KeyedMutex は使わず、共有 ID3D11Fence のみで同期する (Microsoft 公式アプローチ)。
//   - server が CopyResource + Flush + Signal(fence, value) する
//   - client は ID3D11DeviceContext4::Wait(fence, value) で **GPU-side** 待機してから
//     Unity が同じ immediate context でサンプル
// `ID3D11DeviceContext4::Wait` のドキュメントに "equivalent to the Direct3D 12
// ID3D12CommandQueue::Wait" と明記されている GPU 同期 API を利用する。
// これで CPU をブロックせずに「server の書き込み完了 → Unity の読み込み」の順序を保証できる。

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::sync::{Mutex, PoisonError};
use std::sync::atomic::{AtomicPtr, Ordering};


use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Device1, ID3D11Device5, ID3D11DeviceContext, ID3D11DeviceContext4,
    ID3D11Fence, ID3D11Texture2D,
};
use windows::core::Interface;

fn log_debug(message: &str) {
    crate::logging::write("d3d11", message);
}

// ---- Unity Native Plugin Interface (subset) ----
//
// Unity の IUnityInterfaces / IUnityGraphicsD3D11 は C 側でフィールドが関数ポインタの
// 構造体になっている (vtable 同等)。我々は Get* を呼ぶ "受け側" だけなので、
// 必要な関数ポインタだけを正しい順序で並べた最小定義で十分。

#[repr(C)]
#[derive(Copy, Clone)]
pub struct UnityInterfaceGUID {
    pub m_guid_high: u64,
    pub m_guid_low: u64,
}

#[repr(C)]
pub struct IUnityInterfaces {
    pub get_interface:
        unsafe extern "C" fn(guid: UnityInterfaceGUID) -> *mut c_void,
    pub register_interface:
        unsafe extern "C" fn(guid: UnityInterfaceGUID, ptr: *mut c_void),
    pub get_interface_split:
        unsafe extern "C" fn(high: u64, low: u64) -> *mut c_void,
    pub register_interface_split:
        unsafe extern "C" fn(high: u64, low: u64, ptr: *mut c_void),
}

#[repr(C)]
pub struct IUnityGraphicsD3D11 {
    pub get_device: unsafe extern "C" fn() -> *mut c_void, // ID3D11Device*
    // 残り (TextureFromRenderBuffer 等) は使わないので省略。順序が重要なため
    // 追加する場合は Unity 公式ヘッダの順番を厳守すること。
}

// Unity native plugin GUID (split form)
const UNITY_GRAPHICSD3D11_GUID_HIGH: u64 = 0xAAB3_7EF8_7A87_D748;
const UNITY_GRAPHICSD3D11_GUID_LOW: u64 = 0xBF76_967F_07EF_B177;

// ---- 状態 ----

/// Unity の ID3D11Device (生ポインタ)。所有権は Unity 側にあるため、
/// AddRef はせずただ参照だけする。
static UNITY_DEVICE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// UnityPluginLoad で受け取る IUnityInterfaces*。
/// IUnityGraphicsD3D11 の取得は Graphics 初期化後でないと NULL を返すため、
/// pointer を保持して必要なタイミングで lazily に問い合わせる。
static UNITY_INTERFACES: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

struct OpenedTexture {
    handle: u64,
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
}

/// 現在開いているテクスチャ (HANDLE 値とサイズで cache key)。
/// + 直前世代を 1 つ保持して、Unity が前フレームの ID3D11Texture2D* を
/// まだ参照中でも安全に解放できるようにする。
struct OpenedState {
    current: Option<OpenedTexture>,
    previous: Option<OpenedTexture>,
}

static OPENED: Mutex<OpenedState> = Mutex::new(OpenedState {
    current: None,
    previous: None,
});

/// 共有 ID3D11Fence の保持状態。`open_fence` で初期化、`wait_fence` で利用。
struct FenceState {
    fence: ID3D11Fence,
    /// Unity の immediate context (DeviceContext4 に cast 済み)。
    /// `Wait(fence, value)` を呼ぶ宛先で、open_fence 時に 1 度キャッシュする。
    context4: ID3D11DeviceContext4,
    /// 直近の Wait 完了値。これ以下の target は no-op で済ませる。
    last_waited: u64,
}

unsafe impl Send for FenceState {}

static FENCE: Mutex<Option<FenceState>> = Mutex::new(None);

// ---- Unity からのコールバック ----

pub fn set_unity_interfaces(unity_interfaces: *mut IUnityInterfaces) {
    UNITY_INTERFACES.store(unity_interfaces as *mut c_void, Ordering::Release);
    // Graphics device がまだ未初期化の段階で UnityPluginLoad が呼ばれることが
    // 多いので、ここでは D3D11 device の取得を試みるだけ。失敗しても問題ない。
    try_resolve_d3d11_device();
}

pub fn clear_unity_interfaces() {
    UNITY_INTERFACES.store(std::ptr::null_mut(), Ordering::Release);
    UNITY_DEVICE.store(std::ptr::null_mut(), Ordering::Release);
    {
        let mut state = OPENED.lock().unwrap_or_else(PoisonError::into_inner);
        state.current = None;
        state.previous = None;
    }
    *FENCE.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Unity 以外のホスト (CefUnity.Viewer 等) が自前の ID3D11Device を注入する。
///
/// デバイスの所有権は呼び出し側にあり、こちら側は AddRef せず借用するだけなので、
/// CEF shutdown まで生存させること。
pub fn set_external_device(device: *mut c_void) {
    UNITY_DEVICE.store(device, Ordering::Release);
}

/// 保持している IUnityInterfaces* から ID3D11Device を遅延取得する。
/// 取得に成功したら UNITY_DEVICE に格納する。既に取得済みの場合は何もしない。
fn try_resolve_d3d11_device() -> *mut c_void {
    let cached = UNITY_DEVICE.load(Ordering::Acquire);
    if !cached.is_null() {
        return cached;
    }
    let interfaces = UNITY_INTERFACES.load(Ordering::Acquire);
    if interfaces.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let interfaces = interfaces as *mut IUnityInterfaces;
        let graphics_d3d11_pointer = ((*interfaces).get_interface_split)(
            UNITY_GRAPHICSD3D11_GUID_HIGH,
            UNITY_GRAPHICSD3D11_GUID_LOW,
        );
        if graphics_d3d11_pointer.is_null() {
            return std::ptr::null_mut();
        }
        let graphics_d3d11 = graphics_d3d11_pointer as *mut IUnityGraphicsD3D11;
        let device = ((*graphics_d3d11).get_device)();
        if device.is_null() {
            return std::ptr::null_mut();
        }
        UNITY_DEVICE.store(device, Ordering::Release);
        device
    }
}

pub fn is_connected() -> bool {
    !try_resolve_d3d11_device().is_null()
}

// ---- shared fence (D3D11/D3D12 共通の GPU-side 同期) ----

/// 共有 ID3D11Fence を Unity の D3D11Device で開いてグローバルに保持する。
/// 同時に Unity の immediate context を `ID3D11DeviceContext4` として取得・キャッシュする
/// (`Wait` を発行するため)。`cef_unity_create_browser` が成功した直後に 1 度だけ呼ばれる想定。
pub fn open_fence(handle_value: u64) -> Result<(), String> {
    if handle_value == 0 {
        return Err("fence handle is 0".to_string());
    }
    let device_pointer = try_resolve_d3d11_device();
    if device_pointer.is_null() {
        return Err("Unity D3D11 device not yet available".to_string());
    }

    let device: ID3D11Device = unsafe {
        let raw = device_pointer;
        ID3D11Device::from_raw_borrowed(&raw)
            .ok_or_else(|| "ID3D11Device::from_raw_borrowed failed".to_string())?
            .clone()
    };
    let device5: ID3D11Device5 = device
        .cast()
        .map_err(|error| format!("cast ID3D11Device5 (Unity device): {:?}", error))?;
    let mut fence_option: Option<ID3D11Fence> = None;
    unsafe {
        device5
            .OpenSharedFence(HANDLE(handle_value as *mut _), &mut fence_option)
            .map_err(|error| format!("OpenSharedFence: {:?}", error))?;
    }
    let fence: ID3D11Fence = fence_option.ok_or_else(|| "OpenSharedFence returned None".to_string())?;

    // Unity の immediate context を `Wait` 発行用に取得。
    let context: ID3D11DeviceContext = unsafe {
        device
            .GetImmediateContext()
            .map_err(|error| format!("GetImmediateContext: {:?}", error))?
    };
    let context4: ID3D11DeviceContext4 = context
        .cast()
        .map_err(|error| format!("cast ID3D11DeviceContext4 (Unity context): {:?}", error))?;

    *FENCE.lock().unwrap_or_else(PoisonError::into_inner) = Some(FenceState {
        fence,
        context4,
        last_waited: 0,
    });
    log_debug(&format!(
        "open_fence: opened handle=0x{:x}",
        handle_value
    ));
    Ok(())
}

/// `wait_fence` / `open_or_cached` を最初に呼んだ OS スレッド ID。
///
/// `ID3D11DeviceContext` は非スレッドセーフな COM で、Unity は自身の render thread から
/// これを使う。我々が別スレッド (C# のメインスレッド) から `Wait` を発行すると競合し得るため、
/// 実際に単一スレッドから呼ばれているかを実測できるようにする。
static CONTEXT_CALLER_THREAD_ID: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// immediate context を触る関数の入口で呼び、呼び出しスレッドが変わったら警告を残す。
fn record_context_caller_thread(function_name: &str) {
    let current = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    let previous = CONTEXT_CALLER_THREAD_ID.swap(current, Ordering::Relaxed);
    if previous != 0 && previous != current {
        log_debug(&format!(
            "WARNING: {} が別スレッドから呼ばれた ({} -> {})。\
             ID3D11DeviceContext は非スレッドセーフのため Unity の render thread と競合し得る",
            function_name, previous, current
        ));
    }
}

/// Unity の immediate context に "fence が `target_value` に到達するまで以降の
/// GPU ワークを保留" を指示する (GPU-side wait)。CPU はブロックしない。
/// fence 未対応 (open_fence 未呼び出し) の場合は no-op。
///
/// **呼び出しスレッド規約: Unity のメインスレッドからのみ呼ぶこと。**
/// immediate context は非スレッドセーフなので、複数スレッドから触ると
/// DEVICE_REMOVED 系の障害になり得る (D3D12 の `ID3D12CommandQueue::Wait` は
/// スレッドセーフなので d3d12.rs 側にこの制約は無い)。
pub fn wait_fence(target_value: u64) -> Result<(), String> {
    if target_value == 0 {
        return Ok(());
    }
    record_context_caller_thread("wait_fence");
    let mut guard = FENCE.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(state) = guard.as_mut() else {
        return Ok(()); // fence 未対応経路
    };
    if target_value <= state.last_waited {
        return Ok(());
    }
    unsafe {
        state
            .context4
            .Wait(&state.fence, target_value)
            .map_err(|error| format!("ID3D11DeviceContext4::Wait({}): {:?}", target_value, error))?;
    }
    state.last_waited = target_value;
    Ok(())
}

// ---- HANDLE → ID3D11Texture2D ----

/// shm 上の HANDLE 値を Unity の D3D11Device で OpenSharedResource1 する。
/// 同じ HANDLE 値なら cache 内のものを返す (KeyedMutex は使わないので Acquire/Release なし)。
///
/// 同期は呼び出し側で `wait_fence(fence_value)` を呼ぶことで GPU-side に提供される。
/// 返したポインタは次に open_or_cached が呼ばれて HANDLE が変わるか、
/// clear_unity_interfaces が呼ばれるまで有効。
///
/// **呼び出しスレッド規約: `wait_fence` と同じく Unity のメインスレッドからのみ呼ぶこと。**
pub fn open_or_cached(
    handle_value: u64,
    width: u32,
    height: u32,
) -> Option<(*mut c_void, u32, u32)> {
    if handle_value == 0 {
        return None;
    }
    let device_pointer = try_resolve_d3d11_device();
    if device_pointer.is_null() {
        return None;
    }

    let mut state = OPENED.lock().unwrap_or_else(PoisonError::into_inner);

    let cache_hit = matches!(
        state.current.as_ref(),
        Some(cached) if cached.handle == handle_value && cached.width == width && cached.height == height
    );

    if !cache_hit {
        let device: ID3D11Device = unsafe {
            let raw = device_pointer;
            match ID3D11Device::from_raw_borrowed(&raw) {
                Some(device) => device.clone(),
                None => {
                    log_debug(&format!(
                        "open_or_cached: from_raw_borrowed failed (device_pointer={:p})",
                        device_pointer
                    ));
                    return None;
                }
            }
        };
        let device1: ID3D11Device1 = match device.cast() {
            Ok(device1) => device1,
            Err(error) => {
                log_debug(&format!("cast to ID3D11Device1 failed: {:?}", error));
                return None;
            }
        };

        let handle = HANDLE(handle_value as *mut _);
        let texture: ID3D11Texture2D = match unsafe { device1.OpenSharedResource1(handle) } {
            Ok(texture) => texture,
            Err(error) => {
                log_debug(&format!(
                    "OpenSharedResource1 failed for handle=0x{:x}: {:?}",
                    handle_value, error
                ));
                return None;
            }
        };
        log_debug(&format!(
            "opened handle=0x{:x} texture={:p} {}x{}",
            handle_value,
            texture.as_raw(),
            width,
            height
        ));

        let new_entry = OpenedTexture {
            handle: handle_value,
            texture,
            width,
            height,
        };
        let old_current = state.current.take();
        state.previous = old_current;
        state.current = Some(new_entry);
    }

    let current = state.current.as_ref()?;
    Some((current.texture.as_raw(), current.width, current.height))
}
