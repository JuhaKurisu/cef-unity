// CEF Server: browser management, CEF handlers, IPC command processing.

use cef::*;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cef_unity_ipc::{self as ipc, Command, Response, ShmWriter};

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn log(msg: &str) {
    let path = std::env::temp_dir().join("cef_unity_server.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[{:?}] {}", std::time::SystemTime::now(), msg);
    }
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
            if count <= 5 || count.is_multiple_of(100) {
                log(&format!("on_paint #{}: {}x{}", count, width, height));
            }
            if type_.get_raw() != PaintElementType::VIEW.get_raw() {
                return;
            }
            let size = (width * height * 4) as usize;
            let src = unsafe { std::slice::from_raw_parts(buffer, size) };
            self.shm.write_frame(src, width as u32, height as u32);
        }
    }
}

wrap_life_span_handler! {
    struct ServerLifeSpanHandler {
        browser_slot: Arc<Mutex<Option<Browser>>>,
    }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            log("on_after_created called");
            if let Some(b) = browser {
                *self.browser_slot.lock().unwrap() = Some(b.clone());
                log("browser stored in slot");
            }
        }
    }
}

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
    }
    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(cl) = command_line {
                cl.append_switch(Some(&CefString::from("use-mock-keychain")));
                cl.append_switch(Some(&CefString::from("single-process")));
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
    }
    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render_handler.clone())
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span_handler.clone())
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
}

impl CefServer {
    pub fn new() -> Self {
        CefServer {
            browsers: HashMap::new(),
            next_browser_id: AtomicU32::new(1),
            server_pid: std::process::id(),
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
        let mut app = ServerApp::new(bph);
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

        let render_handler = ServerRenderHandler::new(
            Arc::clone(&shm),
            Arc::clone(&viewport_w),
            Arc::clone(&viewport_h),
        );
        let life_span_handler = ServerLifeSpanHandler::new(Arc::clone(&browser_slot));
        let mut client = ServerClient::new(render_handler, life_span_handler);

        let window_info = WindowInfo::default().set_as_windowless(std::ptr::null_mut());
        let ok = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(url)),
            Some(&BrowserSettings {
                background_color: 0xFF000000,
                windowless_frame_rate: 60,
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

        self.browsers.insert(
            id,
            BrowserState {
                shm,
                browser: browser_slot,
                viewport_w,
                viewport_h,
            },
        );

        Response::BrowserCreated {
            browser_id: id,
            shm_flink,
        }
    }

    fn destroy_browser(&mut self, browser_id: u32) -> Response {
        if let Some(state) = self.browsers.remove(&browser_id) {
            if let Some(browser) = state.browser.lock().unwrap().take()
                && let Some(host) = Browser::host(&browser) {
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
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap()
                && let Some(frame) = Browser::main_frame(browser) {
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
                && let Some(host) = Browser::host(browser) {
                    BrowserHost::was_resized(&host);
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
                && let Some(host) = Browser::host(browser) {
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
                && let Some(host) = Browser::host(browser) {
                    let event = MouseEvent { x, y, modifiers };
                    let button_type = match button {
                        1 => MouseButtonType::MIDDLE,
                        2 => MouseButtonType::RIGHT,
                        _ => MouseButtonType::LEFT,
                    };
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
                && let Some(host) = Browser::host(browser) {
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
    // <server.app>/Contents/Helpers/cef-unity-rust-helper.app/Contents/MacOS/cef-unity-rust-helper
    exe_dir
        .parent()
        .unwrap() // Contents
        .join("Helpers/cef-unity-rust-helper.app/Contents/MacOS/cef-unity-rust-helper")
}

#[cfg(target_os = "linux")]
fn helper_binary_path(exe_dir: &std::path::Path) -> std::path::PathBuf {
    exe_dir.join("cef-unity-rust-helper")
}

#[cfg(target_os = "windows")]
fn helper_binary_path(exe_dir: &std::path::Path) -> std::path::PathBuf {
    exe_dir.join("cef-unity-rust-helper.exe")
}
