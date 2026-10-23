//! Hardware probe for a Blackmagic Web Presenter 4K.
//!
//!   probe <ip> dump     print the raw state the device sends on connect, then the stream status
//!   probe <ip> status   print the current stream status
//!   probe <ip> start    start streaming (this really starts the encoder) and print the result
//!   probe <ip> stop     stop streaming and print the result
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use streamit_core::devices::Encoder;
use streamit_webpresenter::{WebPresenter, parse_blocks, parse_status};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

const USAGE: &str = "usage: probe <ip> dump|status|start|stop";

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
    let addr = SocketAddr::new(ip, 9977);
    match args.get(1).map(String::as_str) {
        Some("dump") => dump(addr).await,
        Some("status") => {
            let wp = connected(addr).await?;
            println!(
                "status: {:?}",
                wp.status().await.map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Some(cmd @ ("start" | "stop")) => {
            let wp = connected(addr).await?;
            println!(
                "before: {:?}",
                wp.status().await.map_err(|e| e.to_string())?
            );
            if cmd == "start" {
                wp.start().await
            } else {
                wp.stop().await
            }
            .map_err(|e| e.to_string())?;
            println!("{cmd}: ACK");
            // let the device report the new state
            tokio::time::sleep(Duration::from_secs(2)).await;
            println!(
                "after:  {:?}",
                wp.status().await.map_err(|e| e.to_string())?
            );
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

async fn connected(addr: SocketAddr) -> Result<std::sync::Arc<WebPresenter>, String> {
    let wp = WebPresenter::connect(addr)
        .await
        .map_err(|e| e.to_string())?;
    for _ in 0..50 {
        if wp.status().await.is_ok() {
            return Ok(wp);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("no STREAM STATE block received (is the device reachable on port 9977?)".into())
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
