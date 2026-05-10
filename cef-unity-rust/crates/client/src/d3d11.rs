// Windows: Unity の ID3D11Device を IUnityGraphicsD3D11 経由で取得し、
// サーバが共有してきた NT 共有 HANDLE を OpenSharedResource1 で開く。
// 開いた ID3D11Texture2D* を Unity の Texture2D.CreateExternalTexture に渡す。

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

use std::io::Write;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Device1, ID3D11Device5, ID3D11Fence, ID3D11Texture2D,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::Interface;

/// fence Wait のタイムアウト (ms)。Unity を長時間ブロックさせないため有限値にする。
/// 実機では数 ms 以内に signal されているはずなので 100ms あれば十分。
const FENCE_WAIT_TIMEOUT_MS: u32 = 100;

fn log_debug(msg: &str) {
    let path = std::env::temp_dir().join("cef_unity_debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[d3d11] {}", msg);
    }
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
    /// `WaitForSingleObject` 用の再利用イベント。fence の各 SetEventOnCompletion で
    /// このイベントを reset 状態に切り替える。
    event: HANDLE,
    /// 直近の Wait 完了値。これ以下の target は no-op で済ませる。
    last_waited: u64,
}

unsafe impl Send for FenceState {}

impl Drop for FenceState {
    fn drop(&mut self) {
        if !self.event.is_invalid() {
            let _ = unsafe { CloseHandle(self.event) };
        }
    }
}

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
        let mut state = OPENED.lock().unwrap();
        state.current = None;
        state.previous = None;
    }
    *FENCE.lock().unwrap() = None;
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
        let gd3d11_ptr = ((*interfaces).get_interface_split)(
            UNITY_GRAPHICSD3D11_GUID_HIGH,
            UNITY_GRAPHICSD3D11_GUID_LOW,
        );
        if gd3d11_ptr.is_null() {
            return std::ptr::null_mut();
        }
        let gd3d11 = gd3d11_ptr as *mut IUnityGraphicsD3D11;
        let device = ((*gd3d11).get_device)();
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

// ---- shared fence (D3D12 クロス API 同期 兼 D3D11 明示同期) ----

/// 共有 ID3D11Fence を Unity の D3D11Device で開いてグローバルに保持する。
/// `cef_unity_create_browser` が成功した直後に 1 度だけ呼ばれる想定。
/// 既に開いている場合は上書きする (browser 切替時)。
pub fn open_fence(handle_value: u64) -> Result<(), String> {
    if handle_value == 0 {
        return Err("fence handle is 0".to_string());
    }
    let device_ptr = try_resolve_d3d11_device();
    if device_ptr.is_null() {
        return Err("Unity D3D11 device not yet available".to_string());
    }

    let device: ID3D11Device = unsafe {
        let raw = device_ptr;
        ID3D11Device::from_raw_borrowed(&raw)
            .ok_or_else(|| "ID3D11Device::from_raw_borrowed failed".to_string())?
            .clone()
    };
    let device5: ID3D11Device5 = device
        .cast()
        .map_err(|e| format!("cast ID3D11Device5 (Unity device): {:?}", e))?;
    let mut fence_opt: Option<ID3D11Fence> = None;
    unsafe {
        device5
            .OpenSharedFence(HANDLE(handle_value as *mut _), &mut fence_opt)
            .map_err(|e| format!("OpenSharedFence: {:?}", e))?;
    }
    let fence: ID3D11Fence = fence_opt.ok_or_else(|| "OpenSharedFence returned None".to_string())?;

    // Auto-reset, initially non-signaled。
    let event: HANDLE = unsafe {
        CreateEventW(None, false, false, None)
            .map_err(|e| format!("CreateEventW: {:?}", e))?
    };

    *FENCE.lock().unwrap() = Some(FenceState {
        fence,
        event,
        last_waited: 0,
    });
    log_debug(&format!(
        "open_fence: opened handle=0x{:x}",
        handle_value
    ));
    Ok(())
}

