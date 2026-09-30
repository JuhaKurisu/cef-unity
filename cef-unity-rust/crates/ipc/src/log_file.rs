// client と server が共用するログファイル。
//
// 保存先ディレクトリは利用側 (Unity なら persistentDataPath 配下) が決めて渡す。
// 各プロセスは起動ごとに `<prefix>-<UTC 時刻>-<pid>.log` を新しく作り、1 行ずつ直接
// 書き込む (バッファしない)。プロセスが落ちても直前の行まで残すためで、落ちた server の
// 記録が次の server の起動で消えないよう、ファイルは起動ごとに分けて古いものから消す。
//
// レベルは 2 段階:
// - Essential: 起動・終了・障害・復旧・エラー。常に書く。量が少ないのでリリースでも常時 ON
// - Verbose: 毎フレーム級の診断。`verbose` を立てたときだけ書く

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

/// 1 つの prefix について残すファイルの数 (今回の分を含む)。
pub const KEPT_FILES_PER_PREFIX: usize = 10;

/// 1 ファイルの上限。異常系でエラーが毎フレーム出続けてもディスクを食い潰さないよう、
/// 超えたら以降の行を捨てる。
pub const MAXIMUM_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// ファイルを開く前に書かれた Essential 行を溜めておく数 (UnityPluginLoad など)。
const MAXIMUM_PENDING_LINES: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Essential,
    Verbose,
}

struct Sink {
    file: File,
    written_bytes: u64,
    limit_reached: bool,
}

struct State {
    sink: Option<Sink>,
    /// ファイルを開く前の Essential 行。開いた時点で先頭に書き出す。
    pending_lines: Vec<String>,
}

static VERBOSE: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<State> = Mutex::new(State {
    sink: None,
    pending_lines: Vec::new(),
});

/// Verbose 行を書くか。毎フレームの計測を行うかどうかの判定にも使う。
pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

/// `directory` に今回のログファイルを作って書き込み先にする。古いファイルは
/// `KEPT_FILES_PER_PREFIX` 個を残して消す。作ったファイルのパスを返す。
pub fn open(directory: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    let path = session_path(directory, prefix, std::process::id(), SystemTime::now());
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut sink = Sink {
        file,
        written_bytes: 0,
        limit_reached: false,
    };
    for line in std::mem::take(&mut state.pending_lines) {
        sink.write_line(&line);
    }
    state.sink = Some(sink);
    drop(state);
    prune(directory, prefix, KEPT_FILES_PER_PREFIX);
    Ok(path)
}

/// 書き込み先を閉じる。以降の行は次に `open` するまで捨てる (Essential は溜める)。
pub fn close() {
    STATE.lock().unwrap_or_else(PoisonError::into_inner).sink = None;
}

/// 1 行書く。Verbose は `is_verbose()` のときだけ書く。
pub fn write(level: LogLevel, message: &str) {
    if level == LogLevel::Verbose && !is_verbose() {
        return;
    }
    let line = format!("{} {}\n", format_timestamp(SystemTime::now()), message);
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    match state.sink.as_mut() {
        Some(sink) => sink.write_line(&line),
        None => {
            if level == LogLevel::Essential && state.pending_lines.len() < MAXIMUM_PENDING_LINES {
                state.pending_lines.push(line);
            }
        }
    }
}

impl Sink {
    fn write_line(&mut self, line: &str) {
        if self.limit_reached {
            return;
        }
        if self.written_bytes + line.len() as u64 > MAXIMUM_FILE_BYTES {
            self.limit_reached = true;
            let _ = self
                .file
                .write_all(b"log size limit reached; further lines are dropped\n");
            return;
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.written_bytes += line.len() as u64;
        }
    }
}

