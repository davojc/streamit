//! Hardware probe for a Blackmagic VideoHub, shared by the `videohub-probe` example and
//! `streamit probe videohub`.
//!
//!   <ip> dump                 print the raw dump the device sends, then the routes (1-based)
//!   <ip> route <out1> <in1>   route an output to an input (1-based) and print the result
use crate::{VideoHub, parse_blocks};
use anyhow::{Context, anyhow, bail};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use streamit_core::devices::Router;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

pub const USAGE: &str = "usage: videohub <ip> dump | videohub <ip> route <out1> <in1>";

/// Runs the probe; `args` are everything after the program (or `probe videohub`) name.
pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let ip: IpAddr = args.first().ok_or_else(|| anyhow!(USAGE))?.parse().context("bad ip")?;
    let addr = SocketAddr::new(ip, 9990);
    match args.get(1).map(String::as_str) {
        Some("dump") => dump(addr).await,
        Some("route") => {
            let n = |i: usize| -> anyhow::Result<u16> {
                args.get(i).ok_or_else(|| anyhow!(USAGE))?.parse().map_err(|_| anyhow!(USAGE))
            };
            route(addr, n(2)?, n(3)?).await
        }
        _ => bail!(USAGE),
    }
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
    println!("--- raw dump (save as crates/streamit-videohub/tests/fixtures/videohub_dump.txt) ---");
    print!("{text}");
    println!("--- routes (1-based: output <- input) ---");
    for b in parse_blocks(&text).iter().filter(|b| b.name == "VIDEO OUTPUT ROUTING") {
        for l in &b.lines {
            let mut it = l.split_whitespace().filter_map(|x| x.parse::<u16>().ok());
            if let (Some(o), Some(i)) = (it.next(), it.next()) {
                println!("output {} <- input {}", o + 1, i + 1);
            }
        }
    }
    Ok(())
}

async fn route(addr: SocketAddr, out: u16, inp: u16) -> anyhow::Result<()> {
    let hub = VideoHub::connect(addr).await?;
    let mut now = None;
    for _ in 0..50 {
        if let Ok(v) = hub.route_of(out).await {
            now = Some(v);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let now = now.ok_or_else(|| {
        anyhow!("no routing dump received (is the hub reachable, is the output number valid?)")
    })?;
    println!("before: output {out} <- input {now}");
    hub.route(out, inp).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    println!("after:  output {out} <- input {}", hub.route_of(out).await?);
    Ok(())
}
