//! Unified event stream feeding the TUI main loop.
//!
//! `EventHandler` owns one tokio task that fans crossterm input and a tick
//! timer into the same channel the Wi-Fi worker writes to. The main loop
//! only needs to `recv()` on a single receiver.

use anyhow::Result;
use std::time::Duration;

use crossterm::event::{Event as CtEvent, KeyEvent};
use futures::{FutureExt, StreamExt};
use tokio::sync::mpsc;

use crate::corewlan::{InterfaceState, ScannedNetwork};

#[derive(Debug, Clone)]
pub enum Event {
    Tick,
    Key(KeyEvent),
    Resize(u16, u16),
    State(InterfaceState),
    ScanStarted,
    ScanResult(Vec<ScannedNetwork>),
    PreferredResult(Vec<String>),
    Notice(String),
    Error(String),
    ShareReady(SharePayload),
    JoinSavedFailed { ssid: String, reason: String },
}

#[derive(Debug, Clone)]
pub struct SharePayload {
    pub ssid: String,
    pub uri: String,
    pub has_password: bool,
}

pub struct EventHandler {
    pub tx: mpsc::UnboundedSender<Event>,
    pub rx: mpsc::UnboundedReceiver<Event>,
}

impl EventHandler {
    pub fn new(tick_ms: u64) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let tx_for_task = tx.clone();
        tokio::spawn(async move {
            let mut reader = crossterm::event::EventStream::new();
            let mut tick = tokio::time::interval(Duration::from_millis(tick_ms));
            loop {
                let tick_delay = tick.tick();
                let next = reader.next().fuse();
                tokio::select! {
                    () = tx_for_task.closed() => break,
                    _ = tick_delay => {
                        if tx_for_task.send(Event::Tick).is_err() { break; }
                    }
                    Some(Ok(evt)) = next => match evt {
                        CtEvent::Key(k) if k.kind == crossterm::event::KeyEventKind::Press => {
                            if tx_for_task.send(Event::Key(k)).is_err() { break; }
                        }
                        CtEvent::Resize(w, h) => {
                            if tx_for_task.send(Event::Resize(w, h)).is_err() { break; }
                        }
                        _ => {}
                    }
                }
            }
        });
        Self { tx, rx }
    }

    pub async fn next(&mut self) -> Result<Event> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("event channel closed"))
    }
}
