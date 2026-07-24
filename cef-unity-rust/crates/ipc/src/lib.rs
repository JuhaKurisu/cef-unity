// IPC protocol definitions and shared memory layout for CEF Server <-> Unity communication.
//
// Transport: ipc-channel (Mach ports on macOS)
// Pixel data: shared_memory crate (POSIX shm)

use ipc_channel::ipc::{IpcReceiver, IpcSender};
use serde::{Deserialize, Serialize};
use shared_memory::{Shmem, ShmemConf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug)]
pub enum Command {
    CreateBrowser {
        width: i32,
        height: i32,
        url: String,
    },
    DestroyBrowser {
        browser_id: u32,
    },
    LoadUrl {
        browser_id: u32,
        url: String,
    },
    Resize {
        browser_id: u32,
        width: i32,
        height: i32,
    },
    MouseMove {
        browser_id: u32,
        x: i32,
        y: i32,
        modifiers: u32,
    },
    MouseClick {
        browser_id: u32,
        x: i32,
        y: i32,
        modifiers: u32,
        button: u8,
        mouse_up: bool,
        click_count: i32,
    },
    MouseWheel {
        browser_id: u32,
        x: i32,
        y: i32,
        modifiers: u32,
        delta_x: i32,
        delta_y: i32,
    },
    KeyEvent {
        browser_id: u32,
        /// 0=RAWKEYDOWN, 1=KEYUP, 2=CHAR
        event_type: u8,
        modifiers: u32,
        windows_key_code: i32,
        native_key_code: i32,
        character: u16,
        unmodified_character: u16,
        is_system_key: i32,
        focus_on_editable_field: i32,
    },
    ExecuteJavaScript {
        browser_id: u32,
        code: String,
    },
    EditCommand {
        browser_id: u32,
        /// 0=Copy, 1=Paste, 2=Cut, 3=SelectAll, 4=Undo, 5=Redo
        command: u8,
    },
    GetCurrentUrl {
        browser_id: u32,
    },
    ImeSetComposition {
        browser_id: u32,
        text: String,
        selection_start: u32,
        selection_end: u32,
    },
    ImeCommitText {
        browser_id: u32,
        text: String,
    },
    ImeFinishComposingText {
        browser_id: u32,
        keep_selection: bool,
    },
    ImeCancelComposition {
        browser_id: u32,
    },
    /// External BeginFrame: Unity 側のフレーム冒頭で発行され、CEF Viz Compositor に
    /// 「次のフレームを描いてよい」と通知する。WindowInfo::external_begin_frame_enabled=1
    /// 時のみ意味を持ち、windowless_frame_rate に基づく自発フレーム生成を置き換える。
    /// `unity_frame` は発行時の Time.frameCount。end-to-end フレーム遅延計測に使う。
    SendExternalBeginFrame {
        browser_id: u32,
        unity_frame: u64,
    },
    GetLogs,
    Shutdown,
}

