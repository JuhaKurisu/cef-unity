//! CEF (producer, 実時間) → 出力デバイス (consumer) の滞留量制御つきリングバッファ。
//! C# 版 CefAudioRing (Assets/CefUnity/Runtime/CefAudioRing.cs) の移植。
//!
//! producer と consumer はクロックが独立しておりレートがわずかにずれる。固定レートで
//! 読むと滞留量が一方向にドリフトし、いずれアンダーラン (無音) かオーバーフロー (破棄)
//! で音がぶつ切りになる。そこで滞留量の誤差に応じて消費レートを ±max_rate_adjust だけ
//! 滑らかに操作し (steering)、線形補間で出力することで目標滞留量へ収束させる。
//!
//! ネイティブ音声出力では SHM ドレイン (write) と補間出力 (read) の両方が同一の
//! オーディオコールバックスレッドで動くため、C# 版と違いロックを持たない。

pub struct AudioRing {
    buffer: Vec<f32>, // interleaved
    capacity_frames: usize,
    channels: usize,
    target_frames: usize,
    max_rate_adjust: f64,
    write_frame: u64,  // 累積書き込みフレーム数 (producer)
    read_frame: f64,   // 小数フレーム位置 (consumer)。常に write_frame 以下。
    primed: bool,      // 初回に目標滞留量へ達したか。達するまでは無音を出す。
    /// データ不足で無音を出した出力フレーム数 (累積)。>0 ならアンダーラン発生。
    pub underrun_frames: u64,
    /// 容量超過で破棄した入力フレーム数 (累積)。>0 ならオーバーフロー発生。
    pub overflow_drop_frames: u64,
}

impl AudioRing {
    pub fn new(
        capacity_frames: usize,
        channels: usize,
        target_frames: usize,
        max_rate_adjust: f64,
    ) -> AudioRing {
        let capacity = capacity_frames.max(2);
        let channel_count = channels.max(1);
        AudioRing {
            buffer: vec![0.0; capacity * channel_count],
            capacity_frames: capacity,
            channels: channel_count,
            target_frames: target_frames.clamp(1, capacity - 1),
            max_rate_adjust,
            write_frame: 0,
            read_frame: 0.0,
            primed: false,
            underrun_frames: 0,
            overflow_drop_frames: 0,
        }
    }

    /// 現在の滞留フレーム数 (producer が書いて consumer がまだ消費していない量)。
    pub fn occupancy_frames(&self) -> f64 {
        self.write_frame as f64 - self.read_frame
    }

    #[cfg(test)]
    pub fn target_frames(&self) -> usize {
        self.target_frames
    }

    /// producer: interleaved サンプル source[..frame_count*channels] を書く。
    /// 容量を超える場合は最古フレームを捨てる (バックストップ)。
    pub fn write(&mut self, source: &[f32], mut frame_count: usize) {
        if frame_count == 0 {
            return;
        }
        let mut offset = 0usize;

        // パケット自体が容量を超える: 最新側だけ残す。
        if frame_count > self.capacity_frames {
            let skip = frame_count - self.capacity_frames;
            offset = skip * self.channels;
            self.overflow_drop_frames += skip as u64;
            frame_count = self.capacity_frames;
        }

        // 空き不足: 最古を捨てる = read 位置を前進。
        let occupancy = self.write_frame as i64 - self.read_frame.floor() as i64;
        let free = self.capacity_frames as i64 - occupancy;
        if frame_count as i64 > free {
            let drop = frame_count as i64 - free;
            self.read_frame += drop as f64;
            self.overflow_drop_frames += drop as u64;
        }

        for frame_index in 0..frame_count {
            let destination_base = (self.write_frame as usize % self.capacity_frames) * self.channels;
            let source_base = offset + frame_index * self.channels;
            self.buffer[destination_base..destination_base + self.channels]
                .copy_from_slice(&source[source_base..source_base + self.channels]);
            self.write_frame += 1;
        }
    }

