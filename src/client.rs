//! Client side of the daemon socket. Used by the TUI (long-lived
//! connection that transparently reconnects) and by CLI subcommands
//! (short-lived one-shots).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::sleep;

use crate::event::Event;
use crate::ipc::{self, ClientRequest, Hello, ServerEvent};
use crate::worker::Request;

/// Hands the TUI an object with the same `send(Request)` API the old
/// in-process `WifiHandle` had. Behind the scenes a single tokio task owns
/// the socket: it forwards outbound requests and pushes inbound `ServerEvent`s
/// into the channel the main loop already reads.
#[derive(Clone)]
pub struct RemoteWifiHandle {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) tx: UnboundedSender<ClientRequest>,
    pub(crate) next_id: Arc<AtomicU64>,
}

/// One live, handshaken connection to the daemon.
struct Conn {
    reader: ipc::Reader,
    write_half: ipc::Writer,
}

enum ConnOutcome {
    /// Socket died (EOF/broken pipe) but the app still holds the handle.
    Disconnected,
    /// Every `RemoteWifiHandle` was dropped — the app is shutting down.
    HandleDropped,
}

impl RemoteWifiHandle {
    /// Connect for the long-lived TUI. Transparently reconnects if the daemon
    /// restarts (e.g. after a crash the LaunchAgent relaunches it), and
    /// re-requests a state snapshot on each (re)connect.
    pub async fn connect(events: UnboundedSender<ServerEvent>) -> Result<Self> {
        Self::connect_inner(events, true, true).await
    }

    /// Connect for a short-lived CLI one-shot: no auto state-dump (the caller
    /// sends its own request) and no reconnect (a closed socket ends the
    /// session).
    pub async fn connect_oneshot(events: UnboundedSender<ServerEvent>) -> Result<Self> {
        Self::connect_inner(events, false, false).await
    }

    async fn connect_inner(
        events: UnboundedSender<ServerEvent>,
        auto_init: bool,
        reconnect: bool,
    ) -> Result<Self> {
        let path = ipc::socket_path();
        // First connection is synchronous so startup failures surface to the
        // caller (e.g. "daemon unreachable — run `macwifi install-daemon`").
        let conn = connect_with_retry(&path, &events).await?;

        let (req_tx, req_rx) = mpsc::unbounded_channel::<ClientRequest>();
        // Shared with `supervise` so the per-reconnect init burst draws from
        // the same id sequence as ordinary sends.
        let next_id = Arc::new(AtomicU64::new(1));
        tokio::spawn(supervise(
            path,
            conn,
            req_rx,
            events,
            auto_init,
            reconnect,
            next_id.clone(),
        ));

        Ok(Self {
            inner: Arc::new(Inner {
                tx: req_tx,
                next_id,
            }),
        })
    }

    pub fn send(&self, req: Request) {
        let _ = self.send_tracked(req);
    }

    /// Send a request and return the id its replies will carry. Callers that
    /// wait for a specific answer (the CLI one-shots) need this; the TUI, which
    /// reacts to whatever arrives, does not.
    pub fn send_tracked(&self, req: Request) -> u64 {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.inner.tx.send(ClientRequest { id, request: req });
        id
    }
}

/// Owns the connection lifecycle for one handle: drives a connection until it
/// drops, then (if `reconnect`) re-establishes it. Holds `req_rx` across
/// reconnects so requests queued during an outage are flushed once we're back.
#[allow(clippy::too_many_arguments)]
async fn supervise(
    path: std::path::PathBuf,
    mut conn: Conn,
    mut req_rx: UnboundedReceiver<ClientRequest>,
    events: UnboundedSender<ServerEvent>,
    auto_init: bool,
    reconnect: bool,
    next_id: Arc<AtomicU64>,
) {
    let mut pending = None;
    loop {
        if auto_init {
            send_init(&mut conn.write_half, &next_id).await;
        }
        let (rx, outcome, failed_request) =
            run_connection(conn, req_rx, pending.take(), &events).await;
        req_rx = rx;
        pending = failed_request;

        match outcome {
            ConnOutcome::HandleDropped => return,
            ConnOutcome::Disconnected => {
                if !reconnect {
                    let _ = events.send(ServerEvent::unsolicited(Event::Error(
                        "daemon connection closed — restart with `macwifi install-daemon`".into(),
                    )));
                    return;
                }
                let _ = events.send(ServerEvent::unsolicited(Event::Notice(
                    "daemon connection lost — reconnecting…".into(),
                )));
                match connect_with_retry(&path, &events).await {
                    Ok(c) => conn = c,
                    Err(e) => {
                        let _ = events.send(ServerEvent::unsolicited(Event::Error(format!(
                            "daemon unreachable — {e}. Restart with `macwifi install-daemon`"
                        ))));
                        return;
                    }
                }
            }
        }
    }
}

