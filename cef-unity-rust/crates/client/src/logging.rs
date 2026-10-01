//! クライアント側ログの単一窓口。
//!
//! 書き込み先とレベルは `cef_unity_ipc::log_file` が持つ (server と共用)。
//! 保存先は `cef_unity_initialize` で利用側から受け取り、起動ごとに
//! `client-<時刻>-<pid>.log` を作る。
//!
//! - `essential`: 起動・終了・障害・復旧・エラー。レベルが essential 以上のとき書く
//! - `verbose`: 毎フレーム級の診断。レベルが verbose のときだけ書く
//!   (無効時は即 return し、異常系で毎フレーム呼ばれても file I/O を起こさない)

use cef_unity_ipc::log_file::{self, LogLevel};

pub fn essential(prefix: &str, message: &str) {
    write(LogLevel::Essential, prefix, message);
}

pub fn verbose(prefix: &str, message: &str) {
    write(LogLevel::Verbose, prefix, message);
}

/// `prefix` は経路の識別子 ("d3d11" / "d3d12" など)。空文字なら付けない。
fn write(level: LogLevel, prefix: &str, message: &str) {
    if !log_file::is_enabled(level) {
        return;
    }
    if prefix.is_empty() {
        log_file::write(level, message);
    } else {
        log_file::write(level, &format!("[{}] {}", prefix, message));
    }
}