/// panic の内容と発生箇所、バックトレースを Essential で書く hook を入れる
/// (既存の hook はその後に呼ぶ)。何度呼んでも 1 回だけ入る。
pub fn install_panic_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |information| {
            let backtrace = std::backtrace::Backtrace::force_capture();
            let thread = std::thread::current();
            write(
                LogLevel::Essential,
                &format!(
                    "panic on thread '{}': {}\n{}",
                    thread.name().unwrap_or("<unnamed>"),
                    information,
                    backtrace
                ),
            );
            previous_hook(information);
        }));
    });
}

/// 今回のログファイルのパス。時刻を先頭に置くので名前順 = 古い順になる。
pub fn session_path(directory: &Path, prefix: &str, process_id: u32, now: SystemTime) -> PathBuf {
    let (date, time, _) = civil_time(now);
    directory.join(format!(
        "{}-{}T{}Z-{}.log",
        prefix,
        date.replace('-', ""),
        time.replace(':', ""),
        process_id
    ))
}

/// `directory` の `<prefix>-*.log` を新しい方から `keep` 個残して消す。
/// 別のプロセスが使用中で消せないもの (Windows) は残す。
pub fn prune(directory: &Path, prefix: &str, keep: usize) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let file_prefix = format!("{}-", prefix);
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(&file_prefix) && name.ends_with(".log"))
        .collect();
    names.sort();
    let excess = names.len().saturating_sub(keep);
    for name in &names[..excess] {
        let _ = std::fs::remove_file(directory.join(name));
    }
}

/// `2026-09-30T10:15:30.123Z` 形式 (UTC)。
pub fn format_timestamp(time: SystemTime) -> String {
    let (date, clock, milliseconds) = civil_time(time);
    format!("{}T{}.{:03}Z", date, clock, milliseconds)
}

/// UTC の ("YYYY-MM-DD", "HH:MM:SS", ミリ秒)。
fn civil_time(time: SystemTime) -> (String, String, u32) {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since_epoch.as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (
        format!("{:04}-{:02}-{:02}", year, month, day),
        format!(
            "{:02}:{:02}:{:02}",
            second_of_day / 3600,
            second_of_day % 3600 / 60,
            second_of_day % 60
        ),
        since_epoch.subsec_millis(),
    )
}

/// 1970-01-01 からの日数を (年, 月, 日) にする (Howard Hinnant の civil_from_days)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 } as u32;
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamp_is_utc_iso_8601() {
        let time = UNIX_EPOCH + Duration::from_millis(1_790_000_000_123);
        assert_eq!(format_timestamp(time), "2026-09-21T14:13:20.123Z");
        assert_eq!(format_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        // うるう日
        let leap_day = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(format_timestamp(leap_day), "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn session_path_sorts_by_time() {
        let directory = Path::new("logs");
        let earlier = session_path(directory, "server", 999, UNIX_EPOCH + Duration::from_secs(1_790_000_000));
        let later = session_path(directory, "server", 1, UNIX_EPOCH + Duration::from_secs(1_790_000_001));
        assert_eq!(earlier, directory.join("server-20260921T141320Z-999.log"));
        assert!(earlier.file_name() < later.file_name(), "名前順が古い順になること");
    }

    #[test]
    fn prune_keeps_the_newest_files_of_the_prefix_only() {
        let directory = std::env::temp_dir().join(format!("cef_unity_log_prune_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        for second in 0..5 {
            let path = session_path(&directory, "server", 1, UNIX_EPOCH + Duration::from_secs(second));
            std::fs::write(path, "").unwrap();
        }
        std::fs::write(directory.join("cef-19700101T000000Z-1.log"), "").unwrap();
        std::fs::write(directory.join("server-notes.txt"), "").unwrap();

        prune(&directory, "server", 2);

        let mut remaining: Vec<String> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            vec![
                "cef-19700101T000000Z-1.log",
                "server-19700101T000003Z-1.log",
                "server-19700101T000004Z-1.log",
                "server-notes.txt",
            ],
            "同じ prefix の .log だけを新しい順に 2 個残すこと"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
