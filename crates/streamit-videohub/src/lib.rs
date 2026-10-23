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
    link: Link,
}

impl VideoHub {
    /// Starts the client with production timings. It does not wait for the link: while the
    /// device is unreachable every call reports `Disconnected`, and it keeps retrying.
    pub async fn connect(addr: SocketAddr) -> Result<Arc<VideoHub>> {
        Self::connect_with(addr, Timings::default()).await
    }

    pub async fn connect_with(addr: SocketAddr, timings: Timings) -> Result<Arc<VideoHub>> {
        let table: Arc<Mutex<HashMap<u16, u16>>> = Arc::default();
        let t = table.clone();
        let on_block: OnBlock = Arc::new(move |b: &Block| {
            if b.name == ROUTING {
                apply_routing(&t, b);
            }
        });
        let t = table.clone();
        let on_reset: OnReset = Arc::new(move || {
            t.lock().unwrap_or_else(|e| e.into_inner()).clear();
        });
        // Synced once the first VIDEO OUTPUT ROUTING block has arrived.
        let link = Link::spawn("videohub", ROUTING, addr, timings, on_block, on_reset);
        Ok(Arc::new(VideoHub { table, link }))
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
            return Err(DeviceError::Other(
                "outputs and inputs are numbered from 1".into(),
            ));
        }
        self.link
            .command(
                format_route(output, input),
                &format!("route {output} <- {input}"),
            )
            .await
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
