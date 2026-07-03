//! Emits a build identifier (short git hash + build timestamp) so the daemon
//! and client can detect when they're running mismatched builds across the
//! socket. Falls back to "unknown" outside a git checkout.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    println!("cargo:rustc-env=MACWIFI_BUILD_ID={hash}-{secs}");
    // Rebuild when HEAD moves so the hash stays fresh.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
}
