//! Sessions: the real ↔ simulation switch.
//!
//! A session is one mode's device layer (built by a [`DeviceFactory`]), its engine, its
//! command log, its snapshot log and its health poll. The window and the Stream Deck
//! never hold a session: they hold the [`Front`], whose command channel and snapshot
//! stay the same across switches. Commands sent to the front go to the current
//! session's engine (through its command log), and its snapshots come back out.
//!
//! The app always starts in real mode. A switch is refused unless nothing is happening
//! (stage Idle or Ended, and no job); it stops the old session completely, then opens the
//! new one. Opening the real session is the normal read-only startup. The simulation
//! session builds only pretend devices: [`SimFactory`] never touches `wiring`, mDNS or a
//! socket, and the real factory is not called while simulating.
//!
//! Config rule: the real session's engine shares the window's config (Settings saves
//! apply to it as before). The simulation engine gets its own copy, the effective config
//! of [`sim_config`], rebuilt from the real config at each switch and never saved.
//! Settings saves while simulating still write the real file (and the window's copy);
//! the simulation picks them up the next time it is opened.
use crate::logging::simulation_span;
use crate::wiring::{self, Absent, Probes};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use streamit_core::audio::LiveLevels;
use streamit_core::config::Config;
use streamit_core::devices::{Broadcasts, Devices};
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, Engine, Snapshot};
use streamit_core::golive::Stage;
use streamit_sim::{SimControl, Speed};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tracing::{Instrument, Span, info, warn};

/// What a refused switch says.
pub const REFUSED: &str = "Finish or end the service first";
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
    /// The config the engine runs on when it is not the real one (simulation's effective
    /// copy). Never saved.
    pub config: Option<Config>,
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
            sim: Some(sim),
            config: Some(config),
            sim_defaults,
            tasks: Vec::new(),
        }
    }
}

/// The simulation's effective config: `c` with each unset (0) VideoHub number replaced
/// by its simulated default, and the names of the fields replaced (ruling S4).
pub fn sim_config(c: &Config) -> (Config, Vec<&'static str>) {
    let mut e = c.clone();
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
        Paths { levels: streamit_core::config::levels_path(config), sim_levels: config.with_file_name("sim-levels.toml") }
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
    #[allow(dead_code)] // read by the screens (Task 7)
    pub mode: watch::Receiver<Mode>,
    /// The pretend world, while simulating.
    #[allow(dead_code)] // read by the Simulation tab (Task 7)
    pub sim: watch::Receiver<Option<SimControl>>,
    /// Settings the simulation filled with a simulated default (empty in real mode).
    #[allow(dead_code)] // read by Settings (Task 7)
    pub sim_defaults: watch::Receiver<Vec<&'static str>>,
    /// The current session's YouTube, for the window's broadcast list.
    pub yt: watch::Receiver<Arc<dyn Broadcasts>>,
}

/// The senders behind the `Front`.
struct Outputs {
    /// The current session's command log; `None` while switching.
    target: watch::Sender<Option<mpsc::Sender<Command>>>,
    snap: watch::Sender<Snapshot>,
    mode: watch::Sender<Mode>,
    sim: watch::Sender<Option<SimControl>>,
    sim_defaults: watch::Sender<Vec<&'static str>>,
    yt: watch::Sender<Arc<dyn Broadcasts>>,
    /// The Stream Deck's last reported health: it reports only changes, so each new
    /// engine is told it when it starts.
    deck: Arc<Mutex<Option<DeviceHealth>>>,
}

/// One running session.
struct Session {
    mode: Mode,
    /// The command log, snapshot forwarding, snapshot log and health poll.
    tasks: Vec<JoinHandle<()>>,
    device_tasks: Vec<AbortHandle>,
    /// The engine's snapshot: closed once the engine and all its tasks have stopped.
    engine: watch::Receiver<Snapshot>,
    sim: Option<SimControl>,
    /// Where this session's engine saves the mic levels.
    #[cfg_attr(not(test), allow(dead_code))]
    levels_file: PathBuf,
}

