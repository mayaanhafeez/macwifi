//! Daemon server: owns the CoreWLAN worker and accepts client connections
//! over a unix socket.
//!
//! Launched by a `LaunchAgent` so its parent is `launchd` and TCC gives it
//! the bundle's Location grant. Without that the whole point is moot — see
//! `install.rs` for the plist that wires this up. The wrapper script uses
//! `/usr/bin/open -W -a /Applications/macwifi.app --args daemon` so the
//! Aqua app session is set up (verified empirically — direct launchd-exec
//! gets blank SSIDs even with the Location grant).

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::event::Event;
use crate::ipc::{self, ClientRequest, Hello, Reader, ServerEvent, Writer};
use crate::worker::{LocalWifiHandle, Origin, WorkerCommand, WorkerEvent};

/// Daemon-internal client identity, allocated once a connection has completed
/// its handshake. Never crosses the wire — the client only ever sees the
/// request ids it chose itself.
static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

/// Connected clients, by daemon-internal id.
type Clients = Arc<Mutex<HashMap<u64, UnboundedSender<ServerEvent>>>>;

/// Where an event the worker produced should go.
enum Fanout {
    /// Only the client whose request produced it. A notice about a join one
    /// client asked for is noise in every other client's UI.
    Direct,
    /// Every connected client. Interface state and the preferred-network list
    /// are properties of the machine, not of the request that revealed them,
    /// so a second TUI must not go stale because someone else asked.
    Global,
}

fn fanout_of(event: &Event) -> Fanout {
    match event {
        Event::State(_) | Event::PreferredResult(_) => Fanout::Global,
        Event::ScanStarted
        | Event::ScanResult(_)
        | Event::Notice(_)
        | Event::Error(_)
        | Event::ShareReady(_)
        | Event::JoinSavedFailed { .. }
        | Event::DaemonDiagnose(_) => Fanout::Direct,
    }
}

/// Deliver one worker event to the client(s) that should see it.
///
/// A `Global` event still carries the requesting client's id *for that client*,
/// so a CLI one-shot waiting on `Event::State` can recognise its own reply
/// while every other client receives the same event unsolicited.
fn route(clients: &Clients, origin: Option<Origin>, event: Event) {
    let mut map = clients.lock().unwrap();
    match (origin, fanout_of(&event)) {
        (Some(o), Fanout::Direct) => {
            if let Some(tx) = map.get(&o.client_id)
                && tx
                    .send(ServerEvent {
                        request_id: Some(o.request_id),
                        event,
                    })
                    .is_err()
            {
                map.remove(&o.client_id);
            }
        }
        (Some(o), Fanout::Global) => {
            map.retain(|id, tx| {
                let request_id = (*id == o.client_id).then_some(o.request_id);
                tx.send(ServerEvent {
                    request_id,
                    event: event.clone(),
                })
                .is_ok()
            });
        }
        (None, _) => {
            map.retain(|_, tx| tx.send(ServerEvent::unsolicited(event.clone())).is_ok());
        }
    }
}

pub async fn run() -> Result<()> {
    crate::logging::init();
    crate::dlog!("daemon starting (build {})", ipc::BUILD_ID);
    let path = ipc::socket_path();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create {}", parent.display()))?;
    }
    // Remove stale socket from a prior run.
    let _ = tokio::fs::remove_file(&path).await;
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))?;
    crate::dlog!("listening at {}", path.display());

    // Fire Location prompt + start the CFRunLoop pump. Runs forever on a
    // background thread; we need it both for TCC and for CoreWLAN's
    // "process behaves like a GUI app" check. This is done *after* binding the
    // socket: on a first-ever launch it can block up to 30s waiting for the
    // user to answer the TCC prompt, and doing it before the bind would make
    // the TUI's 2s connect retry give up with "daemon unreachable".
    crate::location::request_when_in_use();

    let (worker_tx, mut worker_rx) = mpsc::unbounded_channel::<WorkerEvent>();
    let wifi = LocalWifiHandle::spawn(worker_tx);

    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let clients_task = clients.clone();
    tokio::spawn(async move {
        while let Some(ev) = worker_rx.recv().await {
            match ev {
                WorkerEvent::Emit { origin, event } => route(&clients_task, origin, event),
            }
        }
    });

    loop {
        let (stream, _) = listener.accept().await.context("accept unix connection")?;
        let wifi = wifi.clone();
        let clients = clients.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_one(stream, wifi, clients).await {
                crate::dlog!("client task ended: {e:#}");
            }
        });
    }
}

