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
        source: *mut std::os::raw::c_void,
        width: u32,
        height: u32,
        format: u32,
    ) -> *mut std::os::raw::c_void;
    fn iosurface_pool_copy_no_wait_unsafe(
        source: *mut std::os::raw::c_void,
        width: u32,
        height: u32,
        format: u32,
    ) -> *mut std::os::raw::c_void;
    fn iosurface_pool_set_completion_callback(
        callback: extern "C" fn(*mut std::os::raw::c_void, u32, u32, u32),
    );
    fn iosurface_pool_copy_async(
        source: *mut std::os::raw::c_void,
        width: u32,
        height: u32,
        format: u32,
    ) -> i32;
    fn iosurface_pool_in_flight_copies() -> i32;
    fn iosurface_pool_poisoned_copies() -> u64;
    fn iosurface_pool_poison_copies_reading(source: *mut std::os::raw::c_void) -> i32;
}

use cef_unity_ipc::{self as ipc, AudioSharedMemoryWriter, Command, Response, SharedMemoryWriter};

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

pub(crate) fn log(message: &str) {
    if !LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let path = std::env::temp_dir().join("cef_unity_server.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "[{:?}] {}", std::time::SystemTime::now(), message);
    }

    let mut buffer = LOG_BUFFER.lock().unwrap_or_else(PoisonError::into_inner);
    if buffer.len() >= MAX_LOG_ENTRIES {
        buffer.remove(0);
    }
    buffer.push(message.to_string());
}

fn drain_logs() -> Vec<String> {
    std::mem::take(&mut *LOG_BUFFER.lock().unwrap_or_else(PoisonError::into_inner))
}

// ---------------------------------------------------------------------------
// CEF loader
// ---------------------------------------------------------------------------

/// macOS: current_exe() からの相対パスで CEF フレームワークを動的ロードする。
/// バンドル構造: Contents/MacOS/<executable> → Contents/Frameworks/Chromium Embedded Framework.framework/
#[cfg(target_os = "macos")]
fn load_cef_auto() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let executable = std::env::current_exe().expect("failed to get current_exe");
    let frameworks_dir = executable
        .parent().unwrap()   // MacOS
        .parent().unwrap()   // Contents
        .join("Frameworks");
    let framework_path = frameworks_dir.join(cef::sys::FRAMEWORK_PATH);
    let c_string = CString::new(framework_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        cef::load_library(Some(unsafe { &*c_string.as_ptr().cast() })),
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
    /// Kept alive so SharedMemoryWriter::drop cleans up shared memory on browser destroy.
    #[allow(dead_code)]
    shared_memory: Arc<SharedMemoryWriter>,
    /// 音声リングバッファ。AudioHandler が PCM を書き込む。ブラウザ破棄まで生かす。
    #[allow(dead_code)]
    audio_shared_memory: Arc<AudioSharedMemoryWriter>,
    browser: Arc<Mutex<Option<Browser>>>,
    viewport_width: Arc<AtomicI32>,
    viewport_height: Arc<AtomicI32>,
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

fn now_nanoseconds() -> u64 {
    epoch().elapsed().as_nanos() as u64
}

/// 最後に send_external_begin_frame を呼んだ時刻 (ns since epoch)。
/// 0 は未発行を意味する。
static LAST_BEGIN_FRAME_NANOSECONDS: AtomicU64 = AtomicU64::new(0);

/// 最後の SendExternalBeginFrame に載っていた Unity の Time.frameCount。
/// on_accelerated_paint で shared_memory に転送し、Unity 側で end-to-end の遅延フレーム数を測る。
static LAST_BEGIN_FRAME_UNITY_FRAME: AtomicU64 = AtomicU64::new(0);

/// 直近 N サンプルの BeginFrame → paint レイテンシ集計バッファ (μs 単位)。
const LATENCY_WINDOW: usize = 60;
static LATENCY_SAMPLES: Mutex<Vec<u64>> = Mutex::new(Vec::new());

// ---------------------------------------------------------------------------
// paint 統計 (GPU コピー完了待ち・Mach 送信待ちと pump 停止の相関を見る計装)
// ---------------------------------------------------------------------------
//
// on_accelerated_paint はメッセージ pump スレッド (CFRunLoop) 上で実行されるため、
// GPU コピー完了待ちが伸びると pump 自体が止まる。1 秒窓で「pump tick 数」と
// 「コピー待ち時間」を並べて出すことで、その因果を時系列で確認できるようにする。
// `--logging` 有効時のみ計測する (無効時は Instant::now() も呼ばない)。

static COPY_COUNT: AtomicU64 = AtomicU64::new(0);
/// 非同期モードで in-flight 追跡スロットが枯渇して捨てた paint 数。
static COPY_DROPPED_COUNT: AtomicU64 = AtomicU64::new(0);
/// GPU コピー未完了のため BeginFrame の発行を見送った回数 (issue #7 のゲート)。
static BEGIN_FRAME_DEFERRED_COUNT: AtomicU64 = AtomicU64::new(0);
static COPY_WAIT_TOTAL_MICROSECONDS: AtomicU64 = AtomicU64::new(0);
static COPY_WAIT_MAX_MICROSECONDS: AtomicU64 = AtomicU64::new(0);
static SEND_WAIT_TOTAL_MICROSECONDS: AtomicU64 = AtomicU64::new(0);
static SEND_WAIT_MAX_MICROSECONDS: AtomicU64 = AtomicU64::new(0);

// issue #14: dirty rect の面積が転送面積のどれだけかを測る (全面コピーの無駄の定量)。
static DAMAGE_AREA_TOTAL: AtomicU64 = AtomicU64::new(0);
static SURFACE_AREA_TOTAL: AtomicU64 = AtomicU64::new(0);

// issue #13: 1 tick で drain したコマンド数と、そのうち BeginFrame の数のバースト最大。
static DRAIN_MAX_BURST: AtomicU64 = AtomicU64::new(0);
static DRAIN_MAX_BEGIN_FRAME_BURST: AtomicU64 = AtomicU64::new(0);
static DRAIN_BEGIN_FRAME_TOTAL: AtomicU64 = AtomicU64::new(0);

/// event loop から 1 tick 分の drain 結果を記録する (issue #13 の計測)。
pub fn record_drain_burst(command_count: u64, begin_frame_count: u64) {
    if !paint_statistics_enabled() {
        return;
    }
    DRAIN_MAX_BURST.fetch_max(command_count, Ordering::Relaxed);
    DRAIN_MAX_BEGIN_FRAME_BURST.fetch_max(begin_frame_count, Ordering::Relaxed);
    DRAIN_BEGIN_FRAME_TOTAL.fetch_add(begin_frame_count, Ordering::Relaxed);
}

/// プロセスの CPU 時間 (user + sys) をミリ秒で返す。issue #12 の 1000Hz pump の
/// コストを測るために使う。
#[cfg(not(windows))]
fn process_cpu_milliseconds() -> u64 {
    #[repr(C)]
    struct TimeValue {
        seconds: i64,
        microseconds: i32,
    }
    #[repr(C)]
    struct ResourceUsage {
        user_time: TimeValue,
        system_time: TimeValue,
        rest: [u64; 30],
    }
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut ResourceUsage) -> i32;
    }
    let mut usage = ResourceUsage {
        user_time: TimeValue { seconds: 0, microseconds: 0 },
        system_time: TimeValue { seconds: 0, microseconds: 0 },
        rest: [0; 30],
    };
    if unsafe { getrusage(0, &mut usage) } != 0 {
        return 0;
    }
    let user = usage.user_time.seconds as u64 * 1000 + usage.user_time.microseconds as u64 / 1000;
    let system = usage.system_time.seconds as u64 * 1000 + usage.system_time.microseconds as u64 / 1000;
    user + system
}

/// Windows には `getrusage` が無いので、同じ値 (kernel + user) を `GetProcessTimes`
/// から取る。取得に失敗したら 0 を返す (統計ログの cpu= が 0 になるだけ)。
#[cfg(windows)]
fn process_cpu_milliseconds() -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let mut creation_time = FILETIME::default();
    let mut exit_time = FILETIME::default();
    let mut kernel_time = FILETIME::default();
    let mut user_time = FILETIME::default();
    let result = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation_time,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        )
    };
    if result.is_err() {
        return 0;
    }
    // FILETIME は 100 ナノ秒単位。
    let to_milliseconds = |time: FILETIME| {
        (((time.dwHighDateTime as u64) << 32) | (time.dwLowDateTime as u64)) / 10_000
    };
    to_milliseconds(kernel_time) + to_milliseconds(user_time)
}

