// CEF Server: browser management, CEF handlers, IPC command processing.

use cef::*;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

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

use cef_unity_ipc::{self as ipc, AudioShmWriter, Command, Response, ShmWriter};

use crate::d3d11_pool::D3D11Pool;

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

const MAX_LOG_ENTRIES: usize = 1000;
static LOG_BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// ログ有効/無効。main で --logging に従って設定される。false で全ログ抑制
/// (ファイル書き込み・バッファ蓄積の双方を行わない → GetLogs も空を返す)。
static LOG_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// ログ出力の有効/無効を設定する。CEF 初期化前に呼ぶこと。
pub fn set_logging(enabled: bool) {
    LOG_ENABLED.store(enabled, Ordering::Relaxed);
}

fn log(msg: &str) {
    if !LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let path = std::env::temp_dir().join("cef_unity_server.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[{:?}] {}", std::time::SystemTime::now(), msg);
    }

    let mut buf = LOG_BUFFER.lock().unwrap_or_else(PoisonError::into_inner);
    if buf.len() >= MAX_LOG_ENTRIES {
        buf.remove(0);
    }
    buf.push(msg.to_string());
}

fn drain_logs() -> Vec<String> {
    std::mem::take(&mut *LOG_BUFFER.lock().unwrap_or_else(PoisonError::into_inner))
}

// ---------------------------------------------------------------------------
// CEF loader
// ---------------------------------------------------------------------------

