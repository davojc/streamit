//! ATEM UDP session layer: packet header, hello handshake, acks, resends, ping and reconnect.
//!
//! Ported from atem-connection (MIT, github.com/nrkno/sofie-atem-connection, `main` at
//! 9c77b69919d10108c92231903023acf4cf1cf7db), `src/lib/atemSocketChild.ts`. Timings follow the
//! StreamIt brief rather than upstream: resend after 100 ms (upstream 60 ms), at most 10 resends,
//! ping after 500 ms of silence (upstream sends none), link lost after 3 s (upstream 5 s).
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::time::Duration;
use streamit_core::devices::{DeviceError, Result};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

pub const ATEM_PORT: u16 = 9910;
pub const HEADER_LEN: usize = 12;
/// Packet ids are 15 bits; the ATEM wraps at 1 << 15.
pub const MAX_PACKET_ID: u32 = 1 << 15;
/// The length field is 11 bits.
pub const MAX_PACKET_LEN: usize = 0x07ff;
/// How far ahead of sequence an inbound packet may be and still be buffered.
const MAX_OUT_OF_ORDER_PACKETS: u32 = 256;

/// Header flags (the top five bits of byte 0). Upstream: `PacketFlag` in atemSocketChild.ts.
pub mod flag {
    pub const ACK_REQUEST: u8 = 0x01;
    pub const NEW_SESSION_ID: u8 = 0x02;
    pub const IS_RETRANSMIT: u8 = 0x04;
    pub const RETRANSMIT_REQUEST: u8 = 0x08;
    pub const ACK_REPLY: u8 = 0x10;
}

/// Upstream: `COMMAND_CONNECT_HELLO` in src/lib/atemSocketChild.ts.
pub const COMMAND_CONNECT_HELLO: [u8; 20] = [
    0x10, 0x14, 0x53, 0xab, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3a, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// The 12-byte packet header. Bytes 8-9 (upstream: "client packet id") are not used and are
/// written as zero, as upstream does for every packet except the hello.
///
/// Upstream layout (src/lib/atemSocketChild.ts, `sendPacket`, `_sendAck`, `_receivePacket`):
/// u16 BE `flags << 11 | length`, u16 session id, u16 ack id, u16 retransmit-from id,
/// u16 unused, u16 packet id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    pub flags: u8,
    pub length: u16,
    pub session_id: u16,
    pub ack_id: u16,
    pub retransmit_from: u16,
    pub packet_id: u16,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        let word = (u16::from(self.flags) << 11) | (self.length & MAX_PACKET_LEN as u16);
        b[0..2].copy_from_slice(&word.to_be_bytes());
        b[2..4].copy_from_slice(&self.session_id.to_be_bytes());
        b[4..6].copy_from_slice(&self.ack_id.to_be_bytes());
        b[6..8].copy_from_slice(&self.retransmit_from.to_be_bytes());
        b[10..12].copy_from_slice(&self.packet_id.to_be_bytes());
        b
    }

    pub fn parse(b: &[u8]) -> Option<Header> {
        if b.len() < HEADER_LEN {
            return None;
        }
        let u16_at = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
        Some(Header {
            flags: b[0] >> 3,
            length: u16_at(0) & MAX_PACKET_LEN as u16,
            session_id: u16_at(2),
            ack_id: u16_at(4),
            retransmit_from: u16_at(6),
            packet_id: u16_at(10),
        })
    }
}

/// Upstream `_isPacketCoveredByAck`: whether an ack of `ack` also acknowledges `pkt`.
pub fn covered_by_ack(ack: u16, pkt: u16) -> bool {
    let (ack, pkt) = (u32::from(ack), u32::from(pkt));
    let tolerance = MAX_PACKET_ID / 2;
    let shortly_before = pkt < ack && pkt + tolerance > ack;
    let shortly_after = pkt > ack && pkt < ack + tolerance;
    let before_wrap = pkt > ack + tolerance;
    pkt == ack || ((shortly_before || before_wrap) && !shortly_after)
}

