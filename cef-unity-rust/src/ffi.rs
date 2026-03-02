use cef::*;
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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
// Global state – tracks whether CEF is initialized
// ---------------------------------------------------------------------------

static CEF_INITIALIZED: OnceLock<bool> = OnceLock::new();
static CEF_THREAD: OnceLock<Mutex<Option<thread::JoinHandle<()>>>> = OnceLock::new();

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
                cl.append_switch(Some(&CefString::from("single-process")));
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
/// CEF is initialized on a dedicated background thread to avoid conflicting
/// with Unity's NSApplication run loop on macOS.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_init() -> i32 {
    if CEF_INITIALIZED.get().is_some() {
        return -1; // already initialized
    }

    // Resolve all paths relative to the dylib's directory
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

    // Load the CEF dynamic library (dlopen + api_hash)
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

    // Spawn a dedicated thread for CEF initialization + message loop.
    // This avoids conflicting with Unity's main-thread NSApplication run loop.
    let fw_dir = framework_dir.clone();
    let p_dir = plugin_dir.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    let handle = thread::Builder::new()
        .name("cef-message-loop".into())
        .spawn(move || {
            let args = cef::args::Args::new();
            let resources_dir = fw_dir.join("Resources");

            let mut settings = Settings::default();
            settings.no_sandbox = 1;
            settings.windowless_rendering_enabled = 1;
            // Default message loop: CEF runs its own CFRunLoop on this thread.
            // (no external_message_pump, no multi_threaded_message_loop)
            settings.framework_dir_path = CefString::from(fw_dir.to_str().unwrap());
            settings.browser_subprocess_path =
                CefString::from(p_dir.join("cef-unity-rust-helper").to_str().unwrap());
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

            let _ = tx.send(result);

            if result != 0 {
                eprintln!("[cef-unity] CEF message loop starting on background thread");
                run_message_loop();
                eprintln!("[cef-unity] CEF message loop ended, calling shutdown");
                shutdown();
                eprintln!("[cef-unity] CEF shutdown complete");
            } else {
                eprintln!("[cef-unity] initialize() returned 0 (failure)");
            }
        })
        .expect("failed to spawn CEF thread");

    let result = match rx.recv() {
        Ok(r) => r,
        Err(_) => {
            eprintln!("[cef-unity] CEF thread died before sending result");
            return -4;
        }
    };

    if result == 0 {
        eprintln!("[cef-unity] initialize() failed");
        return -2;
    }

    let _ = CEF_THREAD.set(Mutex::new(Some(handle)));
    let _ = CEF_INITIALIZED.set(true);
    eprintln!("[cef-unity] initialization successful");
    0
}

/// Pump the CEF message loop for one iteration.
/// CEF runs its own message loop on a dedicated background thread,
/// so this is a no-op. Kept for API compatibility.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_tick() {
    // CEF message loop runs on its own dedicated thread.
}

/// Shut down the CEF framework. Call once at app exit.
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_shutdown() {
    if CEF_INITIALIZED.get().is_some() {
        eprintln!("[cef-unity] requesting CEF message loop quit");
        quit_message_loop();
        // Wait for the CEF thread to finish (it calls shutdown() internally)
        if let Some(mutex) = CEF_THREAD.get() {
            if let Some(handle) = mutex.lock().unwrap().take() {
                let _ = handle.join();
            }
        }
        eprintln!("[cef-unity] shutdown complete");
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
