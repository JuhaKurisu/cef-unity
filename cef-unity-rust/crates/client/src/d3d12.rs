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

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11Texture2D,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER, ID3D12CommandAllocator,
    ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::IDXGIKeyedMutex;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::Interface;

const FENCE_WAIT_TIMEOUT_MS: u32 = 100;
const KEYED_MUTEX_TIMEOUT_MS: u32 = 100;
/// 1 にすると stale 検出後即 release。server は Unity フレームの半分の頻度でペイントできる。
const STALE_FRAME_FORCE_RELEASE_THRESHOLD: u32 = 1;

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
    /// 同じ NT 共有ハンドルを D3D11 device で開いた view。`IDXGIKeyedMutex` を
    /// 取り出すためだけに保持 (D3D12 resource からは直接 cast できないため)。
    /// GPU 操作には使わない。alive にしておかないと mutex が無効化される。
    _d3d11_tex: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
    width: u32,
    height: u32,
}

struct OpenedState {
    current: Option<OpenedTexture>,
    previous: Option<OpenedTexture>,
    /// 現在 current に対して `Acquire(1)` 済みなら true。
    held: bool,
    /// `tick()` が呼ばれた連続回数 (frame_id 不変中)。
    stale_count: u32,
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
    held: false,
    stale_count: 0,
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

/// D3D12 client 専用の補助 D3D11 device。
/// `ID3D12Resource` から直接 `IDXGIKeyedMutex` に cast できないため、
/// 同じ NT 共有 HANDLE を D3D11 で別途開いて mutex 経路を確保する。
/// この device 上で GPU 操作はしない (mutex の Acquire/Release のみ)。
static D3D11_DEVICE: Mutex<Option<ID3D11Device1>> = Mutex::new(None);

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
        if state.held
            && let Some(c) = state.current.as_ref()
        {
            let _ = unsafe { c.mutex.ReleaseSync(0) };
        }
        state.held = false;
        state.stale_count = 0;
        state.current = None;
        state.previous = None;
        state.declared.clear();
        state.cmd_list = None;
        state.cmd_allocator = None;
    }
    *FENCE.lock().unwrap() = None;
    *D3D11_DEVICE.lock().unwrap() = None;
}

/// 補助 D3D11 device を遅延初期化する。
fn ensure_d3d11_device() -> Result<ID3D11Device1, String> {
    let mut guard = D3D11_DEVICE.lock().unwrap();
    if let Some(d) = guard.as_ref() {
        return Ok(d.clone());
    }
    let mut device: Option<ID3D11Device> = None;
    let mut feat: D3D_FEATURE_LEVEL = D3D_FEATURE_LEVEL::default();
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut feat),
            None,
        )
        .map_err(|e| format!("D3D11CreateDevice (mutex helper): {:?}", e))?;
    }
    let device = device.ok_or_else(|| "D3D11Device is None".to_string())?;
    let device1: ID3D11Device1 = device
        .cast()
        .map_err(|e| format!("cast Device1: {:?}", e))?;
    *guard = Some(device1.clone());
    log_debug("ensure_d3d11_device: created mutex helper D3D11 device");
    Ok(device1)
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

/// 1 個の transition barrier を作る。`pResource` は ManuallyDrop で COM ポインタを
/// "借用" するだけなので AddRef/Release されない (`resource` 側が所有権を保持)。
fn make_transition_barrier(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: unsafe {
                    std::mem::transmute_copy::<ID3D12Resource, std::mem::ManuallyDrop<Option<ID3D12Resource>>>(resource)
                },
                Subresource: 0,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

/// barrier 群を Unity の active queue に乗せ、最終 state を Unity に宣言する共通処理。
fn execute_barriers(
    state: &mut OpenedState,
    device: &ID3D12Device,
    resource: &ID3D12Resource,
    barriers: &[D3D12_RESOURCE_BARRIER],
    expected: D3D12_RESOURCE_STATES,
    current: D3D12_RESOURCE_STATES,
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
        cmd_list.ResourceBarrier(barriers);
        cmd_list
            .Close()
            .map_err(|e| format!("CommandList.Close: {:?}", e))?;
    }

    let state_decl = UnityGraphicsD3D12ResourceState {
        resource: resource.as_raw(),
        expected: expected.0,
        current: current.0,
    };

    let gfx_ptr = UNITY_GFX_D3D12.load(Ordering::Acquire);
    if gfx_ptr.is_null() {
        return Err("IUnityGraphicsD3D12v5 not available".to_string());
    }
    unsafe {
        let gfx = gfx_ptr as *mut IUnityGraphicsD3D12v5;
        ((*gfx).execute_command_list)(cmd_list.as_raw(), 1, &state_decl);
    }
    Ok(())
}

