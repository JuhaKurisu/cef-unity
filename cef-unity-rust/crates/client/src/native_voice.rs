//! CRI 方式ネイティブ音声出力。
//!
//! 自前の AudioShmReader (独立カーソル) + steering つきローカルリング + AudioUnit
//! (au_output.c) で、Unity FMOD ミキサ (dspBuf×numBuffers) を完全に迂回して再生する。
//! SHM ドレインと補間出力は全て AU render callback スレッド内 = 単一スレッドで、
//! ロック・アロケーションなし。
//!
//! read カーソルは AudioShmReader のローカルフィールドなので、既存の
//! cef_unity_read_audio (録画 tap) とは独立カーソルで同時使用できる。
//! CefAudioOutput (Unity ミキサ再生) と両方 ON にすると二重再生になる点に注意。

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cef_unity_ipc::{AUDIO_MAX_CHANNELS, AudioShmReader};

use crate::au_output;
use crate::audio_ring::AudioRing;

/// 1 回の SHM read で取り込む最大フレーム数 (scratch のフレーム容量)。
/// CEF パケットは 1024 フレーム単位なので通常 1 回で全量ドレインできる。
const SCRATCH_FRAMES: usize = 4096;
/// ローカルリング容量 (秒)。オーバーフローのバックストップ。
const RING_CAPACITY_SECONDS: f64 = 0.25;

/// 診断カウンタ。callback スレッドが書き、メインスレッド (stats FFI) が読む。
struct VoiceStats {
    occupancy_frames: AtomicU64,
    underrun_frames: AtomicU64,
    overflow_frames: AtomicU64,
}

/// AU render callback から参照されるデータ一式。
/// Box で heap 上に固定し、au_output_stop が返るまで移動も解放もしない。
struct PullCtx {
    reader: AudioShmReader,
    ring: AudioRing,
    scratch: Vec<f32>,
    stats: Arc<VoiceStats>,
    channels: usize,
}

impl PullCtx {
    /// callback 本体 (AU 非依存でテスト可能な形に分離)。
    /// out[..frames*channels] を必ず埋める (データ不足は無音)。
    fn pull_into(&mut self, out: &mut [f32], frames: usize) {
        // フォーマット変化チェック: チャネル数が変わったら無音を出して待つ
        // (リング再構築 = 再起動は C# 側 CefNativeAudio の責務)。
        let (_, ch, _) = self.reader.format();
        if ch as usize != self.channels {
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

        self.stats
            .occupancy_frames
            .store(self.ring.occupancy_frames().max(0.0) as u64, Ordering::Relaxed);
        self.stats
            .underrun_frames
            .store(self.ring.underrun_frames, Ordering::Relaxed);
        self.stats
            .overflow_frames
            .store(self.ring.overflow_drop_frames, Ordering::Relaxed);
    }
}

unsafe extern "C" fn pull_trampoline(ctx: *mut c_void, out: *mut f32, frames: i32) -> i32 {
    let ctx = unsafe { &mut *(ctx as *mut PullCtx) };
    let frames = frames as usize;
    let out = unsafe { std::slice::from_raw_parts_mut(out, frames * ctx.channels) };
    ctx.pull_into(out, frames);
    frames as i32
}

pub struct NativeVoice {
    /// AU callback が参照する。Drop の au_output_stop が返るまで解放してはならない。
    #[allow(dead_code)]
    ctx: Box<PullCtx>,
    au: *mut c_void,
    stats: Arc<VoiceStats>,
    src_rate: u32,
}

impl NativeVoice {
    /// SHM flink から自前リーダーを開いて AU 再生を開始する。
    /// ストリームフォーマット未確定 (sample_rate/channels が 0) なら Err —
    /// 呼び出し側は cef_unity_get_audio_format が 1 を返してから呼ぶこと。
    pub fn start(flink: &str, target_ms: f32, io_frames: i32) -> Result<NativeVoice, String> {
        let (mut ctx, stats, src_rate) = Self::prepare(flink, target_ms)?;
        let io_frames = if io_frames > 0 { io_frames } else { 128 };
        let au = unsafe {
            au_output::au_output_start(
                src_rate as f64,
                ctx.channels as i32,
                io_frames,
                pull_trampoline,
                &mut *ctx as *mut PullCtx as *mut c_void,
            )
        };
        if au.is_null() {
            return Err("au_output_start failed".into());
        }
        Ok(NativeVoice {
            ctx,
            au,
            stats,
            src_rate,
        })
    }

    /// AU 起動を除いた初期化 (テストからも使う)。
    fn prepare(
        flink: &str,
        target_ms: f32,
    ) -> Result<(Box<PullCtx>, Arc<VoiceStats>, u32), String> {
        let reader = AudioShmReader::open(flink).map_err(|e| format!("audio shm open: {}", e))?;
        let (rate, ch, _active) = reader.format();
        if rate == 0 || ch == 0 {
            return Err("audio stream format not ready".into());
        }
        let channels = (ch as usize).min(AUDIO_MAX_CHANNELS);
        let cap = ((rate as f64 * RING_CAPACITY_SECONDS) as usize).max(2);
        let target = ((rate as f32 * target_ms / 1000.0) as usize).clamp(1, cap - 1);
        let stats = Arc::new(VoiceStats {
            occupancy_frames: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            overflow_frames: AtomicU64::new(0),
        });
        let ctx = Box::new(PullCtx {
            reader,
            ring: AudioRing::new(cap, channels, target, 0.02),
            scratch: vec![0.0; SCRATCH_FRAMES * AUDIO_MAX_CHANNELS],
            stats: stats.clone(),
            channels,
        });
        Ok((ctx, stats, rate))
    }

