// CEF Server: browser management, CEF handlers, IPC command processing.

use cef::*;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// IOSurface FFI (macOS only)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
#[link(name = "IOSurface", kind = "framework")]
unsafe extern "C" {
    fn IOSurfaceGetID(buffer: *mut std::os::raw::c_void) -> u32;
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn mach_iosurface_server_accept() -> i32;
    fn mach_iosurface_server_send(
        io_surface_ref: *mut std::os::raw::c_void,
        width: u32,
        height: u32,
        format: u32,
    ) -> i32;
    fn mach_iosurface_server_has_client() -> i32;
    fn iosurface_pool_copy_and_get(
        src: *mut std::os::raw::c_void,
        w: u32,
        h: u32,
        format: u32,
    ) -> *mut std::os::raw::c_void;
}

use cef_unity_ipc::{self as ipc, Command, Response, ShmWriter};

use crate::d3d11_pool::D3D11Pool;

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

const MAX_LOG_ENTRIES: usize = 1000;
static LOG_BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn log(msg: &str) {
    let path = std::env::temp_dir().join("cef_unity_server.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[{:?}] {}", std::time::SystemTime::now(), msg);
    }

    let mut buf = LOG_BUFFER.lock().unwrap();
    if buf.len() >= MAX_LOG_ENTRIES {
        buf.remove(0);
    }
    buf.push(msg.to_string());
}

fn drain_logs() -> Vec<String> {
    std::mem::take(&mut *LOG_BUFFER.lock().unwrap())
}

// ---------------------------------------------------------------------------
// CEF loader
// ---------------------------------------------------------------------------

/// macOS: get_cef_dir() でフレームワークを探して動的ロードする。
#[cfg(target_os = "macos")]
fn load_cef_auto() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cef_dir = cef::sys::get_cef_dir().expect("CEF directory not found");
    let framework_path = cef_dir.join(cef::sys::FRAMEWORK_PATH);
    let cstr = CString::new(framework_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        cef::load_library(Some(unsafe { &*cstr.as_ptr().cast() })),
        1,
        "Failed to load CEF framework"
    );
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
}

/// 非 macOS: libcef はリンク時解決。api_hash のみ呼ぶ。
#[cfg(not(target_os = "macos"))]
fn load_cef_auto() {
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
}

// ---------------------------------------------------------------------------
// Per-browser state
// ---------------------------------------------------------------------------

struct BrowserState {
    /// Kept alive so ShmWriter::drop cleans up shared memory on browser destroy.
    #[allow(dead_code)]
    shm: Arc<ShmWriter>,
    browser: Arc<Mutex<Option<Browser>>>,
    viewport_w: Arc<AtomicI32>,
    viewport_h: Arc<AtomicI32>,
    /// Windows: D3D11 共有テクスチャプール (on_accelerated_paint で使用)。
    /// 非 Windows / 失敗時は None で software 経路にフォールバック。
    #[allow(dead_code)]
    d3d11_pool: Option<Arc<D3D11Pool>>,
}

// ---------------------------------------------------------------------------
// CEF Handlers
// ---------------------------------------------------------------------------

static PAINT_COUNT: AtomicU64 = AtomicU64::new(0);

