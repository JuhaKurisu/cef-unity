//! Linux: サーバから受け取った dmabuf を GL テクスチャとして取り込む。
//!
//! `d3d11.rs` / `metal_texture.m` に相当する層。ホストの GL コンテキストが
//! current なスレッドからのみ呼ぶこと。

#![cfg(target_os = "linux")]

/// 受け取った dmabuf の GL テクスチャを世代付きで保持する。
///
/// サーバは出力バッファを作り直すたびに世代を進め、共有メモリヘッダにも同じ値を書く。
/// クライアントはヘッダの世代と一致するテクスチャだけを使う。一致しない間は
/// 前フレームの絵を維持する — 新しいサイズのヘッダを見たのに古いテクスチャを描く、
/// という不整合を構造的に避けるため。
pub struct DmabufTextureCache {
    generation: Option<u32>,
    texture: u32,
}

impl DmabufTextureCache {
    pub fn new() -> Self {
        Self {
            generation: None,
            texture: 0,
        }
    }

    /// 新しい世代のテクスチャを格納する。古い世代のものは使えなくなる。
    pub fn store(&mut self, generation: u32, texture: u32) {
        self.generation = Some(generation);
        self.texture = texture;
    }

    /// 指定した世代のテクスチャ。持っていなければ `None`。
    pub fn texture_for_generation(&self, generation: u32) -> Option<u32> {
        if self.generation == Some(generation) {
            Some(self.texture)
        } else {
            None
        }
    }

    /// 保持している世代。まだ何も受け取っていなければ `None`。
    pub fn current_generation(&self) -> Option<u32> {
        self.generation
    }
}

impl Default for DmabufTextureCache {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// GL 取り込み
// ---------------------------------------------------------------------------

unsafe extern "C" {
    fn dmabuf_import_texture(
        file_descriptor: std::ffi::c_int,
        width: std::ffi::c_int,
        height: std::ffi::c_int,
        stride: u32,
        modifier: u64,
        fourcc: u32,
        out_texture: *mut u32,
    ) -> std::ffi::c_int;
    fn dmabuf_release_texture(texture: u32);
}

/// サーバから届いた dmabuf をホストの GL コンテキストへ取り込む。
///
/// **ホストの GL コンテキストが current なスレッドからのみ呼ぶこと。**
/// EGL/GL はスレッド束縛で、違うスレッドから呼ぶと黙って失敗する。
/// `d3d11.rs` と同じく呼び出しスレッドを記録し、変化したら警告を出す。
pub fn import_texture(
    file_descriptor: &std::os::fd::OwnedFd,
    width: u32,
    height: u32,
    stride: u32,
    modifier: u64,
    fourcc: u32,
) -> Option<u32> {
    warn_if_calling_thread_changed();
    let mut texture: u32 = 0;
    let result = unsafe {
        dmabuf_import_texture(
            std::os::fd::AsRawFd::as_raw_fd(file_descriptor),
            width as std::ffi::c_int,
            height as std::ffi::c_int,
            stride,
            modifier,
            fourcc,
            &mut texture,
        )
    };
    if result == 0 {
        crate::logging::write("[dmabuf] ", "GL テクスチャに取り込めない");
        return None;
    }
    Some(texture)
}

/// 取り込んだテクスチャを解放する。世代が変わったときに古いものを捨てる。
pub fn release_texture(texture: u32) {
    if texture != 0 {
        unsafe { dmabuf_release_texture(texture) };
    }
}

/// GL は呼び出しスレッドに束縛される。違反を早く見つけるための記録。
fn warn_if_calling_thread_changed() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static FIRST_THREAD: AtomicU64 = AtomicU64::new(0);
    // ThreadId は数値化できないため、アドレスで代用する。
    thread_local! {
        static THREAD_MARKER: u8 = const { 0 };
    }
    let current = THREAD_MARKER.with(|marker| marker as *const u8 as u64);
    let first = FIRST_THREAD.compare_exchange(0, current, Ordering::Relaxed, Ordering::Relaxed);
    if let Err(previous) = first {
        if previous != current {
            crate::logging::write(
                "[dmabuf] ",
                "WARNING: 取り込みが別スレッドから呼ばれた。\
                 GL コンテキストはスレッド束縛なので取り込みは失敗する",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 世代が一致すればテクスチャを返す() {
        let mut cache = DmabufTextureCache::new();
        cache.store(3, 42);
        assert_eq!(cache.texture_for_generation(3), Some(42));
    }

    #[test]
    fn 世代が一致しなければ返さない() {
        let mut cache = DmabufTextureCache::new();
        cache.store(3, 42);
        assert_eq!(cache.texture_for_generation(4), None);
    }

    #[test]
    fn 新しい世代を格納すると古い世代は返らなくなる() {
        let mut cache = DmabufTextureCache::new();
        cache.store(3, 42);
        cache.store(4, 43);
        assert_eq!(cache.texture_for_generation(4), Some(43));
        assert_eq!(cache.texture_for_generation(3), None);
    }

    #[test]
    fn 何も受け取っていなければ何も返さない() {
        let cache = DmabufTextureCache::new();
        assert_eq!(cache.texture_for_generation(0), None);
        assert_eq!(cache.current_generation(), None);
    }
}
