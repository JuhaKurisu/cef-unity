// Windows: CEF の on_accelerated_paint で渡される NT 共有 HANDLE は、
// コールバック外では無効になる「pool の借用」である。そのため、
//   1. サーバ側に独自 ID3D11Device を作る
//   2. CEF の HANDLE を OpenSharedResource1 で開く
//   3. 自前の "出力テクスチャ" に CopyResource で blit する
//   4. 出力テクスチャの NT 共有 HANDLE を DuplicateHandle で
//      クライアントプロセスに渡す
// という流れが必要になる。
//
// 出力テクスチャはサイズ変更時のみ再作成する単一インスタンス構成。
// テアリング対策はとりあえず行わない (要観察)。
//
// 非 Windows ではビルドが通るように空のスタブを提供する
// (wrap_render_handler! マクロが cfg-gated フィールドを許容しないため、
//  ハンドラ側のフィールド型は cfg なしで宣言する必要がある)。

#[cfg(target_os = "windows")]
use std::sync::Mutex;

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_HANDLE_OPTIONS, DUPLICATE_SAME_ACCESS, HANDLE, HMODULE,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::{
    IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};
#[cfg(target_os = "windows")]
use windows::core::Interface;

// 非 Windows: スタブ (Windows 用 DXGI_FORMAT 引数の代替も含む)。
#[cfg(not(target_os = "windows"))]
pub type DXGI_FORMAT = u32;

#[cfg(not(target_os = "windows"))]
pub struct D3D11Pool;

#[cfg(not(target_os = "windows"))]
impl D3D11Pool {
    pub fn new(_client_pid: Option<u32>) -> Result<Self, String> {
        Err("D3D11Pool not supported on this platform".to_string())
    }
}

#[cfg(target_os = "windows")]
pub struct D3D11Pool {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    state: Mutex<PoolState>,
    /// クライアントプロセスへの DUP_HANDLE 用ハンドル。
    /// `client_pid` 不明時は None (このときは DuplicateHandle せず、
    /// ローカルの shared HANDLE 値をそのまま返す = 同一プロセス前提のテスト経路)。
    client_proc: Option<usize>, // 実体は HANDLE。Send/Sync のため usize で保持
}

#[cfg(target_os = "windows")]
unsafe impl Send for D3D11Pool {}
#[cfg(target_os = "windows")]
unsafe impl Sync for D3D11Pool {}

#[cfg(target_os = "windows")]
struct PoolState {
    /// 出力テクスチャ。サイズ変更時のみ再生成する。
    texture: Option<ID3D11Texture2D>,
    /// クライアントプロセス内で有効な DuplicateHandle 済みの HANDLE 値。
    client_handle_value: u64,
    /// サーバプロセス内のローカル shared HANDLE (Drop で CloseHandle する)。
    /// `client_proc` が None のときは client_handle_value と同じ値が入る。
    local_handle_value: u64,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
}

#[cfg(target_os = "windows")]
impl Drop for D3D11Pool {
    fn drop(&mut self) {
        if let Ok(state) = self.state.lock() {
            if state.local_handle_value != 0 {
                let _ = unsafe { CloseHandle(HANDLE(state.local_handle_value as *mut _)) };
            }
        }
        if let Some(p) = self.client_proc {
            let _ = unsafe { CloseHandle(HANDLE(p as *mut _)) };
        }
    }
}

