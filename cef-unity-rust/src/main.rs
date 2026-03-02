use cef::*;

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
    let cef_dir = cef_unity_rust::load_cef();
    let args = cef::args::Args::new();

    let framework_dir = cef_dir.join("Chromium Embedded Framework.framework");
    let exe_dir = std::env::current_exe().unwrap();
    let exe_dir = exe_dir.parent().unwrap();

    // GPUライブラリを実行ファイルの隣にシンボリックリンク
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
    settings.framework_dir_path = CefString::from(framework_dir.to_str().unwrap());
    settings.browser_subprocess_path =
        CefString::from(exe_dir.join("cef-unity-rust-helper").to_str().unwrap());
    settings.resources_dir_path = CefString::from(resources_dir.to_str().unwrap());
    let locales_dir = resources_dir.join("locales");
    if locales_dir.exists() {
        settings.locales_dir_path = CefString::from(locales_dir.to_str().unwrap());
    }

    assert_ne!(
        initialize(Some(args.as_main_args()), Some(&settings), None, std::ptr::null_mut()),
        0
    );

    let mut client = MyClient::new();
    browser_host_create_browser(
        Some(&WindowInfo::default()),
        Some(&mut client),
        Some(&CefString::from("https://www.google.com")),
        Some(&BrowserSettings::default()),
        None,
        None,
    );

    run_message_loop();
    shutdown();
}