    pub fn set_volume(&self, v: f32) {
        unsafe { au_output::au_output_set_volume(self.au, v) };
    }

    /// (滞留量 ms, 累積アンダーランフレーム, 累積オーバーフローフレーム)。
    pub fn stats(&self) -> (f32, u64, u64) {
        let occ = self.stats.occupancy_frames.load(Ordering::Relaxed);
        let occ_ms = if self.src_rate > 0 {
            occ as f32 / self.src_rate as f32 * 1000.0
        } else {
            0.0
        };
        (
            occ_ms,
            self.stats.underrun_frames.load(Ordering::Relaxed),
            self.stats.overflow_frames.load(Ordering::Relaxed),
        )
    }
}

impl Drop for NativeVoice {
    fn drop(&mut self) {
        // 排水待ち付き stop。返った後は callback が走らないので ctx を安全に解放できる。
        unsafe { au_output::au_output_stop(self.au) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cef_unity_ipc::AudioShmWriter;

    // テストごとに一意な flink (プロセス ID + タグ)。
    fn temp_flink(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("cef-unity-audio-test-{}-{}", std::process::id(), tag))
            .to_str()
            .unwrap()
            .to_string()
    }

    // planar packet (全サンプル同値, 2ch) を書き込む。
    fn write_const_packet(w: &AudioShmWriter, frames: usize, value: f32) {
        let plane = vec![value; frames];
        let planes = [plane.as_ptr(), plane.as_ptr()];
        unsafe { w.write_packet(planes.as_ptr(), frames, 2) };
    }

    #[test]
    fn prepare_fails_before_stream_start() {
        let flink = temp_flink("noformat");
        let _w = AudioShmWriter::new(&flink).unwrap();
        // start_stream 前 = フォーマット未確定 → Err
        assert!(NativeVoice::prepare(&flink, 15.0).is_err());
    }

    #[test]
    fn pull_primes_then_outputs_shm_data() {
        let flink = temp_flink("basic");
        let w = AudioShmWriter::new(&flink).unwrap();
        w.start_stream(48000, 2);

        let (mut ctx, stats, rate) = NativeVoice::prepare(&flink, 15.0).unwrap();
        assert_eq!(rate, 48000);
        assert_eq!(ctx.channels, 2);

        // target=15ms@48k=720 フレーム。1024 フレーム書けばプライミング完了できる。
        write_const_packet(&w, 1024, 0.5);

        let mut out = vec![9.9f32; 128 * 2];
        ctx.pull_into(&mut out, 128);

        // 1 回目の pull で SHM 全量 (1024) がリングへ入り (>720)、プライミング完了して
        // データが出る。
        assert!(
            out.iter().any(|&s| (s - 0.5).abs() < 1e-6),
            "プライミング完了後は SHM のデータが出力されるべき: {:?}",
            &out[..8]
        );
        assert!(stats.occupancy_frames.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn pull_before_priming_outputs_silence() {
        let flink = temp_flink("prime");
        let w = AudioShmWriter::new(&flink).unwrap();
        w.start_stream(48000, 2);

        let (mut ctx, stats, _) = NativeVoice::prepare(&flink, 15.0).unwrap();

        // target(720) 未満しか書かない → まだ無音のはず。
        write_const_packet(&w, 256, 0.5);

        let mut out = vec![9.9f32; 128 * 2];
        ctx.pull_into(&mut out, 128);

        assert!(
            out.iter().all(|&s| s == 0.0),
            "プライミング前は無音であるべき"
        );
        assert_eq!(stats.underrun_frames.load(Ordering::Relaxed), 128);
    }

    #[test]
    fn pull_on_channel_change_outputs_silence() {
        let flink = temp_flink("chchange");
        let w = AudioShmWriter::new(&flink).unwrap();
        w.start_stream(48000, 2);

        let (mut ctx, _stats, _) = NativeVoice::prepare(&flink, 15.0).unwrap();
        write_const_packet(&w, 1024, 0.5);

        // チャネル数が変わった (2→1) → 再起動は C# 側の責務。native は無音を出す。
        w.start_stream(48000, 1);

        let mut out = vec![9.9f32; 128 * 2];
        ctx.pull_into(&mut out, 128);
        assert!(
            out.iter().all(|&s| s == 0.0),
            "チャネル数変化中は無音であるべき"
        );
    }

    #[test]
    fn pull_drains_shm_across_multiple_scratch_reads() {
        let flink = temp_flink("drain");
        let w = AudioShmWriter::new(&flink).unwrap();
        w.start_stream(48000, 2);

        let (mut ctx, stats, _) = NativeVoice::prepare(&flink, 15.0).unwrap();

        // SCRATCH_FRAMES(4096) を超える量を書く → ループで複数回 read して全量ドレイン。
        write_const_packet(&w, 4096, 0.5);
        write_const_packet(&w, 4096, 0.5);
        write_const_packet(&w, 1000, 0.5);

        let mut out = vec![0.0f32; 128 * 2];
        ctx.pull_into(&mut out, 128);

        // 全量 (9192) がリングへ移っている: occupancy = 9192 - 消費分。
        // 消費は steering 上限で 1 フレームあたり最大 1.02 (max_rate_adjust=0.02) なので
        // 128 フレーム出力での消費は最大 ~131。
        let occ = stats.occupancy_frames.load(Ordering::Relaxed);
        assert!(
            occ >= 9192 - 132 && occ <= 9192,
            "SHM 全量がドレインされるべき: occ={}",
            occ
        );
    }
}
