//! Sessions: the real ↔ simulation switch.
//!
//! A session is one mode's device layer (built by a [`DeviceFactory`]), its engine, its
//! snapshot log and its health poll. The window and the Stream Deck never hold a
//! session: they hold the [`Front`], whose command channel and snapshot stay the same
//! across switches. One router passes (and logs) each front command to the current
//! session's engine through a gate, and the engine's snapshots come back out.
//!
//! The app always starts in real mode. A switch is refused unless nothing is happening
//! (stage Idle or Ended, and no job). It closes the gate (later presses are dropped),
//! waits until every press already on its way has reached the old engine, checks again,
//! and only then stops the old session completely and opens the new one, so no press
//! made in one world can ever run in the other. Opening the real session is the normal
//! read-only startup. The simulation
//! session builds only pretend devices: [`SimFactory`] never touches `wiring`, mDNS or a
//! socket, and the real factory is not called while simulating.
//!
//! Config rule: the real session's engine shares the window's config (Settings saves
//! apply to it as before). The simulation engine gets its own copy, the effective config
//! of [`sim_config`], rebuilt from the real config at each switch and never saved; its
//! `SimControl` shares it and rewrites the settle, hold and fade on each speed change.
//! Settings saves while simulating still write the real file (and the window's copy);
//! the simulation picks them up the next time it is opened.
use crate::logging::simulation_span;
use crate::wiring::{self, Absent, Probes};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use streamit_core::audio::LiveLevels;
use streamit_core::config::Config;
use streamit_core::devices::{Broadcasts, Devices};
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, Engine, Snapshot};
use streamit_core::golive::Stage;
use streamit_sim::{SimControl, Speed};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tracing::{Instrument, Span, error, info, warn};

/// What a refused switch says.
pub const REFUSED: &str = "Finish or end the service first";
/// What a switch says while the engine is still in its startup check.
pub const CHECKING: &str = "Still checking the equipment — try again in a moment";
/// The simulation could not be opened; the real equipment was reopened.
pub const SIM_FAILED: &str = "The simulation could not start; StreamIt is back in real mode";
/// Neither mode could be opened: nothing more can be switched.
pub const BROKEN: &str = "StreamIt could not reopen the equipment: restart StreamIt";
/// "Crash & restart app" is a simulation control.
pub const RESTART_REAL: &str = "Crash & restart is only available in simulation";
/// Longest a switch waits for presses already on their way to reach the engine.
pub const DRAIN_WAIT: Duration = Duration::from_secs(2);
/// How long the engine is given to act on the last press it took.
const SETTLE: Duration = Duration::from_millis(20);
/// Longest a stopping session waits for its engine and the engine's side tasks to let go
/// of the devices. A side task still running then (a slow YouTube call) finishes on its
/// own; it holds the old devices only until it does.
pub const STOP_WAIT: Duration = Duration::from_secs(5);

/// Controller ruling S4: in simulation an unset (0) VideoHub number gets these values,
/// in the effective config only.
pub const SIM_INPUT8_OUTPUT: u16 = 11;
pub const SIM_DECKLINK_INPUT: u16 = 39;
pub const SIM_FALLBACK_NORMAL_INPUT: u16 = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Real,
    Simulation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchRefused(pub String);

impl std::fmt::Display for SwitchRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One mode's devices, as a factory built them.
pub struct Built {
    pub devices: Devices,
    /// The first poll, sent to the engine straight away.
    pub health: Vec<(DeviceKind, DeviceHealth)>,
    /// What the health poll reads; `None` runs no poll.
    pub probes: Option<Probes>,
    /// The pretend world's handle, for the Simulation tab.
    pub sim: Option<SimControl>,
    /// The config the engine runs on when it is not the real one: simulation's effective
    /// copy, shared with its `SimControl` (which rescales its waits with the speed).
    /// Never saved.
    pub config: Option<Arc<RwLock<Config>>>,
    /// The fields `config` filled in with a simulated default.
    pub sim_defaults: Vec<&'static str>,
    /// Background device tasks to stop with the session.
    pub tasks: Vec<AbortHandle>,
}

impl Built {
    /// Just devices: no health, no poll, the real config.
    #[cfg(test)]
    pub fn plain(devices: Devices) -> Built {
        Built {
            devices,
            health: Vec::new(),
            probes: None,
            sim: None,
            config: None,
            sim_defaults: Vec::new(),
            tasks: Vec::new(),
        }
    }
}

#[async_trait]
pub trait DeviceFactory: Send + Sync {
    async fn build(&self, c: &Config) -> Built;
}

/// The real equipment: `wiring::build`, which only connects and listens.
pub struct RealFactory;

#[async_trait]
impl DeviceFactory for RealFactory {
    async fn build(&self, c: &Config) -> Built {
        let w = wiring::build(c).await;
        Built {
            devices: w.devices,
            health: w.health,
            probes: Some(w.probes),
            sim: None,
            config: None,
            sim_defaults: Vec::new(),
            tasks: w.tasks,
        }
    }
}

/// The pretend equipment of `streamit_sim`, seeded from the effective config. No real
/// client, no mDNS, no socket.
pub struct SimFactory;

#[async_trait]
impl DeviceFactory for SimFactory {
    async fn build(&self, c: &Config) -> Built {
        let (config, sim_defaults) = sim_config(c);
        if !sim_defaults.is_empty() {
            info!(fields = ?sim_defaults, "simulation: using simulated defaults for unset settings");
        }
        let sim = SimControl::new(&config, Speed::X1);
        let devices = wiring::logged(sim.devices());
        let probes = Probes::ready(devices.clone());
        let health = probes.poll(config.videohub.input8_output).await;
        Built {
            devices,
            health,
            probes: Some(probes),
            config: Some(sim.shared_config()),
            sim: Some(sim),
            sim_defaults,
            tasks: Vec::new(),
        }
    }
}

/// The simulation's effective config: `c` with each unset (0) VideoHub number replaced
/// by its simulated default, and the names of the fields replaced (ruling S4). Its
/// YouTube stream is the pretend one the pretend broadcast is bound to, so a restart
/// finds that broadcast again; Settings still shows (and saves) the real stream id.
pub fn sim_config(c: &Config) -> (Config, Vec<&'static str>) {
    let mut e = c.clone();
    e.youtube.stream_id = Some(streamit_sim::SEED_STREAM.into());
    let mut defaulted = Vec::new();
    let hub = &mut e.videohub;
    for (field, value, default) in [
        ("videohub.input8_output", &mut hub.input8_output, SIM_INPUT8_OUTPUT),
        ("videohub.decklink_input", &mut hub.decklink_input, SIM_DECKLINK_INPUT),
        ("videohub.fallback_normal_input", &mut hub.fallback_normal_input, SIM_FALLBACK_NORMAL_INPUT),
    ] {
        if *value == 0 {
            *value = default;
            defaulted.push(field);
        }
    }
    (e, defaulted)
}

/// Where each mode keeps the mics' live levels: beside the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub levels: PathBuf,
    pub sim_levels: PathBuf,
}

impl Paths {
    pub fn beside(config: &Path) -> Paths {
        Paths {
            levels: streamit_core::config::levels_path(config),
            sim_levels: config.with_file_name("sim-levels.toml"),
        }
    }

    pub fn levels_for(&self, mode: Mode) -> &Path {
        match mode {
            Mode::Real => &self.levels,
            Mode::Simulation => &self.sim_levels,
        }
    }
}