/// 初回 OpenSharedHandle 後の状態宣言。`COMMON → PIXEL_SHADER_RESOURCE`。
fn declare_initial_state(
    state: &mut OpenedState,
    device: &ID3D12Device,
    resource: &ID3D12Resource,
) -> Result<(), String> {
    let barrier = make_transition_barrier(
        resource,
        D3D12_RESOURCE_STATE_COMMON,
        D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
    );
    execute_barriers(
        state,
        device,
        resource,
        &[barrier],
        D3D12_RESOURCE_STATE_COMMON,
        D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
    )?;
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

/// frame_id が更新されたタイミングでのみ呼ぶこと (server が Release(1) 済みの想定)。
/// KeyedMutex プロトコルで前フレームを Release(0) し、新フレームを Acquire(1) する。
/// KeyedMutex は GPU レベルの implicit fence で cache coherence と書き込み排他を提供するので、
/// 別途の `refresh_state` 往復遷移は不要。
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

    // 1. 前フレームで保持中なら Release(0)。Unity render thread の前フレームの
    //    サンプルコマンドは Release 時点までに submit 済みなので KeyedMutex の
    //    implicit fence で捕捉される。
    if state.held
        && let Some(c) = state.current.as_ref()
        && let Err(e) = unsafe { c.mutex.ReleaseSync(0) }
    {
        log_debug(&format!("ReleaseSync(0) failed: {:?}", e));
    }
    state.held = false;
    state.stale_count = 0;

    // 2. cache hit?
    let cache_hit = matches!(
        state.current.as_ref(),
        Some(c) if c.handle == handle_value && c.width == width && c.height == height
    );

    let is_new = !cache_hit;
    if is_new {
        // D3D12 として開く (Unity サンプル用)
        let mut res_opt: Option<ID3D12Resource> = None;
        if let Err(e) = unsafe {
            device.OpenSharedHandle(HANDLE(handle_value as *mut _), &mut res_opt)
        } {
            log_debug(&format!(
                "OpenSharedHandle (D3D12) failed for handle=0x{:x}: {:?}",
                handle_value, e
            ));
            return None;
        }
        let resource = res_opt?;

        // 同じ HANDLE を D3D11 として開いて IDXGIKeyedMutex を取得 (D3D12 では cast 不可)。
        let d3d11_device = match ensure_d3d11_device() {
            Ok(d) => d,
            Err(e) => {
                log_debug(&format!("ensure_d3d11_device failed: {}", e));
                return None;
            }
        };
        let d3d11_tex: ID3D11Texture2D = match unsafe {
            d3d11_device.OpenSharedResource1(HANDLE(handle_value as *mut _))
        } {
            Ok(t) => t,
            Err(e) => {
                log_debug(&format!(
                    "D3D11 OpenSharedResource1 failed for handle=0x{:x}: {:?}",
                    handle_value, e
                ));
                return None;
            }
        };
        let mutex: IDXGIKeyedMutex = match d3d11_tex.cast() {
            Ok(m) => m,
            Err(e) => {
                log_debug(&format!("cast D3D11 tex to IDXGIKeyedMutex failed: {:?}", e));
                return None;
            }
        };
        log_debug(&format!(
            "opened handle=0x{:x} d3d12_resource={:p} d3d11_tex={:p} {}x{}",
            handle_value,
            resource.as_raw(),
            d3d11_tex.as_raw(),
            width,
            height
        ));

        let new_entry = OpenedTexture {
            handle: handle_value,
            resource,
            _d3d11_tex: d3d11_tex,
            mutex,
            width,
            height,
        };
        let old_current = state.current.take();
        state.previous = old_current;
        state.current = Some(new_entry);
    }

    // 3. 新 current を Acquire(1) — server の Release(1) と GPU work 完了を待つ
    {
        let current = state.current.as_ref()?;
        if let Err(e) = unsafe { current.mutex.AcquireSync(1, KEYED_MUTEX_TIMEOUT_MS) } {
            log_debug(&format!(
                "AcquireSync(1) failed (timeout {}ms): {:?}",
                KEYED_MUTEX_TIMEOUT_MS, e
            ));
            return None;
        }
    }
    state.held = true;

    // 4. 新規テクスチャの場合のみ初期状態を宣言 (COMMON → PIXEL_SHADER_RESOURCE)。
    //    Acquire 後にやることで、server の GPU 書き込みが完了してから barrier が走る。
    //    1 度宣言すれば以後 Unity が状態を踏襲するので追加 barrier は不要。
    if is_new {
        let resource = state.current.as_ref()?.resource.clone();
        if let Err(e) = declare_initial_state(&mut state, &device, &resource) {
            log_debug(&format!("declare_initial_state failed: {}", e));
            // 初期状態が宣言できないと Unity のサンプルで validation error になりうる。
            // とりあえずポインタは返すが、issue は ログに記録。
        }
    }

    let current = state.current.as_ref()?;
    Some((current.resource.as_raw(), current.width, current.height))
}

/// frame_id 不変時に呼ぶ。一定回数連続で呼ばれたら強制 Release して
/// 静的コンテンツでのデッドロックを防ぐ。
pub fn tick() {
    let mut state = OPENED.lock().unwrap();
    if !state.held {
        return;
    }
    state.stale_count = state.stale_count.saturating_add(1);
    if state.stale_count >= STALE_FRAME_FORCE_RELEASE_THRESHOLD {
        if let Some(c) = state.current.as_ref()
            && let Err(e) = unsafe { c.mutex.ReleaseSync(0) }
        {
            log_debug(&format!("force ReleaseSync(0) failed: {:?}", e));
        }
        state.held = false;
        state.stale_count = 0;
        log_debug("tick: force-released after stale frames");
    }
}

#[allow(dead_code)]
fn _unused_state_types(s: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_STATES {
    s
}
