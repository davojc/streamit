//! Blackmagic VideoHub client (Ethernet protocol, TCP 9990).
//!
//! The protocol is 0-based; this crate's public API (and the `Router` trait and the config)
//! is 1-based, as the device front panel and Companion number things. Conversion happens
//! only at the protocol boundary, in `format_route` and when reading routing blocks.
use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use streamit_core::devices::{DeviceError, Result, Router};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

const ROUTING: &str = "VIDEO OUTPUT ROUTING";

/// One protocol block: a header line ending in ':' followed by lines, ended by a blank line.
/// A bare reply such as `ACK` or `NAK` is a block with that name and no lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub name: String,
    pub lines: Vec<String>,
}

/// Incremental block parser; feed it one line at a time (without the line ending).
#[derive(Default)]
struct BlockParser {
    cur: Option<Block>,
}

impl BlockParser {
    fn push_line(&mut self, line: &str) -> Option<Block> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            return self.cur.take();
        }
        match &mut self.cur {
            Some(b) => {
                b.lines.push(line.to_string());
                None
            }
            None => match line.trim().strip_suffix(':') {
                Some(name) => {
                    self.cur = Some(Block {
                        name: name.to_string(),
                        lines: vec![],
                    });
                    None
                }
                None => Some(Block {
                    name: line.trim().to_string(),
                    lines: vec![],
                }),
            },
        }
    }
}

/// Splits protocol text into blocks. Accepts `\n` and `\r\n`. Unknown blocks are kept
/// (callers ignore what they do not need).
pub fn parse_blocks(text: &str) -> Vec<Block> {
    let mut p = BlockParser::default();
    let mut out: Vec<Block> = text.lines().filter_map(|l| p.push_line(l)).collect();
    out.extend(p.cur.take());
    out
}

/// The wire form of a route command. Arguments are 1-based; the output is 0-based.
pub fn format_route(out1: u16, in1: u16) -> String {
    format!(
        "{ROUTING}:\n{} {}\n\n",
        out1.saturating_sub(1),
        in1.saturating_sub(1)
    )
}

/// Timing knobs. `Default` is the production setting; tests shorten them.
#[derive(Debug, Clone)]
pub struct Timings {
    /// Wait before each reconnect attempt; the last value repeats.
    pub backoff: Vec<Duration>,
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
    pub route_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            backoff: [1, 2, 4, 8].map(Duration::from_secs).to_vec(),
            ping_interval: Duration::from_secs(5),
            ping_timeout: Duration::from_secs(3),
            route_timeout: Duration::from_secs(2),
            connect_timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Default)]
struct State {
    connected: bool,
    /// True once this session's first VIDEO OUTPUT ROUTING block has arrived.
    synced: bool,
    /// Incremented per session, so a stale resync request never kills a newer session.
    epoch: u64,
    /// 1-based output -> 1-based input.
    table: HashMap<u16, u16>,
}

struct Cmd {
    out: u16,
    inp: u16,
    reply: oneshot::Sender<bool>,
}

enum Pending {
    Route(Cmd),
    Ping,
}

pub struct VideoHub {
    state: Arc<Mutex<State>>,
    cmds: mpsc::Sender<Cmd>,
    timings: Timings,
    /// Asks the session to drop the link and reconnect (lost reply sync).
    reset: Arc<Notify>,
    task: JoinHandle<()>,
}

/// Aborts the task when dropped (a dropped `JoinHandle` only detaches).
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Default)]
struct Outcome {
    /// The `VideoHub` handle is gone.
    gone: bool,
    /// The dump arrived on this connection.
    synced: bool,
    /// Reconnect immediately (no backoff).
    resync: bool,
}

impl Drop for VideoHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl VideoHub {
    /// Starts the client with production timings. It does not wait for the link: while the
    /// device is unreachable every call reports `Disconnected`, and it keeps retrying.
    pub async fn connect(addr: SocketAddr) -> Result<Arc<VideoHub>> {
        Self::connect_with(addr, Timings::default()).await
    }