wrap_render_handler! {
    struct ServerRenderHandler {
        shm: Arc<ShmWriter>,
        viewport_w: Arc<AtomicI32>,
        viewport_h: Arc<AtomicI32>,
        d3d11_pool: Option<Arc<D3D11Pool>>,
    }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            let w = self.viewport_w.load(Ordering::Relaxed);
            let h = self.viewport_h.load(Ordering::Relaxed);
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = w;
                rect.height = h;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            let w = self.viewport_w.load(Ordering::Relaxed);
            let h = self.viewport_h.load(Ordering::Relaxed);
            if let Some(si) = screen_info {
                si.size = std::mem::size_of::<ScreenInfo>();
                si.device_scale_factor = 1.0;
                si.depth = 32;
                si.depth_per_component = 8;
                si.is_monochrome = 0;
                si.rect = Rect { x: 0, y: 0, width: w, height: h };
                si.available_rect = Rect { x: 0, y: 0, width: w, height: h };
            }
            1
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            let count = PAINT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            if count <= 3 || count.is_multiple_of(100) {
                log(&format!("on_paint #{}: {}x{}", count, width, height));
            }
            if type_.get_raw() != PaintElementType::VIEW.get_raw() {
                return;
            }
            let size = (width * height * 4) as usize;
            let src = unsafe { std::slice::from_raw_parts(buffer, size) };
            self.shm.write_frame(src, width as u32, height as u32);
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            if type_.get_raw() != PaintElementType::VIEW.get_raw() {
                return;
            }
            #[cfg(target_os = "macos")]
            if let Some(info) = info {
                let io_surface = info.shared_texture_io_surface;
                if io_surface.is_null() {
                    return;
                }
                let w = info.extra.coded_size.width as u32;
                let h = info.extra.coded_size.height as u32;
                let format = if info.format.get_raw() == ColorType::RGBA_8888.get_raw() {
                    1u32
                } else {
                    0u32
                };

                let count = PAINT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

                // GPU blit: CEF IOSurface → pool IOSurface (must complete before returning)
                let pool_surface = unsafe {
                    iosurface_pool_copy_and_get(io_surface, w, h, format)
                };
                if pool_surface.is_null() {
                    if count <= 5 {
                        log("on_accelerated_paint: pool copy failed");
                    }
                    return;
                }

                // Accept pending client subscription (non-blocking)
                unsafe { mach_iosurface_server_accept(); }

                // Send the copied pool IOSurface via Mach port to connected client
                let ret = unsafe {
                    mach_iosurface_server_send(pool_surface, w, h, format)
                };

                if count <= 5 || count.is_multiple_of(3000) {
                    let src_id = unsafe { IOSurfaceGetID(io_surface) };
                    let dst_id = unsafe { IOSurfaceGetID(pool_surface) };
                    let has_client = unsafe { mach_iosurface_server_has_client() };
                    log(&format!(
                        "on_accelerated_paint #{}: {}x{} src_id={} pool_id={} mach_send={} client={}",
                        count, w, h, src_id, dst_id, ret, has_client
                    ));
                }

                // Also write metadata to ShmHeader (for frame change detection)
                let surface_id = unsafe { IOSurfaceGetID(pool_surface) };
                self.shm.write_iosurface_info(surface_id, w, h, format);
            }

            #[cfg(target_os = "windows")]
            if let Some(info) = info {
                let Some(pool) = self.d3d11_pool.as_ref() else { return; };

                let src_handle_raw = info.shared_texture_handle;
                if src_handle_raw.is_null() {
                    return;
                }
                let w = info.extra.coded_size.width as u32;
                let h = info.extra.coded_size.height as u32;
                if w == 0 || h == 0 { return; }

                // CEF Windows OSR は通常 BGRA8 を出す。format フィールドで RGBA を判別する。
                let format_tag: u32 = if info.format.get_raw() == ColorType::RGBA_8888.get_raw() {
                    1
                } else {
                    0
                };

                let count = PAINT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

                // cef::sys::HANDLE = *mut c_void; windows::Win32::Foundation::HANDLE は newtype
                use windows::Win32::Foundation::HANDLE as WinHandle;
                use windows::Win32::Graphics::Dxgi::Common::{
                    DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
                };
                // CEF はガンマエンコード済み (sRGB) のバイトを出すので、Unity に
                // sRGB→linear 自動変換させるため _SRGB フォーマットでプールテクスチャを作る。
                // CopyResource は同 family (UNORM ↔ UNORM_SRGB) なら通る。
                let dxgi_format = if format_tag == 1 {
                    DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
                } else {
                    DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
                };
                let src_handle = WinHandle(src_handle_raw as *mut _);

                match pool.copy_from_source(src_handle, w, h, dxgi_format) {
                    Ok((client_handle, fence_value)) => {
                        if count <= 5 || count.is_multiple_of(300) {
                            log(&format!(
                                "on_accelerated_paint #{}: {}x{} fmt={} client_handle=0x{:x} fence={}",
                                count, w, h, format_tag, client_handle, fence_value
                            ));
                        }
                        self.shm
                            .write_d3d11_handle(client_handle, w, h, format_tag, fence_value);
                    }
                    Err(e) => {
                        if count <= 5 {
                            log(&format!("on_accelerated_paint pool error: {}", e));
                        }
                    }
                }
            }
        }

        fn on_ime_composition_range_changed(
            &self,
            _browser: Option<&mut Browser>,
            _selected_range: Option<&Range>,
            character_bounds: Option<&[Rect]>,
        ) {
            if let Some(bounds) = character_bounds
                && let Some(last) = bounds.last() {
                    // 最後の文字の右端 = 確定後の次のカーソル位置
                    self.shm.write_ime_caret(last.x + last.width, last.y, last.width, last.height);
                }
        }

    }
}

