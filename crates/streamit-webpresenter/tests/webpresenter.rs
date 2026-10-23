use std::net::SocketAddr;
use std::time::Duration;
use streamit_core::devices::{DeviceError, Encoder, EncoderStatus};
use streamit_webpresenter::{Timings, WebPresenter, format_action, parse_blocks, parse_status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const DUMP: &str = "PROTOCOL PREAMBLE:\nVersion: 1.0\n\nSTREAM STATE:\nStatus: Idle\n\n";

// TODO(capture): written from Companion's bmd-webpresenter module; replace with a real capture
// from the church Web Presenter (`probe <ip> dump`).
const FIXTURE: &str = include_str!("fixtures/webpresenter_dump.txt");

fn fast() -> Timings {
    Timings {
        backoff: vec![Duration::from_millis(20), Duration::from_millis(40)],
        ping_interval: Duration::from_secs(60),
        ping_timeout: Duration::from_millis(200),
        command_timeout: Duration::from_millis(300),
        connect_timeout: Duration::from_millis(500),
    }
}

async fn fake() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    (l, a)
}

async fn accept_with_dump(l: &TcpListener) -> TcpStream {
    let (mut s, _) = l.accept().await.unwrap();
    s.write_all(DUMP.as_bytes()).await.unwrap();
    s
}

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

async fn wait_status(wp: &WebPresenter, want: EncoderStatus) {
    for _ in 0..100 {
        if wp.status().await == Ok(want.clone()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("status never became {want:?}: {:?}", wp.status().await);
}

#[test]
fn parses_stream_state_status() {
    let blocks = parse_blocks("STREAM STATE:\r\nStatus: Streaming\r\n\r\n");
    assert_eq!(parse_status(&blocks[0]), Some(EncoderStatus::Streaming));
    for (text, want) in [
        ("Idle", EncoderStatus::Idle),
        ("Connecting", EncoderStatus::Connecting),
        ("Interrupted", EncoderStatus::Interrupted),
        ("Stopping", EncoderStatus::Unknown("Stopping".into())),
    ] {
        let b = parse_blocks(&format!("STREAM STATE:\nStatus: {text}\n\n"));
        assert_eq!(parse_status(&b[0]), Some(want));
    }
    // other blocks, and a STREAM STATE without a Status line, carry no status
    assert_eq!(
        parse_status(&parse_blocks("IDENTITY:\nStatus: Idle\n\n")[0]),
        None
    );
    assert_eq!(
        parse_status(&parse_blocks("STREAM STATE:\nAction: Start\n\n")[0]),
        None
    );
}

#[test]
fn formats_start_and_stop() {
    assert_eq!(format_action(true), "STREAM STATE:\nAction: Start\n\n");
    assert_eq!(format_action(false), "STREAM STATE:\nAction: Stop\n\n");
}

#[test]
fn parses_captured_fixture() {
    let blocks = parse_blocks(FIXTURE);
    let st = blocks.iter().find(|b| b.name == "STREAM STATE").unwrap();
    assert_eq!(parse_status(st), Some(EncoderStatus::Idle));
}

#[tokio::test]
async fn status_follows_dump_and_unsolicited_updates() {
    let (l, addr) = fake().await;
    let wp = WebPresenter::connect_with(addr, fast()).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_status(&wp, EncoderStatus::Idle).await;
    s.write_all(b"STREAM STATE:\r\nStatus: Streaming\r\n\r\n")
        .await
        .unwrap();
    wait_status(&wp, EncoderStatus::Streaming).await;
}

#[tokio::test]
async fn start_and_stop_wait_for_ack() {
    let (l, addr) = fake().await;
    let wp = WebPresenter::connect_with(addr, fast()).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_status(&wp, EncoderStatus::Idle).await;

    let w = wp.clone();
    let t = tokio::spawn(async move { w.start().await });
    assert_eq!(read_msg(&mut s).await, "STREAM STATE:\nAction: Start\n\n");
    s.write_all(b"ACK\n\nSTREAM STATE:\nStatus: Connecting\n\n")
        .await
        .unwrap();
    assert_eq!(t.await.unwrap(), Ok(()));
    wait_status(&wp, EncoderStatus::Connecting).await;

    let w = wp.clone();
    let t = tokio::spawn(async move { w.stop().await });
    assert_eq!(read_msg(&mut s).await, "STREAM STATE:\nAction: Stop\n\n");
    s.write_all(b"ACK\n\n").await.unwrap();
    assert_eq!(t.await.unwrap(), Ok(()));
}

#[tokio::test]
async fn start_errors_on_nak() {
    let (l, addr) = fake().await;
    let wp = WebPresenter::connect_with(addr, fast()).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_status(&wp, EncoderStatus::Idle).await;
    let w = wp.clone();
    let t = tokio::spawn(async move { w.start().await });
    read_msg(&mut s).await;
    s.write_all(b"NAK\n\n").await.unwrap();
    assert!(matches!(t.await.unwrap(), Err(DeviceError::Refused(_))));
    assert_eq!(wp.status().await, Ok(EncoderStatus::Idle));
}

#[tokio::test]
async fn start_times_out_then_resyncs() {
    let (l, addr) = fake().await;
    let wp = WebPresenter::connect_with(addr, fast()).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_status(&wp, EncoderStatus::Idle).await;
    let w = wp.clone();
    let t = tokio::spawn(async move { w.start().await });
    read_msg(&mut s).await;
    assert!(matches!(t.await.unwrap(), Err(DeviceError::Timeout(_))));
    // the link is dropped and re-established; a stale reply cannot leak into the next command
    let mut s2 = tokio::time::timeout(Duration::from_secs(3), accept_with_dump(&l))
        .await
        .expect("no reconnect after timeout");
    wait_status(&wp, EncoderStatus::Idle).await;
    let w = wp.clone();
    let t = tokio::spawn(async move { w.stop().await });
    read_msg(&mut s2).await;
    s2.write_all(b"NAK\n\n").await.unwrap();
    assert!(matches!(t.await.unwrap(), Err(DeviceError::Refused(_))));
}

#[tokio::test]
async fn disconnected_before_state_block_and_when_peer_closes() {
    let (l, addr) = fake().await;
    let wp = WebPresenter::connect_with(addr, fast()).await.unwrap();
    let (mut s, _) = l.accept().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        wp.status().await,
        Err(DeviceError::Disconnected("not synced".into()))
    );
    assert!(matches!(
        wp.start().await,
        Err(DeviceError::Disconnected(_))
    ));
    s.write_all(DUMP.as_bytes()).await.unwrap();
    wait_status(&wp, EncoderStatus::Idle).await;
    drop(s);
    let mut down = false;
    for _ in 0..100 {
        if matches!(wp.status().await, Err(DeviceError::Disconnected(_))) {
            down = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(down, "never reported Disconnected");
    assert!(matches!(wp.stop().await, Err(DeviceError::Disconnected(_))));
    let _s2 = accept_with_dump(&l).await;
    wait_status(&wp, EncoderStatus::Idle).await;
}
