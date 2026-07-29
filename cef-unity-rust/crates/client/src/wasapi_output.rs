//! Windows のネイティブ音声出力 (WASAPI 共有モード・イベント駆動)。
//!
//! macOS の `native_voice.rs` (AudioUnit) に対応する Windows 実装。Unity の FMOD
//! ミキサ (dspBuffer × numBuffers) を迂回して低遅延で再生する。
//!
//! SHM ドレインと steering リングの扱いは `audio_pull` に共通化してあり、
//! ここは「WASAPI デバイスを開いてレンダースレッドを回す」出力層だけを持つ。
//!
//! # レート変換
//!
//! AudioUnit と違い WASAPI 共有モードはミキサのフォーマット固定で、暗黙の
//! レート変換をしてくれない。ソース (CEF) とデバイスのレートが違う場合は
//! `audio_pull` のリング読み出しに `base_step = source_rate / device_rate` を
//! 渡して、リング側の補間でリサンプルする。

#![cfg(target_os = "windows")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};

use crate::audio_pull::{self, PullContext, VoiceStatistics};

/// WAVE_FORMAT_IEEE_FLOAT。ミキサのフォーマットが float かどうかの判定に使う。
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// 要求レイテンシからバッファのフレーム数を求める。切り上げでアンダーランを避ける。
fn buffer_frame_count(sample_rate: u32, target_milliseconds: f32) -> u32 {
    let frames = (sample_rate as f32) * target_milliseconds / 1000.0;
    frames.ceil().max(1.0) as u32
}

/// 音量を有効範囲に丸める。
fn clamp_volume(volume: f32) -> f32 {
    volume.clamp(0.0, 1.0)
}

/// レンダースレッドと共有する制御状態。
struct SharedControl {
    stop: AtomicBool,
    /// 音量を 1/1000 単位の整数で持つ (f32 の atomic が無いため)。
    volume_per_mille: AtomicU32,
    stop_event: HANDLE,
}

unsafe impl Send for SharedControl {}
unsafe impl Sync for SharedControl {}

pub struct WasapiOutput {
    control: Arc<SharedControl>,
    thread: Option<std::thread::JoinHandle<()>>,
    statistics: Arc<VoiceStatistics>,
    source_rate: u32,
}

impl WasapiOutput {
    /// SHM flink から自前リーダーを開いて WASAPI 再生を開始する。
    /// ストリームフォーマット未確定なら Err (呼び出し側は
    /// `cef_unity_get_audio_format` が 1 を返してから呼ぶこと)。
    ///
    /// `io_frames` は AudioUnit 用の引数で WASAPI では使わない (共有モードでは
    /// バッファ長はミキサ側が決めるため)。シグネチャを macOS と揃えるために受け取る。
    pub fn start(
        flink: &str,
        target_milliseconds: f32,
        io_frames: i32,
    ) -> Result<WasapiOutput, String> {
        let _ = io_frames;
        let (context, statistics, source_rate) = audio_pull::prepare(flink, target_milliseconds)?;

        let stop_event = unsafe {
            CreateEventW(None, false, false, None)
                .map_err(|error| format!("CreateEventW (stop): {:?}", error))?
        };
        let control = Arc::new(SharedControl {
            stop: AtomicBool::new(false),
            volume_per_mille: AtomicU32::new(1000),
            stop_event,
        });

        let (ready_sender, ready_receiver) = std::sync::mpsc::channel::<Result<(), String>>();
        let thread_control = control.clone();
        let thread = std::thread::Builder::new()
            .name("cef-wasapi-output".to_string())
            .spawn(move || {
                render_thread_main(context, source_rate, thread_control, ready_sender);
            })
            .map_err(|error| format!("spawn wasapi thread: {}", error))?;

        // デバイスを開くまで待って、失敗ならここで Err を返す
        // (呼び出し側が Unity ミキサ経路へフォールバックできるように)。
        match ready_receiver.recv() {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                control.stop.store(true, Ordering::Release);
                let _ = thread.join();
                unsafe {
                    let _ = CloseHandle(stop_event);
                }
                return Err(message);
            }
            Err(_) => {
                let _ = thread.join();
                unsafe {
                    let _ = CloseHandle(stop_event);
                }
                return Err("wasapi thread exited before signalling readiness".into());
            }
        }

