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
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::corewlan::ScannedNetwork;
use crate::event::Event;
use crate::ipc::{self, ClientRequest, Hello, Reader, ServerEvent, Writer};
use crate::scan::{self, ScanCoordinator, ScanDecision, ScanWaiter};
use crate::worker::{
    JoinKind, LocalWifiHandle, Origin, Request, ScanTimings, WorkerCommand, WorkerEvent,
};

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
        | Event::ScanFailed(_)
        | Event::Notice(_)
        | Event::Error(_)
        | Event::ShareReady(_)
        | Event::JoinSavedFailed { .. }
        | Event::DaemonDiagnose(_) => Fanout::Direct,
    }
}

/// Everything a connection task needs to service a request: who is connected,
/// what the worker is doing, and who is waiting on a scan.
#[derive(Clone)]
struct Hub {
    clients: Clients,
    coordinator: Arc<Mutex<ScanCoordinator>>,
    wifi: LocalWifiHandle,
}

impl Hub {
    fn route(&self, origin: Option<Origin>, event: Event) {
        route(&self.clients, origin, event);
    }

    fn origin(waiter: ScanWaiter) -> Option<Origin> {
        Some(Origin {
            client_id: waiter.client_id,
            request_id: waiter.request_id,
        })
    }

    /// Send a scan result (plus its redaction diagnostic, if any) to one
    /// waiter. Each client needs its own `Vec` because the reply is serialized
    /// per connection; the `Arc` saves the copy between cache and events.
    fn deliver_scan(
        &self,
        waiter: ScanWaiter,
        networks: &Arc<Vec<ScannedNetwork>>,
        diagnostic: Option<&str>,
    ) {
        let origin = Self::origin(waiter);
        self.route(origin, Event::ScanResult(networks.as_ref().clone()));
        if let Some(diagnostic) = diagnostic {
            self.route(origin, Event::Error(diagnostic.to_string()));
        }
    }

    /// Admit one `Request::Scan`, coalescing it with any scan already running
    /// and answering it from cache when a recent result is still good.
    fn scan_requested(&self, waiter: ScanWaiter) {
        let received = Instant::now();
        let origin = Self::origin(waiter);
        let decision = self.coordinator.lock().unwrap().request(waiter, received);
        // Every path emits `ScanStarted` first: it is what moves the TUI into
        // its scanning state, and on a cache hit the result follows in the
        // same breath.
        self.route(origin, Event::ScanStarted);
        match decision {
            ScanDecision::Cached(networks) => {
                let diagnostic = scan::redaction_diagnostic(&networks);
                self.deliver_scan(waiter, &networks, diagnostic.as_deref());
                crate::dlog!(
                    "scan client={} request_id={} cache=hit coalesced=false total_ms={} \
networks={} blank_ssids={} waiters=1 outcome=ok",
                    waiter.client_id,
                    waiter.request_id,
                    received.elapsed().as_millis(),
                    networks.len(),
                    scan::blank_ssids(&networks),
                );
            }
            ScanDecision::Coalesced => {
                crate::dlog!(
                    "scan client={} request_id={} cache=miss coalesced=true — joined running scan",
                    waiter.client_id,
                    waiter.request_id,
                );
            }
            ScanDecision::Start { operation_id } => {
                crate::dlog!(
                    "scan client={} request_id={} cache=miss coalesced=false \
operation={operation_id} — starting physical scan",
                    waiter.client_id,
                    waiter.request_id,
                );
                self.wifi.send_command(WorkerCommand::Scan { operation_id });
            }
        }
    }

    /// Hand a finished physical scan to everyone who waited on it.
    fn scan_finished(
        &self,
        operation_id: u64,
        result: Result<Vec<ScannedNetwork>, String>,
        timings: ScanTimings,
    ) {
        let finished =
            self.coordinator
                .lock()
                .unwrap()
                .finish(operation_id, result, Instant::now());
        let Some(completion) = finished else {
            crate::dlog!("scan operation={operation_id} was superseded — dropping its result");
            return;
        };
        let waiters = completion.waiters.len();
        match &completion.result {
            Ok(networks) => {
                // Computed once so every waiter on this sweep is told the same
                // thing.
                let diagnostic = scan::redaction_diagnostic(networks);
                for waiter in &completion.waiters {
                    self.deliver_scan(*waiter, networks, diagnostic.as_deref());
                }
                crate::worker::log_scan(timings, Ok(networks), waiters);
            }
            Err(e) => {
                for waiter in &completion.waiters {
                    self.route(Self::origin(*waiter), Event::ScanFailed(e.clone()));
                }
                crate::worker::log_scan(timings, Err(e), waiters);
            }
        }
        crate::dlog!(
            "scan operation={operation_id} coordinator_ms={} waiters={waiters}",
            completion.elapsed.as_millis(),
        );
    }