/// Wrapper that pairs a Command with whether the sender expects a response.
/// The server only sends back a Response when `expects_response` is true.
#[derive(Serialize, Deserialize, Debug)]
pub struct CommandEnvelope {
    pub command: Command,
    pub expects_response: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Response {
    BrowserCreated {
        browser_id: u32,
        shared_memory_flink: String,
        /// Windows D3D11/D3D12 同期用の shared ID3D11Fence の NT 共有 HANDLE 値。
        /// クライアントプロセスへ DuplicateHandle 済み。0 の場合は fence 未対応 (非 Windows / pool 構築失敗)。
        d3d11_fence_handle: u64,
        /// 音声リングバッファ用の共有メモリ flink パス。CEF AudioHandler が PCM を
        /// このバッファへ書き込み、クライアントは AudioSharedMemoryReader で読み出す。
        audio_shared_memory_flink: String,
    },
    CurrentUrl { url: String },
    Logs { entries: Vec<String> },
    Ok,
    Error { message: String },
}

/// Sent from server to client during bootstrap to establish bidirectional channels.
#[derive(Serialize, Deserialize, Debug)]
pub struct Bootstrap {
    pub command_sender: IpcSender<CommandEnvelope>,
    pub response_receiver: IpcReceiver<Response>,
    /// Server PID — used to derive Mach service name for IOSurface port transfer.
    pub server_pid: u32,
}

/// Derive the Mach bootstrap service name for IOSurface port transfer.
pub fn iosurface_service_name(server_pid: u32) -> String {
    format!("com.cef-unity.iosurface.{}", server_pid)
}

// ---------------------------------------------------------------------------
// Shared memory layout (per browser)
// ---------------------------------------------------------------------------

pub const MAX_WIDTH: u32 = 3840;
pub const MAX_HEIGHT: u32 = 2160;
pub const BUFFER_SIZE: usize = (MAX_WIDTH * MAX_HEIGHT * 4) as usize;
pub const SHARED_MEMORY_HEADER_SIZE: usize = 128;
pub const SHARED_MEMORY_TOTAL_SIZE: usize = SHARED_MEMORY_HEADER_SIZE + BUFFER_SIZE * 2;

/// Header at offset 0 of shared memory. 128 bytes, cache-line aligned.
#[repr(C, align(64))]
pub struct SharedMemoryHeader {
    pub frame_id: AtomicU64,
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// 0 or 1 — which buffer is currently readable
    pub active_buffer: AtomicU32,
    /// IME caret rect (set by on_ime_composition_range_changed)
    pub ime_caret_x: AtomicI32,
    pub ime_caret_y: AtomicI32,
    pub ime_caret_width: AtomicI32,
    pub ime_caret_height: AtomicI32,
    // ---- Accelerated paint metadata (shared between macOS IOSurface and Windows D3D11) ----
    pub accelerated_frame_id: AtomicU64,
    pub accelerated_surface_id: AtomicU32,
    pub accelerated_width: AtomicU32,
    pub accelerated_height: AtomicU32,
    pub accelerated_format: AtomicU32, // 0=BGRA, 1=RGBA
    // ---- Windows D3D11 accelerated paint ----
    /// NT shared HANDLE (DuplicateHandle 済みの値) を 64bit で格納する。
    /// macOS では未使用 (IOSurface の Mach port 経由で別チャネル送信)。
    pub d3d11_handle: AtomicU64,
    /// d3d11_handle に対応するフレーム ID (handle 値だけだと変化検出できないため用意)。
    pub d3d11_frame_id: AtomicU64,
    /// shared ID3D11Fence の最新 Signal 値。クライアントはこの値以上に到達するのを
    /// 待ってからサンプルする。書き込み順は: フレームピクセル書き込み → Signal →
    /// この値更新 → d3d11_frame_id 更新。
    pub d3d11_fence_value: AtomicU64,
    /// 直近の on_accelerated_paint に対応する SendExternalBeginFrame 発行時の
    /// Unity フレーム番号 (Time.frameCount)。Unity 側はこれと現在の frameCount の
    /// 差分で end-to-end のフレーム遅延を測れる。0 = 未設定。
    pub accelerated_paint_unity_frame: AtomicU64,
}

use std::sync::atomic::AtomicI32;

const _: () = assert!(std::mem::size_of::<SharedMemoryHeader>() == SHARED_MEMORY_HEADER_SIZE);

/// Generate a shared memory flink path for a browser.
pub fn shared_memory_flink_path(server_pid: u32, browser_id: u32) -> String {
    let temporary_directory = std::env::temp_dir();
    temporary_directory.join(format!("cef-unity-shm-{}-{}", server_pid, browser_id))
        .to_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// Audio shared memory layout (per browser)
//
// 映像とは独立した共有メモリセグメント。CEF の AudioHandler は別スレッド
// (audio thread) から on_audio_stream_packet を呼ぶため、映像経路 (frame_id /
// double-pump 同期) を一切乱さないよう専用セグメントに分離する。
//
// リングバッファは「フレーム」単位で管理する。1 フレーム = 1 サンプリング時刻の
// 全チャネル分。各フレームスロットは固定ストライド AUDIO_MAX_CHANNELS 個の f32 を
// 占有する (実チャネル数に依らずインデックス計算を一定にするため)。インターリーブ
// 配置 (LRLRLR...) で格納する。
// ---------------------------------------------------------------------------

/// リングバッファが格納できる最大チャネル数。これを超えるレイアウトは切り詰める。
pub const AUDIO_MAX_CHANNELS: usize = 8;
/// リングバッファ容量 (フレーム数)。48kHz で約 1 秒分。アンダーラン/オーバーランの
/// マージンを十分に取る。
pub const AUDIO_RING_FRAMES: usize = 48_000;
/// リング全体の f32 サンプル数 (フレーム × 固定ストライド)。
pub const AUDIO_RING_SAMPLES: usize = AUDIO_RING_FRAMES * AUDIO_MAX_CHANNELS;
/// リングデータ部のバイト数。
pub const AUDIO_RING_BYTES: usize = AUDIO_RING_SAMPLES * 4;
/// 音声ヘッダのサイズ (64 バイト, cache-line aligned)。
pub const AUDIO_SHARED_MEMORY_HEADER_SIZE: usize = 64;
/// 音声共有メモリ全体のサイズ。
pub const AUDIO_SHARED_MEMORY_TOTAL_SIZE: usize = AUDIO_SHARED_MEMORY_HEADER_SIZE + AUDIO_RING_BYTES;

/// 音声共有メモリ先頭のヘッダ。64 バイト, cache-line aligned。
#[repr(C, align(64))]
pub struct AudioSharedMemoryHeader {
    /// ストリームのサンプリングレート (Hz)。on_audio_stream_started で設定。
    pub sample_rate: AtomicU32,
    /// チャネル数 (1=mono, 2=stereo, ...)。AUDIO_MAX_CHANNELS で切り詰め済み。
    pub channels: AtomicU32,
    /// 1 = ストリーム再生中, 0 = 停止。
    pub stream_active: AtomicU32,
    _pad0: AtomicU32,
    /// ストリーム開始からの累積書き込みフレーム数 (単調増加)。リーダーは前回値との
    /// 差分で新規データ量を知る。ストリーム再開時は 0 にリセットされる。
    pub write_frames: AtomicU64,
    /// リング容量 (フレーム数)。常に AUDIO_RING_FRAMES だがリーダー向けに公開する。
    pub ring_frames: AtomicU32,
    _pad1: AtomicU32,
}

const _: () = assert!(std::mem::size_of::<AudioSharedMemoryHeader>() == AUDIO_SHARED_MEMORY_HEADER_SIZE);

/// 音声リングバッファ用の共有メモリ flink パスを生成する。
pub fn audio_shared_memory_flink_path(server_pid: u32, browser_id: u32) -> String {
    let temporary_directory = std::env::temp_dir();
    temporary_directory.join(format!("cef-unity-audio-{}-{}", server_pid, browser_id))
        .to_str()
        .unwrap()
        .to_string()
}

/// サーバー側: CEF AudioHandler から PCM をリングバッファへ書き込むハンドル。
pub struct AudioSharedMemoryWriter {
    shared_memory: Shmem,
    pub flink: String,
}

// on_audio_stream_packet は単一の audio thread からのみ呼ばれる (SPSC writer)。
unsafe impl Send for AudioSharedMemoryWriter {}
unsafe impl Sync for AudioSharedMemoryWriter {}

impl AudioSharedMemoryWriter {
    pub fn new(flink: &str) -> std::io::Result<Self> {
        let _ = std::fs::remove_file(flink);

        let shared_memory = ShmemConf::new()
            .size(AUDIO_SHARED_MEMORY_TOTAL_SIZE)
            .flink(flink)
            .create()
            .map_err(|error| std::io::Error::other(error.to_string()))?;

        unsafe {
            std::ptr::write_bytes(shared_memory.as_ptr(), 0, AUDIO_SHARED_MEMORY_HEADER_SIZE);
        }
        let writer = AudioSharedMemoryWriter {
            shared_memory,
            flink: flink.to_string(),
        };
        writer.header()
            .ring_frames
            .store(AUDIO_RING_FRAMES as u32, Ordering::Release);
        Ok(writer)
    }

    fn header(&self) -> &AudioSharedMemoryHeader {
        unsafe { &*(self.shared_memory.as_ptr() as *const AudioSharedMemoryHeader) }
    }

    fn ring_base(&self) -> *mut f32 {
        unsafe { self.shared_memory.as_ptr().add(AUDIO_SHARED_MEMORY_HEADER_SIZE) as *mut f32 }
    }

    /// ストリーム開始。フォーマットを設定し write カーソルを 0 に戻す。
    pub fn start_stream(&self, sample_rate: u32, channels: u32) {
        let header = self.header();
        let clamped_channels = (channels as usize).min(AUDIO_MAX_CHANNELS) as u32;
        header.write_frames.store(0, Ordering::Release);
        header.sample_rate.store(sample_rate, Ordering::Release);
        header.channels.store(clamped_channels, Ordering::Release);
        header.stream_active.store(1, Ordering::Release);
    }

    /// ストリーム停止。
    pub fn stop_stream(&self) {
        self.header().stream_active.store(0, Ordering::Release);
    }

    /// 現在のストリームのチャネル数 (start_stream で設定された値)。
    pub fn channels(&self) -> usize {
        self.header().channels.load(Ordering::Acquire) as usize
    }

    /// planar float PCM (`data[channel][frame]`) を interleaved でリングへ書き込む。
    /// `data` は長さ `channels` のチャネルポインタ配列。`frames` は 1 チャネルあたりの
    /// サンプル数。
    ///
    /// # Safety
    /// `data` は `channels` 個の有効な `*const f32` を指し、各ポインタは少なくとも
    /// `frames` 個の f32 を指していなければならない。
    pub unsafe fn write_packet(&self, data: *const *const f32, frames: usize, channels: usize) {
        if frames == 0 || channels == 0 || data.is_null() {
            return;
        }
        let clamped_channels = channels.min(AUDIO_MAX_CHANNELS);
        let header = self.header();
        let capacity = AUDIO_RING_FRAMES;
        let base = self.ring_base();
        // writer は単独スレッドなので Relaxed で現在値を取得。
        let start = header.write_frames.load(Ordering::Relaxed) as usize;

        let planes = unsafe { std::slice::from_raw_parts(data, channels) };
        for frame_index in 0..frames {
            let ring_frame = (start + frame_index) % capacity;
            let destination = unsafe { base.add(ring_frame * AUDIO_MAX_CHANNELS) };
            for (channel_index, &plane) in planes.iter().enumerate().take(clamped_channels) {
                let sample = unsafe { *plane.add(frame_index) };
                unsafe { *destination.add(channel_index) = sample };
            }
        }
        // データ書き込み完了後にカーソルを公開 (Release)。
        header
            .write_frames
            .store((start + frames) as u64, Ordering::Release);
    }
}

/// クライアント側: リングバッファから PCM を読み出すハンドル。
pub struct AudioSharedMemoryReader {
    shared_memory: Shmem,
    /// これまでに読み出したフレーム数 (累積)。
    pub read_frames: u64,
}

unsafe impl Send for AudioSharedMemoryReader {}

impl AudioSharedMemoryReader {
    pub fn open(flink: &str) -> std::io::Result<Self> {
        let shared_memory = ShmemConf::new()
            .flink(flink)
            .open()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(AudioSharedMemoryReader {
            shared_memory,
            read_frames: 0,
        })
    }

    fn header(&self) -> &AudioSharedMemoryHeader {
        unsafe { &*(self.shared_memory.as_ptr() as *const AudioSharedMemoryHeader) }
    }

    fn ring_base(&self) -> *const f32 {
        unsafe { self.shared_memory.as_ptr().add(AUDIO_SHARED_MEMORY_HEADER_SIZE) as *const f32 }
    }

    /// 現在のストリームフォーマットを返す `(sample_rate, channels, active)`。
    pub fn format(&self) -> (u32, u32, bool) {
        let header = self.header();
        (
            header.sample_rate.load(Ordering::Acquire),
            header.channels.load(Ordering::Acquire),
            header.stream_active.load(Ordering::Acquire) != 0,
        )
    }

    /// 未読の PCM を最大 `max_frames` フレーム読み出し、`out` に interleaved で書き込む。
    /// `out` は少なくとも `max_frames * channels` 個の f32 を保持できること。
    /// 戻り値は `(読み出したフレーム数, チャネル数)`。
    ///
    /// オーバーラン (writer がリングを 1 周以上追い越した) 場合は最古のデータを捨て、
    /// 直近 `AUDIO_RING_FRAMES` フレーム分まで巻き戻して読む。
    pub fn read(&mut self, out: &mut [f32], max_frames: usize) -> (usize, usize) {
        let header = self.header();
        let channels = header.channels.load(Ordering::Acquire) as usize;
        if channels == 0 {
            return (0, 0);
        }
        let clamped_channels = channels.min(AUDIO_MAX_CHANNELS);
        let write = header.write_frames.load(Ordering::Acquire);

        // ストリーム再開で write_frames がリセットされたら読みカーソルも合わせる。
        if write < self.read_frames {
            self.read_frames = write;
        }
        let capacity = AUDIO_RING_FRAMES;
        let mut available = (write - self.read_frames) as usize;
        if available > capacity {
            // オーバーラン: 最古を捨てて直近 capacity フレームへ巻き戻す。
            self.read_frames = write - capacity as u64;
            available = capacity;
        }
        let to_read = available.min(max_frames);
        if to_read == 0 {
            return (0, channels);
        }

        let base = self.ring_base();
        let start = self.read_frames as usize;
        for frame_index in 0..to_read {
            let ring_frame = (start + frame_index) % capacity;
            let source = unsafe { base.add(ring_frame * AUDIO_MAX_CHANNELS) };
            let output_offset = frame_index * channels;
            for channel_index in 0..clamped_channels {
                out[output_offset + channel_index] = unsafe { *source.add(channel_index) };
            }
            // 実チャネル < 宣言チャネルの端数を 0 埋め (通常発生しない)。
            for channel_index in clamped_channels..channels {
                out[output_offset + channel_index] = 0.0;
            }
        }
        self.read_frames += to_read as u64;
        (to_read, channels)
    }
}

// ---------------------------------------------------------------------------
// High-level shared memory wrappers (shared_memory crate)
// ---------------------------------------------------------------------------

/// Server-side handle for writing frames into shared memory.
pub struct SharedMemoryWriter {
    shared_memory: Shmem,
    pub flink: String,
}

unsafe impl Send for SharedMemoryWriter {}

impl SharedMemoryWriter {
    pub fn new(flink: &str) -> std::io::Result<Self> {
        // Remove stale flink file
        let _ = std::fs::remove_file(flink);

        let shared_memory = ShmemConf::new()
            .size(SHARED_MEMORY_TOTAL_SIZE)
            .flink(flink)
            .create()
            .map_err(|error| std::io::Error::other(error.to_string()))?;

        // Zero-initialize header
        unsafe {
            std::ptr::write_bytes(shared_memory.as_ptr(), 0, SHARED_MEMORY_HEADER_SIZE);
        }

        Ok(SharedMemoryWriter {
            shared_memory,
            flink: flink.to_string(),
        })
    }

    fn header(&self) -> &SharedMemoryHeader {
        unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) }
    }