pub struct Sessions {
    real: Arc<dyn DeviceFactory>,
    sim: Arc<dyn DeviceFactory>,
    /// The real config, shared with the window.
    config: Arc<RwLock<Config>>,
    paths: Paths,
    out: Outputs,
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
        let out = Outputs { target, snap, mode, sim: sim_tx, sim_defaults: defaults, yt, deck: Arc::default() };
        spawn_router(front_rx, target_rx, out.deck.clone());
        let sessions = Sessions { real, sim, config, paths, out, current: tokio::sync::Mutex::new(None) };
        let first = sessions.open(Mode::Real).await;
        *sessions.current.lock().await = Some(first);
        let front = Front { tx: front_tx, rx: snap_rx, mode: mode_rx, sim: sim_rx, sim_defaults: defaults_rx, yt: yt_rx };
        (sessions, front)
    }

    /// Switches to `to`: refused unless the stage is Idle or Ended with no job running.
    /// Stops the old session's engine, health poll and device tasks, then builds the new
    /// devices and spawns the new engine. Switching to the current mode does nothing.
    #[cfg_attr(not(test), allow(dead_code))] // called from Settings › Equipment (Task 7)
    pub async fn switch(&self, to: Mode) -> Result<(), SwitchRefused> {
        let mut current = self.current.lock().await;
        let old = current.take().expect("a session is always open outside a switch");
        if old.mode == to {
            *current = Some(old);
            return Ok(());
        }
        // The engine's own snapshot is the latest word on what is happening.
        if !may_switch(&old.engine.borrow()) {
            info!(to = ?to, "switch refused: a service or sequence is under way");
            *current = Some(old);
            return Err(SwitchRefused(REFUSED.into()));
        }
        info!(from = ?old.mode, to = ?to, "switching mode");
        self.close(old).await;
        *current = Some(self.open(to).await);
        info!(mode = ?to, "mode switched");
        Ok(())
    }

    /// The levels file the current engine saves to.
    #[cfg(test)]
    pub async fn levels_file(&self) -> PathBuf {
        self.current.lock().await.as_ref().expect("open").levels_file.clone()
    }

    /// Builds `mode`'s devices and starts its engine, logs and poll, then points the
    /// front at it.
    async fn open(&self, mode: Mode) -> Session {
        let span = span_for(mode);
        let real = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        let factory = match mode {
            Mode::Real => &self.real,
            Mode::Simulation => &self.sim,
        };
        let built = factory.build(&real).instrument(span.clone()).await;
        let config = match built.config {
            Some(effective) => Arc::new(RwLock::new(effective)),
            None => self.config.clone(),
        };
        let levels_file = self.paths.levels_for(mode).to_path_buf();
        let (engine, tx, mut tasks) = {
            let _in = span.enter();
            // The mics' live levels from the last run of this mode, so Go Live and Recover
            // after a restart fade up to them rather than to 0 dB.
            let levels = LiveLevels::load(&levels_file).unwrap_or_else(|e| {
                warn!("{e:#}; starting without saved mic levels");
                LiveLevels::default()
            });
            info!(path = %levels_file.display(), mics = levels.0.len(), "mic levels loaded");
            let engine = Engine::spawn_with(
                built.devices.clone(),
                config.clone(),
                levels,
                Some(levels_file.clone()),
                mode == Mode::Simulation,
            );
            let (tx, log) = spawn_command_log(engine.tx);
            let snap_log = spawn_snapshot_log(engine.rx.clone());
            (engine.rx, tx, vec![log, snap_log])
        };
        for (kind, health) in &built.health {
            let _ = tx.send(Command::DeviceHealth(*kind, health.clone())).await;
        }
        let deck = self.out.deck.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(h) = deck {
            let _ = tx.send(Command::DeviceHealth(DeviceKind::StreamDeck, h)).await;
        }
        if let Some(probes) = built.probes {
            let _in = span.enter();
            tasks.push(wiring::spawn_health_poll(probes, config, tx.clone(), engine.clone(), &built.health));
        }
        tasks.push(spawn_forward(engine.clone(), self.out.snap.clone()));
        self.out.yt.send_replace(built.devices.yt.clone());
        self.out.sim.send_replace(built.sim.clone());
        self.out.sim_defaults.send_replace(built.sim_defaults);
        self.out.mode.send_replace(mode);
        self.out.target.send_replace(Some(tx));
        Session { mode, tasks, device_tasks: built.tasks, engine, sim: built.sim, levels_file }
    }

    /// Stops `s` completely: no command reaches it any more, its logs and health poll
    /// stop, the front lets go of its devices, its engine stops (its command channel
    /// closes with the command log), and its device tasks stop. The real clients'
    /// reconnect loops end when their last handle (held by the engine, the poll and the
    /// front) is dropped; the pretend player's end timer ends when the world is reset.
    async fn close(&self, s: Session) {
        self.out.target.send_replace(None);
        self.out.yt.send_replace(Absent::new("switching mode"));
        self.out.sim.send_replace(None);
        for t in &s.tasks {
            t.abort();
        }
        for t in s.tasks {
            let _ = t.await;
        }
        for t in &s.device_tasks {
            t.abort();
        }
        if let Some(sim) = &s.sim {
            sim.reset();
        }
        let mut engine = s.engine;
        let stopped = tokio::time::timeout(STOP_WAIT, async { while engine.changed().await.is_ok() {} }).await;
        if stopped.is_err() {
            warn!(
                mode = ?s.mode,
                "the old engine's side tasks are still finishing; they keep the old devices until they do"
            );
        }
    }
}