    /// Wait out one verification interval, then hand the check back to the
    /// CoreWLAN thread.
    ///
    /// The worker owns non-`Send` CoreWLAN handles, so the check itself has to
    /// happen there — but the *waiting* does not, and doing it there blocked
    /// every other request for up to ten seconds. A verification whose client
    /// has since disconnected is still allowed to finish: abandoning it would
    /// not undo the association, and `route` already drops the reply for a
    /// client that is gone.
    fn schedule_verify(
        &self,
        origin: Option<Origin>,
        ssid: String,
        kind: JoinKind,
        attempts_remaining: u8,
    ) {
        let wifi = self.wifi.clone();
        tokio::spawn(async move {
            tokio::time::sleep(crate::worker::VERIFY_INTERVAL).await;
            wifi.send_command(WorkerCommand::VerifyJoin {
                origin,
                ssid,
                kind,
                attempts_remaining,
            });
        });
    }

    fn client_disconnected(&self, client_id: u64) {
        self.clients.lock().unwrap().remove(&client_id);
        self.coordinator.lock().unwrap().forget_client(client_id);
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

    let hub = Hub {
        clients: Arc::new(Mutex::new(HashMap::new())),
        coordinator: Arc::new(Mutex::new(ScanCoordinator::default())),
        wifi,
    };

    let hub_task = hub.clone();
    tokio::spawn(async move {
        while let Some(ev) = worker_rx.recv().await {
            match ev {
                WorkerEvent::Emit { origin, event } => hub_task.route(origin, event),
                WorkerEvent::ScanFinished {
                    operation_id,
                    result,
                    timings,
                } => hub_task.scan_finished(operation_id, result, timings),
                WorkerEvent::ScheduleVerify {
                    origin,
                    ssid,
                    kind,
                    attempts_remaining,
                } => hub_task.schedule_verify(origin, ssid, kind, attempts_remaining),
            }
        }
    });

    loop {
        let (stream, _) = listener.accept().await.context("accept unix connection")?;
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_one(stream, hub).await {
                crate::dlog!("client task ended: {e:#}");
            }
        });
    }
}

async fn serve_one(stream: UnixStream, hub: Hub) -> Result<()> {
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
    hub.clients.lock().unwrap().insert(client_id, client_tx);
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
    let result = read_requests(&mut reader, &hub, client_id).await;

    hub.client_disconnected(client_id);
    crate::dlog!("client {client_id} disconnected");
    writer_task.abort();
    result
}