fn ack_packet(session_id: u16, id: u16) -> Vec<u8> {
    // Upstream `_sendAck`.
    Header {
        flags: flag::ACK_REPLY,
        length: HEADER_LEN as u16,
        session_id,
        ack_id: id,
        ..Header::default()
    }
    .encode()
    .to_vec()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    pub resend_after: Duration,
    pub max_resends: u32,
    pub ping_after: Duration,
    pub lost_after: Duration,
    pub hello_retry: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            resend_after: Duration::from_millis(100),
            max_resends: 10,
            ping_after: Duration::from_millis(500),
            lost_after: Duration::from_secs(3),
            // Upstream CONNECTION_RETRY_INTERVAL.
            hello_retry: Duration::from_secs(1),
        }
    }
}

/// What the session asks its driver to do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Send this datagram.
    Send(Vec<u8>),
    /// The established link was lost: recreate the socket before the next send.
    Reset,
    /// A session was established (hello answered).
    Connected,
    /// The established session was lost; a new hello follows.
    Lost,
    /// The payload of an in-sequence reliable packet (one or more commands).
    Commands(Vec<u8>),
    /// The packet carrying this token was acked.
    Acked(u64),
    /// The packet carrying this token will never be acked.
    Failed(u64, DeviceError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conn {
    SynSent,
    Established,
}

#[derive(Debug)]
struct InFlight {
    id: u16,
    token: Option<u64>,
    bytes: Vec<u8>,
    last_sent: Instant,
    resent: u32,
}

/// The protocol state machine, free of I/O so it can be tested directly.
#[derive(Debug)]
pub struct Session {
    t: Timings,
    conn: Conn,
    session_id: u16,
    next_send_id: u16,
    last_rx_id: u16,
    out_of_order: BTreeMap<u16, Vec<u8>>,
    in_flight: Vec<InFlight>,
    last_rx_at: Instant,
    hello_at: Instant,
    last_ping_at: Instant,
}

impl Session {
    pub fn new(t: Timings, now: Instant) -> Session {
        Session {
            t,
            conn: Conn::SynSent,
            session_id: 0,
            next_send_id: 1,
            last_rx_id: 0,
            out_of_order: BTreeMap::new(),
            in_flight: Vec::new(),
            last_rx_at: now,
            hello_at: now,
            last_ping_at: now,
        }
    }

    pub fn is_established(&self) -> bool {
        self.conn == Conn::Established
    }

    /// Sends the hello that opens a session.
    pub fn start(&mut self, now: Instant, out: &mut Vec<Output>) {
        self.restart(
            now,
            out,
            DeviceError::Disconnected("atem link restarted".into()),
        );
    }

    /// Upstream `restartConnection`: drop the session and say hello again.
    fn restart(&mut self, now: Instant, out: &mut Vec<Output>, fail_with: DeviceError) {
        if self.conn == Conn::Established {
            out.push(Output::Lost);
            out.push(Output::Reset);
        }
        for p in self.in_flight.drain(..) {
            if let Some(t) = p.token {
                out.push(Output::Failed(t, fail_with.clone()));
            }
        }
        self.conn = Conn::SynSent;
        self.session_id = 0;
        self.next_send_id = 1;
        self.last_rx_id = 0;
        self.out_of_order.clear();
        self.hello_at = now;
        out.push(Output::Send(COMMAND_CONNECT_HELLO.to_vec()));
    }

    /// Upstream `_receivePacket`.
    pub fn handle_datagram(&mut self, dg: &[u8], now: Instant, out: &mut Vec<Output>) {
        let Some(h) = Header::parse(dg) else { return };
        if usize::from(h.length) != dg.len() {
            return;
        }
        self.last_rx_at = now;
        self.session_id = h.session_id;

        if h.flags & flag::NEW_SESSION_ID != 0 {
            if self.conn != Conn::Established {
                out.push(Output::Connected);
            }
            self.conn = Conn::Established;
            self.last_rx_id = h.packet_id;
            self.out_of_order.clear();
            self.last_ping_at = now;
            out.push(Output::Send(ack_packet(self.session_id, h.packet_id)));
            return;
        }
        if self.conn != Conn::Established {
            return;
        }

        if h.flags & flag::RETRANSMIT_REQUEST != 0 {
            let from = (u32::from(h.retransmit_from) % MAX_PACKET_ID) as u16;
            self.retransmit_from(from, now, out);
            if self.conn != Conn::Established {
                return;
            }
        }

        if h.flags & flag::ACK_REQUEST != 0 && u32::from(h.packet_id) < MAX_PACKET_ID {
            let id = h.packet_id;
            let distance =
                (u32::from(id) + MAX_PACKET_ID - u32::from(self.last_rx_id)) % MAX_PACKET_ID;
            if distance == 1 {
                self.last_rx_id = id;
                if dg.len() > HEADER_LEN {
                    out.push(Output::Commands(dg[HEADER_LEN..].to_vec()));
                }
                // Deliver any buffered packets which are now in sequence.
                loop {
                    let next = ((u32::from(self.last_rx_id) + 1) % MAX_PACKET_ID) as u16;
                    let Some(p) = self.out_of_order.remove(&next) else {
                        break;
                    };
                    self.last_rx_id = next;
                    if !p.is_empty() {
                        out.push(Output::Commands(p));
                    }
                }
                out.push(Output::Send(ack_packet(self.session_id, self.last_rx_id)));
            } else if distance > 1 && distance <= MAX_OUT_OF_ORDER_PACKETS {
                self.out_of_order
                    .entry(id)
                    .or_insert_with(|| dg[HEADER_LEN..].to_vec());
            } else if covered_by_ack(self.last_rx_id, id) {
                // A retransmit of something already acked: ack again.
                out.push(Output::Send(ack_packet(self.session_id, self.last_rx_id)));
            }
        }

        if h.flags & flag::ACK_REPLY != 0 {
            let ack = h.ack_id;
            let mut kept = Vec::with_capacity(self.in_flight.len());
            for p in self.in_flight.drain(..) {
                if covered_by_ack(ack, p.id) {
                    if let Some(t) = p.token {
                        out.push(Output::Acked(t));
                    }
                } else {
                    kept.push(p);
                }
            }
            self.in_flight = kept;
        }
    }

    /// Upstream `_retransmitFrom`.
    fn retransmit_from(&mut self, from: u16, now: Instant, out: &mut Vec<Output>) {
        let Some(idx) = self.in_flight.iter().position(|p| p.id == from) else {
            tracing::debug!("atem asked for packet {from}, which is not in flight; restarting");
            self.restart(
                now,
                out,
                DeviceError::Disconnected("atem link restarted".into()),
            );
            return;
        };
        for p in &mut self.in_flight[idx..] {
            if p.id == from || !covered_by_ack(from, p.id) {
                p.last_sent = now;
                p.resent += 1;
                out.push(Output::Send(p.bytes.clone()));
            }
        }
    }

    /// Sends `payload` in a reliable (ack-requested) packet. `token` comes back in `Acked` or `Failed`.
    /// Upstream `sendPacket`.
    pub fn send_reliable(
        &mut self,
        payload: &[u8],
        token: Option<u64>,
        now: Instant,
        out: &mut Vec<Output>,
    ) {
        if self.conn != Conn::Established {
            if let Some(t) = token {
                out.push(Output::Failed(
                    t,
                    DeviceError::Disconnected("atem not connected".into()),
                ));
            }
            return;
        }
        if HEADER_LEN + payload.len() > MAX_PACKET_LEN {
            if let Some(t) = token {
                out.push(Output::Failed(
                    t,
                    DeviceError::Other("atem packet too long".into()),
                ));
            }
            return;
        }
        let id = self.next_send_id;
        self.next_send_id = ((u32::from(id) + 1) % MAX_PACKET_ID) as u16;
        let mut bytes = Header {
            flags: flag::ACK_REQUEST,
            length: (HEADER_LEN + payload.len()) as u16,
            session_id: self.session_id,
            packet_id: id,
            ..Header::default()
        }
        .encode()
        .to_vec();
        bytes.extend_from_slice(payload);
        out.push(Output::Send(bytes.clone()));
        self.in_flight.push(InFlight {
            id,
            token,
            bytes,
            last_sent: now,
            resent: 0,
        });
    }

    /// Runs the timers: resends, pings, hello retries and the lost-link watchdog.
    pub fn poll(&mut self, now: Instant, out: &mut Vec<Output>) {
        if self.conn != Conn::Established {
            if now >= self.hello_at + self.t.hello_retry {
                self.restart(
                    now,
                    out,
                    DeviceError::Disconnected("atem not connected".into()),
                );
            }
            return;
        }
        if now >= self.last_rx_at + self.t.lost_after {
            tracing::warn!("atem silent for {:?}; reconnecting", self.t.lost_after);
            self.restart(now, out, DeviceError::Disconnected("atem link lost".into()));
            return;
        }
        // Resends, oldest first (upstream `_checkForRetransmit`, but each packet on its own clock).
        let mut i = 0;
        while i < self.in_flight.len() {
            let p = &mut self.in_flight[i];
            if now < p.last_sent + self.t.resend_after {
                i += 1;
                continue;
            }
            if p.resent < self.t.max_resends {
                p.resent += 1;
                p.last_sent = now;
                out.push(Output::Send(p.bytes.clone()));
                i += 1;
            } else if p.token.is_none() {
                // An unanswered ping: the silence watchdog decides when the link is lost.
                self.in_flight.remove(i);
            } else {
                // Upstream resets the connection so the stream cannot stay stuck behind the gap.
                tracing::warn!("atem never acked packet {}; reconnecting", p.id);
                self.restart(
                    now,
                    out,
                    DeviceError::Timeout("atem did not ack the command".into()),
                );
                return;
            }
        }
        if now >= self.ping_due() {
            self.last_ping_at = now;
            self.send_reliable(&[], None, now, out);
        }
    }

    fn ping_due(&self) -> Instant {
        self.last_rx_at.max(self.last_ping_at) + self.t.ping_after
    }

    /// When `poll` next has work to do.
    pub fn next_deadline(&self) -> Instant {
        if self.conn != Conn::Established {
            return self.hello_at + self.t.hello_retry;
        }
        let mut d = (self.last_rx_at + self.t.lost_after).min(self.ping_due());
        for p in &self.in_flight {
            d = d.min(p.last_sent + self.t.resend_after);
        }
        d
    }
}

/// A datagram socket connected to one ATEM. A trait so tests can run on a fake.
#[async_trait]
pub trait Transport: Send + Sync + 'static {
    async fn send(&self, dg: &[u8]) -> std::io::Result<()>;
    async fn recv(&self) -> std::io::Result<Vec<u8>>;
    /// Replaces the socket with a fresh one (new local port), as upstream does on reconnect.
    async fn reset(&mut self) -> std::io::Result<()>;
}

