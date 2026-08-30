//! Linux: dmabuf プールの安全なラッパー。C 実装は `dmabuf_pool.c`。
//!
//! CEF の `on_accelerated_paint` が渡す dmabuf はプールの借用で、コールバックを
//! 抜けると再利用される。そのため自前の出力バッファへ blit し、その dmabuf を
//! クライアントへ渡す。macOS の `iosurface_pool.m`、Windows の `d3d11_pool.rs` に相当する。

#![cfg(target_os = "linux")]

use cef_unity_ipc::file_descriptor_channel::DmabufDescriptor;

unsafe extern "C" {
    fn dmabuf_pool_create(failure_stage: *mut std::ffi::c_int) -> *mut std::ffi::c_void;
    fn dmabuf_pool_destroy(pool: *mut std::ffi::c_void);
    fn dmabuf_pool_upload(
        pool: *mut std::ffi::c_void,
        pixels: *const u8,
        width: std::ffi::c_int,
        height: std::ffi::c_int,
    ) -> std::ffi::c_int;
    fn dmabuf_pool_output(
        pool: *mut std::ffi::c_void,
        out_file_descriptor: *mut std::ffi::c_int,
        out_stride: *mut u32,
        out_modifier: *mut u64,
        out_fourcc: *mut u32,
        out_generation: *mut u32,
        out_width: *mut std::ffi::c_int,
        out_height: *mut std::ffi::c_int,
    ) -> std::ffi::c_int;
    fn dmabuf_pool_read_output_pixel(
        pool: *mut std::ffi::c_void,
        x: std::ffi::c_int,
        y: std::ffi::c_int,
        out_rgba: *mut u8,
    ) -> std::ffi::c_int;
}

/// `dmabuf_pool_create` が失敗した段階の名前。C 側の `DMABUF_POOL_STAGE_*` と対応する。
/// 環境不足での失敗は想定内 (software paint に落ちる) なので、原因の切り分け用。
fn failure_stage_name(stage: std::ffi::c_int) -> &'static str {
    match stage {
        1 => "レンダーノードを開けない (/dev/dri/renderD128)",
        2 => "gbm_create_device に失敗",
        3 => "eglGetPlatformDisplayEXT が取れない",
        4 => "eglGetPlatformDisplay(GBM) に失敗",
        5 => "eglInitialize に失敗",
        6 => "eglChooseConfig に失敗",
        7 => "eglCreateContext に失敗",
        8 => "eglMakeCurrent に失敗",
        9 => "EGL/GL の拡張関数が取れない",
        _ => "不明",
    }
}


/// 診断: dmabuf を Vulkan で import して読む。書いた本人 (Vulkan) と同じ経路。

/// 診断: dmabuf を CPU から直接読んで中心ピクセルを返す。

/// EGL / GBM のリソースを束ねたプール。
pub struct DmabufPool {
    handle: *mut std::ffi::c_void,
}

impl DmabufPool {
    /// 構築できなければ `None`。
    ///
    /// レンダーノードが無い、EGL の拡張が足りない、といった環境では失敗する。
    /// 呼び出し側は `None` を受けて software paint に落とす
    /// (Windows の `d3d11_pool.is_some()` と同じ判定の仕方)。
    pub fn create() -> Option<Self> {
        let mut failure_stage: std::ffi::c_int = 0;
        let handle = unsafe { dmabuf_pool_create(&mut failure_stage) };
        if handle.is_null() {
            crate::server::log(&format!(
                "dmabuf プールを構築できないため software paint に落ちる: {}",
                failure_stage_name(failure_stage)
            ));
            None
        } else {
            Some(Self { handle })
        }
    }
}

impl DmabufPool {
    /// CEF の dmabuf を取り込み、出力バッファへ描画して完了を待つ。
    ///

    /// CPU の BGRA ピクセルを出力バッファへアップロードする (半ゼロコピー経路)。
    ///
    /// CEF の software paint の出力を dmabuf 化してクライアントへゼロコピーで渡す。
    /// 出力バッファはサイズが変わったときだけ作り直し、そのとき `generation` が進む。
    pub fn upload(&self, pixels: &[u8], width: u32, height: u32) -> bool {
        if pixels.len() < (width as usize) * (height as usize) * 4 {
            return false;
        }
        let result = unsafe {
            dmabuf_pool_upload(
                self.handle,
                pixels.as_ptr(),
                width as std::ffi::c_int,
                height as std::ffi::c_int,
            )
        };
        result != 0
    }

    /// 出力バッファの現在の記述子。まだ一度も blit していなければ `None`。
    ///
    /// 返る file descriptor は複製されたもので、所有権は呼び出し側に移る。
    pub fn output(&self) -> Option<(std::os::fd::OwnedFd, DmabufDescriptor)> {
        let mut file_descriptor: std::ffi::c_int = -1;
        let mut stride: u32 = 0;
        let mut modifier: u64 = 0;
        let mut fourcc: u32 = 0;
        let mut generation: u32 = 0;
        let mut width: std::ffi::c_int = 0;
        let mut height: std::ffi::c_int = 0;
        let result = unsafe {
            dmabuf_pool_output(
                self.handle,
                &mut file_descriptor,
                &mut stride,
                &mut modifier,
                &mut fourcc,
                &mut generation,
                &mut width,
                &mut height,
            )
        };
        if result == 0 {
            return None;
        }
        Some((
            unsafe { std::os::fd::FromRawFd::from_raw_fd(file_descriptor) },
            DmabufDescriptor {
                width: width as u32,
                height: height as u32,
                stride,
                modifier,
                fourcc,
                generation,
            },
        ))
    }