/// The stable handle the window and the deck hold. Cloneable.
#[derive(Clone)]
pub struct Front {
    /// Goes to the current session's engine. Commands sent while a switch is under way
    /// are dropped (and logged).
    pub tx: mpsc::Sender<Command>,
    /// The current session's snapshot.
    pub rx: watch::Receiver<Snapshot>,
    pub mode: watch::Receiver<Mode>,
    /// The pretend world, while simulating.
    pub sim: watch::Receiver<Option<SimControl>>,
    /// Settings the simulation filled with a simulated default (empty in real mode).
    pub sim_defaults: watch::Receiver<Vec<&'static str>>,
    /// The current session's YouTube, for the window's broadcast list.
    pub yt: watch::Receiver<Arc<dyn Broadcasts>>,
    /// The current session's devices, for profile backups; `None` while switching.
    pub devices: watch::Receiver<Option<Devices>>,
    /// True while one session is being stopped and the next opened.
    pub switching: watch::Receiver<bool>,
}

/// Where the front's commands go: the current engine, and the span to log them in.
#[derive(Clone)]
struct Route {
    tx: mpsc::Sender<Command>,
    span: Span,
}

/// The senders behind the `Front`.
struct Outputs {
    /// The current session's engine; `None` while the gate is closed for a switch.
    target: watch::Sender<Option<Route>>,
    /// Commands the router has taken but not yet handed to an engine (or dropped).
    inflight: Arc<AtomicUsize>,
    snap: watch::Sender<Snapshot>,
    mode: watch::Sender<Mode>,
    sim: watch::Sender<Option<SimControl>>,
    sim_defaults: watch::Sender<Vec<&'static str>>,
    yt: watch::Sender<Arc<dyn Broadcasts>>,
    devices: watch::Sender<Option<Devices>>,
    switching: watch::Sender<bool>,
}

/// One mode's device layer, kept for the whole session (an engine restart reuses it).
struct Kit {
    devices: Devices,
    /// The config the engine runs on: the shared real one, or simulation's own copy.
    config: Arc<RwLock<Config>>,
    sim: Option<SimControl>,
    sim_defaults: Vec<&'static str>,
    device_tasks: Vec<AbortHandle>,
}

/// One running session.
struct Session {
    mode: Mode,
    kit: Kit,
    /// The engine's command sender. Only this, the gate (`Outputs::target`) and the
    /// health poll hold it, so the engine stops once the session is closed.
    tx: mpsc::Sender<Command>,
    span: Span,
    /// The snapshot forwarding, snapshot log and health poll.
    tasks: Vec<JoinHandle<()>>,
    /// The engine's snapshot: closed once the engine and all its tasks have stopped.
    engine: watch::Receiver<Snapshot>,
    /// Where this session's engine saves the mic levels.
    #[cfg_attr(not(test), allow(dead_code))]
    levels_file: PathBuf,
}

impl Session {
    fn route(&self) -> Route {
        Route { tx: self.tx.clone(), span: self.span.clone() }
    }
}

/// Everything a session is opened from, shared with the task that opens it.
struct Inner {
    real: Arc<dyn DeviceFactory>,
    sim: Arc<dyn DeviceFactory>,
    /// The real config, shared with the window.
    config: Arc<RwLock<Config>>,
    paths: Paths,
    out: Outputs,
}

pub struct Sessions {
    inner: Arc<Inner>,
    /// `None` only if a switch failed and the real equipment could not be reopened.
    current: tokio::sync::Mutex<Option<Session>>,
}

/// Switching is allowed only when nothing is happening.
fn may_switch(s: &Snapshot) -> bool {
    matches!(s.stage, Stage::Idle | Stage::Ended) && s.job.is_none()
}

fn span_for(mode: Mode) -> Span {
    match mode {
        Mode::Real => Span::none(),
        Mode::Simulation => simulation_span(),
    }
}

impl Sessions {
    /// Starts in real mode: builds the real devices and spawns the engine.
    pub async fn start(
        real: Arc<dyn DeviceFactory>,
        sim: Arc<dyn DeviceFactory>,
        config: Arc<RwLock<Config>>,
        paths: Paths,
    ) -> (Sessions, Front) {
        let (front_tx, front_rx) = mpsc::channel::<Command>(64);
        let (target, target_rx) = watch::channel(None);
        let (snap, snap_rx) = watch::channel(Snapshot::default());
        let (mode, mode_rx) = watch::channel(Mode::Real);
        let (sim_tx, sim_rx) = watch::channel(None);
        let (defaults, defaults_rx) = watch::channel(Vec::new());
        let (yt, yt_rx) = watch::channel(Absent::new("starting") as Arc<dyn Broadcasts>);
        let (devices, devices_rx) = watch::channel(None);
        let (switching, switching_rx) = watch::channel(false);
        let inflight = Arc::new(AtomicUsize::new(0));
        spawn_router(front_rx, target_rx, inflight.clone());
        let out = Outputs { target, inflight, snap, mode, sim: sim_tx, sim_defaults: defaults, yt, devices, switching };
        let inner = Arc::new(Inner { real, sim, config, paths, out });
        let first = Inner::open(inner.clone(), Mode::Real).await;
        let sessions = Sessions { inner, current: tokio::sync::Mutex::new(Some(first)) };
        let front = Front {
            tx: front_tx,
            rx: snap_rx,
            mode: mode_rx,
            sim: sim_rx,
            sim_defaults: defaults_rx,
            yt: yt_rx,
            devices: devices_rx,
            switching: switching_rx,
        };
        (sessions, front)
    }

    /// Switches to `to`. Refused unless the stage is Idle or Ended with no job running,
    /// checked again once every press already on its way has reached the engine.
    /// Stops the old session's engine, health poll and device tasks, then builds the new
    /// devices and spawns the new engine. Switching to the current mode does nothing.
    pub async fn switch(&self, to: Mode) -> Result<(), SwitchRefused> {
        let mut current = self.current.lock().await;
        let Some(old) = current.take() else {
            return Err(SwitchRefused(BROKEN.into()));
        };
        if old.mode == to {
            *current = Some(old);
            return Ok(());
        }
        // 1. Busy now: refuse without touching the gate.
        if !may_switch(&old.engine.borrow()) {
            info!(to = ?to, "switch refused: a service or sequence is under way");
            *current = Some(old);
            return Err(SwitchRefused(REFUSED.into()));
        }
        let out = &self.inner.out;
        // 2. Close the gate: from here front presses are dropped (and logged).
        out.target.send_replace(None);
        // 3. Let every press already on its way reach the engine. An engine still in its
        //    startup check is not reading commands yet: refuse rather than wait for it.
        if !drained(&out.inflight, &old.tx).await {
            info!(to = ?to, "switch refused: the engine is still in its startup check");
            out.target.send_replace(Some(old.route()));
            *current = Some(old);
            return Err(SwitchRefused(CHECKING.into()));
        }
        // 4. A late press that started something runs in the world it was made in.
        if !may_switch(&old.engine.borrow()) {
            info!(to = ?to, "switch refused: a press just before the switch started a sequence");
            out.target.send_replace(Some(old.route()));
            *current = Some(old);
            return Err(SwitchRefused(REFUSED.into()));
        }
        // 5. Nothing can reach the old engine now: stop it and open the new mode.
        info!(from = ?old.mode, to = ?to, "switching mode");
        out.switching.send_replace(true);
        self.inner.close(old).await;
        let result = match Inner::open_guarded(self.inner.clone(), to).await {
            Some(s) => {
                *current = Some(s);
                info!(mode = ?to, "mode switched");
                Ok(())
            }
            None if to == Mode::Real => Err(SwitchRefused(BROKEN.into())),
            None => match Inner::open_guarded(self.inner.clone(), Mode::Real).await {
                Some(s) => {
                    *current = Some(s);
                    Err(SwitchRefused(SIM_FAILED.into()))
                }
                None => Err(SwitchRefused(BROKEN.into())),
            },
        };
        out.switching.send_replace(false);
        result
    }