/// Something the driver reports to its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    Connected,
    Lost,
    Commands(Vec<u8>),
}

/// A request to send one packet's payload reliably; `done` resolves when it is acked.
pub struct SendRequest {
    pub payload: Vec<u8>,
    pub done: oneshot::Sender<Result<()>>,
}

/// Drives a `Session` over a `Transport` until `requests` closes.
pub async fn run_link<T: Transport>(
    mut transport: T,
    timings: Timings,
    mut requests: mpsc::Receiver<SendRequest>,
    mut on_event: impl FnMut(LinkEvent) + Send,
) {
    let mut session = Session::new(timings, Instant::now());
    let mut pending: std::collections::HashMap<u64, oneshot::Sender<Result<()>>> =
        std::collections::HashMap::new();
    let mut next_token: u64 = 0;
    let mut out = Vec::new();
    session.start(Instant::now(), &mut out);
    loop {
        for o in out.drain(..) {
            match o {
                Output::Send(dg) => {
                    if let Err(e) = transport.send(&dg).await {
                        tracing::debug!("atem send failed: {e}");
                    }
                }
                Output::Reset => {
                    if let Err(e) = transport.reset().await {
                        tracing::warn!("atem socket reset failed: {e}");
                    }
                }
                Output::Connected => on_event(LinkEvent::Connected),
                Output::Lost => on_event(LinkEvent::Lost),
                Output::Commands(p) => on_event(LinkEvent::Commands(p)),
                Output::Acked(t) => {
                    if let Some(tx) = pending.remove(&t) {
                        let _ = tx.send(Ok(()));
                    }
                }
                Output::Failed(t, e) => {
                    if let Some(tx) = pending.remove(&t) {
                        let _ = tx.send(Err(e));
                    }
                }
            }
        }
        let deadline = session.next_deadline();
        tokio::select! {
            biased;
            r = transport.recv() => match r {
                Ok(dg) => session.handle_datagram(&dg, Instant::now(), &mut out),
                // Windows reports ICMP port-unreachable as a recv error on UDP; keep going.
                Err(e) => tracing::debug!("atem recv failed: {e}"),
            },
            req = requests.recv() => match req {
                None => return,
                Some(SendRequest { payload, done }) => {
                    let token = next_token;
                    next_token += 1;
                    pending.insert(token, done);
                    session.send_reliable(&payload, Some(token), Instant::now(), &mut out);
                }
            },
            _ = tokio::time::sleep_until(deadline) => session.poll(Instant::now(), &mut out),
        }
    }
}

