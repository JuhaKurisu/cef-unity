//! Linux: dmabuf プールの安全なラッパー。C 実装は `dmabuf_pool.c`。
//!
//! CEF の `on_accelerated_paint` が渡す dmabuf はプールの借用で、コールバックを
//! 抜けると再利用される。そのため自前の出力バッファへ blit し、その dmabuf を
//! クライアントへ渡す。macOS の `iosurface_pool.m`、Windows の `d3d11_pool.rs` に相当する。

#![cfg(target_os = "linux")]

unsafe extern "C" {
    fn dmabuf_pool_create(failure_stage: *mut std::ffi::c_int) -> *mut std::ffi::c_void;
    fn dmabuf_pool_destroy(pool: *mut std::ffi::c_void);
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
        // GPU の無い環境 (CI ランナー、コンテナ) では構築できないのが正しい挙動なので、
        // レンダーノードの有無で期待値を切り替える。
        let render_node_exists = std::path::Path::new("/dev/dri/renderD128").exists();
        let pool = DmabufPool::create();
        assert_eq!(pool.is_some(), render_node_exists);
    }

    #[test]
    fn 構築と破棄を繰り返してもリークしない() {
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
