//! Wire protocol shared between daemon and client.
//!
//! Newline-delimited JSON over a per-user unix socket. Each side sends a
//! `Hello` line first to confirm protocol version. Subsequent lines are
//! `Request` (client → daemon) or `Event` (daemon → client).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
}

/// Default location for the daemon's accept socket. Per-user, mode 0600. The
/// daemon ensures the parent directory exists before binding.
pub fn socket_path() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    base.join("macwifi").join("daemon.sock")
}

pub async fn write_line<W: AsyncWriteExt + Unpin, T: Serialize>(
    w: &mut W,
    v: &T,
) -> Result<()> {
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
