// Windows: Unity の ID3D12Device を IUnityGraphicsD3D12v5 経由で取得し、
// サーバが共有してきた NT 共有 HANDLE を ID3D12Device::OpenSharedHandle で開く。
// 開いた ID3D12Resource* を Unity の Texture2D.CreateExternalTexture に渡す。
//
// 同期: サーバ側 ID3D11Fence の NT 共有 HANDLE を ID3D12Device::OpenSharedHandle で
// ID3D12Fence として開き、SetEventOnCompletion + WaitForSingleObject で CPU 側待機する。
// (queue->Wait は queue の thread safety 懸念を避けるため、D3D11 経路と統一して CPU 待機)
//
// 状態遷移: OpenSharedHandle 直後のリソースは D3D12_RESOURCE_STATE_COMMON。
// IUnityGraphicsD3D12v5::ExecuteCommandList で COMMON → PIXEL_SHADER_RESOURCE バリアを
// Unity のキューに乗せ、states 引数で「以後このリソースは PIXEL_SHADER_RESOURCE 状態」
// と Unity に教える。一度宣言すれば以後 Unity は state を踏襲する。

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER, ID3D12CommandAllocator,
    ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::Interface;

const FENCE_WAIT_TIMEOUT_MS: u32 = 100;

fn log_debug(msg: &str) {
    let path = std::env::temp_dir().join("cef_unity_debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[d3d12] {}", msg);
    }
}

// ---- Unity Native Plugin Interface (subset) ----

#[repr(C)]
struct IUnityInterfaces {
    get_interface: unsafe extern "C" fn(guid_high: u64, guid_low: u64) -> *mut c_void,
    register_interface: unsafe extern "C" fn(guid_high: u64, guid_low: u64, ptr: *mut c_void),
    get_interface_split: unsafe extern "C" fn(high: u64, low: u64) -> *mut c_void,
    register_interface_split: unsafe extern "C" fn(high: u64, low: u64, ptr: *mut c_void),
}

/// Unity が `IUnityGraphicsD3D12v5` ヘッダで宣言する各エントリの順序通りに並べた最小定義。
/// 順序を変えると ABI が壊れるので Unity 公式ヘッダと一致を保つこと。
#[repr(C)]
struct IUnityGraphicsD3D12v5 {
    get_device: unsafe extern "C" fn() -> *mut c_void, // ID3D12Device*
    get_frame_fence: unsafe extern "C" fn() -> *mut c_void, // ID3D12Fence*
    get_next_frame_fence_value: unsafe extern "C" fn() -> u64,
    execute_command_list: unsafe extern "C" fn(
        command_list: *mut c_void,
        state_count: i32,
        states: *const UnityGraphicsD3D12ResourceState,
    ) -> u64,
    set_physical_video_memory_control_values: unsafe extern "C" fn(*const c_void),
    get_command_queue: unsafe extern "C" fn() -> *mut c_void, // ID3D12CommandQueue*
    texture_from_render_buffer: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
}

/// `ExecuteCommandList` 引数で Unity に状態遷移を宣言する構造体。
/// ヘッダの `UnityGraphicsD3D12ResourceState` と一致。
#[repr(C)]
struct UnityGraphicsD3D12ResourceState {
    resource: *mut c_void, // ID3D12Resource*
    expected: i32,         // D3D12_RESOURCE_STATES (i32 ビットフラグ)
    current: i32,
}

// IUnityGraphicsD3D12v5 GUID: 0xF5C8D8A37D37BC42, 0xB02DFE93B5064A27
const UNITY_GRAPHICSD3D12_V5_GUID_HIGH: u64 = 0xF5C8_D8A3_7D37_BC42;
const UNITY_GRAPHICSD3D12_V5_GUID_LOW: u64 = 0xB02D_FE93_B506_4A27;

// ---- 状態 ----

static UNITY_INTERFACES: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static UNITY_GFX_D3D12: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static UNITY_DEVICE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

struct OpenedTexture {
    handle: u64,
    resource: ID3D12Resource,
    width: u32,
    height: u32,
}

struct OpenedState {
    current: Option<OpenedTexture>,
    previous: Option<OpenedTexture>,
    /// Unity に状態を宣言済みのリソース集合 (pointer 値で識別)。
    /// 一度宣言した後は Unity が状態を踏襲するので追加の barrier は不要。
    declared: Vec<usize>,
    /// 状態遷移用の小さなコマンドアロケータ + コマンドリスト。
    /// Unity から取得した ID3D12Device で 1 度だけ作る。
    cmd_allocator: Option<ID3D12CommandAllocator>,
    cmd_list: Option<ID3D12GraphicsCommandList>,
}

