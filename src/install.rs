//! Install / uninstall the per-user LaunchAgent that runs the daemon.
//!
//! The plist uses `/usr/bin/open -W -a /Applications/macwifi.app --args
//! daemon` rather than execing the binary directly. That wrapper triggers
//! a real Aqua app session via Launch Services, which is required for
//! CoreWLAN to return un-redacted SSIDs (verified empirically — direct
//! launchd-exec returns blank SSIDs even with full Location grant).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

const LABEL: &str = "dev.macwifi.daemon";
const APP_PATH: &str = "/Applications/macwifi.app";
const STDOUT_LOG: &str = "/tmp/macwifi-daemon.out.log";
const STDERR_LOG: &str = "/tmp/macwifi-daemon.err.log";

pub fn install() -> Result<()> {
    if !Path::new(APP_PATH).exists() {
        bail!(
            "{} not found.\n\
             Build with `scripts/bundle.sh` and then copy:\n  \
             cp -R target/release/macwifi.app /Applications/",
            APP_PATH
        );
    }

    let plist_path = plist_path()?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let plist = build_plist()?;
    std::fs::write(&plist_path, plist)
        .with_context(|| format!("write {}", plist_path.display()))?;
    println!("wrote {}", plist_path.display());

    let uid = unsafe { libc::geteuid() };
    let domain = format!("gui/{uid}");
    let label_target = format!("{}/{}", domain, LABEL);

    // Boot out a stale instance, then bring the new one up. `bootout` for a
    // non-loaded target returns non-zero, which is fine.
    let _ = Command::new("launchctl")
        .args(["bootout", &label_target])
        .status();
    let st = Command::new("launchctl")
        .args(["bootstrap", &domain])
        .arg(&plist_path)
        .status()
        .context("run launchctl bootstrap")?;
    if !st.success() {
        bail!(
            "launchctl bootstrap failed (exit {}); check {STDERR_LOG} for details",
            st.code().unwrap_or(-1)
        );
    }
    println!("launchctl bootstrap {} {} → ok", domain, plist_path.display());

    // Wait briefly for the daemon to bind the socket.
    let sock = crate::ipc::socket_path();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !sock.exists() {
        if std::time::Instant::now() >= deadline {
            bail!(
                "daemon did not create {} within 15s — check {STDOUT_LOG} / {STDERR_LOG}",
                sock.display()
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    println!("daemon is up — socket at {}", sock.display());
    println!();
    println!("First scan from a TUI may trigger a one-time Location prompt and");
    println!("a per-SSID keychain prompt; click 'Always Allow' on the keychain");
    println!("dialog so it doesn't re-fire after every connect.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    let domain = format!("gui/{uid}");
    let label_target = format!("{}/{}", domain, LABEL);

    let _ = Command::new("launchctl")
        .args(["bootout", &label_target])
        .status();

    let plist_path = plist_path()?;
    if plist_path.exists() {
        std::fs::remove_file(&plist_path)
            .with_context(|| format!("remove {}", plist_path.display()))?;
        println!("removed {}", plist_path.display());
    }
    let sock = crate::ipc::socket_path();
    let _ = std::fs::remove_file(&sock);
    println!("done");
    Ok(())
}

fn plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("locate home directory")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn build_plist() -> Result<String> {
    // Bake the resolved socket path so launchd creates the parent dir if it
    // doesn't yet exist (Sockets dict in plist also accepts paths).
    let sock = crate::ipc::socket_path();
    let sock_str = sock.to_string_lossy().to_string();
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/open</string>
        <string>-W</string>
        <string>-a</string>
        <string>{APP_PATH}</string>
        <string>--args</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{STDOUT_LOG}</string>
    <key>StandardErrorPath</key>
    <string>{STDERR_LOG}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>MACWIFI_SOCKET_PATH</key>
        <string>{sock_str}</string>
    </dict>
</dict>
</plist>
"#
    ))
}
