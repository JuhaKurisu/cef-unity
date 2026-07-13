# CRI 方式ネイティブ音声出力 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** client dylib 内に AudioUnit 直結のネイティブ音声出力 (NativeVoice) を追加し、Unity FMOD ミキサをバイパスして内部音声遅延を ~160ms → ~30ms 級へ削減する (macOS 先行)。

**Architecture:** server は変更なし。client dylib に「自前の AudioShmReader (独立カーソル) + steering つきローカルリング + AudioUnit (DefaultOutput)」を自己完結で追加する。SHM ドレインと補間出力は全て CoreAudio render callback スレッド内で行う (単一スレッド・ロック不要)。レート変換は AU 内蔵コンバータに任せ、steering は ppm ドリフト吸収と滞留量制御のみ担当する。停止は detached フラグ + active カウンタの排水待ち (DetachAndWait) で UAF を構造的に排除する。設計の出典はメモリ `audio-native-output.md` / `audio-latency.md` (2026-07-03 確定、PoC 済み)。

**Tech Stack:** Rust (edition 2024) / C (AudioUnit, C11 atomics) / C# (Unity)。csbindgen で FFI バインディング自動生成。

## Global Constraints

- FFI 関数は `crates/client/src/lib.rs` に定義すること (csbindgen は `input_extern_file("src/lib.rs")` のみパースする。他ファイルに書くと NativeMethods.g.cs に出ない)
- `cargo build` すると build.rs が `NativeMethods.g.cs` を **cef-unity-csharp と unityproject の両方へ自動生成**する。`CefUnity.cs` (高レベルラッパ) は**両プロジェクト手動同期** (namespace: csharp 側 `Interop` / Unity 側 `CefUnity.Interop`)
- 既存スタイルに従う: `#[unsafe(no_mangle)]`、`unsafe extern "C" { ... }`、日本語 doc コメント
- オーディオコールバック内でのアロケーション・ロック・ブロッキング・Unity API 呼び出しは禁止
- 新規モジュールは `#[cfg(target_os = "macos")]`。FFI 関数自体は全プラットフォームに存在させ、非 macOS では -1 / no-op を返す
- Rust 変更後は `bash deploy.sh` (cef-unity-rust/ から実行)。**dylib 変更後は Unity Editor 再起動必須**
- 遅延の実耳検証は**内蔵スピーカーか有線** (BT は単体 +219ms で無意味)
- `CefAudioOutput` (Unity ミキサ経路) と NativeVoice を同時に有効にすると二重再生 — 排他はサンプルの enum 切替で保証
- コミットメッセージ末尾: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

## ファイル構造

| ファイル | 責務 |
|---|---|
| Create: `cef-unity-rust/crates/client/src/audio_ring.rs` | C# `CefAudioRing` の Rust 移植。steering + 線形補間 + プライミング。純ロジック (ロックなし・単一スレッド前提)。単体テスト同居 |
| Create: `cef-unity-rust/crates/client/src/au_output.c` | AudioUnit (DefaultOutput) C シム。start/stop/set_volume。detached+active の排水プロトコル |
| Create: `cef-unity-rust/crates/client/src/au_output.rs` | 上記の Rust extern 宣言 + `#[ignore]` 実機スモークテスト |
| Create: `cef-unity-rust/crates/client/src/native_voice.rs` | `NativeVoice`: 自前 AudioShmReader + AudioRing + AU の束ね。pull ロジックは `PullCtx::pull_into` に分離してテスト可能に |
| Modify: `cef-unity-rust/crates/client/src/lib.rs` | mod 宣言、`ClientBrowserInstance` に `audio_flink`/`native_voice` 追加、FFI 4本、destroy 先頭で voice stop |
| Modify: `cef-unity-rust/crates/client/build.rs` | au_output.c の cc ビルド + AudioUnit/AudioToolbox/CoreAudio リンク |
| Modify: `cef-unity-unityproject/Assets/CefUnity/Interop/CefUnity.cs` と `cef-unity-csharp/Interop/CefUnity.cs` | Browser に StartNativeAudio / StopNativeAudio / SetNativeAudioVolume / TryGetNativeAudioStats |
| Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefNativeAudio.cs` | MonoBehaviour: ストリーム開始検出 → native start、AudioListener.volume/pause 同期、`[CefAudio-NAT]` 診断ログ、チャネル数変化で再起動 |
| Modify: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs` | `AudioRendererMode` enum (UnityMixer / Native) で経路切替、teardown 順序 |

データフロー (Native 経路):

```
CEF AudioHandler → AudioShmWriter (server, 変更なし)
    → [SHM ring]
        → PullCtx.pull_into (AU render callback, 2.9ms 周期)
            reader.read で全量ドレイン → AudioRing.write
            AudioRing.read (baseStep=1.0, steering) → out
        → 音量乗算 (au_output.c) → CoreAudio → スピーカー
既存 cef_unity_read_audio (録画 tap) は独立カーソルで並行動作 (無変更)
```

---

### Task 1: `audio_ring.rs` — steering リングの Rust 移植

**Files:**
- Create: `cef-unity-rust/crates/client/src/audio_ring.rs`
- Modify: `cef-unity-rust/crates/client/src/lib.rs` (mod 宣言 1 行)

**Interfaces:**
- Consumes: なし (純ロジック)
- Produces: `AudioRing::new(capacity_frames: usize, channels: usize, target_frames: usize, max_rate_adjust: f64)`, `write(&mut self, src: &[f32], frame_count: usize)`, `read(&mut self, dst: &mut [f32], frame_count: usize, base_step: f64)`, `occupancy_frames(&self) -> f64`, `target_frames(&self) -> usize`, pub フィールド `underrun_frames: u64` / `overflow_drop_frames: u64` — Task 3 が使用

移植元: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefAudioRing.cs` (ロジック同一)。native では producer/consumer が同一コールバックスレッドなのでロックを持たない点だけが差分。

- [ ] **Step 1: mod 宣言を追加**

`crates/client/src/lib.rs` の先頭 mod 群 (`#[cfg(target_os = "windows")] mod d3d11;` の直後) に追加:

```rust
#[cfg(target_os = "macos")]
mod audio_ring;
```

- [ ] **Step 2: 失敗するテストを書く**

`crates/client/src/audio_ring.rs` を新規作成し、まず骨組み (`AudioRing` 未実装のまま) + テストを書く。ファイル全体:

```rust
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
    pub fn write(&mut self, src: &[f32], frame_count: usize) {
        todo!()
    }

    /// consumer: dst を frame_count フレーム分 (interleaved) 埋める。
    /// base_step = srcRate/outRate (出力1フレームあたり進める src フレーム数)。
    /// 滞留量が目標から外れていれば step を ±max_rate_adjust だけ操作して収束させる。
    /// データ不足時は無音で埋め underrun_frames を加算する。
    pub fn read(&mut self, dst: &mut [f32], frame_count: usize, base_step: f64) {
        todo!()
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
        assert!(max_disc < 0.05, "出力に不連続 (クリック) があってはならない: {}", max_disc);
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
```

- [ ] **Step 3: テストが失敗する (todo! で panic する) ことを確認**

Run: `cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-rust && cargo test -p cef-unity-client audio_ring`
Expected: FAIL (`not yet implemented` panic × 7)

- [ ] **Step 4: write / read を実装**

`todo!()` を置き換える。C# 版と同一ロジック:

```rust
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
```

- [ ] **Step 5: テストが通ることを確認**

Run: `cargo test -p cef-unity-client audio_ring`
Expected: PASS (7 tests)

- [ ] **Step 6: コミット**

```bash
git add cef-unity-rust/crates/client/src/audio_ring.rs cef-unity-rust/crates/client/src/lib.rs
git commit -m "feat: AudioRing (CefAudioRing の Rust 移植, native voice 用)"
```

---

### Task 2: `au_output.c` + Rust バインディング + build.rs

**Files:**
- Create: `cef-unity-rust/crates/client/src/au_output.c`
- Create: `cef-unity-rust/crates/client/src/au_output.rs`
- Modify: `cef-unity-rust/crates/client/build.rs`
- Modify: `cef-unity-rust/crates/client/src/lib.rs` (mod 宣言 1 行)

**Interfaces:**
- Consumes: なし
- Produces (Task 3 が使用):
  - `pub type AuPullFn = unsafe extern "C" fn(ctx: *mut c_void, out: *mut f32, frames: i32) -> i32`
  - `pub unsafe fn au_output_start(src_rate: f64, channels: i32, io_frames: i32, pull: AuPullFn, ctx: *mut c_void) -> *mut c_void` (失敗時 NULL)
  - `pub unsafe fn au_output_stop(handle: *mut c_void)` (排水待ちして返る。返った後 pull は二度と呼ばれない)
  - `pub unsafe fn au_output_set_volume(handle: *mut c_void, volume: f32)`
- pull 契約: **out を必ず frames フレーム分 (interleaved) 埋める** (不足は無音埋め)。戻り値は実データフレーム数 (現状未使用)

- [ ] **Step 1: au_output.c を作成**

```c
// AudioUnit (DefaultOutput) 出力シム。
// Unity の FMOD ミキサを迂回して CoreAudio に直結する低遅延経路 (CRI 方式)。
// AudioUnit は C API なので Obj-C 不要。metal_texture.m と同じ cc ビルドパターン。
//
// スレッド/ライフサイクル契約:
// - pull は CoreAudio render callback スレッドから呼ばれる (io_frames ごと ≈ 2.9ms@128)。
// - au_output_stop は detached フラグ → Stop → 実行中 callback の排水待ち、の順で
//   同期停止する。返った後 pull は二度と呼ばれないので ctx を安全に解放できる。
#include <AudioUnit/AudioUnit.h>
#include <CoreAudio/CoreAudio.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t (*au_pull_fn)(void* ctx, float* out, int32_t frames);

typedef struct {
    AudioUnit unit;
    au_pull_fn pull;
    void* ctx;
    _Atomic float volume;
    atomic_int detached;
    atomic_int active;
} au_output_t;

static OSStatus au_render(void* ref, AudioUnitRenderActionFlags* flags,
                          const AudioTimeStamp* ts, UInt32 bus,
                          UInt32 frames, AudioBufferList* io) {
    (void)flags; (void)ts; (void)bus;
    au_output_t* h = (au_output_t*)ref;
    float* out = (float*)io->mBuffers[0].mData;
    UInt32 samples = frames * io->mBuffers[0].mNumberChannels;

    atomic_fetch_add_explicit(&h->active, 1, memory_order_acquire);
    if (atomic_load_explicit(&h->detached, memory_order_acquire)) {
        memset(out, 0, samples * sizeof(float));
    } else {
        h->pull(h->ctx, out, (int32_t)frames);
        float v = atomic_load_explicit(&h->volume, memory_order_relaxed);
        if (v != 1.0f) {
            for (UInt32 i = 0; i < samples; i++) out[i] *= v;
        }
    }
    atomic_fetch_sub_explicit(&h->active, 1, memory_order_release);
    return noErr;
}

void* au_output_start(double src_rate, int32_t channels, int32_t io_frames,
                      au_pull_fn pull, void* ctx) {
    AudioComponentDescription desc;
    memset(&desc, 0, sizeof(desc));
    desc.componentType = kAudioUnitType_Output;
    // DefaultOutput はデフォルトデバイスの切替に自動追従する。
    desc.componentSubType = kAudioUnitSubType_DefaultOutput;
    desc.componentManufacturer = kAudioUnitManufacturer_Apple;
    AudioComponent comp = AudioComponentFindNext(NULL, &desc);
    if (!comp) return NULL;

    au_output_t* h = (au_output_t*)calloc(1, sizeof(au_output_t));
    if (!h) return NULL;
    h->pull = pull;
    h->ctx = ctx;
    atomic_store(&h->volume, 1.0f);

    if (AudioComponentInstanceNew(comp, &h->unit) != noErr) {
        free(h);
        return NULL;
    }

    // 入力スコープに src フォーマットを設定 → AU 内蔵コンバータがデバイスレートへ
    // 変換する (手動 SRC 不要。残るは ppm ドリフトのみで、それは steering が吸収)。
    AudioStreamBasicDescription fmt;
    memset(&fmt, 0, sizeof(fmt));
    fmt.mSampleRate = src_rate;
    fmt.mFormatID = kAudioFormatLinearPCM;
    fmt.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
    fmt.mFramesPerPacket = 1;
    fmt.mChannelsPerFrame = (UInt32)channels;
    fmt.mBitsPerChannel = 32;
    fmt.mBytesPerFrame = (UInt32)channels * 4;
    fmt.mBytesPerPacket = (UInt32)channels * 4;
    if (AudioUnitSetProperty(h->unit, kAudioUnitProperty_StreamFormat,
                             kAudioUnitScope_Input, 0, &fmt, sizeof(fmt)) != noErr) {
        AudioComponentInstanceDispose(h->unit);
        free(h);
        return NULL;
    }

    // IO バッファフレーム数。デバイス共有の設定なので他アプリの callback 周期にも
    // 影響する。失敗してもデバイス既定サイズで動くので続行。
    UInt32 io_size = (UInt32)io_frames;
    AudioUnitSetProperty(h->unit, kAudioDevicePropertyBufferFrameSize,
                         kAudioUnitScope_Global, 0, &io_size, sizeof(io_size));

    AURenderCallbackStruct cb;
    cb.inputProc = au_render;
    cb.inputProcRefCon = h;
    if (AudioUnitSetProperty(h->unit, kAudioUnitProperty_SetRenderCallback,
                             kAudioUnitScope_Input, 0, &cb, sizeof(cb)) != noErr ||
        AudioUnitInitialize(h->unit) != noErr) {
        AudioComponentInstanceDispose(h->unit);
        free(h);
        return NULL;
    }
    if (AudioOutputUnitStart(h->unit) != noErr) {
        AudioUnitUninitialize(h->unit);
        AudioComponentInstanceDispose(h->unit);
        free(h);
        return NULL;
    }
    return h;
}

void au_output_stop(void* handle) {
    au_output_t* h = (au_output_t*)handle;
    if (!h) return;
    // DetachAndWait: 以降の callback は pull せず無音 → Stop → 実行中 callback の排水待ち。
    atomic_store_explicit(&h->detached, 1, memory_order_release);
    AudioOutputUnitStop(h->unit);
    while (atomic_load_explicit(&h->active, memory_order_acquire) != 0) {
        // callback は µs オーダーなので実質即時に抜ける。
    }
    AudioUnitUninitialize(h->unit);
    AudioComponentInstanceDispose(h->unit);
    free(h);
}

void au_output_set_volume(void* handle, float v) {
    au_output_t* h = (au_output_t*)handle;
    if (!h) return;
    atomic_store_explicit(&h->volume, v, memory_order_relaxed);
}
```

