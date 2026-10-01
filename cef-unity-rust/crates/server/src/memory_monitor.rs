// プロセスごとのメモリの推移を一定間隔で essential のログへ書く。
//
// 対象は server 自身と、その子孫 (CEF のレンダラー・GPU プロセス・ユーティリティなど。
// Linux ではレンダラーが zygote の子になるので子孫まで辿る)。どのプロセスがいつから
// 増えたかを、報告者のログだけで追えるようにするためのもの。
//
// システム全体のプロセス一覧を走査するので、pump を止めないよう専用スレッドで行う。

use std::collections::HashMap;
use std::ffi::OsString;
use std::time::Duration;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// 記録の間隔。
const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

/// 1 プロセス分の記録。
#[derive(Debug, PartialEq)]
struct ProcessMemory {
    kind: String,
    process_id: u32,
    /// 物理メモリ上の使用量 (Windows: working set、macOS / Linux: RSS)。
    resident_bytes: u64,
    /// 仮想メモリ (Windows: private bytes、macOS / Linux: 仮想アドレス空間の大きさ)。
    virtual_bytes: u64,
}

/// 記録用のスレッドを起こす。最初の 1 行はすぐに書く。
pub fn start() {
    let spawned = std::thread::Builder::new()
        .name("memory-monitor".to_string())
        .spawn(|| {
            let mut system = System::new();
            loop {
                crate::server::log_essential(&format_line(&sample(&mut system)));
                std::thread::sleep(SAMPLE_INTERVAL);
            }
        });
    if let Err(error) = spawned {
        crate::server::log_essential(&format!("memory monitor thread spawn failed: {}", error));
    }
}

fn sample(system: &mut System) -> Vec<ProcessMemory> {
    // 親子関係はシステム全体から取り、メモリとコマンドラインは対象のプロセスだけ読む
    // (他のプロセスのコマンドラインを読むのは重いため)。
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let root = std::process::id();
    let parents: Vec<(u32, u32)> = system
        .processes()
        .iter()
        .filter_map(|(process_id, process)| {
            process
                .parent()
                .map(|parent| (process_id.as_u32(), parent.as_u32()))
        })
        .collect();
    let targets: Vec<Pid> = std::iter::once(root)
        .chain(descendants(&parents, root))
        .map(Pid::from_u32)
        .collect();
    // 起動途中のプロセスは --type が付く前のコマンドラインを持つことがあるので、
    // コマンドラインも毎回読み直す。
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&targets),
        false,
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_cmd(UpdateKind::Always),
    );

    let mut records: Vec<ProcessMemory> = targets
        .iter()
        .map(|process_id| process_id.as_u32())
        .filter_map(|process_id| {
            let process = system.process(Pid::from_u32(process_id))?;
            Some(ProcessMemory {
                kind: if process_id == root {
                    "server".to_string()
                } else {
                    process_kind(process.cmd())
                },
                process_id,
                resident_bytes: process.memory(),
                virtual_bytes: process.virtual_memory(),
            })
        })
        .collect();
    // server を先頭に、残りは種類・pid の順に並べて行どうしを比べやすくする。
    records.sort_by(|left, right| {
        (left.process_id != root, &left.kind, left.process_id)
            .cmp(&(right.process_id != root, &right.kind, right.process_id))
    });
    records
}

/// `root` の子孫の pid (親子関係 `(子, 親)` の一覧から辿る)。
fn descendants(parents: &[(u32, u32)], root: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(child, parent) in parents {
        if child != parent {
            children.entry(parent).or_default().push(child);
        }
    }
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(process_id) = pending.pop() {
        for &child in children.get(&process_id).into_iter().flatten() {
            if child != root && !found.contains(&child) {
                found.push(child);
                pending.push(child);
            }
        }
    }
    found
}

/// CEF の子プロセスの種類 (`--type=<種類>`)。無ければ "unknown"。
fn process_kind(command_line: &[OsString]) -> String {
    command_line
        .iter()
        .filter_map(|argument| argument.to_str())
        .find_map(|argument| argument.strip_prefix("--type="))
        .unwrap_or("unknown")
        .to_string()
}

fn format_line(records: &[ProcessMemory]) -> String {
    let processes: Vec<String> = records
        .iter()
        .map(|record| {
            format!(
                "{} pid={} resident={} virtual={}",
                record.kind,
                record.process_id,
                format_mebibytes(record.resident_bytes),
                format_mebibytes(record.virtual_bytes)
            )
        })
        .collect();
    format!("memory: {}", processes.join(" | "))
}

fn format_mebibytes(bytes: u64) -> String {
    format!("{:.1}MiB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_follow_grandchildren_and_skip_others() {
        // 10 = server、11/12 = 子、13 = 孫 (zygote 配下のレンダラー)、20 = 無関係
        let parents = [(11, 10), (12, 10), (13, 12), (20, 1), (10, 1)];
        let mut found = descendants(&parents, 10);
        found.sort();
        assert_eq!(found, vec![11, 12, 13]);
    }

    #[test]
    fn descendants_survive_cycles() {
        let parents = [(11, 10), (10, 11), (12, 12)];
        assert_eq!(descendants(&parents, 10), vec![11]);
    }

    #[test]
    fn process_kind_reads_type_switch() {
        let command_line: Vec<OsString> = ["helper.exe", "--type=gpu-process", "--no-sandbox"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(process_kind(&command_line), "gpu-process");
        assert_eq!(process_kind(&[OsString::from("helper.exe")]), "unknown");
    }

    #[test]
    fn line_lists_every_process() {
        let records = [
            ProcessMemory {
                kind: "server".to_string(),
                process_id: 10,
                resident_bytes: 96 * 1024 * 1024,
                virtual_bytes: 1536 * 1024 * 1024,
            },
            ProcessMemory {
                kind: "gpu-process".to_string(),
                process_id: 11,
                resident_bytes: 160 * 1024 * 1024 + 512 * 1024,
                virtual_bytes: 200 * 1024 * 1024,
            },
        ];
        assert_eq!(
            format_line(&records),
            "memory: server pid=10 resident=96.0MiB virtual=1536.0MiB | \
             gpu-process pid=11 resident=160.5MiB virtual=200.0MiB"
        );
    }
}