wrap_life_span_handler! {
    struct ServerLifeSpanHandler {
        browser_slot: Arc<Mutex<Option<Browser>>>,
    }
    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // ポップアップをキャンセルし、現在のブラウザで URL を開く
            let url_str = target_url.map(|u| u.to_string()).unwrap_or_default();
            log(&format!("on_before_popup: url={}", url_str));
            if let (Some(b), Some(url)) = (browser, target_url)
                && let Some(frame) = Browser::main_frame(b) {
                    Frame::load_url(&frame, Some(url));
                }
            1 // キャンセル
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            log("on_after_created called");
            if let Some(b) = browser {
                *self.browser_slot.lock().unwrap() = Some(b.clone());
                log("browser stored in slot");
            }
        }
    }
}

wrap_display_handler! {
    struct ServerDisplayHandler {
        shm: Arc<ShmWriter>,
    }
    impl DisplayHandler {
        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            _level: LogSeverity,
            message: Option<&CefString>,
            _source: Option<&CefString>,
            _line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            if let Some(msg) = message {
                let s = msg.to_string();
                if let Some(rest) = s.strip_prefix("__CARET__:") {
                    let parts: Vec<&str> = rest.split(':').collect();
                    if parts.len() == 4
                        && let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
                            parts[0].parse::<i32>(),
                            parts[1].parse::<i32>(),
                            parts[2].parse::<i32>(),
                            parts[3].parse::<i32>(),
                        ) {
                            self.shm.write_ime_caret(x, y, w, h);
                            return 1; // suppress from console output
                        }
                }
            }
            0
        }
    }
}

wrap_load_handler! {
    struct ServerLoadHandler {
        browser_slot: Arc<Mutex<Option<Browser>>>,
    }
    impl LoadHandler {
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: ::std::os::raw::c_int,
        ) {
            if let Some(f) = frame
                && f.is_main() != 0 {
                    Frame::execute_java_script(
                        f,
                        Some(&CefString::from(CARET_TRACKING_JS)),
                        Some(&CefString::from("cef-unity://caret-tracker")),
                        0,
                    );
                }
        }
    }
}

/// JavaScript to track caret position via selectionchange and click events.
/// Reports position as console.log("__CARET__:x:y:w:h").
const CARET_TRACKING_JS: &str = r#"
(function() {
    if (window.__cefUnityCaretTracker) return;
    window.__cefUnityCaretTracker = true;

    function reportCaret() {
        var sel = window.getSelection();
        if (!sel || sel.rangeCount === 0) return;
        var range = sel.getRangeAt(0).cloneRange();
        range.collapse(false);
        var rect = range.getBoundingClientRect();
        if (rect && rect.width === 0 && rect.height > 0) {
            console.log("__CARET__:" +
                Math.round(rect.x) + ":" +
                Math.round(rect.y) + ":" +
                Math.round(rect.width) + ":" +
                Math.round(rect.height));
        }
    }

    document.addEventListener("selectionchange", reportCaret);
    document.addEventListener("click", function() {
        setTimeout(reportCaret, 0);
    });
    document.addEventListener("keyup", function(e) {
        if (["ArrowLeft","ArrowRight","ArrowUp","ArrowDown","Home","End"].includes(e.key)) {
            reportCaret();
        }
    });
    document.addEventListener("input", function() {
        setTimeout(reportCaret, 0);
    });
})();
"#;

