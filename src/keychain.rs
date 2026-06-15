//! Read Wi-Fi passwords from the System keychain.
//!
//! macOS stores Wi-Fi passwords in `/Library/Keychains/System.keychain` as
//! generic-password items with `service="AirPort"`, `account=<SSID>`. The
//! authoritative copy lives there — we deliberately do NOT duplicate it
//! into the login keychain.
//!
//! First read from this app for a given SSID triggers two macOS dialogs:
//!   1. Keychain access prompt: "macwifi wants to use … in your keychain".
//!      Click **Always Allow** (Allow is one-time only).
//!   2. Admin auth prompt: modifying the System keychain ACL is privileged,
//!      so macOS asks the user to enter their account password to confirm.
//! After both, our bundle's code-signing identity sits on the item's ACL.
//! Future reads from this app for that SSID are silent.
//!
//! Persistence across rebuilds requires a stable signing identity — see
//! `scripts/bundle.sh` and `CODESIGN_IDENTITY`. Ad-hoc rebuilds invalidate
//! the ACL match and re-prompt.
//!
//! We use `security_framework::passwords::get_generic_password`, which
//! wraps `SecItemCopyMatching`. Shelling out to `/usr/bin/security` would
//! attach the ACL grant to that binary instead of our app and re-prompt
//! every run.

use anyhow::{Context, Result};
use security_framework::passwords::get_generic_password;

const WIFI_SERVICE: &str = "AirPort";

pub fn wifi_password(ssid: &str) -> Result<String> {
    let bytes = get_generic_password(WIFI_SERVICE, ssid)
        .with_context(|| format!("system keychain lookup for {ssid}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