/// 統計を有効化するか (= ログ有効か) を返す。
fn paint_statistics_enabled() -> bool {
    LOG_ENABLED.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// 非同期 GPU コピー (issue #7 の修正) — 既定で有効
// ---------------------------------------------------------------------------

/// 非同期コピーを使うか (既定 true)。`CEF_UNITY_SYNC_COPY=1` で従来の同期版に戻せる。
///
/// 同期版は `waitUntilCompleted` を CEF の message pump スレッド上で行うため、外部の
/// GPU/CPU 競合下で pump ごと数百 ms 止まる (実測 copy_wait_max 796ms、pump 1000→7
/// ticks/s)。入力・JS タイマー・IPC がまとめて凍結するのが issue #7 の症状。
#[cfg(target_os = "macos")]
pub fn use_async_copy() -> bool {
    static USE_ASYNC_COPY: OnceLock<bool> = OnceLock::new();
    *USE_ASYNC_COPY.get_or_init(|| {
        // 検証用の既知不良構成は同期経路側にあるので、指定されていたら非同期を降りる。
        !use_unsafe_no_wait_copy()
            && !std::env::var("CEF_UNITY_SYNC_COPY").is_ok_and(|value| value == "1")
    })
}

#[cfg(not(target_os = "macos"))]
pub fn use_async_copy() -> bool {
    false
}

/// 新しい BeginFrame を発行してよいか。
///
/// CEF は BeginFrame を受けて初めて次のフレームを描き、描き先は転送元プールの
/// IOSurface である。非同期コピー中はその IOSurface をまだ GPU が読んでいるため、
/// 読み終わるまで BeginFrame を発行しない。ここで「待つ」のではなく「発行しない」
/// ことが要点で、pump は回り続ける (= 入力・JS タイマー・IPC は止まらない)。
/// 供給が追いつかない分はフレームレートの低下として現れる。
///
/// 1 枚先行させる余地は無い: CEF の転送元プールは枚数固定ではなく (競合下で十数枚に
/// 増えるのを実測)、返却済みの surface を次フレームに再び選ぶ可能性を排除できない。
/// 直列でもフレーム供給レートは同期版と同じ (同期版も 1 コピー完了ごとに 1 フレーム)。
#[cfg(target_os = "macos")]
fn begin_frame_gate_open() -> bool {
    if !use_async_copy() || disable_begin_frame_gate() {
        return true;
    }
    unsafe { iosurface_pool_in_flight_copies() == 0 }
}

/// 検証用: BeginFrame ゲートを外す。転送元の契約 (コールバックから返るとプールへ戻る)
/// を破る既知不良構成で、poison 検出器が実際に反応することを確かめる positive control
/// 専用。実運用では使わない。
#[cfg(target_os = "macos")]
fn disable_begin_frame_gate() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        std::env::var("CEF_UNITY_NO_BEGIN_FRAME_GATE").is_ok_and(|value| value == "1")
    })
}

#[cfg(not(target_os = "macos"))]
fn begin_frame_gate_open() -> bool {
    true
}

/// 検証用の既知不良構成 (完了を待たずに送る) を使うか。ティアリング検出器の
/// negative control 専用で、実運用では使わない。
#[cfg(target_os = "macos")]
pub fn use_unsafe_no_wait_copy() -> bool {
    static USE_UNSAFE_NO_WAIT: OnceLock<bool> = OnceLock::new();
    *USE_UNSAFE_NO_WAIT.get_or_init(|| {
        std::env::var("CEF_UNITY_UNSAFE_NO_WAIT").is_ok_and(|value| value == "1")
    })
}

/// Linux で accelerated paint 経路を使えるかを判定する。
///
/// Chromium の GL 初期化は実ディスプレイを要求するため、ヘッドレス環境では GPU
/// プロセスが起動できない (`--ozone-platform=headless` で SIGSEGV する。詳細は
/// `docs/LINUX_GPU_FEASIBILITY.md`)。ディスプレイが無い環境では software paint に
/// 落とす。CI ランナーとコンテナはこちらに該当する。
///
/// `display` は `DISPLAY` 環境変数の値。Wayland のみで Xwayland が無い環境は
/// 未設定になるため、保守的に software paint となる。
#[cfg(target_os = "linux")]
fn linux_accelerated_paint_available(
    use_gpu: bool,
    display: Option<&str>,
    pool_available: bool,
) -> bool {
    use_gpu && display.is_some_and(|value| !value.is_empty()) && pool_available
}

/// 環境から `linux_accelerated_paint_available` を評価する。
/// ozone プラットフォームの選択と `shared_texture_enabled` の双方で同じ判定を使う。
/// DRM の fourcc。CEF が渡すピクセル順に合わせて選ぶ。
/// `'A','R','2','4'` — メモリ上は B,G,R,A の順。
#[cfg(target_os = "linux")]
const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241;
/// `'A','B','2','4'` — メモリ上は R,G,B,A の順。
#[cfg(target_os = "linux")]
const DRM_FORMAT_ABGR8888: u32 = 0x3432_4241;

/// 生の file descriptor を複製して所有権付きにする。
///
/// CEF が渡す fd はコールバックの間だけ借りているものなので、複製して寿命を分ける。
#[cfg(target_os = "linux")]
fn borrow_file_descriptor(raw: std::os::fd::RawFd) -> Option<std::os::fd::OwnedFd> {
    unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) }
        .try_clone_to_owned()
        .ok()
}

/// accelerated paint が成立しない理由を記録する。毎フレーム出すとログが溢れるので
/// 先頭数回だけにする。黙って捨てると原因が分からなくなるため、無言にはしない。
#[cfg(target_os = "linux")]
fn log_once_linux_accelerated_paint_problem(reason: &str) {
    static LOGGED_COUNT: AtomicU64 = AtomicU64::new(0);
    let count = LOGGED_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if count <= 5 {
        log(&format!(
            "on_accelerated_paint (Linux) をスキップ: {} — フレームは供給されない",
            reason
        ));
    }
}

// EGL コンテキストは作成したスレッドでしか current にできないため、プールは
// スレッドローカルに持つ。`on_accelerated_paint` と `create_browser` が同じ
// スレッドで動く限りプールは 1 つだけ作られる。違うスレッドで初期化されたら
// GPU リソースを二重に掴むことになるので、検出して警告する
// (`d3d11.rs` が呼び出しスレッド ID を記録しているのと同じ流儀)。
#[cfg(target_os = "linux")]
thread_local! {
    static DMABUF_POOL: std::cell::OnceCell<Option<crate::dmabuf_pool::DmabufPool>> =
        const { std::cell::OnceCell::new() };
}

#[cfg(target_os = "linux")]
static DMABUF_POOL_INITIALIZED_THREADS: AtomicU32 = AtomicU32::new(0);

/// 出力バッファの fd をクライアントへ渡すチャネル。
/// `pending_flush` と同じく現状は単一 Browser 構成を想定している。
#[cfg(target_os = "linux")]
static DMABUF_CHANNEL: Mutex<Option<cef_unity_ipc::file_descriptor_channel::FileDescriptorChannel>> =
    Mutex::new(None);

/// 直近にクライアントへ送った世代。0 は未送信。
#[cfg(target_os = "linux")]
static SENT_DMABUF_GENERATION: AtomicU32 = AtomicU32::new(0);

/// blit 済みの出力バッファをクライアントへ公開する。
///
/// 出力バッファが作り直された (= generation が進んだ) ときだけ fd を送る。
/// 定常状態では 1 本も送らない。書き込み順は「fd 送信 → 共有メモリヘッダ更新」で、
/// クライアントがヘッダの世代を見た時点では必ず対応する fd が届いている。
#[cfg(target_os = "linux")]
fn publish_linux_accelerated_frame(
    shared_memory: &SharedMemoryWriter,
    pool: &crate::dmabuf_pool::DmabufPool,
) {
    let Some((output_file_descriptor, descriptor)) = pool.output() else {
        log_once_linux_accelerated_paint_problem("出力記述子が取れない");
        return;
    };

    if SENT_DMABUF_GENERATION.load(Ordering::Relaxed) != descriptor.generation {
        let channel_guard = DMABUF_CHANNEL.lock().unwrap();
        let Some(channel) = channel_guard.as_ref() else {
            // クライアントがまだ接続していない。次フレームで再試行する。
            return;
        };
        if let Err(error) = channel.send(
            &descriptor.to_bytes(),
            &[std::os::fd::AsRawFd::as_raw_fd(&output_file_descriptor)],
        ) {
            log(&format!("dmabuf の fd を送れない: {}", error));
            return;
        }
        SENT_DMABUF_GENERATION.store(descriptor.generation, Ordering::Relaxed);
        log(&format!(
            "dmabuf を送信: generation={} {}x{} stride={} modifier={:#x}",
            descriptor.generation, descriptor.width, descriptor.height,
            descriptor.stride, descriptor.modifier
        ));
    }

    // d3d11 経路と同じく、frame_id 増分の前に Unity frame を書く。
    shared_memory.write_paint_unity_frame(LAST_BEGIN_FRAME_UNITY_FRAME.load(Ordering::Relaxed));
    shared_memory.write_dmabuf_info(
        descriptor.generation,
        descriptor.width,
        descriptor.height,
        // クライアントは dmabuf の fourcc で解釈するので、format タグは 0 固定にする。
        0,
    );
}