static OPENED: Mutex<OpenedState> = Mutex::new(OpenedState {
    current: None,
    previous: None,
    declared: Vec::new(),
    cmd_allocator: None,
    cmd_list: None,
});

struct FenceState {
    fence: ID3D12Fence,
    event: HANDLE,
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

pub fn set_unity_interfaces(unity_interfaces: *mut c_void) {
    UNITY_INTERFACES.store(unity_interfaces, Ordering::Release);
    try_resolve_d3d12_device();
}

pub fn clear_unity_interfaces() {
    UNITY_INTERFACES.store(std::ptr::null_mut(), Ordering::Release);
    UNITY_GFX_D3D12.store(std::ptr::null_mut(), Ordering::Release);
    UNITY_DEVICE.store(std::ptr::null_mut(), Ordering::Release);
    {
        let mut state = OPENED.lock().unwrap();
        state.current = None;
        state.previous = None;
        state.declared.clear();
        state.cmd_list = None;
        state.cmd_allocator = None;
    }
    *FENCE.lock().unwrap() = None;
}

/// IUnityInterfaces から IUnityGraphicsD3D12v5 経由で ID3D12Device* を取得する。
/// Unity が D3D12 で動いていないときは null を返す (= 我々は D3D11 経路に fallback)。
fn try_resolve_d3d12_device() -> *mut c_void {
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
        let gfx_ptr = ((*interfaces).get_interface_split)(
            UNITY_GRAPHICSD3D12_V5_GUID_HIGH,
            UNITY_GRAPHICSD3D12_V5_GUID_LOW,
        );
        if gfx_ptr.is_null() {
            return std::ptr::null_mut();
        }
        UNITY_GFX_D3D12.store(gfx_ptr, Ordering::Release);
        let gfx = gfx_ptr as *mut IUnityGraphicsD3D12v5;
        let device = ((*gfx).get_device)();
        if device.is_null() {
            return std::ptr::null_mut();
        }
        UNITY_DEVICE.store(device, Ordering::Release);
        device
    }
}

pub fn is_connected() -> bool {
    !try_resolve_d3d12_device().is_null()
}

fn unity_device() -> Option<ID3D12Device> {
    let ptr = try_resolve_d3d12_device();
    if ptr.is_null() {
        return None;
    }
    unsafe { ID3D12Device::from_raw_borrowed(&ptr).map(|d| d.clone()) }
}

fn ensure_cmd_objects(state: &mut OpenedState, device: &ID3D12Device) -> Result<(), String> {
    if state.cmd_allocator.is_some() && state.cmd_list.is_some() {
        return Ok(());
    }
    let allocator: ID3D12CommandAllocator = unsafe {
        device
            .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
            .map_err(|e| format!("CreateCommandAllocator: {:?}", e))?
    };
    let cmd_list: ID3D12GraphicsCommandList = unsafe {
        device
            .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
            .map_err(|e| format!("CreateCommandList: {:?}", e))?
    };
    // CreateCommandList は record 状態で返すので、即 Close して以降は Reset から始める。
    unsafe {
        cmd_list
            .Close()
            .map_err(|e| format!("CommandList.Close (initial): {:?}", e))?;
    }
    state.cmd_allocator = Some(allocator);
    state.cmd_list = Some(cmd_list);
    Ok(())
}

/// Unity に「リソースを COMMON → PIXEL_SHADER_RESOURCE に遷移する」と宣言する。
/// 同じリソースに対しては最初の 1 回だけ呼ぶ。以後 Unity が状態を踏襲してくれる。
fn declare_initial_state(
    state: &mut OpenedState,
    device: &ID3D12Device,
    resource: &ID3D12Resource,
) -> Result<(), String> {
    ensure_cmd_objects(state, device)?;

    let allocator = state.cmd_allocator.as_ref().unwrap();
    let cmd_list = state.cmd_list.as_ref().unwrap();

    unsafe {
        allocator
            .Reset()
            .map_err(|e| format!("CommandAllocator.Reset: {:?}", e))?;
        cmd_list
            .Reset(allocator, None)
            .map_err(|e| format!("CommandList.Reset: {:?}", e))?;
    }

    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: unsafe {
                    std::mem::transmute_copy::<ID3D12Resource, std::mem::ManuallyDrop<Option<ID3D12Resource>>>(resource)
                },
                Subresource: 0,
                StateBefore: D3D12_RESOURCE_STATE_COMMON,
                StateAfter: D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            }),
        },
    };
    unsafe {
        cmd_list.ResourceBarrier(&[barrier]);
        cmd_list
            .Close()
            .map_err(|e| format!("CommandList.Close: {:?}", e))?;
    }

    // Unity の ExecuteCommandList へ。states[] で「期待 state = COMMON、実行後 state = PIXEL_SHADER_RESOURCE」を宣言。
    let state_decl = UnityGraphicsD3D12ResourceState {
        resource: resource.as_raw(),
        expected: D3D12_RESOURCE_STATE_COMMON.0,
        current: D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE.0,
    };

    let gfx_ptr = UNITY_GFX_D3D12.load(Ordering::Acquire);
    if gfx_ptr.is_null() {
        return Err("IUnityGraphicsD3D12v5 not available".to_string());
    }
    unsafe {
        let gfx = gfx_ptr as *mut IUnityGraphicsD3D12v5;
        ((*gfx).execute_command_list)(cmd_list.as_raw(), 1, &state_decl);
    }

    state.declared.push(resource.as_raw() as usize);
    log_debug(&format!(
        "declare_initial_state: resource={:p} COMMON->PIXEL_SHADER_RESOURCE",
        resource.as_raw()
    ));
    Ok(())
}

