//! Blackmagic Web Presenter 4K client (Ethernet protocol, TCP 9977).
//!
//! Built on `streamit-blockproto` (same block protocol as the VideoHub). Block shapes come
//! from Companion's bmd-webpresenter module and must be confirmed with the probe capture:
//! `STREAM STATE:\nAction: Start|Stop\n\n` to command, `STREAM STATE:\nStatus: <state>\n\n`
//! to report, `ACK`/`NAK` as the reply.
pub mod probe;

use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
pub use streamit_blockproto::{Block, Timings, parse_blocks};
use streamit_blockproto::{Link, OnBlock, OnReset};
use streamit_core::devices::{Encoder, EncoderStatus, Result};

const STREAM_STATE: &str = "STREAM STATE";

/// The wire form of a start/stop command.
pub fn format_action(start: bool) -> String {
    format!("{STREAM_STATE}:\nAction: {}\n\n", if start { "Start" } else { "Stop" })
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

const STREAM_SETTINGS: &str = "STREAM SETTINGS";

/// A streaming setting StreamIt never keeps or sends: the stream key (anyone with it can
/// stream to the channel), and the lists of choices the device offers.
pub fn is_private_or_list(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n == "key" || n.contains("stream key") || n.starts_with("available")
}

/// The settings in a `STREAM SETTINGS` block, without the key or lists.
pub fn parse_stream_settings(b: &Block) -> Vec<(String, String)> {
    b.lines
        .iter()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, _)| !is_private_or_list(k))
        .collect()
}

/// The wire form of putting streaming settings back. The key is never sent.
pub fn format_stream_settings(settings: &[(String, String)]) -> String {
    let mut s = format!("{STREAM_SETTINGS}:\n");
    for (k, v) in settings.iter().filter(|(k, _)| !is_private_or_list(k)) {
        s.push_str(&format!("{}: {}\n", k.trim(), v.replace(['\r', '\n'], " ").trim()));
    }
    s.push('\n');
    s
}

pub struct WebPresenter {
    status: Arc<Mutex<Option<EncoderStatus>>>,
    settings: Arc<Mutex<Vec<(String, String)>>>,
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
        let settings: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let (s, set) = (status.clone(), settings.clone());
        let on_block: OnBlock = Arc::new(move |b: &Block| {
            if let Some(st) = parse_status(b) {
                *s.lock().unwrap_or_else(|e| e.into_inner()) = Some(st);
            }
            if b.name == STREAM_SETTINGS {
                // The device sends what changed: merge it into what we have.
                let mut have = set.lock().unwrap_or_else(|e| e.into_inner());
                for (k, v) in parse_stream_settings(b) {
                    match have.iter_mut().find(|(hk, _)| *hk == k) {
                        Some(slot) => slot.1 = v,
                        None => have.push((k, v)),
                    }
                }
            }
        });
        let (s, set) = (status.clone(), settings.clone());
        let on_reset: OnReset = Arc::new(move || {
            *s.lock().unwrap_or_else(|e| e.into_inner()) = None;
            set.lock().unwrap_or_else(|e| e.into_inner()).clear();
        });
        // Synced once the first STREAM STATE block has arrived.
        let link = Link::spawn("webpresenter", STREAM_STATE, addr, timings, on_block, on_reset);
        Ok(Arc::new(WebPresenter { status, settings, link }))
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
        let st = self.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        // Synced means a STREAM STATE block arrived, but it may have lacked a Status line.
        Ok(st.unwrap_or_else(|| EncoderStatus::Unknown("no status reported".into())))
    }

    async fn stream_settings(&self) -> Result<Vec<(String, String)>> {
        self.link.ensure_usable()?;
        Ok(self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    async fn set_stream_settings(&self, settings: &[(String, String)]) -> Result<()> {
        let keep: Vec<(String, String)> = settings.iter().filter(|(k, _)| !is_private_or_list(k)).cloned().collect();
        if keep.is_empty() {
            return Ok(());
        }
        self.link.command(format_stream_settings(&keep), "stream settings").await
    }
}
