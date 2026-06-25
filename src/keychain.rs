//! Read Wi-Fi passwords from the System keychain.
//!
//! macOS stores Wi-Fi passwords in `/Library/Keychains/System.keychain` as
//! generic-password items with `service="AirPort"`, `account=<SSID>`. The
//! authoritative copy lives there — we deliberately do NOT duplicate it
//! into the login keychain.
//!
//! Each read of a saved network's password triggers a macOS admin-auth prompt.
//! There is no persistent silent path for a third-party process: macOS enforces
//! these items' partition-list ACLs by the caller's code identity (not uid), so
//! the grant can't be made to stick — a spike confirmed even a root process gets
//! `errSecAuthFailed` (-25293). See `ARCHITECTURE_PASSWORDS.md` for the full
//! finding and why the earlier ACL-grant approach was removed.
//!
//! This path is only used for QR sharing and the rare fallback join; ordinary
//! connect/reconnect goes through CoreWLAN, which is silent. So the prompt is
//! a once-in-a-while cost on Share, not a per-connect annoyance.
//!
//! We use `security_framework::passwords::get_generic_password`, which wraps
//! `SecItemCopyMatching`. Shelling out to `/usr/bin/security` would behave the
//! same but attach any future grant to that binary instead of our app.

use anyhow::{Context, Result};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

const WIFI_SERVICE: &str = "AirPort";

/// Service name for macwifi's *own* cached copies of Wi-Fi passwords. These live
/// in the **login** keychain (not the System keychain) under macwifi's stable
/// code-signing identity, so macwifi can read them back with no prompt — the
/// thing the System keychain's partition-list ACL forbids for a third-party app.
/// A distinct service keeps these separate from the OS's `AirPort` items.
const CACHE_SERVICE: &str = "macwifi-wifi";

const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

pub fn wifi_password(ssid: &str) -> Result<String> {
    let bytes = get_generic_password(WIFI_SERVICE, ssid)
        .with_context(|| format!("system keychain lookup for {ssid}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Store macwifi's own copy of `ssid`'s password in the login keychain so future
/// reconnects can read it back silently. Add-or-update; safe to call repeatedly.
pub fn cache_password(ssid: &str, password: &str) -> Result<()> {
    set_generic_password(CACHE_SERVICE, ssid, password.as_bytes())
        .with_context(|| format!("cache password for {ssid}"))
}

/// Read macwifi's cached password for `ssid` from the login keychain. Returns
/// `Ok(None)` if we've never cached one (a network saved outside macwifi). This
/// read is silent — macwifi owns the item under its own code identity.
pub fn cached_password(ssid: &str) -> Result<Option<String>> {
    match get_generic_password(CACHE_SERVICE, ssid) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cached password lookup for {ssid}")),
    }
}

/// Remove macwifi's cached copy of `ssid`'s password. Idempotent — a missing
/// item is not an error (the network may never have been cached).
pub fn forget_cached(ssid: &str) -> Result<()> {
    match delete_generic_password(CACHE_SERVICE, ssid) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
        Err(e) => Err(e).with_context(|| format!("forget cached password for {ssid}")),
    }
}