- [ ] **Step 2: au_output.rs (バインディング + スモークテスト) を作成**

```rust
//! au_output.c (AudioUnit DefaultOutput シム) の Rust バインディング。
//! pull 契約: out を必ず frames フレーム分 (interleaved) 埋めること。

use std::ffi::c_void;

pub type AuPullFn = unsafe extern "C" fn(ctx: *mut c_void, out: *mut f32, frames: i32) -> i32;

unsafe extern "C" {
    /// AU を起動し再生を開始する。失敗時は NULL。
    pub fn au_output_start(
        src_rate: f64,
        channels: i32,
        io_frames: i32,
        pull: AuPullFn,
        ctx: *mut c_void,
    ) -> *mut c_void;
    /// 同期停止。返った後 pull は二度と呼ばれない (排水待ち済み)。
    pub fn au_output_stop(handle: *mut c_void);
    pub fn au_output_set_volume(handle: *mut c_void, volume: f32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static PULL_COUNT: AtomicU64 = AtomicU64::new(0);

    unsafe extern "C" fn sine_pull(_ctx: *mut c_void, out: *mut f32, frames: i32) -> i32 {
        let n = PULL_COUNT.fetch_add(1, Ordering::Relaxed);
        let buf = unsafe { std::slice::from_raw_parts_mut(out, frames as usize * 2) };
        for f in 0..frames as usize {
            let t = (n * frames as u64 + f as u64) as f64 / 48000.0;
            let s = (2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32 * 0.1;
            buf[f * 2] = s;
            buf[f * 2 + 1] = s;
        }
        frames
    }

    /// 実機スモーク: 440Hz を 300ms 鳴らして止める。オーディオデバイスが必要なので
    /// 通常の cargo test では走らせない。手動実行:
    /// `cargo test -p cef-unity-client au_smoke -- --ignored`
    #[test]
    #[ignore]
    fn au_smoke_start_pull_stop() {
        PULL_COUNT.store(0, Ordering::Relaxed);
        let h = unsafe {
            au_output_start(48000.0, 2, 128, sine_pull, std::ptr::null_mut())
        };
        assert!(!h.is_null(), "au_output_start が失敗した");
        unsafe { au_output_set_volume(h, 0.5) };
        std::thread::sleep(std::time::Duration::from_millis(300));
        unsafe { au_output_stop(h) };
        let pulls = PULL_COUNT.load(Ordering::Relaxed);
        // 128 フレーム @48kHz ≈ 2.67ms 周期 → 300ms で ~112 回。半分以上あれば動作している。
        assert!(pulls > 50, "pull 回数が少なすぎる: {}", pulls);
    }
}
```

- [ ] **Step 3: build.rs に cc ビルドとフレームワークリンクを追加**

`#[cfg(target_os = "macos")]` ブロック内 (metal_texture の compile 後) に追加:

```rust
        cc::Build::new()
            .file("src/au_output.c")
            .compile("au_output");
        println!("cargo:rustc-link-lib=framework=AudioUnit");
        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
```

- [ ] **Step 4: lib.rs に mod 宣言を追加**

`mod audio_ring;` の直後:

```rust
#[cfg(target_os = "macos")]
mod au_output;
```

- [ ] **Step 5: ビルド確認 + 実機スモーク**

