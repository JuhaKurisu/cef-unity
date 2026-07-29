//! ネイティブ音声出力の「SHM ドレイン → ローカルリング → 出力バッファ充填」ロジック。
//!
//! macOS (AudioUnit) と Windows (WASAPI) で共通。出力デバイス層だけが
//! プラットフォーム依存で、この取り込み・steering・診断カウンタの扱いは同一なので
//! ここに一本化する (複製すると片方だけ直して挙動が分岐する)。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cef_unity_ipc::{AUDIO_MAX_CHANNELS, AudioSharedMemoryReader};

use crate::audio_ring::AudioRing;

/// 1 回の SHM read で取り込む最大フレーム数 (scratch のフレーム容量)。
/// CEF パケットは 1024 フレーム単位なので通常 1 回で全量ドレインできる。
pub const SCRATCH_FRAMES: usize = 4096;
/// ローカルリング容量 (秒)。オーバーフローのバックストップ。
pub const RING_CAPACITY_SECONDS: f64 = 0.25;

/// 診断カウンタ。出力スレッドが書き、メインスレッド (statistics FFI) が読む。
pub struct VoiceStatistics {
    pub occupancy_frames: AtomicU64,
    pub underrun_frames: AtomicU64,
    pub overflow_frames: AtomicU64,
}

/// 出力コールバックから参照されるデータ一式。
pub struct PullContext {
    pub reader: AudioSharedMemoryReader,
    pub ring: AudioRing,
    pub scratch: Vec<f32>,
    pub statistics: Arc<VoiceStatistics>,
    pub channels: usize,
}

impl PullContext {
    /// コールバック本体 (出力デバイス非依存でテスト可能な形)。
    /// `out[..frames*channels]` を必ず埋める (データ不足は無音)。
    ///
    /// `base_step` はリング読み出しのレート比。出力デバイス側がレート変換を
    /// 行う場合 (macOS の AU 内蔵コンバータ) は 1.0 を渡す。
    pub fn pull_into(&mut self, out: &mut [f32], frames: usize, base_step: f64) {
        // フォーマット変化チェック: チャネル数が変わったら無音を出して待つ
        // (リング再構築 = 再起動は C# 側 CefNativeAudio の責務)。
        let (_, channels, _) = self.reader.format();
        if channels as usize != self.channels {
            out[..frames * self.channels].fill(0.0);
            return;
        }

        // SHM を全量ドレイン → ローカルリングへ。SHM リング自体が jitter buffer なので
        // 取り残すと滞留が二重になる。
        loop {
            let (got, _) = self.reader.read(&mut self.scratch, SCRATCH_FRAMES);
            if got == 0 {
                break;
            }
            self.ring.write(&self.scratch[..got * self.channels], got);
            if got < SCRATCH_FRAMES {
                break;
            }
        }

        self.ring.read(out, frames, base_step);

        self.statistics
            .occupancy_frames
            .store(self.ring.occupancy_frames().max(0.0) as u64, Ordering::Relaxed);
        self.statistics
            .underrun_frames
            .store(self.ring.underrun_frames, Ordering::Relaxed);
        self.statistics
            .overflow_frames
            .store(self.ring.overflow_drop_frames, Ordering::Relaxed);
    }
}

/// SHM flink を開いて PullContext を組み立てる (出力デバイスの起動は含まない)。
/// ストリームフォーマット未確定 (sample_rate/channels が 0) なら Err —
/// 呼び出し側は `cef_unity_get_audio_format` が 1 を返してから呼ぶこと。
///
/// 戻り値の 3 つ目はソースのサンプリングレート。
pub fn prepare(
    flink: &str,
    target_milliseconds: f32,
) -> Result<(Box<PullContext>, Arc<VoiceStatistics>, u32), String> {
    let reader =
        AudioSharedMemoryReader::open(flink).map_err(|error| format!("audio shm open: {}", error))?;
    let (rate, channels, _active) = reader.format();
    if rate == 0 || channels == 0 {
        return Err("audio stream format not ready".into());
    }
    let channels = (channels as usize).min(AUDIO_MAX_CHANNELS);
    let capacity = ((rate as f64 * RING_CAPACITY_SECONDS) as usize).max(2);
    let target = ((rate as f32 * target_milliseconds / 1000.0) as usize).clamp(1, capacity - 1);
    let statistics = Arc::new(VoiceStatistics {
        occupancy_frames: AtomicU64::new(0),
        underrun_frames: AtomicU64::new(0),
        overflow_frames: AtomicU64::new(0),
    });
    let context = Box::new(PullContext {
        reader,
        ring: AudioRing::new(capacity, channels, target, 0.02),
        scratch: vec![0.0; SCRATCH_FRAMES * AUDIO_MAX_CHANNELS],
        statistics: statistics.clone(),
        channels,
    });
    Ok((context, statistics, rate))
}

/// 滞留フレーム数を ms に直す (statistics FFI 用)。
pub fn occupancy_milliseconds(statistics: &VoiceStatistics, source_rate: u32) -> f32 {
    let occupancy = statistics.occupancy_frames.load(Ordering::Relaxed);
    if source_rate > 0 {
        occupancy as f32 / source_rate as f32 * 1000.0
    } else {
        0.0
    }
}
