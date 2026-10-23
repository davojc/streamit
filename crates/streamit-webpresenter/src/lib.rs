//! Blackmagic Web Presenter 4K client (Ethernet protocol, TCP 9977).
//!
//! Built on `streamit-blockproto` (same block protocol as the VideoHub). Block shapes come
//! from Companion's bmd-webpresenter module and must be confirmed with the probe capture:
//! `STREAM STATE:\nAction: Start|Stop\n\n` to command, `STREAM STATE:\nStatus: <state>\n\n`
//! to report, `ACK`/`NAK` as the reply.
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
pub use streamit_blockproto::{Block, Timings, parse_blocks};
use streamit_blockproto::{Link, OnBlock, OnReset};
use streamit_core::devices::{Encoder, EncoderStatus, Result};

const STREAM_STATE: &str = "STREAM STATE";

/// The wire form of a start/stop command.
pub fn format_action(start: bool) -> String {
    format!(
        "{STREAM_STATE}:\nAction: {}\n\n",
        if start { "Start" } else { "Stop" }
    )
}

/// The status carried by a `STREAM STATE` block, if any. Unknown values are kept verbatim.
pub fn parse_status(b: &Block) -> Option<EncoderStatus> {
    if b.name != STREAM_STATE {
        return None;
    }
    let value = b.lines.iter().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("status").then(|| v.trim())
    })?;
    Some(match value.to_ascii_lowercase().as_str() {
        "idle" => EncoderStatus::Idle,
        "connecting" => EncoderStatus::Connecting,
        "streaming" => EncoderStatus::Streaming,
        "interrupted" => EncoderStatus::Interrupted,
        _ => EncoderStatus::Unknown(value.to_string()),
    })
}

pub struct WebPresenter {
    status: Arc<Mutex<Option<EncoderStatus>>>,
    link: Link,
}

impl WebPresenter {
    /// Starts the client with production timings. It does not wait for the link: while the
    /// device is unreachable every call reports `Disconnected`, and it keeps retrying.
    pub async fn connect(addr: SocketAddr) -> Result<Arc<WebPresenter>> {
        Self::connect_with(addr, Timings::default()).await
    }

    pub async fn connect_with(addr: SocketAddr, timings: Timings) -> Result<Arc<WebPresenter>> {
        let status: Arc<Mutex<Option<EncoderStatus>>> = Arc::default();
        let s = status.clone();
        let on_block: OnBlock = Arc::new(move |b: &Block| {
            if let Some(st) = parse_status(b) {
                *s.lock().unwrap_or_else(|e| e.into_inner()) = Some(st);
            }
        });
        let s = status.clone();
        let on_reset: OnReset = Arc::new(move || {
            *s.lock().unwrap_or_else(|e| e.into_inner()) = None;
        });
        // Synced once the first STREAM STATE block has arrived.
        let link = Link::spawn(
            "webpresenter",
            STREAM_STATE,
            addr,
            timings,
            on_block,
            on_reset,
        );
        Ok(Arc::new(WebPresenter { status, link }))
    }
}

#[async_trait]
impl Encoder for WebPresenter {
    async fn start(&self) -> Result<()> {
        self.link.command(format_action(true), "stream start").await
    }

    async fn stop(&self) -> Result<()> {
        self.link.command(format_action(false), "stream stop").await
    }

    async fn status(&self) -> Result<EncoderStatus> {
        self.link.ensure_usable()?;
        let st = self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // Synced means a STREAM STATE block arrived, but it may have lacked a Status line.
        Ok(st.unwrap_or_else(|| EncoderStatus::Unknown("no status reported".into())))
    }
}