async fn read_requests(reader: &mut Reader, hub: &Hub, client_id: u64) -> Result<()> {
    loop {
        let req: Option<ClientRequest> = ipc::read_line(reader).await?;
        let Some(ClientRequest { id, request }) = req else {
            return Ok(());
        };
        // Scans are admitted by the coordinator, which may answer from cache
        // or fold this request into a sweep already in progress. Everything
        // else goes straight to the CoreWLAN thread.
        if matches!(request, Request::Scan) {
            hub.scan_requested(ScanWaiter {
                client_id,
                request_id: id,
            });
            continue;
        }
        hub.wifi.send_command(WorkerCommand::Request {
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

/// Socket-level tests for scan admission. They run the real `serve_one` over a
/// real unix socket with a fake worker in place of CoreWLAN, so request
/// correlation and coalescing are exercised end to end.
#[cfg(test)]
mod socket_tests {
    use super::*;
    use crate::corewlan::{ScannedNetwork, Security};
    use crate::worker::{FakeWorker, ScanTimings};
    use std::time::Duration;
    use tokio::net::UnixStream;

    struct Harness {
        hub: Hub,
        worker: FakeWorker,
        path: std::path::PathBuf,
        _dir: TempDir,
    }

    /// A unique directory removed on drop. The socket path must be short —
    /// `sun_path` is 104 bytes on macOS — so this stays under the plain temp
    /// dir rather than nesting.
    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Harness {
        fn start() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "macwifi-t{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("d.sock");
            let listener = UnixListener::bind(&path).unwrap();

            let (wifi, worker) = FakeWorker::spawn();
            let hub = Hub {
                clients: Arc::new(Mutex::new(HashMap::new())),
                coordinator: Arc::new(Mutex::new(ScanCoordinator::default())),
                wifi,
            };

            let accept_hub = hub.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let hub = accept_hub.clone();
                    tokio::spawn(async move {
                        let _ = serve_one(stream, hub).await;
                    });
                }
            });

            Self {
                hub,
                worker,
                path,
                _dir: TempDir(dir),
            }
        }

        async fn connect(&self) -> Client {
            let stream = UnixStream::connect(&self.path).await.unwrap();
            let (read_half, mut writer) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let hello: Hello = ipc::read_line(&mut reader).await.unwrap().unwrap();
            assert_eq!(hello.version, ipc::PROTOCOL_VERSION);
            ipc::write_line(
                &mut writer,
                &Hello {
                    version: ipc::PROTOCOL_VERSION,
                    build: ipc::BUILD_ID.to_string(),
                },
            )
            .await
            .unwrap();
            Client {
                reader,
                writer: Some(writer),
                next_id: 0,
            }
        }
    }

    struct Client {
        reader: Reader,
        writer: Option<Writer>,
        next_id: u64,
    }

    impl Client {
        async fn send(&mut self, request: Request) -> u64 {
            self.next_id += 1;
            ipc::write_line(
                self.writer.as_mut().unwrap(),
                &ClientRequest {
                    id: self.next_id,
                    request,
                },
            )
            .await
            .unwrap();
            self.next_id
        }

        async fn next(&mut self) -> ServerEvent {
            tokio::time::timeout(Duration::from_secs(5), ipc::read_line(&mut self.reader))
                .await
                .expect("daemon replied within 5s")
                .unwrap()
                .expect("connection stayed open")
        }

        fn disconnect(&mut self) {
            self.writer.take();
        }
    }

    fn networks(count: usize) -> Vec<ScannedNetwork> {
        (0..count)
            .map(|i| ScannedNetwork {
                ssid: Some(format!("net{i}")),
                bssid: None,
                rssi: -50,
                channel: None,
                security: Security::Open,
            })
            .collect()
    }

    fn timings() -> ScanTimings {
        ScanTimings {
            queue: Duration::ZERO,
            corewlan: Duration::from_millis(900),
            postprocess: Duration::ZERO,
        }
    }

    fn scan_operations(worker: &FakeWorker) -> Vec<u64> {
        worker
            .commands()
            .into_iter()
            .filter_map(|c| match c {
                WorkerCommand::Scan { operation_id } => Some(operation_id),
                WorkerCommand::Request { .. } | WorkerCommand::VerifyJoin { .. } => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn ten_requests_cause_one_physical_scan() {
        let h = Harness::start();
        let mut client = h.connect().await;

        let mut ids = Vec::new();
        for _ in 0..10 {
            ids.push(client.send(Request::Scan).await);
        }
        // Each request is acknowledged with ScanStarted, so draining ten of
        // them proves the daemon has admitted all ten.
        for id in &ids {
            let ev = client.next().await;
            assert!(matches!(ev.event, Event::ScanStarted));
            assert_eq!(ev.request_id, Some(*id));
        }

        let ops = scan_operations(&h.worker);
        assert_eq!(ops.len(), 1, "ten requests must share one physical scan");

        h.hub.scan_finished(ops[0], Ok(networks(3)), timings());
        for id in &ids {
            let ev = client.next().await;
            assert_eq!(ev.request_id, Some(*id));
            match ev.event {
                Event::ScanResult(n) => assert_eq!(n.len(), 3),
                other => panic!("expected a scan result, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_fresh_result_is_served_without_touching_the_worker() {
        let h = Harness::start();
        let mut client = h.connect().await;

        client.send(Request::Scan).await;
        assert!(matches!(client.next().await.event, Event::ScanStarted));
        let ops = scan_operations(&h.worker);
        h.hub.scan_finished(ops[0], Ok(networks(2)), timings());
        assert!(matches!(client.next().await.event, Event::ScanResult(_)));

        let id = client.send(Request::Scan).await;
        let started = client.next().await;
        assert!(matches!(started.event, Event::ScanStarted));
        assert_eq!(started.request_id, Some(id));
        let result = client.next().await;
        assert_eq!(result.request_id, Some(id));
        match result.event {
            Event::ScanResult(n) => assert_eq!(n.len(), 2),
            other => panic!("expected the cached result, got {other:?}"),
        }
        assert!(
            scan_operations(&h.worker).is_empty(),
            "a cache hit must not reach the worker"
        );
    }

    #[tokio::test]
    async fn two_clients_scanning_at_once_get_their_own_ids() {
        let h = Harness::start();
        let mut one = h.connect().await;
        let mut two = h.connect().await;

        // Distinct id sequences would hide a mix-up, so make them collide:
        // both clients use request id 1.
        let id_one = one.send(Request::Scan).await;
        assert!(matches!(one.next().await.event, Event::ScanStarted));
        let id_two = two.send(Request::Scan).await;
        assert!(matches!(two.next().await.event, Event::ScanStarted));
        assert_eq!(id_one, id_two, "both clients used request id 1");

        let ops = scan_operations(&h.worker);
        assert_eq!(ops.len(), 1);
        h.hub.scan_finished(ops[0], Ok(networks(1)), timings());

        for client in [&mut one, &mut two] {
            let ev = client.next().await;
            assert_eq!(ev.request_id, Some(1));
            assert!(matches!(ev.event, Event::ScanResult(_)));
        }
    }

    #[tokio::test]
    async fn a_client_leaving_mid_scan_does_not_affect_the_others() {
        let h = Harness::start();
        let mut staying = h.connect().await;
        let mut leaving = h.connect().await;

        staying.send(Request::Scan).await;
        assert!(matches!(staying.next().await.event, Event::ScanStarted));
        leaving.send(Request::Scan).await;
        assert!(matches!(leaving.next().await.event, Event::ScanStarted));

        leaving.disconnect();
        drop(leaving);
        // Wait for the daemon's reader task to observe the EOF.
        for _ in 0..100 {
            if h.hub.clients.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(h.hub.clients.lock().unwrap().len(), 1);

        let ops = scan_operations(&h.worker);
        h.hub.scan_finished(ops[0], Ok(networks(4)), timings());
        match staying.next().await.event {
            Event::ScanResult(n) => assert_eq!(n.len(), 4),
            other => panic!("expected a scan result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failed_scan_releases_every_waiter() {
        let h = Harness::start();
        let mut client = h.connect().await;

        client.send(Request::Scan).await;
        assert!(matches!(client.next().await.event, Event::ScanStarted));
        let ops = scan_operations(&h.worker);
        h.hub
            .scan_finished(ops[0], Err("scan failed: radio off".into()), timings());
        assert!(matches!(client.next().await.event, Event::ScanFailed(_)));

        // In-flight state was cleared, so the next request starts a new scan
        // rather than waiting forever on the failed one.
        client.send(Request::Scan).await;
        assert!(matches!(client.next().await.event, Event::ScanStarted));
        assert_eq!(scan_operations(&h.worker).len(), 1);
    }

    #[tokio::test]
    async fn a_stale_protocol_version_is_rejected_readably() {
        let h = Harness::start();
        let stream = UnixStream::connect(&h.path).await.unwrap();
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let _: Hello = ipc::read_line(&mut reader).await.unwrap().unwrap();
        ipc::write_line(
            &mut writer,
            &Hello {
                version: ipc::PROTOCOL_VERSION - 1,
                build: "old".into(),
            },
        )
        .await
        .unwrap();

        // The daemon drops the connection; the client sees EOF rather than
        // hanging, and its own handshake check produces the actionable message.
        let next: Option<ServerEvent> =
            tokio::time::timeout(Duration::from_secs(5), ipc::read_line(&mut reader))
                .await
                .expect("daemon closed the connection promptly")
                .unwrap();
        assert!(next.is_none());
    }
}

#[cfg(test)]
mod verify_tests {
    use super::*;
    use crate::worker::{FakeWorker, JoinKind, VERIFY_INTERVAL};
    use std::time::Duration;

    fn hub() -> (Hub, FakeWorker) {
        let (wifi, worker) = FakeWorker::spawn();
        (
            Hub {
                clients: Arc::new(Mutex::new(HashMap::new())),
                coordinator: Arc::new(Mutex::new(ScanCoordinator::default())),
                wifi,
            },
            worker,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn a_pending_verification_does_not_hold_the_worker() {
        let (hub, worker) = hub();
        hub.schedule_verify(None, "Cafe".into(), JoinKind::Password, 7);

        // The wait happens on the runtime, not on the CoreWLAN thread: nothing
        // is enqueued while it elapses, so a scan arriving now would be next.
        tokio::time::sleep(VERIFY_INTERVAL / 2).await;
        assert!(worker.try_next().is_none());

        tokio::time::sleep(VERIFY_INTERVAL).await;
        match worker.try_next() {
            Some(WorkerCommand::VerifyJoin {
                ssid,
                attempts_remaining,
                ..
            }) => {
                assert_eq!(ssid, "Cafe");
                assert_eq!(attempts_remaining, 7);
            }
            other => panic!("expected a verification check, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_scan_during_verification_is_not_queued_behind_it() {
        let (hub, worker) = hub();
        hub.schedule_verify(
            None,
            "Cafe".into(),
            JoinKind::Saved {
                fail_detail: "cached password did not associate".into(),
            },
            19,
        );

        hub.scan_requested(ScanWaiter {
            client_id: 1,
            request_id: 1,
        });
        // The scan command is first in the queue despite the join being issued
        // first — the ten-second verification window is no longer ahead of it.
        assert!(matches!(
            worker.try_next(),
            Some(WorkerCommand::Scan { .. })
        ));

        tokio::time::sleep(VERIFY_INTERVAL + Duration::from_millis(1)).await;
        assert!(matches!(
            worker.try_next(),
            Some(WorkerCommand::VerifyJoin { .. })
        ));
    }
}