Run: `cargo build -p cef-unity-client && cargo test -p cef-unity-client au_smoke -- --ignored`
Expected: ビルド成功、`au_smoke_start_pull_stop ... ok` (内蔵スピーカーから 300ms の 440Hz が鳴る)

※ サンドボックス環境で CoreAudio アクセスが拒否される場合は権限昇格して実行。

- [ ] **Step 6: コミット**

```bash
git add cef-unity-rust/crates/client/src/au_output.c cef-unity-rust/crates/client/src/au_output.rs cef-unity-rust/crates/client/build.rs cef-unity-rust/crates/client/src/lib.rs
git commit -m "feat: AudioUnit 出力シム au_output.c (CRI 方式の出力バックエンド)"
```

---

### Task 3: `native_voice.rs` — NativeVoice 本体

**Files:**
- Create: `cef-unity-rust/crates/client/src/native_voice.rs`
- Modify: `cef-unity-rust/crates/client/src/lib.rs` (mod 宣言 1 行)

**Interfaces:**
- Consumes: `AudioRing` (Task 1), `au_output` (Task 2), `cef_unity_ipc::{AudioShmReader, AUDIO_MAX_CHANNELS}` (既存)
- Produces (Task 4 が使用):
  - `NativeVoice::start(flink: &str, target_ms: f32, io_frames: i32) -> Result<NativeVoice, String>`
  - `NativeVoice::set_volume(&self, v: f32)`
  - `NativeVoice::stats(&self) -> (f32, u64, u64)` — (occupancy_ms, underrun_frames, overflow_frames)
  - `Drop` が排水待ち付き stop (drop 後に callback は走らない)

- [ ] **Step 1: lib.rs に mod 宣言を追加**

```rust
#[cfg(target_os = "macos")]
mod native_voice;
```

- [ ] **Step 2: native_voice.rs を骨組み + テストで作成**

`PullCtx::pull_into` を `todo!()` にした状態でテストを先に書く。ファイル全体:

```rust
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
        todo!()
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
        Ok(NativeVoice { ctx, au, stats, src_rate })
    }

    /// AU 起動を除いた初期化 (テストからも使う)。
    fn prepare(flink: &str, target_ms: f32) -> Result<(Box<PullCtx>, Arc<VoiceStats>, u32), String> {
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

    // テストごとに一意な flink (プロセス内カウンタ + pid)。
    fn temp_flink(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("cef-unity-audio-test-{}-{}", std::process::id(), tag))
            .to_str()
            .unwrap()
            .to_string()
    }

    // planar packet (全サンプル同値) を書き込む。
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
```

- [ ] **Step 3: テストが失敗することを確認**

Run: `cargo test -p cef-unity-client native_voice`
Expected: `prepare_fails_before_stream_start` は PASS、pull 系 4 テストが FAIL (`not yet implemented`)

- [ ] **Step 4: pull_into を実装**

```rust
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
```

- [ ] **Step 5: テストが通ることを確認**

Run: `cargo test -p cef-unity-client native_voice`
Expected: PASS (5 tests)

- [ ] **Step 6: コミット**

```bash
git add cef-unity-rust/crates/client/src/native_voice.rs cef-unity-rust/crates/client/src/lib.rs
git commit -m "feat: NativeVoice (独立カーソル SHM リーダー + steering + AudioUnit)"
```

---

### Task 4: lib.rs FFI 4本 + destroy 順序

**Files:**
- Modify: `cef-unity-rust/crates/client/src/lib.rs`
  - `ClientBrowserInstance` (123 行付近)
  - `cef_unity_create_browser` (430 行付近の instance 構築)
  - `cef_unity_destroy_browser` (450 行付近) / `cef_unity_destroy_browser_blocking` (875 行付近)
  - Audio セクション末尾 (852 行付近、`cef_unity_read_audio` の後) に FFI 4本