        crate::logging::write(
            "wasapi",
            &format!("native audio started (source_rate={})", source_rate),
        );
        Ok(WasapiOutput {
            control,
            thread: Some(thread),
            statistics,
            source_rate,
        })
    }

    pub fn set_volume(&self, volume: f32) {
        let per_mille = (clamp_volume(volume) * 1000.0).round() as u32;
        self.control
            .volume_per_mille
            .store(per_mille, Ordering::Relaxed);
    }

    /// (滞留量 ms, 累積アンダーランフレーム, 累積オーバーフローフレーム)。
    pub fn statistics(&self) -> (f32, u64, u64) {
        (
            audio_pull::occupancy_milliseconds(&self.statistics, self.source_rate),
            self.statistics.underrun_frames.load(Ordering::Relaxed),
            self.statistics.overflow_frames.load(Ordering::Relaxed),
        )
    }
}

impl Drop for WasapiOutput {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Release);
        unsafe {
            let _ = SetEvent(self.control.stop_event);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe {
            let _ = CloseHandle(self.control.stop_event);
        }
        crate::logging::write("wasapi", "native audio stopped");
    }
}

/// ミキサのフォーマットが 32bit float かどうか。
fn format_is_float(format: *const WAVEFORMATEX) -> bool {
    unsafe {
        let tag = (*format).wFormatTag;
        if tag == WAVE_FORMAT_IEEE_FLOAT {
            return true;
        }
        if tag == WAVE_FORMAT_EXTENSIBLE {
            let extensible = format as *const WAVEFORMATEXTENSIBLE;
            // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT の Data1 は WAVE_FORMAT_IEEE_FLOAT と同値
            return (*extensible).SubFormat.data1 == WAVE_FORMAT_IEEE_FLOAT as u32;
        }
        false
    }
}

fn render_thread_main(
    mut context: Box<PullContext>,
    source_rate: u32,
    control: Arc<SharedControl>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    unsafe {
        // MTA で初期化する (このスレッド専用)。
        let initialize_result = CoInitializeEx(None, COINIT_MULTITHREADED);
        if initialize_result.is_err() {
            let _ = ready.send(Err(format!("CoInitializeEx: {:?}", initialize_result)));
            return;
        }

        let result = run_render_loop(&mut context, source_rate, &control, &ready);
        if let Err(message) = result {
            // ready をまだ送っていない場合に備えて送る (送信済みなら無視される)。
            let _ = ready.send(Err(message.clone()));
            crate::logging::write("wasapi", &format!("render loop ended with error: {}", message));
        }
        CoUninitialize();
    }
}

