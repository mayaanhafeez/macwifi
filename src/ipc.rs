//! Wire protocol shared between daemon and client.
//!
//! Newline-delimited JSON over a per-user unix socket. Each side sends a
//! `Hello` line first to confirm protocol version. Subsequent lines are
//! `ClientRequest` (client → daemon) or `ServerEvent` (daemon → client).
//!
//! Both directions are wrapped in a correlation envelope rather than adding an
//! id to every `Request`/`Event` variant. With several clients on one daemon —
//! a TUI plus any number of CLI one-shots — an uncorrelated reply stream means
//! one client's scan result can satisfy another client's request.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::event::Event;
use crate::worker::Request;

pub const PROTOCOL_VERSION: u32 = 3;

/// A request plus the id the client will match its reply against. Ids are
/// per-connection and monotonic; the daemon never interprets them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRequest {
    pub id: u64,
    pub request: Request,
}

/// An event plus the request it answers. `None` means unsolicited: a global
/// state change caused by someone else, or a daemon-wide notice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEvent {
    pub request_id: Option<u64>,
    pub event: Event,
}

impl ServerEvent {
    /// An event no client asked for.
    pub fn unsolicited(event: Event) -> Self {
        Self {
            request_id: None,
            event,
        }
    }
}

/// Build identifier baked in at compile time (see `build.rs`): short git hash +
/// build timestamp. Compared across the handshake so a stale daemon left running
/// after a rebuild can be surfaced to the user.
pub const BUILD_ID: &str = env!("MACWIFI_BUILD_ID");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    #[serde(default)]
    pub build: String,
}

/// Default location for the daemon's accept socket. Per-user, mode 0600. The
/// daemon ensures the parent directory exists before binding.
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("MACWIFI_SOCKET_PATH") {
        return PathBuf::from(path);
    }
    let base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    base.join("macwifi").join("daemon.sock")
}

pub async fn write_line<W: AsyncWriteExt + Unpin, T: Serialize>(w: &mut W, v: &T) -> Result<()> {
    let mut buf = serde_json::to_vec(v).context("serialize ipc message")?;
    buf.push(b'\n');
    w.write_all(&buf).await.context("socket write")?;
    w.flush().await.ok();
    Ok(())
}

/// Read one newline-terminated JSON line. Returns `Ok(None)` on clean EOF.
pub async fn read_line<R: AsyncBufReadExt + Unpin, T: DeserializeOwned>(
    r: &mut R,
) -> Result<Option<T>> {
    let mut line = String::new();
    let n = r.read_line(&mut line).await.context("socket read")?;
    if n == 0 {
        return Ok(None);
    }
    let v = serde_json::from_str(line.trim_end()).context("parse ipc message")?;
    Ok(Some(v))
}

pub type Reader = BufReader<OwnedReadHalf>;
pub type Writer = OwnedWriteHalf;
