//! Dedicated worker thread that owns the CoreWLAN handles.
//!
//! `Retained<CWInterface>` is not `Send`, so we pin it to one OS thread and
//! drive it via a `std::sync::mpsc` command channel. Responses flow back as
//! `WorkerEvent` values on a tokio channel the daemon reads from.
//!
//! Worker *commands* are deliberately a different type from socket `Request`s:
//! commands carry the requesting client's `Origin` and other daemon-internal
//! bookkeeping that must never reach the wire.

use serde::{Deserialize, Serialize};
use std::sync::mpsc::{self as std_mpsc, Sender};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

use crate::corewlan::{ScannedNetwork, Security, WifiClient, WifiInterface};
use crate::event::{Event, JoinFailReason, SharePayload};
use crate::{dlog, keychain, networksetup};

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
    JoinWithPassword {
        ssid: String,
        password: String,
    },
    Share {
        ssid: String,
        security: ShareSecurity,
    },
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

pub fn join_request(ssid: String, password: String) -> Request {
    if password.is_empty() {
        Request::Associate(Associate {
            ssid,
            kind: AssociateKind::Open,
        })
    } else {
        Request::JoinWithPassword { ssid, password }
    }
}

/// Identifies the client request an emitted event answers, so the daemon can
/// address the reply instead of broadcasting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub client_id: u64,
    pub request_id: u64,
}

/// What the daemon asks the CoreWLAN thread to do. Not serializable: unlike
/// `Request` this never crosses the socket.
#[derive(Debug, Clone)]
pub enum WorkerCommand {
    Request {
        origin: Option<Origin>,
        request: Request,
    },
    /// A physical scan the daemon's coordinator started. It carries an
    /// operation id rather than an origin because one sweep may be answering
    /// any number of clients — the coordinator, not the worker, knows who.
    Scan { operation_id: u64 },
    /// One more check that a join took effect. Each is a single cheap state
    /// read; the waiting between them happens on the daemon's runtime.
    VerifyJoin {
        origin: Option<Origin>,
        ssid: String,
        kind: JoinKind,
        attempts_remaining: u8,
    },
}

/// Which join produced the association being confirmed. The two report an
/// unconfirmed association differently — and the client reacts differently, one
/// re-prompting for a password and the other not — so the distinction has to
/// survive the hop out to the timer and back.
#[derive(Debug, Clone)]
pub enum JoinKind {
    /// A `networksetup` join with a password the user just entered.
    Password,
    /// A silent reconnect to a saved network, carrying the detail its
    /// `JoinSavedFailed` will report if the association never confirms.
    Saved { fail_detail: String },
}

/// How many times a join is checked before it is called unconfirmed.
/// Association and DHCP can lag `networksetup`'s return by several seconds.
const VERIFY_ATTEMPTS: u8 = 20;

/// How long the daemon waits between verification checks.
pub const VERIFY_INTERVAL: Duration = Duration::from_millis(500);

/// What the CoreWLAN thread reports back.
#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Emit {
        origin: Option<Origin>,
        event: Event,
    },
    ScanFinished {
        operation_id: u64,
        result: Result<Vec<ScannedNetwork>, String>,
        timings: ScanTimings,
    },
    /// The worker wants another verification check after `VERIFY_INTERVAL`.
    /// The worker cannot sleep on it — that is the whole point — so it asks
    /// the daemon to sleep and re-enqueue.
    ScheduleVerify {
        origin: Option<Origin>,
        ssid: String,
        kind: JoinKind,
        attempts_remaining: u8,
    },
}

/// Where one scan's time went, split at the boundaries we can actually act on.
///
/// `queue` is time the command spent behind other worker operations — ours to
/// fix. `corewlan` is the synchronous channel sweep — not ours to fix, only to
/// stop asking for redundantly.
#[derive(Debug, Clone, Copy)]
pub struct ScanTimings {
    pub queue: Duration,
    pub corewlan: Duration,
    pub postprocess: Duration,
}