/// The production transport: a UDP socket connected to the ATEM.
pub struct UdpTransport {
    remote: std::net::SocketAddr,
    socket: tokio::net::UdpSocket,
}

impl UdpTransport {
    pub async fn bind(remote: std::net::SocketAddr) -> std::io::Result<UdpTransport> {
        Ok(UdpTransport {
            remote,
            socket: Self::open(remote).await?,
        })
    }

    async fn open(remote: std::net::SocketAddr) -> std::io::Result<tokio::net::UdpSocket> {
        let local: std::net::SocketAddr = if remote.is_ipv4() {
            (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
        } else {
            (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
        };
        let s = tokio::net::UdpSocket::bind(local).await?;
        s.connect(remote).await?;
        Ok(s)
    }
}

#[async_trait]
impl Transport for UdpTransport {
    async fn send(&self, dg: &[u8]) -> std::io::Result<()> {
        self.socket.send(dg).await.map(|_| ())
    }
    async fn recv(&self) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; 2048];
        let n = self.socket.recv(&mut buf).await?;
        buf.truncate(n);
        Ok(buf)
    }
    async fn reset(&mut self) -> std::io::Result<()> {
        self.socket = Self::open(self.remote).await?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use tokio::sync::Mutex;

    /// The client side of an in-memory datagram pair.
    pub struct FakeTransport {
        inbound: Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
        outbound: mpsc::UnboundedSender<Vec<u8>>,
        pub resets: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    /// The device side: what the client sent, and a way to send to it.
    pub struct FakeAtem {
        pub to_client: mpsc::UnboundedSender<Vec<u8>>,
        pub from_client: mpsc::UnboundedReceiver<Vec<u8>>,
        pub resets: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    pub fn pair() -> (FakeTransport, FakeAtem) {
        let (to_client, inbound) = mpsc::unbounded_channel();
        let (outbound, from_client) = mpsc::unbounded_channel();
        let resets = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (
            FakeTransport {
                inbound: Mutex::new(inbound),
                outbound,
                resets: resets.clone(),
            },
            FakeAtem {
                to_client,
                from_client,
                resets,
            },
        )
    }

    #[async_trait]
    impl Transport for FakeTransport {
        async fn send(&self, dg: &[u8]) -> std::io::Result<()> {
            let _ = self.outbound.send(dg.to_vec());
            Ok(())
        }
        async fn recv(&self) -> std::io::Result<Vec<u8>> {
            match self.inbound.lock().await.recv().await {
                Some(d) => Ok(d),
                None => std::future::pending().await,
            }
        }
        async fn reset(&mut self) -> std::io::Result<()> {
            self.resets
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    /// Upstream socket-child.spec.ts: the ATEM's answer to the hello, session 0x531b, packet id 0.
    pub const HELLO_REPLY: [u8; 20] = [
        0x10, 0x14, 0x53, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0xd1, 0x00, 0x00, 0x02, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    /// A reliable packet from the ATEM.
    pub fn atem_packet(session: u16, id: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = Header {
            flags: flag::ACK_REQUEST,
            length: (HEADER_LEN + payload.len()) as u16,
            session_id: session,
            packet_id: id,
            ..Header::default()
        }
        .encode()
        .to_vec();
        v.extend_from_slice(payload);
        v
    }

    /// The ATEM acking our packet `id`.
    pub fn atem_ack(session: u16, id: u16) -> Vec<u8> {
        Header {
            flags: flag::ACK_REPLY,
            length: HEADER_LEN as u16,
            session_id: session,
            ack_id: id,
            ..Header::default()
        }
        .encode()
        .to_vec()
    }

    impl FakeAtem {
        /// Answers the hello and swallows the client's ack of it.
        pub async fn accept(&mut self) {
            let hello = self.from_client.recv().await.expect("hello");
            assert_eq!(hello, COMMAND_CONNECT_HELLO);
            self.to_client.send(HELLO_REPLY.to_vec()).unwrap();
            let ack = self.from_client.recv().await.expect("ack of hello");
            assert_eq!(Header::parse(&ack).unwrap().flags, flag::ACK_REPLY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    fn sends(out: &[Output]) -> Vec<Vec<u8>> {
        out.iter()
            .filter_map(|o| match o {
                Output::Send(b) => Some(b.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn header_roundtrip() {
        let h = Header {
            flags: flag::ACK_REQUEST | flag::ACK_REPLY,
            length: 0x123,
            session_id: 0x8001,
            ack_id: 0x0102,
            retransmit_from: 0x0304,
            packet_id: 0x7fff,
        };
        let b = h.encode();
        assert_eq!(
            b,
            [
                0x89, 0x23, 0x80, 0x01, 0x01, 0x02, 0x03, 0x04, 0x00, 0x00, 0x7f, 0xff
            ]
        );
        assert_eq!(Header::parse(&b), Some(h));
        assert_eq!(Header::parse(&b[..11]), None);
    }

    #[test]
    fn covered_by_ack_matches_upstream_wrap_rules() {
        assert!(covered_by_ack(5, 5));
        assert!(covered_by_ack(5, 4));
        assert!(!covered_by_ack(5, 6));
        // Just before the wrap is covered by an ack just after it.
        assert!(covered_by_ack(2, 32766));
        assert!(!covered_by_ack(32766, 2));
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_hello_bytes_match_upstream() {
        let mut s = Session::new(Timings::default(), Instant::now());
        let mut out = vec![];
        s.start(Instant::now(), &mut out);
        // Copied from upstream COMMAND_CONNECT_HELLO.
        let upstream: [u8; 20] = [
            0x10, 0x14, 0x53, 0xab, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3a, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(out, vec![Output::Send(upstream.to_vec())]);
        assert!(!s.is_established());
    }

    #[tokio::test(start_paused = true)]
    async fn acks_reliable_packets() {
        let now = Instant::now();
        let mut s = Session::new(Timings::default(), now);
        let mut out = vec![];
        s.start(now, &mut out);
        out.clear();

        s.handle_datagram(&HELLO_REPLY, now, &mut out);
        assert!(s.is_established());
        // Upstream socket-child.spec.ts: the ack of the hello reply.
        assert_eq!(
            out,
            vec![
                Output::Connected,
                Output::Send(vec![0x80, 0x0c, 0x53, 0x1b, 0, 0, 0, 0, 0, 0, 0, 0]),
            ]
        );
        out.clear();

        // The ATEM switches to its real session id after the hello.
        s.handle_datagram(&atem_packet(0x8034, 1, b"p1"), now, &mut out);
        assert_eq!(
            out,
            vec![
                Output::Commands(b"p1".to_vec()),
                Output::Send(vec![0x80, 0x0c, 0x80, 0x34, 0x00, 0x01, 0, 0, 0, 0, 0, 0]),
            ]
        );
        out.clear();

        // A duplicate is re-acked but not delivered again.
        s.handle_datagram(&atem_packet(0x8034, 1, b"p1"), now, &mut out);
        assert_eq!(out, vec![Output::Send(atem_ack_from_client(0x8034, 1))]);
        out.clear();

        // Out of order: 3 is held until 2 arrives, then both are delivered in order.
        s.handle_datagram(&atem_packet(0x8034, 3, b"p3"), now, &mut out);
        assert!(out.is_empty(), "{out:?}");
        s.handle_datagram(&atem_packet(0x8034, 2, b"p2"), now, &mut out);
        assert_eq!(
            out,
            vec![
                Output::Commands(b"p2".to_vec()),
                Output::Commands(b"p3".to_vec()),
                Output::Send(atem_ack_from_client(0x8034, 3)),
            ]
        );
        out.clear();

        // A datagram whose length field disagrees with its size is dropped.
        let mut bad = atem_packet(0x8034, 4, b"p4");
        bad.push(0);
        s.handle_datagram(&bad, now, &mut out);
        assert!(out.is_empty());
    }

    fn atem_ack_from_client(session: u16, id: u16) -> Vec<u8> {
        atem_ack(session, id)
    }

    #[tokio::test(start_paused = true)]
    async fn reliable_packet_layout_matches_upstream() {
        let now = Instant::now();
        let mut s = Session::new(Timings::default(), now);
        let mut out = vec![];
        s.start(now, &mut out);
        s.handle_datagram(&HELLO_REPLY, now, &mut out);
        s.handle_datagram(&atem_packet(0x8034, 1, b""), now, &mut out);
        out.clear();
        s.send_reliable(&[0xaa, 0xbb], Some(7), now, &mut out);
        // Upstream sendPacket: AckRequest << 11 | length, session, zeros, packet id 1.
        assert_eq!(
            sends(&out),
            vec![vec![
                0x08, 0x0e, 0x80, 0x34, 0, 0, 0, 0, 0, 0, 0x00, 0x01, 0xaa, 0xbb
            ]]
        );
        out.clear();
        s.handle_datagram(&atem_ack(0x8034, 1), now, &mut out);
        assert_eq!(out, vec![Output::Acked(7)]);
    }

    async fn start_link(
        timings: Timings,
    ) -> (
        FakeAtem,
        mpsc::Sender<SendRequest>,
        mpsc::UnboundedReceiver<LinkEvent>,
    ) {
        let (t, atem) = pair();
        let (req_tx, req_rx) = mpsc::channel(8);
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_link(t, timings, req_rx, move |e| {
            let _ = ev_tx.send(e);
        }));
        (atem, req_tx, ev_rx)
    }

    async fn send(
        req: &mpsc::Sender<SendRequest>,
        payload: &[u8],
    ) -> oneshot::Receiver<Result<()>> {
        let (done, rx) = oneshot::channel();
        req.send(SendRequest {
            payload: payload.to_vec(),
            done,
        })
        .await
        .unwrap();
        rx
    }

    #[tokio::test(start_paused = true)]
    async fn resends_unacked_after_100ms() {
        let (mut atem, req, mut ev) = start_link(Timings::default()).await;
        atem.accept().await;
        assert_eq!(ev.recv().await, Some(LinkEvent::Connected));

        let mut done = send(&req, b"cmd").await;
        let first = atem.from_client.recv().await.unwrap();
        let t0 = Instant::now();
        assert_eq!(Header::parse(&first).unwrap().packet_id, 1);
        assert_eq!(&first[HEADER_LEN..], b"cmd");

        // Nothing until 100 ms have passed, then the identical packet again.
        let again = atem.from_client.recv().await.unwrap();
        assert_eq!(Instant::now() - t0, Duration::from_millis(100));
        assert_eq!(again, first);
        assert!(done.try_recv().is_err(), "must not resolve before the ack");

        atem.to_client.send(atem_ack(0x531b, 1)).unwrap();
        assert_eq!(done.await.unwrap(), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_after_resend_budget() {
        let (mut atem, req, mut ev) = start_link(Timings::default()).await;
        atem.accept().await;
        assert_eq!(ev.recv().await, Some(LinkEvent::Connected));
        // Keep the link alive so only the resend budget can fail the command.
        let keepalive = atem.to_client.clone();
        tokio::spawn(async move {
            for id in 1..100u16 {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if keepalive.send(atem_packet(0x531b, id, b"")).is_err() {
                    break;
                }
            }
        });
        let t0 = Instant::now();
        let done = send(&req, b"cmd").await;
        let r = done.await.unwrap();
        assert!(matches!(r, Err(DeviceError::Timeout(_))), "{r:?}");
        // One send plus ten resends, 100 ms apart, then give up on the 11th deadline.
        assert_eq!(Instant::now() - t0, Duration::from_millis(1100));
        let mut cmd_sends = 0;
        while let Ok(dg) = atem.from_client.try_recv() {
            if dg.len() > HEADER_LEN && &dg[HEADER_LEN..] == b"cmd" {
                cmd_sends += 1;
            }
        }
        assert_eq!(cmd_sends, 11);
    }

    #[tokio::test(start_paused = true)]
    async fn pings_after_500ms_of_silence() {
        let (mut atem, _req, mut ev) = start_link(Timings::default()).await;
        atem.accept().await;
        let t0 = Instant::now();
        assert_eq!(ev.recv().await, Some(LinkEvent::Connected));
        let ping = atem.from_client.recv().await.unwrap();
        assert_eq!(Instant::now() - t0, Duration::from_millis(500));
        let h = Header::parse(&ping).unwrap();
        assert_eq!(h.flags, flag::ACK_REQUEST);
        assert_eq!(h.length as usize, HEADER_LEN);
    }

    #[tokio::test(start_paused = true)]
    async fn lost_after_3s_of_silence_then_says_hello_again() {
        let (mut atem, req, mut ev) = start_link(Timings::default()).await;
        atem.accept().await;
        let t0 = Instant::now();
        assert_eq!(ev.recv().await, Some(LinkEvent::Connected));
        assert_eq!(ev.recv().await, Some(LinkEvent::Lost));
        assert_eq!(Instant::now() - t0, Duration::from_secs(3));
        assert_eq!(atem.resets.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Pings went out meanwhile; the last datagram is a fresh hello.
        let mut last = None;
        while let Ok(dg) = atem.from_client.try_recv() {
            last = Some(dg);
        }
        assert_eq!(last.unwrap(), COMMAND_CONNECT_HELLO);
        // While not connected, sends fail at once.
        let r = send(&req, b"cmd").await.await.unwrap();
        assert!(matches!(r, Err(DeviceError::Disconnected(_))), "{r:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn retries_hello_every_second_until_answered() {
        let (mut atem, _req, _ev) = start_link(Timings::default()).await;
        assert_eq!(
            atem.from_client.recv().await.unwrap(),
            COMMAND_CONNECT_HELLO
        );
        let t0 = Instant::now();
        assert_eq!(
            atem.from_client.recv().await.unwrap(),
            COMMAND_CONNECT_HELLO
        );
        assert_eq!(Instant::now() - t0, Duration::from_secs(1));
        assert_eq!(atem.resets.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn resends_on_retransmit_request() {
        let now = Instant::now();
        let mut s = Session::new(Timings::default(), now);
        let mut out = vec![];
        s.start(now, &mut out);
        s.handle_datagram(&HELLO_REPLY, now, &mut out);
        s.send_reliable(b"a", Some(1), now, &mut out);
        s.send_reliable(b"b", Some(2), now, &mut out);
        out.clear();
        let req = Header {
            flags: flag::RETRANSMIT_REQUEST,
            length: HEADER_LEN as u16,
            session_id: 0x531b,
            retransmit_from: 1,
            ..Header::default()
        }
        .encode();
        s.handle_datagram(&req, now, &mut out);
        let resent: Vec<Vec<u8>> = sends(&out)
            .iter()
            .map(|d| d[HEADER_LEN..].to_vec())
            .collect();
        assert_eq!(resent, vec![b"a".to_vec(), b"b".to_vec()]);
    }
}
