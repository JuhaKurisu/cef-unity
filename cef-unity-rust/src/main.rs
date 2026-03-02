use cef::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const WIDTH: i32 = 1920 * 2;
const HEIGHT: i32 = 1080 * 2;

wrap_render_handler! {
    struct MyRenderHandler {
        saved: Arc<AtomicBool>,
    }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = WIDTH;
                rect.height = HEIGHT;
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
            if self.saved.swap(true, Ordering::SeqCst) {
                return;
            }

            let size = (width * height * 4) as usize;
            let bgra = unsafe { std::slice::from_raw_parts(buffer, size) };
            let mut rgba = bgra.to_vec();
            // BGRA -> RGBA: swap B and R channels
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }

            if let Some(img) = image::RgbaImage::from_raw(width as u32, height as u32, rgba) {
                img.save("output.png").expect("Failed to save PNG");
                eprintln!("Saved output.png ({}x{})", width, height);
            }
            quit_message_loop();
        }
    }
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
    struct MyClient {
        render_handler: RenderHandler,
    }
    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(MyLifeSpanHandler::new())
        }
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render_handler.clone())
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
    settings.windowless_rendering_enabled = 1;
    settings.framework_dir_path = CefString::from(framework_dir.to_str().unwrap());
    settings.browser_subprocess_path =
        CefString::from(exe_dir.join("cef-unity-rust-helper").to_str().unwrap());
    settings.resources_dir_path = CefString::from(resources_dir.to_str().unwrap());
    let locales_dir = resources_dir.join("locales");
    if locales_dir.exists() {
        settings.locales_dir_path = CefString::from(locales_dir.to_str().unwrap());
    }

    assert_ne!(
        initialize(
            Some(args.as_main_args()),
            Some(&settings),
            None,
            std::ptr::null_mut()
        ),
        0
    );

    let render_handler = MyRenderHandler::new(Arc::new(AtomicBool::new(false)));
    let mut client = MyClient::new(render_handler);
    let window_info = WindowInfo::default().set_as_windowless(std::ptr::null_mut());

    browser_host_create_browser(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(
            format!(
                "file://{}",
                std::fs::canonicalize("test_alpha.html")
                    .expect("test_alpha.html not found")
                    .display()
            )
            .as_str(),
        )),
        Some(&BrowserSettings {
            background_color: 0x00000000,
            ..Default::default()
        }),
        None,
        None,
    );

    run_message_loop();
    shutdown();
}