    /// consumer: destination を frame_count フレーム分 (interleaved) 埋める。
    /// base_step = srcRate/outRate (出力1フレームあたり進める source フレーム数)。
    /// 滞留量が目標から外れていれば step を ±max_rate_adjust だけ操作して収束させる。
    /// データ不足時は無音で埋め underrun_frames を加算する。
    pub fn read(&mut self, destination: &mut [f32], frame_count: usize, base_step: f64) {
        for frame_index in 0..frame_count {
            let output_base = frame_index * self.channels;
            let occupancy = self.write_frame as f64 - self.read_frame;

            // 初回プライミング: 目標滞留量に達するまでは無音 (read を進めない)。
            // 開始直後のピッチ揺れを避けるためクリーンに目標まで貯めてから再生開始する。
            if !self.primed {
                if occupancy < self.target_frames as f64 {
                    destination[output_base..output_base + self.channels].fill(0.0);
                    self.underrun_frames += 1;
                    continue;
                }
                self.primed = true;
            }

            // 線形補間には floor と floor+1 の 2 フレームが要る。
            if occupancy < 2.0 {
                destination[output_base..output_base + self.channels].fill(0.0);
                self.underrun_frames += 1;
                continue;
            }

            let index_0 = self.read_frame.floor() as u64;
            let fraction = (self.read_frame - index_0 as f64) as f32;
            let base_0 = (index_0 as usize % self.capacity_frames) * self.channels;
            let base_1 = ((index_0 + 1) as usize % self.capacity_frames) * self.channels;
            for channel_index in 0..self.channels {
                let sample_0 = self.buffer[base_0 + channel_index];
                let sample_1 = self.buffer[base_1 + channel_index];
                destination[output_base + channel_index] = sample_0 + (sample_1 - sample_0) * fraction;
            }

            // レート操作: 滞留量誤差を [-1,1] に正規化し ±max_rate_adjust を掛ける。
            // occupancy > target → step を大きく (速く消費) して滞留を減らす。逆も同様。
            let error = ((occupancy - self.target_frames as f64) / self.target_frames as f64)
                .clamp(-1.0, 1.0);
            let mut step = base_step * (1.0 + self.max_rate_adjust * error);

            // 補間に floor+1 が要るので利用可能量を食い尽くさないようガード。
            let max_advance = occupancy - 1.0;
            if step > max_advance {
                step = max_advance;
            }
            if step < 0.0 {
                step = 0.0;
            }
            self.read_frame += step;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_RATE: usize = 48_000;
    const OUTPUT_RATE: usize = 44_100;
    const CHANNELS: usize = 2;

    fn make_ring() -> AudioRing {
        let capacity = (0.5 * SOURCE_RATE as f64).ceil() as usize;
        let target = (0.08 * SOURCE_RATE as f64).ceil() as usize;
        AudioRing::new(capacity, CHANNELS, target, 0.01)
    }

    // 440Hz サイン波を interleaved で frame_count フレーム生成する。phase は継続用に更新。
    fn make_sine(frame_count: usize, phase: &mut f64) -> Vec<f32> {
        let mut buffer = vec![0.0f32; frame_count * CHANNELS];
        let delta_phase = 2.0 * std::f64::consts::PI * 440.0 / SOURCE_RATE as f64;
        for frame_index in 0..frame_count {
            let sample = (phase.sin() * 0.2) as f32;
            for channel_index in 0..CHANNELS {
                buffer[frame_index * CHANNELS + channel_index] = sample;
            }
            *phase += delta_phase;
        }
        buffer
    }

    // producer/consumer を tick 単位で交互に動かし、(プライミング後アンダーラン,
    // オーバーフロー, 出力の最大不連続量) を返す。C# 版 RunStreamingScenario の移植。
    fn run_streaming_scenario(
        produce_frames_per_tick: usize,
        consume_frames_per_tick: usize,
        ticks: usize,
    ) -> (u64, u64, f32) {
        let mut ring = make_ring();
        let base_step = SOURCE_RATE as f64 / OUTPUT_RATE as f64;
        let mut phase = 0.0;

        let mut output_buffer = vec![0.0f32; (consume_frames_per_tick + 8) * CHANNELS];
        let mut max_discontinuity = 0.0f32;
        let mut underrun_at_prime_window: Option<u64> = None;
        // プライミング完了とみなす tick (目標 80ms ≒ 8 tick + 余裕)。これ以降を評価。
        const PRIME_WINDOW_TICKS: usize = 20;
        // 連続性は安定後 (priming 直後の開始トランジェントを除く) のみ評価。
        const CONTINUITY_FROM_TICK: usize = 25;

        let mut previous_frame: Option<[f32; CHANNELS]> = None;

        for tick in 0..ticks {
            let sine = make_sine(produce_frames_per_tick, &mut phase);
            ring.write(&sine, produce_frames_per_tick);

            ring.read(&mut output_buffer, consume_frames_per_tick, base_step);

            if tick == PRIME_WINDOW_TICKS {
                underrun_at_prime_window = Some(ring.underrun_frames);
            }

            if tick >= CONTINUITY_FROM_TICK {
                for frame_index in 0..consume_frames_per_tick {
                    if let Some(previous) = previous_frame {
                        for channel_index in 0..CHANNELS {
                            let difference = (output_buffer[frame_index * CHANNELS + channel_index] - previous[channel_index]).abs();
                            if difference > max_discontinuity {
                                max_discontinuity = difference;
                            }
                        }
                    }
                    let mut current = [0.0f32; CHANNELS];
                    current.copy_from_slice(&output_buffer[frame_index * CHANNELS..frame_index * CHANNELS + CHANNELS]);
                    previous_frame = Some(current);
                }
            }
        }

        let underrun_after_prime = match underrun_at_prime_window {
            Some(count) => ring.underrun_frames - count,
            None => ring.underrun_frames,
        };
        (underrun_after_prime, ring.overflow_drop_frames, max_discontinuity)
    }

    #[test]
    fn read_with_unit_step_returns_written_samples_in_order() {
        // baseStep=1.0 (リサンプルなし), 1ch でランプを書くとそのまま順に出る (fraction=0)。
        let mut ring = AudioRing::new(1000, 1, 4, 0.0);
        let ramp: Vec<f32> = (0..100).map(|index| index as f32).collect();
        ring.write(&ramp, 100); // 目標(4)以上溜まっている → プライミング即完了

        let mut out = vec![0.0f32; 10];
        ring.read(&mut out, 10, 1.0);

        for (index, &sample) in out.iter().enumerate() {
            assert!((sample - index as f32).abs() < 1e-4, "index {}: {}", index, sample);
        }
        assert_eq!(ring.underrun_frames, 0);
        assert_eq!(ring.overflow_drop_frames, 0);
    }

    #[test]
    fn read_before_target_reached_outputs_silence_and_counts_underrun() {
        let mut ring = AudioRing::new(1000, 1, 50, 0.0);
        let few = vec![1.0f32; 10];
        ring.write(&few, 10); // 目標 50 未満 → まだプライミングしない

        let mut out = vec![9.9f32; 8];
        ring.read(&mut out, 8, 1.0);

        for &sample in &out {
            assert_eq!(sample, 0.0, "プライミング前は無音であるべき");
        }
        assert_eq!(ring.underrun_frames, 8);
    }

    #[test]
    fn write_beyond_capacity_drops_oldest_and_counts_overflow() {
        let mut ring = AudioRing::new(100, 1, 10, 0.0);
        let big: Vec<f32> = (0..500).map(|index| index as f32).collect();
        ring.write(&big, 500); // 容量 100 を大きく超える

        assert!(
            ring.overflow_drop_frames > 0,
            "容量超過で破棄が記録されるべき"
        );
        assert!(
            ring.occupancy_frames() <= 100.0,
            "滞留量は容量以内に収まるべき"
        );
    }

    #[test]
    fn steady_state_matched_clocks_no_underrun_no_overflow_continuous_output() {
        let (underrun, overflow, max_discontinuity) = run_streaming_scenario(480, 441, 500);
        assert_eq!(underrun, 0, "プライミング後のアンダーランは 0 であるべき");
        assert_eq!(overflow, 0, "オーバーフロー破棄は 0 であるべき");
        // 440Hz/0.2amp の隣接サンプル差は最大 ~0.0125。クリックなら ~0.4 跳ぶ。
        assert!(
            max_discontinuity < 0.05,
            "出力に不連続 (クリック) があってはならない: {}",
            max_discontinuity
        );
    }

    #[test]
    fn producer_slightly_faster_steering_absorbs_no_overflow_no_underrun() {
        // producer が consumer よりわずかに速い (≈+0.4%)。steering (±1%) で吸収できるはず。
        let (underrun, overflow, max_discontinuity) = run_streaming_scenario(482, 441, 800);
        assert_eq!(overflow, 0, "速い producer でも steering がオーバーフローを防ぐべき");
        assert_eq!(underrun, 0, "アンダーランは発生しないべき");
        assert!(max_discontinuity < 0.05, "出力は連続であるべき: {}", max_discontinuity);
    }

    #[test]
    fn producer_slightly_slower_steering_absorbs_no_underrun_no_overflow() {
        // producer がわずかに遅い (≈-0.4%)。steering が消費を緩めてアンダーランを防ぐ。
        let (underrun, overflow, max_discontinuity) = run_streaming_scenario(478, 441, 800);
        assert_eq!(underrun, 0, "遅い producer でも steering がアンダーランを防ぐべき");
        assert_eq!(overflow, 0, "オーバーフローは発生しないべき");
        assert!(max_discontinuity < 0.05, "出力は連続であるべき: {}", max_discontinuity);
    }

    #[test]
    fn steady_state_occupancy_converges_near_target() {
        // 定常運転後、滞留量が目標近傍へ収束していること。
        let mut ring = make_ring();
        let base_step = SOURCE_RATE as f64 / OUTPUT_RATE as f64;
        let mut phase = 0.0;
        let mut produced = 0usize;
        let mut consumed = 0usize;
        let mut output_buffer = vec![0.0f32; 441 * CHANNELS];

        for tick in 0..600usize {
            let produce_frames = (((tick + 1) as f64 * 480.0).round() as usize) - produced;
            let sine = make_sine(produce_frames, &mut phase);
            ring.write(&sine, produce_frames);
            produced += produce_frames;

            let mut consume_frames = (((tick + 1) as f64 * 441.0).round() as usize) - consumed;
            if consume_frames > output_buffer.len() / CHANNELS {
                consume_frames = output_buffer.len() / CHANNELS;
            }
            ring.read(&mut output_buffer, consume_frames, base_step);
            consumed += consume_frames;
        }

        let occupancy = ring.occupancy_frames();
        let target = ring.target_frames() as f64;
        assert!(
            occupancy >= target * 0.5 && occupancy <= target * 1.5,
            "滞留量 ({:.0}) は目標 ({}) 近傍へ収束すべき",
            occupancy,
            target
        );
    }
}
