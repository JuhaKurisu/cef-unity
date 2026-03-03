use cef::*;
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

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
// Global state
// ---------------------------------------------------------------------------

/// CEF library loaded + initialized (never reset; CEF cannot re-initialize)
static CEF_INITIALIZED: AtomicBool = AtomicBool::new(false);

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
            eprintln!("[cef-unity] on_paint called: {}x{}", width, height);
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
            eprintln!("[cef-unity] on_after_created called");
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
///
/// CEF is initialized on a dedicated background thread with external_message_pump
/// to prevent CEF from hooking into the host application's main-thread CFRunLoop.
/// This avoids the ChromeWebAppShortcutCopierMain crash on macOS.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_init() -> i32 {
    // CEF cannot be re-initialized after shutdown in the same process.
    // On subsequent calls (Unity Play/Stop cycles), just ensure the
    // pump thread is running.
    if CEF_INITIALIZED.load(Ordering::SeqCst) {
        return 0;
    }

    let plugin_dir = crate::dylib_dir();
    eprintln!("[cef-unity] plugin_dir = {}", plugin_dir.display());

    let framework_dir = match crate::find_cef_framework(&plugin_dir) {
        Some(fw) => fw,
        None => {
            eprintln!(
                "[cef-unity] CEF framework not found in {} or CEF_PATH",
                plugin_dir.display()
            );
            return -3;
        }
    };
    eprintln!("[cef-unity] framework_dir = {}", framework_dir.display());

    crate::load_cef(&framework_dir);

    // Symlink GPU libraries next to the dylib
    let libraries_dir = framework_dir.join("Libraries");
    if libraries_dir.exists() {
        for lib in &["libGLESv2.dylib", "libEGL.dylib"] {
            let src = libraries_dir.join(lib);
            let dst = plugin_dir.join(lib);
            if src.exists() && !dst.exists() {
                let _ = std::os::unix::fs::symlink(&src, &dst);
            }
        }
    }

    // Spawn a dedicated CEF thread that initializes and then pumps messages.
    // do_message_loop_work() must be called on the same thread as initialize().
    let fw_dir = framework_dir.clone();
    let p_dir = plugin_dir.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    let _handle = thread::Builder::new()
        .name("cef-ui".into())
        .spawn(move || {
            let args = cef::args::Args::new();
            let resources_dir = fw_dir.join("Resources");

            // ヘルパーバイナリのパス: .appバンドルがあればそちらを使う。
            // .appバンドル内のInfo.plistでCFBundleIdentifierを親プロセスに合わせることで
            // MachPortRendezvousServerのサービス名が一致する。
            let helper_app = p_dir.join("cef-unity-rust-helper.app/Contents/MacOS/cef-unity-rust-helper");
            let helper_bare = p_dir.join("cef-unity-rust-helper");
            let helper_path = if helper_app.exists() { helper_app } else { helper_bare };

            let cache_dir = p_dir.join("cef_cache");

            let mut settings = Settings::default();
            settings.no_sandbox = 1;
            settings.windowless_rendering_enabled = 1;
            settings.external_message_pump = 1;
            settings.framework_dir_path = CefString::from(fw_dir.to_str().unwrap());
            settings.browser_subprocess_path = CefString::from(helper_path.to_str().unwrap());
            settings.resources_dir_path = CefString::from(resources_dir.to_str().unwrap());
            settings.root_cache_path = CefString::from(cache_dir.to_str().unwrap());
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
            eprintln!("[cef-unity] initialize() returned {}", result);
            let _ = tx.send(result);

            if result != 0 {
                // Pump messages forever on this thread (CEF cannot re-initialize)
                loop {
                    do_message_loop_work();
                    thread::sleep(std::time::Duration::from_millis(16));
                }
            }
        })
        .expect("failed to spawn CEF UI thread");

    let result = match rx.recv() {
        Ok(r) => r,
        Err(_) => {
            eprintln!("[cef-unity] CEF UI thread died before sending result");
            return -4;
        }
    };

    if result == 0 {
        eprintln!("[cef-unity] initialize() failed");
        return -2;
    }

    CEF_INITIALIZED.store(true, Ordering::SeqCst);
    0
}

/// No-op. The CEF UI thread runs for the lifetime of the process because
/// CEF cannot be re-initialized after shutdown.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_shutdown() {}

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
    if !CEF_INITIALIZED.load(Ordering::SeqCst) || url.is_null() {
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
    let new_frame = back.has_new_frame;
    if new_frame {
        std::mem::swap(&mut instance.front, &mut back.data);
        instance.front_w = back.width;
        instance.front_h = back.height;
        back.has_new_frame = false;
    }
    drop(back);

    unsafe {
        *out_buffer = instance.front.as_ptr();
        *out_width = instance.front_w;
        *out_height = instance.front_h;
    }
    new_frame as i32
}