wrap_browser_process_handler! {
    struct ServerBrowserProcessHandler;
    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            crate::event_loop::schedule_pump(delay_ms);
        }
    }
}

wrap_app! {
    struct ServerApp {
        browser_process_handler: BrowserProcessHandler,
        use_gpu: bool,
    }
    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(cl) = command_line {
                cl.append_switch(Some(&CefString::from("use-mock-keychain")));
                cl.append_switch_with_value(
                    Some(&CefString::from("autoplay-policy")),
                    Some(&CefString::from("no-user-gesture-required")),
                );
                // GPU サンドボックスを無効化 (shared_texture_enabled で GPU プロセスが正常に
                // 動作するために必要。Unity プラグイン環境では CEF レベルのサンドボックスも
                // 無効 (no_sandbox=1) なので、GPU サンドボックスも不要)
                cl.append_switch(Some(&CefString::from("disable-gpu-sandbox")));

                if !self.use_gpu {
                    // CPU モード: Chromium に GPU を一切使わせない。
                    // これにより on_paint 用の GPU→CPU readback が発生しなくなり、
                    // Skia software pipeline のみで動く。
                    cl.append_switch(Some(&CefString::from("disable-gpu")));
                    cl.append_switch(Some(&CefString::from("disable-gpu-compositing")));
                    // Skia software raster の並列度を上げる (デフォルト 1-2 → 4)。
                    // 4K で全画面ダーティなスクロール時に効く。
                    cl.append_switch_with_value(
                        Some(&CefString::from("num-raster-threads")),
                        Some(&CefString::from("4")),
                    );
                }
            }
        }
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(self.browser_process_handler.clone())
        }
    }
}

wrap_client! {
    struct ServerClient {
        render_handler: RenderHandler,
        life_span_handler: LifeSpanHandler,
        display_handler: DisplayHandler,
        load_handler: LoadHandler,
    }
    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render_handler.clone())
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span_handler.clone())
        }
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display_handler.clone())
        }
        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load_handler.clone())
        }
    }
}

// ---------------------------------------------------------------------------
// ObjC helpers (macOS only)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn cef_unity_inject_app_protocol();
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

pub struct CefServer {
    browsers: HashMap<u32, BrowserState>,
    next_browser_id: AtomicU32,
    server_pid: u32,
    /// Windows: クライアントプロセス PID (DuplicateHandle 用)。
    #[allow(dead_code)]
    client_pid: Option<u32>,
    /// GPU (accelerated paint) を使うか。false の場合 D3D11Pool を作らず
    /// shared_texture_enabled を立てないため、CEF は on_paint (software) のみ呼ぶ。
    use_gpu: bool,
}

impl CefServer {
    pub fn new(client_pid: Option<u32>, use_gpu: bool) -> Self {
        CefServer {
            browsers: HashMap::new(),
            next_browser_id: AtomicU32::new(1),
            server_pid: std::process::id(),
            client_pid,
            use_gpu,
        }
    }

