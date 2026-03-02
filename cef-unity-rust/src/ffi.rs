use cef::*;
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Opaque handle type (becomes IntPtr in C#)
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct CefUnityBrowser {
    _opaque: u8,
}

// ---------------------------------------------------------------------------
// Shared state between CEF thread and Unity main thread
// ---------------------------------------------------------------------------

struct BackBuffer {
    data: Vec<u8>,
    width: i32,
    height: i32,
    has_new_frame: bool,
}

struct SharedState {
    buffer: Mutex<BackBuffer>,
    browser: Arc<Mutex<Option<Browser>>>,
    viewport_w: AtomicI32,
    viewport_h: AtomicI32,
}

// ---------------------------------------------------------------------------
// Browser instance (the real data behind the opaque handle)
// ---------------------------------------------------------------------------

struct BrowserInstance {
    shared: Arc<SharedState>,
    front: Vec<u8>,
    front_w: i32,
    front_h: i32,
}

fn handle_to_ref<'a>(handle: *mut CefUnityBrowser) -> &'a mut BrowserInstance {
    unsafe { &mut *(handle as *mut BrowserInstance) }
}

// ---------------------------------------------------------------------------
// Global state – tracks whether CEF is initialized
// ---------------------------------------------------------------------------

static CEF_INITIALIZED: OnceLock<bool> = OnceLock::new();

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

wrap_render_handler! {
    struct MyRenderHandler {
        shared: Arc<SharedState>,
    }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = self.shared.viewport_w.load(Ordering::Relaxed);
                rect.height = self.shared.viewport_h.load(Ordering::Relaxed);
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
            if type_.get_raw() != PaintElementType::VIEW.get_raw() {
                return;
            }
            let size = (width * height * 4) as usize;
            let src = unsafe { std::slice::from_raw_parts(buffer, size) };
            let mut back = self.shared.buffer.lock().unwrap();
            back.data.resize(size, 0);
            back.data.copy_from_slice(src);
            back.width = width;
            back.height = height;
            back.has_new_frame = true;
        }
    }
}

wrap_life_span_handler! {
    struct MyLifeSpanHandler {
        browser_slot: Arc<Mutex<Option<Browser>>>,
    }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(b) = browser {
                *self.browser_slot.lock().unwrap() = Some(b.clone());
            }
        }
    }
}

wrap_app! {
    struct MyApp;
    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(cl) = command_line {
                cl.append_switch(Some(&CefString::from("use-mock-keychain")));
            }
        }
    }
}

wrap_client! {
    struct MyClient {
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
// Global functions – CEF framework lifecycle
// ---------------------------------------------------------------------------

/// Initialize the CEF framework. Call once at app startup.
/// Returns 0 on success, non-zero on failure.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_init() -> i32 {
    if CEF_INITIALIZED.get().is_some() {
        return -1; // already initialized
    }

    let cef_dir = crate::load_cef();
    let args = cef::args::Args::new();

    let framework_dir = cef_dir.join("Chromium Embedded Framework.framework");
    let exe_dir = std::env::current_exe().unwrap();
    let exe_dir = exe_dir.parent().unwrap();

    // Symlink GPU libraries next to the executable
    let libraries_dir = framework_dir.join("Libraries");
    if libraries_dir.exists() {
        for lib in &["libGLESv2.dylib", "libEGL.dylib"] {
            let src = libraries_dir.join(lib);
            let dst = exe_dir.join(lib);
            if src.exists() && !dst.exists() {
                let _ = std::os::unix::fs::symlink(&src, &dst);
            }
        }
    }

    let resources_dir = framework_dir.join("Resources");
    let mut settings = Settings::default();
    settings.no_sandbox = 1;
    settings.windowless_rendering_enabled = 1;
    settings.external_message_pump = 1;
    settings.framework_dir_path = CefString::from(framework_dir.to_str().unwrap());
    settings.browser_subprocess_path =
        CefString::from(exe_dir.join("cef-unity-rust-helper").to_str().unwrap());
    settings.resources_dir_path = CefString::from(resources_dir.to_str().unwrap());
    let locales_dir = resources_dir.join("locales");
    if locales_dir.exists() {
        settings.locales_dir_path = CefString::from(locales_dir.to_str().unwrap());
    }

    let mut app = MyApp::new();
    let result = initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    );

    if result == 0 {
        return -2; // initialize failed
    }

    let _ = CEF_INITIALIZED.set(true);
    0
}

