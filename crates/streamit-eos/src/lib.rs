//! ETC Eos family lighting console over OSC on TCP (port 3032).
//!
//! The console's "TCP Format for OSC" setting (Setup › System › Show Control › OSC) must
//! match `Framing`: OSC 1.1 (SLIP, the default here) or OSC 1.0 (packet length headers).
//! StreamIt subscribes (`/eos/subscribe 1`) so the console sends the show name and the
//! active cue, and pings (`/eos/ping`) every few seconds: the console is up while it
//! answers. Firing a cue is `/eos/cue/<list>/<cue>/fire`.
//!
//! Sources: ETC's Eos Family "Show Control User Guide" (OSC chapter) for the addresses
//! and framing; not yet checked against the church's console.
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use streamit_core::devices::{DeviceError, Lighting, LightingStatus, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{info, warn};

/// The console's OSC TCP port.
pub const PORT: u16 = 3032;

/// How OSC packets are framed on the TCP stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Framing {
    /// OSC 1.1: SLIP (packets between END bytes).
    #[default]
    Slip,
    /// OSC 1.0: each packet after a 4-byte big-endian length.
    Length,
}

/// One OSC argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Int(i32),
    Float(f32),
    Str(String),
}

/// One OSC message.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub address: String,
    pub args: Vec<Arg>,
}

impl Message {
    pub fn new(address: impl Into<String>, args: Vec<Arg>) -> Message {
        Message { address: address.into(), args }
    }

    fn str_arg(&self, i: usize) -> Option<&str> {
        match self.args.get(i) {
            Some(Arg::Str(s)) => Some(s),
            _ => None,
        }
    }
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// The OSC 1.0 bytes of `m`.
pub fn encode(m: &Message) -> Vec<u8> {
    let mut out = Vec::new();
    push_str(&mut out, &m.address);
    let mut tags = String::from(",");
    for a in &m.args {
        tags.push(match a {
            Arg::Int(_) => 'i',
            Arg::Float(_) => 'f',
            Arg::Str(_) => 's',
        });
    }
    push_str(&mut out, &tags);
    for a in &m.args {
        match a {
            Arg::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            Arg::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
            Arg::Str(s) => push_str(&mut out, s),
        }
    }
    out
}

fn read_str(b: &[u8], at: &mut usize) -> Option<String> {
    let rest = b.get(*at..)?;
    let end = rest.iter().position(|&c| c == 0)?;
    let s = String::from_utf8_lossy(&rest[..end]).into_owned();
    *at += (end + 4) & !3;
    Some(s)
}

fn read_4(b: &[u8], at: &mut usize) -> Option<[u8; 4]> {
    let v: [u8; 4] = b.get(*at..*at + 4)?.try_into().ok()?;
    *at += 4;
    Some(v)
}

/// The message in OSC packet `b`, or `None` for a bundle or anything malformed. Argument
/// types StreamIt does not use are skipped where their size is known.
pub fn decode(b: &[u8]) -> Option<Message> {
    let mut at = 0;
    let address = read_str(b, &mut at)?;
    if !address.starts_with('/') {
        return None;
    }
    if at >= b.len() {
        return Some(Message { address, args: Vec::new() });
    }
    let tags = read_str(b, &mut at)?;
    let mut args = Vec::new();
    for t in tags.strip_prefix(',')?.chars() {
        match t {
            'i' => args.push(Arg::Int(i32::from_be_bytes(read_4(b, &mut at)?))),
            'f' => args.push(Arg::Float(f32::from_be_bytes(read_4(b, &mut at)?))),
            's' => args.push(Arg::Str(read_str(b, &mut at)?)),
            'T' | 'F' | 'N' | 'I' => {}
            'h' | 'd' | 't' => at += 8,
            _ => return None,
        }
    }
    Some(Message { address, args })
}

const END: u8 = 0xC0;
const ESC: u8 = 0xDB;
const ESC_END: u8 = 0xDC;
const ESC_ESC: u8 = 0xDD;

/// `packet` framed for the stream.
pub fn frame(packet: &[u8], framing: Framing) -> Vec<u8> {
    match framing {
        Framing::Length => {
            let mut out = (packet.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(packet);
            out
        }
        Framing::Slip => {
            let mut out = vec![END];
            for &c in packet {
                match c {
                    END => out.extend_from_slice(&[ESC, ESC_END]),
                    ESC => out.extend_from_slice(&[ESC, ESC_ESC]),
                    c => out.push(c),
                }
            }
            out.push(END);
            out
        }
    }
}

/// Splits a byte stream into packets. Feed it what arrives; it returns the whole packets.
#[derive(Debug, Default)]
pub struct Deframer {
    framing: Framing,
    buf: Vec<u8>,
    escaped: bool,
}

/// The largest packet kept: anything bigger is dropped.
const MAX_PACKET: usize = 64 * 1024;

impl Deframer {
    pub fn new(framing: Framing) -> Deframer {
        Deframer { framing, ..Deframer::default() }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        match self.framing {
            Framing::Slip => {
                for &c in bytes {
                    match (self.escaped, c) {
                        (false, END) => {
                            if !self.buf.is_empty() {
                                out.push(std::mem::take(&mut self.buf));
                            }
                        }
                        (false, ESC) => self.escaped = true,
                        (true, ESC_END) => {
                            self.buf.push(END);
                            self.escaped = false;
                        }
                        (true, ESC_ESC) => {
                            self.buf.push(ESC);
                            self.escaped = false;
                        }
                        (true, c) => {
                            self.buf.push(c);
                            self.escaped = false;
                        }
                        (false, c) => self.buf.push(c),
                    }
                    if self.buf.len() > MAX_PACKET {
                        self.buf.clear();
                    }
                }
            }
            Framing::Length => {
                self.buf.extend_from_slice(bytes);
                while self.buf.len() >= 4 {
                    let n = u32::from_be_bytes(self.buf[..4].try_into().expect("4 bytes")) as usize;
                    if n > MAX_PACKET {
                        self.buf.clear();
                        break;
                    }
                    if self.buf.len() < 4 + n {
                        break;
                    }
                    out.push(self.buf[4..4 + n].to_vec());
                    self.buf.drain(..4 + n);
                }
            }
        }
        out
    }
}

/// The address that fires `cue` in cue list `list`.
pub fn fire_address(list: u32, cue: &str) -> String {
    format!("/eos/cue/{list}/{}/fire", cue.trim())
}

/// Timing knobs. `Default` is the production setting; tests shorten them.
#[derive(Debug, Clone)]
pub struct Timings {
    pub ping_every: Duration,
    /// The console is down when it has said nothing for this long.
    pub silent_after: Duration,
    pub reconnect_after: Duration,
    pub connect_timeout: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            ping_every: Duration::from_secs(3),
            silent_after: Duration::from_secs(10),
            reconnect_after: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Default)]
struct State {
    connected: bool,
    last_heard: Option<Instant>,
    status: LightingStatus,
}

/// A self-reconnecting connection to an Eos console. Dropping it stops it.
pub struct EosClient {
    state: Arc<Mutex<State>>,
    tx: mpsc::Sender<Vec<u8>>,
    framing: Framing,
    timings: Timings,
    task: JoinHandle<()>,
}

impl Drop for EosClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl EosClient {
    /// Starts the client. It does not wait for the console: while it is unreachable
    /// every call reports `Disconnected`, and it keeps retrying.
    pub fn connect(addr: SocketAddr, framing: Framing) -> Arc<EosClient> {
        Self::connect_with(addr, framing, Timings::default())
    }