/// 待ち受けを開始し、クライアントの接続を別スレッドで受け付ける。
///
/// accept は接続が来るまで戻らないため、ブラウザ生成を止めないようスレッドに逃がす。
#[cfg(target_os = "linux")]
fn start_dmabuf_listener(socket_path: String) {
    std::thread::spawn(move || {
        let listener =
            match cef_unity_ipc::file_descriptor_channel::FileDescriptorChannel::listen(
                &socket_path,
            ) {
                Ok(listener) => listener,
                Err(error) => {
                    log(&format!("dmabuf ソケットを listen できない: {}", error));
                    return;
                }
            };
        match listener.accept() {
            Ok(channel) => {
                *DMABUF_CHANNEL.lock().unwrap() = Some(channel);
                log("dmabuf チャネルにクライアントが接続した");
            }
            Err(error) => log(&format!("dmabuf チャネルの accept に失敗: {}", error)),
        }
        // listener を保持し続ける (drop するとソケットファイルが消える)。
        std::thread::park();
    });
}

#[cfg(target_os = "linux")]
/// プールを、それを使う処理と同じスレッドで操作する。
/// スレッドローカルの参照は持ち出せないため、操作はクロージャで受け取る。
fn with_dmabuf_pool<T>(action: impl FnOnce(Option<&crate::dmabuf_pool::DmabufPool>) -> T) -> T {
    DMABUF_POOL.with(|cell| {
        let pool = cell.get_or_init(|| {
            let count = DMABUF_POOL_INITIALIZED_THREADS.fetch_add(1, Ordering::Relaxed) + 1;
            if count > 1 {
                log(&format!(
                    "警告: dmabuf プールが {} 本目のスレッドで初期化された。\
                     EGL コンテキストはスレッド束縛なので GPU リソースを二重に掴んでいる",
                    count
                ));
            }
            crate::dmabuf_pool::DmabufPool::create()
        });
        action(pool.as_ref())
    })
}

#[cfg(target_os = "linux")]
fn linux_use_accelerated_paint(use_gpu: bool) -> bool {
    let display = std::env::var("DISPLAY").ok();
    // プールの構築は EGL の初期化を伴うので、安い条件が揃ってから評価する。
    let cheap_conditions_hold =
        linux_accelerated_paint_available(use_gpu, display.as_deref(), true);
    cheap_conditions_hold && with_dmabuf_pool(|pool| pool.is_some())
}

/// 既知不良構成は IOSurface プール (macOS 専用) の中にしか無いので、他プラットフォーム
/// では常に false。統計ログの mode 表示は全プラットフォームでコンパイルされるため、
/// `use_async_copy` と同様にスタブが要る。
#[cfg(not(target_os = "macos"))]
pub fn use_unsafe_no_wait_copy() -> bool {
    false
}

/// 完了ハンドラ (Metal の直列送信キュー) から shm へ書くためのラッパ。
///
/// `SharedMemoryWriter` は `Send` だが `Sync` ではないため static に直接置けない。
/// ここで触るのは `write_paint_unity_frame` / `write_iosurface_info` の 2 つだけで、
/// いずれも header の atomic フィールドしか書かない。書き込み元は直列キューの 1 本に
/// 限られる (`iosurface_pool.m` の g_send_queue) ため、共有しても競合しない。
#[cfg(target_os = "macos")]
struct AsyncCopyTarget(Arc<SharedMemoryWriter>);

#[cfg(target_os = "macos")]
unsafe impl Send for AsyncCopyTarget {}
#[cfg(target_os = "macos")]
unsafe impl Sync for AsyncCopyTarget {}

/// 完了ハンドラから shm へ書くために、現在の browser の SharedMemoryWriter を保持する。
/// プール・Mach チャネルと同様にプロセス内で 1 つの accelerated paint 経路を前提とする。
#[cfg(target_os = "macos")]
static ASYNC_COPY_SHARED_MEMORY: Mutex<Option<AsyncCopyTarget>> = Mutex::new(None);

/// blit 完了後に Metal 側の直列キューから呼ばれる。この時点で surface は転送して安全。
#[cfg(target_os = "macos")]
extern "C" fn on_async_copy_completed(
    surface: *mut std::os::raw::c_void,
    width: u32,
    height: u32,
    format: u32,
) {
    if surface.is_null() {
        return;
    }
    // 保留中の client 購読を受け付ける (ノンブロッキング)。
    unsafe { mach_iosurface_server_accept() };

    let send_started_at = paint_statistics_enabled().then(Instant::now);
    let _send_result = unsafe { mach_iosurface_server_send(surface, width, height, format) };
    if let Some(started_at) = send_started_at {
        record_wait(
            &SEND_WAIT_TOTAL_MICROSECONDS,
            &SEND_WAIT_MAX_MICROSECONDS,
            started_at.elapsed().as_micros() as u64,
        );
    }

    let shared_memory = {
        let guard = ASYNC_COPY_SHARED_MEMORY
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        guard.as_ref().map(|target| Arc::clone(&target.0))
    };
    if let Some(shared_memory) = shared_memory {
        let surface_id = unsafe { IOSurfaceGetID(surface) };
        shared_memory
            .write_paint_unity_frame(LAST_BEGIN_FRAME_UNITY_FRAME.load(Ordering::Relaxed));
        shared_memory.write_iosurface_info(surface_id, width, height, format);
    }
}

/// on_accelerated_paint と event loop tick が同一スレッドかを 1 度だけ記録する
/// (同一なら、コピー完了待ちはそのまま pump の停止時間になる)。
static PAINT_THREAD_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn log_paint_thread_once() {
    if PAINT_THREAD_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    log(&format!(
        "on_accelerated_paint thread = {:?}",
        std::thread::current().id()
    ));
}

fn record_wait(total: &AtomicU64, maximum: &AtomicU64, elapsed_microseconds: u64) {
    total.fetch_add(elapsed_microseconds, Ordering::Relaxed);
    maximum.fetch_max(elapsed_microseconds, Ordering::Relaxed);
}

/// 統計窓の起点。emit は event loop の tick から行うため、pump が凍結している間は
/// 窓が 1 秒を超えて伸びる (窓長そのものが停止時間の証拠になる)。
struct PaintStatisticsWindow {
    started_at: Instant,
    pump_count_at_start: u64,
    paint_count_at_start: u64,
    cpu_milliseconds_at_start: u64,
}

static PAINT_STATISTICS_WINDOW: Mutex<Option<PaintStatisticsWindow>> = Mutex::new(None);

/// event loop の tick 毎に呼び、1 秒以上経過していれば STATISTICS 行を出す。
pub fn report_paint_statistics(pump_count: u64) {
    if !paint_statistics_enabled() {
        return;
    }
    let mut guard = PAINT_STATISTICS_WINDOW
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let window = guard.get_or_insert_with(|| PaintStatisticsWindow {
        started_at: Instant::now(),
        pump_count_at_start: pump_count,
        paint_count_at_start: PAINT_COUNT.load(Ordering::Relaxed),
        cpu_milliseconds_at_start: process_cpu_milliseconds(),
    });
    let elapsed = window.started_at.elapsed();
    if elapsed.as_millis() < 1000 {
        return;
    }

    let paint_count = PAINT_COUNT.load(Ordering::Relaxed);
    let pump_ticks = pump_count.saturating_sub(window.pump_count_at_start);
    let paints = paint_count.saturating_sub(window.paint_count_at_start);
    let copies = COPY_COUNT.swap(0, Ordering::Relaxed);
    let copies_dropped = COPY_DROPPED_COUNT.swap(0, Ordering::Relaxed);
    let copy_wait_total = COPY_WAIT_TOTAL_MICROSECONDS.swap(0, Ordering::Relaxed);
    let copy_wait_max = COPY_WAIT_MAX_MICROSECONDS.swap(0, Ordering::Relaxed);
    let send_wait_total = SEND_WAIT_TOTAL_MICROSECONDS.swap(0, Ordering::Relaxed);
    let send_wait_max = SEND_WAIT_MAX_MICROSECONDS.swap(0, Ordering::Relaxed);
    let cpu_milliseconds_now = process_cpu_milliseconds();
    let cpu_milliseconds = cpu_milliseconds_now.saturating_sub(window.cpu_milliseconds_at_start);
    let damage_area = DAMAGE_AREA_TOTAL.swap(0, Ordering::Relaxed);
    let surface_area = SURFACE_AREA_TOTAL.swap(0, Ordering::Relaxed);
    let damage_ratio = if surface_area > 0 {
        damage_area as f64 / surface_area as f64 * 100.0
    } else {
        0.0
    };
    let drain_max_burst = DRAIN_MAX_BURST.swap(0, Ordering::Relaxed);
    let drain_max_begin_frame_burst = DRAIN_MAX_BEGIN_FRAME_BURST.swap(0, Ordering::Relaxed);
    let drain_begin_frames = DRAIN_BEGIN_FRAME_TOTAL.swap(0, Ordering::Relaxed);
    let begin_frames_deferred = BEGIN_FRAME_DEFERRED_COUNT.swap(0, Ordering::Relaxed);
    // 転送元の契約違反で捨てたフレームの累計 (ゲートが効いていれば 0 のまま)。
    #[cfg(target_os = "macos")]
    let poisoned_copies = unsafe { iosurface_pool_poisoned_copies() };
    #[cfg(not(target_os = "macos"))]
    let poisoned_copies = 0u64;
    *window = PaintStatisticsWindow {
        started_at: Instant::now(),
        pump_count_at_start: pump_count,
        paint_count_at_start: paint_count,
        cpu_milliseconds_at_start: cpu_milliseconds_now,
    };
    drop(guard);

    log(&format!(
        "STATISTICS tick_thread={:?} mode={} window={}ms pump_ticks={} paints={} copies={} dropped={} \
         copy_wait_total={}.{:03}ms copy_wait_max={}.{:03}ms \
         send_wait_total={}.{:03}ms send_wait_max={}.{:03}ms \
         cpu={}ms damage_ratio={:.1}% \
         drain_max={} begin_frame_drained={} begin_frame_burst_max={} \
         begin_frame_deferred={} poisoned_total={}",
        std::thread::current().id(),
        if use_async_copy() {
            "async"
        } else if use_unsafe_no_wait_copy() {
            "unsafe-no-wait"
        } else {
            "sync"
        },
        elapsed.as_millis(),
        pump_ticks,
        paints,
        copies,
        copies_dropped,
        copy_wait_total / 1000, copy_wait_total % 1000,
        copy_wait_max / 1000, copy_wait_max % 1000,
        send_wait_total / 1000, send_wait_total % 1000,
        send_wait_max / 1000, send_wait_max % 1000,
        cpu_milliseconds,
        damage_ratio,
        drain_max_burst,
        drain_begin_frames,
        drain_max_begin_frame_burst,
        begin_frames_deferred,
        poisoned_copies,
    ));
}

