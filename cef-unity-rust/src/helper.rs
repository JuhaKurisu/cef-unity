fn main() {
    // ヘルパーの起動をログファイルに記録
    let process_type = std::env::args()
        .skip_while(|a| !a.starts_with("--type="))
        .next()
        .unwrap_or_else(|| "unknown".to_string());

    let log_path = std::env::temp_dir().join("cef_unity_helper.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        use std::io::Write;
        let _ = writeln!(f, "[{:?}] helper started: pid={} {}", std::time::SystemTime::now(), std::process::id(), process_type);
    }

    let result = std::panic::catch_unwind(|| {
        cef_unity_rust::load_cef_auto();
        let args = cef::args::Args::new();
        cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut())
    });

    match result {
        Ok(code) => {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                use std::io::Write;
                let _ = writeln!(f, "[{:?}] helper exit: pid={} code={}", std::time::SystemTime::now(), std::process::id(), code);
            }
            std::process::exit(code);
        }
        Err(e) => {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                use std::io::Write;
                let _ = writeln!(f, "[{:?}] helper PANIC: pid={} {:?}", std::time::SystemTime::now(), std::process::id(), e);
            }
            std::process::exit(1);
        }
    }
}