    /// Initialize CEF. Must be called on main thread before anything else.
    pub fn init_cef(&self) -> bool {
        log("init_cef() starting");

        #[cfg(target_os = "macos")]
        unsafe {
            cef_unity_inject_app_protocol();
        }

        load_cef_auto();

        let args = cef::args::Args::new();
        let exe_path = std::env::current_exe().unwrap();
        let exe_dir = exe_path.parent().unwrap();

        let helper_path = helper_binary_path(exe_dir);
        log(&format!("helper_path = {}", helper_path.display()));

        let cache_dir = std::env::temp_dir().join("cef_unity_cache");
        let _ = std::fs::create_dir_all(&cache_dir);

        let mut settings = Settings::default();
        settings.no_sandbox = 1;
        settings.windowless_rendering_enabled = 1;
        settings.external_message_pump = 1;
        settings.root_cache_path = CefString::from(cache_dir.to_str().unwrap());
        settings.browser_subprocess_path = CefString::from(helper_path.to_str().unwrap());

        // macOS: Framework ベースのパス設定
        #[cfg(target_os = "macos")]
        {
            let cef_dir = cef::sys::get_cef_dir().expect("CEF directory not found");
            let framework_dir = cef_dir.join("Chromium Embedded Framework.framework");
            let resources_dir = framework_dir.join("Resources");
            settings.framework_dir_path = CefString::from(framework_dir.to_str().unwrap());
            settings.resources_dir_path = CefString::from(resources_dir.to_str().unwrap());
            let locales_dir = resources_dir.join("locales");
            if locales_dir.exists() {
                settings.locales_dir_path = CefString::from(locales_dir.to_str().unwrap());
            }
        }

        // 非 macOS: 実行ファイルと同じディレクトリ
        #[cfg(not(target_os = "macos"))]
        {
            let exe_dir_str = exe_dir.to_str().unwrap();
            settings.resources_dir_path = CefString::from(exe_dir_str);
            let locales_dir = exe_dir.join("locales");
            if locales_dir.exists() {
                settings.locales_dir_path = CefString::from(locales_dir.to_str().unwrap());
            }
        }

        let cef_log = std::env::temp_dir().join("cef_debug.log");
        settings.log_file = CefString::from(cef_log.to_str().unwrap());
        settings.log_severity = LogSeverity::VERBOSE;

        let bph = ServerBrowserProcessHandler::new();
        let mut app = ServerApp::new(bph, self.use_gpu);
        let result = initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        log(&format!("initialize() returned {}", result));
        result != 0
    }

    /// Handle a single IPC command. Returns a Response.
    pub fn handle_command(&mut self, cmd: Command) -> Response {
        match cmd {
            Command::CreateBrowser { width, height, url } => {
                self.create_browser(width, height, &url)
            }
            Command::DestroyBrowser { browser_id } => self.destroy_browser(browser_id),
            Command::LoadUrl { browser_id, url } => self.load_url(browser_id, &url),
            Command::Resize {
                browser_id,
                width,
                height,
            } => self.resize(browser_id, width, height),
            Command::MouseMove {
                browser_id,
                x,
                y,
                modifiers,
            } => self.mouse_move(browser_id, x, y, modifiers),
            Command::MouseClick {
                browser_id,
                x,
                y,
                modifiers,
                button,
                mouse_up,
                click_count,
            } => self.mouse_click(browser_id, x, y, modifiers, button, mouse_up, click_count),
            Command::MouseWheel {
                browser_id,
                x,
                y,
                modifiers,
                delta_x,
                delta_y,
            } => self.mouse_wheel(browser_id, x, y, modifiers, delta_x, delta_y),
            Command::KeyEvent {
                browser_id,
                event_type,
                modifiers,
                windows_key_code,
                native_key_code,
                character,
                unmodified_character,
                is_system_key,
                focus_on_editable_field,
            } => self.key_event(
                browser_id,
                event_type,
                modifiers,
                windows_key_code,
                native_key_code,
                character,
                unmodified_character,
                is_system_key,
                focus_on_editable_field,
            ),
            Command::ExecuteJavaScript { browser_id, code } => {
                self.execute_javascript(browser_id, &code)
            }
            Command::EditCommand {
                browser_id,
                command,
            } => self.edit_command(browser_id, command),
            Command::GetCurrentUrl { browser_id } => self.get_current_url(browser_id),
            Command::ImeSetComposition {
                browser_id,
                text,
                selection_start,
                selection_end,
            } => self.ime_set_composition(browser_id, &text, selection_start, selection_end),
            Command::ImeCommitText { browser_id, text } => self.ime_commit_text(browser_id, &text),
            Command::ImeFinishComposingText {
                browser_id,
                keep_selection,
            } => self.ime_finish_composing_text(browser_id, keep_selection),
            Command::ImeCancelComposition { browser_id } => self.ime_cancel_composition(browser_id),
            Command::GetLogs => Response::Logs {
                entries: drain_logs(),
            },
            Command::Shutdown => {
                // Caller handles shutdown
                Response::Ok
            }
        }
    }

