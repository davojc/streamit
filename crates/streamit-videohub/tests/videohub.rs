use std::net::SocketAddr;
use std::time::Duration;
use streamit_core::devices::{DeviceError, Router};
use streamit_videohub::{Timings, VideoHub, format_route, parse_blocks};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const DUMP: &str = "PROTOCOL PREAMBLE:\nVersion: 2.8\n\nVIDEOHUB DEVICE:\nDevice present: true\n\nVIDEO OUTPUT ROUTING:\n10 12\n0 0\n\n";

// TODO(capture): written from the Blackmagic protocol spec; replace with a real capture
// from the church VideoHub (`probe <ip> dump`).
const FIXTURE: &str = include_str!("fixtures/videohub_dump.txt");

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

async fn wait_route(hub: &VideoHub, out: u16, want: u16) {
    for _ in 0..100 {
        if hub.route_of(out).await == Ok(want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("route_of({out}) never became {want}: {:?}", hub.route_of(out).await);
}

#[test]
fn parses_routing_dump() {
    let blocks = parse_blocks(DUMP);
    let names: Vec<_> = blocks.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["PROTOCOL PREAMBLE", "VIDEOHUB DEVICE", "VIDEO OUTPUT ROUTING"]);
    assert_eq!(blocks[2].lines, ["10 12", "0 0"]);
}

#[test]
fn parses_crlf_and_bare_acks() {
    let blocks = parse_blocks("ACK\r\n\r\nNAK\n\nVIDEO OUTPUT ROUTING:\r\n1 2\r\n\r\n");
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0].name, "ACK");
    assert_eq!(blocks[1].name, "NAK");
    assert_eq!(blocks[2].lines, ["1 2"]);
}

#[test]
fn parses_captured_fixture() {
    let blocks = parse_blocks(FIXTURE);
    assert_eq!(blocks.len(), 6);
    let routing = blocks.iter().find(|b| b.name == "VIDEO OUTPUT ROUTING").unwrap();
    assert_eq!(routing.lines, ["0 0", "1 1", "10 7", "39 39"]);
}

#[test]
fn format_route_is_zero_based() {
    assert_eq!(format_route(11, 13), "VIDEO OUTPUT ROUTING:\n10 12\n\n");
}

#[tokio::test]
async fn dump_fills_table_one_based() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let _s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;
    assert_eq!(hub.route_of(1).await, Ok(1));
    assert_eq!(hub.route_of(30).await, Err(DeviceError::Other("unknown output".into())));
}

#[tokio::test]
async fn unsolicited_routing_block_updates_table() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;
    s.write_all(b"VIDEO OUTPUT ROUTING:\r\n10 3\r\n\r\n").await.unwrap();
    wait_route(&hub, 11, 4).await;
}

#[tokio::test]
async fn route_waits_for_ack_and_errors_on_nak() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;

    let h = hub.clone();
    let t = tokio::spawn(async move { h.route(11, 5).await });
    assert_eq!(read_msg(&mut s).await, "VIDEO OUTPUT ROUTING:\n10 4\n\n");
    s.write_all(b"ACK\n\nVIDEO OUTPUT ROUTING:\n10 4\n\n").await.unwrap();
    assert_eq!(t.await.unwrap(), Ok(()));
    wait_route(&hub, 11, 5).await;

    let h = hub.clone();
    let t = tokio::spawn(async move { h.route(11, 6).await });
    read_msg(&mut s).await;
    s.write_all(b"NAK\n\n").await.unwrap();
    assert!(matches!(t.await.unwrap(), Err(DeviceError::Refused(_))));
    assert_eq!(hub.route_of(11).await, Ok(5));
}

#[tokio::test]
async fn route_times_out_without_reply() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;
    let h = hub.clone();
    let t = tokio::spawn(async move { h.route(11, 5).await });
    read_msg(&mut s).await;
    assert!(matches!(t.await.unwrap(), Err(DeviceError::Timeout(_))));
}

