//! Minimal file logger for the daemon.
//!
//! The daemon is launched via `/usr/bin/open -W -a macwifi.app --args daemon`,
//! so the LaunchAgent's `StandardErrorPath` captures `open`'s stderr, not the
//! daemon's — every `eprintln!` from the daemon process itself is silently
//! dropped. This writes daemon-side diagnostics to a real file next to the
//! socket instead.
//!
//! Never log password values here. SSIDs are fine: the file is per-user and
//! mode 0600.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

/// Path of the daemon log: the same directory as the socket.
pub fn log_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    base.join("macwifi").join("daemon.log")
}

/// Open (creating/truncating as needed) the daemon log. Call once at daemon
/// startup. Best-effort: if the file can't be opened, `log()` falls back to
/// stderr. Truncates if the previous log grew past 1 MiB so it can't grow
/// without bound across daemon restarts.
pub fn init() {
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let truncate = std::fs::metadata(&path)
        .map(|m| m.len() > 1024 * 1024)
        .unwrap_or(false);
    let file = OpenOptions::new()
        .create(true)
        .append(!truncate)
        .write(true)
        .truncate(truncate)
        .mode(0o600)
        .open(&path);
    if let Ok(f) = file {
        let _ = LOG.set(Mutex::new(f));
    }
}

/// Write one timestamped line. Falls back to stderr if `init()` hasn't run or
/// the file handle is unavailable. Use via the `dlog!` macro.
pub fn line(msg: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some(lock) = LOG.get() {
        if let Ok(mut f) = lock.lock() {
            let _ = writeln!(f, "[{secs}] {msg}");
            let _ = f.flush();
            return;
        }
    }
    eprintln!("[{secs}] {msg}");
}

/// `dlog!("join failed for {ssid}")` — formats and writes a daemon log line.
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        $crate::logging::line(&format!($($arg)*))
    };
}