    fn create_browser(&mut self, width: i32, height: i32, url: &str) -> Response {
        let id = self.next_browser_id.fetch_add(1, Ordering::Relaxed);
        let shm_flink = ipc::shm_flink_path(self.server_pid, id);

        let shm = match ShmWriter::new(&shm_flink) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                return Response::Error {
                    msg: format!("shm_create failed: {}", e),
                };
            }
        };

        let viewport_w = Arc::new(AtomicI32::new(width));
        let viewport_h = Arc::new(AtomicI32::new(height));
        let browser_slot: Arc<Mutex<Option<Browser>>> = Arc::new(Mutex::new(None));

        // Windows のみ: D3D11 共有テクスチャプールを作成 (失敗時は software 経路にフォールバック)。
        // 非 Windows ではスタブ実装が常に Err を返すので None になる。
        // CPU モード (use_gpu=false) では作らず、software paint を強制する。
        let d3d11_pool: Option<Arc<D3D11Pool>> = if !self.use_gpu {
            log("use_gpu=false: skipping D3D11Pool, forcing software paint");
            None
        } else {
            match D3D11Pool::new(self.client_pid) {
                Ok(p) => {
                    log(&format!(
                        "D3D11Pool created (client_pid={:?})",
                        self.client_pid
                    ));
                    Some(Arc::new(p))
                }
                Err(_e) => {
                    #[cfg(target_os = "windows")]
                    log(&format!(
                        "D3D11Pool::new failed, falling back to software paint: {}",
                        _e
                    ));
                    None
                }
            }
        };

        let render_handler = ServerRenderHandler::new(
            Arc::clone(&shm),
            Arc::clone(&viewport_w),
            Arc::clone(&viewport_h),
            d3d11_pool.clone(),
        );
        let life_span_handler = ServerLifeSpanHandler::new(Arc::clone(&browser_slot));
        let display_handler = ServerDisplayHandler::new(Arc::clone(&shm));
        let load_handler = ServerLoadHandler::new(Arc::clone(&browser_slot));
        let mut client = ServerClient::new(
            render_handler,
            life_span_handler,
            display_handler,
            load_handler,
        );

        // cef_window_handle_t はプラットフォーム依存:
        //   macOS / Linux: *mut c_void
        //   Windows: HWND (newtype wrapping *mut c_void)
        #[cfg(target_os = "windows")]
        let parent_handle = cef::sys::HWND(std::ptr::null_mut());
        #[cfg(not(target_os = "windows"))]
        let parent_handle = std::ptr::null_mut();
        let mut window_info = WindowInfo::default().set_as_windowless(parent_handle);
        // macOS: IOSurface Mach port 転送を使用 (use_gpu=true のときのみ)。
        // Windows: D3D11 共有テクスチャプールが構築できた場合のみ accelerated paint を有効化。
        // CPU モード (use_gpu=false) ではどのプラットフォームでも立てない。
        #[cfg(target_os = "macos")]
        if self.use_gpu {
            window_info.shared_texture_enabled = 1;
        }
        #[cfg(target_os = "windows")]
        if d3d11_pool.is_some() {
            window_info.shared_texture_enabled = 1;
        }
        let ok = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(url)),
            Some(&BrowserSettings {
                background_color: 0x00000000,
                windowless_frame_rate: 120,
                ..Default::default()
            }),
            None,
            None,
        );
        log(&format!(
            "browser_host_create_browser id={} returned {}",
            id, ok
        ));

        if ok == 0 {
            return Response::Error {
                msg: "browser_host_create_browser failed".to_string(),
            };
        }

        let d3d11_fence_handle = d3d11_pool
            .as_ref()
            .map(|p| p.client_fence_handle())
            .unwrap_or(0);

        self.browsers.insert(
            id,
            BrowserState {
                shm,
                browser: browser_slot,
                viewport_w,
                viewport_h,
                d3d11_pool,
            },
        );

        Response::BrowserCreated {
            browser_id: id,
            shm_flink,
            d3d11_fence_handle,
        }
    }

    fn destroy_browser(&mut self, browser_id: u32) -> Response {
        if let Some(state) = self.browsers.remove(&browser_id) {
            if let Some(browser) = state.browser.lock().unwrap().take()
                && let Some(host) = Browser::host(&browser)
            {
                BrowserHost::close_browser(&host, 1);
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn load_url(&mut self, browser_id: u32, url: &str) -> Response {
        log(&format!("load_url: browser_id={}, url={}", browser_id, url));
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(frame) = Browser::main_frame(browser)
            {
                Frame::load_url(&frame, Some(&CefString::from(url)));
                return Response::Ok;
            }
            Response::Error {
                msg: "browser not ready yet".to_string(),
            }
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn resize(&mut self, browser_id: u32, width: i32, height: i32) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            state.viewport_w.store(width, Ordering::Relaxed);
            state.viewport_h.store(height, Ordering::Relaxed);
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(host) = Browser::host(browser)
            {
                BrowserHost::was_resized(&host);
                BrowserHost::invalidate(&host, PaintElementType::VIEW);
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn mouse_move(&self, browser_id: u32, x: i32, y: i32, modifiers: u32) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(host) = Browser::host(browser)
            {
                let event = MouseEvent { x, y, modifiers };
                BrowserHost::send_mouse_move_event(&host, Some(&event), 0);
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn mouse_click(
        &self,
        browser_id: u32,
        x: i32,
        y: i32,
        modifiers: u32,
        button: u8,
        mouse_up: bool,
        click_count: i32,
    ) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(host) = Browser::host(browser)
            {
                let event = MouseEvent { x, y, modifiers };
                let button_type = match button {
                    1 => MouseButtonType::MIDDLE,
                    2 => MouseButtonType::RIGHT,
                    _ => MouseButtonType::LEFT,
                };
                // mouse-down 時にフォーカスを設定 (OSR ではこれがないとキャレットが出ない)
                if !mouse_up {
                    BrowserHost::set_focus(&host, 1);
                }
                BrowserHost::send_mouse_click_event(
                    &host,
                    Some(&event),
                    button_type,
                    mouse_up as i32,
                    click_count,
                );
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn mouse_wheel(
        &self,
        browser_id: u32,
        x: i32,
        y: i32,
        modifiers: u32,
        delta_x: i32,
        delta_y: i32,
    ) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(host) = Browser::host(browser)
            {
                let event = MouseEvent { x, y, modifiers };
                BrowserHost::send_mouse_wheel_event(&host, Some(&event), delta_x, delta_y);
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn key_event(
        &self,
        browser_id: u32,
        event_type: u8,
        modifiers: u32,
        windows_key_code: i32,
        native_key_code: i32,
        character: u16,
        unmodified_character: u16,
        is_system_key: i32,
        focus_on_editable_field: i32,
    ) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(host) = Browser::host(browser)
            {
                let type_ = match event_type {
                    1 => KeyEventType::KEYUP,
                    2 => KeyEventType::CHAR,
                    _ => KeyEventType::RAWKEYDOWN,
                };
                let event = KeyEvent {
                    size: std::mem::size_of::<KeyEvent>(),
                    type_,
                    modifiers,
                    windows_key_code,
                    native_key_code,
                    is_system_key,
                    character,
                    unmodified_character,
                    focus_on_editable_field,
                };
                BrowserHost::send_key_event(&host, Some(&event));
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn execute_javascript(&self, browser_id: u32, code: &str) -> Response {
        log(&format!(
            "execute_javascript: browser_id={}, code={}",
            browser_id,
            &code[..code.char_indices().nth(100).map_or(code.len(), |(i, _)| i)]
        ));
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(frame) = Browser::main_frame(browser)
            {
                log("execute_javascript: calling Frame::execute_java_script");
                Frame::execute_java_script(
                    &frame,
                    Some(&CefString::from(code)),
                    Some(&CefString::from("cef-unity://execute")),
                    0,
                );
                log("execute_javascript: done");
            } else {
                log("execute_javascript: browser or frame not available");
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn edit_command(&self, browser_id: u32, command: u8) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(frame) = Browser::main_frame(browser)
            {
                match command {
                    0 => frame.copy(),
                    1 => frame.paste(),
                    2 => frame.cut(),
                    3 => frame.select_all(),
                    4 => frame.undo(),
                    5 => frame.redo(),
                    _ => {}
                }
                return Response::Ok;
            }
            Response::Error {
                msg: "browser or frame not available".to_string(),
            }
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn get_current_url(&self, browser_id: u32) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(frame) = Browser::main_frame(browser)
            {
                let url = frame.url();
                let url_str = CefString::from(&url);
                return Response::CurrentUrl {
                    url: url_str.to_string(),
                };
            }
            Response::Error {
                msg: "browser or frame not available".to_string(),
            }
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn ime_set_composition(
        &self,
        browser_id: u32,
        text: &str,
        selection_start: u32,
        selection_end: u32,
    ) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(host) = Browser::host(browser)
            {
                let cef_text = CefString::from(text);
                let char_len = text.chars().count() as u32;
                let underline = CompositionUnderline {
                    size: std::mem::size_of::<CompositionUnderline>(),
                    range: Range {
                        from: 0,
                        to: char_len,
                    },
                    color: 0xFF000000,   // 黒の下線
                    background_color: 0, // 背景なし (透明)
                    thick: 0,            // 細い下線
                    style: CompositionUnderlineStyle::SOLID,
                };
                let selection_range = Range {
                    from: selection_start,
                    to: selection_end,
                };
                let invalid_range = Range {
                    from: u32::MAX,
                    to: u32::MAX,
                };
                BrowserHost::ime_set_composition(
                    &host,
                    Some(&cef_text),
                    Some(&[underline]),
                    Some(&invalid_range),
                    Some(&selection_range),
                );
                log(format!(
                    "ime set composition: text={}, selection_start={}, selection_end={}",
                    text, selection_start, selection_end
                )
                .as_str());
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn ime_commit_text(&self, browser_id: u32, text: &str) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(host) = Browser::host(browser)
            {
                let cef_text = CefString::from(text);
                // macOS では replacement_range に InvalidRange ({UINT32_MAX, UINT32_MAX}) を渡す必要がある。
                // None (null pointer) を渡すと CEF 内部で {0, 0} に変換され、正しく動作しない。
                let invalid_range = Range {
                    from: u32::MAX,
                    to: u32::MAX,
                };
                BrowserHost::ime_commit_text(&host, Some(&cef_text), Some(&invalid_range), 0);
                log(format!("ime commit text: text={}", text).as_str());
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn ime_finish_composing_text(&self, browser_id: u32, keep_selection: bool) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(host) = Browser::host(browser)
            {
                BrowserHost::ime_finish_composing_text(&host, keep_selection as i32);
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    fn ime_cancel_composition(&self, browser_id: u32) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(host) = Browser::host(browser)
            {
                BrowserHost::ime_cancel_composition(&host);
            }
            Response::Ok
        } else {
            Response::Error {
                msg: format!("browser {} not found", browser_id),
            }
        }
    }

    /// Shut down all browsers and CEF.
    pub fn shutdown(&mut self) {
        log("shutting down all browsers");
        let ids: Vec<u32> = self.browsers.keys().copied().collect();
        for id in ids {
            self.destroy_browser(id);
        }
        // Pump a few times to process close commands
        for _ in 0..10 {
            do_message_loop_work();
        }
        cef::shutdown();
        log("CEF shutdown complete");
    }
}

// ---------------------------------------------------------------------------
// Platform-specific helper binary path
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn helper_binary_path(exe_dir: &std::path::Path) -> std::path::PathBuf {
    // CEF は browser_subprocess_path から "Helper (GPU)" 等のバリアントを自動検出する。
    // <server.app>/Contents/Frameworks/cef-unity-server Helper.app/Contents/MacOS/cef-unity-server Helper
    exe_dir
        .parent()
        .unwrap() // Contents
        .join("Frameworks/cef-unity-server Helper.app/Contents/MacOS/cef-unity-server Helper")
}

#[cfg(target_os = "linux")]
fn helper_binary_path(exe_dir: &std::path::Path) -> std::path::PathBuf {
    exe_dir.join("cef-unity-rust-helper")
}

#[cfg(target_os = "windows")]
fn helper_binary_path(exe_dir: &std::path::Path) -> std::path::PathBuf {
    exe_dir.join("cef-unity-rust-helper.exe")
}