    /// Write IME caret position into the shared memory header.
    pub fn write_ime_caret(&self, x: i32, y: i32, width: i32, height: i32) {
        let header = self.header();
        header.ime_caret_x.store(x, Ordering::Release);
        header.ime_caret_y.store(y, Ordering::Release);
        header.ime_caret_width.store(width, Ordering::Release);
        header.ime_caret_height.store(height, Ordering::Release);
    }

    /// Write IOSurface info into the shared memory header.
    pub fn write_iosurface_info(&self, surface_id: u32, width: u32, height: u32, format: u32) {
        let header = self.header();
        header.accelerated_surface_id.store(surface_id, Ordering::Release);
        header.accelerated_width.store(width, Ordering::Release);
        header.accelerated_height.store(height, Ordering::Release);
        header.accelerated_format.store(format, Ordering::Release);
        header.accelerated_frame_id.fetch_add(1, Ordering::Release);
    }

    /// この paint に対応する SendExternalBeginFrame 発行時の Unity frame 番号を書き込む。
    /// accelerated_frame_id / d3d11_frame_id を更新する**前**に書くこと
    /// (クライアントは frame_id 増分を検出してから他フィールドを読むため)。
    pub fn write_paint_unity_frame(&self, unity_frame: u64) {
        let header = self.header();
        header
            .accelerated_paint_unity_frame
            .store(unity_frame, Ordering::Release);
    }

