use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

use cef::args::Args;
use cef::*;

wrap_app! {
    struct MyApp;
    impl App {}
}

wrap_life_span_handler! {
    struct MyLifeSpanHandler;
    impl LifeSpanHandler {
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            quit_message_loop();
        }
    }
}

wrap_client! {
    struct MyClient;
    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(MyLifeSpanHandler::new())
        }
    }
}

fn main() {
    // Load CEF library from build output
    let cef_dir = cef::sys::get_cef_dir().expect("CEF directory not found");
    let framework_path = cef_dir.join(cef::sys::FRAMEWORK_PATH);
    let framework_cstr =
        CString::new(framework_path.as_os_str().as_bytes()).expect("Invalid framework path");
    assert_eq!(
        load_library(Some(unsafe { &*framework_cstr.as_ptr().cast() })),
        1,
        "Failed to load CEF framework"
    );

    // Configure CEF API version (required before initialize)
    api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();

    // Resolve paths
    let framework_dir = cef_dir.join("Chromium Embedded Framework.framework");
    let resources_dir = framework_dir.join("Resources");
    let locales_dir = resources_dir.join("locales");
    let libraries_dir = framework_dir.join("Libraries");

    // Helper binary is in the same directory as the main binary
    let exe_path = std::env::current_exe().expect("Failed to get current executable path");
    let exe_dir = exe_path.parent().unwrap();
    let helper_path = exe_dir.join("cef-unity-rust-helper");

    // Symlink GPU libraries (libGLESv2.dylib, libEGL.dylib) next to the executable
    // so that CEF's GPU process can find them
    if libraries_dir.exists() {
        for lib in &["libGLESv2.dylib", "libEGL.dylib"] {
            let src = libraries_dir.join(lib);
            let dst = exe_dir.join(lib);
            if src.exists() && !dst.exists() {
                let _ = std::os::unix::fs::symlink(&src, &dst);
            }
        }
    }

    let mut settings = Settings::default();
    settings.no_sandbox = 1;
    settings.framework_dir_path = CefString::from(framework_dir.to_str().unwrap());
    settings.browser_subprocess_path = CefString::from(helper_path.to_str().unwrap());
    settings.resources_dir_path = CefString::from(resources_dir.to_str().unwrap());
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
        panic!("Failed to initialize CEF");
    }

    let window_info = WindowInfo::default();
    let mut client = MyClient::new();
    let url = CefString::from("https://www.google.com");
    let browser_settings = BrowserSettings::default();

    browser_host_create_browser(
        Some(&window_info),
        Some(&mut client),
        Some(&url),
        Some(&browser_settings),
        None,
        None,
    );

    run_message_loop();
    shutdown();
}
