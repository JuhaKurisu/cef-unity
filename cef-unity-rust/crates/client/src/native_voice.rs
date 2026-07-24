//! CRI 方式ネイティブ音声出力。
//!
//! 自前の AudioSharedMemoryReader (独立カーソル) + steering つきローカルリング + AudioUnit
//! (au_output.c) で、Unity FMOD ミキサ (dspBuf×numBuffers) を完全に迂回して再生する。
//! SHM ドレインと補間出力は全て AU render callback スレッド内 = 単一スレッドで、
//! ロック・アロケーションなし。
//!
//! read カーソルは AudioSharedMemoryReader のローカルフィールドなので、既存の
//! cef_unity_read_audio (録画 tap) とは独立カーソルで同時使用できる。
//! CefAudioOutput (Unity ミキサ再生) と両方 ON にすると二重再生になる点に注意。

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cef_unity_ipc::{AUDIO_MAX_CHANNELS, AudioSharedMemoryReader};

use crate::au_output;
use crate::audio_ring::AudioRing;

/// 1 回の SHM read で取り込む最大フレーム数 (scratch のフレーム容量)。
/// CEF パケットは 1024 フレーム単位なので通常 1 回で全量ドレインできる。
const SCRATCH_FRAMES: usize = 4096;
/// ローカルリング容量 (秒)。オーバーフローのバックストップ。
const RING_CAPACITY_SECONDS: f64 = 0.25;

/// 診断カウンタ。callback スレッドが書き、メインスレッド (statistics FFI) が読む。
struct VoiceStatistics {
    occupancy_frames: AtomicU64,
    underrun_frames: AtomicU64,
    overflow_frames: AtomicU64,
}

/// AU render callback から参照されるデータ一式。
/// Box で heap 上に固定し、audio_unit_output_stop が返るまで移動も解放もしない。
struct PullContext {
    reader: AudioSharedMemoryReader,
    ring: AudioRing,
    scratch: Vec<f32>,
    statistics: Arc<VoiceStatistics>,
    channels: usize,
}

impl PullContext {
    /// callback 本体 (AU 非依存でテスト可能な形に分離)。
    /// out[..frames*channels] を必ず埋める (データ不足は無音)。
    fn pull_into(&mut self, out: &mut [f32], frames: usize) {
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

        // レート変換は AU 内蔵コンバータが行うので baseStep=1.0。
        // steering はクロックドリフト (ppm) と滞留量制御のみ担当する。
        self.ring.read(out, frames, 1.0);

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

/// pull_trampoline 内で panic が起きた回数。edition 2024 では extern "C" 越しの
/// unwind が即 abort (= Unity ごと落ちる) なので、RT スレッドの将来バグは
/// 「無音 1 バッファ + このカウンタ」に留める。statistics FFI への露出は将来課題。
static PULL_PANIC_COUNT: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" fn pull_trampoline(context: *mut c_void, out: *mut f32, frames: i32) -> i32 {
    let context = context as *mut PullContext;
    let frames = frames as usize;
    let channels = unsafe { (*context).channels };
    let out = unsafe { std::slice::from_raw_parts_mut(out, frames * channels) };
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unsafe { &mut *context }.pull_into(out, frames);
    }))
    .is_err();
    if panicked {
        out.fill(0.0);
        PULL_PANIC_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    frames as i32
}

pub struct NativeVoice {
    /// AU callback が参照する。Drop の audio_unit_output_stop が返るまで解放してはならない。
    /// Box のまま持つと「AU スレッドへ渡した生ポインタ」と Box の noalias 主張が
    /// 衝突する (Stacked Borrows の aliasing UB) ため、raw で所有する。
    context: *mut PullContext,
    audio_unit: *mut c_void,
    statistics: Arc<VoiceStatistics>,
    source_rate: u32,
}

impl NativeVoice {
    /// SHM flink から自前リーダーを開いて AU 再生を開始する。
    /// ストリームフォーマット未確定 (sample_rate/channels が 0) なら Err —
    /// 呼び出し側は cef_unity_get_audio_format が 1 を返してから呼ぶこと。
    pub fn start(flink: &str, target_milliseconds: f32, io_frames: i32) -> Result<NativeVoice, String> {
        let (context, statistics, source_rate) = Self::prepare(flink, target_milliseconds)?;
        let channels = context.channels;
        let io_frames = if io_frames > 0 { io_frames } else { 128 };
        let context = Box::into_raw(context);
        let audio_unit = unsafe {
            au_output::audio_unit_output_start(
                source_rate as f64,
                channels as i32,
                io_frames,
                pull_trampoline,
                context as *mut c_void,
            )
        };
        if audio_unit.is_null() {
            // 起動失敗 = callback は未登録なので即回収してよい
            drop(unsafe { Box::from_raw(context) });
            return Err("audio_unit_output_start failed".into());
        }
        Ok(NativeVoice {
            context,
            audio_unit,
            statistics,
            source_rate,
        })
    }