    pub fn connect_with(addr: SocketAddr, framing: Framing, timings: Timings) -> Arc<EosClient> {
        let state: Arc<Mutex<State>> = Arc::default();
        let (tx, rx) = mpsc::channel(32);
        let task = tokio::spawn(run(addr, framing, timings.clone(), state.clone(), rx));
        Arc::new(EosClient { state, tx, framing, timings, task })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn usable(&self) -> Result<()> {
        let s = self.lock();
        if !s.connected {
            return Err(DeviceError::Disconnected("lighting console not connected".into()));
        }
        match s.last_heard {
            Some(t) if t.elapsed() <= self.timings.silent_after => Ok(()),
            _ => Err(DeviceError::Timeout("the lighting console is not answering".into())),
        }
    }

    async fn send(&self, m: Message) -> Result<()> {
        self.tx
            .send(frame(&encode(&m), self.framing))
            .await
            .map_err(|_| DeviceError::Disconnected("lighting console link stopped".into()))
    }
}

#[async_trait]
impl Lighting for EosClient {
    async fn fire_cue(&self, list: u32, cue: &str) -> Result<()> {
        if list == 0 || cue.trim().is_empty() {
            return Err(DeviceError::Other("a cue needs a list (from 1) and a number".into()));
        }
        self.usable()?;
        self.send(Message::new(fire_address(list, cue), vec![])).await
    }