impl ScanTimings {
    /// Everything the worker thread was responsible for.
    pub fn worker_total(&self) -> Duration {
        self.queue + self.corewlan + self.postprocess
    }
}

/// In-process worker handle. The daemon uses this directly; the client never
/// constructs one. The `Local`/`Remote` enum that the TUI sees lives in
/// `app::WifiHandle`.
#[derive(Clone)]
pub struct LocalWifiHandle {
    tx: Sender<Queued>,
}

/// A command plus the instant it entered the worker queue. The worker is a
/// strict FIFO over one CoreWLAN thread, so the gap between these two points is
/// exactly the latency an unrelated slow operation imposed on this command.
struct Queued {
    command: WorkerCommand,
    enqueued: Instant,
}

/// Event sink bound to one command's origin, so `dispatch` doesn't have to
/// thread the origin through every emit site.
struct Emitter {
    tx: UnboundedSender<WorkerEvent>,
    origin: Option<Origin>,
}

impl Emitter {
    fn send(&self, event: Event) {
        let _ = self.tx.send(WorkerEvent::Emit {
            origin: self.origin,
            event,
        });
    }

    fn schedule_verify(&self, ssid: String, kind: JoinKind, attempts_remaining: u8) {
        let _ = self.tx.send(WorkerEvent::ScheduleVerify {
            origin: self.origin,
            ssid,
            kind,
            attempts_remaining,
        });
    }
}

impl LocalWifiHandle {
    pub fn spawn(events: UnboundedSender<WorkerEvent>) -> Self {
        let (tx, rx) = std_mpsc::channel::<Queued>();
        thread::Builder::new()
            .name("wifi-worker".into())
            .spawn(move || worker_loop(rx, events))
            .expect("spawn wifi-worker thread");
        Self { tx }
    }

    pub fn send_command(&self, command: WorkerCommand) {
        let _ = self.tx.send(Queued {
            command,
            enqueued: Instant::now(),
        });
    }

    pub fn send(&self, req: Request) {
        self.send_command(WorkerCommand::Request {
            origin: None,
            request: req,
        });
    }
}

/// A worker handle that records commands instead of driving CoreWLAN, so the
/// daemon's scan admission can be tested without a radio.
#[cfg(test)]
pub(crate) struct FakeWorker {
    rx: std_mpsc::Receiver<Queued>,
}

#[cfg(test)]
impl FakeWorker {
    pub(crate) fn spawn() -> (LocalWifiHandle, Self) {
        let (tx, rx) = std_mpsc::channel::<Queued>();
        (LocalWifiHandle { tx }, Self { rx })
    }

    /// The next command the daemon sent, if any.
    pub(crate) fn try_next(&self) -> Option<WorkerCommand> {
        self.rx.try_recv().ok().map(|q| q.command)
    }

