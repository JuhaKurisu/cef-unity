//! クライアント側ログの単一窓口。
//!
//! 以前は `lib.rs` の `log_to_file` と `d3d11.rs` / `d3d12.rs` の `log_debug` が
//! 別々に実装されており、後者はマスターフラグ (`cef_unity_initialize` の
//! `enable_log`) を無視して呼ばれるたびにファイルを open していた。fence 失敗が
//! 継続する異常系では毎フレーム file I/O が走り、フレームスパイクの原因になる
//! (macOS 側で実測して排除したのと同型の問題)。
//!
//! ここに一本化し、無効時は即 return、有効時はファイルハンドルを保持する。

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

static LOG_ENABLED: AtomicBool = AtomicBool::new(false);
static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);

/// ログの有効/無効を設定する。無効化時は保持しているハンドルを閉じる。
pub fn set_enabled(enabled: bool) {
    LOG_ENABLED.store(enabled, Ordering::SeqCst);
    if !enabled {
        *LOG_FILE.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

pub fn is_enabled() -> bool {
    LOG_ENABLED.load(Ordering::Relaxed)
}

/// ログを 1 行書く。`prefix` は経路の識別子 ("d3d11" / "d3d12")。
/// 空文字なら従来どおりタイムスタンプを前置する。無効時は即 return。
pub fn write(prefix: &str, message: &str) {
    if !is_enabled() {
        return;
    }
    let mut guard = LOG_FILE.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_none() {
        let path = std::env::temp_dir().join("cef_unity_debug.log");
        *guard = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
    }
    let Some(file) = guard.as_mut() else {
        return;
    };
    if prefix.is_empty() {
        let _ = writeln!(file, "[{:?}] {}", std::time::SystemTime::now(), message);
    } else {
        let _ = writeln!(file, "[{}] {}", prefix, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_flag_round_trips() {
        set_enabled(false);
        assert!(!is_enabled(), "既定では無効であること");
        set_enabled(true);
        assert!(is_enabled(), "有効化が反映されること");
        set_enabled(false);
        assert!(!is_enabled(), "無効化が反映されること");
    }

    #[test]
    fn write_while_disabled_does_not_open_the_file() {
        set_enabled(false);
        write("d3d11", "この行は書かれないこと");
        let guard = LOG_FILE.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            guard.is_none(),
            "無効時はファイルハンドルを開かないこと (異常系の毎フレーム open を防ぐ)"
        );
    }
}