#[tokio::test]
async fn reports_disconnected_when_peer_closes_then_reconnects() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;
    drop(s);
    let mut down = false;
    for _ in 0..100 {
        if matches!(hub.route_of(11).await, Err(DeviceError::Disconnected(_))) {
            down = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(down, "never reported Disconnected");
    assert!(matches!(hub.route(11, 2).await, Err(DeviceError::Disconnected(_))));
    let _s2 = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;
}

#[tokio::test]
async fn silent_peer_is_declared_dead_by_ping() {
    let (l, addr) = fake().await;
    let mut t = fast();
    t.ping_interval = Duration::from_millis(50);
    let hub = VideoHub::connect_with(addr, t).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;
    assert_eq!(read_msg(&mut s).await, "PING:\n\n");
    // never ACK: the hub must drop the link and reconnect
    let _s2 = tokio::time::timeout(Duration::from_secs(3), l.accept()).await.expect("no reconnect").unwrap();
}

#[tokio::test]
async fn answered_pings_keep_link_up() {
    let (l, addr) = fake().await;
    let mut t = fast();
    t.ping_interval = Duration::from_millis(50);
    let hub = VideoHub::connect_with(addr, t).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;
    for _ in 0..4 {
        assert_eq!(read_msg(&mut s).await, "PING:\n\n");
        s.write_all(b"ACK\n\n").await.unwrap();
    }
    assert_eq!(hub.route_of(11).await, Ok(13));
}

#[tokio::test]
async fn timeout_then_nak_never_reports_ok() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;

    // Route A: the device reads it but never replies.
    let h = hub.clone();
    let a = tokio::spawn(async move { h.route(11, 5).await });
    read_msg(&mut s).await;
    assert!(matches!(a.await.unwrap(), Err(DeviceError::Timeout(_))));

    // The hub must drop the link and reconnect, discarding the skewed queue.
    let mut s2 =
        tokio::time::timeout(Duration::from_secs(3), accept_with_dump(&l)).await.expect("no reconnect after timeout");
    wait_route(&hub, 11, 13).await;

    // Route B is refused; nothing from the old link may leak into it.
    let h = hub.clone();
    let b = tokio::spawn(async move { h.route(11, 6).await });
    read_msg(&mut s2).await;
    s2.write_all(b"NAK\n\n").await.unwrap();
    assert!(matches!(b.await.unwrap(), Err(DeviceError::Refused(_))));
    assert_eq!(hub.route_of(11).await, Ok(13));
}

#[tokio::test]
async fn route_of_before_dump_is_disconnected() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let (mut s, _) = l.accept().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(hub.route_of(11).await, Err(DeviceError::Disconnected("not synced".into())));
    assert!(matches!(hub.route(11, 2).await, Err(DeviceError::Disconnected(_))));
    s.write_all(DUMP.as_bytes()).await.unwrap();
    wait_route(&hub, 11, 13).await;
}

#[tokio::test]
async fn reconnect_replaces_table_with_new_dump() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;
    assert_eq!(hub.route_of(1).await, Ok(1));
    drop(s);
    let (mut s2, _) = l.accept().await.unwrap();
    s2.write_all(b"VIDEO OUTPUT ROUTING:\n10 2\n\n").await.unwrap();
    wait_route(&hub, 11, 3).await;
    assert_eq!(hub.route_of(1).await, Err(DeviceError::Other("unknown output".into())));
}

#[tokio::test]
async fn non_utf8_label_does_not_break_link() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let mut s = accept_with_dump(&l).await;
    wait_route(&hub, 11, 13).await;
    s.write_all(b"INPUT LABELS:\n0 Caf\xe9 Chapel\n\nVIDEO OUTPUT ROUTING:\n10 5\n\n").await.unwrap();
    wait_route(&hub, 11, 6).await; // same connection: no reconnect happened
}

#[tokio::test]
async fn input_labels_are_one_based_and_cover_every_input() {
    let (l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    let (mut s, _) = l.accept().await.unwrap();
    s.write_all(FIXTURE.as_bytes()).await.unwrap();
    wait_route(&hub, 11, 8).await;
    let labels = hub.input_labels().await.unwrap();
    assert_eq!(labels.len(), 40, "Video inputs: 40");
    assert_eq!(labels[0], "Camera 1");
    assert_eq!(labels[7], "Camera 8");
    assert_eq!(labels[11], "DeckLink");
    assert_eq!(labels[1], "", "no label");
    // A label changed on the VideoHub arrives as its own block.
    s.write_all(
        b"INPUT LABELS:
1 Pulpit camera

",
    )
    .await
    .unwrap();
    for _ in 0..100 {
        if hub.input_labels().await.unwrap()[1] == "Pulpit camera" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the new label never arrived");
}

#[tokio::test]
async fn input_labels_before_the_dump_are_disconnected() {
    let (_l, addr) = fake().await;
    let hub = VideoHub::connect_with(addr, fast()).await.unwrap();
    assert!(matches!(hub.input_labels().await, Err(DeviceError::Disconnected(_))));
}