/// paint 到着時にレイテンシを記録し、N サンプル貯まったら統計をログに出す。
fn record_paint_latency() {
    // on_accelerated_paint の hot path から毎フレーム呼ばれるため、
    // ログ無効時は lock/push/sort を一切行わず素通しする。
    if !LOG_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let begin_nanoseconds = LAST_BEGIN_FRAME_NANOSECONDS.load(Ordering::Relaxed);
    if begin_nanoseconds == 0 {
        return; // BeginFrame 未発行 (初期化中の自発フレーム等)
    }
    let now = now_nanoseconds();
    if now <= begin_nanoseconds {
        return;
    }
    let elapsed_microseconds = (now - begin_nanoseconds) / 1000;

    let mut samples = LATENCY_SAMPLES.lock().unwrap_or_else(PoisonError::into_inner);
    samples.push(elapsed_microseconds);
    if samples.len() >= LATENCY_WINDOW {
        let count = samples.len() as u64;
        let sum: u64 = samples.iter().sum();
        let average = sum / count;
        let min = *samples.iter().min().unwrap();
        let max = *samples.iter().max().unwrap();
        // 中央値も出す (外れ値の影響を見るため)
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        samples.clear();
        drop(samples);
        log(&format!(
            "BeginFrame→paint latency (n={}): average={}.{:03}ms median={}.{:03}ms min={}.{:03}ms max={}.{:03}ms",
            count,
            average / 1000, average % 1000,
            median / 1000, median % 1000,
            min / 1000, min % 1000,
            max / 1000, max % 1000,
        ));
    }
}