    /// "Crash & restart app" (simulation only): the engine stops as if the app had crashed
    /// (a sequence it was running is aborted part-way) and a new one starts on the same
    /// pretend devices, as they stand, with the normal read-only startup. Unlike a switch
    /// it does not wait for an idle moment. The gate discipline is the switch's: the gate
    /// closes, presses already on their way are let reach the old engine (and die with
    /// it), the engine is swapped and the gate reopens; presses meanwhile are dropped.
    pub async fn restart_engine(&self) -> Result<(), SwitchRefused> {
        let mut current = self.current.lock().await;
        let Some(old) = current.take() else {
            return Err(SwitchRefused(BROKEN.into()));
        };
        if old.mode != Mode::Simulation || old.kit.sim.is_none() {
            *current = Some(old);
            return Err(SwitchRefused(RESTART_REAL.into()));
        }
        let out = &self.inner.out;
        out.target.send_replace(None);
        if !drained(&out.inflight, &old.tx).await {
            info!("crash & restart: the engine is still in its startup check; restarting anyway");
        }
        out.switching.send_replace(true);
        let Session { mode, kit, tx, span, tasks, engine, .. } = old;
        span.in_scope(|| warn!("crash & restart: stopping the engine as if StreamIt had crashed"));
        stop_engine(out, mode, tx, tasks, engine).await;
        // The pretend devices are all there from the start, as SimFactory builds them.
        let probes = Probes::ready(kit.devices.clone());
        let hub_output = kit.config.read().unwrap_or_else(|e| e.into_inner()).videohub.input8_output;
        let health = probes.poll(hub_output).instrument(span.clone()).await;
        let s = Inner::start(&self.inner, mode, span, kit, health, Some(probes)).await;
        s.span.in_scope(|| info!("crash & restart: the engine started again"));
        *current = Some(s);
        out.switching.send_replace(false);
        Ok(())
    }

    /// The levels file the current engine saves to.
    #[cfg(test)]
    pub async fn levels_file(&self) -> PathBuf {
        self.current.lock().await.as_ref().expect("open").levels_file.clone()
    }
}

/// Waits until no front press is between the router and the engine and the engine has
/// taken every queued command, then a little longer so it has acted on the last one.
/// False if that takes longer than `DRAIN_WAIT` (the engine is in its startup check).
async fn drained(inflight: &AtomicUsize, engine: &mpsc::Sender<Command>) -> bool {
    let quiet = || inflight.load(Ordering::SeqCst) == 0 && engine.capacity() == engine.max_capacity();
    let settle = async {
        loop {
            if quiet() {
                tokio::task::yield_now().await;
                tokio::time::sleep(SETTLE).await;
                if quiet() {
                    return;
                }
            } else {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    };
    tokio::time::timeout(DRAIN_WAIT, settle).await.is_ok()
}

impl Inner {
    /// `open` in a task of its own, so a factory that panics leaves the app able to
    /// recover. `None` if it failed (and nothing of it is left running).
    async fn open_guarded(inner: Arc<Inner>, mode: Mode) -> Option<Session> {
        match tokio::spawn(Inner::open(inner, mode)).await {
            Ok(s) => Some(s),
            Err(e) => {
                error!(mode = ?mode, "could not open {mode:?} mode: {e}");
                None
            }
        }
    }

    /// Builds `mode`'s devices and starts its engine, snapshot log and health poll, then
    /// points the front at it.
    async fn open(inner: Arc<Inner>, mode: Mode) -> Session {
        let span = span_for(mode);
        let real = inner.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        let factory = match mode {
            Mode::Real => &inner.real,
            Mode::Simulation => &inner.sim,
        };
        let built = factory.build(&real).instrument(span.clone()).await;
        let config = built.config.unwrap_or_else(|| inner.config.clone());
        let kit = Kit {
            devices: built.devices,
            config,
            sim: built.sim,
            sim_defaults: built.sim_defaults,
            device_tasks: built.tasks,
        };
        Inner::start(&inner, mode, span, kit, built.health, built.probes).await
    }

    /// Starts an engine on `kit` (with its snapshot log and health poll) and points the
    /// front at it. The engine's startup is the normal read-only check.
    async fn start(
        inner: &Inner,
        mode: Mode,
        span: Span,
        kit: Kit,
        health: Vec<(DeviceKind, DeviceHealth)>,
        probes: Option<Probes>,
    ) -> Session {
        let config = kit.config.clone();
        let levels_file = inner.paths.levels_for(mode).to_path_buf();
        let (engine, tx, mut tasks) = {
            let _in = span.enter();
            // The mics' live levels from the last run of this mode, so Go Live and Recover
            // after a restart fade up to them rather than to 0 dB.
            let levels = LiveLevels::load(&levels_file).unwrap_or_else(|e| {
                warn!("{e:#}; starting without saved stream audio levels");
                LiveLevels::default()
            });
            info!(path = %levels_file.display(), mics = levels.0.len(), "stream audio levels loaded");
            let engine = Engine::spawn_with(
                kit.devices.clone(),
                config.clone(),
                levels,
                Some(levels_file.clone()),
                mode == Mode::Simulation,
            );
            let snap_log = spawn_snapshot_log(engine.rx.clone());
            (engine.rx, engine.tx, vec![snap_log])
        };
        for (kind, h) in &health {
            let _ = tx.send(Command::DeviceHealth(*kind, h.clone())).await;
        }
        if let Some(probes) = probes {
            let _in = span.enter();
            tasks.push(wiring::spawn_health_poll(probes, config, tx.clone(), engine.clone(), &health));
        }
        let out = &inner.out;
        tasks.push(spawn_forward(engine.clone(), out.snap.clone()));
        out.yt.send_replace(kit.devices.yt.clone());
        out.devices.send_replace(Some(kit.devices.clone()));
        out.sim.send_replace(kit.sim.clone());
        out.sim_defaults.send_replace(kit.sim_defaults.clone());
        out.mode.send_replace(mode);
        let session = Session { mode, kit, tx, span, tasks, engine, levels_file };
        // Opening the gate also tells the new engine the Stream Deck's health (the router
        // remembers it; the deck reports only changes).
        out.target.send_replace(Some(session.route()));
        session
    }

    /// Stops `s` completely (the gate is already closed, so no command reaches it): its
    /// snapshot log, forwarding and health poll stop, the front lets go of its devices,
    /// its engine stops once its last command sender is dropped, and its device tasks
    /// stop. The real clients' reconnect loops end when their last handle (held by the
    /// engine, the poll and the front) is dropped; the pretend player's end timer ends
    /// when the world is reset.
    async fn close(&self, s: Session) {
        let Session { mode, kit, tx, tasks, engine, .. } = s;
        self.out.target.send_replace(None);
        self.out.yt.send_replace(Absent::new("switching mode"));
        self.out.devices.send_replace(None);
        self.out.sim.send_replace(None);
        for t in &kit.device_tasks {
            t.abort();
        }
        if let Some(sim) = &kit.sim {
            sim.reset();
        }
        drop(kit);
        stop_engine(&self.out, mode, tx, tasks, engine).await;
    }
}

/// Stops an engine (the gate already closed, so no command reaches it): its snapshot
/// log, forwarding and health poll stop, its command channel closes, and the engine
/// aborts any sequence it was running. Waits up to `STOP_WAIT` for the engine and its
/// side tasks to let go of the devices.
async fn stop_engine(
    out: &Outputs,
    mode: Mode,
    tx: mpsc::Sender<Command>,
    tasks: Vec<JoinHandle<()>>,
    mut engine: watch::Receiver<Snapshot>,
) {
    out.target.send_replace(None);
    drop(tx);
    for t in &tasks {
        t.abort();
    }
    for t in tasks {
        let _ = t.await;
    }
    let stopped = tokio::time::timeout(STOP_WAIT, async { while engine.changed().await.is_ok() {} }).await;
    if stopped.is_err() {
        error!(
            mode = ?mode,
            "the old engine has not stopped within {} s: its side tasks keep the old devices until they finish",
            STOP_WAIT.as_secs()
        );
    }
}

/// Passes each front command to the current engine, logging it in that session's span;
/// while the gate is closed, drops it. Remembers the Stream Deck's last health and tells
/// each newly opened engine.
fn spawn_router(
    mut rx: mpsc::Receiver<Command>,
    mut target: watch::Receiver<Option<Route>>,
    inflight: Arc<AtomicUsize>,
) {
    tokio::spawn(async move {
        let mut deck: Option<DeviceHealth> = None;
        loop {
            tokio::select! {
                changed = target.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let open = target.borrow_and_update().is_some();
                    if let (true, Some(h)) = (open, deck.clone()) {
                        deliver(&target, &inflight, Command::DeviceHealth(DeviceKind::StreamDeck, h)).await;
                    }
                }
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { return };
                    if let Command::DeviceHealth(DeviceKind::StreamDeck, h) = &cmd {
                        deck = Some(h.clone());
                    }
                    deliver(&target, &inflight, cmd).await;
                }
            }
        }
    });
}

