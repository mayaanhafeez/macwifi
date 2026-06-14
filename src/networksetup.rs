//! Shell-out helpers around `networksetup(8)` for operations CoreWLAN
//! either doesn't expose cleanly or requires elevated authorization for.

use anyhow::{Context, Result, bail};
use std::process::Command;

pub fn set_power(iface: &str, on: bool) -> Result<()> {
    let arg = if on { "on" } else { "off" };
    let status = Command::new("networksetup")
        .args(["-setairportpower", iface, arg])
        .status()
        .context("running networksetup -setairportpower")?;
    if !status.success() {
        bail!("networksetup -setairportpower exited {status}");
    }
    Ok(())
}

pub fn list_preferred(iface: &str) -> Result<Vec<String>> {
    let out = Command::new("networksetup")
        .args(["-listpreferredwirelessnetworks", iface])
        .output()
        .context("running networksetup -listpreferredwirelessnetworks")?;
    if !out.status.success() {
        bail!(
            "networksetup -listpreferredwirelessnetworks exited {}",
            out.status
        );
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(s.lines()
        .skip(1)
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

pub fn set_airport_network(iface: &str, ssid: &str, password: Option<&str>) -> Result<()> {
    let mut args = vec!["-setairportnetwork", iface, ssid];
    if let Some(p) = password {
        args.push(p);
    }
    let out = Command::new("networksetup")
        .args(&args)
        .output()
        .context("running networksetup -setairportnetwork")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // networksetup -setairportnetwork exits 0 even on auth failure; check
    // the stdout text for the success/failure marker.
    if stdout.contains("Failed") || !stderr.trim().is_empty() {
        bail!(
            "{}",
            stderr
                .trim()
                .to_string()
                .or_default_if_empty(stdout.trim().to_string())
        );
    }
    Ok(())
}

trait OrDefaultIfEmpty {
    fn or_default_if_empty(self, fallback: String) -> String;
}
impl OrDefaultIfEmpty for String {
    fn or_default_if_empty(self, fallback: String) -> String {
        if self.is_empty() { fallback } else { self }
    }
}

pub fn remove_preferred(iface: &str, ssid: &str) -> Result<()> {
    let status = Command::new("networksetup")
        .args(["-removepreferredwirelessnetwork", iface, ssid])
        .status()
        .context("running networksetup -removepreferredwirelessnetwork")?;
    if !status.success() {
        bail!("networksetup -removepreferredwirelessnetwork exited {status}");
    }
    Ok(())
}