**Interfaces:**
- Consumes: `NativeVoice` (Task 3)
- Produces (csbindgen 経由で C# の NativeMethods に生成される):
  - `cef_unity_audio_native_start(handle, target_ms: f32, io_frames: i32) -> i32` (0=成功, -1=失敗)
  - `cef_unity_audio_native_stop(handle)`
  - `cef_unity_audio_native_set_volume(handle, volume: f32)`
  - `cef_unity_audio_native_stats(handle, out_occupancy_ms: *mut f32, out_underrun_frames: *mut u64, out_overflow_frames: *mut u64) -> i32` (0=再生中, -1=停止中)

- [ ] **Step 1: ClientBrowserInstance にフィールドを追加**

```rust
struct ClientBrowserInstance {
    browser_id: u32,
    shm: ShmReader,
    /// 音声リングバッファのリーダー。サーバーが flink を返さなかった場合や
    /// open に失敗した場合は None (音声無効)。
    audio_shm: Option<AudioShmReader>,
    /// 音声リングの flink。NativeVoice が独立カーソルの自前リーダーを開くのに使う。
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    audio_flink: String,
    /// CRI 方式ネイティブ音声出力 (macOS)。Unity ミキサを迂回して AudioUnit で再生。
    #[cfg(target_os = "macos")]
    native_voice: Option<native_voice::NativeVoice>,
}
```

- [ ] **Step 2: create_browser の instance 構築を更新**

`cef_unity_create_browser` 内 (430 行付近):

```rust
            let instance = Box::new(ClientBrowserInstance {
                browser_id,
                shm,
                audio_shm,
                audio_flink: audio_shm_flink.clone(),
                #[cfg(target_os = "macos")]
                native_voice: None,
            });
```

※ `audio_shm_flink` は match バインディングで既に手元にある (388 行付近)。ログ出力 (394 行) が先に借用しているので `.clone()` を使う。

- [ ] **Step 3: destroy 2 関数の先頭で voice を停止**

ヘルパーを `handle_to_ref` の直後に追加:

```rust
/// ネイティブ音声を停止する (排水待ち)。destroy の先頭で呼ぶこと —
/// NativeVoice は自前 reader/Shmem を持ち instance と参照関係がないため、
/// stop (排水待ち) さえ済めば以降の解放順序で UAF は構造的に起きない。
fn stop_native_voice(instance: &mut ClientBrowserInstance) {
    #[cfg(target_os = "macos")]
    {
        instance.native_voice.take();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = instance;
    }
}
```

`cef_unity_destroy_browser` (450 行付近) — `Box::from_raw` 直後に挿入:

```rust
    let mut instance = unsafe { Box::from_raw(handle as *mut ClientBrowserInstance) };
    stop_native_voice(&mut instance);
```

`cef_unity_destroy_browser_blocking` (875 行付近) も同様:

```rust
    let mut instance = unsafe { Box::from_raw(handle as *mut ClientBrowserInstance) };
    stop_native_voice(&mut instance);
```

- [ ] **Step 4: FFI 4本を追加**

`cef_unity_read_audio` の直後 (852 行付近):

```rust
// ---------------------------------------------------------------------------
// Audio: ネイティブ出力 (CRI 方式)。Unity ミキサを迂回して OS オーディオ API に直結。
// 現状 macOS (AudioUnit) のみ。非対応 OS では start が -1 を返す。
// ---------------------------------------------------------------------------

/// ネイティブ音声出力を開始する。
/// 既存の `cef_unity_read_audio` (録画 tap) とはリングカーソルが独立しており併用可。
/// CefAudioOutput (Unity ミキサ再生) と同時に有効にすると二重再生になる。
///
/// `target_ms`: jitter buffer の目標滞留量 (推奨 15)。
/// `io_frames`: CoreAudio IO バッファフレーム数 (推奨 128 ≈ 2.9ms)。0 以下は 128。
/// 戻り値: 0=成功 (既に再生中も 0)、-1=失敗 (音声無効・フォーマット未確定・
/// AU 起動失敗・非対応 OS)。フォーマット未確定で失敗するため、呼び出し側は
/// `cef_unity_get_audio_format` が 1 を返してから呼ぶこと。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_audio_native_start(
    handle: *mut CefUnityBrowser,
    target_ms: f32,
    io_frames: i32,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    #[cfg(target_os = "macos")]
    {
        let instance = handle_to_ref(handle);
        if instance.native_voice.is_some() {
            return 0;
        }
        if instance.audio_flink.is_empty() {
            return -1;
        }
        match native_voice::NativeVoice::start(&instance.audio_flink, target_ms, io_frames) {
            Ok(v) => {
                instance.native_voice = Some(v);
                log_to_file(&format!(
                    "native audio started (target={}ms io_frames={})",
                    target_ms, io_frames
                ));
                0
            }
            Err(e) => {
                log_to_file(&format!("native audio start failed: {}", e));
                -1
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (target_ms, io_frames);
        -1
    }
}

/// ネイティブ音声出力を停止する (排水待ちして返る)。未開始なら何もしない。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_audio_native_stop(handle: *mut CefUnityBrowser) {
    if handle.is_null() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let instance = handle_to_ref(handle);
        if instance.native_voice.take().is_some() {
            log_to_file("native audio stopped");
        }
    }
}

/// ネイティブ音声出力の音量 (0.0〜)。callback 内で乗算される。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_audio_native_set_volume(handle: *mut CefUnityBrowser, volume: f32) {
    if handle.is_null() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let instance = handle_to_ref(handle);
        if let Some(v) = instance.native_voice.as_ref() {
            v.set_volume(volume);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = volume;
    }
}

/// ネイティブ音声出力の診断値を取得する。
/// `out_occupancy_ms`: jitter buffer の滞留量 (ms)。
/// `out_underrun_frames` / `out_overflow_frames`: 累積フレーム数。
/// 戻り値: 0=再生中、-1=停止中/非対応 OS (out には書き込まない)。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_audio_native_stats(
    handle: *mut CefUnityBrowser,
    out_occupancy_ms: *mut f32,
    out_underrun_frames: *mut u64,
    out_overflow_frames: *mut u64,
) -> i32 {
    if handle.is_null() {
        return -1;
    }
    #[cfg(target_os = "macos")]
    {
        let instance = handle_to_ref(handle);
        let Some(v) = instance.native_voice.as_ref() else {
            return -1;
        };
        let (occ_ms, under, over) = v.stats();
        unsafe {
            if !out_occupancy_ms.is_null() {
                *out_occupancy_ms = occ_ms;
            }
            if !out_underrun_frames.is_null() {
                *out_underrun_frames = under;
            }
            if !out_overflow_frames.is_null() {
                *out_overflow_frames = over;
            }
        }
        0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (out_occupancy_ms, out_underrun_frames, out_overflow_frames);
        -1
    }
}
```

- [ ] **Step 5: ビルド + 全テスト + バインディング生成確認**

Run: `cargo build && cargo test -p cef-unity-client && cargo test -p cef-unity-ipc`
Expected: 全て PASS。続けて:

Run: `git diff --stat -- ../cef-unity-csharp/Interop/NativeMethods.g.cs ../cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs` (リポジトリルートからは `git diff --stat -- cef-unity-csharp/Interop/NativeMethods.g.cs cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs`)
Expected: 両ファイルに `cef_unity_audio_native_*` 4 関数が追加されている (grep で確認: `grep -c cef_unity_audio_native cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs` → 4)

- [ ] **Step 6: コミット**

```bash
git add cef-unity-rust/crates/client/src/lib.rs cef-unity-csharp/Interop/NativeMethods.g.cs cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs
git commit -m "feat: ネイティブ音声出力 FFI (start/stop/set_volume/stats) + destroy 時の排水停止"
```

---

### Task 5: C# Browser API (両プロジェクト)

**Files:**
- Modify: `cef-unity-unityproject/Assets/CefUnity/Interop/CefUnity.cs` (`ReadAudio` の直後、`// ----- IME -----` の前、590 行付近)
- Modify: `cef-unity-csharp/Interop/CefUnity.cs` (同じ位置関係。namespace が `Interop` である点だけ異なり、追加コードは同一)

**Interfaces:**
- Consumes: `NativeMethods.cef_unity_audio_native_*` (Task 4 で自動生成済み)
- Produces (Task 6 が使用):
  - `public bool StartNativeAudio(float targetLatencyMs = 15f, int ioFrames = 128)`
  - `public void StopNativeAudio()`
  - `public void SetNativeAudioVolume(float volume)`
  - `public unsafe bool TryGetNativeAudioStats(out float occupancyMs, out ulong underrunFrames, out ulong overflowFrames)`

- [ ] **Step 1: 両方の CefUnity.cs にメソッドを追加**

`ReadAudio` メソッドの閉じ括弧と `// ----- IME -----` の間に挿入 (両ファイル同一内容):

```csharp
        // ----- ネイティブ音声出力 (CRI 方式) -----

        /// <summary>
        ///     ネイティブ音声出力を開始する。Unity の FMOD ミキサを迂回して OS の
        ///     オーディオ API (macOS: AudioUnit) に直結する低遅延経路 (内部 ~30ms 級。
        ///     <c>CefAudioOutput</c> 経由の Unity ミキサ経路は ~160ms)。
        ///     <para>
        ///     ストリームフォーマット確定後 (<see cref="TryGetAudioFormat" /> が true)
        ///     に呼ぶこと。<c>CefAudioOutput</c> と同時に使うと二重再生になる。
        ///     AudioMixer エフェクト・スペーシャライズ等 Unity ミキサの機能は効かない。
        ///     PCM の取得 (<see cref="ReadAudio" />) はカーソル独立なので併用できる。
        ///     </para>
        /// </summary>
        /// <param name="targetLatencyMs">jitter buffer の目標滞留量 (ms)。</param>
        /// <param name="ioFrames">CoreAudio IO バッファフレーム数 (128 ≈ 2.9ms)。</param>
        /// <returns>開始できたら true (既に再生中も true)。非対応 OS・音声無効・フォーマット未確定は false。</returns>
        public bool StartNativeAudio(float targetLatencyMs = 15f, int ioFrames = 128)
        {
            ThrowIfDisposed();
            return NativeMethods.cef_unity_audio_native_start(_handle, targetLatencyMs, ioFrames) == 0;
        }

        /// <summary>ネイティブ音声出力を停止する (再生していなければ何もしない)。</summary>
        public void StopNativeAudio()
        {
            ThrowIfDisposed();
            NativeMethods.cef_unity_audio_native_stop(_handle);
        }

        /// <summary>ネイティブ音声出力の音量 (0.0〜1.0)。AudioListener とは独立。</summary>
        public void SetNativeAudioVolume(float volume)
        {
            ThrowIfDisposed();
            NativeMethods.cef_unity_audio_native_set_volume(_handle, volume);
        }

        /// <summary>
        ///     ネイティブ音声出力の診断値を取得する。再生中でなければ false。
        ///     underrun/overflow は累積フレーム数 (0 以外ならぶつ切り/破棄発生)。
        /// </summary>
        public unsafe bool TryGetNativeAudioStats(
            out float occupancyMs, out ulong underrunFrames, out ulong overflowFrames)
        {
            ThrowIfDisposed();
            float occ;
            ulong under, over;
            int ok = NativeMethods.cef_unity_audio_native_stats(_handle, &occ, &under, &over);
            occupancyMs = occ;
            underrunFrames = under;
            overflowFrames = over;
            return ok == 0;
        }
```

※ `NativeMethods.g.cs` の実際の生成シグネチャ (ポインタ型 `float*`/`ulong*` か `out` 引数か) を確認し、呼び出し側をそれに合わせること。既存の `cef_unity_get_audio_format` 呼び出し (560 行付近) と同じ流儀になるはず。

- [ ] **Step 2: Unity コンパイル確認**

Run: uloop-compile スキル (Unity プロジェクトのコンパイル)
Expected: エラー 0

cef-unity-csharp 側は `dotnet build` がある場合のみ: `cd cef-unity-csharp && dotnet build` (環境になければ skip し、diff の同一性を目視確認)

- [ ] **Step 3: コミット**

```bash
git add cef-unity-unityproject/Assets/CefUnity/Interop/CefUnity.cs cef-unity-csharp/Interop/CefUnity.cs
git commit -m "feat: Browser にネイティブ音声出力 API (Start/Stop/SetVolume/Stats)"
```

---

### Task 6: CefNativeAudio コンポーネント + サンプルの renderer 切替

**Files:**
- Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefNativeAudio.cs`
- Modify: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs`
  - Audio ヘッダ部 (89-98 行付近): enum + フィールド追加
  - `ApplyAudioDspBufferSize` 呼び出し (278 行付近): Native モードではスキップ
  - `SetupAudioOutput` (759 行付近): 分岐
  - `OnDestroy` (440-445 行付近): native 停止を dispose 前に

**Interfaces:**
- Consumes: `Browser.StartNativeAudio` ほか (Task 5)
- Produces: `CefNativeAudio` — `public Browser Browser { get; set; }`, `public bool IsPlaying { get; }`。ログタグ `[CefAudio-NAT]`

- [ ] **Step 1: CefNativeAudio.cs を作成**

```csharp
using System;
using CefUnity.Interop;
using UnityEngine;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     CRI 方式のネイティブ音声出力を管理するコンポーネント (macOS)。
    ///     <para>
    ///     Unity の FMOD ミキサを迂回し、client dylib 内の AudioUnit で直接再生する
    ///     低遅延経路 (内部 ~30ms 級。<see cref="CefAudioOutput" /> の Unity ミキサ経路は
    ///     ~160ms)。任意の GameObject にアタッチして <see cref="Browser" /> を設定すると、
    ///     ストリーム開始を検出して自動的にネイティブ再生を開始する。
    ///     </para>
    ///     <para>
    ///     注意:
    ///     - <see cref="CefAudioOutput" /> と併用すると二重再生になる (どちらか一方のみ)。
    ///     - AudioMixer エフェクト・スペーシャライズ・オーディオプロファイラ表示は効かない。
    ///     - AudioListener.volume / pause は Update で音量に反映する。
    ///     - PCM の取得 (録画等) は <see cref="Browser.ReadAudio" /> がカーソル独立で併用可能。
    ///     </para>
    /// </summary>
    public class CefNativeAudio : MonoBehaviour
    {
        [SerializeField] [Range(0f, 1f)] private float _volume = 1f;

        [Tooltip("jitter buffer の目標滞留量 (ms)。下げるほど低遅延だがスパイクに弱い")]
        [SerializeField] private float _targetLatencyMs = 15f;

        [Tooltip("CoreAudio IO バッファフレーム数 (128 ≈ 2.9ms)。デバイス共有設定なので他アプリにも影響する")]
        [SerializeField] private int _ioFrames = 128;

        /// <summary>再生対象のブラウザ。外部から設定する。</summary>
        public Browser Browser { get; set; }

        /// <summary>ネイティブ再生中か。</summary>
        public bool IsPlaying { get; private set; }

        private int _srcChannels;
        private float _lastSentVolume = float.NaN;
        private float _diagTimer;
        private ulong _lastUnderrun;
        private ulong _lastOverflow;

        private void Update()
        {
            if (Browser == null) return;

            if (!IsPlaying)
            {
                TryStart();
                return;
            }

            // チャネル数が変わったら再起動 (native 側は無音を出して待っている)。
            // Browser dispose 直後の 1 フレームと競合し得るので防御的に握りつぶす
            // (サンプルは Browser=null → dispose の順序を保証するが、単体利用に備える)。
            try
            {
                if (Browser.TryGetAudioFormat(out _, out int ch) && ch > 0 && ch != _srcChannels)
                {
                    Stop();
                    TryStart();
                    return;
                }

                SyncVolume();
                LogDiagnostics();
            }
            catch (ObjectDisposedException)
            {
                IsPlaying = false;
            }
        }

        /// <summary>ストリームフォーマットが確定していればネイティブ再生を開始する。</summary>
        private void TryStart()
        {
            bool active;
            int sampleRate, channels;
            try
            {
                active = Browser.TryGetAudioFormat(out sampleRate, out channels);
            }
            catch (Exception)
            {
                return;
            }

            if (!active || sampleRate <= 0 || channels <= 0) return;
            if (!Browser.StartNativeAudio(_targetLatencyMs, _ioFrames)) return;

            _srcChannels = channels;
            IsPlaying = true;
            _lastSentVolume = float.NaN; // 次の Update で必ず音量を送る
            if (CefLog.Enabled)
                CefLog.Log($"[CefAudio-NAT] start {sampleRate}Hz x{channels}ch " +
                           $"target={_targetLatencyMs}ms ioFrames={_ioFrames}");
        }

        // AudioListener の音量/ポーズと Inspector 音量を native 側へ同期する。
        private void SyncVolume()
        {
            float v = AudioListener.pause ? 0f : AudioListener.volume * _volume;
            if (!float.IsNaN(_lastSentVolume) && Mathf.Approximately(v, _lastSentVolume)) return;
            try
            {
                Browser.SetNativeAudioVolume(v);
                _lastSentVolume = v;
            }
            catch (Exception)
            {
                // Browser dispose 直後の 1 フレームで来得る。次フレームで IsPlaying が落ちる。
            }
        }

        // 1 秒ごとに滞留量とアンダーラン/オーバーフローをログ出力する診断。
        private void LogDiagnostics()
        {
            if (!CefLog.Enabled) return;
            _diagTimer += Time.unscaledDeltaTime;
            if (_diagTimer < 1f) return;
            _diagTimer = 0f;

            if (!Browser.TryGetNativeAudioStats(out float occMs, out ulong under, out ulong over))
                return;
            ulong underDelta = under - _lastUnderrun;
            ulong overDelta = over - _lastOverflow;
            _lastUnderrun = under;
            _lastOverflow = over;
            CefLog.Log(
                $"[CefAudio-NAT] occ={occMs:F1}ms (target={_targetLatencyMs:F1}ms) | " +
                $"underrun/s={underDelta} overflow/s={overDelta} (total under={under} over={over})");
        }

        private void Stop()
        {
            if (!IsPlaying) return;
            IsPlaying = false;
            try
            {
                Browser?.StopNativeAudio();
            }
            catch (Exception)
            {
                // Browser が先に dispose されていても Rust 側 destroy が voice を停止済み。
            }
        }

        private void OnDisable()
        {
            Stop();
        }
    }
}
```

- [ ] **Step 2: CefUnityBrowserSample.cs に renderer 切替を追加**

(a) Audio ヘッダ部 (89 行付近の `[Header("Audio")]` ブロック) に enum とフィールドを追加:

```csharp
        /// <summary>音声レンダラの選択。</summary>
        public enum AudioRendererMode
        {
            /// <summary>Unity AudioSource (FMOD ミキサ) で再生。ミキサ統合 (エフェクト・スペーシャライズ) が効くが遅延大 (~160ms)。</summary>
            UnityMixer,

            /// <summary>ネイティブ AudioUnit で再生 (macOS)。低遅延 (~30ms) だが Unity ミキサ機能は効かない。</summary>
            Native,
        }
```

`_enableAudio` フィールドの直後に:

```csharp
        [Tooltip("音声レンダラ。UnityMixer=AudioSource 再生 (ミキサ統合, ~160ms) / Native=AudioUnit 直結 (macOS, ~30ms)")]
        [SerializeField] private AudioRendererMode _audioRenderer = AudioRendererMode.UnityMixer;
```

`private CefAudioOutput _audioOutput;` の直後に:

```csharp
        private CefNativeAudio _nativeAudio;
```

(b) 278 行付近 — Native モードでは DSP Reset (全 AudioSource 停止の副作用がある) をスキップ:

変更前:
```csharp
                ApplyAudioDspBufferSize();
                SetupAudioOutput();
```

変更後:
```csharp
                // Native レンダラは FMOD ミキサを使わないので DSP バッファ変更は不要。
                if (_audioRenderer == AudioRendererMode.UnityMixer) ApplyAudioDspBufferSize();
                SetupAudioOutput();
```

(c) `SetupAudioOutput` (759 行付近) を分岐:

変更前:
```csharp
        private void SetupAudioOutput()
        {
            if (!_enableAudio || _browser == null) return;

            _audioOutput = GetComponent<CefAudioOutput>();
            if (_audioOutput == null) _audioOutput = gameObject.AddComponent<CefAudioOutput>();
            _audioOutput.Browser = _browser;
        }
```

変更後:
```csharp
        private void SetupAudioOutput()
        {
            if (!_enableAudio || _browser == null) return;

            if (_audioRenderer == AudioRendererMode.Native)
            {
                _nativeAudio = GetComponent<CefNativeAudio>();
                if (_nativeAudio == null) _nativeAudio = gameObject.AddComponent<CefNativeAudio>();
                _nativeAudio.Browser = _browser;
            }
            else
            {
                _audioOutput = GetComponent<CefAudioOutput>();
                if (_audioOutput == null) _audioOutput = gameObject.AddComponent<CefAudioOutput>();
                _audioOutput.Browser = _browser;
            }
        }
```

(d) `OnDestroy` (440 行付近) — 既存の `_audioOutput` 停止ブロックの直後に追加:

```csharp
            if (_nativeAudio != null)
            {
                // enabled=false の OnDisable で StopNativeAudio が走る (dispose 前に停止)。
                // 仮に順序が崩れても Rust 側 destroy_browser の先頭で voice は停止される。
                _nativeAudio.enabled = false;
                _nativeAudio.Browser = null;
            }
```

- [ ] **Step 3: Unity コンパイル確認**

Run: uloop-compile スキル
Expected: エラー 0

- [ ] **Step 4: コミット**

```bash
git add cef-unity-unityproject/Assets/CefUnity/Runtime/CefNativeAudio.cs cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs
git commit -m "feat: CefNativeAudio コンポーネント + サンプルの音声レンダラ切替"
```

※ Unity が生成する `CefNativeAudio.cs.meta` があれば一緒にコミットする。

---

### Task 7: デプロイ + Unity 実機検証

**Files:** 変更なし (検証のみ)

- [ ] **Step 1: デプロイ**

Run: `cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-rust && bash deploy.sh`
Expected: release ビルド + コピー + codesign 成功

- [ ] **Step 2: Unity Editor 再起動**

dylib 変更のため必須 (Editor は一度ロードした dylib をメモリに保持する)。uloop-launch スキルで再起動する。
性能計測を行うため再起動は必須条件 (5h+ 稼働 Editor は CEF が 20-30fps に劣化する計測の罠もある)。

- [ ] **Step 3: サンプルを Native レンダラへ切替**

uloop-execute-dynamic-code スキルで、シーン上の CefUnityBrowserSample の `_audioRenderer` を `Native` (=1) に設定する (SerializedObject 経由)。または Inspector で手動切替。

- [ ] **Step 4: 440Hz トーンで定常検証**

1. uloop-control-play-mode で Play 開始
2. `$TMPDIR/cef_load_url` に 440Hz トーンの data:URI (WebAudio, gain=0.2) を書いて遷移させる (既存の音声テスト経路)
3. 30 秒以上放置し、uloop-get-logs で `[CefAudio-NAT]` を確認

Expected:
- `[CefAudio-NAT] start 48000Hz x2ch target=15ms ioFrames=128` が出る
- `occ` ≈ 15ms 近傍 (±1 CEF パケット 21.3ms ぶんの変動は正常。プライミング直後は target+パケット量子で ~20-35ms で浮動し得る)
- `underrun/s=0 overflow/s=0` が 30 秒継続
- CefAudioOutput 側の `[CefAudio-LAT]` ログが**出ていない** (二重再生でない証拠)

- [ ] **Step 5: 負荷とライフサイクルの検証**

1. スクロール等の操作負荷をかけて underrun/s=0 が維持されることを確認 (producer がオーディオスレッドに居るのでメインスレッドスパイクの影響を受けないはず)
2. Play 停止 → 再開を 5 回以上繰り返し、クラッシュ・音の欠落がないこと (destroy → voice 排水停止の検証)
3. uloop-get-logs でエラー・例外がないこと

- [ ] **Step 6: 実耳確認 (内蔵スピーカー)**

**必ず内蔵スピーカーか有線で** (WF-C700N 等 BT は単体 +219ms)。YouTube 等の実コンテンツでクラックル・ぶつ切りがないことを確認。

- [ ] **Step 7: メモリ更新 + コミット**

計測結果 (occ 実測値・underrun) をメモリ `audio-latency.md` / `audio-native-output.md` に追記 (実装完了の旨と実測値)。

```bash
git add -A
git commit -m "test: ネイティブ音声出力の実機検証"
```

(検証で修正が入った場合はその修正を含める)

---

## 検証まとめ (受け入れ基準)

| 項目 | 基準 |
|---|---|
| Rust 単体テスト | `cargo test -p cef-unity-client` 全 PASS (audio_ring 7 + native_voice 5) |
| AU スモーク | `cargo test -p cef-unity-client au_smoke -- --ignored` PASS (実音出る) |
| Unity コンパイル | エラー 0 |
| 定常動作 | `[CefAudio-NAT]` で occ≈target・underrun/s=0・overflow/s=0 を 30 秒 |
| 二重再生なし | Native モード時に `[CefAudio-LAT]` (CefAudioOutput) が出ない |
| ライフサイクル | Play 停止/再開 5 回でクラッシュなし |
| 遅延 (参考) | 内部 ~30ms 級見込み (fpb=1024 のままなら +10ms 程度。fpb 512 化 = B 案は本計画のスコープ外) |

## スコープ外 (後続)

- B 案: server.rs `frames_per_buffer` 1024→512 (1 行 + 再デプロイ + Editor 再起動)。native の target 15→12ms 化と合わせて再計測
- Windows (WASAPI IAudioClient3 共有) / Linux (PipeWire): `au_output.c` と同一 API の差し替えのみ。`NativeVoice`・FFI・C# は共通
- 録画トラックへの PCM ミックス (既存 `Browser.ReadAudio` が独立カーソルでそのまま使える)