/// Hands `cmd` to the engine behind the gate. Counted in `inflight` from before the gate
/// is read until the engine has it, so a switch can wait for it.
async fn deliver(target: &watch::Receiver<Option<Route>>, inflight: &AtomicUsize, cmd: Command) {
    inflight.fetch_add(1, Ordering::SeqCst);
    let route = target.borrow().clone();
    match route {
        Some(route) => {
            route.span.in_scope(|| log_command(&cmd));
            if route.tx.send(cmd).await.is_err() {
                route.span.in_scope(|| warn!("the engine has stopped; command dropped"));
            }
        }
        None => info!(command = ?cmd, "switching mode: command dropped"),
    }
    inflight.fetch_sub(1, Ordering::SeqCst);
}

/// Copies the session's snapshots to the front.
fn spawn_forward(mut engine: watch::Receiver<Snapshot>, front: watch::Sender<Snapshot>) -> JoinHandle<()> {
    tokio::spawn(async move {
        front.send_replace(engine.borrow_and_update().clone());
        while engine.changed().await.is_ok() {
            front.send_replace(engine.borrow_and_update().clone());
        }
    })
}

/// Every command from the window and the deck is logged before it reaches the engine.
fn log_command(cmd: &Command) {
    match cmd {
        // Health changes are logged by the poll (and the deck) with their reason.
        Command::DeviceHealth(..) => {}
        Command::SetCues(cues) => {
            let names: Vec<&str> = cues.iter().map(|c| c.name.as_str()).collect();
            info!(cues = ?names, "command SetCues");
        }
        other => info!(command = ?other, "command"),
    }
}

/// The stage without its ticking clock, so the log has one line per real change.
fn stage_label(s: &Stage) -> String {
    match s {
        Stage::Countdown { public, .. } => format!("Countdown (public: {public})"),
        Stage::Ending { .. } => "Ending".into(),
        other => format!("{other:?}"),
    }
}

