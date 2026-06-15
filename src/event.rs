//! Two event types feeding the TUI main loop.
//!
//! - `Event` is the **wire** type that flows from the daemon to the client
//!   over the unix socket. It must serialize cleanly, so it contains no
//!   crossterm or timer state.
//! - `UiEvent` is **local** to the client and never crosses the socket.
//!   `EventHandler` produces these from crossterm input and the tick timer.
//!
//! `main.rs::drive` selects over both channels, dispatching `UiEvent` to the
//! key handler / app tick and `Event` to `App::handle_event`.

use anyhow::Result;
use std::time::Duration;

use crossterm::event::{Event as CtEvent, KeyEvent};
use futures::{FutureExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::corewlan::{InterfaceState, ScannedNetwork};

/// Events the daemon emits in response to a `Request`. Serializable so they
/// can be JSON-encoded over the unix socket; the same type is also used
/// in-process when running the worker locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    State(InterfaceState),
    ScanStarted,
    ScanResult(Vec<ScannedNetwork>),
    PreferredResult(Vec<String>),
    Notice(String),
    Error(String),
    ShareReady(SharePayload),
    JoinSavedFailed { ssid: String, reason: String },
    DaemonDiagnose(DaemonDiagnose),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharePayload {
    pub ssid: String,
    pub uri: String,
    pub has_password: bool,
}

/// Snapshot the daemon reports back when the client runs `diagnose`. Mirrors
/// the local diagnose section so a user sees the daemon's TCC environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonDiagnose {
    pub pid: i32,
    pub parent_pid: i32,
    pub location_auth_raw: i32,
    pub interface: String,
    pub current_ssid: Option<String>,
    pub scan_count: usize,
    pub scan_blank: usize,
}

/// Events local to the client process. Carry crossterm types that can't be
/// serialized — never cross the socket.
#[derive(Debug, Clone)]
pub enum UiEvent {
    Tick,
    Key(KeyEvent),
    Resize(u16, u16),
}

/// Fans crossterm input + a tick timer into a single `UiEvent` channel. The
/// wire `Event` channel is driven separately by `client::RemoteWifiHandle`
/// (in client mode) or the worker (in daemon mode).
pub struct UiEventHandler {
    pub rx: mpsc::UnboundedReceiver<UiEvent>,
}

impl UiEventHandler {
    pub fn new(tick_ms: u64) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut reader = crossterm::event::EventStream::new();
            let mut tick = tokio::time::interval(Duration::from_millis(tick_ms));
            loop {
                let tick_delay = tick.tick();
                let next = reader.next().fuse();
                tokio::select! {
                    () = tx.closed() => break,
                    _ = tick_delay => {
                        if tx.send(UiEvent::Tick).is_err() { break; }
                    }
                    Some(Ok(evt)) = next => match evt {
                        CtEvent::Key(k) if k.kind == crossterm::event::KeyEventKind::Press => {
                            if tx.send(UiEvent::Key(k)).is_err() { break; }
                        }
                        CtEvent::Resize(w, h) => {
                            if tx.send(UiEvent::Resize(w, h)).is_err() { break; }
                        }
                        _ => {}
                    }
                }
            }
        });
        Self { rx }
    }

    pub async fn next(&mut self) -> Result<UiEvent> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("ui event channel closed"))
    }
}
