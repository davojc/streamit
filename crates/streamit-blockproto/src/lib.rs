//! Generic client for Blackmagic's text "block" protocol (VideoHub 9990, Web Presenter 9977).
//!
//! A block is a header line ending in ':' followed by lines and a blank line; bare `ACK` /
//! `NAK` replies are blocks with no lines. This crate owns everything device-independent:
//! the parser, connect/backoff, PING keepalive, a lossy-UTF-8 line reader with a 64 KiB cap,
//! ACK/NAK FIFO matching, resync (drop and reconnect) when a reply is lost, the "synced" gate
//! (the caller names the block that marks the first full state), and abort-on-drop.
//! Device crates supply an `on_block` hook for the data they keep and an `on_reset` hook that
//! clears it when the link changes.
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use streamit_core::devices::{DeviceError, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

/// One protocol block: a header line ending in ':' followed by lines, ended by a blank line.
/// A bare reply such as `ACK` or `NAK` is a block with that name and no lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub name: String,
    pub lines: Vec<String>,
}

/// Incremental block parser; feed it one line at a time (without the line ending).
#[derive(Default)]
pub struct BlockParser {
    cur: Option<Block>,
}

impl BlockParser {
    pub fn push_line(&mut self, line: &str) -> Option<Block> {
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

/// Timing knobs. `Default` is the production setting; tests shorten them.
#[derive(Debug, Clone)]
pub struct Timings {
    /// Wait before each reconnect attempt; the last value repeats.
    pub backoff: Vec<Duration>,
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
    /// How long a command waits for its ACK/NAK.
    pub command_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            backoff: [1, 2, 4, 8].map(Duration::from_secs).to_vec(),
            ping_interval: Duration::from_secs(5),
            ping_timeout: Duration::from_secs(3),
            command_timeout: Duration::from_secs(2),
            connect_timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Default)]
struct LinkState {
    connected: bool,
    /// True once this session's sync block has arrived.
    synced: bool,
    /// Incremented per session, so a stale resync request never kills a newer session.
    epoch: u64,
}

struct Cmd {
    text: String,
    reply: oneshot::Sender<bool>,
}

enum Pending {
    Cmd(Cmd),
    Ping,
}

/// Called with every block except ACK/NAK, before the sync gate opens for that block.
pub type OnBlock = Arc<dyn Fn(&Block) + Send + Sync>;
/// Called when the link goes up or down; device data must be cleared.
pub type OnReset = Arc<dyn Fn() + Send + Sync>;

/// A self-reconnecting block-protocol connection. Dropping it stops the background task.
pub struct Link {
    name: &'static str,
    state: Arc<Mutex<LinkState>>,
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
    /// The `Link` handle is gone.
    gone: bool,
    /// The sync block arrived on this connection.
    synced: bool,
    /// Reconnect immediately (no backoff).
    resync: bool,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Link {
    /// Starts the client. It does not wait for the link: while the device is unreachable
    /// every call reports `Disconnected`, and it keeps retrying. Must run inside a tokio
    /// runtime. `name` labels logs and errors; `sync_block` is the block whose first arrival
    /// marks the device state as known.
    pub fn spawn(
        name: &'static str,
        sync_block: &'static str,
        addr: SocketAddr,
        timings: Timings,
        on_block: OnBlock,
        on_reset: OnReset,
    ) -> Link {
        let state = Arc::new(Mutex::new(LinkState::default()));
        let (tx, rx) = mpsc::channel(16);
        let reset = Arc::new(Notify::new());
        let ctx = Ctx {
            name,
            sync_block,
            addr,
            t: timings.clone(),
            state: state.clone(),
            reset: reset.clone(),
            on_block,
            on_reset,
        };
        let task = tokio::spawn(run(ctx, rx));
        Link {
            name,
            state,
            cmds: tx,
            timings,
            reset,
            task,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LinkState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn down(&self) -> DeviceError {
        DeviceError::Disconnected(format!("{} link is down", self.name))
    }

    /// Returns the session epoch while connected and synced; otherwise `Disconnected`.
    fn usable(&self) -> Result<u64> {
        let s = self.lock();
        if !s.connected {
            Err(self.down())
        } else if !s.synced {
            Err(DeviceError::Disconnected("not synced".into()))
        } else {
            Ok(s.epoch)
        }
    }

    /// `Ok` only while connected and synced; otherwise `Disconnected`.
    pub fn ensure_usable(&self) -> Result<()> {
        self.usable().map(|_| ())
    }

    /// Sends `text` (a complete wire command) and waits for its ACK/NAK. NAK is `Refused`;
    /// no reply in time is `Timeout` and drops the link to resync the reply queue.
    /// `what` describes the command in errors.
    pub async fn command(&self, text: String, what: &str) -> Result<()> {
        let epoch = self.usable()?;
        let (reply, rx) = oneshot::channel();
        self.cmds
            .send(Cmd { text, reply })
            .await
            .map_err(|_| self.down())?;
        match tokio::time::timeout(self.timings.command_timeout, rx).await {
            Err(_) => {
                // A lost reply would skew the ACK/NAK queue: drop the link and resync.
                if self.lock().epoch == epoch {
                    self.reset.notify_one();
                }
                Err(DeviceError::Timeout(format!("no ACK for {what}")))
            }
            Ok(Err(_)) => Err(self.down()),
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(DeviceError::Refused(format!(
                "{} refused {what}",
                self.name
            ))),
        }
    }
}

struct Ctx {
    name: &'static str,
    sync_block: &'static str,
    addr: SocketAddr,
    t: Timings,
    state: Arc<Mutex<LinkState>>,
    reset: Arc<Notify>,
    on_block: OnBlock,
    on_reset: OnReset,
}

async fn run(ctx: Ctx, mut cmds: mpsc::Receiver<Cmd>) {
    let (name, addr) = (ctx.name, ctx.addr);
    let backoff = if ctx.t.backoff.is_empty() {
        vec![Duration::from_secs(1)]
    } else {
        ctx.t.backoff.clone()
    };
    let mut attempt = 0usize;
    loop {
        match tokio::time::timeout(ctx.t.connect_timeout, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                tracing::info!(%addr, "{name} connected");
                set_link(&ctx, true);
                let out = session(stream, &ctx, &mut cmds).await;
                set_link(&ctx, false);
                if out.gone {
                    return;
                }
                if out.synced {
                    attempt = 0;
                }
                tracing::warn!(%addr, "{name} link lost");
                if out.resync {
                    continue;
                }
            }
            Ok(Err(e)) => tracing::debug!(%addr, "{name} connect failed: {e}"),
            Err(_) => tracing::debug!(%addr, "{name} connect timed out"),
        }
        tokio::time::sleep(backoff[attempt.min(backoff.len() - 1)]).await;
        attempt += 1;
    }
}

fn set_link(ctx: &Ctx, up: bool) {
    {
        let mut s = ctx.state.lock().unwrap_or_else(|e| e.into_inner());
        s.connected = up;
        s.synced = false;
        if up {
            s.epoch += 1;
        }
    }
    (ctx.on_reset)();
}

/// Longest line accepted from the device; a longer one means the link is garbage.
const MAX_LINE: u64 = 64 * 1024;

/// Runs one connection until it ends.
async fn session(stream: TcpStream, ctx: &Ctx, cmds: &mut mpsc::Receiver<Cmd>) -> Outcome {
    let name = ctx.name;
    let t = &ctx.t;
    // Commands queued while the link was down are stale: dropping them reports Disconnected.
    while cmds.try_recv().is_ok() {}
    // So is a resync request left over from a previous session.
    let _ = tokio::time::timeout(Duration::ZERO, ctx.reset.notified()).await;

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
                tracing::warn!("{name} sent an over-long line; dropping link");
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
                    "ACK" | "NAK" => {
                        let ack = block.name == "ACK";
                        match pending.pop_front() {
                            Some(Pending::Cmd(c)) => {
                                let _ = c.reply.send(ack);
                            }
                            Some(Pending::Ping) => ping_deadline = None,
                            None => {}
                        }
                    }
                    _ => {
                        (ctx.on_block)(&block);
                        if block.name == ctx.sync_block {
                            ctx.state.lock().unwrap_or_else(|e| e.into_inner()).synced = true;
                        }
                    }
                }
            }
            cmd = cmds.recv() => {
                let Some(c) = cmd else { out.gone = true; break };
                if wr.write_all(c.text.as_bytes()).await.is_err() {
                    break;
                }
                pending.push_back(Pending::Cmd(c));
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
                tracing::warn!("{name} did not answer PING");
                break;
            }
            _ = ctx.reset.notified() => {
                tracing::warn!("{name} reply lost; resyncing");
                out.resync = true;
                break;
            }
        }
    }
    out.synced = ctx.state.lock().unwrap_or_else(|e| e.into_inner()).synced;
    // Dropping `pending` drops the reply senders, so waiting commands see Disconnected.
    out
}