/// デバイスを開いてレンダーループを回す。`ready` は初期化成否を 1 度だけ送る。
unsafe fn run_render_loop(
    context: &mut PullContext,
    source_rate: u32,
    control: &SharedControl,
    ready: &std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|error| format!("CoCreateInstance(MMDeviceEnumerator): {:?}", error))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|error| format!("GetDefaultAudioEndpoint: {:?}", error))?;
        let audio_client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|error| format!("IMMDevice::Activate(IAudioClient): {:?}", error))?;

        let mix_format = audio_client
            .GetMixFormat()
            .map_err(|error| format!("GetMixFormat: {:?}", error))?;
        if mix_format.is_null() {
            return Err("GetMixFormat returned null".into());
        }
        let device_rate = (*mix_format).nSamplesPerSec;
        let device_channels = (*mix_format).nChannels as usize;
        let is_float = format_is_float(mix_format);

        // 共有モードは 100ns 単位。要求レイテンシからバッファ長を決める。
        // AUTOCONVERTPCM を付けるとレート/チャネル変換をミキサ側に任せられるが、
        // 対応しない環境もあるので、失敗したら変換なしで開き直してこちらで
        // リサンプルする (base_step)。
        let requested_duration =
            (buffer_frame_count(device_rate, 40.0) as i64) * 10_000_000 / device_rate as i64;

        let mut used_autoconvert = true;
        let mut initialize_result = audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            requested_duration,
            0,
            mix_format,
            None,
        );
        if initialize_result.is_err() {
            used_autoconvert = false;
            initialize_result = audio_client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                requested_duration,
                0,
                mix_format,
                None,
            );
        }
        if let Err(error) = initialize_result {
            CoTaskMemFree(Some(mix_format as *const _));
            return Err(format!("IAudioClient::Initialize: {:?}", error));
        }

        if !is_float {
            // WAVEFORMATEX は packed なので、format! に参照を渡さないよう値を取り出す。
            let format_tag = (*mix_format).wFormatTag;
            CoTaskMemFree(Some(mix_format as *const _));
            return Err(format!(
                "unsupported mixer format (not IEEE float): wFormatTag={}",
                format_tag
            ));
        }

        let render_event = CreateEventW(None, false, false, None)
            .map_err(|error| format!("CreateEventW (render): {:?}", error))?;
        audio_client
            .SetEventHandle(render_event)
            .map_err(|error| format!("SetEventHandle: {:?}", error))?;

        let buffer_frames = audio_client
            .GetBufferSize()
            .map_err(|error| format!("GetBufferSize: {:?}", error))?;
        let render_client: IAudioRenderClient = audio_client
            .GetService()
            .map_err(|error| format!("GetService(IAudioRenderClient): {:?}", error))?;

        // ソースとデバイスのレートが違い、かつミキサ変換が使えない場合のみ
        // こちらでリサンプルする。
        let base_step = if used_autoconvert || device_rate == 0 {
            1.0
        } else {
            source_rate as f64 / device_rate as f64
        };

        crate::logging::write(
            "wasapi",
            &format!(
                "device opened: rate={} channels={} buffer_frames={} autoconvert={} base_step={:.4}",
                device_rate, device_channels, buffer_frames, used_autoconvert, base_step
            ),
        );

        audio_client
            .Start()
            .map_err(|error| format!("IAudioClient::Start: {:?}", error))?;

        // 初期化成功をここで通知する (以後 ready は送らない)。
        let _ = ready.send(Ok(()));

        // PullContext のチャネル数とデバイスのチャネル数が違う場合に備えた作業領域。
        let source_channels = context.channels;
        let mut pull_buffer = vec![0.0f32; buffer_frames as usize * source_channels.max(1)];

        let mut result = Ok(());
        while !control.stop.load(Ordering::Acquire) {
            let wait = WaitForSingleObject(render_event, 200);
            if wait != WAIT_OBJECT_0 {
                // タイムアウトは stop フラグ確認のために正常。それ以外は抜ける。
                if control.stop.load(Ordering::Acquire) {
                    break;
                }
                continue;
            }

            let padding = match audio_client.GetCurrentPadding() {
                Ok(padding) => padding,
                Err(error) => {
                    result = Err(format!("GetCurrentPadding: {:?}", error));
                    break;
                }
            };
            let available = buffer_frames.saturating_sub(padding);
            if available == 0 {
                continue;
            }

            let buffer = match render_client.GetBuffer(available) {
                Ok(buffer) => buffer,
                Err(error) => {
                    result = Err(format!("GetBuffer: {:?}", error));
                    break;
                }
            };

            let frames = available as usize;
            let needed = frames * source_channels;
            if pull_buffer.len() < needed {
                pull_buffer.resize(needed, 0.0);
            }
            context.pull_into(&mut pull_buffer[..needed], frames, base_step);

            let volume = control.volume_per_mille.load(Ordering::Relaxed) as f32 / 1000.0;
            let destination =
                std::slice::from_raw_parts_mut(buffer as *mut f32, frames * device_channels);
            copy_with_channel_mapping(
                &pull_buffer[..needed],
                destination,
                frames,
                source_channels,
                device_channels,
                volume,
            );

            if let Err(error) = render_client.ReleaseBuffer(available, 0) {
                result = Err(format!("ReleaseBuffer: {:?}", error));
                break;
            }
        }

        let _ = audio_client.Stop();
        let _ = CloseHandle(render_event);
        CoTaskMemFree(Some(mix_format as *const _));
        result
    }
}