/// macOS: current_exe() からの相対パスで CEF フレームワークを動的ロードする。
/// バンドル構造: Contents/MacOS/<exe> → Contents/Frameworks/Chromium Embedded Framework.framework/
#[cfg(target_os = "macos")]
fn load_cef_auto() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let exe = std::env::current_exe().expect("failed to get current_exe");
    let frameworks_dir = exe
        .parent().unwrap()   // MacOS
        .parent().unwrap()   // Contents
        .join("Frameworks");
    let framework_path = frameworks_dir.join(cef::sys::FRAMEWORK_PATH);
    let cstr = CString::new(framework_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        cef::load_library(Some(unsafe { &*cstr.as_ptr().cast() })),
        1,
        "Failed to load CEF framework: {}",
        framework_path.display()
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
    /// 音声リングバッファ。AudioHandler が PCM を書き込む。ブラウザ破棄まで生かす。
    #[allow(dead_code)]
    audio_shm: Arc<AudioShmWriter>,
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

// ---------------------------------------------------------------------------
// BeginFrame → on_accelerated_paint レイテンシ計測
// ---------------------------------------------------------------------------

/// プロセス起動からのモノトニック時刻基準点。
fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ns() -> u64 {
    epoch().elapsed().as_nanos() as u64
}

/// 最後に send_external_begin_frame を呼んだ時刻 (ns since epoch)。
/// 0 は未発行を意味する。
static LAST_BEGIN_FRAME_NS: AtomicU64 = AtomicU64::new(0);

/// 最後の SendExternalBeginFrame に載っていた Unity の Time.frameCount。
/// on_accelerated_paint で shm に転送し、Unity 側で end-to-end の遅延フレーム数を測る。
static LAST_BEGIN_FRAME_UNITY_FRAME: AtomicU64 = AtomicU64::new(0);

/// 直近 N サンプルの BeginFrame → paint レイテンシ集計バッファ (μs 単位)。
const LATENCY_WINDOW: usize = 60;
static LATENCY_SAMPLES: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// paint 到着時にレイテンシを記録し、N サンプル貯まったら統計をログに出す。
fn record_paint_latency() {
    // on_accelerated_paint の hot path から毎フレーム呼ばれるため、
    // ログ無効時は lock/push/sort を一切行わず素通しする。
    if !LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let begin_ns = LAST_BEGIN_FRAME_NS.load(Ordering::Relaxed);
    if begin_ns == 0 {
        return; // BeginFrame 未発行 (初期化中の自発フレーム等)
    }
    let now = now_ns();
    if now <= begin_ns {
        return;
    }
    let elapsed_us = (now - begin_ns) / 1000;

    let mut samples = LATENCY_SAMPLES.lock().unwrap_or_else(PoisonError::into_inner);
    samples.push(elapsed_us);
    if samples.len() >= LATENCY_WINDOW {
        let count = samples.len() as u64;
        let sum: u64 = samples.iter().sum();
        let avg = sum / count;
        let min = *samples.iter().min().unwrap();
        let max = *samples.iter().max().unwrap();
        // 中央値も出す (外れ値の影響を見るため)
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        samples.clear();
        drop(samples);
        log(&format!(
            "BeginFrame→paint latency (n={}): avg={}.{:03}ms median={}.{:03}ms min={}.{:03}ms max={}.{:03}ms",
            count,
            avg / 1000, avg % 1000,
            median / 1000, median % 1000,
            min / 1000, min % 1000,
            max / 1000, max % 1000,
        ));
    }
}

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
            let (width, height) = (width as u32, height as u32);
            // software 経路の shm バッファは MAX_W×MAX_H 固定長。超過フレームを
            // write_frame に渡すと assert panic → CEF コールバック越しの unwind で
            // プロセス abort するため、ここで読み捨てる (制約は software 経路にのみ
            // 実在する。viewport 側で clamp すると GPU 経路まで Unity の想定サイズと
            // 乖離して縦伸び/マウス座標ズレになる — 2026-07-23 のリグレッションで実証)。
            if width == 0 || height == 0 || width > ipc::MAX_W || height > ipc::MAX_H {
                if count <= 3 || count.is_multiple_of(100) {
                    log(&format!(
                        "on_paint: {}x{} exceeds software shm buffer ({}x{}) — frame skipped",
                        width, height, ipc::MAX_W, ipc::MAX_H
                    ));
                }
                return;
            }
            let size = (width as usize) * (height as usize) * 4;
            let src = unsafe { std::slice::from_raw_parts(buffer, size) };
            self.shm.write_frame(src, width, height);
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

                // BeginFrame → paint レイテンシを記録 (外的 BeginFrame モードでのみ意味あり)
                record_paint_latency();

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

                // 生存確認用ログ: 最初の 5 件 + 600 件ごと (≒10秒 @ 60fps)
                if count <= 5 || count.is_multiple_of(600) {
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
                // accel_frame_id 増分の前に Unity frame を書く: クライアントは frame_id 変化
                // を検出してから他フィールドを読むため、これより前に書いてあれば read 時に
                // 必ず観測される。
                self.shm
                    .write_paint_unity_frame(LAST_BEGIN_FRAME_UNITY_FRAME.load(Ordering::Relaxed));
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

                // BeginFrame → paint レイテンシを記録 (外的 BeginFrame モードでのみ意味あり)
                record_paint_latency();

                match pool.copy_from_source(src_handle, w, h, dxgi_format) {
                    Ok((client_handle, fence_value)) => {
                        if count <= 5 || count.is_multiple_of(600) {
                            log(&format!(
                                "on_accelerated_paint #{}: {}x{} fmt={} client_handle=0x{:x} fence={}",
                                count, w, h, format_tag, client_handle, fence_value
                            ));
                        }
                        // d3d11_frame_id 増分の前に Unity frame を書く。
                        self.shm.write_paint_unity_frame(
                            LAST_BEGIN_FRAME_UNITY_FRAME.load(Ordering::Relaxed),
                        );
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
                *self.browser_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(b.clone());
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

/// JavaScript to track caret position via selectionchange / click / focusin events.
/// Reports position as console.log("__CARET__:x:y:w:h") (viewport 座標)。
///
/// `window.getSelection()` は input/textarea 内部のキャレットを返さない
/// (テキストコントロールの選択は selectionStart/End でしか公開されない) ため、
/// テキストコントロールは mirror div 方式 (textarea-caret-position と同手法) で
/// キャレット座標を計算する。空の contenteditable は collapsed range の rect が
/// 全ゼロになるため要素矩形へフォールバックする。
const CARET_TRACKING_JS: &str = r#"
(function() {
    if (window.__cefUnityCaretTracker) return;
    window.__cefUnityCaretTracker = true;

    var MIRROR_PROPS = [
        "direction", "boxSizing", "width", "height", "overflowX", "overflowY",
        "borderTopWidth", "borderRightWidth", "borderBottomWidth", "borderLeftWidth",
        "borderStyle",
        "paddingTop", "paddingRight", "paddingBottom", "paddingLeft",
        "fontStyle", "fontVariant", "fontWeight", "fontStretch", "fontSize",
        "fontSizeAdjust", "lineHeight", "fontFamily",
        "textAlign", "textTransform", "textIndent", "textDecoration",
        "letterSpacing", "wordSpacing", "tabSize"
    ];

    function lineHeightOf(computed) {
        var lh = parseFloat(computed.lineHeight);
        if (!lh) lh = (parseFloat(computed.fontSize) || 16) * 1.2;
        return lh;
    }

    // selection API をサポートする input type のみ true (email/number は throw する)
    function isTextControl(el) {
        if (!el || !el.nodeName) return false;
        if (el.nodeName === "TEXTAREA") return true;
        if (el.nodeName !== "INPUT") return false;
        var t = (el.type || "text").toLowerCase();
        return t === "text" || t === "search" || t === "tel" ||
               t === "url" || t === "password";
    }

    // フィールド先頭 (padding 内側) の座標。キャレット位置が計算できない場合の近似。
    function elementCaretFallback(el) {
        var r = el.getBoundingClientRect();
        var c = window.getComputedStyle(el);
        return {
            x: r.left + (parseInt(c.borderLeftWidth) || 0) + (parseInt(c.paddingLeft) || 0),
            y: r.top + (parseInt(c.borderTopWidth) || 0) + (parseInt(c.paddingTop) || 0),
            h: lineHeightOf(c)
        };
    }

    // input/textarea のキャレット座標を mirror div で計測する。
    function textControlCaretRect(el) {
        var pos = el.selectionEnd || 0;
        var computed = window.getComputedStyle(el);
        var isInput = el.nodeName === "INPUT";

        var div = document.createElement("div");
        div.style.position = "absolute";
        div.style.visibility = "hidden";
        div.style.top = "-9999px";
        div.style.left = "0";
        for (var i = 0; i < MIRROR_PROPS.length; i++) {
            div.style[MIRROR_PROPS[i]] = computed[MIRROR_PROPS[i]];
        }
        div.style.whiteSpace = "pre-wrap";
        if (!isInput) div.style.wordWrap = "break-word";
        div.style.overflow = "hidden";

        var value = el.value || "";
        if (el.type === "password") value = "•".repeat(value.length);
        var before = value.substring(0, pos);
        if (isInput) before = before.replace(/\s/g, " ");
        div.textContent = before;

        var span = document.createElement("span");
        span.textContent = value.substring(pos) || ".";
        div.appendChild(span);
        document.body.appendChild(div);

        var elRect = el.getBoundingClientRect();
        var x = elRect.left + (parseInt(computed.borderLeftWidth) || 0) +
                span.offsetLeft - el.scrollLeft;
        var y = elRect.top + (parseInt(computed.borderTopWidth) || 0) +
                span.offsetTop - el.scrollTop;
        document.body.removeChild(div);

        return { x: x, y: y, h: lineHeightOf(computed) };
    }

    // contenteditable / designMode: collapsed selection の rect を使う。
    function selectionCaretRect(el) {
        var sel = window.getSelection();
        if (sel && sel.rangeCount > 0) {
            var range = sel.getRangeAt(0).cloneRange();
            range.collapse(false);
            var rect = range.getBoundingClientRect();
            if (rect && rect.height > 0) {
                return { x: rect.left, y: rect.top, h: rect.height };
            }
        }
        // 空の contenteditable では rect が全ゼロ → 要素矩形で近似
        return elementCaretFallback(el);
    }

    function reportCaret() {
        var el = document.activeElement;
        var r = null;
        try {
            if (isTextControl(el)) {
                r = textControlCaretRect(el);
            } else if (el && el.isContentEditable) {
                r = selectionCaretRect(el);
            } else if (el && (el.nodeName === "INPUT" || el.nodeName === "TEXTAREA")) {
                // selection API 非対応の input type (email/number など)
                r = elementCaretFallback(el);
            }
        } catch (e) {
            if (el && el.getBoundingClientRect) r = elementCaretFallback(el);
        }
        if (!r) return;
        console.log("__CARET__:" +
            Math.round(r.x) + ":" +
            Math.round(r.y) + ":0:" +
            Math.round(r.h));
    }

    document.addEventListener("selectionchange", reportCaret);
    document.addEventListener("click", function() {
        setTimeout(reportCaret, 0);
    });
    document.addEventListener("focusin", function() {
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

wrap_audio_handler! {
    struct ServerAudioHandler {
        audio_shm: Arc<AudioShmWriter>,
    }
    impl AudioHandler {
        /// CEF が要求する音声出力フォーマットを指定する。1 を返すと OSR の音声が
        /// このハンドラへルーティングされ、ブラウザプロセス側では再生されなくなる。
        /// channel_layout / sample_rate は CEF (ページ) の出力に合わせ、frames_per_buffer
        /// のみ指定する。
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&mut AudioParameters>,
        ) -> ::std::os::raw::c_int {
            if let Some(p) = params {
                p.channel_layout = ChannelLayout::LAYOUT_STEREO;
                p.sample_rate = 48_000;
                // 1 コールバックあたりのフレーム数。小さいほど低遅延だがコールバック頻度↑。
                // 512 = 10.7ms@48kHz (B 案 2026-07-13: 1024→512 でキャプチャ遅延を半減。
                // パケット量子も半減するので native 経路の target を 15→12ms に下げられる)。
                p.frames_per_buffer = 512;
            }
            1
        }

        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&AudioParameters>,
            channels: ::std::os::raw::c_int,
        ) {
            let sample_rate = params.map(|p| p.sample_rate).unwrap_or(48_000);
            let ch = channels.max(0) as u32;
            log(&format!(
                "on_audio_stream_started: sample_rate={} channels={}",
                sample_rate, ch
            ));
            self.audio_shm.start_stream(sample_rate as u32, ch);
        }

        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: ::std::os::raw::c_int,
            _pts: i64,
        ) {
            if data.is_null() || frames <= 0 {
                return;
            }
            // チャネル数は on_audio_stream_started でヘッダに記録済み。
            let channels = self.audio_shm.channels();
            if channels == 0 {
                return;
            }
            unsafe {
                self.audio_shm
                    .write_packet(data as *const *const f32, frames as usize, channels);
            }
        }

        fn on_audio_stream_stopped(&self, _browser: Option<&mut Browser>) {
            log("on_audio_stream_stopped");
            self.audio_shm.stop_stream();
        }

        fn on_audio_stream_error(&self, _browser: Option<&mut Browser>, message: Option<&CefString>) {
            let msg = message.map(|m| m.to_string()).unwrap_or_default();
            log(&format!("on_audio_stream_error: {}", msg));
            self.audio_shm.stop_stream();
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
        audio_handler: AudioHandler,
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
        fn audio_handler(&self) -> Option<AudioHandler> {
            Some(self.audio_handler.clone())
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

/// Server-side double-pump (flush) の保留状態。
/// クライアントから 1 回 SendExternalBeginFrame (BF#1) を受けると、サーバーは event loop
/// の 1ms tick 上で数 ms 後に追加の BeginFrame (flush) を内部発行する。これにより
/// 「BF#1 で renderer が生成 → flush で display が最新内容を draw」を **クライアントを
/// ブロックせずに** 完結させ、Unity 側はノンブロッキング受信だけで 0F を得る。
struct PendingFlush {
    browser_id: u32,
    unity_frame: u64,
    /// BF#1 を発行した時刻。flush のスケジュール基準。
    bf1: Instant,
    /// これまでに発行した flush 回数。
    flushes_done: u32,
    /// BF#1 発行時点の PAINT_COUNT。このフレーム内に届いた paint 数の基準。
    paints_at_bf1: u64,
}

/// flush を発行する経過時間しきい値 (ms)。renderer submit (実測 ~1.5-3ms) を跨ぐよう
/// +3ms と +6ms の 2 段で撃ち、遅い submit も取り逃さない。すべてサーバースレッド上で
/// 行われ Unity をブロックしない。
/// 2 段目は paint 到着済みチェック (process_pending_flushes) でスキップされ得る。
/// +6ms なのは flush#1 (+3ms) の draw → GPU → on_accelerated_paint 実行 (~2-3ms、
/// tick 内で process_pending_flushes より後の do_message_loop_work で走る) を跨いで
/// スキップ判定を効かせるため。
const FLUSH_THRESHOLDS_MS: [f64; 2] = [3.0, 6.0];

/// この回数以上「paint が発生したフレーム」が連続したら、ページは連続描画中
/// (スクロール/アニメーション) とみなして flush を抑止し BF#1 のみの 60Hz 駆動にする。
/// 小さすぎると離散入力の連打 (キーリピート等) で誤検出し 0F が失われ、大きすぎると
/// スクロール開始直後の飽和期間が延びる。3 = 50ms 連続で描画が続いたら抑止。
const DAMAGE_STREAK_SUPPRESS_FLUSH: u32 = 3;

/// 計測用トグル: `<temp_dir>/cef_no_server_flush` が在ると server-side flush を無効化
/// (BF#1 のみ = 1F baseline)。プロセス起動時に 1 回だけ判定 (server は Play ごとに再起動)。
fn server_flush_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| !std::env::temp_dir().join("cef_no_server_flush").exists())
}

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
    /// Server-side flush の保留状態 (現状は単一 Browser 構成を想定)。
    pending_flush: Option<PendingFlush>,
    /// 前回 BF#1 発行時点の PAINT_COUNT (フレーム間 paint 有無の判定基準)。
    paints_at_last_bf1: u64,
    /// 「paint が発生したフレーム」の連続数。スクロールや rAF アニメーション中は
    /// 毎フレーム damage が出るためこの値が伸び続ける。
    damage_streak: u32,
}

impl CefServer {
    pub fn new(client_pid: Option<u32>, use_gpu: bool) -> Self {
        CefServer {
            browsers: HashMap::new(),
            next_browser_id: AtomicU32::new(1),
            server_pid: std::process::id(),
            client_pid,
            use_gpu,
            pending_flush: None,
            paints_at_last_bf1: 0,
            damage_streak: 0,
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

        // macOS: exe からの相対パスで Framework を解決
        #[cfg(target_os = "macos")]
        {
            let frameworks_dir = exe_dir
                .parent().unwrap()   // Contents
                .join("Frameworks");
            let framework_dir = frameworks_dir.join("Chromium Embedded Framework.framework");
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
            Command::SendExternalBeginFrame {
                browser_id,
                unity_frame,
            } => self.send_external_begin_frame(browser_id, unity_frame),
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
        // viewport はクライアント申告値をそのまま使う。上限 clamp をここに入れると
        // GPU 経路 (IOSurface — サイズ上限なし) で Unity 側の想定サイズと乖離し、
        // テクスチャの引き伸ばし + マウス座標ズレになる (Retina の縦長 Game view は
        // 2160px を超える)。software shm の上限は on_paint 側でガードする。
        let width = width.max(1);
        let height = height.max(1);
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

        let audio_shm_flink = ipc::audio_shm_flink_path(self.server_pid, id);
        let audio_shm = match AudioShmWriter::new(&audio_shm_flink) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                return Response::Error {
                    msg: format!("audio_shm_create failed: {}", e),
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
        let audio_handler = ServerAudioHandler::new(Arc::clone(&audio_shm));
        let mut client = ServerClient::new(
            render_handler,
            life_span_handler,
            display_handler,
            load_handler,
            audio_handler,
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
        // External BeginFrame: Unity の LateUpdate から SendExternalBeginFrame で 1 フレーム
        // ずつ駆動する。これにより CEF の Viz Compositor は自発的に paint せず、
        // Unity のフレーム周期と完全に同期する (二重レート/位相ドリフトの解消)。
        // windowless_frame_rate はこのモードでは無視される。
        window_info.external_begin_frame_enabled = 1;
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
                audio_shm,
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
            audio_shm_flink,
        }
    }

    fn destroy_browser(&mut self, browser_id: u32) -> Response {
        if let Some(state) = self.browsers.remove(&browser_id) {
            if let Some(browser) = state.browser.lock().unwrap_or_else(PoisonError::into_inner).take()
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
        // create_browser と同じく上限 clamp はしない (GPU 経路のサイズ乖離防止)。
        let width = width.max(1);
        let height = height.max(1);
        if let Some(state) = self.browsers.get(&browser_id) {
            state.viewport_w.store(width, Ordering::Relaxed);
            state.viewport_h.store(height, Ordering::Relaxed);
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// 指定ブラウザに External BeginFrame を 1 回発行する低レベルヘルパ。
    /// 発行時刻 + unity_frame を記録 (on_accelerated_paint 側で読む)。
    /// host が取得できた (= 実際に発行した) 場合 true。
    fn issue_begin_frame(&self, browser_id: u32, unity_frame: u64) -> bool {
        if let Some(state) = self.browsers.get(&browser_id)
            && let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
            && let Some(host) = Browser::host(browser)
        {
            LAST_BEGIN_FRAME_NS.store(now_ns(), Ordering::Relaxed);
            LAST_BEGIN_FRAME_UNITY_FRAME.store(unity_frame, Ordering::Relaxed);
            BrowserHost::send_external_begin_frame(&host);
            return true;
        }
        false
    }

    /// External BeginFrame (BF#1) を発行し、server-side flush を予約する。
    /// クライアント (Unity) は 1 フレームに 1 回だけこれを呼べばよい。flush (BF#2..) は
    /// サーバーが event loop の tick 上で内部発行するため、クライアントは追加の BeginFrame を
    /// 撃つ必要がなく (IPC フラッディング無し)、PostLateUpdate でノンブロッキング受信するだけで
    /// 同フレーム内の最新内容 (0F) を得られる。
    /// `unity_frame` は発行時の Time.frameCount。
    fn send_external_begin_frame(&mut self, browser_id: u32, unity_frame: u64) -> Response {
        if !self.browsers.contains_key(&browser_id) {
            return Response::Error {
                msg: format!("browser {} not found", browser_id),
            };
        }
        // damage streak 判定: 前フレームに paint があったか (= ページが連続描画中か)。
        // スクロール/アニメーション中は毎フレーム damage が出るため streak が伸びる。
        // その間 flush (BF#2..) を撃つと draw/blit/送信が倍増して renderer/GPU が飽和し、
        // begin_frame_pending_ ガードの BF drop でコンテンツが欠落する (実測: Wikipedia
        // スクロールで 52fps + ジッタ、flush 無しなら完全な 60fps)。よって連続描画中は
        // BF#1 のみの 60Hz 駆動に切り替える (コンテンツは 1F 遅延になるが連続アニメでは
        // 知覚されず、滑らかさが優先される)。孤立した入力 (単発のキー/クリック) は直後の
        // フレームで streak が途切れて 0 に戻るため、従来通り flush による 0F 反映が
        // 維持される。毎フレーム paint を生む持続入力 (キーリピート等) は streak が伸びて
        // 抑止対象になるが、これも連続アニメと同様 1F 遅延は知覚されない。
        let paints_now = PAINT_COUNT.load(Ordering::Relaxed);
        let painted_last_frame = paints_now > self.paints_at_last_bf1;
        self.paints_at_last_bf1 = paints_now;
        self.damage_streak = if painted_last_frame {
            self.damage_streak.saturating_add(1)
        } else {
            0
        };

        if self.issue_begin_frame(browser_id, unity_frame)
            && server_flush_enabled()
            && self.damage_streak < DAMAGE_STREAK_SUPPRESS_FLUSH
        {
            // flush を予約 (tick で +3ms, +6ms に発行)。
            self.pending_flush = Some(PendingFlush {
                browser_id,
                unity_frame,
                bf1: Instant::now(),
                flushes_done: 0,
                paints_at_bf1: paints_now,
            });
        } else {
            self.pending_flush = None;
        }
        Response::Ok
    }

    /// event loop の tick (macOS は 1ms 間隔) から毎回呼ばれる。保留中の flush の発行時刻が
    /// 来ていれば BeginFrame (flush) を発行する。サーバースレッド上で動くため Unity を
    /// ブロックしない。BF#1 由来の renderer submit を跨ぐタイミングで撃つことで、display が
    /// 最新内容を draw → on_accelerated_paint が fresh #B を Mach 送信する。
    pub fn process_pending_flushes(&mut self) {
        let action = {
            let Some(pf) = self.pending_flush.as_mut() else {
                return;
            };
            let idx = pf.flushes_done as usize;
            if idx >= FLUSH_THRESHOLDS_MS.len() {
                None // すべて発行済み → クリア
            } else if PAINT_COUNT
                .load(Ordering::Relaxed)
                .wrapping_sub(pf.paints_at_bf1)
                >= 2
            {
                // BF#1 以降に既に 2 paint (#A stale + flush 由来 fresh #B) が届いている
                // = このフレームの最新内容は配信済み。以降の flush は冗長な draw を増やす
                // だけで、スクロール等 damage が毎 BF 発生する状況では renderer/GPU を
                // 飽和させて begin_frame_pending_ による BF drop → コンテンツ欠落を招く
                // (実測: Wikipedia スクロールで 60→52fps) ため撃たずに終了する。
                None
            } else {
                let elapsed_ms = pf.bf1.elapsed().as_secs_f64() * 1000.0;
                if elapsed_ms >= FLUSH_THRESHOLDS_MS[idx] {
                    pf.flushes_done += 1;
                    Some((pf.browser_id, pf.unity_frame, pf.flushes_done))
                } else {
                    return; // まだ発行時刻でない (保留継続)
                }
            }
        };
        match action {
            Some((bid, uf, done)) => {
                self.issue_begin_frame(bid, uf);
                if done as usize >= FLUSH_THRESHOLDS_MS.len() {
                    self.pending_flush = None;
                }
            }
            None => self.pending_flush = None,
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