/// Logs stage changes, offers and every new engine warning.
fn spawn_snapshot_log(mut rx: watch::Receiver<Snapshot>) -> JoinHandle<()> {
    tokio::spawn(
        async move {
            let mut last = rx.borrow().clone();
            while rx.changed().await.is_ok() {
                let s = rx.borrow_and_update().clone();
                if stage_label(&s.stage) != stage_label(&last.stage) {
                    info!(stage = %stage_label(&s.stage), "stage");
                }
                if s.last_warning != last.last_warning
                    && let Some(w) = &s.last_warning
                {
                    warn!(warning = %w, "engine warning");
                }
                if s.offers != last.offers {
                    info!(offers = ?s.offers, "offers");
                }
                if s.playing != last.playing {
                    info!(playing = ?s.playing, "video");
                }
                last = s;
            }
        }
        .in_current_span(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Mutex, Weak};
    use streamit_core::audio::LiveLevels;
    use streamit_core::config::AudioSourceId;
    use streamit_core::devices::StreamSwitcher;
    use streamit_core::fakes::{CallLog, fake_devices};
    use streamit_core::golive::Stage;
    use streamit_core::recovery::Offer;
    use streamit_sim::{Input8Source, SEED_BROADCAST};
    use tokio::time::Instant;

    const WAIT: Duration = Duration::from_secs(120);

    /// A factory of fake devices that counts its builds, keeps the last build's call log
    /// and a weak reference to its Stream ATEM, and panics if called once armed.
    #[derive(Default)]
    struct Spy {
        builds: AtomicUsize,
        armed: AtomicBool,
        last_log: Mutex<Option<CallLog>>,
        last_stream: Mutex<Option<Weak<dyn StreamSwitcher>>>,
        /// Each build's YouTube answers `upcoming` this late, holding the engine in its
        /// startup check.
        slow_startup: Mutex<Option<Duration>>,
    }

    impl Spy {
        fn builds(&self) -> usize {
            self.builds.load(Ordering::SeqCst)
        }
        fn arm(&self) {
            self.armed.store(true, Ordering::SeqCst);
        }
        fn calls(&self) -> Vec<streamit_core::fakes::Call> {
            self.last_log.lock().unwrap().as_ref().unwrap().calls()
        }
    }

    #[async_trait]
    impl DeviceFactory for Spy {
        async fn build(&self, _c: &Config) -> Built {
            assert!(!self.armed.load(Ordering::SeqCst), "this factory must not be called now");
            self.builds.fetch_add(1, Ordering::SeqCst);
            let (devices, log, h) = fake_devices();
            if let Some(d) = *self.slow_startup.lock().unwrap() {
                h.yt.set_delay("upcoming", d);
            }
            *self.last_log.lock().unwrap() = Some(log);
            *self.last_stream.lock().unwrap() = Some(Arc::downgrade(&devices.stream));
            Built::plain(devices)
        }
    }

    /// A factory that takes `.0` to build.
    struct Slow<F>(Duration, F);

    #[async_trait]
    impl<F: DeviceFactory> DeviceFactory for Slow<F> {
        async fn build(&self, c: &Config) -> Built {
            tokio::time::sleep(self.0).await;
            self.1.build(c).await
        }
    }

    /// A factory that fails.
    struct Panics;

    #[async_trait]
    impl DeviceFactory for Panics {
        async fn build(&self, _c: &Config) -> Built {
            panic!("this factory fails");
        }
    }

    fn paths(name: &str) -> (PathBuf, Paths) {
        let dir = std::env::temp_dir().join(format!("streamit-session-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.toml");
        let p = Paths::beside(&config);
        (config, p)
    }

    async fn start_with(
        name: &str,
        real: Arc<dyn DeviceFactory>,
        sim: Arc<dyn DeviceFactory>,
    ) -> (Sessions, Front, PathBuf, Paths) {
        start_cfg(name, real, sim, Config::default()).await
    }

    async fn start_cfg(
        name: &str,
        real: Arc<dyn DeviceFactory>,
        sim: Arc<dyn DeviceFactory>,
        c: Config,
    ) -> (Sessions, Front, PathBuf, Paths) {
        let (config, p) = paths(name);
        let shared = Arc::new(RwLock::new(c));
        let (s, f) = Sessions::start(real, sim, shared, p.clone()).await;
        (s, f, config, p)
    }

    async fn wait_for(front: &Front, what: &str, f: impl FnMut(&Snapshot) -> bool) -> Snapshot {
        let mut rx = front.rx.clone();
        match tokio::time::timeout(WAIT, rx.wait_for(f)).await {
            Ok(Ok(s)) => s.clone(),
            _ => panic!("timed out waiting for {what}: {:?}", front.rx.borrow()),
        }
    }

    fn refused() -> Result<(), SwitchRefused> {
        Err(SwitchRefused("Finish or end the service first".into()))
    }

    /// The whole simulation session (its command log, engine, sequence tasks, device
    /// calls and health poll) logs to the simulation log only.
    #[tokio::test(start_paused = true)]
    async fn simulation_session_logs_to_the_sim_log_only() {
        use crate::logging::{file_layers, tests::Buf};
        use tracing_subscriber::layer::SubscriberExt;
        let (real, sim) = (Buf::default(), Buf::default());
        let (r, m) = (real.clone(), sim.clone());
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("info"))
            .with(file_layers(move || r.clone(), move || m.clone()));
        // The test runtime has one thread, so every task logs through this subscriber.
        let _default = tracing::subscriber::set_default(subscriber);
        // Callsite interest is global, and while only one dispatcher is registered
        // tracing-core works it out from the *calling thread's* default alone. A callsite
        // first hit by a parallel test (whose thread has no subscriber) would then be
        // cached as "never" and this test's events from it lost. A second, do-nothing
        // dispatcher kept alive makes every registration ask all registered dispatchers
        // (this one included); then every callsite's interest is recomputed.
        let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        tokio::task::yield_now().await;
        tracing::callsite::rebuild_interest_cache();

        let (s, front, ..) = start_with("simlog", Arc::new(Spy::default()), Arc::new(SimFactory)).await;
        front.tx.send(Command::Refresh).await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        s.switch(Mode::Simulation).await.unwrap();
        front.tx.send(Command::Prepare).await.unwrap();
        wait_for(&front, "Prepare to start", |s| s.job.is_some()).await;
        wait_for(&front, "Prepare to finish", |s| s.job.is_none()).await;
        tokio::time::sleep(Duration::from_secs(5)).await; // a health poll or two

        let (real, sim) = (real.text(), sim.text());
        assert!(real.contains("command=Refresh"), "{real}");
        assert!(real.contains("switching mode"), "{real}");
        assert!(sim.contains("command=Prepare"), "the command log: {sim}");
        assert!(sim.contains("device call"), "the sequence's device calls: {sim}");
        assert!(sim.contains("stage"), "the snapshot log: {sim}");
        for line in ["command=Prepare", "device call", "simulated defaults"] {
            assert!(!real.contains(line), "{line} leaked to the real log: {real}");
        }
        assert!(!sim.contains("command=Refresh"), "{sim}");
    }

    #[tokio::test(start_paused = true)]
    async fn simulation_builds_no_real_devices() {
        let real = Arc::new(Spy::default());
        // The real SimFactory: it builds only streamit_sim's pretend devices (SimControl)
        // and never calls wiring::build, discovery or any client's connect, so the only
        // way a real client could be built here is through the real factory, armed below.
        let (s, front, ..) = start_with("nodev", real.clone(), Arc::new(SimFactory)).await;
        assert_eq!(real.builds(), 1, "the app starts in real mode");
        assert_eq!(*front.mode.borrow(), Mode::Real);
        assert!(front.sim.borrow().is_none());
        real.arm(); // from here a call to the real factory panics

        s.switch(Mode::Simulation).await.unwrap();
        assert_eq!(*front.mode.borrow(), Mode::Simulation);
        assert!(front.sim.borrow().is_some(), "the Simulation tab gets its handle");
        wait_for(&front, "a simulation snapshot", |s| s.simulation).await;
        // The window's YouTube is the pretend one.
        let yt = front.yt.borrow().clone();
        let up = yt.upcoming().await.unwrap();
        assert_eq!(up[0].id, SEED_BROADCAST);
        assert_eq!(real.builds(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn switch_refused_while_job_runs() {
        let sim = Arc::new(Spy::default());
        sim.arm();
        // A video in Idle: the stage stays Idle, so only the running job refuses the switch.
        // (The fake player plays until told it ended.)
        let mut c = Config::default();
        c.videohub.input8_output = 12;
        c.videohub.decklink_input = 40;
        c.cues = vec![streamit_core::rollin::Cue {
            path: PathBuf::from("v.mp4"),
            name: "v".into(),
            destination: streamit_core::rollin::Destination::AudioOnly,
            gain_db: 0.0,
            return_input: None,
        }];
        let (s, front, ..) = start_cfg("job", Arc::new(Spy::default()), sim.clone(), c).await;
        front.tx.send(Command::PlayCue(0)).await.unwrap();
        wait_for(&front, "the video to run", |s| s.job.is_some()).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let at = front.rx.borrow().clone();
        assert_eq!((at.stage.clone(), at.job), (Stage::Idle, Some("a video")));
        assert_eq!(s.switch(Mode::Simulation).await, refused());
        assert_eq!(*front.mode.borrow(), Mode::Real);
        assert_eq!(sim.builds(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn switch_refused_when_live() {
        let sim = Arc::new(Spy::default());
        sim.arm();
        let (s, front, ..) = start_with("live", Arc::new(Spy::default()), sim.clone()).await;
        front.tx.send(Command::GoLive).await.unwrap();
        wait_for(&front, "live with no job", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert_eq!(s.switch(Mode::Simulation).await, refused());
        assert_eq!(*front.mode.borrow(), Mode::Real);
    }

    #[tokio::test(start_paused = true)]
    async fn switch_back_to_real_rebuilds_real_devices_read_only() {
        let real = Arc::new(Spy::default());
        let (s, front, ..) = start_with("back", real.clone(), Arc::new(SimFactory)).await;
        s.switch(Mode::Simulation).await.unwrap();
        wait_for(&front, "simulation", |s| s.simulation).await;
        s.switch(Mode::Real).await.unwrap();
        assert_eq!(real.builds(), 2, "switching back builds the real devices again");
        assert_eq!(*front.mode.borrow(), Mode::Real);
        assert!(front.sim.borrow().is_none());
        assert!(front.sim_defaults.borrow().is_empty());
        let snap = wait_for(&front, "a real snapshot", |s| !s.simulation).await;
        assert_eq!(snap.stage, Stage::Idle);
        // Let the startup check run to the end: it only reads.
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(real.calls(), vec![], "the normal read-only startup changes nothing");
    }

    #[tokio::test(start_paused = true)]
    async fn front_survives_switch() {
        let (s, front, ..) = start_with("front", Arc::new(Spy::default()), Arc::new(SimFactory)).await;
        // What the deck holds: copies made before the switch.
        let (deck_tx, deck_rx) = (front.tx.clone(), front.rx.clone());
        s.switch(Mode::Simulation).await.unwrap();
        deck_tx.send(Command::Prepare).await.unwrap();
        let mut rx = deck_rx;
        let got = tokio::time::timeout(WAIT, rx.wait_for(|s| s.simulation && s.job == Some("Prepare"))).await;
        assert!(got.is_ok(), "the new engine ran the command: {:?}", front.rx.borrow());
    }

    #[tokio::test(start_paused = true)]
    async fn deck_health_carries_over_a_switch() {
        let (s, front, ..) = start_with("deck", Arc::new(Spy::default()), Arc::new(SimFactory)).await;
        front.tx.send(Command::DeviceHealth(DeviceKind::StreamDeck, DeviceHealth::Ok)).await.unwrap();
        wait_for(&front, "deck health", |s| s.device_status.contains_key(&DeviceKind::StreamDeck)).await;
        s.switch(Mode::Simulation).await.unwrap();
        wait_for(&front, "deck health in simulation", |s| {
            s.simulation && s.device_status.get(&DeviceKind::StreamDeck) == Some(&DeviceHealth::Ok)
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_a_session_drops_its_devices() {
        let real = Arc::new(Spy::default());
        let (s, front, ..) = start_with("drop", real.clone(), Arc::new(SimFactory)).await;
        let _ = &front;
        tokio::time::sleep(Duration::from_secs(30)).await; // the startup check is done
        let weak = real.last_stream.lock().unwrap().clone().unwrap();
        assert!(weak.upgrade().is_some());
        s.switch(Mode::Simulation).await.unwrap();
        // The real clients' reconnect loops end when their last handle drops.
        assert!(weak.upgrade().is_none(), "the old session's devices are all released");
    }

    /// A press already on its way to the real engine (queued behind its slow startup
    /// check) must never run on the real equipment once the mode reads Simulation.
    #[tokio::test(start_paused = true)]
    async fn prepare_sent_just_before_switch_never_runs_on_real_after_mode_reads_simulation() {
        let real = Arc::new(Spy::default());
        *real.slow_startup.lock().unwrap() = Some(Duration::from_secs(8));
        let (s, front, ..) = start_with("race", real.clone(), Arc::new(SimFactory)).await;
        front.tx.send(Command::Prepare).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await; // queued behind the startup check
        let r = s.switch(Mode::Simulation).await;
        let mode = *front.mode.borrow();
        tokio::time::sleep(Duration::from_secs(60)).await;
        let ran_on_real = !real.calls().is_empty();
        match r {
            Ok(()) => {
                assert_eq!(mode, Mode::Simulation);
                assert!(!ran_on_real, "the press ran on the real equipment after the switch: {:?}", real.calls());
            }
            Err(e) => {
                assert_eq!(e.0, CHECKING);
                assert_eq!(*front.mode.borrow(), Mode::Real);
                assert!(ran_on_real, "refused: the press runs in the world it was made in");
            }
        }
    }

    /// A press still waiting in the front's queue when the gate closes is dropped: it
    /// reaches neither the old engine nor the new one.
    #[tokio::test(start_paused = true)]
    async fn press_queued_at_the_gate_is_dropped() {
        let real = Arc::new(Spy::default());
        let (s, front, ..) = start_with("gate", real.clone(), Arc::new(SimFactory)).await;
        tokio::time::sleep(Duration::from_secs(30)).await; // past the startup check
        // One thread: the router has not run between these two lines.
        front.tx.send(Command::Prepare).await.unwrap();
        s.switch(Mode::Simulation).await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(real.calls(), vec![], "nothing ran on the real equipment");
        let snap = front.rx.borrow().clone();
        assert!(snap.simulation);
        assert_eq!((snap.stage, snap.job), (Stage::Idle, None), "nor in the simulation");
    }

    #[tokio::test(start_paused = true)]
    async fn command_sent_during_switch_is_dropped_not_delivered_to_new_engine() {
        let slow_sim = Arc::new(Slow(Duration::from_secs(3), SimFactory));
        let (s, front, ..) = start_with("during", Arc::new(Spy::default()), slow_sim).await;
        let s = Arc::new(s);
        let sw = tokio::spawn({
            let s = s.clone();
            async move { s.switch(Mode::Simulation).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(*front.switching.borrow(), "the window can show the switch");
        front.tx.send(Command::Prepare).await.unwrap();
        sw.await.unwrap().unwrap();
        assert!(!*front.switching.borrow());
        tokio::time::sleep(Duration::from_secs(30)).await;
        let snap = front.rx.borrow().clone();
        assert!(snap.simulation);
        assert_eq!((snap.stage, snap.job), (Stage::Idle, None), "the press was dropped");
    }

    #[tokio::test(start_paused = true)]
    async fn deck_health_reported_during_a_switch_reaches_the_new_engine() {
        let slow_sim = Arc::new(Slow(Duration::from_secs(3), SimFactory));
        let (s, front, ..) = start_with("deckmid", Arc::new(Spy::default()), slow_sim).await;
        front.tx.send(Command::DeviceHealth(DeviceKind::StreamDeck, DeviceHealth::Ok)).await.unwrap();
        let s = Arc::new(s);
        let sw = tokio::spawn({
            let s = s.clone();
            async move { s.switch(Mode::Simulation).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        let unplugged = DeviceHealth::Down("unplugged".into());
        front.tx.send(Command::DeviceHealth(DeviceKind::StreamDeck, unplugged.clone())).await.unwrap();
        sw.await.unwrap().unwrap();
        wait_for(&front, "the deck's latest health", |s| {
            s.simulation && s.device_status.get(&DeviceKind::StreamDeck) == Some(&unplugged)
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn failed_open_falls_back_to_real() {
        let real = Arc::new(Spy::default());
        let (s, front, ..) = start_with("fail", real.clone(), Arc::new(Panics)).await;
        let r = s.switch(Mode::Simulation).await;
        assert!(r.is_err(), "{r:?}");
        assert_eq!(*front.mode.borrow(), Mode::Real);
        assert!(!*front.switching.borrow());
        assert_eq!(real.builds(), 2, "the real equipment was reopened");
        // Still usable.
        front.tx.send(Command::Prepare).await.unwrap();
        wait_for(&front, "Prepare on real", |s| !s.simulation && s.job.is_some()).await;
    }

    #[test]
    fn sim_levels_live_beside_the_config() {
        let p = Paths::beside(Path::new("C:/x/StreamIt/config.toml"));
        assert_eq!(p.levels, Path::new("C:/x/StreamIt/levels.toml"));
        assert_eq!(p.sim_levels, Path::new("C:/x/StreamIt/sim-levels.toml"));
        assert_eq!(p.levels_for(Mode::Real), p.levels.as_path());
        assert_eq!(p.levels_for(Mode::Simulation), p.sim_levels.as_path());
    }

    #[tokio::test(start_paused = true)]
    async fn sim_uses_sim_levels_file() {
        let (s, front, _config, p) = start_with("levels", Arc::new(Spy::default()), Arc::new(SimFactory)).await;
        let mic = AudioSourceId { input: 1301, source: -65280 };
        let real_levels = LiveLevels([(mic, -7.5)].into_iter().collect());
        real_levels.save(&p.levels).unwrap();
        let before = std::fs::read_to_string(&p.levels).unwrap();
        s.switch(Mode::Simulation).await.unwrap();
        assert_eq!(s.levels_file().await, p.sim_levels);
        // Prepare recaptures the (pretend) mic levels and saves them.
        front.tx.send(Command::Prepare).await.unwrap();
        wait_for(&front, "Prepare to start", |s| s.job.is_some()).await;
        wait_for(&front, "Prepare to finish", |s| s.job.is_none()).await;
        assert!(p.sim_levels.is_file(), "the pretend levels went to sim-levels.toml");
        assert_eq!(std::fs::read_to_string(&p.levels).unwrap(), before, "levels.toml untouched");
        // Prepared is a service under way: no switching back now.
        assert_eq!(s.switch(Mode::Real).await, refused());
    }

    #[test]
    fn sim_config_fills_only_unset_numbers() {
        let mut c = Config::default();
        c.videohub.input8_output = 0;
        c.videohub.decklink_input = 0;
        c.videohub.fallback_normal_input = 0;
        let (e, d) = sim_config(&c);
        assert_eq!(
            (e.videohub.input8_output, e.videohub.decklink_input, e.videohub.fallback_normal_input),
            (11, 39, 13)
        );
        assert_eq!(d, vec!["videohub.input8_output", "videohub.decklink_input", "videohub.fallback_normal_input"]);
        // The pretend broadcast is bound to the pretend stream, so the effective config
        // names that stream, whatever the real one is (never saved).
        c.youtube.stream_id = Some("real-stream".into());
        assert_eq!(sim_config(&c).0.youtube.stream_id.as_deref(), Some(streamit_sim::SEED_STREAM));
        c.videohub.input8_output = 4;
        c.videohub.fallback_normal_input = 5;
        let (e, d) = sim_config(&c);
        assert_eq!((e.videohub.input8_output, e.videohub.decklink_input, e.videohub.fallback_normal_input), (4, 39, 5));
        assert_eq!(d, vec!["videohub.decklink_input"]);
    }

    #[tokio::test(start_paused = true)]
    async fn sim_fills_unset_videohub_numbers_without_saving() {
        let (config, p) = paths("simdefaults");
        let c = Config::default();
        assert_eq!(c.videohub.input8_output, 0, "the defaults leave the VideoHub numbers unset");
        c.save(&config).unwrap();
        let saved = std::fs::read(&config).unwrap();
        let shared = Arc::new(RwLock::new(c));
        let (s, front) = Sessions::start(Arc::new(Spy::default()), Arc::new(SimFactory), shared.clone(), p).await;
        s.switch(Mode::Simulation).await.unwrap();
        assert_eq!(
            *front.sim_defaults.borrow(),
            vec!["videohub.input8_output", "videohub.decklink_input", "videohub.fallback_normal_input"]
        );
        // The pretend world was seeded from the effective config: output 11 carries input 13.
        let sim = front.sim.borrow().clone().unwrap();
        assert_eq!(sim.snapshot().input8, Input8Source::Other(13));
        // Settings and the Simulation tab read the running simulation's effective config.
        let e = &sim.config().videohub;
        assert_eq!((e.input8_output, e.decklink_input, e.fallback_normal_input), (11, 39, 13));
        // Nothing was saved, and the real config is unchanged.
        assert_eq!(std::fs::read(&config).unwrap(), saved);
        let real = shared.read().unwrap().videohub.clone();
        assert_eq!((real.input8_output, real.decklink_input, real.fallback_normal_input), (0, 0, 0));
    }

    /// "Crash & restart app": the engine restarts on the pretend world as it stands (not
    /// reset), and its normal read-only startup sees the DeckLink on air and offers Recover.
    #[tokio::test(start_paused = true)]
    async fn restart_engine_keeps_sim_world_and_reassesses() {
        let (s, front, ..) = start_with("restart", Arc::new(Spy::default()), Arc::new(SimFactory)).await;
        s.switch(Mode::Simulation).await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await; // past the startup check
        let snap = front.rx.borrow().clone();
        assert!(snap.simulation && !snap.offers.contains(&Offer::Recover), "{snap:?}");
        let sim = front.sim.borrow().clone().unwrap();
        let decklink = Config::default().stream_atem.decklink_input;
        sim.set_program(decklink); // the video was on the stream when the app "crashed"
        s.restart_engine().await.unwrap();
        assert_eq!(*front.mode.borrow(), Mode::Simulation);
        assert!(!*front.switching.borrow());
        let after = front.sim.borrow().clone().expect("the Simulation tab keeps its handle");
        assert_eq!(after.snapshot().stream_program, decklink, "the pretend world was not reset");
        let snap = wait_for(&front, "Recover offered", |s| s.simulation && s.offers.contains(&Offer::Recover)).await;
        assert_eq!((snap.stage, snap.job), (Stage::Idle, None));
        // The new engine takes presses.
        front.tx.send(Command::Recover).await.unwrap();
        wait_for(&front, "Recover to run", |s| s.job == Some("Recover")).await;
    }

    /// I3: "Crash & restart app" after going public finds the pretend broadcast again
    /// (bound to the simulation's own stream), so the restart lands on Live, not Idle.
    #[tokio::test(start_paused = true)]
    async fn restart_after_going_public_keeps_the_broadcast() {
        let (s, front, ..) = start_with("restartpublic", Arc::new(Spy::default()), Arc::new(SimFactory)).await;
        s.switch(Mode::Simulation).await.unwrap();
        wait_for(&front, "the pretend broadcast", |s| s.simulation && s.broadcast.is_some()).await;
        let sim = front.sim.borrow().clone().unwrap();
        sim.set_speed(Speed::X10);
        front.tx.send(Command::Prepare).await.unwrap();
        wait_for(&front, "Prepared", |s| matches!(s.stage, Stage::Prepared(_)) && s.job.is_none()).await;
        front.tx.send(Command::GoLive).await.unwrap();
        wait_for(&front, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert_eq!(sim.snapshot().yt_life_cycle.as_deref(), Some("live"));
        s.restart_engine().await.unwrap();
        let snap = wait_for(&front, "Live again", |s| s.simulation && s.stage == Stage::Live).await;
        assert_eq!(snap.broadcast.as_ref().map(|b| b.id.as_str()), Some(SEED_BROADCAST), "{snap:?}");
        tokio::time::sleep(Duration::from_secs(10)).await; // the startup check has finished
        let snap = front.rx.borrow().clone();
        assert_eq!((snap.stage.clone(), snap.job), (Stage::Live, None));
        assert!(snap.offers.is_empty(), "live, public and on a camera: nothing to offer: {:?}", snap.offers);
        assert!(!snap.mics_ducked);
    }

    /// I4: the speed scales the closing hold (and the VideoHub settle and mic fade) the
    /// simulation engine waits on: at 10× the 10 s hold takes 1 s.
    #[tokio::test(start_paused = true)]
    async fn speed_scales_the_closing_hold_and_settle() {
        let (s, front, ..) = start_with("holdspeed", Arc::new(Spy::default()), Arc::new(SimFactory)).await;
        s.switch(Mode::Simulation).await.unwrap();
        wait_for(&front, "the pretend broadcast", |s| s.simulation && s.broadcast.is_some()).await;
        let sim = front.sim.borrow().clone().unwrap();
        sim.set_speed(Speed::X10);
        let real = Config::default();
        assert_eq!(real.timings.closing_hold, Duration::from_secs(10));
        let shared = sim.shared_config();
        assert_eq!(shared.read().unwrap().videohub.settle, Duration::from_millis(150));
        assert_eq!(sim.config().videohub.settle, real.videohub.settle, "Settings shows the real value");
        front.tx.send(Command::Prepare).await.unwrap();
        wait_for(&front, "Prepared", |s| matches!(s.stage, Stage::Prepared(_)) && s.job.is_none()).await;
        front.tx.send(Command::GoLive).await.unwrap();
        wait_for(&front, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        front.tx.send(Command::EndService).await.unwrap();
        wait_for(&front, "the closing hold", |s| matches!(s.stage, Stage::Ending { .. })).await;
        let hold = Instant::now();
        wait_for(&front, "Ended", |s| s.stage == Stage::Ended).await;
        let took = hold.elapsed();
        // The hold, then pretend YouTube's last transition (0.25 s at 10×).
        assert!(took >= Duration::from_secs(1) && took < Duration::from_secs(2), "{took:?}");
    }

    /// A video running when the app "crashes" is aborted: it never runs its return path.
    #[tokio::test(start_paused = true)]
    async fn restart_engine_aborts_the_running_job() {
        let c = Config {
            cues: vec![streamit_core::rollin::Cue {
                path: PathBuf::from("missing.mp4"), // unreadable: plays for 30 s
                name: "v".into(),
                destination: streamit_core::rollin::Destination::OnlineOnly,
                gain_db: 0.0,
                return_input: None,
            }],
            ..Config::default()
        };
        let (s, front, ..) = start_cfg("restartjob", Arc::new(Spy::default()), Arc::new(SimFactory), c.clone()).await;
        s.switch(Mode::Simulation).await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
        front.tx.send(Command::PlayCue(0)).await.unwrap();
        wait_for(&front, "the video to play", |s| s.playing == Some(0)).await;
        let sim = front.sim.borrow().clone().unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await; // the pretend mix to input 8
        assert_eq!(sim.snapshot().stream_program, c.stream_atem.decklink_input, "the video is on the stream");
        assert_eq!(front.rx.borrow().job, Some("a video"));
        s.restart_engine().await.unwrap();
        let snap = wait_for(&front, "Recover offered", |s| s.offers.contains(&Offer::Recover)).await;
        assert_eq!((snap.job, snap.playing), (None, None));
        tokio::time::sleep(Duration::from_secs(60)).await; // past the video's end
        assert_eq!(
            sim.snapshot().stream_program,
            c.stream_atem.decklink_input,
            "the aborted video never cut back to the camera"
        );
        assert_eq!(front.rx.borrow().job, None);
    }

    /// A press queued at the gate when the restart begins reaches neither engine.
    #[tokio::test(start_paused = true)]
    async fn press_at_restart_reaches_no_engine() {
        let (s, front, ..) = start_with("restartgate", Arc::new(Spy::default()), Arc::new(SimFactory)).await;
        s.switch(Mode::Simulation).await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
        // One thread: the router has not run between these two lines.
        front.tx.send(Command::Prepare).await.unwrap();
        s.restart_engine().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
        let snap = front.rx.borrow().clone();
        assert!(snap.simulation);
        assert_eq!((snap.stage, snap.job), (Stage::Idle, None), "the press was dropped");
    }

    #[tokio::test(start_paused = true)]
    async fn restart_engine_only_in_simulation() {
        let real = Arc::new(Spy::default());
        let (s, front, ..) = start_with("restartreal", real.clone(), Arc::new(SimFactory)).await;
        assert_eq!(s.restart_engine().await, Err(SwitchRefused(RESTART_REAL.into())));
        assert_eq!(real.builds(), 1);
        assert_eq!(*front.mode.borrow(), Mode::Real);
    }

    /// Task 9 step 4, driven as the window and the deck drive it (egui ignores synthetic
    /// clicks): switch to simulation, Prepare, Countdown at 10×, Live, play a video, End
    /// service, leave simulation. Every step completes and shows in the activity list.
    #[tokio::test(start_paused = true)]
    async fn simulation_rehearsal_end_to_end() {
        let real = Arc::new(Spy::default());
        let c = Config {
            cues: vec![streamit_core::rollin::Cue {
                path: PathBuf::from("notices.mp4"), // unreadable: plays for 30 s (3 s at 10×)
                name: "Notices".into(),
                destination: streamit_core::rollin::Destination::HallAndOnline,
                gain_db: 0.0,
                return_input: None,
            }],
            ..Config::default()
        };
        let (s, front, ..) = start_cfg("rehearsal", real.clone(), Arc::new(SimFactory), c).await;
        // `real` keeps only its latest build's log: hold on to the first real session's.
        let first_real = real.last_log.lock().unwrap().clone().unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
        let idle = front.rx.borrow().clone();
        assert_eq!(view::switch_state(&idle, *front.mode.borrow()).label, "Switch to simulation");

        // No step raises a warning.
        let no_warning = |front: &Front, step: &str| {
            let s = front.rx.borrow().clone();
            assert_eq!(s.last_warning, None, "a warning after {step}: {:#?}", s.activity);
        };

        // 1. Switch to simulation.
        s.switch(Mode::Simulation).await.unwrap();
        assert_eq!(*front.mode.borrow(), Mode::Simulation);
        let sim = front.sim.borrow().clone().unwrap();
        wait_for(&front, "the pretend broadcast", |s| s.simulation && s.broadcast.is_some()).await;
        no_warning(&front, "the switch");
        let decklink = sim.config().stream_atem.decklink_input;
        let cam8 = sim.config().videohub.fallback_normal_input;
        assert_eq!(cam8, SIM_FALLBACK_NORMAL_INPUT, "the effective config's simulated default");

        // 2. Prepare.
        front.tx.send(Command::Prepare).await.unwrap();
        wait_for(&front, "Prepare to run", |s| s.job == Some("Prepare")).await;
        let prepared = wait_for(&front, "Prepared", |s| matches!(s.stage, Stage::Prepared(_)) && s.job.is_none()).await;
        no_warning(&front, "Prepare");
        assert!(!view::switch_state(&prepared, Mode::Simulation).enabled, "no leaving mid-service");
        assert_eq!(s.switch(Mode::Real).await, refused());

        // 3. Countdown at 10×.
        sim.set_speed(Speed::X10);
        front.tx.send(Command::Countdown).await.unwrap();
        wait_for(&front, "the countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        let counting = Instant::now();

        // 4. Live: the 30 s countdown video ends after 3 s at 10×.
        wait_for(&front, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert!(counting.elapsed() < Duration::from_secs(10), "the countdown ran at 10×: {:?}", counting.elapsed());
        wait_for(&front, "public", |s| s.activity.iter().any(|a| a.text.starts_with("YouTube public"))).await;
        no_warning(&front, "the countdown");
        let v = sim.snapshot();
        assert_eq!(v.yt_life_cycle.as_deref(), Some("live"));
        assert!(view::went_public_text(v.last_go_public, v.speed).ends_with("at 10×"));

        // 5. Play a video.
        front.tx.send(Command::PlayCue(0)).await.unwrap();
        wait_for(&front, "the video to play", |s| s.playing == Some(0)).await;
        wait_for(&front, "the video to end", |s| s.playing.is_none() && s.job.is_none()).await;
        no_warning(&front, "the video");
        tokio::time::sleep(Duration::from_secs(1)).await; // any pretend mix still settling
        let v = sim.snapshot();
        assert_ne!(v.stream_program, decklink, "the stream is back off input 8");
        assert_eq!(v.input8, Input8Source::Other(cam8), "input 8 is routed back to Cam 8");
        assert!(!v.in8_audio_on, "input-8 audio is off again");

        // 6. End service.
        front.tx.send(Command::EndService).await.unwrap();
        let ended = wait_for(&front, "Ended", |s| s.stage == Stage::Ended && s.job.is_none()).await;
        no_warning(&front, "End service");
        tokio::time::sleep(Duration::from_secs(5)).await; // pretend YouTube's last transition
        let v = sim.snapshot();
        assert_eq!(v.encoder, streamit_core::devices::EncoderStatus::Idle, "the Web Presenter stopped");
        assert_eq!(v.yt_life_cycle.as_deref(), Some("complete"), "the broadcast is complete");
        let texts: Vec<&str> = ended.activity.iter().rev().map(|a| a.text.as_str()).collect();
        for want in [
            "Ready for the countdown",
            "Countdown started (0:30)",
            "Video 1 · Notices · started",
            "Video 1 · Notices · ended",
            "Ending the service",
            "Service ended",
        ] {
            assert!(texts.contains(&want), "{want:?} missing from the activity list: {texts:#?}");
        }
        assert!(texts.iter().any(|t| t.starts_with("YouTube public")), "{texts:#?}");
        let at = |want: &str| texts.iter().position(|t| *t == want).unwrap();
        assert!(at("Ready for the countdown") < at("Countdown started (0:30)"));
        assert!(at("Countdown started (0:30)") < at("Video 1 · Notices · started"));
        assert!(at("Video 1 · Notices · ended") < at("Ending the service"));
        assert!(at("Ending the service") < at("Service ended"));

        // 7. Leave simulation: the normal read-only startup on the real equipment.
        assert_eq!(view::switch_state(&ended, Mode::Simulation).label, "Leave simulation");
        assert!(view::switch_state(&ended, Mode::Simulation).enabled);
        s.switch(Mode::Real).await.unwrap();
        assert_eq!(*front.mode.borrow(), Mode::Real);
        assert!(front.sim.borrow().is_none());
        let snap = wait_for(&front, "a real snapshot", |s| !s.simulation).await;
        assert_eq!(snap.stage, Stage::Idle);
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(real.builds(), 2, "the real equipment was built at startup and again on leaving");
        assert_eq!(first_real.calls(), vec![], "nothing in the rehearsal touched the first real session");
        assert_eq!(real.calls(), vec![], "the second real session's startup only read");
    }
}