    /// AU 起動を除いた初期化 (テストからも使う)。
    fn prepare(
        flink: &str,
        target_milliseconds: f32,
    ) -> Result<(Box<PullContext>, Arc<VoiceStatistics>, u32), String> {
        let reader = AudioSharedMemoryReader::open(flink).map_err(|error| format!("audio shm open: {}", error))?;
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

    pub fn set_volume(&self, volume: f32) {
        unsafe { au_output::audio_unit_output_set_volume(self.audio_unit, volume) };
    }

    /// (滞留量 ms, 累積アンダーランフレーム, 累積オーバーフローフレーム)。
    pub fn statistics(&self) -> (f32, u64, u64) {
        let occupancy = self.statistics.occupancy_frames.load(Ordering::Relaxed);
        let occupancy_milliseconds = if self.source_rate > 0 {
            occupancy as f32 / self.source_rate as f32 * 1000.0
        } else {
            0.0
        };
        (
            occupancy_milliseconds,
            self.statistics.underrun_frames.load(Ordering::Relaxed),
            self.statistics.overflow_frames.load(Ordering::Relaxed),
        )
    }
}

impl Drop for NativeVoice {
    fn drop(&mut self) {
        // 排水待ち付き stop。返った後は callback が走らないので context を安全に解放できる。
        unsafe { au_output::audio_unit_output_stop(self.audio_unit) };
        drop(unsafe { Box::from_raw(self.context) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cef_unity_ipc::AudioSharedMemoryWriter;

    // テストごとに一意な flink (プロセス ID + タグ)。
    fn temporary_flink(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("cef-unity-audio-test-{}-{}", std::process::id(), tag))
            .to_str()
            .unwrap()
            .to_string()
    }

    // planar packet (全サンプル同値, 2ch) を書き込む。
    fn write_constant_packet(writer: &AudioSharedMemoryWriter, frames: usize, value: f32) {
        let plane = vec![value; frames];
        let planes = [plane.as_ptr(), plane.as_ptr()];
        unsafe { writer.write_packet(planes.as_ptr(), frames, 2) };
    }

    #[test]
    fn prepare_fails_before_stream_start() {
        let flink = temporary_flink("noformat");
        let _writer = AudioSharedMemoryWriter::new(&flink).unwrap();
        // start_stream 前 = フォーマット未確定 → Err
        assert!(NativeVoice::prepare(&flink, 15.0).is_err());
    }

    #[test]
    fn pull_primes_then_outputs_shm_data() {
        let flink = temporary_flink("basic");
        let writer = AudioSharedMemoryWriter::new(&flink).unwrap();
        writer.start_stream(48000, 2);

        let (mut context, statistics, rate) = NativeVoice::prepare(&flink, 15.0).unwrap();
        assert_eq!(rate, 48000);
        assert_eq!(context.channels, 2);

        // target=15ms@48k=720 フレーム。1024 フレーム書けばプライミング完了できる。
        write_constant_packet(&writer, 1024, 0.5);

        let mut out = vec![9.9f32; 128 * 2];
        context.pull_into(&mut out, 128);

        // 1 回目の pull で SHM 全量 (1024) がリングへ入り (>720)、プライミング完了して
        // データが出る。
        assert!(
            out.iter().any(|&sample| (sample - 0.5).abs() < 1e-6),
            "プライミング完了後は SHM のデータが出力されるべき: {:?}",
            &out[..8]
        );
        assert!(statistics.occupancy_frames.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn pull_before_priming_outputs_silence() {
        let flink = temporary_flink("prime");
        let writer = AudioSharedMemoryWriter::new(&flink).unwrap();
        writer.start_stream(48000, 2);

        let (mut context, statistics, _) = NativeVoice::prepare(&flink, 15.0).unwrap();

        // target(720) 未満しか書かない → まだ無音のはず。
        write_constant_packet(&writer, 256, 0.5);

        let mut out = vec![9.9f32; 128 * 2];
        context.pull_into(&mut out, 128);

        assert!(
            out.iter().all(|&sample| sample == 0.0),
            "プライミング前は無音であるべき"
        );
        assert_eq!(statistics.underrun_frames.load(Ordering::Relaxed), 128);
    }

    #[test]
    fn pull_on_channel_change_outputs_silence() {
        let flink = temporary_flink("chchange");
        let writer = AudioSharedMemoryWriter::new(&flink).unwrap();
        writer.start_stream(48000, 2);

        let (mut context, _statistics, _) = NativeVoice::prepare(&flink, 15.0).unwrap();
        write_constant_packet(&writer, 1024, 0.5);

        // チャネル数が変わった (2→1) → 再起動は C# 側の責務。native は無音を出す。
        writer.start_stream(48000, 1);

        let mut out = vec![9.9f32; 128 * 2];
        context.pull_into(&mut out, 128);
        assert!(
            out.iter().all(|&sample| sample == 0.0),
            "チャネル数変化中は無音であるべき"
        );
    }

    #[test]
    fn pull_drains_shared_memory_across_multiple_scratch_reads() {
        let flink = temporary_flink("drain");
        let writer = AudioSharedMemoryWriter::new(&flink).unwrap();
        writer.start_stream(48000, 2);

        let (mut context, statistics, _) = NativeVoice::prepare(&flink, 15.0).unwrap();

        // SCRATCH_FRAMES(4096) を超える量を書く → ループで複数回 read して全量ドレイン。
        write_constant_packet(&writer, 4096, 0.5);
        write_constant_packet(&writer, 4096, 0.5);
        write_constant_packet(&writer, 1000, 0.5);

        let mut out = vec![0.0f32; 128 * 2];
        context.pull_into(&mut out, 128);

        // 全量 (9192) がリングへ移っている: occupancy = 9192 - 消費分。
        // 消費は steering 上限で 1 フレームあたり最大 1.02 (max_rate_adjust=0.02) なので
        // 128 フレーム出力での消費は最大 ~131。
        let occupancy = statistics.occupancy_frames.load(Ordering::Relaxed);
        assert!(
            occupancy >= 9192 - 132 && occupancy <= 9192,
            "SHM 全量がドレインされるべき: occupancy={}",
            occupancy
        );
    }
}