#[cfg(target_os = "windows")]
impl D3D11Pool {
    pub fn new(client_pid: Option<u32>) -> Result<Self, String> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let mut feat: D3D_FEATURE_LEVEL = D3D_FEATURE_LEVEL::default();
        unsafe {
            D3D11CreateDevice(
                None,                       // pAdapter: 既定アダプタ
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),          // software: 未使用 (null HMODULE)
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,                       // pFeatureLevels: 既定
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feat),
                Some(&mut context),
            )
            .map_err(|e| format!("D3D11CreateDevice failed: {:?}", e))?;
        }
        let device = device.ok_or_else(|| "D3D11 device is None".to_string())?;
        let context = context.ok_or_else(|| "D3D11 context is None".to_string())?;

        let client_proc = if let Some(pid) = client_pid {
            let h = unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid) }
                .map_err(|e| format!("OpenProcess(PROCESS_DUP_HANDLE, pid={}) failed: {:?}", pid, e))?;
            if h.is_invalid() {
                return Err(format!("OpenProcess returned invalid handle for pid {}", pid));
            }
            Some(h.0 as usize)
        } else {
            None
        };

        Ok(D3D11Pool {
            device,
            context,
            state: Mutex::new(PoolState {
                texture: None,
                client_handle_value: 0,
                local_handle_value: 0,
                width: 0,
                height: 0,
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
            }),
            client_proc,
        })
    }

    /// CEF が渡してきた source 共有 HANDLE をサーバ側 ID3D11Device で開き、
    /// 出力テクスチャに CopyResource で写し、出力テクスチャのクライアント側
    /// HANDLE 値を返す。
    pub fn copy_from_source(
        &self,
        src_handle: HANDLE,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
    ) -> Result<u64, String> {
        unsafe {
            // 1. CEF source を開く
            let device1: ID3D11Device1 = self
                .device
                .cast()
                .map_err(|e| format!("cast ID3D11Device1: {:?}", e))?;
            let src_tex: ID3D11Texture2D = device1
                .OpenSharedResource1(src_handle)
                .map_err(|e| format!("OpenSharedResource1(src): {:?}", e))?;

            // 2. 出力テクスチャをサイズ変更時のみ作り直し
            let mut state = self.state.lock().unwrap();
            let need_recreate = state.texture.is_none()
                || state.width != width
                || state.height != height
                || state.format != format;
            if need_recreate {
                // 旧 local handle を閉じる (client 側の dup'd handle は client が解放する)
                if state.local_handle_value != 0 {
                    let _ = CloseHandle(HANDLE(state.local_handle_value as *mut _));
                    state.local_handle_value = 0;
                    state.client_handle_value = 0;
                }
                state.texture = None;

                let desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: format,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                    CPUAccessFlags: 0,
                    // NT shared handle は単独では使えず、SHARED または KEYEDMUTEX と組み合わせる必要がある。
                    // KEYEDMUTEX なしでも同期は OK (ID3D11DeviceContext::Flush + Unity の読み取りタイミング差で
                    // テアリングが起きる可能性は残るが、シンプルさを優先)。
                    MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                        as u32,
                };
                let mut new_tex: Option<ID3D11Texture2D> = None;
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut new_tex))
                    .map_err(|e| format!("CreateTexture2D: {:?}", e))?;
                let new_tex = new_tex.ok_or_else(|| "CreateTexture2D returned None".to_string())?;

                let dxgi_res: IDXGIResource1 = new_tex
                    .cast()
                    .map_err(|e| format!("cast IDXGIResource1: {:?}", e))?;
                let local_handle: HANDLE = dxgi_res
                    .CreateSharedHandle(
                        None,
                        DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                        None,
                    )
                    .map_err(|e| format!("CreateSharedHandle: {:?}", e))?;

                let client_handle_value: u64 = if let Some(client_proc) = self.client_proc {
                    let mut dup = HANDLE::default();
                    let cp = HANDLE(client_proc as *mut _);
                    DuplicateHandle(
                        GetCurrentProcess(),
                        local_handle,
                        cp,
                        &mut dup,
                        0,
                        false,
                        DUPLICATE_HANDLE_OPTIONS(DUPLICATE_SAME_ACCESS.0),
                    )
                    .map_err(|e| format!("DuplicateHandle: {:?}", e))?;
                    dup.0 as u64
                } else {
                    local_handle.0 as u64
                };

                state.texture = Some(new_tex);
                state.local_handle_value = local_handle.0 as u64;
                state.client_handle_value = client_handle_value;
                state.width = width;
                state.height = height;
                state.format = format;
            }

            // 3. CopyResource source → 出力テクスチャ
            let dst_tex = state
                .texture
                .as_ref()
                .ok_or_else(|| "dst texture is None".to_string())?;
            self.context.CopyResource(dst_tex, &src_tex);
            self.context.Flush();

            Ok(state.client_handle_value)
        }
    }
}