    pub(crate) fn commands(&self) -> Vec<WorkerCommand> {
        std::iter::from_fn(|| self.try_next()).collect()
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

/// Work the worker performs before it accepts any client command.
///
/// Deliberately *excludes* `Request::Scan`. The daemon used to scan here, but
/// nothing is subscribed yet at that point, so the result was discarded — and
/// the client's own `send_init` scan then queued behind it, paying for a second
/// physical scan. Initial scan demand belongs to whoever actually connects.
const STARTUP_REQUESTS: &[Request] = &[Request::RefreshState, Request::RefreshPreferred];

fn worker_loop(rx: std_mpsc::Receiver<Queued>, events: UnboundedSender<WorkerEvent>) {
    // Startup failures are nobody's request, so they broadcast.
    let startup = Emitter {
        tx: events.clone(),
        origin: None,
    };
    let client = match WifiClient::shared() {
        Ok(c) => c,
        Err(e) => {
            startup.send(Event::Error(format!("CoreWLAN init failed: {e}")));
            return;
        }
    };
    let iface = match client.default_interface() {
        Ok(i) => i,
        Err(e) => {
            startup.send(Event::Error(format!("no Wi-Fi interface: {e}")));
            return;
        }
    };

    for req in STARTUP_REQUESTS {
        dispatch(&iface, &startup, req.clone(), Duration::ZERO);
    }

    while let Ok(Queued { command, enqueued }) = rx.recv() {
        let queue = enqueued.elapsed();
        match command {
            WorkerCommand::Request { origin, request } => {
                let emitter = Emitter {
                    tx: events.clone(),
                    origin,
                };
                dispatch(&iface, &emitter, request, queue);
            }
            WorkerCommand::VerifyJoin {
                origin,
                ssid,
                kind,
                attempts_remaining,
            } => {
                let emitter = Emitter {
                    tx: events.clone(),
                    origin,
                };
                check_join(&iface, &emitter, ssid, kind, attempts_remaining);
            }
            WorkerCommand::Scan { operation_id } => {
                let (result, timings) = run_scan(&iface, queue);
                let _ = events.send(WorkerEvent::ScanFinished {
                    operation_id,
                    result,
                    timings,
                });
            }
        }
    }
}

/// Handle one worker request on the CoreWLAN thread.
fn dispatch(iface: &WifiInterface, events: &Emitter, req: Request, queue: Duration) {
    match req {
        Request::RefreshState => emit_state(iface, events),
        Request::RefreshPreferred => emit_preferred(iface, events),
        Request::Scan => emit_scan(iface, events, queue),
        Request::SetPower(on) => {
            if let Err(e) = iface.set_power(on) {
                let name = iface.name();
                if let Err(e2) = networksetup::set_power(&name, on) {
                    events.send(Event::Error(format!(
                        "power toggle failed: CoreWLAN={e}; networksetup={e2}"
                    )));
                } else {
                    events.send(Event::Notice(format!(
                        "Wi-Fi {}",
                        if on { "on" } else { "off" }
                    )));
                }
            } else {
                events.send(Event::Notice(format!(
                    "Wi-Fi {}",
                    if on { "on" } else { "off" }
                )));
            }
            emit_state(iface, events);
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
                    events.send(Event::Notice(format!("connected to {ssid}")));
                }
                Err(e) => {
                    events.send(Event::Error(format!("connect failed: {e}")));
                }
            }
            emit_state(iface, events);
            emit_preferred(iface, events);
        }
        Request::JoinWithPassword { ssid, password } => {
            let name = iface.name();
            match networksetup::set_airport_network(&name, &ssid, Some(&password)) {
                Ok(()) => {
                    // Cache our own copy as soon as networksetup accepts the
                    // password — do NOT gate this on verify_join. Association
                    // + DHCP can lag the command's return by several seconds,
                    // and a slow-but-successful join used to leave nothing
                    // cached, forcing another prompt next time. A genuinely
                    // wrong password self-corrects: the next JoinSaved fails
                    // with AssociationFailed and re-prompts, overwriting this.
                    if let Err(e) = keychain::cache_password(&ssid, &password) {
                        dlog!("cache_password({ssid}) failed: {e:#}");
                    } else {
                        dlog!("cached password for {ssid}");
                    }
                    // Verification continues asynchronously: the worker must
                    // not sit on the CoreWLAN thread for ten seconds while
                    // every other request — a scan above all — waits behind it.
                    begin_verify(iface, events, ssid, JoinKind::Password);
                }
                Err(e) => {
                    dlog!("join to {ssid} failed: {e:#}");
                    events.send(Event::Error(format!("join failed: {e}")));
                    emit_state(iface, events);
                }
            }
        }
        Request::JoinSaved(ssid) => {
            // Silent-reconnect strategy. Each failure is classified so the
            // client can decide whether to re-prompt for a password (a real
            // credential problem) or just show a toast (network simply out of
            // range). We deliberately do NOT read the System keychain here —
            // its AirPort item is walled off by a partition-list ACL even from
            // root (the -25293 finding; see keychain.rs) so it would only fire
            // a useless admin dialog. macwifi's own login-keychain cache is the
            // only silent path for secured networks.
            match join_saved(iface, &ssid) {
                JoinSavedOutcome::Verify { fail_detail } => {
                    begin_verify(iface, events, ssid, JoinKind::Saved { fail_detail });
                }
                JoinSavedOutcome::Failed(reason, detail) => {
                    dlog!("JoinSaved({ssid}) failed: {reason:?} — {detail}");
                    events.send(Event::JoinSavedFailed {
                        ssid,
                        reason,
                        detail,
                    });
                    emit_state(iface, events);
                }
            }
        }
        Request::Disconnect => {
            iface.disassociate();
            events.send(Event::Notice("disconnected".into()));
            emit_state(iface, events);
        }
        Request::Share { ssid, security } => {
            let password = match security {
                ShareSecurity::Nopass => None,
                ShareSecurity::Wpa | ShareSecurity::Wep => match keychain::share_password(&ssid) {
                    Ok(password) => Some(password),
                    Err(e) => {
                        events.send(Event::Error(format!("keychain: {e} — sharing SSID only")));
                        None
                    }
                },
            };
            let (uri, has_pw) = share_uri(&ssid, security, password.as_deref());
            events.send(Event::ShareReady(SharePayload {
                schema_version: 1,
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
                    events.send(Event::Notice(format!("forgot {ssid}")));
                }
                Err(e) => {
                    events.send(Event::Error(format!("forget failed: {e}")));
                }
            }
            emit_preferred(iface, events);
        }
        Request::Diagnose => {
            emit_diagnose(iface, events);
        }
    }
}

fn emit_diagnose(iface: &WifiInterface, events: &Emitter) {
    use crate::event::DaemonDiagnose;
    let state = iface.state();
    let scan = iface.scan().unwrap_or_default();
    let blank = scan
        .iter()
        .filter(|n| n.ssid.as_deref().is_none_or(str::is_empty))
        .count();
    let location_auth_raw = unsafe {
        let mgr = objc2_core_location::CLLocationManager::new();
        mgr.authorizationStatus().0
    };
    let pid = unsafe { libc::getpid() };
    let parent_pid = unsafe { libc::getppid() };
    let (interface, current_ssid) = match &state {
        Ok(s) => (s.name.clone(), s.ssid.clone()),
        Err(_) => (iface.name(), None),
    };
    events.send(Event::DaemonDiagnose(DaemonDiagnose {
        pid,
        parent_pid,
        location_auth_raw,
        interface,
        current_ssid,
        scan_count: scan.len(),
        scan_blank: blank,
    }));
}

fn emit_state(iface: &WifiInterface, events: &Emitter) {
    match iface.state() {
        Ok(s) => {
            events.send(Event::State(s));
        }
        Err(e) => {
            events.send(Event::Error(format!("state refresh failed: {e}")));
        }
    }
}

/// What a `JoinSaved` attempt reached before any confirmation.
enum JoinSavedOutcome {
    /// An association was issued; whether it took effect is not yet known.
    /// Carries the detail to report if it never confirms.
    Verify { fail_detail: String },
    /// Failed outright, with no association attempted or one that CoreWLAN
    /// rejected immediately.
    Failed(JoinFailReason, String),
}

/// Try to silently reconnect to a saved network. Returns the specific reason on
/// failure so the client can react appropriately (re-prompt vs. toast). Never
/// falls back to a blind `associate_open` on a secured network: CoreWLAN's
/// nil-password associate does not consult saved credentials, so it can't help
/// and may tear down an association we just started.
fn join_saved(iface: &WifiInterface, ssid: &str) -> JoinSavedOutcome {
    // Is it even in range? A directed scan is the authoritative check and also
    // tells us the security type. `find_network`-style empty result ⇒ out of
    // range, which is not a credential problem.
    let scan = match iface.scan_for_ssid(ssid) {
        Ok(scan) => scan,
        Err(e) => {
            return JoinSavedOutcome::Failed(
                JoinFailReason::NotInRange,
                format!("directed scan failed: {e}"),
            );
        }
    };
    let Some(net) = scan.into_iter().next() else {
        return JoinSavedOutcome::Failed(
            JoinFailReason::NotInRange,
            "network not visible in scan".to_string(),
        );
    };

    // Open networks need no credential.
    if net.security == Security::Open {
        let detail = "open associate did not take effect".to_string();
        return if iface.associate_open(ssid).is_ok() {
            JoinSavedOutcome::Verify {
                fail_detail: detail,
            }
        } else {
            JoinSavedOutcome::Failed(JoinFailReason::AssociationFailed, detail)
        };
    }

    // Secured: use macwifi's own cached password if we have one.
    match keychain::cached_password(ssid) {
        Ok(Some(pw)) => {
            let detail = "cached password did not associate".to_string();
            if iface.associate_psk(ssid, &pw).is_ok() {
                JoinSavedOutcome::Verify {
                    fail_detail: detail,
                }
            } else {
                JoinSavedOutcome::Failed(JoinFailReason::AssociationFailed, detail)
            }
        }
        Ok(None) => JoinSavedOutcome::Failed(
            JoinFailReason::NoCachedCredential,
            "no saved credential in macwifi cache".to_string(),
        ),
        // cached_password already deleted the poisoned item; a re-entered
        // password will recreate it cleanly.
        Err(e) => JoinSavedOutcome::Failed(JoinFailReason::KeychainDenied, format!("{e:#}")),
    }
}

/// What to do after one verification check. Split out from the CoreWLAN call
/// so the retry budget can be tested without a radio.
#[derive(Debug, PartialEq, Eq)]
enum VerifyStep {
    Joined,
    Retry { attempts_remaining: u8 },
    GiveUp,
}

fn verify_step(joined: bool, attempts_remaining: u8) -> VerifyStep {
    if joined {
        VerifyStep::Joined
    } else if attempts_remaining > 0 {
        VerifyStep::Retry {
            attempts_remaining: attempts_remaining - 1,
        }
    } else {
        VerifyStep::GiveUp
    }
}

/// The event a join that never confirmed should report.
///
/// The two kinds differ deliberately: an unconfirmed `JoinWithPassword` is a
/// notice, because `networksetup` accepted the password and the association may
/// still land, while an unconfirmed `JoinSaved` is a failure the client answers
/// by prompting for a password.
fn verify_failure_event(ssid: String, kind: JoinKind) -> Event {
    match kind {
        JoinKind::Password => Event::Notice(format!(
            "join to {ssid} sent — association not yet confirmed, it may still complete"
        )),
        JoinKind::Saved { fail_detail } => Event::JoinSavedFailed {
            ssid,
            reason: JoinFailReason::AssociationFailed,
            detail: fail_detail,
        },
    }
}

/// Begin confirming a join. The first check runs inline, so a join that lands
/// immediately still reports immediately and costs nothing extra.
fn begin_verify(iface: &WifiInterface, events: &Emitter, ssid: String, kind: JoinKind) {
    check_join(iface, events, ssid, kind, VERIFY_ATTEMPTS - 1);
}

/// One verification check.
///
/// `networksetup -setairportnetwork` exits 0 and prints a *localized* "Failed…"
/// line on auth/password errors, so the only trustworthy signal is reading back
/// the interface's current SSID via CoreWLAN. (The daemon holds the Location
/// grant, so the readback isn't redacted here.) This used to poll in a
/// `thread::sleep` loop for up to ten seconds — on the single CoreWLAN thread,
/// so every scan behind it paid the full ten seconds too.
fn check_join(
    iface: &WifiInterface,
    events: &Emitter,
    ssid: String,
    kind: JoinKind,
    attempts_remaining: u8,
) {
    let joined = iface
        .state()
        .is_ok_and(|st| st.ssid.as_deref() == Some(ssid.as_str()));
    match verify_step(joined, attempts_remaining) {
        VerifyStep::Joined => {
            if matches!(kind, JoinKind::Saved { .. }) {
                dlog!("reconnected to {ssid}");
            }
            events.send(Event::Notice(format!("connected to {ssid}")));
        }
        VerifyStep::Retry { attempts_remaining } => {
            events.schedule_verify(ssid, kind, attempts_remaining);
            // Nothing is settled yet, so no state refresh: the caller gets one
            // when the sequence resolves.
            return;
        }
        VerifyStep::GiveUp => {
            dlog!("join to {ssid}: association unconfirmed after {VERIFY_ATTEMPTS} checks");
            events.send(verify_failure_event(ssid, kind));
        }
    }
    emit_state(iface, events);
}

fn emit_preferred(iface: &WifiInterface, events: &Emitter) {
    let name = iface.name();
    match networksetup::list_preferred(&name) {
        Ok(v) => {
            events.send(Event::PreferredResult(v));
        }
        Err(e) => {
            events.send(Event::Error(format!("preferred list failed: {e}")));
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

fn share_uri(ssid: &str, security: ShareSecurity, password: Option<&str>) -> (String, bool) {
    let Some(password) = password else {
        return (format!("WIFI:T:nopass;S:{};;", escape_wifi(ssid)), false);
    };
    let security = match security {
        ShareSecurity::Wep => "WEP",
        ShareSecurity::Wpa => "WPA",
        ShareSecurity::Nopass => "nopass",
    };
    (
        format!(
            "WIFI:T:{security};S:{};P:{};;",
            escape_wifi(ssid),
            escape_wifi(password)
        ),
        true,
    )
}

/// Run one physical scan, sorted strongest-signal-first, and report where the
/// time went. Post-processing is measured separately so a slow sample can be
/// blamed on the right layer.
fn run_scan(
    iface: &WifiInterface,
    queue: Duration,
) -> (Result<Vec<ScannedNetwork>, String>, ScanTimings) {
    let scan_start = Instant::now();
    let result = iface.scan();
    let corewlan = scan_start.elapsed();
    let post_start = Instant::now();
    let result = match result {
        Ok(mut n) => {
            n.sort_by_key(|x| -x.rssi);
            Ok(n)
        }
        Err(e) => Err(format!("scan failed: {e}")),
    };
    let timings = ScanTimings {
        queue,
        corewlan,
        postprocess: post_start.elapsed(),
    };
    (result, timings)
}

/// The direct `Request::Scan` path, used by an in-process `WifiHandle::Local`.
/// The daemon does not come through here — its scans are admitted by
/// `scan::ScanCoordinator` so they can be coalesced and cached — but both paths
/// share `run_scan` and the redaction diagnostic so the two cannot drift.
fn emit_scan(iface: &WifiInterface, events: &Emitter, queue: Duration) {
    events.send(Event::ScanStarted);
    let (result, timings) = run_scan(iface, queue);
    match result {
        Ok(networks) => {
            let diagnostic = crate::scan::redaction_diagnostic(&networks);
            log_scan(timings, Ok(&networks), 1);
            events.send(Event::ScanResult(networks));
            if let Some(diagnostic) = diagnostic {
                events.send(Event::Error(diagnostic));
            }
        }
        Err(e) => {
            log_scan(timings, Err(&e), 1);
            events.send(Event::ScanFailed(e));
        }
    }
}

/// One machine-readable line per scan so a slow sample can be attributed to
/// worker queueing, CoreWLAN itself, or our own post-processing.
pub fn log_scan(timings: ScanTimings, result: Result<&[ScannedNetwork], &str>, waiters: usize) {
    let (networks, blank, outcome) = match result {
        Ok(n) => (n.len(), crate::scan::blank_ssids(n), "ok"),
        Err(_) => (0, 0, "error"),
    };
    dlog!(
        "scan queue_ms={} corewlan_ms={} postprocess_ms={} worker_total_ms={} \
networks={networks} blank_ssids={blank} waiters={waiters} outcome={outcome}",
        timings.queue.as_millis(),
        timings.corewlan.as_millis(),
        timings.postprocess.as_millis(),
        timings.worker_total().as_millis(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_retries_exactly_the_configured_budget() {
        // begin_verify runs the first check inline, so the scheduled retries
        // are one fewer than the total attempts.
        let mut attempts_remaining = VERIFY_ATTEMPTS - 1;
        let mut checks = 1;
        loop {
            match verify_step(false, attempts_remaining) {
                VerifyStep::Retry {
                    attempts_remaining: n,
                } => {
                    attempts_remaining = n;
                    checks += 1;
                }
                VerifyStep::GiveUp => break,
                VerifyStep::Joined => unreachable!("never joined in this test"),
            }
            assert!(checks <= VERIFY_ATTEMPTS, "retry budget must terminate");
        }
        assert_eq!(checks, VERIFY_ATTEMPTS);
        // The old blocking loop polled for the same ~10s window.
        assert_eq!(
            VERIFY_INTERVAL * u32::from(VERIFY_ATTEMPTS),
            std::time::Duration::from_secs(10)
        );
    }

    #[test]
    fn a_confirmed_join_stops_checking_immediately() {
        assert_eq!(verify_step(true, VERIFY_ATTEMPTS - 1), VerifyStep::Joined);
        assert_eq!(verify_step(true, 0), VerifyStep::Joined);
    }

    #[test]
    fn an_unconfirmed_password_join_is_only_a_notice() {
        // networksetup accepted the password and the association may still
        // land, so this must not look like a credential failure.
        let ev = verify_failure_event("Cafe".into(), JoinKind::Password);
        match ev {
            Event::Notice(m) => assert!(m.contains("not yet confirmed"), "{m}"),
            other => panic!("expected a notice, got {other:?}"),
        }
    }

    #[test]
    fn an_unconfirmed_saved_join_reports_association_failure() {
        let ev = verify_failure_event(
            "Cafe".into(),
            JoinKind::Saved {
                fail_detail: "cached password did not associate".into(),
            },
        );
        match ev {
            Event::JoinSavedFailed {
                ssid,
                reason,
                detail,
            } => {
                assert_eq!(ssid, "Cafe");
                // The client re-prompts for a password on this reason.
                assert_eq!(reason, JoinFailReason::AssociationFailed);
                assert_eq!(detail, "cached password did not associate");
            }
            other => panic!("expected JoinSavedFailed, got {other:?}"),
        }
    }

    #[test]
    fn startup_does_not_scan() {
        // A scan here would be thrown away (no client is subscribed yet) and
        // would queue ahead of the connecting client's own scan.
        assert!(
            !STARTUP_REQUESTS.iter().any(|r| matches!(r, Request::Scan)),
            "worker startup must not perform a physical scan"
        );
    }

    #[test]
    fn startup_primes_state_and_preferred() {
        assert!(
            STARTUP_REQUESTS
                .iter()
                .any(|r| matches!(r, Request::RefreshState))
        );
        assert!(
            STARTUP_REQUESTS
                .iter()
                .any(|r| matches!(r, Request::RefreshPreferred))
        );
    }

    #[test]
    fn blank_password_creates_open_association_request() {
        let request = join_request("Airport WiFi".into(), String::new());

        assert!(matches!(
            request,
            Request::Associate(Associate {
                ssid,
                kind: AssociateKind::Open,
            }) if ssid == "Airport WiFi"
        ));
    }

    #[test]
    fn share_uri_escapes_reserved_characters() {
        let (uri, has_password) = share_uri("Cafe;WiFi", ShareSecurity::Wpa, Some("a:b\\c"));

        assert_eq!(uri, "WIFI:T:WPA;S:Cafe\\;WiFi;P:a\\:b\\\\c;;");
        assert!(has_password);
    }

    #[test]
    fn share_uri_without_password_is_open() {
        let (uri, has_password) = share_uri("Cafe", ShareSecurity::Wpa, None);

        assert_eq!(uri, "WIFI:T:nopass;S:Cafe;;");
        assert!(!has_password);
    }

    #[test]
    fn nonblank_password_is_preserved() {
        let request = join_request("Secure WiFi".into(), "secret".into());

        assert!(matches!(
            request,
            Request::JoinWithPassword { ssid, password }
                if ssid == "Secure WiFi" && password == "secret"
        ));
    }
}