wrap_render_handler! {
    struct ServerRenderHandler {
        shared_memory: Arc<SharedMemoryWriter>,
        viewport_width: Arc<AtomicI32>,
        viewport_height: Arc<AtomicI32>,
        d3d11_pool: Option<Arc<D3D11Pool>>,
    }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            let width = self.viewport_width.load(Ordering::Relaxed);
            let height = self.viewport_height.load(Ordering::Relaxed);
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = width;
                rect.height = height;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            let width = self.viewport_width.load(Ordering::Relaxed);
            let height = self.viewport_height.load(Ordering::Relaxed);
            if let Some(screen_info) = screen_info {
                screen_info.size = std::mem::size_of::<ScreenInfo>();
                screen_info.device_scale_factor = 1.0;
                screen_info.depth = 32;
                screen_info.depth_per_component = 8;
                screen_info.is_monochrome = 0;
                screen_info.rect = Rect { x: 0, y: 0, width: width, height: height };
                screen_info.available_rect = Rect { x: 0, y: 0, width: width, height: height };
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
            // software 経路の shared_memory バッファは MAX_WIDTH×MAX_HEIGHT 固定長。超過フレームを
            // write_frame に渡すと assert panic → CEF コールバック越しの unwind で
            // プロセス abort するため、ここで読み捨てる (制約は software 経路にのみ
            // 実在する。viewport 側で clamp すると GPU 経路まで Unity の想定サイズと
            // 乖離して縦伸び/マウス座標ズレになる — 2026-07-23 のリグレッションで実証)。
            if width == 0 || height == 0 || width > ipc::MAX_WIDTH || height > ipc::MAX_HEIGHT {
                if count <= 3 || count.is_multiple_of(100) {
                    log(&format!(
                        "on_paint: {}x{} exceeds software shm buffer ({}x{}) — frame skipped",
                        width, height, ipc::MAX_WIDTH, ipc::MAX_HEIGHT
                    ));
                }
                return;
            }
            let size = (width as usize) * (height as usize) * 4;
            let source = unsafe { std::slice::from_raw_parts(buffer, size) };
            self.shared_memory.write_frame(source, width, height);
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
            // Linux: CEF の dmabuf を自前の出力バッファへ blit する。CEF のバッファは
            // プールの借用で、このコールバックを抜けると再利用されるため、ここで
            // コピーを完了させる (macOS / Windows と同じ構造)。
            #[cfg(target_os = "linux")]
            {
                let Some(info) = info else {
                    log_once_linux_accelerated_paint_problem("info=None");
                    return;
                };
                if info.plane_count != 1 {
                    // 複数プレーンは想定していない (CEF は RGBA 1 プレーンを渡す)。
                    log_once_linux_accelerated_paint_problem("plane_count が 1 ではない");
                    return;
                }

                let width = info.extra.coded_size.width as u32;
                let height = info.extra.coded_size.height as u32;
                let source = crate::dmabuf_pool::DmabufSource {
                    // CEF が所有する fd を借りるだけなので、複製して所有権を分ける。
                    file_descriptor: match borrow_file_descriptor(info.planes[0].fd) {
                        Some(file_descriptor) => file_descriptor,
                        None => {
                            log_once_linux_accelerated_paint_problem("fd を複製できない");
                            return;
                        }
                    },
                    stride: info.planes[0].stride,
                    modifier: info.modifier,
                    fourcc: if info.format.get_raw() == ColorType::RGBA_8888.get_raw() {
                        DRM_FORMAT_ABGR8888
                    } else {
                        DRM_FORMAT_ARGB8888
                    },
                    width,
                    height,
                };

                with_dmabuf_pool(|pool| {
                    let Some(pool) = pool else {
                        log_once_linux_accelerated_paint_problem("プールが無い");
                        return;
                    };
                    if !pool.blit(&source) {
                        log_once_linux_accelerated_paint_problem("blit に失敗");
                        return;
                    }
                    record_paint_latency();
                    PAINT_COUNT.fetch_add(1, Ordering::Relaxed);
                    publish_linux_accelerated_frame(&self.shared_memory, pool);
                });
                return;
            }
            #[cfg(target_os = "macos")]
            if let Some(info) = info {
                let io_surface = info.shared_texture_io_surface;
                if io_surface.is_null() {
                    return;
                }
                // issue #14: 実際に damage している面積を記録する (コピーは常に全面)。
                if paint_statistics_enabled() {
                    let damage: u64 = _dirty_rects
                        .map(|rects| {
                            rects
                                .iter()
                                .map(|rect| (rect.width as u64) * (rect.height as u64))
                                .sum()
                        })
                        .unwrap_or(0);
                    let surface = (info.extra.coded_size.width as u64)
                        * (info.extra.coded_size.height as u64);
                    DAMAGE_AREA_TOTAL.fetch_add(damage, Ordering::Relaxed);
                    SURFACE_AREA_TOTAL.fetch_add(surface, Ordering::Relaxed);
                }
                let width = info.extra.coded_size.width as u32;
                let height = info.extra.coded_size.height as u32;
                let format = if info.format.get_raw() == ColorType::RGBA_8888.get_raw() {
                    1u32
                } else {
                    0u32
                };

                // BeginFrame → paint レイテンシを記録 (外的 BeginFrame モードでのみ意味あり)
                record_paint_latency();

                let count = PAINT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

                // GPU blit: CEF IOSurface → pool IOSurface (must complete before returning)
                // 完了待ちは pump スレッドを塞ぐため、所要時間を統計に記録する。
                let copy_started_at = paint_statistics_enabled().then(Instant::now);
                if copy_started_at.is_some() {
                    log_paint_thread_once();
                }

                // 非同期モード: blit を投げるだけで返り、送信と shm 書き込みは完了ハンドラで行う。
                if use_async_copy() {
                    // CEF はコールバックから返った時点で転送元をプールへ戻す (ヘッダの契約)。
                    // つまり今回交付された転送元は、前回それを読んだ blit の途中で
                    // 上書きされている可能性がある。まだ読んでいる blit があればその結果を
                    // 捨てる。BeginFrame ゲートが効いていればここは常に 0 件になる。
                    let poisoned =
                        unsafe { iosurface_pool_poison_copies_reading(io_surface) };
                    if poisoned > 0 {
                        log(&format!(
                            "on_accelerated_paint #{}: CEF が読み取り中の転送元を再交付した \
                             (poisoned={}) — BeginFrame ゲートが機能していない",
                            count, poisoned
                        ));
                    }
                    {
                        let mut guard = ASYNC_COPY_SHARED_MEMORY
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                        // 同期版は self.shared_memory を直接使うので browser ごとに正しい。
                        // 非同期版は完了ハンドラから触るため global に置くが、別 browser の
                        // paint が来たら差し替える (置きっぱなしだと最初の browser の shm に
                        // 書き続けてしまう)。
                        let matches_current = guard
                            .as_ref()
                            .is_some_and(|target| Arc::ptr_eq(&target.0, &self.shared_memory));
                        if !matches_current {
                            if guard.is_none() {
                                unsafe {
                                    iosurface_pool_set_completion_callback(on_async_copy_completed)
                                };
                            }
                            *guard = Some(AsyncCopyTarget(Arc::clone(&self.shared_memory)));
                        }
                    }
                    let submit_result =
                        unsafe { iosurface_pool_copy_async(io_surface, width, height, format) };
                    if let Some(started_at) = copy_started_at {
                        COPY_COUNT.fetch_add(1, Ordering::Relaxed);
                        record_wait(
                            &COPY_WAIT_TOTAL_MICROSECONDS,
                            &COPY_WAIT_MAX_MICROSECONDS,
                            started_at.elapsed().as_micros() as u64,
                        );
                        if submit_result == 0 {
                            COPY_DROPPED_COUNT.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    if submit_result < 0 && count <= 5 {
                        log("on_accelerated_paint: async pool copy failed");
                    }
                    return;
                }

                let pool_surface = if use_unsafe_no_wait_copy() {
                    unsafe { iosurface_pool_copy_no_wait_unsafe(io_surface, width, height, format) }
                } else {
                    unsafe { iosurface_pool_copy_and_get(io_surface, width, height, format) }
                };
                if let Some(started_at) = copy_started_at {
                    COPY_COUNT.fetch_add(1, Ordering::Relaxed);
                    record_wait(
                        &COPY_WAIT_TOTAL_MICROSECONDS,
                        &COPY_WAIT_MAX_MICROSECONDS,
                        started_at.elapsed().as_micros() as u64,
                    );
                }
                if pool_surface.is_null() {
                    if count <= 5 {
                        log("on_accelerated_paint: pool copy failed");
                    }
                    return;
                }

                // Accept pending client subscription (non-blocking)
                unsafe { mach_iosurface_server_accept(); }

                // Send the copied pool IOSurface via Mach port to connected client
                // mach_msg は 10ms timeout の blocking send なので、これも pump を止め得る。
                let send_started_at = paint_statistics_enabled().then(Instant::now);
                let send_result = unsafe {
                    mach_iosurface_server_send(pool_surface, width, height, format)
                };
                if let Some(started_at) = send_started_at {
                    record_wait(
                        &SEND_WAIT_TOTAL_MICROSECONDS,
                        &SEND_WAIT_MAX_MICROSECONDS,
                        started_at.elapsed().as_micros() as u64,
                    );
                }

                // 生存確認用ログ: 最初の 5 件 + 600 件ごと (≒10秒 @ 60fps)
                if count <= 5 || count.is_multiple_of(600) {
                    let source_id = unsafe { IOSurfaceGetID(io_surface) };
                    let destination_id = unsafe { IOSurfaceGetID(pool_surface) };
                    let has_client = unsafe { mach_iosurface_server_has_client() };
                    log(&format!(
                        "on_accelerated_paint #{}: {}x{} source_id={} pool_id={} mach_send={} client={}",
                        count, width, height, source_id, destination_id, send_result, has_client
                    ));
                }

                // Also write metadata to SharedMemoryHeader (for frame change detection)
                let surface_id = unsafe { IOSurfaceGetID(pool_surface) };
                // accel_frame_id 増分の前に Unity frame を書く: クライアントは frame_id 変化
                // を検出してから他フィールドを読むため、これより前に書いてあれば read 時に
                // 必ず観測される。
                self.shared_memory
                    .write_paint_unity_frame(LAST_BEGIN_FRAME_UNITY_FRAME.load(Ordering::Relaxed));
                self.shared_memory.write_iosurface_info(surface_id, width, height, format);
            }

            #[cfg(target_os = "windows")]
            if let Some(info) = info {
                let Some(pool) = self.d3d11_pool.as_ref() else { return; };

                let source_handle_raw = info.shared_texture_handle;
                if source_handle_raw.is_null() {
                    return;
                }
                let width = info.extra.coded_size.width as u32;
                let height = info.extra.coded_size.height as u32;
                if width == 0 || height == 0 { return; }

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
                let source_handle = WinHandle(source_handle_raw as *mut _);

                // BeginFrame → paint レイテンシを記録 (外的 BeginFrame モードでのみ意味あり)
                record_paint_latency();

                match pool.copy_from_source(source_handle, width, height, dxgi_format) {
                    Ok((client_handle, fence_value)) => {
                        if count <= 5 || count.is_multiple_of(600) {
                            log(&format!(
                                "on_accelerated_paint #{}: {}x{} fmt={} client_handle=0x{:x} fence={}",
                                count, width, height, format_tag, client_handle, fence_value
                            ));
                        }
                        // d3d11_frame_id 増分の前に Unity frame を書く。
                        self.shared_memory.write_paint_unity_frame(
                            LAST_BEGIN_FRAME_UNITY_FRAME.load(Ordering::Relaxed),
                        );
                        self.shared_memory
                            .write_d3d11_handle(client_handle, width, height, format_tag, fence_value);
                    }
                    Err(error) => {
                        if count <= 5 {
                            log(&format!("on_accelerated_paint pool error: {}", error));
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
                    self.shared_memory.write_ime_caret(last.x + last.width, last.y, last.width, last.height);
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
            let url_string = target_url.map(|url| url.to_string()).unwrap_or_default();
            log(&format!("on_before_popup: url={}", url_string));
            if let (Some(browser), Some(url)) = (browser, target_url)
                && let Some(frame) = Browser::main_frame(browser) {
                    Frame::load_url(&frame, Some(url));
                }
            1 // キャンセル
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            log("on_after_created called");
            if let Some(browser) = browser {
                *self.browser_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(browser.clone());
                log("browser stored in slot");
            }
        }
    }
}

wrap_display_handler! {
    struct ServerDisplayHandler {
        shared_memory: Arc<SharedMemoryWriter>,
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
            if let Some(message) = message {
                let text = message.to_string();
                if let Some(rest) = text.strip_prefix("__CARET__:") {
                    let parts: Vec<&str> = rest.split(':').collect();
                    if parts.len() == 4
                        && let (Ok(x), Ok(y), Ok(width), Ok(height)) = (
                            parts[0].parse::<i32>(),
                            parts[1].parse::<i32>(),
                            parts[2].parse::<i32>(),
                            parts[3].parse::<i32>(),
                        ) {
                            self.shared_memory.write_ime_caret(x, y, width, height);
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
            if let Some(frame) = frame
                && frame.is_main() != 0 {
                    Frame::execute_java_script(
                        frame,
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

    var MIRROR_PROPERTIES = [
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
        var lineHeight = parseFloat(computed.lineHeight);
        if (!lineHeight) lineHeight = (parseFloat(computed.fontSize) || 16) * 1.2;
        return lineHeight;
    }

    // selection API をサポートする input type のみ true (email/number は throw する)
    function isTextControl(element) {
        if (!element || !element.nodeName) return false;
        if (element.nodeName === "TEXTAREA") return true;
        if (element.nodeName !== "INPUT") return false;
        var typeName = (element.type || "text").toLowerCase();
        return typeName === "text" || typeName === "search" || typeName === "tel" ||
               typeName === "url" || typeName === "password";
    }

    // フィールド先頭 (padding 内側) の座標。キャレット位置が計算できない場合の近似。
    function elementCaretFallback(element) {
        var rect = element.getBoundingClientRect();
        var computed = window.getComputedStyle(element);
        return {
            x: rect.left + (parseInt(computed.borderLeftWidth) || 0) + (parseInt(computed.paddingLeft) || 0),
            y: rect.top + (parseInt(computed.borderTopWidth) || 0) + (parseInt(computed.paddingTop) || 0),
            height: lineHeightOf(computed)
        };
    }

    // input/textarea のキャレット座標を mirror div で計測する。
    function textControlCaretRect(element) {
        var position = element.selectionEnd || 0;
        var computed = window.getComputedStyle(element);
        var isInput = element.nodeName === "INPUT";

        var div = document.createElement("div");
        div.style.position = "absolute";
        div.style.visibility = "hidden";
        div.style.top = "-9999px";
        div.style.left = "0";
        for (var index = 0; index < MIRROR_PROPERTIES.length; index++) {
            div.style[MIRROR_PROPERTIES[index]] = computed[MIRROR_PROPERTIES[index]];
        }
        div.style.whiteSpace = "pre-wrap";
        if (!isInput) div.style.wordWrap = "break-word";
        div.style.overflow = "hidden";

        var value = element.value || "";
        if (element.type === "password") value = "•".repeat(value.length);
        var before = value.substring(0, position);
        if (isInput) before = before.replace(/\s/g, " ");
        div.textContent = before;

        var span = document.createElement("span");
        span.textContent = value.substring(position) || ".";
        div.appendChild(span);
        document.body.appendChild(div);

        var elementRect = element.getBoundingClientRect();
        var x = elementRect.left + (parseInt(computed.borderLeftWidth) || 0) +
                span.offsetLeft - element.scrollLeft;
        var y = elementRect.top + (parseInt(computed.borderTopWidth) || 0) +
                span.offsetTop - element.scrollTop;
        document.body.removeChild(div);

        return { x: x, y: y, height: lineHeightOf(computed) };
    }

    // contenteditable / designMode: collapsed selection の rect を使う。
    function selectionCaretRect(element) {
        var selection = window.getSelection();
        if (selection && selection.rangeCount > 0) {
            var range = selection.getRangeAt(0).cloneRange();
            range.collapse(false);
            var rect = range.getBoundingClientRect();
            if (rect && rect.height > 0) {
                return { x: rect.left, y: rect.top, height: rect.height };
            }
        }
        // 空の contenteditable では rect が全ゼロ → 要素矩形で近似
        return elementCaretFallback(element);
    }

    function reportCaret() {
        var element = document.activeElement;
        var rect = null;
        try {
            if (isTextControl(element)) {
                rect = textControlCaretRect(element);
            } else if (element && element.isContentEditable) {
                rect = selectionCaretRect(element);
            } else if (element && (element.nodeName === "INPUT" || element.nodeName === "TEXTAREA")) {
                // selection API 非対応の input type (email/number など)
                rect = elementCaretFallback(element);
            }
        } catch (error) {
            if (element && element.getBoundingClientRect) rect = elementCaretFallback(element);
        }
        if (!rect) return;
        console.log("__CARET__:" +
            Math.round(rect.x) + ":" +
            Math.round(rect.y) + ":0:" +
            Math.round(rect.height));
    }

    document.addEventListener("selectionchange", reportCaret);
    document.addEventListener("click", function() {
        setTimeout(reportCaret, 0);
    });
    document.addEventListener("focusin", function() {
        setTimeout(reportCaret, 0);
    });
    document.addEventListener("keyup", function(event) {
        if (["ArrowLeft","ArrowRight","ArrowUp","ArrowDown","Home","End"].includes(event.key)) {
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
        audio_shared_memory: Arc<AudioSharedMemoryWriter>,
    }
    impl AudioHandler {
        /// CEF が要求する音声出力フォーマットを指定する。1 を返すと OSR の音声が
        /// このハンドラへルーティングされ、ブラウザプロセス側では再生されなくなる。
        /// channel_layout / sample_rate は CEF (ページ) の出力に合わせ、frames_per_buffer
        /// のみ指定する。
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            parameters: Option<&mut AudioParameters>,
        ) -> ::std::os::raw::c_int {
            if let Some(parameters) = parameters {
                parameters.channel_layout = ChannelLayout::LAYOUT_STEREO;
                parameters.sample_rate = 48_000;
                // 1 コールバックあたりのフレーム数。小さいほど低遅延だがコールバック頻度↑。
                // 512 = 10.7ms@48kHz (B 案 2026-07-13: 1024→512 でキャプチャ遅延を半減。
                // パケット量子も半減するので native 経路の target を 15→12ms に下げられる)。
                parameters.frames_per_buffer = 512;
            }
            1
        }

        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            parameters: Option<&AudioParameters>,
            channels: ::std::os::raw::c_int,
        ) {
            let sample_rate = parameters.map(|parameters| parameters.sample_rate).unwrap_or(48_000);
            let channel_count = channels.max(0) as u32;
            log(&format!(
                "on_audio_stream_started: sample_rate={} channels={}",
                sample_rate, channel_count
            ));
            self.audio_shared_memory.start_stream(sample_rate as u32, channel_count);
        }

        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: ::std::os::raw::c_int,
            _presentation_timestamp: i64,
        ) {
            if data.is_null() || frames <= 0 {
                return;
            }
            // チャネル数は on_audio_stream_started でヘッダに記録済み。
            let channels = self.audio_shared_memory.channels();
            if channels == 0 {
                return;
            }
            unsafe {
                self.audio_shared_memory
                    .write_packet(data as *const *const f32, frames as usize, channels);
            }
        }

        fn on_audio_stream_stopped(&self, _browser: Option<&mut Browser>) {
            log("on_audio_stream_stopped");
            self.audio_shared_memory.stop_stream();
        }

        fn on_audio_stream_error(&self, _browser: Option<&mut Browser>, message: Option<&CefString>) {
            let message = message.map(|text| text.to_string()).unwrap_or_default();
            log(&format!("on_audio_stream_error: {}", message));
            self.audio_shared_memory.stop_stream();
        }
    }
}

wrap_browser_process_handler! {
    struct ServerBrowserProcessHandler;
    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_milliseconds: i64) {
            crate::event_loop::schedule_pump(delay_milliseconds);
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
            if let Some(command_line) = command_line {
                command_line.append_switch(Some(&CefString::from("use-mock-keychain")));
                command_line.append_switch_with_value(
                    Some(&CefString::from("autoplay-policy")),
                    Some(&CefString::from("no-user-gesture-required")),
                );
                // GPU サンドボックスを無効化 (shared_texture_enabled で GPU プロセスが正常に
                // 動作するために必要。Unity プラグイン環境では CEF レベルのサンドボックスも
                // 無効 (no_sandbox=1) なので、GPU サンドボックスも不要)
                command_line.append_switch(Some(&CefString::from("disable-gpu-sandbox")));

                // Linux: 既定は headless バックエンド。OSR にはウィンドウが無く画面を
                // 要求する理由がないうえ、X11 が無い環境 (CI ランナー、コンテナ、
                // サーバー) で初期化が失敗するのを防げる。
                //
                // ただし accelerated paint (dmabuf) 経路だけは実ディスプレイを要求する
                // ため x11 を選ぶ。headless では GPU プロセスが起動できない。
                // 詳細と実測データは docs/LINUX_GPU_FEASIBILITY.md を参照。
                #[cfg(target_os = "linux")]
                {
                    let accelerated = linux_use_accelerated_paint(self.use_gpu);
                    let ozone_platform = if accelerated { "x11" } else { "headless" };
                    log(&format!(
                        "ozone-platform = {} (accelerated paint = {})",
                        ozone_platform, accelerated
                    ));
                    command_line.append_switch_with_value(
                        Some(&CefString::from("ozone-platform")),
                        Some(&CefString::from(ozone_platform)),
                    );
                    if accelerated {
                        // Skia を Vulkan バックエンドにする。ANGLE の GL 経由で dmabuf を
                        // インポートするとテクスチャがレンダリング可能にならず
                        // (GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT)、SkSurface を作れずに
                        // paint がまったく発生しない。
                        command_line.append_switch_with_value(
                            Some(&CefString::from("enable-features")),
                            Some(&CefString::from("Vulkan")),
                        );
                    }
                }

                if !self.use_gpu {
                    // CPU モード: Chromium に GPU を一切使わせない。
                    // これにより on_paint 用の GPU→CPU readback が発生しなくなり、
                    // Skia software pipeline のみで動く。
                    command_line.append_switch(Some(&CefString::from("disable-gpu")));
                    command_line.append_switch(Some(&CefString::from("disable-gpu-compositing")));
                    // Skia software raster の並列度を上げる (デフォルト 1-2 → 4)。
                    // 4K で全画面ダーティなスクロール時に効く。
                    command_line.append_switch_with_value(
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
    begin_frame_1: Instant,
    /// これまでに発行した flush 回数。
    flushes_done: u32,
    /// BF#1 発行時点の PAINT_COUNT。このフレーム内に届いた paint 数の基準。
    paints_at_begin_frame_1: u64,
}

/// flush を発行する経過時間しきい値 (ms)。renderer submit (実測 ~1.5-3ms) を跨ぐよう
/// +3ms と +6ms の 2 段で撃ち、遅い submit も取り逃さない。すべてサーバースレッド上で
/// 行われ Unity をブロックしない。
/// 2 段目は paint 到着済みチェック (process_pending_flushes) でスキップされ得る。
/// +6ms なのは flush#1 (+3ms) の draw → GPU → on_accelerated_paint 実行 (~2-3ms、
/// tick 内で process_pending_flushes より後の do_message_loop_work で走る) を跨いで
/// スキップ判定を効かせるため。
const FLUSH_THRESHOLDS_MILLISECONDS: [f64; 2] = [3.0, 6.0];

/// この回数以上「paint が発生したフレーム」が連続したら、ページは連続描画中
/// (スクロール/アニメーション) とみなして flush を抑止し BF#1 のみの 60Hz 駆動にする。
/// 小さすぎると離散入力の連打 (キーリピート等) で誤検出し 0F が失われ、大きすぎると
/// スクロール開始直後の飽和期間が延びる。3 = 50ms 連続で描画が続いたら抑止。
const DAMAGE_STREAK_SUPPRESS_FLUSH: u32 = 3;

/// 計測用トグル: `<temp_dir>/cef_no_server_flush` が在ると server-side flush を無効化
/// (BF#1 のみ = 1F baseline)。プロセス起動時に 1 回だけ判定 (server は Play ごとに再起動)。
fn server_flush_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| !std::env::temp_dir().join("cef_no_server_flush").exists())
}

/// 計測用トグル: `<temp_dir>/cef_no_streak_cooldown` が在ると抑止トライアルの
/// クールダウン (SUPPRESSION_RETRY_FRAMES) を無効化し旧挙動 (常時トライアル =
/// 低速ドラッグで 4 フレーム周期の発振) に戻す。A/B 比較用。
fn streak_cooldown_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| !std::env::temp_dir().join("cef_no_streak_cooldown").exists())
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
    paints_at_last_begin_frame_1: u64,
    /// 「paint が発生したフレーム」の連続数。スクロールや rAF アニメーション中は
    /// 毎フレーム damage が出るためこの値が伸び続ける。
    damage_streak: u32,
    /// 前回の BF#1 が「streak 抑止で flush なし」だったか (抑止トライアルの成否判定用)。
    last_begin_frame_1_suppressed: bool,
    /// 抑止トライアル失敗後のクールダウン残フレーム数。0 なら抑止を試してよい。
    suppression_cooldown: u32,
    /// GPU コピー未完了で発行を見送った BF#1。tick でゲートが開き次第発行する。
    /// (browser_id, unity_frame)。新しい BF#1 が来たら上書きする — 溜めても意味がない。
    deferred_begin_frame: Option<(u32, u64)>,
}

/// 抑止トライアル失敗 (抑止フレームで paint が来ない = BF#1-only パイプラインが
/// 流れていない) 後に flush 常用へ戻す期間 (フレーム数 ≒ 1 秒)。
/// 低速の指ドラッグ帯域では抑止に定着せず 0F↔1F を 4 フレーム周期で発振し、遷移の
/// たびに 1 paint 落ちる (実測 2026-07-23 build: paint パターン 1,1,1,0 の規則周期 =
/// 低速スクロールのガタつきの正体)。この帯域は damage が軽く flush 常用でも GPU は
/// 飽和しないため、失敗を検出したら試行を 1 秒に 1 回まで抑える。抑止が成立する帯域
/// (高速スクロール/momentum = 52fps 回帰の実測があった保護領域) ではトライアルが
/// 即座に成功しクールダウンは発生しない = 従来挙動と完全に同一。
const SUPPRESSION_RETRY_FRAMES: u32 = 60;

impl CefServer {
    pub fn new(client_pid: Option<u32>, use_gpu: bool) -> Self {
        CefServer {
            browsers: HashMap::new(),
            next_browser_id: AtomicU32::new(1),
            server_pid: std::process::id(),
            client_pid,
            use_gpu,
            pending_flush: None,
            paints_at_last_begin_frame_1: 0,
            damage_streak: 0,
            last_begin_frame_1_suppressed: false,
            suppression_cooldown: 0,
            deferred_begin_frame: None,
        }
    }

    /// Initialize CEF. Must be called on main thread before anything else.
    pub fn initialize_cef(&self) -> bool {
        log("initialize_cef() starting");

        #[cfg(target_os = "macos")]
        unsafe {
            cef_unity_inject_app_protocol();
        }

        load_cef_auto();

        let arguments = cef::args::Args::new();
        let executable_path = std::env::current_exe().unwrap();
        let executable_directory = executable_path.parent().unwrap();

        let helper_path = helper_binary_path(executable_directory);
        log(&format!("helper_path = {}", helper_path.display()));

        let cache_dir = std::env::temp_dir().join("cef_unity_cache");
        let _ = std::fs::create_dir_all(&cache_dir);

        let mut settings = Settings::default();
        settings.no_sandbox = 1;
        settings.windowless_rendering_enabled = 1;
        settings.external_message_pump = 1;
        settings.root_cache_path = CefString::from(cache_dir.to_str().unwrap());
        settings.browser_subprocess_path = CefString::from(helper_path.to_str().unwrap());

        // macOS: executable からの相対パスで Framework を解決
        #[cfg(target_os = "macos")]
        {
            let frameworks_dir = executable_directory
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
            let executable_directory_string = executable_directory.to_str().unwrap();
            settings.resources_dir_path = CefString::from(executable_directory_string);
            let locales_dir = executable_directory.join("locales");
            if locales_dir.exists() {
                settings.locales_dir_path = CefString::from(locales_dir.to_str().unwrap());
            }
        }

        let cef_log = std::env::temp_dir().join("cef_debug.log");
        settings.log_file = CefString::from(cef_log.to_str().unwrap());
        settings.log_severity = LogSeverity::VERBOSE;

        let browser_process_handler = ServerBrowserProcessHandler::new();
        let mut app = ServerApp::new(browser_process_handler, self.use_gpu);
        let result = initialize(
            Some(arguments.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        log(&format!("initialize() returned {}", result));
        result != 0
    }

    /// Handle a single IPC command. Returns a Response.
    pub fn handle_command(&mut self, command: Command) -> Response {
        match command {
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
        // 2160px を超える)。software shared_memory の上限は on_paint 側でガードする。
        let width = width.max(1);
        let height = height.max(1);
        let id = self.next_browser_id.fetch_add(1, Ordering::Relaxed);
        let shared_memory_flink = ipc::shared_memory_flink_path(self.server_pid, id);

        let shared_memory = match SharedMemoryWriter::new(&shared_memory_flink) {
            Ok(writer) => Arc::new(writer),
            Err(error) => {
                return Response::Error {
                    message: format!("shm_create failed: {}", error),
                };
            }
        };

        let audio_shared_memory_flink = ipc::audio_shared_memory_flink_path(self.server_pid, id);
        let audio_shared_memory = match AudioSharedMemoryWriter::new(&audio_shared_memory_flink) {
            Ok(writer) => Arc::new(writer),
            Err(error) => {
                return Response::Error {
                    message: format!("audio_shm_create failed: {}", error),
                };
            }
        };

        let viewport_width = Arc::new(AtomicI32::new(width));
        let viewport_height = Arc::new(AtomicI32::new(height));
        let browser_slot: Arc<Mutex<Option<Browser>>> = Arc::new(Mutex::new(None));

        // Windows のみ: D3D11 共有テクスチャプールを作成 (失敗時は software 経路にフォールバック)。
        // 非 Windows ではスタブ実装が常に Err を返すので None になる。
        // CPU モード (use_gpu=false) では作らず、software paint を強制する。
        let d3d11_pool: Option<Arc<D3D11Pool>> = if !self.use_gpu {
            log("use_gpu=false: skipping D3D11Pool, forcing software paint");
            None
        } else {
            match D3D11Pool::new(self.client_pid) {
                Ok(pool) => {
                    log(&format!(
                        "D3D11Pool created (client_pid={:?})",
                        self.client_pid
                    ));
                    Some(Arc::new(pool))
                }
                Err(_error) => {
                    #[cfg(target_os = "windows")]
                    log(&format!(
                        "D3D11Pool::new failed, falling back to software paint: {}",
                        _error
                    ));
                    None
                }
            }
        };

        let render_handler = ServerRenderHandler::new(
            Arc::clone(&shared_memory),
            Arc::clone(&viewport_width),
            Arc::clone(&viewport_height),
            d3d11_pool.clone(),
        );
        let life_span_handler = ServerLifeSpanHandler::new(Arc::clone(&browser_slot));
        let display_handler = ServerDisplayHandler::new(Arc::clone(&shared_memory));
        let load_handler = ServerLoadHandler::new(Arc::clone(&browser_slot));
        let audio_handler = ServerAudioHandler::new(Arc::clone(&audio_shared_memory));
        let mut client = ServerClient::new(
            render_handler,
            life_span_handler,
            display_handler,
            load_handler,
            audio_handler,
        );

        // cef_window_handle_t はプラットフォーム依存:
        //   macOS: *mut c_void
        //   Linux: c_ulong (X11 Window / XID)
        //   Windows: HWND (newtype wrapping *mut c_void)
        #[cfg(target_os = "windows")]
        let parent_handle = cef::sys::HWND(std::ptr::null_mut());
        #[cfg(target_os = "macos")]
        let parent_handle = std::ptr::null_mut();
        #[cfg(target_os = "linux")]
        let parent_handle: cef::sys::cef_window_handle_t = 0;
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
        // Linux: ozone プラットフォームの選択と同じ判定を使う。x11 を選んでいない
        // (= headless) のに立てると、CEF が software paint を止めるだけで
        // accelerated paint も来ず、フレームが一切供給されなくなる。
        #[cfg(target_os = "linux")]
        if linux_use_accelerated_paint(self.use_gpu) {
            window_info.shared_texture_enabled = 1;
            // 出力バッファの fd を渡す経路。パスはクライアントも server_pid と
            // browser_id から同じ規則で導出する (macOS の Mach サービス名と同じ流儀)。
            start_dmabuf_listener(cef_unity_ipc::dmabuf_socket_path(self.server_pid, id));
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
                message: "browser_host_create_browser failed".to_string(),
            };
        }

        let d3d11_fence_handle = d3d11_pool
            .as_ref()
            .map(|pool| pool.client_fence_handle())
            .unwrap_or(0);

        self.browsers.insert(
            id,
            BrowserState {
                shared_memory,
                audio_shared_memory,
                browser: browser_slot,
                viewport_width,
                viewport_height,
                d3d11_pool,
            },
        );

        Response::BrowserCreated {
            browser_id: id,
            shared_memory_flink,
            d3d11_fence_handle,
            audio_shared_memory_flink,
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
                message: format!("browser {} not found", browser_id),
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
                message: "browser not ready yet".to_string(),
            }
        } else {
            Response::Error {
                message: format!("browser {} not found", browser_id),
            }
        }
    }

    fn resize(&mut self, browser_id: u32, width: i32, height: i32) -> Response {
        // create_browser と同じく上限 clamp はしない (GPU 経路のサイズ乖離防止)。
        let width = width.max(1);
        let height = height.max(1);
        if let Some(state) = self.browsers.get(&browser_id) {
            state.viewport_width.store(width, Ordering::Relaxed);
            state.viewport_height.store(height, Ordering::Relaxed);
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
                && let Some(host) = Browser::host(browser)
            {
                BrowserHost::was_resized(&host);
                BrowserHost::invalidate(&host, PaintElementType::VIEW);
            }
            Response::Ok
        } else {
            Response::Error {
                message: format!("browser {} not found", browser_id),
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
                message: format!("browser {} not found", browser_id),
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
                message: format!("browser {} not found", browser_id),
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
                message: format!("browser {} not found", browser_id),
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
                message: format!("browser {} not found", browser_id),
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
                message: format!("browser {} not found", browser_id),
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
                message: "browser or frame not available".to_string(),
            }
        } else {
            Response::Error {
                message: format!("browser {} not found", browser_id),
            }
        }
    }

    fn get_current_url(&self, browser_id: u32) -> Response {
        if let Some(state) = self.browsers.get(&browser_id) {
            if let Some(ref browser) = *state.browser.lock().unwrap_or_else(PoisonError::into_inner)
                && let Some(frame) = Browser::main_frame(browser)
            {
                let url = frame.url();
                let url_string = CefString::from(&url);
                return Response::CurrentUrl {
                    url: url_string.to_string(),
                };
            }
            Response::Error {
                message: "browser or frame not available".to_string(),
            }
        } else {
            Response::Error {
                message: format!("browser {} not found", browser_id),
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
                message: format!("browser {} not found", browser_id),
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
                message: format!("browser {} not found", browser_id),
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
                message: format!("browser {} not found", browser_id),
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
                message: format!("browser {} not found", browser_id),
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
            LAST_BEGIN_FRAME_NANOSECONDS.store(now_nanoseconds(), Ordering::Relaxed);
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
                message: format!("browser {} not found", browser_id),
            };
        }
        // 前フレームの GPU コピーが転送元をまだ読んでいる間は発行できない (issue #7)。
        // 待たずに保留し、tick で再試行する。damage streak の判定はここでは進めない
        // — 実際に発行するときに 1 回だけ行う。
        if !begin_frame_gate_open() {
            self.deferred_begin_frame = Some((browser_id, unity_frame));
            BEGIN_FRAME_DEFERRED_COUNT.fetch_add(1, Ordering::Relaxed);
            return Response::Ok;
        }
        self.deferred_begin_frame = None;

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
        let painted_last_frame = paints_now > self.paints_at_last_begin_frame_1;
        self.paints_at_last_begin_frame_1 = paints_now;
        self.damage_streak = if painted_last_frame {
            self.damage_streak.saturating_add(1)
        } else {
            0
        };

        // 抑止トライアルの失敗検出 (SUPPRESSION_RETRY_FRAMES の定義コメント参照):
        // 前フレームを抑止したのに paint が来なかった = BF#1-only パイプラインが
        // 流れていない帯域。クールダウンを置いて flush 常用に戻す。
        // 開発トグル cef_no_streak_cooldown で旧挙動 (常時トライアル) に戻せる。
        if self.last_begin_frame_1_suppressed && !painted_last_frame && streak_cooldown_enabled() {
            self.suppression_cooldown = SUPPRESSION_RETRY_FRAMES;
        } else if self.suppression_cooldown > 0 {
            self.suppression_cooldown -= 1;
        }
        let suppress = self.damage_streak >= DAMAGE_STREAK_SUPPRESS_FLUSH
            && self.suppression_cooldown == 0;
        self.last_begin_frame_1_suppressed = suppress && server_flush_enabled();

        if self.issue_begin_frame(browser_id, unity_frame)
            && server_flush_enabled()
            && !suppress
        {
            // flush を予約 (tick で +3ms, +6ms に発行)。
            self.pending_flush = Some(PendingFlush {
                browser_id,
                unity_frame,
                begin_frame_1: Instant::now(),
                flushes_done: 0,
                paints_at_begin_frame_1: paints_now,
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
        // ゲート待ちで見送った BF#1 を、転送元の読み出しが終わり次第発行する。
        // flush より先に処理する — 保留中の BF#1 が無いのに flush だけ撃っても意味がない。
        if let Some((browser_id, unity_frame)) = self.deferred_begin_frame
            && begin_frame_gate_open()
        {
            self.deferred_begin_frame = None;
            self.send_external_begin_frame(browser_id, unity_frame);
        }

        let action = {
            let Some(pending_flush) = self.pending_flush.as_mut() else {
                return;
            };
            let index = pending_flush.flushes_done as usize;
            if index >= FLUSH_THRESHOLDS_MILLISECONDS.len() {
                None // すべて発行済み → クリア
            } else if PAINT_COUNT
                .load(Ordering::Relaxed)
                .wrapping_sub(pending_flush.paints_at_begin_frame_1)
                >= 2
            {
                // BF#1 以降に既に 2 paint (#A stale + flush 由来 fresh #B) が届いている
                // = このフレームの最新内容は配信済み。以降の flush は冗長な draw を増やす
                // だけで、スクロール等 damage が毎 BF 発生する状況では renderer/GPU を
                // 飽和させて begin_frame_pending_ による BF drop → コンテンツ欠落を招く
                // (実測: Wikipedia スクロールで 60→52fps) ため撃たずに終了する。
                None
            } else {
                let elapsed_milliseconds = pending_flush.begin_frame_1.elapsed().as_secs_f64() * 1000.0;
                if elapsed_milliseconds >= FLUSH_THRESHOLDS_MILLISECONDS[index] {
                    pending_flush.flushes_done += 1;
                    Some((pending_flush.browser_id, pending_flush.unity_frame, pending_flush.flushes_done))
                } else {
                    return; // まだ発行時刻でない (保留継続)
                }
            }
        };
        match action {
            Some((browser_id, unity_frame, done)) => {
                // flush は最新内容を取り直すための追加 BeginFrame。ゲートが閉じている
                // ときは撃たない (BF#1 と違い保留はしない — 次フレームで撃ち直せばよい)。
                if begin_frame_gate_open() {
                    self.issue_begin_frame(browser_id, unity_frame);
                }
                if done as usize >= FLUSH_THRESHOLDS_MILLISECONDS.len() {
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
fn helper_binary_path(executable_directory: &std::path::Path) -> std::path::PathBuf {
    // CEF は browser_subprocess_path から "Helper (GPU)" 等のバリアントを自動検出する。
    // <server.app>/Contents/Frameworks/cef-unity-server Helper.app/Contents/MacOS/cef-unity-server Helper
    executable_directory
        .parent()
        .unwrap() // Contents
        .join("Frameworks/cef-unity-server Helper.app/Contents/MacOS/cef-unity-server Helper")
}

#[cfg(target_os = "linux")]
fn helper_binary_path(executable_directory: &std::path::Path) -> std::path::PathBuf {
    executable_directory.join("cef-unity-rust-helper")
}

#[cfg(target_os = "windows")]
fn helper_binary_path(executable_directory: &std::path::Path) -> std::path::PathBuf {
    // ".exe" は Windows の実行ファイル拡張子であり識別子ではない (命名規約の展開対象外)。
    // ここが実ファイル名と食い違うと CEF のサブプロセス起動が error_code=63 で失敗する。
    executable_directory.join("cef-unity-rust-helper.exe")
}

#[cfg(all(test, target_os = "linux"))]
mod linux_accelerated_paint_tests {
    use super::linux_accelerated_paint_available;

    #[test]
    fn 三条件が揃えば有効() {
        assert!(linux_accelerated_paint_available(true, Some(":0"), true));
    }

    #[test]
    fn cpu_モードでは無効() {
        assert!(!linux_accelerated_paint_available(false, Some(":0"), true));
    }

    #[test]
    fn ディスプレイが無ければ無効() {
        // CI ランナーとコンテナがこれに該当する。headless + software paint に落ちる。
        assert!(!linux_accelerated_paint_available(true, None, true));
    }

    #[test]
    fn 空文字の_display_はディスプレイ無しとして扱う() {
        assert!(!linux_accelerated_paint_available(true, Some(""), true));
    }

    #[test]
    fn プールを構築できなければ無効() {
        // EGL の拡張が足りない、ドライバが古い、といった環境。
        assert!(!linux_accelerated_paint_available(true, Some(":0"), false));
    }
}
