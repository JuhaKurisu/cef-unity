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
        let h = unsafe { au_output_start(48000.0, 2, 128, sine_pull, std::ptr::null_mut()) };
        assert!(!h.is_null(), "au_output_start が失敗した");
        unsafe { au_output_set_volume(h, 0.5) };
        std::thread::sleep(std::time::Duration::from_millis(300));
        unsafe { au_output_stop(h) };
        let pulls = PULL_COUNT.load(Ordering::Relaxed);
        // 128 フレーム @48kHz ≈ 2.67ms 周期 → 300ms で ~112 回。半分以上あれば動作している。
        assert!(pulls > 50, "pull 回数が少なすぎる: {}", pulls);
    }
}