    /// 診断: CEF から取り込んだ入力テクスチャの中心ピクセル。

    /// 出力バッファの 1 ピクセルを読み戻す。blit が効いているかの診断に使う。
    pub fn read_output_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let mut pixel = [0u8; 4];
        let result = unsafe {
            dmabuf_pool_read_output_pixel(
                self.handle,
                x as std::ffi::c_int,
                y as std::ffi::c_int,
                pixel.as_mut_ptr(),
            )
        };
        if result == 0 { None } else { Some(pixel) }
    }

}

impl Drop for DmabufPool {
    fn drop(&mut self) {
        unsafe { dmabuf_pool_destroy(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn レンダーノードがある環境でだけプールを構築できる() {
        let _guard = lock_gpu_tests();
        // GPU の無い環境 (CI ランナー、コンテナ) では構築できないのが正しい挙動なので、
        // レンダーノードの有無で期待値を切り替える。
        let render_node_exists = std::path::Path::new("/dev/dri/renderD128").exists();
        let pool = DmabufPool::create();
        assert_eq!(pool.is_some(), render_node_exists);
    }

    /// GPU テストを直列化する。file descriptor 数はプロセス全体の値なので、
    /// 並行実行すると他テストが開いた分を数えてしまう。EGL コンテキストを
    /// 複数同時に current にしないためでもある。
    static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_gpu_tests() -> std::sync::MutexGuard<'static, ()> {
        // 別のテストが panic しても後続を止めない。
        GPU_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// GPU の無い環境 (CI ランナー、コンテナ) では検証対象外なので早期に抜ける。
    fn pool_or_skip() -> Option<DmabufPool> {
        if !std::path::Path::new("/dev/dri/renderD128").exists() {
            return None;
        }
        Some(DmabufPool::create().expect("レンダーノードがあるのにプールを構築できない"))
    }



    #[test]
    fn upload_した色が出力バッファから読める() {
        // 半ゼロコピー経路の本体: CPU の BGRA ピクセルを出力 dmabuf へアップロードする。
        let _guard = lock_gpu_tests();
        let Some(pool) = pool_or_skip() else { return };
        // BGRA で (B=255, G=128, R=0, A=255) = 青っぽい色
        let mut pixels = vec![0u8; 64 * 64 * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[0] = 255; // B
            pixel[1] = 128; // G
            pixel[2] = 0;   // R
            pixel[3] = 255; // A
        }
        assert!(pool.upload(&pixels, 64, 64));

        let (_fd, descriptor) = pool.output().expect("出力記述子が取れない");
        assert_eq!(descriptor.width, 64);
        assert_eq!(descriptor.generation, 1);

        let read = pool.read_output_pixel(32, 32).expect("読み戻せない");
        // read_output_pixel は RGBA で返る
        assert!(read[0] <= 2, "R が乗っている: {:?}", read);
        assert!((126..=130).contains(&read[1]), "G が合わない: {:?}", read);
        assert!(read[2] >= 253, "B が合わない: {:?}", read);
    }



    #[test]
    fn サイズが変わると_generation_が進む() {
        let _guard = lock_gpu_tests();
        let Some(pool) = pool_or_skip() else { return };
        let small = vec![0u8; 64 * 64 * 4];
        assert!(pool.upload(&small, 64, 64));
        let (_fd1, first) = pool.output().unwrap();

        let large = vec![0u8; 128 * 128 * 4];
        assert!(pool.upload(&large, 128, 128));
        let (_fd2, second) = pool.output().unwrap();

        assert_eq!(second.generation, first.generation + 1);
        assert_eq!(second.width, 128);
    }

    #[test]
    fn 同じサイズなら_generation_は進まない() {
        let _guard = lock_gpu_tests();
        let Some(pool) = pool_or_skip() else { return };
        let pixels = vec![0u8; 64 * 64 * 4];
        assert!(pool.upload(&pixels, 64, 64));
        let (_fd1, first) = pool.output().unwrap();
        assert!(pool.upload(&pixels, 64, 64));
        let (_fd2, second) = pool.output().unwrap();
        assert_eq!(second.generation, first.generation);
    }

    #[test]
    fn upload_前は出力記述子が取れない() {
        let _guard = lock_gpu_tests();
        let Some(pool) = pool_or_skip() else { return };
        assert!(pool.output().is_none());
    }

    #[test]
    fn 構築と破棄を繰り返してもリークしない() {
        let _guard = lock_gpu_tests();
        if !std::path::Path::new("/dev/dri/renderD128").exists() {
            return;
        }
        let count_open_file_descriptors =
            || std::fs::read_dir("/proc/self/fd").map(|entries| entries.count()).unwrap_or(0);

        // 1 回目で遅延初期化される分を除くため、計測は 2 回目以降で行う。
        drop(DmabufPool::create());
        let before = count_open_file_descriptors();
        for _ in 0..5 {
            drop(DmabufPool::create());
        }
        let after = count_open_file_descriptors();
        assert!(
            after <= before,
            "file descriptor が増えている: {} -> {}",
            before,
            after
        );
    }
}
