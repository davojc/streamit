//! Hardware probe for a Blackmagic VideoHub.
//!
//!   probe <ip> dump                 print the raw dump the device sends, then the routes (1-based)
//!   probe <ip> route <out1> <in1>   route an output to an input (1-based) and print the result
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use streamit_core::devices::Router;
use streamit_videohub::{VideoHub, parse_blocks};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

const USAGE: &str = "usage: probe <ip> dump | probe <ip> route <out1> <in1>";

#[tokio::main]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn real_main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ip: IpAddr = args
        .first()
        .ok_or(USAGE)?
        .parse()
        .map_err(|e| format!("bad ip: {e}"))?;
    let addr = SocketAddr::new(ip, 9990);
    match args.get(1).map(String::as_str) {
        Some("dump") => dump(addr).await,
        Some("route") => {
            let n = |i: usize| -> Result<u16, String> {
                args.get(i)
                    .ok_or(USAGE)?
                    .parse()
                    .map_err(|_| USAGE.to_string())
            };
            route(addr, n(2)?, n(3)?).await
        }
        _ => Err(USAGE.into()),
    }
}

async fn dump(addr: SocketAddr) -> Result<(), String> {
    let mut s = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(addr))
        .await
        .map_err(|_| format!("timed out connecting to {addr}"))?
        .map_err(|e| format!("cannot connect to {addr}: {e}"))?;
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
        "--- raw dump (save as crates/streamit-videohub/tests/fixtures/videohub_dump.txt) ---"
    );
    print!("{text}");
    println!("--- routes (1-based: output <- input) ---");
    for b in parse_blocks(&text)
        .iter()
        .filter(|b| b.name == "VIDEO OUTPUT ROUTING")
    {
        for l in &b.lines {
            let mut it = l.split_whitespace().filter_map(|x| x.parse::<u16>().ok());
            if let (Some(o), Some(i)) = (it.next(), it.next()) {
                println!("output {} <- input {}", o + 1, i + 1);
            }
        }
    }
    Ok(())
}

async fn route(addr: SocketAddr, out: u16, inp: u16) -> Result<(), String> {
    let hub = VideoHub::connect(addr).await.map_err(|e| e.to_string())?;
    let mut now = None;
    for _ in 0..50 {
        if let Ok(v) = hub.route_of(out).await {
            now = Some(v);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let now =
        now.ok_or("no routing dump received (is the hub reachable, is the output number valid?)")?;
    println!("before: output {out} <- input {now}");
    hub.route(out, inp).await.map_err(|e| e.to_string())?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    println!(
        "after:  output {out} <- input {}",
        hub.route_of(out).await.map_err(|e| e.to_string())?
    );
    Ok(())
}