/// Pump one connection: forward inbound events and outbound requests until the
/// socket dies or the handle is dropped. Returns `req_rx` so the next
/// connection can keep draining the same queue.
///
/// The blocking `read_line` runs in its own task that is never cancelled
/// (tokio's `read_line` is not cancel-safe — a `select!` could drop it
/// mid-line and lose bytes). The `select!` here only ever cancels the
/// cancel-safe `req_rx.recv()` and the reader task's `JoinHandle`.
async fn run_connection(
    conn: Conn,
    mut req_rx: UnboundedReceiver<ClientRequest>,
    mut pending: Option<ClientRequest>,
    events: &UnboundedSender<ServerEvent>,
) -> (
    UnboundedReceiver<ClientRequest>,
    ConnOutcome,
    Option<ClientRequest>,
) {
    let Conn {
        mut reader,
        mut write_half,
    } = conn;

    let reader_events = events.clone();
    let mut reader_handle = tokio::spawn(async move {
        loop {
            match ipc::read_line::<_, ServerEvent>(&mut reader).await {
                Ok(Some(ev)) => {
                    if reader_events.send(ev).is_err() {
                        return;
                    }
                }
                // Clean EOF or a read error both mean this connection is done;
                // the supervisor decides whether to reconnect.
                Ok(None) | Err(_) => return,
            }
        }
    });

    let outcome = loop {
        if let Some(req) = pending.take()
            && ipc::write_line(&mut write_half, &req).await.is_err()
        {
            pending = Some(req);
            break ConnOutcome::Disconnected;
        }
        tokio::select! {
            maybe_req = req_rx.recv() => match maybe_req {
                Some(req) => {
                    if ipc::write_line(&mut write_half, &req).await.is_err() {
                        pending = Some(req);
                        break ConnOutcome::Disconnected;
                    }
                }
                None => break ConnOutcome::HandleDropped,
            },
            _ = &mut reader_handle => break ConnOutcome::Disconnected,
        }
    };

    reader_handle.abort();
    let _ = write_half.shutdown().await;
    (req_rx, outcome, pending)
}

/// Ask the daemon for a fresh snapshot so the (re)connected TUI isn't blank or
/// stale. Best-effort: a write failure here just means the connection already
/// died and the supervisor will notice on the next loop.
async fn send_init(write_half: &mut ipc::Writer, next_id: &AtomicU64) {
    for request in [
        Request::RefreshState,
        Request::RefreshPreferred,
        Request::Scan,
    ] {
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        if ipc::write_line(write_half, &ClientRequest { id, request })
            .await
            .is_err()
        {
            break;
        }
    }
}

/// One connect attempt: open the socket and complete the version handshake.
/// Emits a `Notice` if the daemon reports a different build than this client —
/// the usual symptom of a stale daemon left running after a rebuild.
async fn establish(path: &Path, events: &UnboundedSender<ServerEvent>) -> Result<Conn> {
    let stream = UnixStream::connect(path).await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Handshake: server speaks first.
    let server_hello: Option<Hello> = ipc::read_line(&mut reader).await?;
    let server_hello = server_hello.ok_or_else(|| anyhow::anyhow!("daemon closed before hello"))?;
    if server_hello.version != ipc::PROTOCOL_VERSION {
        bail!(
            "protocol version mismatch (server {}, client {}) — the daemon is a \
             stale build; run scripts/reinstall.sh",
            server_hello.version,
            ipc::PROTOCOL_VERSION
        );
    }
    if !server_hello.build.is_empty() && server_hello.build != ipc::BUILD_ID {
        let _ = events.send(ServerEvent::unsolicited(Event::Notice(format!(
            "daemon is running build {}, client is {} — run scripts/reinstall.sh to update",
            server_hello.build,
            ipc::BUILD_ID
        ))));
    }
    ipc::write_line(
        &mut write_half,
        &Hello {
            version: ipc::PROTOCOL_VERSION,
            build: ipc::BUILD_ID.to_string(),
        },
    )
    .await?;

    Ok(Conn { reader, write_half })
}

/// Connect with brief retry to cover the launchd-restart window.
async fn connect_with_retry(path: &Path, events: &UnboundedSender<ServerEvent>) -> Result<Conn> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..20 {
        match establish(path, events).await {
            Ok(conn) => return Ok(conn),
            Err(e) => {
                last_err = Some(e.context(format!(
                    "connect {} (attempt {})",
                    path.display(),
                    attempt + 1
                )));
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!(
            "daemon unreachable at {} — run `macwifi install-daemon`",
            path.display()
        )
    }))
}

/// Open a one-shot connection, send one request, drain events until
/// `terminal` matches one, then disconnect. Used by every CLI subcommand
/// that operates through the daemon.
///
/// Only events correlated to *our* request can satisfy `terminal`. With
/// several clients on one daemon an uncorrelated stream would let a
/// concurrent `macwifi scan` hand us its result — or, worse, let another
/// client's failure look like ours. Uncorrelated errors are the one
/// exception: a daemon-wide failure or a dropped socket carries no request
/// id, and swallowing it would leave the CLI hanging until its timeout.
pub async fn cli_one_shot(req: Request, terminal: impl Fn(&Event) -> bool) -> Result<Vec<Event>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    let handle = RemoteWifiHandle::connect_oneshot(tx)
        .await
        .context("connect to daemon")?;
    let request_id = handle.send_tracked(req);

    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let ev = tokio::time::timeout(remaining, rx.recv())
            .await
            .context("daemon reply timed out")?
            .ok_or_else(|| anyhow::anyhow!("daemon channel closed"))?;
        let ours = ev.request_id == Some(request_id);
        if !ours && !matches!(ev.event, Event::Error(_) if ev.request_id.is_none()) {
            continue;
        }
        if matches!(&ev.event, Event::Notice(message) if message.starts_with("daemon is running build "))
        {
            continue;
        }
        let done = terminal(&ev.event);
        events.push(ev.event);
        if done {
            return Ok(events);
        }
    }
}
