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
use streamit_core::devices::{DeviceError, LabelKind, Result, Router};

const ROUTING: &str = "VIDEO OUTPUT ROUTING";
const INPUT_LABELS: &str = "INPUT LABELS";
const OUTPUT_LABELS: &str = "OUTPUT LABELS";
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

/// The wire form of a label change. `n` is 1-based; the protocol's is 0-based. A label is
/// one line, so line breaks become spaces.
pub fn format_label(kind: LabelKind, n: u16, text: &str) -> String {
    let block = match kind {
        LabelKind::Input => INPUT_LABELS,
        LabelKind::Output => OUTPUT_LABELS,
    };
    let text = text.replace(['\r', '\n'], " ");
    format!("{block}:\n{} {}\n\n", n.saturating_sub(1), text.trim())
}

/// Many label changes as one block (the VideoHub ACKs a block once).
pub fn format_labels(kind: LabelKind, labels: &[(u16, String)]) -> String {
    let block = match kind {
        LabelKind::Input => INPUT_LABELS,
        LabelKind::Output => OUTPUT_LABELS,
    };
    let mut s = format!("{block}:\n");
    for (n, text) in labels {
        let text = text.replace(['\r', '\n'], " ");
        s.push_str(&format!("{} {}\n", n.saturating_sub(1), text.trim()));
    }
    s.push('\n');
    s
}

/// Many routes as one block: (output, input), 1-based.
pub fn format_routes(routes: &[(u16, u16)]) -> String {
    let mut s = format!("{ROUTING}:\n");
    for &(o, i) in routes {
        s.push_str(&format!("{} {}\n", o.saturating_sub(1), i.saturating_sub(1)));
    }
    s.push('\n');
    s
}

pub struct VideoHub {
    /// 1-based output -> 1-based input.
    table: Arc<Mutex<HashMap<u16, u16>>>,
    labels: Arc<Mutex<Labels>>,
    link: Link,
}

/// The input and output labels (1-based) and how many of each the VideoHub says it has.
#[derive(Default)]
struct Labels {
    inputs: HashMap<u16, String>,
    outputs: HashMap<u16, String>,
    input_count: u16,
    output_count: u16,
}

impl Labels {
    fn map(&mut self, kind: LabelKind) -> &mut HashMap<u16, String> {
        match kind {
            LabelKind::Input => &mut self.inputs,
            LabelKind::Output => &mut self.outputs,
        }
    }

    /// One entry per input or output (as many as the VideoHub says it has, or up to the
    /// highest labelled one); "" where one has no label.
    fn list(&self, kind: LabelKind) -> Vec<String> {
        let (map, count) = match kind {
            LabelKind::Input => (&self.inputs, self.input_count),
            LabelKind::Output => (&self.outputs, self.output_count),
        };
        let count = map.keys().copied().max().unwrap_or(0).max(count);
        (1..=count).map(|i| map.get(&i).cloned().unwrap_or_default()).collect()
    }
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
            INPUT_LABELS => apply_labels(&l, LabelKind::Input, b),
            OUTPUT_LABELS => apply_labels(&l, LabelKind::Output, b),
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

    async fn input_labels(&self) -> Result<Vec<String>> {
        self.link.ensure_usable()?;
        Ok(self.labels.lock().unwrap_or_else(|e| e.into_inner()).list(LabelKind::Input))
    }

    async fn output_labels(&self) -> Result<Vec<String>> {
        self.link.ensure_usable()?;
        Ok(self.labels.lock().unwrap_or_else(|e| e.into_inner()).list(LabelKind::Output))
    }

    /// Sends the new label and keeps it once the VideoHub ACKs. (The VideoHub also sends the
    /// changed label block to every client, which says the same.)
    async fn set_label(&self, kind: LabelKind, n: u16, text: &str) -> Result<()> {
        if n == 0 {
            return Err(DeviceError::Other("inputs and outputs are numbered from 1".into()));
        }
        let what = format!("{kind:?} {n} label").to_lowercase();
        self.link.command(format_label(kind, n, text), &what).await?;
        let text = text.replace(['\r', '\n'], " ").trim().to_string();
        self.labels.lock().unwrap_or_else(|e| e.into_inner()).map(kind).insert(n, text);
        Ok(())
    }

    async fn routing_table(&self) -> Result<Vec<u16>> {
        self.link.ensure_usable()?;
        let t = self.table.lock().unwrap_or_else(|e| e.into_inner());
        let count = t.keys().copied().max().unwrap_or(0);
        Ok((1..=count).map(|o| t.get(&o).copied().unwrap_or(0)).collect())
    }

    async fn set_labels(&self, kind: LabelKind, labels: &[(u16, String)]) -> Result<()> {
        if labels.is_empty() {
            return Ok(());
        }
        let what = format!("{} {kind:?} labels", labels.len()).to_lowercase();
        self.link.command(format_labels(kind, labels), &what).await?;
        let mut l = self.labels.lock().unwrap_or_else(|e| e.into_inner());
        for (n, text) in labels {
            l.map(kind).insert(*n, text.replace(['\r', '\n'], " ").trim().to_string());
        }
        Ok(())
    }

    async fn route_many(&self, routes: &[(u16, u16)]) -> Result<()> {
        if routes.is_empty() {
            return Ok(());
        }
        if routes.iter().any(|&(o, i)| o == 0 || i == 0) {
            return Err(DeviceError::Other("outputs and inputs are numbered from 1".into()));
        }
        self.link.command(format_routes(routes), &format!("{} routes", routes.len())).await
    }
}

fn apply_labels(labels: &Mutex<Labels>, kind: LabelKind, block: &Block) {
    let mut l = labels.lock().unwrap_or_else(|e| e.into_inner());
    for line in &block.lines {
        let (n, text) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        if let Ok(n) = n.trim().parse::<u16>() {
            l.map(kind).insert(n.saturating_add(1), text.trim().to_string());
        }
    }
}

fn apply_device(labels: &Mutex<Labels>, block: &Block) {
    let mut l = labels.lock().unwrap_or_else(|e| e.into_inner());
    for line in &block.lines {
        let count = |prefix: &str| line.strip_prefix(prefix).and_then(|n| n.trim().parse::<u16>().ok());
        if let Some(n) = count("Video inputs:") {
            l.input_count = n;
        }
        if let Some(n) = count("Video outputs:") {
            l.output_count = n;
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
