use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use streamit_core::devices::DeviceError;
use streamit_videohub::Timings;
use streamit_videohub::multiview::MultiView;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// Written from the Blackmagic MultiView manual (Videohub Ethernet Protocol section): the
// solo source is routing output 4 (0-based), the audio source output 5.
const DUMP: &str = "PROTOCOL PREAMBLE:\nVersion: 2.3\n\nINPUT LABELS:\n0 Cam 1\n1 Cam 2\n2 Cam 3\n3 Cam 4\n\n\
VIDEO OUTPUT ROUTING:\n4 1\n5 0\n\nCONFIGURATION:\nLayout: 2x2\nSolo enabled: False\n\n";

fn fast() -> Timings {
    Timings {
        backoff: vec![Duration::from_millis(20), Duration::from_millis(40)],
        ping_interval: Duration::from_secs(60),
        ping_timeout: Duration::from_millis(200),
        command_timeout: Duration::from_millis(300),
        connect_timeout: Duration::from_millis(500),
    }
}

async fn connected() -> (Arc<MultiView>, TcpStream) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = l.local_addr().unwrap();
    let mv = MultiView::connect_with(addr, fast());
    let (mut s, _) = l.accept().await.unwrap();
    s.write_all(DUMP.as_bytes()).await.unwrap();
    for _ in 0..100 {
        if mv.ensure_usable().is_ok() {
            return (mv, s);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the MultiView client never synced");
}

/// Reads until a blank line ("\n\n") and returns the text.
async fn read_msg(s: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    loop {
        let mut b = [0u8; 1];
        s.read_exact(&mut b).await.unwrap();
        buf.push(b[0]);
        if buf.ends_with(b"\n\n") {
            return String::from_utf8(buf).unwrap();
        }
    }
}

async fn wait_solo(mv: &MultiView, want: Option<u16>) {
    for _ in 0..100 {
        if mv.solo() == Ok(want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("solo never became {want:?}: {:?}", mv.solo());
}

#[tokio::test]
async fn reads_solo_from_the_dump_and_updates() {
    let (mv, mut s) = connected().await;
    assert_eq!(mv.solo(), Ok(None), "solo off: all four");
    s.write_all(b"CONFIGURATION:\nSolo enabled: True\n\n").await.unwrap();
    wait_solo(&mv, Some(2)).await;
    s.write_all(b"VIDEO OUTPUT ROUTING:\n4 3\n\n").await.unwrap();
    wait_solo(&mv, Some(4)).await;
}

#[tokio::test]
async fn full_screen_routes_the_solo_source_then_turns_solo_on() {
    let (mv, mut s) = connected().await;
    let m = mv.clone();
    let t = tokio::spawn(async move { m.set_solo(Some(3)).await });
    assert_eq!(read_msg(&mut s).await, "VIDEO OUTPUT ROUTING:\n4 2\n\n");
    s.write_all(b"ACK\n\nVIDEO OUTPUT ROUTING:\n4 2\n\n").await.unwrap();
    assert_eq!(read_msg(&mut s).await, "CONFIGURATION:\nSolo enabled: true\n\n");
    s.write_all(b"ACK\n\nCONFIGURATION:\nSolo enabled: True\n\n").await.unwrap();
    assert_eq!(t.await.unwrap(), Ok(()));
    wait_solo(&mv, Some(3)).await;

    let m = mv.clone();
    let t = tokio::spawn(async move { m.set_solo(None).await });
    assert_eq!(read_msg(&mut s).await, "CONFIGURATION:\nSolo enabled: false\n\n");
    s.write_all(b"ACK\n\nCONFIGURATION:\nSolo enabled: False\n\n").await.unwrap();
    assert_eq!(t.await.unwrap(), Ok(()));
    wait_solo(&mv, None).await;
}

#[tokio::test]
async fn a_refused_full_screen_is_an_error() {
    let (mv, mut s) = connected().await;
    let m = mv.clone();
    let t = tokio::spawn(async move { m.set_solo(Some(1)).await });
    read_msg(&mut s).await;
    s.write_all(b"NAK\n\n").await.unwrap();
    assert!(matches!(t.await.unwrap(), Err(DeviceError::Refused(_))));
    assert_eq!(mv.solo(), Ok(None));
}
