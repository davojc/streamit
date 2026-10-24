use std::time::Duration;
use streamit_core::devices::{DeviceError, Lighting};
use streamit_eos::{Arg, Deframer, EosClient, Framing, Message, Timings, decode, encode, frame};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn fast() -> Timings {
    Timings {
        ping_every: Duration::from_millis(50),
        silent_after: Duration::from_millis(400),
        reconnect_after: Duration::from_millis(30),
        connect_timeout: Duration::from_millis(300),
    }
}

/// Reads messages from StreamIt until one at `address` arrives.
async fn expect(s: &mut TcpStream, d: &mut Deframer, address: &str) -> Message {
    let mut buf = [0u8; 1024];
    let mut seen = Vec::new();
    loop {
        let n = tokio::time::timeout(Duration::from_secs(2), s.read(&mut buf))
            .await
            .unwrap_or_else(|_| panic!("{address} never came; saw {seen:?}"))
            .unwrap();
        assert!(n > 0, "StreamIt closed the link");
        for p in d.push(&buf[..n]) {
            let m = decode(&p).unwrap();
            if m.address == address {
                return m;
            }
            seen.push(m.address);
        }
    }
}

async fn say(s: &mut TcpStream, m: Message) {
    s.write_all(&frame(&encode(&m), Framing::Slip)).await.unwrap();
}

#[tokio::test]
async fn subscribes_reads_the_active_cue_and_fires_cues() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let eos = EosClient::connect_with(l.local_addr().unwrap(), Framing::Slip, fast());
    let (mut s, _) = l.accept().await.unwrap();
    let mut d = Deframer::new(Framing::Slip);
    let sub = expect(&mut s, &mut d, "/eos/subscribe").await;
    assert_eq!(sub.args, vec![Arg::Int(1)]);
    say(&mut s, Message::new("/eos/out/show/name", vec![Arg::Str("Sunday".into())])).await;
    say(&mut s, Message::new("/eos/out/active/cue/text", vec![Arg::Str("1/5 Sermon 3.0 100%".into())])).await;
    let mut status = None;
    for _ in 0..100 {
        if let Ok(st) = eos.status().await
            && st.active_cue.is_some()
        {
            status = Some(st);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let status = status.expect("the console's status arrived");
    assert_eq!(status.show.as_deref(), Some("Sunday"));
    assert_eq!(status.active_cue.as_deref(), Some("1/5 Sermon 3.0 100%"));

    eos.fire_cue(1, "7.5").await.unwrap();
    let fired = expect(&mut s, &mut d, "/eos/cue/1/7.5/fire").await;
    assert!(fired.args.is_empty());
}

#[tokio::test]
async fn pings_and_goes_down_when_the_console_is_silent() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let eos = EosClient::connect_with(l.local_addr().unwrap(), Framing::Slip, fast());
    let (mut s, _) = l.accept().await.unwrap();
    let mut d = Deframer::new(Framing::Slip);
    expect(&mut s, &mut d, "/eos/ping").await;
    // The console never answers: after `silent_after` StreamIt reports it as not answering.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(matches!(eos.status().await, Err(DeviceError::Timeout(_) | DeviceError::Disconnected(_))));
    assert!(eos.fire_cue(1, "1").await.is_err(), "no firing into silence");
}

#[tokio::test]
async fn not_connected_is_disconnected_and_bad_cues_are_refused() {
    // Nothing listens on this port.
    let addr = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
    let eos = EosClient::connect_with(addr, Framing::Slip, fast());
    assert!(matches!(eos.status().await, Err(DeviceError::Disconnected(_))));
    assert!(matches!(eos.fire_cue(0, "1").await, Err(DeviceError::Other(_))));
    assert!(matches!(eos.fire_cue(1, " ").await, Err(DeviceError::Other(_))));
}

#[tokio::test]
async fn length_framing_works_too() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _eos = EosClient::connect_with(l.local_addr().unwrap(), Framing::Length, fast());
    let (mut s, _) = l.accept().await.unwrap();
    let mut d = Deframer::new(Framing::Length);
    expect(&mut s, &mut d, "/eos/subscribe").await;
}
