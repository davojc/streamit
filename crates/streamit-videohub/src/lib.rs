//! Blackmagic VideoHub client (Ethernet protocol, TCP 9990).
//!
//! The protocol is 0-based; this crate's public API (and the `Router` trait and the config)
//! is 1-based, as the device front panel and Companion number things. Conversion happens
//! only at the protocol boundary, in `format_route` and when reading routing blocks.
pub mod probe;

use async_trait::async_trait;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use streamit_blockproto::{Block, Link, OnBlock, OnReset};
pub use streamit_blockproto::{Timings, parse_blocks};
use streamit_core::devices::{DeviceError, Result, Router};

const ROUTING: &str = "VIDEO OUTPUT ROUTING";
const LABELS: &str = "INPUT LABELS";
const DEVICE: &str = "VIDEOHUB DEVICE";

/// The wire form of a route command. Arguments are 1-based; the output is 0-based.
pub fn format_route(out1: u16, in1: u16) -> String {
    format!(
        "{ROUTING}:
{} {}

",
        out1.saturating_sub(1),
        in1.saturating_sub(1)
    )
}

pub struct VideoHub {
    /// 1-based output -> 1-based input.
    table: Arc<Mutex<HashMap<u16, u16>>>,
    labels: Arc<Mutex<Labels>>,
    link: Link,
}

/// The input labels (1-based) and how many inputs the VideoHub says it has.
#[derive(Default)]
struct Labels {
    by_input: HashMap<u16, String>,
    inputs: u16,
}

impl VideoHub {
    /// Starts the client with production timings. It does not wait for the link: while the
    /// device is unreachable every call reports `Disconnected`, and it keeps retrying.
    pub async fn connect(addr: SocketAddr) -> Result<Arc<VideoHub>> {
        Self::connect_with(addr, Timings::default()).await
    }

    pub async fn connect_with(addr: SocketAddr, timings: Timings) -> Result<Arc<VideoHub>> {
        let table: Arc<Mutex<HashMap<u16, u16>>> = Arc::default();
        let labels: Arc<Mutex<Labels>> = Arc::default();
        let (t, l) = (table.clone(), labels.clone());
        let on_block: OnBlock = Arc::new(move |b: &Block| match b.name.as_str() {
            ROUTING => apply_routing(&t, b),
            LABELS => apply_labels(&l, b),
            DEVICE => apply_device(&l, b),
            _ => {}
        });
        let (t, l) = (table.clone(), labels.clone());
        let on_reset: OnReset = Arc::new(move || {
            t.lock().unwrap_or_else(|e| e.into_inner()).clear();
            *l.lock().unwrap_or_else(|e| e.into_inner()) = Labels::default();
        });
        // Synced once the first VIDEO OUTPUT ROUTING block has arrived.
        let link = Link::spawn("videohub", ROUTING, addr, timings, on_block, on_reset);
        Ok(Arc::new(VideoHub { table, labels, link }))
    }
}

#[async_trait]
impl Router for VideoHub {
    async fn route_of(&self, output: u16) -> Result<u16> {
        self.link.ensure_usable()?;
        self.table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&output)
            .copied()
            .ok_or_else(|| DeviceError::Other("unknown output".into()))
    }

    async fn route(&self, output: u16, input: u16) -> Result<()> {
        if output == 0 || input == 0 {
            return Err(DeviceError::Other("outputs and inputs are numbered from 1".into()));
        }
        self.link.command(format_route(output, input), &format!("route {output} <- {input}")).await
    }

    /// One entry per input (as many as the VideoHub says it has, or up to the highest
    /// labelled one); "" where an input has no label.
    async fn input_labels(&self) -> Result<Vec<String>> {
        self.link.ensure_usable()?;
        let l = self.labels.lock().unwrap_or_else(|e| e.into_inner());
        let count = l.by_input.keys().copied().max().unwrap_or(0).max(l.inputs);
        Ok((1..=count).map(|i| l.by_input.get(&i).cloned().unwrap_or_default()).collect())
    }
}

fn apply_labels(labels: &Mutex<Labels>, block: &Block) {
    let mut l = labels.lock().unwrap_or_else(|e| e.into_inner());
    for line in &block.lines {
        let (n, text) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        if let Ok(n) = n.trim().parse::<u16>() {
            l.by_input.insert(n.saturating_add(1), text.trim().to_string());
        }
    }
}

fn apply_device(labels: &Mutex<Labels>, block: &Block) {
    for line in &block.lines {
        if let Some(n) = line.strip_prefix("Video inputs:").and_then(|n| n.trim().parse::<u16>().ok()) {
            labels.lock().unwrap_or_else(|e| e.into_inner()).inputs = n;
        }
    }
}

fn apply_routing(table: &Mutex<HashMap<u16, u16>>, block: &Block) {
    let mut t = table.lock().unwrap_or_else(|e| e.into_inner());
    for line in &block.lines {
        let mut it = line.split_whitespace().map(str::parse::<u16>);
        if let (Some(Ok(o)), Some(Ok(i))) = (it.next(), it.next()) {
            t.insert(o.saturating_add(1), i.saturating_add(1));
        }
    }
}
