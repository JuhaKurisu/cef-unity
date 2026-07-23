// Windows: Unity の ID3D12Device を IUnityGraphicsD3D12v5 経由で取得し、
// サーバが共有してきた NT 共有 HANDLE を ID3D12Device::OpenSharedHandle で開く。
// 開いた ID3D12Resource* を Unity の Texture2D.CreateExternalTexture に渡す。
//
// 同期: KeyedMutex は使わず、共有 ID3D11Fence (= ID3D12Fence) のみで同期する。
//   - server が CopyResource + Flush + Signal(fence, value) する
//   - client は **Unity の ID3D12CommandQueue** に `Wait(fence, value)` を発行
//     (GPU-side wait)。Unity の以降の描画コマンドは GPU 上で fence 完了を待ってから走る
// `ID3D12CommandQueue::Wait` ドキュメント:
//   "Queues a GPU-side wait, and returns immediately. ... the GPU waits until the
//   specified fence reaches or exceeds the specified value."
// これにより、ID3D11Fence (server) と Unity の D3D12 queue (consumer) 間で
// cross-API GPU 同期が成立し、helper D3D11 device 経由の KeyedMutex 回避策が不要になる。
//
// 状態遷移: OpenSharedHandle 直後のリソースは D3D12_RESOURCE_STATE_COMMON。
// IUnityGraphicsD3D12v5::ExecuteCommandList で COMMON → PIXEL_SHADER_RESOURCE バリアを
// Unity のキューに乗せ、states 引数で「以後このリソースは PIXEL_SHADER_RESOURCE 状態」
// と Unity に教える。一度宣言すれば以後 Unity は state を踏襲する。

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::io::Write;
use std::sync::{Mutex, PoisonError};
use std::sync::atomic::{AtomicPtr, Ordering};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
    D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
    D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER, ID3D12CommandAllocator,
    ID3D12CommandQueue, ID3D12Device, ID3D12Fence, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::core::Interface;

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
static UNITY_QUEUE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

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
    last_waited: u64,
}

unsafe impl Send for FenceState {}

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
    UNITY_QUEUE.store(std::ptr::null_mut(), Ordering::Release);
    {
        let mut state = OPENED.lock().unwrap_or_else(PoisonError::into_inner);
        state.current = None;
        state.previous = None;
        state.declared.clear();
        state.cmd_list = None;
        state.cmd_allocator = None;
    }
    *FENCE.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// IUnityInterfaces から IUnityGraphicsD3D12v5 経由で ID3D12Device* / ID3D12CommandQueue* を
/// 取得する。Unity が D3D12 で動いていないときは null を返す (= 我々は D3D11 経路に fallback)。
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
        // queue も同時に取得しておく (毎フレーム取り直す必要なし)。
        let queue = ((*gfx).get_command_queue)();
        if !queue.is_null() {
            UNITY_QUEUE.store(queue, Ordering::Release);
            log_debug(&format!(
                "resolved Unity D3D12 device={:p} queue={:p}",
                device, queue
            ));
        } else {
            log_debug("get_command_queue returned null");
        }
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

fn unity_queue() -> Option<ID3D12CommandQueue> {
    let ptr = UNITY_QUEUE.load(Ordering::Acquire);
    if ptr.is_null() {
        // device resolve のついでに取りに行く
        let _ = try_resolve_d3d12_device();
        let ptr = UNITY_QUEUE.load(Ordering::Acquire);
        if ptr.is_null() {
            return None;
        }
        return unsafe { ID3D12CommandQueue::from_raw_borrowed(&ptr).map(|q| q.clone()) };
    }
    unsafe { ID3D12CommandQueue::from_raw_borrowed(&ptr).map(|q| q.clone()) }
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

    *FENCE.lock().unwrap_or_else(PoisonError::into_inner) = Some(FenceState {
        fence,
        last_waited: 0,
    });
    log_debug(&format!(
        "open_fence: opened handle=0x{:x}",
        handle_value
    ));
    Ok(())
}

/// Unity の D3D12 queue に "fence が `target_value` に到達するまで以降の GPU ワークを保留" を
/// 指示する (GPU-side wait)。CPU はブロックしない。
/// fence 未対応 (open_fence 未呼び出し) の場合は no-op。
pub fn wait_fence(target_value: u64) -> Result<(), String> {
    if target_value == 0 {
        return Ok(());
    }
    let mut guard = FENCE.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(state) = guard.as_mut() else {
        return Ok(());
    };
    if target_value <= state.last_waited {
        return Ok(());
    }
    let queue = unity_queue().ok_or_else(|| "Unity D3D12 queue not available".to_string())?;
    unsafe {
        queue
            .Wait(&state.fence, target_value)
            .map_err(|e| format!("ID3D12CommandQueue::Wait({}): {:?}", target_value, e))?;
    }
    state.last_waited = target_value;
    Ok(())
}

// ---- HANDLE → ID3D12Resource ----

/// frame_id が更新されたタイミングでのみ呼ぶこと。
/// 同期は呼び出し側で `wait_fence(fence_value)` を呼ぶことで GPU-side に提供される。
/// KeyedMutex は使わないので Acquire/Release はない。
pub fn open_or_cached(
    handle_value: u64,
    width: u32,
    height: u32,
) -> Option<(*mut c_void, u32, u32)> {
    if handle_value == 0 {
        return None;
    }
    let device = unity_device()?;

    let mut state = OPENED.lock().unwrap_or_else(PoisonError::into_inner);

    let cache_hit = matches!(
        state.current.as_ref(),
        Some(c) if c.handle == handle_value && c.width == width && c.height == height
    );

    let is_new = !cache_hit;
    if is_new {
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
        log_debug(&format!(
            "opened handle=0x{:x} d3d12_resource={:p} {}x{}",
            handle_value,
            resource.as_raw(),
            width,
            height
        ));

        let new_entry = OpenedTexture {
            handle: handle_value,
            resource,
            width,
            height,
        };
        let old_current = state.current.take();
        state.previous = old_current;
        state.current = Some(new_entry);
    }

    // 新規テクスチャの場合のみ初期状態を宣言 (COMMON → PIXEL_SHADER_RESOURCE)。
    // 1 度宣言すれば以後 Unity が状態を踏襲するので追加 barrier は不要。
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

#[allow(dead_code)]
fn _unused_state_types(s: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_STATES {
    s
}
