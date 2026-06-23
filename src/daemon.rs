//! Daemon server: owns the CoreWLAN worker and accepts client connections
//! over a unix socket.
//!
//! Launched by a `LaunchAgent` so its parent is `launchd` and TCC gives it
//! the bundle's Location grant. Without that the whole point is moot — see
//! `install.rs` for the plist that wires this up. The wrapper script uses
//! `/usr/bin/open -W -a /Applications/macwifi.app --args daemon` so the
//! Aqua app session is set up (verified empirically — direct launchd-exec
//! gets blank SSIDs even with the Location grant).

use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::event::Event;
use crate::ipc::{self, Hello, Reader, Writer};
use crate::worker::{LocalWifiHandle, Request};

pub async fn run() -> Result<()> {
    let path = ipc::socket_path();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create {}", parent.display()))?;
    }
    // Remove stale socket from a prior run.
    let _ = tokio::fs::remove_file(&path).await;
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))?;
    eprintln!("macwifi-daemon listening at {}", path.display());

    // Fire Location prompt + start the CFRunLoop pump. Runs forever on a
    // background thread; we need it both for TCC and for CoreWLAN's
    // "process behaves like a GUI app" check. This is done *after* binding the
    // socket: on a first-ever launch it can block up to 30s waiting for the
    // user to answer the TCC prompt, and doing it before the bind would make
    // the TUI's 2s connect retry give up with "daemon unreachable".
    crate::location::request_when_in_use();

    let (worker_tx, mut worker_rx) = mpsc::unbounded_channel::<Event>();
    let wifi = LocalWifiHandle::spawn(worker_tx);

    let fanout: Arc<Mutex<Vec<UnboundedSender<Event>>>> = Arc::new(Mutex::new(Vec::new()));
    let fanout_task = fanout.clone();
    tokio::spawn(async move {
        while let Some(ev) = worker_rx.recv().await {
            let mut senders = fanout_task.lock().unwrap();
            senders.retain(|tx| tx.send(ev.clone()).is_ok());
        }
    });

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("accept unix connection")?;
        let wifi = wifi.clone();
        let fanout = fanout.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_one(stream, wifi, fanout).await {
                eprintln!("client task ended: {e:#}");
            }
        });
    }
}

async fn serve_one(
    stream: UnixStream,
    wifi: LocalWifiHandle,
    fanout: Arc<Mutex<Vec<UnboundedSender<Event>>>>,
) -> Result<()> {
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

    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<Event>();
    fanout.lock().unwrap().push(client_tx);

    // Writer task: drain client_rx → socket.
    let writer_task = tokio::spawn(async move {
        while let Some(ev) = client_rx.recv().await {
            if let Err(e) = ipc::write_line(&mut writer, &ev).await {
                eprintln!("daemon writer: {e:#}");
                break;
            }
        }
        let _ = writer.shutdown().await;
    });

    // Reader task (this task): parse requests → wifi.send.
    loop {
        let req: Option<Request> = ipc::read_line(&mut reader).await?;
        match req {
            Some(r) => wifi.send(r),
            None => break,
        }
    }

    writer_task.abort();
    Ok(())
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