/// ソースのインターリーブ f32 をデバイスのチャネル数に合わせて書き出す。
///
/// チャネル数が一致すればそのまま、デバイスの方が多ければ余りを 0 で埋め、
/// 少なければ先頭チャネルだけ使う。音量はここで掛ける。
fn copy_with_channel_mapping(
    source: &[f32],
    destination: &mut [f32],
    frames: usize,
    source_channels: usize,
    device_channels: usize,
    volume: f32,
) {
    if source_channels == 0 || device_channels == 0 {
        destination.fill(0.0);
        return;
    }
    for frame in 0..frames {
        let source_base = frame * source_channels;
        let destination_base = frame * device_channels;
        for channel in 0..device_channels {
            let sample = if channel < source_channels {
                source[source_base + channel]
            } else if source_channels == 1 {
                // モノラルソースはデバイス全チャネルへ複製する
                source[source_base]
            } else {
                0.0
            };
            destination[destination_base + channel] = sample * volume;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_frame_count_honors_target_latency() {
        assert_eq!(buffer_frame_count(48_000, 20.0), 960, "48kHz の 20ms は 960 フレーム");
        assert_eq!(buffer_frame_count(44_100, 20.0), 882, "44.1kHz の 20ms は 882 フレーム");
    }

    #[test]
    fn buffer_frame_count_rounds_up_to_avoid_underrun() {
        // 44.1kHz の 1ms は 44.1 フレーム。切り捨てると不足するので切り上げる。
        assert_eq!(buffer_frame_count(44_100, 1.0), 45);
        // 0 フレームは返さない (WASAPI が受け付けない)
        assert_eq!(buffer_frame_count(48_000, 0.0), 1);
    }

    #[test]
    fn volume_is_clamped_to_valid_range() {
        assert_eq!(clamp_volume(-1.0), 0.0);
        assert_eq!(clamp_volume(0.5), 0.5);
        assert_eq!(clamp_volume(3.0), 1.0);
    }

    #[test]
    fn channel_mapping_passes_through_when_counts_match() {
        let source = vec![0.1, 0.2, 0.3, 0.4];
        let mut destination = vec![0.0; 4];
        copy_with_channel_mapping(&source, &mut destination, 2, 2, 2, 1.0);
        assert_eq!(destination, vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn channel_mapping_duplicates_mono_to_all_device_channels() {
        let source = vec![0.5, 0.25];
        let mut destination = vec![0.0; 4];
        copy_with_channel_mapping(&source, &mut destination, 2, 1, 2, 1.0);
        assert_eq!(
            destination,
            vec![0.5, 0.5, 0.25, 0.25],
            "モノラルは両チャネルへ複製されること"
        );
    }

    #[test]
    fn channel_mapping_zero_fills_extra_device_channels() {
        // ステレオソース → 4ch デバイス。3ch 目以降は無音。
        let source = vec![0.1, 0.2];
        let mut destination = vec![9.9; 4];
        copy_with_channel_mapping(&source, &mut destination, 1, 2, 4, 1.0);
        assert_eq!(destination, vec![0.1, 0.2, 0.0, 0.0]);
    }

    #[test]
    fn channel_mapping_applies_volume() {
        let source = vec![1.0, -1.0];
        let mut destination = vec![0.0; 2];
        copy_with_channel_mapping(&source, &mut destination, 1, 2, 2, 0.5);
        assert_eq!(destination, vec![0.5, -0.5]);
    }
}
