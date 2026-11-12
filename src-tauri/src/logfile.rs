//! The helper's log: %LOCALAPPDATA%\SignFlow Helper Files\helper.log, one
//! line per event, kept under 5MB (the previous one is kept as
//! helper.old.log). It never holds a pass, a key, a token or a signed link:
//! callers write what happened, not what was sent.

use std::io::Write;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());
const MAX_BYTES: u64 = 5 * 1024 * 1024;

pub fn path() -> std::path::PathBuf {
    crate::config::home_dir().join("helper.log")
}

fn write(level: &str, msg: &str) {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(&p).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&p, p.with_file_name("helper.old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let line = format!("{} {level} {}\n", chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%z"), msg.replace('\n', " "));
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn info(msg: &str) {
    write("INFO", msg);
}

pub fn warn(msg: &str) {
    write("WARN", msg);
}
