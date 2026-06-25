//! Dedicated worker thread that owns the CoreWLAN handles.
//!
//! `Retained<CWInterface>` is not `Send`, so we pin it to one OS thread and
//! drive it via a `std::sync::mpsc` request channel. Responses flow back as
//! `Event` values on the shared tokio channel the UI reads from.

use std::sync::mpsc::{self as std_mpsc, Sender};
use std::thread;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::corewlan::{WifiClient, WifiInterface};
use crate::event::{Event, SharePayload};
use crate::{keychain, networksetup};

/// Operations the worker can be asked to perform. Serializable so the same
/// type travels over the daemon's unix socket and through the in-process
/// channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Request {
    RefreshState,
    Scan,
    RefreshPreferred,
    SetPower(bool),
    Associate(Associate),
    Disconnect,
    Forget(String),
    JoinSaved(String),
    JoinWithPassword { ssid: String, password: String },
    Share { ssid: String, security: ShareSecurity },
    Diagnose,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareSecurity {
    Wpa,
    Wep,
    Nopass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Associate {
    pub ssid: String,
    pub kind: AssociateKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssociateKind {
    Open,
    Psk(String),
    Peap { username: String, password: String },
    Hidden(Option<String>),
}

/// In-process worker handle. The daemon uses this directly; the client never
/// constructs one. The `Local`/`Remote` enum that the TUI sees lives in
/// `app::WifiHandle`.
#[derive(Clone)]
pub struct LocalWifiHandle {
    tx: Sender<Request>,
}

impl LocalWifiHandle {
    pub fn spawn(events: UnboundedSender<Event>) -> Self {
        let (tx, rx) = std_mpsc::channel::<Request>();
        thread::Builder::new()
            .name("wifi-worker".into())
            .spawn(move || worker_loop(rx, events))
            .expect("spawn wifi-worker thread");
        Self { tx }
    }

    pub fn send(&self, req: Request) {
        let _ = self.tx.send(req);
    }
}

/// Front-door handle the TUI and CLI both use. Dispatches to either the
/// in-process worker (inside the daemon) or the socket-backed remote handle
/// (in the client). Same `send` API as the old `WifiHandle`, so call sites
/// in `app.rs` and `handler.rs` don't change.
#[derive(Clone)]
pub enum WifiHandle {
    Local(LocalWifiHandle),
    Remote(crate::client::RemoteWifiHandle),
}

impl WifiHandle {
    pub fn send(&self, req: Request) {
        match self {
            WifiHandle::Local(h) => h.send(req),
            WifiHandle::Remote(h) => h.send(req),
        }
    }
}

fn worker_loop(rx: std_mpsc::Receiver<Request>, events: UnboundedSender<Event>) {
    let client = match WifiClient::shared() {
        Ok(c) => c,
        Err(e) => {
            let _ = events.send(Event::Error(format!("CoreWLAN init failed: {e}")));
            return;
        }
    };
    let iface = match client.default_interface() {
        Ok(i) => i,
        Err(e) => {
            let _ = events.send(Event::Error(format!("no Wi-Fi interface: {e}")));
            return;
        }
    };

    emit_state(&iface, &events);
    emit_preferred(&iface, &events);
    emit_scan(&iface, &events);

    while let Ok(req) = rx.recv() {
        match req {
            Request::RefreshState => emit_state(&iface, &events),
            Request::RefreshPreferred => emit_preferred(&iface, &events),
            Request::Scan => emit_scan(&iface, &events),
            Request::SetPower(on) => {
                if let Err(e) = iface.set_power(on) {
                    let name = iface.name();
                    if let Err(e2) = networksetup::set_power(&name, on) {
                        let _ = events.send(Event::Error(format!(
                            "power toggle failed: CoreWLAN={e}; networksetup={e2}"
                        )));
                    } else {
                        let _ =
                            events.send(Event::Notice(format!("Wi-Fi {}", if on { "on" } else { "off" })));
                    }
                } else {
                    let _ = events.send(Event::Notice(format!(
                        "Wi-Fi {}",
                        if on { "on" } else { "off" }
                    )));
                }
                emit_state(&iface, &events);
            }
            Request::Associate(req) => {
                let ssid = req.ssid.clone();
                let result = match req.kind {
                    AssociateKind::Open => iface.associate_open(&ssid),
                    AssociateKind::Psk(p) => iface.associate_psk(&ssid, &p),
                    AssociateKind::Peap { username, password } => {
                        iface.associate_peap(&ssid, &username, &password)
                    }
                    AssociateKind::Hidden(pw) => match iface.scan_for_ssid(&ssid) {
                        Ok(_) => match pw {
                            Some(p) => iface.associate_psk(&ssid, &p),
                            None => iface.associate_open(&ssid),
                        },
                        Err(e) => Err(e),
                    },
                };
                match result {
                    Ok(()) => {
                        let _ = events.send(Event::Notice(format!("connected to {ssid}")));
                    }
                    Err(e) => {
                        let _ = events.send(Event::Error(format!("connect failed: {e}")));
                    }
                }
                emit_state(&iface, &events);
                emit_preferred(&iface, &events);
            }
            Request::JoinWithPassword { ssid, password } => {
                let name = iface.name();
                match networksetup::set_airport_network(&name, &ssid, Some(&password)) {
                    Ok(()) if verify_join(&iface, &ssid) => {
                        // Cache our own copy so future reconnects are silent.
                        // Best-effort: a cache miss just means we prompt again.
                        if let Err(e) = keychain::cache_password(&ssid, &password) {
                            eprintln!("warning: could not cache password for {ssid}: {e}");
                        }
                        let _ = events.send(Event::Notice(format!("connected to {ssid}")));
                    }
                    Ok(()) => {
                        let _ = events.send(Event::Error(format!(
                            "join to {ssid} did not take effect — check the password"
                        )));
                    }
                    Err(e) => {
                        let _ = events.send(Event::Error(format!("join failed: {e}")));
                    }
                }
                emit_state(&iface, &events);
            }
            Request::JoinSaved(ssid) => {
                // Silent-reconnect strategy, cheapest first:
                //   1. If macwifi has cached this network's password in the login
                //      keychain (from a prior connect), read it back silently and
                //      associate with it. This is the only reliable silent path
                //      for secured networks — the System keychain's copy is walled
                //      off by a partition-list ACL even from root (the -25308/
                //      -25293 finding; see keychain.rs).
                //   2. Otherwise try CoreWLAN's `associateToNetwork:password:nil`,
                //      which works for open networks and any secured network whose
                //      credential wifid can supply internally.
                //   3. If neither connects, ask the user for the password. We do
                //      NOT read the System keychain here — it fires a useless admin
                //      dialog and fails anyway.
                let joined = match keychain::cached_password(&ssid) {
                    Ok(Some(pw)) => {
                        iface.associate_psk(&ssid, &pw).is_ok() && verify_join(&iface, &ssid)
                    }
                    _ => false,
                } || (iface.associate_open(&ssid).is_ok() && verify_join(&iface, &ssid));

                if joined {
                    let _ = events.send(Event::Notice(format!("connected to {ssid}")));
                } else {
                    let _ = events.send(Event::JoinSavedFailed {
                        ssid,
                        reason: "saved credential unavailable".into(),
                    });
                }
                emit_state(&iface, &events);
            }
            Request::Disconnect => {
                iface.disassociate();
                let _ = events.send(Event::Notice("disconnected".into()));
                emit_state(&iface, &events);
            }
            Request::Share { ssid, security } => {
                let (uri, has_pw) = match security {
                    ShareSecurity::Nopass => (
                        format!("WIFI:T:nopass;S:{};;", escape_wifi(&ssid)),
                        false,
                    ),
                    ShareSecurity::Wpa | ShareSecurity::Wep => {
                        let t = match security {
                            ShareSecurity::Wep => "WEP",
                            _ => "WPA",
                        };
                        match keychain::wifi_password(&ssid) {
                            Ok(pw) => (
                                format!(
                                    "WIFI:T:{};S:{};P:{};;",
                                    t,
                                    escape_wifi(&ssid),
                                    escape_wifi(&pw)
                                ),
                                true,
                            ),
                            Err(e) => {
                                let _ = events.send(Event::Error(format!(
                                    "keychain: {e} — sharing SSID only"
                                )));
                                (
                                    format!("WIFI:T:nopass;S:{};;", escape_wifi(&ssid)),
                                    false,
                                )
                            }
                        }
                    }
                };
                let _ = events.send(Event::ShareReady(SharePayload {
                    ssid,
                    uri,
                    has_password: has_pw,
                }));
            }
            Request::Forget(ssid) => {
                let name = iface.name();
                // Drop our cached login-keychain copy too, so a forgotten
                // network doesn't silently reconnect from our cache later.
                let _ = keychain::forget_cached(&ssid);
                match networksetup::remove_preferred(&name, &ssid) {
                    Ok(()) => {
                        let _ = events.send(Event::Notice(format!("forgot {ssid}")));
                    }
                    Err(e) => {
                        let _ = events.send(Event::Error(format!("forget failed: {e}")));
                    }
                }
                emit_preferred(&iface, &events);
            }
            Request::Diagnose => {
                emit_diagnose(&iface, &events);
            }
        }
    }
}

fn emit_diagnose(iface: &WifiInterface, events: &UnboundedSender<Event>) {
    use crate::event::DaemonDiagnose;
    let state = iface.state();
    let scan = iface.scan().unwrap_or_default();
    let blank = scan
        .iter()
        .filter(|n| n.ssid.as_deref().map_or(true, str::is_empty))
        .count();
    let location_auth_raw = unsafe {
        let mgr = objc2_core_location::CLLocationManager::new();
        mgr.authorizationStatus().0 as i32
    };
    let pid = unsafe { libc::getpid() };
    let parent_pid = unsafe { libc::getppid() };
    let (interface, current_ssid) = match &state {
        Ok(s) => (s.name.clone(), s.ssid.clone()),
        Err(_) => (iface.name(), None),
    };
    let _ = events.send(Event::DaemonDiagnose(DaemonDiagnose {
        pid,
        parent_pid,
        location_auth_raw,
        interface,
        current_ssid,
        scan_count: scan.len(),
        scan_blank: blank,
    }));
}

fn emit_state(iface: &WifiInterface, events: &UnboundedSender<Event>) {
    match iface.state() {
        Ok(s) => {
            let _ = events.send(Event::State(s));
        }
        Err(e) => {
            let _ = events.send(Event::Error(format!("state refresh failed: {e}")));
        }
    }
}

/// Confirm a `networksetup`-driven join actually took effect, independent of
/// locale. `networksetup -setairportnetwork` exits 0 and prints a *localized*
/// "Failed…" line on auth/password errors, so the only trustworthy signal is
/// reading back the interface's current SSID via CoreWLAN. Association can lag
/// the command's return by a moment, so poll briefly. (The daemon holds the
/// Location grant, so the SSID readback isn't redacted here.)
fn verify_join(iface: &WifiInterface, ssid: &str) -> bool {
    for _ in 0..6 {
        if let Ok(st) = iface.state() {
            if st.ssid.as_deref() == Some(ssid) {
                return true;
            }
        }
        thread::sleep(std::time::Duration::from_millis(500));
    }
    false
}

fn emit_preferred(iface: &WifiInterface, events: &UnboundedSender<Event>) {
    let name = iface.name();
    match networksetup::list_preferred(&name) {
        Ok(v) => {
            let _ = events.send(Event::PreferredResult(v));
        }
        Err(e) => {
            let _ = events.send(Event::Error(format!("preferred list failed: {e}")));
        }
    }
}

fn escape_wifi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | ';' | ',' | ':' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn emit_scan(iface: &WifiInterface, events: &UnboundedSender<Event>) {
    let _ = events.send(Event::ScanStarted);
    match iface.scan() {
        Ok(mut n) => {
            n.sort_by_key(|x| -x.rssi);
            let all_blank = !n.is_empty()
                && n.iter()
                    .all(|x| x.ssid.as_deref().map_or(true, str::is_empty));
            let _ = events.send(Event::ScanResult(n));
            if all_blank {
                if let Some(hint) = crate::location::redaction_hint() {
                    let _ = events.send(Event::Error(hint.to_string()));
                } else {
                    // Location says we're authorized but SSIDs are still
                    // redacted — almost always means the running executable
                    // isn't the bundled one TCC granted.
                    let _ = events.send(Event::Error(
                        "SSIDs redacted despite Location auth — run via bundled .app (scripts/bundle.sh) so TCC matches this binary".into(),
                    ));
                }
            }
        }
        Err(e) => {
            let _ = events.send(Event::Error(format!("scan failed: {e}")));
        }
    }
}
