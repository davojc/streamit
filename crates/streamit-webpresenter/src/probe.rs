//! Hardware probe for a Blackmagic Web Presenter 4K, shared by the `webpresenter-probe`
//! example and `streamit probe webpresenter`.
//!
//!   <ip> dump     print the raw state the device sends on connect, then the stream status
//!   <ip> status   print the current stream status
//!   <ip> start    start streaming (this really starts the encoder) and print the result
//!   <ip> stop     stop streaming and print the result
use crate::{WebPresenter, parse_blocks, parse_status};
use anyhow::{Context, anyhow, bail};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use streamit_core::devices::Encoder;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

pub const USAGE: &str = "usage: webpresenter <ip> dump|status|start|stop";

/// Runs the probe; `args` are everything after the program (or `probe webpresenter`) name.
pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let ip: IpAddr = args.first().ok_or_else(|| anyhow!(USAGE))?.parse().context("bad ip")?;
    let addr = SocketAddr::new(ip, 9977);
    match args.get(1).map(String::as_str) {
        Some("dump") => dump(addr).await,
        Some("status") => {
            let wp = connected(addr).await?;
            println!("status: {:?}", wp.status().await?);
            Ok(())
        }
        Some(cmd @ ("start" | "stop")) => {
            let wp = connected(addr).await?;
            println!("before: {:?}", wp.status().await?);
            if cmd == "start" { wp.start().await } else { wp.stop().await }?;
            println!("{cmd}: ACK");
            // let the device report the new state
            tokio::time::sleep(Duration::from_secs(2)).await;
            println!("after:  {:?}", wp.status().await?);
            Ok(())
        }
        _ => bail!(USAGE),
    }
}

async fn connected(addr: SocketAddr) -> anyhow::Result<Arc<WebPresenter>> {
    let wp = WebPresenter::connect(addr).await?;
    for _ in 0..50 {
        if wp.status().await.is_ok() {
            return Ok(wp);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("no STREAM STATE block received (is the device reachable on port 9977?)")
}

async fn dump(addr: SocketAddr) -> anyhow::Result<()> {
    let mut s = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow!("timed out connecting to {addr}"))?
        .with_context(|| format!("cannot connect to {addr}"))?;
    // The device sends its whole state on connect; collect for 2 s of quiet.
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(2), s.read(&mut buf)).await {
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&raw);
    println!(
        "--- raw dump (save as crates/streamit-webpresenter/tests/fixtures/webpresenter_dump.txt) ---"
    );
    print!("{text}");
    println!("--- parsed status ---");
    for b in parse_blocks(&text) {
        if let Some(st) = parse_status(&b) {
            println!("{st:?}");
        }
    }
    Ok(())
}