/// Pump the CEF message loop for one iteration.
/// Call from Unity's Update() every frame.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_tick() {
    if CEF_INITIALIZED.get().is_some() {
        do_message_loop_work();
    }
}

/// Shut down the CEF framework. Call once at app exit.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_shutdown() {
    if CEF_INITIALIZED.get().is_some() {
        shutdown();
    }
}

// ---------------------------------------------------------------------------
// Instance functions – per-browser, via opaque handle
// ---------------------------------------------------------------------------

/// Create a browser instance. Returns a handle (null on failure).
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_create_browser(
    width: i32,
    height: i32,
    url: *const c_char,
) -> *mut CefUnityBrowser {
    if CEF_INITIALIZED.get().is_none() || url.is_null() {
        return std::ptr::null_mut();
    }

    let url_str = unsafe { CStr::from_ptr(url) }.to_str().unwrap_or("");

    let shared = Arc::new(SharedState {
        buffer: Mutex::new(BackBuffer {
            data: Vec::new(),
            width: 0,
            height: 0,
            has_new_frame: false,
        }),
        browser: Arc::new(Mutex::new(None)),
        viewport_w: AtomicI32::new(width),
        viewport_h: AtomicI32::new(height),
    });

    let render_handler = MyRenderHandler::new(Arc::clone(&shared));
    let life_span_handler = MyLifeSpanHandler::new(Arc::clone(&shared.browser));
    let mut client = MyClient::new(render_handler, life_span_handler);

    let window_info = WindowInfo::default().set_as_windowless(std::ptr::null_mut());

    browser_host_create_browser(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(url_str)),
        Some(&BrowserSettings {
            background_color: 0x00000000,
            ..Default::default()
        }),
        None,
        None,
    );

    let instance = Box::new(BrowserInstance {
        shared,
        front: Vec::new(),
        front_w: 0,
        front_h: 0,
    });
    Box::into_raw(instance) as *mut CefUnityBrowser
}

/// Destroy a browser instance.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_destroy_browser(handle: *mut CefUnityBrowser) {
    if handle.is_null() {
        return;
    }
    let instance = unsafe { Box::from_raw(handle as *mut BrowserInstance) };
    // Close the browser if it was created
    if let Some(browser) = instance.shared.browser.lock().unwrap().take() {
        if let Some(host) = Browser::host(&browser) {
            BrowserHost::close_browser(&host, 1);
        }
    }
    drop(instance);
}

/// Load a URL in the browser.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_load_url(handle: *mut CefUnityBrowser, url: *const c_char) {
    if handle.is_null() || url.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);
    let url_str = unsafe { CStr::from_ptr(url) }.to_str().unwrap_or("");
    if let Some(ref browser) = *instance.shared.browser.lock().unwrap() {
        if let Some(frame) = Browser::main_frame(browser) {
            Frame::load_url(&frame, Some(&CefString::from(url_str)));
        }
    }
}

/// Resize the browser viewport.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_resize(handle: *mut CefUnityBrowser, width: i32, height: i32) {
    if handle.is_null() {
        return;
    }
    let instance = handle_to_ref(handle);
    instance.shared.viewport_w.store(width, Ordering::Relaxed);
    instance.shared.viewport_h.store(height, Ordering::Relaxed);
    // Notify CEF that the view was resized
    if let Some(ref browser) = *instance.shared.browser.lock().unwrap() {
        if let Some(host) = Browser::host(browser) {
            BrowserHost::was_resized(&host);
        }
    }
}

/// Get the latest frame buffer.
/// Returns 1 if a new frame is available, 0 if unchanged.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_get_buffer(
    handle: *mut CefUnityBrowser,
    out_buffer: *mut *const u8,
    out_width: *mut i32,
    out_height: *mut i32,
) -> i32 {
    if handle.is_null() || out_buffer.is_null() || out_width.is_null() || out_height.is_null() {
        return 0;
    }
    let instance = handle_to_ref(handle);

    let mut back = instance.shared.buffer.lock().unwrap();
    if back.has_new_frame {
        std::mem::swap(&mut instance.front, &mut back.data);
        instance.front_w = back.width;
        instance.front_h = back.height;
        back.has_new_frame = false;
        drop(back);

        unsafe {
            *out_buffer = instance.front.as_ptr();
            *out_width = instance.front_w;
            *out_height = instance.front_h;
        }
        1
    } else {
        drop(back);
        unsafe {
            *out_buffer = instance.front.as_ptr();
            *out_width = instance.front_w;
            *out_height = instance.front_h;
        }
        0
    }
}