/// fence が `target_value` 以上に到達するまで CPU 待機する。
/// fence 未対応 (open_fence 未呼び出し) の場合は no-op。
/// `target_value` が 0 または既に到達済みの場合も no-op。
pub fn wait_fence(target_value: u64) -> Result<(), String> {
    if target_value == 0 {
        return Ok(());
    }
    let mut guard = FENCE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return Ok(()); // fence 未対応経路 (例: 同一プロセステストや非サポート)
    };
    if target_value <= state.last_waited {
        return Ok(());
    }

    // 既に到達済みなら SetEventOnCompletion は即時シグナルになる。
    unsafe {
        state
            .fence
            .SetEventOnCompletion(target_value, state.event)
            .map_err(|e| format!("SetEventOnCompletion({}): {:?}", target_value, e))?;
    }
    let wait_result = unsafe { WaitForSingleObject(state.event, FENCE_WAIT_TIMEOUT_MS) };
    if wait_result.0 != 0 {
        // 0 = WAIT_OBJECT_0; それ以外は timeout/abandoned/error。
        return Err(format!(
            "WaitForSingleObject returned 0x{:x} (target={})",
            wait_result.0, target_value
        ));
    }
    state.last_waited = target_value;
    Ok(())
}

// ---- HANDLE → ID3D11Texture2D ----

/// shm 上の HANDLE 値を Unity の D3D11Device で OpenSharedResource1 する。
/// 同じ HANDLE 値なら cache 内のものを返す。
/// 戻り値: (ID3D11Texture2D の生ポインタ, width, height)。
///
/// 返したポインタは次に open_or_cached が呼ばれて HANDLE が変わるか、
/// clear_unity_interfaces が呼ばれるまで有効。
pub fn open_or_cached(
    handle_value: u64,
    width: u32,
    height: u32,
) -> Option<(*mut c_void, u32, u32)> {
    if handle_value == 0 {
        return None;
    }
    let device_ptr = try_resolve_d3d11_device();
    if device_ptr.is_null() {
        return None;
    }

    let mut state = OPENED.lock().unwrap();

    // cache hit?
    if let Some(c) = state.current.as_ref() {
        if c.handle == handle_value && c.width == width && c.height == height {
            return Some((c.texture.as_raw(), width, height));
        }
    }

    // 新規に開く
    let device: ID3D11Device = unsafe {
        let raw = device_ptr;
        match ID3D11Device::from_raw_borrowed(&raw) {
            Some(d) => d.clone(),
            None => {
                log_debug(&format!(
                    "open_or_cached: from_raw_borrowed failed (device_ptr={:p})",
                    device_ptr
                ));
                return None;
            }
        }
    };
    let device1: ID3D11Device1 = match device.cast() {
        Ok(d) => d,
        Err(e) => {
            log_debug(&format!("open_or_cached: cast to ID3D11Device1 failed: {:?}", e));
            return None;
        }
    };

    let handle = HANDLE(handle_value as *mut _);
    let tex: ID3D11Texture2D = match unsafe { device1.OpenSharedResource1(handle) } {
        Ok(t) => t,
        Err(e) => {
            log_debug(&format!(
                "open_or_cached: OpenSharedResource1 failed for handle=0x{:x}: {:?}",
                handle_value, e
            ));
            return None;
        }
    };
    log_debug(&format!(
        "open_or_cached: opened handle=0x{:x} tex={:p} {}x{}",
        handle_value,
        tex.as_raw(),
        width,
        height
    ));

    let raw_tex = tex.as_raw();
    let new_entry = OpenedTexture {
        handle: handle_value,
        texture: tex,
        width,
        height,
    };

    // current → previous, new → current
    let old_current = state.current.take();
    state.previous = old_current;
    state.current = Some(new_entry);

    Some((raw_tex, width, height))
}