    async fn status(&self) -> Result<LightingStatus> {
        self.usable()?;
        Ok(self.lock().status.clone())
    }
}

/// What a message from the console changes.
fn apply(state: &mut State, m: &Message) {
    state.last_heard = Some(Instant::now());
    match m.address.as_str() {
        "/eos/out/active/cue/text" => {
            state.status.active_cue = m.str_arg(0).map(str::to_owned).filter(|s| !s.is_empty())
        }
        "/eos/out/show/name" => state.status.show = m.str_arg(0).map(str::to_owned).filter(|s| !s.is_empty()),
        _ => {}
    }
}

async fn run(
    addr: SocketAddr,
    framing: Framing,
    t: Timings,
    state: Arc<Mutex<State>>,
    mut rx: mpsc::Receiver<Vec<u8>>,
) {
    let set = |f: &dyn Fn(&mut State)| f(&mut state.lock().unwrap_or_else(|e| e.into_inner()));
    loop {
        let stream = match tokio::time::timeout(t.connect_timeout, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => s,
            _ => {
                tokio::time::sleep(t.reconnect_after).await;
                continue;
            }
        };
        info!(%addr, "lighting console connected");
        set(&|s| {
            s.connected = true;
            s.last_heard = Some(Instant::now());
        });
        let (mut read, mut write) = stream.into_split();
        let hello = [Message::new("/eos/subscribe", vec![Arg::Int(1)]), Message::new("/eos/ping", vec![])];
        let mut ok = true;
        for m in hello {
            ok &= write.write_all(&frame(&encode(&m), framing)).await.is_ok();
        }
        let mut deframer = Deframer::new(framing);
        let mut buf = vec![0u8; 4096];
        let mut ping = tokio::time::interval(t.ping_every);
        while ok {
            tokio::select! {
                n = read.read(&mut buf) => match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for packet in deframer.push(&buf[..n]) {
                            if let Some(m) = decode(&packet) {
                                set(&|s| apply(s, &m));
                            }
                        }
                    }
                },
                out = rx.recv() => match out {
                    Some(bytes) => ok = write.write_all(&bytes).await.is_ok(),
                    None => return,
                },
                _ = ping.tick() => {
                    ok = write.write_all(&frame(&encode(&Message::new("/eos/ping", vec![])), framing)).await.is_ok();
                    let silent = state.lock().unwrap_or_else(|e| e.into_inner()).last_heard
                        .is_none_or(|h| h.elapsed() > t.silent_after * 2);
                    if silent {
                        warn!(%addr, "lighting console stopped answering; reconnecting");
                        break;
                    }
                }
            }
        }
        warn!(%addr, "lighting console link down");
        set(&|s| {
            s.connected = false;
            s.status = LightingStatus::default();
        });
        tokio::time::sleep(t.reconnect_after).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let m = Message::new(
            "/eos/out/active/cue/text",
            vec![Arg::Str("1/5 Sermon 3.0 100%".into()), Arg::Int(7), Arg::Float(0.5)],
        );
        let b = encode(&m);
        assert_eq!(b.len() % 4, 0, "OSC is 4-byte aligned");
        assert_eq!(decode(&b), Some(m));
        let bare = encode(&Message::new("/eos/ping", vec![]));
        assert_eq!(bare, b"/eos/ping\0\0\0,\0\0\0".to_vec());
        assert_eq!(decode(b"#bundle\0"), None);
    }

    #[test]
    fn slip_frames_escape_and_split() {
        let packet = vec![1, END, 2, ESC, 3];
        let framed = frame(&packet, Framing::Slip);
        assert_eq!(framed, vec![END, 1, ESC, ESC_END, 2, ESC, ESC_ESC, 3, END]);
        let mut d = Deframer::new(Framing::Slip);
        // Arriving in pieces, two packets back to back.
        let mut stream = framed.clone();
        stream.extend(frame(&[9, 9], Framing::Slip));
        let (a, b) = stream.split_at(4);
        let mut got = d.push(a);
        got.extend(d.push(b));
        assert_eq!(got, vec![packet, vec![9, 9]]);
    }

    #[test]
    fn length_frames_split() {
        let mut d = Deframer::new(Framing::Length);
        let mut stream = frame(&[1, 2, 3], Framing::Length);
        stream.extend(frame(&[4], Framing::Length));
        let (a, b) = stream.split_at(5);
        let mut got = d.push(a);
        assert!(got.is_empty() || got == vec![vec![1, 2, 3]]);
        got.extend(d.push(b));
        assert_eq!(got, vec![vec![1, 2, 3], vec![4]]);
    }

    #[test]
    fn fire_addresses() {
        assert_eq!(fire_address(1, "5"), "/eos/cue/1/5/fire");
        assert_eq!(fire_address(2, " 10.5 "), "/eos/cue/2/10.5/fire");
    }
}