    pub async fn connect_with(addr: SocketAddr, timings: Timings) -> Result<Arc<VideoHub>> {
        let state = Arc::new(Mutex::new(State::default()));
        let (tx, rx) = mpsc::channel(16);
        let reset = Arc::new(Notify::new());
        let task = tokio::spawn(run(addr, timings.clone(), state.clone(), rx, reset.clone()));
        Ok(Arc::new(VideoHub {
            state,
            cmds: tx,
            timings,
            reset,
            task,
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn down() -> DeviceError {
    DeviceError::Disconnected("videohub link is down".into())
}

fn usable(s: &State) -> Result<()> {
    if !s.connected {
        Err(down())
    } else if !s.synced {
        Err(DeviceError::Disconnected("not synced".into()))
    } else {
        Ok(())
    }
}

#[async_trait]
impl Router for VideoHub {
    async fn route_of(&self, output: u16) -> Result<u16> {
        let s = self.lock();
        usable(&s)?;
        s.table
            .get(&output)
            .copied()
            .ok_or_else(|| DeviceError::Other("unknown output".into()))
    }

    async fn route(&self, output: u16, input: u16) -> Result<()> {
        if output == 0 || input == 0 {
            return Err(DeviceError::Other(
                "outputs and inputs are numbered from 1".into(),
            ));
        }
        let epoch = {
            let s = self.lock();
            usable(&s)?;
            s.epoch
        };
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(Cmd {
                out: output,
                inp: input,
                reply,
            })
            .await
            .map_err(|_| down())?;
        match tokio::time::timeout(self.timings.route_timeout, rx).await {
            Err(_) => {
                // A lost reply would skew the ACK/NAK queue: drop the link and resync.
                if self.lock().epoch == epoch {
                    self.reset.notify_one();
                }
                Err(DeviceError::Timeout(format!(
                    "no ACK for route {output} <- {input}"
                )))
            }
            Ok(Err(_)) => Err(down()),
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(DeviceError::Refused(format!(
                "videohub refused route {output} <- {input}"
            ))),
        }
    }
}

async fn run(
    addr: SocketAddr,
    t: Timings,
    state: Arc<Mutex<State>>,
    mut cmds: mpsc::Receiver<Cmd>,
    reset: Arc<Notify>,
) {
    let backoff = if t.backoff.is_empty() {
        vec![Duration::from_secs(1)]
    } else {
        t.backoff.clone()
    };
    let mut attempt = 0usize;
    loop {
        match tokio::time::timeout(t.connect_timeout, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                tracing::info!(%addr, "videohub connected");
                set_link(&state, true);
                let out = session(stream, &t, &state, &mut cmds, &reset).await;
                set_link(&state, false);
                if out.gone {
                    return;
                }
                if out.synced {
                    attempt = 0;
                }
                tracing::warn!(%addr, "videohub link lost");
                if out.resync {
                    continue;
                }
            }
            Ok(Err(e)) => tracing::debug!(%addr, "videohub connect failed: {e}"),
            Err(_) => tracing::debug!(%addr, "videohub connect timed out"),
        }
        tokio::time::sleep(backoff[attempt.min(backoff.len() - 1)]).await;
        attempt += 1;
    }
}

fn set_link(state: &Mutex<State>, up: bool) {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    s.connected = up;
    s.synced = false;
    s.table.clear();
    if up {
        s.epoch += 1;
    }
}

fn apply_routing(state: &Mutex<State>, block: &Block) {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    s.synced = true;
    for line in &block.lines {
        let mut it = line.split_whitespace().map(str::parse::<u16>);
        if let (Some(Ok(o)), Some(Ok(i))) = (it.next(), it.next()) {
            s.table.insert(o.saturating_add(1), i.saturating_add(1));
        }
    }
}

/// Longest line accepted from the device; a longer one means the link is garbage.
const MAX_LINE: u64 = 64 * 1024;

/// Runs one connection until it ends.
async fn session(
    stream: TcpStream,
    t: &Timings,
    state: &Mutex<State>,
    cmds: &mut mpsc::Receiver<Cmd>,
    reset: &Notify,
) -> Outcome {
    // Commands queued while the link was down are stale: dropping them reports Disconnected.
    while cmds.try_recv().is_ok() {}
    // So is a resync request left over from a previous session.
    let _ = tokio::time::timeout(Duration::ZERO, reset.notified()).await;

    let (rd, mut wr) = stream.into_split();
    let (btx, mut brx) = mpsc::unbounded_channel::<Block>();
    let _reader = AbortOnDrop(tokio::spawn(async move {
        let mut rd = BufReader::new(rd);
        let mut parser = BlockParser::default();
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            let n = match (&mut rd).take(MAX_LINE).read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n as u64,
            };
            if n >= MAX_LINE && buf.last() != Some(&b'\n') {
                tracing::warn!("videohub sent an over-long line; dropping link");
                return;
            }
            // Labels may not be UTF-8 (e.g. Latin-1); decode lossily.
            let line = String::from_utf8_lossy(&buf);
            if let Some(b) = parser.push_line(&line)
                && btx.send(b).is_err()
            {
                return;
            }
        }
    }));

    let mut pending: VecDeque<Pending> = VecDeque::new();
    let mut ping_deadline: Option<Instant> = None;
    let mut tick = tokio::time::interval_at(Instant::now() + t.ping_interval, t.ping_interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut out = Outcome::default();

    loop {
        let dead = async {
            match ping_deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            block = brx.recv() => {
                let Some(block) = block else { break };
                match block.name.as_str() {
                    ROUTING => apply_routing(state, &block),
                    "ACK" | "NAK" => {
                        let ack = block.name == "ACK";
                        match pending.pop_front() {
                            // The device's own routing broadcast updates the table.
                            Some(Pending::Route(c)) => {
                                let _ = c.reply.send(ack);
                            }
                            Some(Pending::Ping) => ping_deadline = None,
                            None => {}
                        }
                    }
                    _ => {}
                }
            }
            cmd = cmds.recv() => {
                let Some(c) = cmd else { out.gone = true; break };
                if wr.write_all(format_route(c.out, c.inp).as_bytes()).await.is_err() {
                    break;
                }
                pending.push_back(Pending::Route(c));
            }
            _ = tick.tick() => {
                if ping_deadline.is_none() {
                    if wr.write_all(b"PING:\n\n").await.is_err() {
                        break;
                    }
                    pending.push_back(Pending::Ping);
                    ping_deadline = Some(Instant::now() + t.ping_timeout);
                }
            }
            _ = dead => {
                tracing::warn!("videohub did not answer PING");
                break;
            }
            _ = reset.notified() => {
                tracing::warn!("videohub reply lost; resyncing");
                out.resync = true;
                break;
            }
        }
    }
    out.synced = state.lock().unwrap_or_else(|e| e.into_inner()).synced;
    // Dropping `pending` drops the reply senders, so waiting route() calls see Disconnected.
    out
}
