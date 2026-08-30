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
    fn dmabuf_pool_blit(
        pool: *mut std::ffi::c_void,
        source_file_descriptor: std::ffi::c_int,
        source_stride: u32,
        source_modifier: u64,
        source_fourcc: u32,
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
    #[cfg(test)]
    fn dmabuf_pool_fill_test_source(
        pool: *mut std::ffi::c_void,
        file_descriptor: std::ffi::c_int,
        stride: u32,
        modifier: u64,
        fourcc: u32,
        width: std::ffi::c_int,
        height: std::ffi::c_int,
        red: u8,
        green: u8,
        blue: u8,
        alpha: u8,
    ) -> std::ffi::c_int;
    #[cfg(test)]
    fn dmabuf_pool_read_output_pixel(
        pool: *mut std::ffi::c_void,
        x: std::ffi::c_int,
        y: std::ffi::c_int,
        out_rgba: *mut u8,
    ) -> std::ffi::c_int;
    #[cfg(test)]
    fn dmabuf_pool_create_test_source(
        pool: *mut std::ffi::c_void,
        width: std::ffi::c_int,
        height: std::ffi::c_int,
        out_file_descriptor: *mut std::ffi::c_int,
        out_stride: *mut u32,
        out_modifier: *mut u64,
        out_fourcc: *mut u32,
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

/// CEF から渡された dmabuf の記述。`on_accelerated_paint` の引数をそのまま写したもの。
pub struct DmabufSource {
    pub file_descriptor: std::os::fd::OwnedFd,
    pub stride: u32,
    pub modifier: u64,
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
}

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
    /// 出力バッファはサイズが変わったときだけ作り直し、そのとき `generation` が進む。
    pub fn blit(&self, source: &DmabufSource) -> bool {
        let result = unsafe {
            dmabuf_pool_blit(
                self.handle,
                std::os::fd::AsRawFd::as_raw_fd(&source.file_descriptor),
                source.stride,
                source.modifier,
                source.fourcc,
                source.width as std::ffi::c_int,
                source.height as std::ffi::c_int,
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

    /// テスト専用: ソースバッファを単色で塗る。
    #[cfg(test)]
    fn fill_test_source(&self, source: &DmabufSource, red: u8, green: u8, blue: u8, alpha: u8) {
        let result = unsafe {
            dmabuf_pool_fill_test_source(
                self.handle,
                std::os::fd::AsRawFd::as_raw_fd(&source.file_descriptor),
                source.stride,
                source.modifier,
                source.fourcc,
                source.width as std::ffi::c_int,
                source.height as std::ffi::c_int,
                red,
                green,
                blue,
                alpha,
            )
        };
        assert_ne!(result, 0, "テスト用ソースを塗れない");
    }

    /// テスト専用: 出力バッファの 1 ピクセルを読み戻す。
    #[cfg(test)]
    fn read_output_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
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

    /// テスト専用: CEF から渡されたものに見立てたソースバッファを確保する。
    #[cfg(test)]
    fn create_test_source(&self, width: u32, height: u32) -> Option<DmabufSource> {
        let mut file_descriptor: std::ffi::c_int = -1;
        let mut stride: u32 = 0;
        let mut modifier: u64 = 0;
        let mut fourcc: u32 = 0;
        let result = unsafe {
            dmabuf_pool_create_test_source(
                self.handle,
                width as std::ffi::c_int,
                height as std::ffi::c_int,
                &mut file_descriptor,
                &mut stride,
                &mut modifier,
                &mut fourcc,
            )
        };
        if result == 0 {
            return None;
        }
        Some(DmabufSource {
            file_descriptor: unsafe { std::os::fd::FromRawFd::from_raw_fd(file_descriptor) },
            stride,
            modifier,
            fourcc,
            width,
            height,
        })
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
    fn blit_すると出力記述子が取れる() {
        let _guard = lock_gpu_tests();
        let Some(pool) = pool_or_skip() else { return };
        let source = pool.create_test_source(256, 256).expect("ソースを確保できない");
        assert!(pool.blit(&source));

        let (_file_descriptor, descriptor) = pool.output().expect("出力記述子が取れない");
        assert_eq!(descriptor.width, 256);
        assert_eq!(descriptor.height, 256);
        assert!(descriptor.stride >= 256 * 4);
        assert_eq!(descriptor.generation, 1);
    }

    #[test]
    fn blit_がソースの色を出力バッファへ運ぶ() {
        let _guard = lock_gpu_tests();
        // 記述子が取れるだけでは経路の検証にならない。既知の色を入れたソースを
        // blit して、出力バッファから同じ色が読めることを確かめる。
        let Some(pool) = pool_or_skip() else { return };
        let source = pool.create_test_source(64, 64).unwrap();
        pool.fill_test_source(&source, 0, 128, 255, 255);

        assert!(pool.blit(&source));

        let pixel = pool.read_output_pixel(32, 32).expect("出力を読み戻せない");
        // 8bit の丸めで 1 ずれることがあるため許容幅を持たせる。
        assert!(pixel[0] <= 2, "赤が乗っている: {:?}", pixel);
        assert!((126..=130).contains(&pixel[1]), "緑が合わない: {:?}", pixel);
        assert!(pixel[2] >= 253, "青が合わない: {:?}", pixel);
        assert!(pixel[3] >= 253, "アルファが合わない: {:?}", pixel);
    }

    #[test]
    fn サイズが変わると_generation_が進む() {
        let _guard = lock_gpu_tests();
        let Some(pool) = pool_or_skip() else { return };
        let first = pool.create_test_source(256, 256).unwrap();
        assert!(pool.blit(&first));
        let (_first_fd, first_descriptor) = pool.output().unwrap();

        let second = pool.create_test_source(512, 512).unwrap();
        assert!(pool.blit(&second));
        let (_second_fd, second_descriptor) = pool.output().unwrap();

        assert_eq!(second_descriptor.generation, first_descriptor.generation + 1);
        assert_eq!(second_descriptor.width, 512);
    }

    #[test]
    fn 同じサイズなら_generation_は進まない() {
        let _guard = lock_gpu_tests();
        let Some(pool) = pool_or_skip() else { return };
        let first = pool.create_test_source(256, 256).unwrap();
        assert!(pool.blit(&first));
        let (_first_fd, first_descriptor) = pool.output().unwrap();

        let second = pool.create_test_source(256, 256).unwrap();
        assert!(pool.blit(&second));
        let (_second_fd, second_descriptor) = pool.output().unwrap();

        assert_eq!(second_descriptor.generation, first_descriptor.generation);
    }

    #[test]
    fn blit_前は出力記述子が取れない() {
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
