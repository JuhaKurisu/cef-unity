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
    buf: Vec<f32>, // interleaved
    cap_frames: usize,
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
        let cap = capacity_frames.max(2);
        let ch = channels.max(1);
        AudioRing {
            buf: vec![0.0; cap * ch],
            cap_frames: cap,
            channels: ch,
            target_frames: target_frames.clamp(1, cap - 1),
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

    pub fn target_frames(&self) -> usize {
        self.target_frames
    }

    /// producer: interleaved サンプル src[..frame_count*channels] を書く。
    /// 容量を超える場合は最古フレームを捨てる (バックストップ)。
    pub fn write(&mut self, src: &[f32], mut frame_count: usize) {
        if frame_count == 0 {
            return;
        }
        let mut offset = 0usize;

        // パケット自体が容量を超える: 最新側だけ残す。
        if frame_count > self.cap_frames {
            let skip = frame_count - self.cap_frames;
            offset = skip * self.channels;
            self.overflow_drop_frames += skip as u64;
            frame_count = self.cap_frames;
        }

        // 空き不足: 最古を捨てる = read 位置を前進。
        let occ = self.write_frame as i64 - self.read_frame.floor() as i64;
        let free = self.cap_frames as i64 - occ;
        if frame_count as i64 > free {
            let drop = frame_count as i64 - free;
            self.read_frame += drop as f64;
            self.overflow_drop_frames += drop as u64;
        }

        for f in 0..frame_count {
            let dst_base = (self.write_frame as usize % self.cap_frames) * self.channels;
            let src_base = offset + f * self.channels;
            self.buf[dst_base..dst_base + self.channels]
                .copy_from_slice(&src[src_base..src_base + self.channels]);
            self.write_frame += 1;
        }
    }

    /// consumer: dst を frame_count フレーム分 (interleaved) 埋める。
    /// base_step = srcRate/outRate (出力1フレームあたり進める src フレーム数)。
    /// 滞留量が目標から外れていれば step を ±max_rate_adjust だけ操作して収束させる。
    /// データ不足時は無音で埋め underrun_frames を加算する。
    pub fn read(&mut self, dst: &mut [f32], frame_count: usize, base_step: f64) {
        for f in 0..frame_count {
            let ob = f * self.channels;
            let occ = self.write_frame as f64 - self.read_frame;

            // 初回プライミング: 目標滞留量に達するまでは無音 (read を進めない)。
            // 開始直後のピッチ揺れを避けるためクリーンに目標まで貯めてから再生開始する。
            if !self.primed {
                if occ < self.target_frames as f64 {
                    dst[ob..ob + self.channels].fill(0.0);
                    self.underrun_frames += 1;
                    continue;
                }
                self.primed = true;
            }

            // 線形補間には floor と floor+1 の 2 フレームが要る。
            if occ < 2.0 {
                dst[ob..ob + self.channels].fill(0.0);
                self.underrun_frames += 1;
                continue;
            }

            let i0 = self.read_frame.floor() as u64;
            let frac = (self.read_frame - i0 as f64) as f32;
            let b0 = (i0 as usize % self.cap_frames) * self.channels;
            let b1 = ((i0 + 1) as usize % self.cap_frames) * self.channels;
            for c in 0..self.channels {
                let s0 = self.buf[b0 + c];
                let s1 = self.buf[b1 + c];
                dst[ob + c] = s0 + (s1 - s0) * frac;
            }

            // レート操作: 滞留量誤差を [-1,1] に正規化し ±max_rate_adjust を掛ける。
            // occ > target → step を大きく (速く消費) して滞留を減らす。逆も同様。
            let err = ((occ - self.target_frames as f64) / self.target_frames as f64)
                .clamp(-1.0, 1.0);
            let mut step = base_step * (1.0 + self.max_rate_adjust * err);

            // 補間に floor+1 が要るので利用可能量を食い尽くさないようガード。
            let max_advance = occ - 1.0;
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

    const SRC_RATE: usize = 48_000;
    const OUT_RATE: usize = 44_100;
    const CHANNELS: usize = 2;

    fn make_ring() -> AudioRing {
        let cap = (0.5 * SRC_RATE as f64).ceil() as usize;
        let target = (0.08 * SRC_RATE as f64).ceil() as usize;
        AudioRing::new(cap, CHANNELS, target, 0.01)
    }

    // 440Hz サイン波を interleaved で frame_count フレーム生成する。phase は継続用に更新。
    fn make_sine(frame_count: usize, phase: &mut f64) -> Vec<f32> {
        let mut buf = vec![0.0f32; frame_count * CHANNELS];
        let dphi = 2.0 * std::f64::consts::PI * 440.0 / SRC_RATE as f64;
        for f in 0..frame_count {
            let s = (phase.sin() * 0.2) as f32;
            for c in 0..CHANNELS {
                buf[f * CHANNELS + c] = s;
            }
            *phase += dphi;
        }
        buf
    }

    // producer/consumer を tick 単位で交互に動かし、(プライミング後アンダーラン,
    // オーバーフロー, 出力の最大不連続量) を返す。C# 版 RunStreamingScenario の移植。
    fn run_streaming_scenario(
        produce_frames_per_tick: usize,
        consume_frames_per_tick: usize,
        ticks: usize,
    ) -> (u64, u64, f32) {
        let mut ring = make_ring();
        let base_step = SRC_RATE as f64 / OUT_RATE as f64;
        let mut phase = 0.0;

        let mut out_buf = vec![0.0f32; (consume_frames_per_tick + 8) * CHANNELS];
        let mut max_discontinuity = 0.0f32;
        let mut under_at_prime_window: Option<u64> = None;
        // プライミング完了とみなす tick (目標 80ms ≒ 8 tick + 余裕)。これ以降を評価。
        const PRIME_WINDOW_TICKS: usize = 20;
        // 連続性は安定後 (priming 直後の開始トランジェントを除く) のみ評価。
        const CONTINUITY_FROM_TICK: usize = 25;

        let mut prev_frame: Option<[f32; CHANNELS]> = None;

        for t in 0..ticks {
            let sine = make_sine(produce_frames_per_tick, &mut phase);
            ring.write(&sine, produce_frames_per_tick);

            ring.read(&mut out_buf, consume_frames_per_tick, base_step);

            if t == PRIME_WINDOW_TICKS {
                under_at_prime_window = Some(ring.underrun_frames);
            }

            if t >= CONTINUITY_FROM_TICK {
                for f in 0..consume_frames_per_tick {
                    if let Some(prev) = prev_frame {
                        for c in 0..CHANNELS {
                            let d = (out_buf[f * CHANNELS + c] - prev[c]).abs();
                            if d > max_discontinuity {
                                max_discontinuity = d;
                            }
                        }
                    }
                    let mut cur = [0.0f32; CHANNELS];
                    cur.copy_from_slice(&out_buf[f * CHANNELS..f * CHANNELS + CHANNELS]);
                    prev_frame = Some(cur);
                }
            }
        }

        let under_after_prime = match under_at_prime_window {
            Some(w) => ring.underrun_frames - w,
            None => ring.underrun_frames,
        };
        (under_after_prime, ring.overflow_drop_frames, max_discontinuity)
    }

    #[test]
    fn read_with_unit_step_returns_written_samples_in_order() {
        // baseStep=1.0 (リサンプルなし), 1ch でランプを書くとそのまま順に出る (frac=0)。
        let mut ring = AudioRing::new(1000, 1, 4, 0.0);
        let ramp: Vec<f32> = (0..100).map(|i| i as f32).collect();
        ring.write(&ramp, 100); // 目標(4)以上溜まっている → プライミング即完了

        let mut out = vec![0.0f32; 10];
        ring.read(&mut out, 10, 1.0);

        for (i, &s) in out.iter().enumerate() {
            assert!((s - i as f32).abs() < 1e-4, "index {}: {}", i, s);
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

        for &s in &out {
            assert_eq!(s, 0.0, "プライミング前は無音であるべき");
        }
        assert_eq!(ring.underrun_frames, 8);
    }

    #[test]
    fn write_beyond_capacity_drops_oldest_and_counts_overflow() {
        let mut ring = AudioRing::new(100, 1, 10, 0.0);
        let big: Vec<f32> = (0..500).map(|i| i as f32).collect();
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
        let (under, over, max_disc) = run_streaming_scenario(480, 441, 500);
        assert_eq!(under, 0, "プライミング後のアンダーランは 0 であるべき");
        assert_eq!(over, 0, "オーバーフロー破棄は 0 であるべき");
        // 440Hz/0.2amp の隣接サンプル差は最大 ~0.0125。クリックなら ~0.4 跳ぶ。
        assert!(
            max_disc < 0.05,
            "出力に不連続 (クリック) があってはならない: {}",
            max_disc
        );
    }

    #[test]
    fn producer_slightly_faster_steering_absorbs_no_overflow_no_underrun() {
        // producer が consumer よりわずかに速い (≈+0.4%)。steering (±1%) で吸収できるはず。
        let (under, over, max_disc) = run_streaming_scenario(482, 441, 800);
        assert_eq!(over, 0, "速い producer でも steering がオーバーフローを防ぐべき");
        assert_eq!(under, 0, "アンダーランは発生しないべき");
        assert!(max_disc < 0.05, "出力は連続であるべき: {}", max_disc);
    }

    #[test]
    fn producer_slightly_slower_steering_absorbs_no_underrun_no_overflow() {
        // producer がわずかに遅い (≈-0.4%)。steering が消費を緩めてアンダーランを防ぐ。
        let (under, over, max_disc) = run_streaming_scenario(478, 441, 800);
        assert_eq!(under, 0, "遅い producer でも steering がアンダーランを防ぐべき");
        assert_eq!(over, 0, "オーバーフローは発生しないべき");
        assert!(max_disc < 0.05, "出力は連続であるべき: {}", max_disc);
    }

    #[test]
    fn steady_state_occupancy_converges_near_target() {
        // 定常運転後、滞留量が目標近傍へ収束していること。
        let mut ring = make_ring();
        let base_step = SRC_RATE as f64 / OUT_RATE as f64;
        let mut phase = 0.0;
        let mut produced = 0usize;
        let mut consumed = 0usize;
        let mut out_buf = vec![0.0f32; 441 * CHANNELS];

        for t in 0..600usize {
            let prod = (((t + 1) as f64 * 480.0).round() as usize) - produced;
            let sine = make_sine(prod, &mut phase);
            ring.write(&sine, prod);
            produced += prod;

            let mut cons = (((t + 1) as f64 * 441.0).round() as usize) - consumed;
            if cons > out_buf.len() / CHANNELS {
                cons = out_buf.len() / CHANNELS;
            }
            ring.read(&mut out_buf, cons, base_step);
            consumed += cons;
        }

        let occ = ring.occupancy_frames();
        let target = ring.target_frames() as f64;
        assert!(
            occ >= target * 0.5 && occ <= target * 1.5,
            "滞留量 ({:.0}) は目標 ({}) 近傍へ収束すべき",
            occ,
            target
        );
    }
}