/// Passes each front command to the current session; while switching, drops it.
fn spawn_router(
    mut rx: mpsc::Receiver<Command>,
    target: watch::Receiver<Option<mpsc::Sender<Command>>>,
    deck: Arc<Mutex<Option<DeviceHealth>>>,
) {
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if let Command::DeviceHealth(DeviceKind::StreamDeck, h) = &cmd {
                *deck.lock().unwrap_or_else(|e| e.into_inner()) = Some(h.clone());
            }
            let to = target.borrow().clone();
            let sent = match to {
                Some(to) => to.send(cmd.clone()).await.is_ok(),
                None => false,
            };
            if !sent {
                info!(command = ?cmd, "switching mode: command dropped");
            }
        }
    });
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

/// Every command from the window, the deck and the health poll passes through here and
/// is logged before it reaches the engine. Holds the engine's only command sender, so
/// stopping this task stops the engine.
fn spawn_command_log(engine: mpsc::Sender<Command>) -> (mpsc::Sender<Command>, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<Command>(64);
    let task = tokio::spawn(
        async move {
            while let Some(cmd) = rx.recv().await {
                match &cmd {
                    // Health changes are logged by the poll with their reason.
                    Command::DeviceHealth(..) => {}
                    Command::SetCues(cues) => {
                        let names: Vec<&str> = cues.iter().map(|c| c.name.as_str()).collect();
                        info!(cues = ?names, "command SetCues");
                    }
                    other => info!(command = ?other, "command"),
                }
                if engine.send(cmd).await.is_err() {
                    warn!("the engine has stopped; dropping commands");
                    return;
                }
            }
        }
        .in_current_span(),
    );
    (tx, task)
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Mutex, Weak};
    use streamit_core::audio::LiveLevels;
    use streamit_core::config::AudioSourceId;
    use streamit_core::devices::StreamSwitcher;
    use streamit_core::fakes::{CallLog, fake_devices};
    use streamit_core::golive::Stage;
    use streamit_sim::{Input8Source, SEED_BROADCAST};

    const WAIT: Duration = Duration::from_secs(120);

    /// A factory of fake devices that counts its builds, keeps the last build's call log
    /// and a weak reference to its Stream ATEM, and panics if called once armed.
    #[derive(Default)]
    struct Spy {
        builds: AtomicUsize,
        armed: AtomicBool,
        last_log: Mutex<Option<CallLog>>,
        last_stream: Mutex<Option<Weak<dyn StreamSwitcher>>>,
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
            let (devices, log, _h) = fake_devices();
            *self.last_log.lock().unwrap() = Some(log);
            *self.last_stream.lock().unwrap() = Some(Arc::downgrade(&devices.stream));
            Built::plain(devices)
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
        let (config, p) = paths(name);
        let shared = Arc::new(RwLock::new(Config::default()));
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
        let (s, front, ..) = start_with("job", Arc::new(Spy::default()), sim.clone()).await;
        front.tx.send(Command::Prepare).await.unwrap();
        wait_for(&front, "Prepare to run", |s| s.job.is_some()).await;
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
        // Nothing was saved, and the real config is unchanged.
        assert_eq!(std::fs::read(&config).unwrap(), saved);
        let real = shared.read().unwrap().videohub.clone();
        assert_eq!((real.input8_output, real.decklink_input, real.fallback_normal_input), (0, 0, 0));
    }
}