async fn serve_one(stream: UnixStream, wifi: LocalWifiHandle, clients: Clients) -> Result<()> {
    if !peer_uid_matches(&stream)? {
        bail!("peer uid mismatch — refusing connection");
    }

    let (read_half, write_half) = stream.into_split();
    let mut reader: Reader = BufReader::new(read_half);
    let mut writer: Writer = write_half;

    // Handshake: server speaks first.
    ipc::write_line(
        &mut writer,
        &Hello {
            version: ipc::PROTOCOL_VERSION,
            build: ipc::BUILD_ID.to_string(),
        },
    )
    .await?;
    let peer_hello: Option<Hello> = ipc::read_line(&mut reader).await?;
    let peer_hello = peer_hello.ok_or_else(|| anyhow::anyhow!("client closed before hello"))?;
    if peer_hello.version != ipc::PROTOCOL_VERSION {
        bail!(
            "protocol version mismatch (client {}, server {})",
            peer_hello.version,
            ipc::PROTOCOL_VERSION
        );
    }

    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<ServerEvent>();
    clients.lock().unwrap().insert(client_id, client_tx);
    crate::dlog!("client {client_id} connected");

    // Writer task: drain client_rx → socket.
    let writer_task = tokio::spawn(async move {
        while let Some(ev) = client_rx.recv().await {
            if let Err(e) = ipc::write_line(&mut writer, &ev).await {
                crate::dlog!("daemon writer: {e:#}");
                break;
            }
        }
        let _ = writer.shutdown().await;
    });

    // Reader task (this task): parse requests → worker commands.
    let result = read_requests(&mut reader, &wifi, client_id).await;

    clients.lock().unwrap().remove(&client_id);
    crate::dlog!("client {client_id} disconnected");
    writer_task.abort();
    result
}

async fn read_requests(reader: &mut Reader, wifi: &LocalWifiHandle, client_id: u64) -> Result<()> {
    loop {
        let req: Option<ClientRequest> = ipc::read_line(reader).await?;
        let Some(ClientRequest { id, request }) = req else {
            return Ok(());
        };
        wifi.send_command(WorkerCommand::Request {
            origin: Some(Origin {
                client_id,
                request_id: id,
            }),
            request,
        });
    }
}

fn peer_uid_matches(stream: &UnixStream) -> Result<bool> {
    use std::mem;
    #[repr(C)]
    struct Xucred {
        cr_version: libc::c_uint,
        cr_uid: libc::uid_t,
        cr_ngroups: libc::c_short,
        cr_groups: [libc::gid_t; 16],
    }
    const LOCAL_PEERCRED: libc::c_int = 0x001;

    let fd = stream.as_raw_fd();
    let mut cred: Xucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<Xucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            0, // SOL_LOCAL
            LOCAL_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("LOCAL_PEERCRED");
    }
    let self_uid = unsafe { libc::geteuid() };
    Ok(cred.cr_uid == self_uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corewlan::InterfaceState;
    use tokio::sync::mpsc::UnboundedReceiver;

    fn client(clients: &Clients, id: u64) -> UnboundedReceiver<ServerEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        clients.lock().unwrap().insert(id, tx);
        rx
    }

    fn origin(client_id: u64, request_id: u64) -> Option<Origin> {
        Some(Origin {
            client_id,
            request_id,
        })
    }

    fn state() -> Event {
        Event::State(InterfaceState {
            name: "en0".into(),
            powered: true,
            hw_address: None,
            ssid: None,
            bssid: None,
            rssi: -50,
            noise: -90,
            tx_rate: 0.0,
            channel: None,
        })
    }

    #[test]
    fn direct_events_reach_only_the_requesting_client() {
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let mut asked = client(&clients, 1);
        let mut bystander = client(&clients, 2);

        route(&clients, origin(1, 7), Event::Notice("connected".into()));

        let ev = asked.try_recv().expect("requester gets the notice");
        assert_eq!(ev.request_id, Some(7));
        assert!(bystander.try_recv().is_err(), "bystander sees nothing");
    }

    #[test]
    fn concurrent_requests_cannot_complete_from_each_others_events() {
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let mut one = client(&clients, 1);
        let mut two = client(&clients, 2);

        route(&clients, origin(1, 100), Event::ScanResult(Vec::new()));
        route(&clients, origin(2, 200), Event::ScanResult(Vec::new()));

        assert_eq!(one.try_recv().unwrap().request_id, Some(100));
        assert_eq!(two.try_recv().unwrap().request_id, Some(200));
        assert!(one.try_recv().is_err());
        assert!(two.try_recv().is_err());
    }

    #[test]
    fn global_events_reach_everyone_but_only_the_requester_is_correlated() {
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let mut asked = client(&clients, 1);
        let mut bystander = client(&clients, 2);

        route(&clients, origin(1, 7), state());

        assert_eq!(asked.try_recv().unwrap().request_id, Some(7));
        // A second TUI must still see the new interface state — it just did
        // not ask for it, so it arrives unsolicited.
        assert_eq!(bystander.try_recv().unwrap().request_id, None);
    }

    #[test]
    fn originless_events_broadcast_uncorrelated() {
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let mut one = client(&clients, 1);
        let mut two = client(&clients, 2);

        route(&clients, None, Event::Error("CoreWLAN init failed".into()));

        assert_eq!(one.try_recv().unwrap().request_id, None);
        assert_eq!(two.try_recv().unwrap().request_id, None);
    }

    #[test]
    fn disconnected_clients_are_dropped_from_the_registry() {
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let mut alive = client(&clients, 1);
        drop(client(&clients, 2));

        route(&clients, None, state());
        assert!(alive.try_recv().is_ok());
        assert_eq!(clients.lock().unwrap().len(), 1);

        // A direct send to a departed client prunes it too.
        drop(client(&clients, 3));
        route(&clients, origin(3, 1), Event::Notice("gone".into()));
        assert!(!clients.lock().unwrap().contains_key(&3));
    }
}