// ---- shared fence ----

pub fn open_fence(handle_value: u64) -> Result<(), String> {
    if handle_value == 0 {
        return Err("fence handle is 0".to_string());
    }
    let device = unity_device().ok_or_else(|| "Unity D3D12 device not available".to_string())?;

    let mut fence_opt: Option<ID3D12Fence> = None;
    unsafe {
        device
            .OpenSharedHandle(HANDLE(handle_value as *mut _), &mut fence_opt)
            .map_err(|e| format!("ID3D12Device::OpenSharedHandle (fence): {:?}", e))?;
    }
    let fence = fence_opt.ok_or_else(|| "OpenSharedHandle (fence) returned None".to_string())?;

    let event: HANDLE = unsafe {
        CreateEventW(None, false, false, None).map_err(|e| format!("CreateEventW: {:?}", e))?
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

pub fn wait_fence(target_value: u64) -> Result<(), String> {
    if target_value == 0 {
        return Ok(());
    }
    let mut guard = FENCE.lock().unwrap();
    let Some(state) = guard.as_mut() else {
        return Ok(());
    };
    if target_value <= state.last_waited {
        return Ok(());
    }
    unsafe {
        state
            .fence
            .SetEventOnCompletion(target_value, state.event)
            .map_err(|e| format!("SetEventOnCompletion({}): {:?}", target_value, e))?;
    }
    let wait_result = unsafe { WaitForSingleObject(state.event, FENCE_WAIT_TIMEOUT_MS) };
    if wait_result.0 != 0 {
        return Err(format!(
            "WaitForSingleObject returned 0x{:x} (target={})",
            wait_result.0, target_value
        ));
    }
    state.last_waited = target_value;
    Ok(())
}

// ---- HANDLE → ID3D12Resource ----

pub fn open_or_cached(
    handle_value: u64,
    width: u32,
    height: u32,
) -> Option<(*mut c_void, u32, u32)> {
    if handle_value == 0 {
        return None;
    }
    let device = unity_device()?;

    let mut state = OPENED.lock().unwrap();

    // cache hit?
    if let Some(c) = state.current.as_ref()
        && c.handle == handle_value
        && c.width == width
        && c.height == height
    {
        return Some((c.resource.as_raw(), width, height));
    }

    // 新規に開く
    let mut res_opt: Option<ID3D12Resource> = None;
    if let Err(e) = unsafe {
        device.OpenSharedHandle(HANDLE(handle_value as *mut _), &mut res_opt)
    } {
        log_debug(&format!(
            "open_or_cached: OpenSharedHandle failed for handle=0x{:x}: {:?}",
            handle_value, e
        ));
        return None;
    }
    let resource = res_opt?;
    log_debug(&format!(
        "open_or_cached: opened handle=0x{:x} resource={:p} {}x{}",
        handle_value,
        resource.as_raw(),
        width,
        height
    ));

    // Unity に初期状態を宣言 (COMMON → PIXEL_SHADER_RESOURCE)
    if let Err(e) = declare_initial_state(&mut state, &device, &resource) {
        log_debug(&format!("declare_initial_state failed: {}", e));
        return None;
    }

    let raw_res = resource.as_raw();
    let new_entry = OpenedTexture {
        handle: handle_value,
        resource,
        width,
        height,
    };

    let old_current = state.current.take();
    state.previous = old_current;
    state.current = Some(new_entry);

    Some((raw_res, width, height))
}

#[allow(dead_code)]
fn _unused_state_types(s: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_STATES {
    s
}
