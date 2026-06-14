//! Read a Wi-Fi password from the system Keychain. macOS will display its
//! own auth prompt the first time `security(1)` is asked for the password;
//! the user must approve.

use anyhow::{Result, bail};
use std::process::Command;

pub fn wifi_password(ssid: &str) -> Result<String> {
    let out = Command::new("security")
        .args([
            "find-generic-password",
            "-D",
            "AirPort network password",
            "-a",
            ssid,
            "-w",
        ])
        .output()?;
    if !out.status.success() {
        bail!(
            "keychain lookup failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