    /// Windows: NT 共有 HANDLE (client プロセスへ DuplicateHandle 済みの値) を書き込む。
    /// width/height/format/fence_value も同時に更新する。
    /// 書き込み順は handle → fence_value → frame_id (クライアント側で frame_id 増分検出後に
    /// fence_value を読む順番で整合させる)。
    pub fn write_d3d11_handle(
        &self,
        handle: u64,
        width: u32,
        height: u32,
        format: u32,
        fence_value: u64,
    ) {
        let header = self.header();
        header.accelerated_width.store(width, Ordering::Release);
        header.accelerated_height.store(height, Ordering::Release);
        header.accelerated_format.store(format, Ordering::Release);
        header.d3d11_handle.store(handle, Ordering::Release);
        header.d3d11_fence_value.store(fence_value, Ordering::Release);
        // d3d11_frame_id は最後に更新する: クライアントは frame_id の変化を検出して読む
        header.d3d11_frame_id.fetch_add(1, Ordering::Release);
    }

    /// Write a frame. The buffer must be width*height*4 BGRA bytes.
    pub fn write_frame(&self, pixels: &[u8], width: u32, height: u32) {
        // u32 のまま乗算すると巨大寸法で wrap し境界チェックをすり抜けるため usize で計算
        let size = (width as usize) * (height as usize) * 4;
        assert!(width <= MAX_WIDTH && height <= MAX_HEIGHT && size <= BUFFER_SIZE);
        assert_eq!(pixels.len(), size);

        let header = self.header();
        // Write to inactive buffer
        let active = header.active_buffer.load(Ordering::Acquire);
        let target = if active == 0 { 1 } else { 0 };
        let offset = SHARED_MEMORY_HEADER_SIZE + target as usize * BUFFER_SIZE;

        unsafe {
            let destination = self.shared_memory.as_ptr().add(offset);
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), destination, size);
        }

        header.width.store(width, Ordering::Release);
        header.height.store(height, Ordering::Release);
        header.active_buffer.store(target, Ordering::Release);
        header.frame_id.fetch_add(1, Ordering::Release);
    }
}

/// Client-side handle for reading frames from shared memory.
pub struct SharedMemoryReader {
    shared_memory: Shmem,
    pub last_frame_id: u64,
    pub last_accelerated_frame_id: u64,
    pub last_d3d11_frame_id: u64,
}

unsafe impl Send for SharedMemoryReader {}

impl SharedMemoryReader {
    pub fn open(flink: &str) -> std::io::Result<Self> {
        let shared_memory = ShmemConf::new()
            .flink(flink)
            .open()
            .map_err(|error| std::io::Error::other(error.to_string()))?;

        Ok(SharedMemoryReader {
            shared_memory,
            last_frame_id: 0,
            last_accelerated_frame_id: 0,
            last_d3d11_frame_id: 0,
        })
    }

