//! Client side of the daemon socket. Used by the TUI (long-lived
//! connection) and by CLI subcommands (short-lived one-shots).

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::sleep;

use crate::event::Event;
use crate::ipc::{self, Hello};
use crate::worker::Request;

/// Hands the TUI an object with the same `send(Request)` API the old
/// in-process `WifiHandle` had. Behind the scenes a single tokio task owns
/// the socket: it forwards outbound `Request`s and pushes inbound `Event`s
/// into the channel the main loop already reads.
#[derive(Clone)]
pub struct RemoteWifiHandle {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) tx: UnboundedSender<Request>,
    #[allow(dead_code)]
    pub(crate) next_id: AtomicU64,
}

impl RemoteWifiHandle {
    /// Connect (with brief retry to cover the launchd-restart window) and
    /// spawn the I/O task. The task writes inbound events into `events`.
    pub async fn connect(events: UnboundedSender<Event>) -> Result<Self> {
        let path = ipc::socket_path();
        let mut stream = None;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..20 {
            match UnixStream::connect(&path).await {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => {
                    last_err = Some(anyhow::Error::new(e).context(format!(
                        "connect {} (attempt {})",
                        path.display(),
                        attempt + 1
                    )));
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
        let stream = stream.ok_or_else(|| {
            last_err.unwrap_or_else(|| {
                anyhow::anyhow!(
                    "daemon unreachable at {} — run `macwifi install-daemon`",
                    path.display()
                )
            })
        })?;

        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // Handshake: server speaks first.
        let server_hello: Option<Hello> = ipc::read_line(&mut reader).await?;
        let server_hello =
            server_hello.ok_or_else(|| anyhow::anyhow!("daemon closed before hello"))?;
        if server_hello.version != ipc::PROTOCOL_VERSION {
            bail!(
                "protocol version mismatch (server {}, client {})",
                server_hello.version,
                ipc::PROTOCOL_VERSION
            );
        }
        ipc::write_line(
            &mut write_half,
            &Hello {
                version: ipc::PROTOCOL_VERSION,
            },
        )
        .await?;

        let (req_tx, mut req_rx) = mpsc::unbounded_channel::<Request>();

        // Writer task: drain outbound Request queue to the socket.
        tokio::spawn(async move {
            while let Some(req) = req_rx.recv().await {
                if ipc::write_line(&mut write_half, &req).await.is_err() {
                    break;
                }
            }
            let _ = write_half.shutdown().await;
        });

        // Reader task: forward inbound Events into the UI channel.
        let ui_events = events.clone();
        tokio::spawn(async move {
            loop {
                match ipc::read_line::<_, Event>(&mut reader).await {
                    Ok(Some(ev)) => {
                        if ui_events.send(ev).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = ui_events.send(Event::Error(format!("daemon read: {e}")));
                        break;
                    }
                }
            }
            let _ = ui_events.send(Event::Error(
                "daemon connection closed — restart with `macwifi install-daemon`".into(),
            ));
        });

        Ok(Self {
            inner: Arc::new(Inner {
                tx: req_tx,
                next_id: AtomicU64::new(1),
            }),
        })
    }

    pub fn send(&self, req: Request) {
        let _ = self.inner.tx.send(req);
    }
}

/// Open a one-shot connection, send one request, drain events until
/// `terminal` matches one, then disconnect. Used by every CLI subcommand
/// that operates through the daemon.
pub async fn cli_one_shot(
    req: Request,
    terminal: impl Fn(&Event) -> bool,
) -> Result<Vec<Event>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let handle = RemoteWifiHandle::connect(tx).await.context("connect to daemon")?;
    handle.send(req);

    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let ev = tokio::time::timeout(remaining, rx.recv())
            .await
            .context("daemon reply timed out")?
            .ok_or_else(|| anyhow::anyhow!("daemon channel closed"))?;
        let done = terminal(&ev);
        events.push(ev);
        if done {
            return Ok(events);
        }
    }
}