    /// Read Windows D3D11 shared handle info from the shared memory header.
    /// Returns Some((handle, width, height, format, fence_value)) if a new accelerated frame
    /// is available. クライアントは fence_value 以上に到達するのを待ってからサンプルする。
    pub fn get_d3d11_handle(&mut self) -> Option<(u64, u32, u32, u32, u64)> {
        let header = unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) };
        let frame_id = header.d3d11_frame_id.load(Ordering::Acquire);
        if frame_id == self.last_d3d11_frame_id {
            return None;
        }
        self.last_d3d11_frame_id = frame_id;

        let handle = header.d3d11_handle.load(Ordering::Acquire);
        let width = header.accelerated_width.load(Ordering::Acquire);
        let height = header.accelerated_height.load(Ordering::Acquire);
        let format = header.accelerated_format.load(Ordering::Acquire);
        let fence_value = header.d3d11_fence_value.load(Ordering::Acquire);
        if handle == 0 || width == 0 || height == 0 {
            return None;
        }
        Some((handle, width, height, format, fence_value))
    }

    /// Read IOSurface info from the shared memory header.
    /// Returns Some((surface_id, width, height, format)) if a new accelerated frame is available.
    pub fn get_iosurface_info(&mut self) -> Option<(u32, u32, u32, u32)> {
        let header = unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) };
        let frame_id = header.accelerated_frame_id.load(Ordering::Acquire);
        if frame_id == self.last_accelerated_frame_id {
            return None;
        }
        self.last_accelerated_frame_id = frame_id;

        let surface_id = header.accelerated_surface_id.load(Ordering::Acquire);
        let width = header.accelerated_width.load(Ordering::Acquire);
        let height = header.accelerated_height.load(Ordering::Acquire);
        let format = header.accelerated_format.load(Ordering::Acquire);
        if surface_id == 0 || width == 0 || height == 0 {
            return None;
        }
        Some((surface_id, width, height, format))
    }

    /// accel paint の寸法をフレーム消費なしで読む (テスト/診断用)。未 paint なら (0, 0)。
    pub fn read_accelerated_dimensions(&self) -> (u32, u32) {
        let header = unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) };
        (
            header.accelerated_width.load(Ordering::Acquire),
            header.accelerated_height.load(Ordering::Acquire),
        )
    }

    /// 最後の paint が対応する Unity frame 番号 (Time.frameCount) を読む。
    /// Unity 側は current frame との差で end-to-end の遅延フレーム数を計算する。
    pub fn read_paint_unity_frame(&self) -> u64 {
        let header = unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) };
        header.accelerated_paint_unity_frame.load(Ordering::Acquire)
    }

    /// accelerated paint の単調増加カウンタ (accelerated_frame_id) を消費せずに覗き見る。
    /// server は on_accelerated_paint ごとに、IOSurface の Mach 送信が完了した**後**に
    /// このカウンタを +1 する。したがって「このカウンタが進んだ」ことを観測できれば、
    /// 対応する IOSurface の Mach メッセージは既にクライアントの受信ポートに enqueue 済みで、
    /// 次の drain で確実に取得できる。double-pump で flush 後の新規 paint を待つ同期に使う。
    pub fn peek_accelerated_frame_id(&self) -> u64 {
        let header = unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) };
        header.accelerated_frame_id.load(Ordering::Acquire)
    }

    /// Read IME caret rect from the shared memory header.
    pub fn read_ime_caret(&self) -> (i32, i32, i32, i32) {
        let header = unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) };
        (
            header.ime_caret_x.load(Ordering::Acquire),
            header.ime_caret_y.load(Ordering::Acquire),
            header.ime_caret_width.load(Ordering::Acquire),
            header.ime_caret_height.load(Ordering::Acquire),
        )
    }

    /// Zero-copy variant: returns a raw pointer into the shared memory active buffer
    /// instead of copying. Returns None if no new frame since last call.
    /// The pointer is valid as long as this SharedMemoryReader (and its underlying Shmem) is alive.
    pub fn get_active_buffer_pointer(&mut self) -> Option<(*const u8, u32, u32)> {
        let header = unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) };
        let frame_id = header.frame_id.load(Ordering::Acquire);
        if frame_id == self.last_frame_id {
            return None;
        }
        self.last_frame_id = frame_id;

        let active = header.active_buffer.load(Ordering::Acquire);
        let width = header.width.load(Ordering::Acquire);
        let height = header.height.load(Ordering::Acquire);
        // u32 のまま乗算すると wrap して境界チェックをすり抜けるため、次元を先に検証する
        // (MAX_WIDTH×MAX_HEIGHT×4 == BUFFER_SIZE なので次元が正なら size は常に収まる)
        if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
            return None;
        }
        let offset = SHARED_MEMORY_HEADER_SIZE + active as usize * BUFFER_SIZE;
        let pointer = unsafe { self.shared_memory.as_ptr().add(offset) as *const u8 };
        Some((pointer, width, height))
    }

    /// Check if there's a new frame. If so, copy it into `destination` and return (width, height).
    /// Returns None if no new frame since last call.
    pub fn read_frame(&mut self, destination: &mut Vec<u8>) -> Option<(u32, u32)> {
        let header = unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) };
        let frame_id = header.frame_id.load(Ordering::Acquire);
        if frame_id == self.last_frame_id {
            return None;
        }
        self.last_frame_id = frame_id;

        let active = header.active_buffer.load(Ordering::Acquire);
        let width = header.width.load(Ordering::Acquire);
        let height = header.height.load(Ordering::Acquire);
        // u32 のまま乗算すると wrap して境界チェックをすり抜けるため、次元を先に検証する
        // (MAX_WIDTH×MAX_HEIGHT×4 == BUFFER_SIZE なので次元が正なら size は常に収まる)
        if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
            return None;
        }
        let size = (width as usize) * (height as usize) * 4;

        let offset = SHARED_MEMORY_HEADER_SIZE + active as usize * BUFFER_SIZE;
        destination.resize(size, 0);
        unsafe {
            let source = self.shared_memory.as_ptr().add(offset);
            std::ptr::copy_nonoverlapping(source, destination.as_mut_ptr(), size);
        }
        Some((width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipc_channel::ipc;

    #[test]
    fn shared_memory_write_read_roundtrip() {
        let flink = std::env::temp_dir()
            .join("cef-unity-test-shm-roundtrip")
            .to_str()
            .unwrap()
            .to_string();

        let writer = SharedMemoryWriter::new(&flink).expect("SharedMemoryWriter::new");
        let mut reader = SharedMemoryReader::open(&flink).expect("SharedMemoryReader::open");

        // No frame yet
        let mut buffer = Vec::new();
        assert!(reader.read_frame(&mut buffer).is_none());

        // Write a 2x2 BGRA frame (16 bytes)
        let pixels: Vec<u8> = (0..16).collect();
        writer.write_frame(&pixels, 2, 2);

        // Read it back
        let result = reader.read_frame(&mut buffer);
        assert_eq!(result, Some((2, 2)));
        assert_eq!(buffer, pixels);

        // No new frame if we read again
        assert!(reader.read_frame(&mut buffer).is_none());

        // Write another frame, verify double-buffer swap
        let pixels2: Vec<u8> = (16..32).collect();
        writer.write_frame(&pixels2, 2, 2);
        let result2 = reader.read_frame(&mut buffer);
        assert_eq!(result2, Some((2, 2)));
        assert_eq!(buffer, pixels2);
    }

    #[test]
    fn shared_memory_zero_copy_read() {
        let flink = std::env::temp_dir()
            .join("cef-unity-test-shm-zerocopy")
            .to_str()
            .unwrap()
            .to_string();

        let writer = SharedMemoryWriter::new(&flink).expect("SharedMemoryWriter::new");
        let mut reader = SharedMemoryReader::open(&flink).expect("SharedMemoryReader::open");

        // No frame yet
        assert!(reader.get_active_buffer_pointer().is_none());

        // Write a 2x2 BGRA frame (16 bytes)
        let pixels: Vec<u8> = (0..16).collect();
        writer.write_frame(&pixels, 2, 2);

        // Zero-copy read: pointer should point into shared memory with correct data
        let result = reader.get_active_buffer_pointer();
        assert!(result.is_some());
        let (pointer, width, height) = result.unwrap();
        assert_eq!(width, 2);
        assert_eq!(height, 2);
        let slice = unsafe { std::slice::from_raw_parts(pointer, 16) };
        assert_eq!(slice, &pixels[..]);

        // No new frame on second call
        assert!(reader.get_active_buffer_pointer().is_none());

        // Write another frame, verify double-buffer swap
        let pixels2: Vec<u8> = (16..32).collect();
        writer.write_frame(&pixels2, 2, 2);
        let (pointer_2, width_2, height_2) = reader.get_active_buffer_pointer().unwrap();
        assert_eq!(width_2, 2);
        assert_eq!(height_2, 2);
        let slice2 = unsafe { std::slice::from_raw_parts(pointer_2, 16) };
        assert_eq!(slice2, &pixels2[..]);
    }

    #[test]
    fn ipc_channel_command_roundtrip() {
        let (sender, receiver) = ipc::channel::<Command>().unwrap();
        sender.send(Command::CreateBrowser {
            width: 800,
            height: 600,
            url: "https://example.com".to_string(),
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        match command {
            Command::CreateBrowser { width, height, url } => {
                assert_eq!(width, 800);
                assert_eq!(height, 600);
                assert_eq!(url, "https://example.com");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn ipc_channel_response_roundtrip() {
        let (sender, receiver) = ipc::channel::<Response>().unwrap();
        sender.send(Response::BrowserCreated {
            browser_id: 42,
            shared_memory_flink: "/tmp/test-shm".to_string(),
            d3d11_fence_handle: 0xdeadbeef,
            audio_shared_memory_flink: "/tmp/test-audio-shm".to_string(),
        })
        .unwrap();
        let response = receiver.recv().unwrap();
        match response {
            Response::BrowserCreated {
                browser_id,
                shared_memory_flink,
                d3d11_fence_handle,
                audio_shared_memory_flink,
            } => {
                assert_eq!(browser_id, 42);
                assert_eq!(shared_memory_flink, "/tmp/test-shm");
                assert_eq!(d3d11_fence_handle, 0xdeadbeef);
                assert_eq!(audio_shared_memory_flink, "/tmp/test-audio-shm");
            }
            _ => panic!("unexpected response variant"),
        }
    }

    #[test]
    fn audio_shared_memory_write_read_roundtrip() {
        let flink = std::env::temp_dir()
            .join("cef-unity-test-audio-roundtrip")
            .to_str()
            .unwrap()
            .to_string();

        let writer = AudioSharedMemoryWriter::new(&flink).expect("AudioSharedMemoryWriter::new");
        let mut reader = AudioSharedMemoryReader::open(&flink).expect("AudioSharedMemoryReader::open");

        // 開始前は何も読めない
        let mut buffer = vec![0.0f32; 1024];
        assert_eq!(reader.read(&mut buffer, 256), (0, 0));

        // stereo 48kHz で開始
        writer.start_stream(48_000, 2);
        let (sample_rate, channels, active) = reader.format();
        assert_eq!(sample_rate, 48_000);
        assert_eq!(channels, 2);
        assert!(active);

        // 4 フレームの planar データ: L=[0,1,2,3], R=[10,11,12,13]
        let left = [0.0f32, 1.0, 2.0, 3.0];
        let right = [10.0f32, 11.0, 12.0, 13.0];
        let planes: [*const f32; 2] = [left.as_ptr(), right.as_ptr()];
        unsafe {
            writer.write_packet(planes.as_ptr(), 4, 2);
        }

        // interleaved で読み戻す: L0 R0 L1 R1 ...
        let (frames, channels) = reader.read(&mut buffer, 256);
        assert_eq!(frames, 4);
        assert_eq!(channels, 2);
        assert_eq!(&buffer[..8], &[0.0, 10.0, 1.0, 11.0, 2.0, 12.0, 3.0, 13.0]);

        // 再読み出しでは新規なし
        assert_eq!(reader.read(&mut buffer, 256), (0, 2));
    }

    #[test]
    fn key_event_backspace_roundtrip() {
        // Backspace on macOS: VK_BACK=0x08, native=51 (kVK_Delete),
        // character=0x7F (NSDeleteCharacter)
        let (sender, receiver) = ipc::channel::<Command>().unwrap();
        sender.send(Command::KeyEvent {
            browser_id: 1,
            event_type: 0, // RAWKEYDOWN
            modifiers: 0,
            windows_key_code: 0x08,
            native_key_code: 51,
            character: 0x7F,
            unmodified_character: 0x7F,
            is_system_key: 0,
            focus_on_editable_field: 0,
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        match command {
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
            } => {
                assert_eq!(browser_id, 1);
                assert_eq!(event_type, 0); // RAWKEYDOWN
                assert_eq!(modifiers, 0);
                assert_eq!(windows_key_code, 0x08); // VK_BACK
                assert_eq!(native_key_code, 51); // macOS kVK_Delete
                assert_eq!(character, 0x7F); // NSDeleteCharacter
                assert_eq!(unmodified_character, 0x7F);
                assert_eq!(is_system_key, 0);
                assert_eq!(focus_on_editable_field, 0);
            }
            _ => panic!("expected KeyEvent"),
        }
    }

    #[test]
    fn key_event_printable_char_roundtrip() {
        // Typing 'a': VK_A=0x41, native=0 (macOS keycode), char='a'=0x61
        let (sender, receiver) = ipc::channel::<Command>().unwrap();

        // RAWKEYDOWN
        sender.send(Command::KeyEvent {
            browser_id: 1,
            event_type: 0,
            modifiers: 0,
            windows_key_code: 0x41,
            native_key_code: 0,
            character: 0x61, // 'a'
            unmodified_character: 0x61,
            is_system_key: 0,
            focus_on_editable_field: 0,
        })
        .unwrap();

        // CHAR
        sender.send(Command::KeyEvent {
            browser_id: 1,
            event_type: 2, // CHAR
            modifiers: 0,
            windows_key_code: 0x61, // 'a' as key code for CHAR event
            native_key_code: 0,
            character: 0x61,
            unmodified_character: 0x61,
            is_system_key: 0,
            focus_on_editable_field: 0,
        })
        .unwrap();

        // KEYUP
        sender.send(Command::KeyEvent {
            browser_id: 1,
            event_type: 1, // KEYUP
            modifiers: 0,
            windows_key_code: 0x41,
            native_key_code: 0,
            character: 0x61,
            unmodified_character: 0x61,
            is_system_key: 0,
            focus_on_editable_field: 0,
        })
        .unwrap();

        // Verify all three events
        let command_1 = receiver.recv().unwrap();
        assert!(matches!(command_1, Command::KeyEvent { event_type: 0, .. }));

        let command_2 = receiver.recv().unwrap();
        assert!(matches!(command_2, Command::KeyEvent { event_type: 2, .. }));

        let command_3 = receiver.recv().unwrap();
        assert!(matches!(command_3, Command::KeyEvent { event_type: 1, .. }));
    }

    #[test]
    fn command_envelope_roundtrip() {
        let (sender, receiver) = ipc::channel::<CommandEnvelope>().unwrap();
        sender.send(CommandEnvelope {
            command: Command::Resize {
                browser_id: 1,
                width: 1920,
                height: 1080,
            },
            expects_response: false,
        })
        .unwrap();
        let envelope = receiver.recv().unwrap();
        assert!(!envelope.expects_response);
        assert!(matches!(
            envelope.command,
            Command::Resize {
                browser_id: 1,
                width: 1920,
                height: 1080,
            }
        ));

        sender.send(CommandEnvelope {
            command: Command::Resize {
                browser_id: 1,
                width: 800,
                height: 600,
            },
            expects_response: true,
        })
        .unwrap();
        let envelope_2 = receiver.recv().unwrap();
        assert!(envelope_2.expects_response);
    }

    #[test]
    fn key_event_special_keys_values() {
        // Verify correct values for common special keys on macOS
        struct SpecialKey {
            name: &'static str,
            virtual_key: i32,
            native: i32,
            character: u16,
        }

        let keys = [
            SpecialKey {
                name: "Backspace",
                virtual_key: 0x08,
                native: 51,
                character: 0x7F,
            },
            SpecialKey {
                name: "Tab",
                virtual_key: 0x09,
                native: 48,
                character: 0x09,
            },
            SpecialKey {
                name: "Return",
                virtual_key: 0x0D,
                native: 36,
                character: 0x0D,
            },
            SpecialKey {
                name: "Escape",
                virtual_key: 0x1B,
                native: 53,
                character: 0x1B,
            },
            SpecialKey {
                name: "Delete",
                virtual_key: 0x2E,
                native: 117,
                character: 0xF728,
            },
            SpecialKey {
                name: "UpArrow",
                virtual_key: 0x26,
                native: 126,
                character: 0xF700,
            },
            SpecialKey {
                name: "DownArrow",
                virtual_key: 0x28,
                native: 125,
                character: 0xF701,
            },
            SpecialKey {
                name: "LeftArrow",
                virtual_key: 0x25,
                native: 123,
                character: 0xF702,
            },
            SpecialKey {
                name: "RightArrow",
                virtual_key: 0x27,
                native: 124,
                character: 0xF703,
            },
        ];

        let (sender, receiver) = ipc::channel::<Command>().unwrap();

        for key in &keys {
            sender.send(Command::KeyEvent {
                browser_id: 1,
                event_type: 0,
                modifiers: 0,
                windows_key_code: key.virtual_key,
                native_key_code: key.native,
                character: key.character,
                unmodified_character: key.character,
                is_system_key: 0,
                focus_on_editable_field: 0,
            })
            .unwrap();

            let command = receiver.recv().unwrap();
            match command {
                Command::KeyEvent {
                    windows_key_code,
                    native_key_code,
                    character,
                    ..
                } => {
                    assert_eq!(windows_key_code, key.virtual_key, "{} VK mismatch", key.name);
                    assert_eq!(native_key_code, key.native, "{} native mismatch", key.name);
                    assert_eq!(character, key.character, "{} character mismatch", key.name);
                }
                _ => panic!("expected KeyEvent for {}", key.name),
            }
        }
    }

    #[test]
    fn ime_set_composition_roundtrip() {
        let (sender, receiver) = ipc::channel::<Command>().unwrap();
        sender.send(Command::ImeSetComposition {
            browser_id: 1,
            text: "かな".to_string(),
            selection_start: 0,
            selection_end: 2,
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        match command {
            Command::ImeSetComposition {
                browser_id,
                text,
                selection_start,
                selection_end,
            } => {
                assert_eq!(browser_id, 1);
                assert_eq!(text, "かな");
                assert_eq!(selection_start, 0);
                assert_eq!(selection_end, 2);
            }
            _ => panic!("expected ImeSetComposition"),
        }
    }

    #[test]
    fn ime_commit_text_roundtrip() {
        let (sender, receiver) = ipc::channel::<Command>().unwrap();
        sender.send(Command::ImeCommitText {
            browser_id: 1,
            text: "漢字".to_string(),
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        match command {
            Command::ImeCommitText { browser_id, text } => {
                assert_eq!(browser_id, 1);
                assert_eq!(text, "漢字");
            }
            _ => panic!("expected ImeCommitText"),
        }
    }

    #[test]
    fn ime_finish_composing_text_roundtrip() {
        let (sender, receiver) = ipc::channel::<Command>().unwrap();

        // keep_selection = false
        sender.send(Command::ImeFinishComposingText {
            browser_id: 1,
            keep_selection: false,
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        match command {
            Command::ImeFinishComposingText {
                browser_id,
                keep_selection,
            } => {
                assert_eq!(browser_id, 1);
                assert!(!keep_selection);
            }
            _ => panic!("expected ImeFinishComposingText"),
        }

        // keep_selection = true
        sender.send(Command::ImeFinishComposingText {
            browser_id: 2,
            keep_selection: true,
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        match command {
            Command::ImeFinishComposingText {
                browser_id,
                keep_selection,
            } => {
                assert_eq!(browser_id, 2);
                assert!(keep_selection);
            }
            _ => panic!("expected ImeFinishComposingText"),
        }
    }

    #[test]
    fn ime_cancel_composition_roundtrip() {
        let (sender, receiver) = ipc::channel::<Command>().unwrap();
        sender.send(Command::ImeCancelComposition { browser_id: 1 })
            .unwrap();
        let command = receiver.recv().unwrap();
        match command {
            Command::ImeCancelComposition { browser_id } => {
                assert_eq!(browser_id, 1);
            }
            _ => panic!("expected ImeCancelComposition"),
        }
    }

    #[test]
    fn ime_full_composition_workflow() {
        // 典型的なIMEワークフロー: SetComposition → (複数回更新) → CommitText
        let (sender, receiver) = ipc::channel::<Command>().unwrap();

        // 1. ユーザーが「か」を入力 → コンポジション開始
        sender.send(Command::ImeSetComposition {
            browser_id: 1,
            text: "か".to_string(),
            selection_start: 0,
            selection_end: 1,
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        assert!(matches!(
            command,
            Command::ImeSetComposition {
                text,
                selection_end: 1,
                ..
            } if text == "か"
        ));

        // 2. 「かん」に更新
        sender.send(Command::ImeSetComposition {
            browser_id: 1,
            text: "かん".to_string(),
            selection_start: 0,
            selection_end: 2,
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        assert!(matches!(
            command,
            Command::ImeSetComposition {
                text,
                selection_end: 2,
                ..
            } if text == "かん"
        ));

        // 3. 「かんじ」に更新
        sender.send(Command::ImeSetComposition {
            browser_id: 1,
            text: "かんじ".to_string(),
            selection_start: 0,
            selection_end: 3,
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        assert!(matches!(
            command,
            Command::ImeSetComposition {
                text,
                selection_end: 3,
                ..
            } if text == "かんじ"
        ));

        // 4. 変換確定 → 「漢字」
        sender.send(Command::ImeCommitText {
            browser_id: 1,
            text: "漢字".to_string(),
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        assert!(matches!(
            command,
            Command::ImeCommitText { text, .. } if text == "漢字"
        ));
    }

    #[test]
    fn ime_composition_cancel_workflow() {
        // コンポジション開始後にキャンセルするワークフロー
        let (sender, receiver) = ipc::channel::<Command>().unwrap();

        // 1. コンポジション開始
        sender.send(Command::ImeSetComposition {
            browser_id: 1,
            text: "あい".to_string(),
            selection_start: 0,
            selection_end: 2,
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        assert!(matches!(command, Command::ImeSetComposition { .. }));

        // 2. キャンセル
        sender.send(Command::ImeCancelComposition { browser_id: 1 })
            .unwrap();
        let command = receiver.recv().unwrap();
        assert!(matches!(
            command,
            Command::ImeCancelComposition { browser_id: 1 }
        ));
    }

    #[test]
    fn ime_set_composition_empty_text() {
        let (sender, receiver) = ipc::channel::<Command>().unwrap();
        sender.send(Command::ImeSetComposition {
            browser_id: 1,
            text: "".to_string(),
            selection_start: 0,
            selection_end: 0,
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        match command {
            Command::ImeSetComposition {
                text,
                selection_start,
                selection_end,
                ..
            } => {
                assert!(text.is_empty());
                assert_eq!(selection_start, 0);
                assert_eq!(selection_end, 0);
            }
            _ => panic!("expected ImeSetComposition"),
        }
    }

    #[test]
    fn ime_commit_text_emoji() {
        // 絵文字（サロゲートペアを含むUTF-8）のテスト
        let (sender, receiver) = ipc::channel::<Command>().unwrap();
        sender.send(Command::ImeCommitText {
            browser_id: 1,
            text: "🎉".to_string(),
        })
        .unwrap();
        let command = receiver.recv().unwrap();
        match command {
            Command::ImeCommitText { text, .. } => {
                assert_eq!(text, "🎉");
            }
            _ => panic!("expected ImeCommitText"),
        }
    }

    #[test]
    fn ime_envelope_no_response() {
        // IMEコマンドは通常 expects_response=false で送信される
        let (sender, receiver) = ipc::channel::<CommandEnvelope>().unwrap();
        sender.send(CommandEnvelope {
            command: Command::ImeCommitText {
                browser_id: 1,
                text: "テスト".to_string(),
            },
            expects_response: false,
        })
        .unwrap();
        let envelope = receiver.recv().unwrap();
        assert!(!envelope.expects_response);
        assert!(matches!(
            envelope.command,
            Command::ImeCommitText { browser_id: 1, .. }
        ));
    }

    #[test]
    fn ipc_bootstrap_roundtrip() {
        // IpcOneShotServer bootstrap requires separate processes on macOS (Mach ports).
        // Here we test the channel transfer pattern using regular IPC channels instead.
        let (bootstrap_sender, bootstrap_receiver) = ipc::channel::<Bootstrap>().unwrap();

        let (command_sender, command_receiver) = ipc::channel::<CommandEnvelope>().unwrap();
        let (response_sender, response_receiver) = ipc::channel::<Response>().unwrap();

        // Simulate server sending bootstrap
        bootstrap_sender.send(Bootstrap { command_sender, response_receiver, server_pid: 12345 }).unwrap();

        // Client receives bootstrap
        let bootstrap = bootstrap_receiver.recv().unwrap();

        // Send a command through the bootstrapped channel
        bootstrap
            .command_sender
            .send(CommandEnvelope {
                command: Command::Shutdown,
                expects_response: true,
            })
            .unwrap();
        let envelope = command_receiver.recv().unwrap();
        assert!(matches!(envelope.command, Command::Shutdown));
        assert!(envelope.expects_response);

        // Send a response back
        response_sender.send(Response::Ok).unwrap();
        let response = bootstrap.response_receiver.recv().unwrap();
        assert!(matches!(response, Response::Ok));
    }
}
