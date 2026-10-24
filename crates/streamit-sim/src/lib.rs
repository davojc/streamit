//! Pretend equipment so the operator can practise a service with no hardware.
//!
//! One shared [`SimWorld`] holds every pretend device's state; each device handle
//! (switcher, router, encoder, YouTube, player and counter) is a thin view over it plus
//! the shared [`SimClock`]. Delayed effects are stored with a deadline and applied
//! lazily when read, so no background tasks are needed. The one exception is the
//! player's `Ended` event, which needs a small timer task per play (see `SimPlayer`).
pub mod clip;
pub mod clock;
pub mod others;
pub mod switcher;
pub mod youtube;

pub use clip::{clip_length, read_length};
pub use clock::{SimClock, Speed};
pub use others::{SimCounter, SimEncoder, SimPlayer};
pub use switcher::{SimRouter, SimSwitcher, Which, media_player_input};
pub use youtube::SimBroadcasts;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;
use streamit_core::config::{AudioSourceId, Config};
use streamit_core::devices::{
    BroadcastCheck, BroadcastSummary, DeviceError, Devices, EncoderStatus, Health, Phase, PlayerEvent,
};
use streamit_core::engine::DeviceKind;
use tokio::sync::broadcast;
use tokio::time::Instant;

/// Id of the one broadcast the pretend YouTube starts with.
pub const SEED_BROADCAST: &str = "sim-test";
/// The stream the seeded broadcast is bound to ("our stream").
pub const SEED_STREAM: &str = "sim-stream";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fault {
    Unplug(DeviceKind),
    YouTubeRefusesPublic,
    YouTubeSlow,
    NextVideoFails,
    MicFadeFails,
}

/// A mix in progress: the target takes over at `done_at`.
#[derive(Debug, Clone, Copy)]
pub struct Mix {
    pub to: u16,
    pub done_at: Instant,
}

#[derive(Debug, Clone)]
pub struct SwitcherState {
    pub program: u16,
    pub preview: u16,
    pub mix: Option<Mix>,
}

impl SwitcherState {
    pub fn new(program: u16, preview: u16) -> Self {
        Self { program, preview, mix: None }
    }

    /// Applies a mix whose time has come: the target goes to program, the old
    /// program to preview (as on the real switcher).
    pub fn settle(&mut self) {
        if let Some(m) = self.mix
            && Instant::now() >= m.done_at
        {
            self.preview = self.program;
            self.program = m.to;
            self.mix = None;
        }
    }
}

/// A route that has been asked for and takes effect at `done_at`.
#[derive(Debug, Clone, Copy)]
pub struct PendingRoute {
    pub input: u16,
    pub done_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerState {
    Empty,
    Loaded {
        duration: Duration,
    },
    /// `anchor_pos` was the position at `anchor_at`; it advances at the speed factor from
    /// there. Re-anchoring on a speed change keeps the position continuous.
    Playing {
        duration: Duration,
        anchor_pos: Duration,
        anchor_at: Instant,
    },
}

impl PlayerState {
    /// Position at `factor` times real speed; `None` unless playing.
    pub fn position(&self, factor: u32) -> Option<Duration> {
        match self {
            PlayerState::Playing { duration, anchor_pos, anchor_at } => {
                Some((*anchor_pos + anchor_at.elapsed() * factor).min(*duration))
            }
            _ => None,
        }
    }
}

/// A YouTube transition that has been asked for and completes at `done_at`.
#[derive(Debug, Clone)]
pub struct YtPending {
    pub id: String,
    pub to: Phase,
    pub requested: Instant,
    pub done_at: Instant,
}

#[derive(Debug)]
pub struct SimWorld {
    pub stream: SwitcherState,
    pub screens: SwitcherState,
    pub fairlight: HashMap<AudioSourceId, f32>,
    /// Input whose audio flag `input8_audio_on` tracks (the DeckLink input).
    pub decklink_input: u16,
    pub input8_audio_on: bool,
    pub routes: BTreeMap<u16, u16>,
    pub pending_routes: BTreeMap<u16, PendingRoute>,
    pub encoder: EncoderStatus,
    /// When a Connecting encoder starts Streaming.
    pub encoder_at: Option<Instant>,
    /// When the stream's health turns Good.
    pub health_at: Option<Instant>,
    pub broadcasts: Vec<BroadcastSummary>,
    pub yt_check: BroadcastCheck,
    pub yt_pending: Option<YtPending>,
    /// How long the last transition to Live took, request to completion.
    pub last_go_public: Option<Duration>,
    pub player: PlayerState,
    /// Bumped by load, stop and reset: a player timer from an older value must not emit.
    pub player_gen: u64,
    pub player_events: broadcast::Sender<PlayerEvent>,
    /// The last remaining time the core showed on the NDI counter; `None` when cleared.
    pub counter: Option<Duration>,
    pub faults: HashSet<Fault>,
}

pub type Shared = Arc<Mutex<SimWorld>>;

impl SimWorld {
    /// The world as the real equipment would be found: matches the config defaults.
    pub fn seeded(cfg: &Config) -> Self {
        let sa = &cfg.stream_atem;
        let mut fairlight = HashMap::new();
        for m in &sa.mic_sources {
            fairlight.insert(*m, -3.0);
        }
        let hub = &cfg.videohub;
        let normal = if hub.fallback_normal_input == 0 { 13 } else { hub.fallback_normal_input };
        let mut routes = BTreeMap::new();
        routes.insert(hub.input8_output, normal);
        Self {
            stream: SwitcherState::new(3000 + 10 * sa.media_player as u16, 1),
            screens: SwitcherState::new(cfg.screens_atem.t0_input, 1),
            fairlight,
            decklink_input: sa.decklink_input,
            input8_audio_on: false,
            routes,
            pending_routes: BTreeMap::new(),
            encoder: EncoderStatus::Idle,
            encoder_at: None,
            health_at: None,
            broadcasts: vec![BroadcastSummary {
                id: SEED_BROADCAST.into(),
                title: "Test service".into(),
                scheduled_start: None,
                life_cycle: "ready".into(),
                bound_stream_id: Some(SEED_STREAM.into()),
            }],
            yt_check: BroadcastCheck { bound_to_ours: true, auto_start: false, auto_stop: true, monitor_stream: true },
            yt_pending: None,
            last_go_public: None,
            player: PlayerState::Empty,
            player_gen: 0,
            player_events: broadcast::channel(16).0,
            counter: None,
            faults: HashSet::new(),
        }
    }

    /// Applies every delayed effect whose deadline has passed. (The player's `Ended`
    /// event is the one thing a deadline cannot do: see `SimPlayer::play`.)
    pub fn settle(&mut self) {
        self.stream.settle();
        self.screens.settle();
        let now = Instant::now();
        if self.encoder == EncoderStatus::Connecting && self.encoder_at.is_some_and(|t| now >= t) {
            self.encoder = EncoderStatus::Streaming;
        }
        if self.yt_pending.as_ref().is_some_and(|p| now >= p.done_at)
            && let Some(p) = self.yt_pending.take()
        {
            // A life cycle only ever moves forward.
            let mut moved = false;
            if let Some(b) = self.broadcasts.iter_mut().find(|b| b.id == p.id)
                && life_rank(phase_name(p.to)) > life_rank(&b.life_cycle)
            {
                b.life_cycle = phase_name(p.to).into();
                moved = true;
            }
            if moved && p.to == Phase::Live {
                self.last_go_public = Some(p.done_at - p.requested);
            }
        }
        let due: Vec<u16> = self.pending_routes.iter().filter(|(_, p)| now >= p.done_at).map(|(o, _)| *o).collect();
        for o in due {
            if let Some(p) = self.pending_routes.remove(&o) {
                self.routes.insert(o, p.input);
            }
        }
    }

    pub fn switcher_mut(&mut self, which: Which) -> &mut SwitcherState {
        match which {
            Which::Stream => &mut self.stream,
            Which::Screens => &mut self.screens,
        }
    }

    pub fn has_fault(&self, f: Fault) -> bool {
        self.faults.contains(&f)
    }

    /// `Disconnected` when the device is unplugged.
    pub fn check_plugged(&self, kind: DeviceKind) -> Result<(), DeviceError> {
        if self.has_fault(Fault::Unplug(kind)) {
            Err(DeviceError::Disconnected("unplugged (simulation)".into()))
        } else {
            Ok(())
        }
    }

    /// The stream's health: Good once the encoder has been streaming for 5 s (scaled).
    pub fn health(&self) -> Health {
        if self.encoder == EncoderStatus::Streaming && self.health_at.is_some_and(|t| Instant::now() >= t) {
            Health::Good
        } else {
            Health::NotReceiving
        }
    }
}

/// The life-cycle name a transition to `p` ends in.
pub fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Testing => "testing",
        Phase::Live => "live",
        Phase::Complete => "complete",
    }
}

/// Position of a life cycle on its one-way road.
fn life_rank(lc: &str) -> u8 {
    match lc {
        "testing" => 1,
        "live" => 2,
        "complete" => 3,
        _ => 0, // created, ready
    }
}

/// Everything a pretend device needs: the shared world and the shared clock.
#[derive(Clone)]
pub struct SimHandle {
    pub world: Shared,
    /// Crate-private: only `SimControl::set_speed` may change the speed, because it
    /// also re-anchors a video that is playing.
    pub(crate) clock: SimClock,
}

impl SimHandle {
    pub fn new(cfg: &Config, speed: Speed) -> Self {
        Self { world: Arc::new(Mutex::new(SimWorld::seeded(cfg))), clock: SimClock::new(speed) }
    }

    /// Locks the world with every due delayed effect already applied.
    pub fn lock(&self) -> MutexGuard<'_, SimWorld> {
        let mut w = self.world.lock().unwrap_or_else(|e| e.into_inner());
        w.settle();
        w
    }

    pub fn speed(&self) -> Speed {
        self.clock.speed()
    }

    pub fn set_fault(&self, f: Fault, on: bool) {
        let mut w = self.lock();
        if on {
            w.faults.insert(f);
        } else {
            w.faults.remove(&f);
        }
    }

    pub fn stream_switcher(&self) -> SimSwitcher {
        SimSwitcher::new(self.clone(), Which::Stream)
    }

    pub fn screens_switcher(&self) -> SimSwitcher {
        SimSwitcher::new(self.clone(), Which::Screens)
    }

    pub fn router(&self) -> SimRouter {
        SimRouter::new(self.clone())
    }

    pub fn encoder(&self) -> SimEncoder {
        SimEncoder::new(self.clone())
    }

    pub fn broadcasts(&self) -> SimBroadcasts {
        SimBroadcasts::new(self.clone())
    }

    pub fn player(&self) -> SimPlayer {
        SimPlayer::new(self.clone())
    }

    pub fn counter(&self) -> SimCounter {
        SimCounter::new(self.clone())
    }
}

/// All seven pretend devices, sharing one world and one clock.
pub fn build_devices(sim: &SimHandle) -> Devices {
    Devices {
        stream: Arc::new(sim.stream_switcher()),
        screens: Arc::new(sim.screens_switcher()),
        hub: Arc::new(sim.router()),
        encoder: Arc::new(sim.encoder()),
        yt: Arc::new(sim.broadcasts()),
        player: Arc::new(sim.player()),
        counter: Arc::new(sim.counter()),
    }
}

/// What input 8 (the shared camera/DeckLink input) is carrying, from the VideoHub route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input8Source {
    DeckLink,
    /// The VideoHub input routed to it (Camera 8 in the normal case).
    Other(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerView {
    Empty,
    Loaded { duration: Duration },
    Playing { position: Duration, duration: Duration },
}

/// A read-only picture of the pretend world for the Simulation tab.
#[derive(Debug, Clone, PartialEq)]
pub struct SimView {
    pub stream_program: u16,
    pub stream_preview: u16,
    /// Mic levels in dB, ordered by source.
    pub mic_levels: Vec<(AudioSourceId, f32)>,
    pub in8_audio_on: bool,
    pub input8: Input8Source,
    pub screens_program: u16,
    pub encoder: EncoderStatus,
    /// `None` when there is no broadcast.
    pub yt_life_cycle: Option<String>,
    pub yt_health: Health,
    /// Time from the request to go Live until it completed.
    pub last_go_public: Option<Duration>,
    pub player: PlayerView,
    /// `M:SS` as the NDI counter shows it; `None` when it is cleared.
    pub counter_text: Option<String>,
    pub faults: Vec<Fault>,
    pub speed: Speed,
}

/// `M:SS`, rounding up so a countdown reads 0:00 only at the very end.
pub fn counter_text(remaining: Duration) -> String {
    let secs = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The Simulation tab's handle on the pretend world. Cloneable.
#[derive(Clone)]
pub struct SimControl {
    sim: SimHandle,
    /// The effective config as built, unscaled: what Settings shows.
    cfg: Config,
    /// The config the simulation engine runs on: `cfg` with the VideoHub settle, closing
    /// hold and mic fade divided by the speed, rewritten on every speed change.
    effective: Arc<RwLock<Config>>,
}

impl SimControl {
    pub fn new(cfg: &Config, speed: Speed) -> Self {
        Self::from_handle(SimHandle::new(cfg, speed), cfg.clone())
    }

    fn from_handle(sim: SimHandle, cfg: Config) -> Self {
        let c = Self { sim, effective: Arc::new(RwLock::new(cfg.clone())), cfg };
        c.scale_config();
        c
    }

    /// The config to give the simulation engine. Shared: speed changes reach it.
    pub fn shared_config(&self) -> Arc<RwLock<Config>> {
        self.effective.clone()
    }

    /// Rewrites the shared config's settle, hold and fade as the real values over the speed.
    fn scale_config(&self) {
        let f = self.sim.clock.speed().factor();
        let mut e = self.effective.write().unwrap_or_else(|e| e.into_inner());
        e.videohub.settle = self.cfg.videohub.settle / f;
        e.timings.closing_hold = self.cfg.timings.closing_hold / f;
        e.stream_atem.fade = self.cfg.stream_atem.fade / f;
    }

    pub fn handle(&self) -> &SimHandle {
        &self.sim
    }

    /// The config the pretend world was built from: the running simulation's effective
    /// config (with any simulated defaults), not the settings file.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn devices(&self) -> Devices {
        build_devices(&self.sim)
    }

    pub fn snapshot(&self) -> SimView {
        let w = self.sim.lock();
        let speed = self.sim.clock.speed();
        let mut mic_levels: Vec<_> = w.fairlight.iter().map(|(s, db)| (*s, *db)).collect();
        mic_levels.sort_by_key(|(s, _)| (s.input, s.source));
        let routed = w.routes.get(&self.cfg.videohub.input8_output).copied().unwrap_or(0);
        let decklink = self.cfg.videohub.decklink_input;
        let mut faults: Vec<Fault> = w.faults.iter().copied().collect();
        faults.sort_by_key(|f| format!("{f:?}"));
        SimView {
            stream_program: w.stream.program,
            stream_preview: w.stream.preview,
            mic_levels,
            in8_audio_on: w.input8_audio_on,
            input8: if decklink != 0 && routed == decklink {
                Input8Source::DeckLink
            } else {
                Input8Source::Other(routed)
            },
            screens_program: w.screens.program,
            encoder: w.encoder.clone(),
            yt_life_cycle: w.broadcasts.first().map(|b| b.life_cycle.clone()),
            yt_health: w.health(),
            last_go_public: w.last_go_public,
            player: match w.player {
                PlayerState::Empty => PlayerView::Empty,
                PlayerState::Loaded { duration } => PlayerView::Loaded { duration },
                PlayerState::Playing { duration, .. } => {
                    PlayerView::Playing { position: w.player.position(speed.factor()).unwrap_or_default(), duration }
                }
            },
            counter_text: w.counter.map(counter_text),
            faults,
            speed,
        }
    }

    pub fn set_preview(&self, input: u16) {
        self.sim.lock().stream.preview = input;
    }

    /// A cut straight to `input`, cancelling any mix in progress.
    pub fn set_program(&self, input: u16) {
        let mut w = self.sim.lock();
        w.stream.program = input;
        w.stream.mix = None;
    }

    /// Swaps program and preview at once.
    pub fn cut(&self) {
        let mut w = self.sim.lock();
        let s = &mut w.stream;
        std::mem::swap(&mut s.program, &mut s.preview);
        s.mix = None;
    }

    /// A mix to preview taking the usual pretend second; ignored while one is running.
    pub fn auto(&self) {
        let done_at = Instant::now() + self.sim.clock.scale(switcher::PRETEND_DELAY);
        let mut w = self.sim.lock();
        let s = &mut w.stream;
        if s.mix.is_none() {
            s.mix = Some(Mix { to: s.preview, done_at });
        }
    }

    pub fn toggle_fault(&self, f: Fault) {
        let on = !self.sim.lock().has_fault(f);
        self.sim.set_fault(f, on);
    }

    /// Changes the speed; a video that is playing keeps its position and carries on at the
    /// new rate. The engine's VideoHub settle, closing hold and mic fade follow from its
    /// next sequence (one already waiting keeps the wait it started with).
    pub fn set_speed(&self, speed: Speed) {
        {
            let mut w = self.sim.lock();
            let old = self.sim.clock.speed().factor();
            if let PlayerState::Playing { duration, .. } = w.player {
                let anchor_pos = w.player.position(old).unwrap_or_default();
                w.player = PlayerState::Playing { duration, anchor_pos, anchor_at: Instant::now() };
            }
            self.sim.clock.set_speed(speed);
        }
        self.scale_config();
    }

    /// Back to the seeded world: faults cleared, video stopped, counter cleared. The
    /// speed and anyone already subscribed to player events are kept.
    pub fn reset(&self) {
        let mut w = self.sim.lock();
        let mut fresh = SimWorld::seeded(&self.cfg);
        fresh.player_events = w.player_events.clone();
        fresh.player_gen = w.player_gen + 1; // any running end timer is now stale
        *w = fresh;
        drop(w);
        self.scale_config();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use streamit_core::audio::{LiveLevels, down_threshold};
    use streamit_core::devices::{Broadcasts, Counter, Encoder, Player};
    use streamit_core::recovery::{Offer, assess, recover};
    use streamit_core::rollin::{Cue, Destination, play};
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    const OUT: u16 = 12;
    const DK: u16 = 40;
    const CAM8: u16 = 5;

    fn cfg() -> Config {
        let mut c = Config::default();
        c.videohub.input8_output = OUT;
        c.videohub.decklink_input = DK;
        c.videohub.fallback_normal_input = CAM8;
        c
    }

    async fn ms(n: u64) {
        tokio::time::sleep(Duration::from_millis(n)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn encoder_streams_after_three_seconds() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let e = sim.encoder();
        assert_eq!(e.status().await.unwrap(), EncoderStatus::Idle);
        e.start().await.unwrap();
        assert_eq!(e.status().await.unwrap(), EncoderStatus::Connecting);
        ms(2900).await;
        assert_eq!(e.status().await.unwrap(), EncoderStatus::Connecting);
        ms(100).await;
        assert_eq!(e.status().await.unwrap(), EncoderStatus::Streaming);
        e.stop().await.unwrap();
        assert_eq!(e.status().await.unwrap(), EncoderStatus::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn encoder_start_while_running_keeps_its_deadline() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let e = sim.encoder();
        e.start().await.unwrap();
        ms(2000).await;
        e.start().await.unwrap();
        ms(1000).await;
        assert_eq!(e.status().await.unwrap(), EncoderStatus::Streaming);
    }

    #[tokio::test(start_paused = true)]
    async fn youtube_health_good_five_seconds_after_streaming() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let (e, yt) = (sim.encoder(), sim.broadcasts());
        assert_eq!(yt.health().await.unwrap(), Health::NotReceiving);
        e.start().await.unwrap();
        ms(7900).await; // streaming at 3 s, so Good at 8 s
        assert_eq!(yt.health().await.unwrap(), Health::NotReceiving);
        ms(100).await;
        assert_eq!(yt.health().await.unwrap(), Health::Good);
        e.stop().await.unwrap();
        assert_eq!(yt.health().await.unwrap(), Health::NotReceiving);
    }

    #[tokio::test(start_paused = true)]
    async fn youtube_seed_fixes_and_transitions() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let yt = sim.broadcasts();
        assert!(yt.signed_in().await.unwrap());
        let up = yt.upcoming().await.unwrap();
        assert_eq!(up.len(), 1);
        assert_eq!(
            (up[0].id.as_str(), up[0].title.as_str(), up[0].life_cycle.as_str()),
            ("sim-test", "Test service", "ready")
        );
        assert_eq!(up[0].bound_stream_id.as_deref(), Some(SEED_STREAM));
        yt.fix("sim-test", streamit_core::devices::Fix::AutoStartOff).await.unwrap();
        let c = yt.check("sim-test").await.unwrap();
        assert!(c.bound_to_ours && !c.auto_start && c.auto_stop && c.monitor_stream);
        let t = Instant::now();
        yt.transition("sim-test", Phase::Testing).await.unwrap();
        assert_eq!(t.elapsed(), Duration::from_millis(2500));
        assert_eq!(yt.life_cycle("sim-test").await.unwrap(), "testing");
        yt.transition("sim-test", Phase::Live).await.unwrap();
        assert_eq!(yt.life_cycle("sim-test").await.unwrap(), "live");
        assert_eq!(sim.lock().last_go_public, Some(Duration::from_millis(2500)));
        assert!(yt.life_cycle("nope").await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn youtube_refuses_public_fault() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let yt = sim.broadcasts();
        sim.set_fault(Fault::YouTubeRefusesPublic, true);
        yt.transition("sim-test", Phase::Testing).await.unwrap();
        let r = yt.transition("sim-test", Phase::Live).await;
        assert!(matches!(r, Err(DeviceError::Refused(_))), "{r:?}");
        assert_eq!(yt.life_cycle("sim-test").await.unwrap(), "testing");
    }

    #[tokio::test(start_paused = true)]
    async fn youtube_slow_fault_delays_every_call_and_scales() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let yt = sim.broadcasts();
        sim.set_fault(Fault::YouTubeSlow, true);
        let t = Instant::now();
        yt.health().await.unwrap();
        assert_eq!(t.elapsed(), Duration::from_secs(20));
        SimControl::from_handle(sim.clone(), cfg()).set_speed(Speed::X10);
        let t = Instant::now();
        yt.life_cycle("sim-test").await.unwrap();
        assert_eq!(t.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn unplugged_encoder_and_youtube_are_disconnected() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        sim.set_fault(Fault::Unplug(DeviceKind::WebPresenter), true);
        sim.set_fault(Fault::Unplug(DeviceKind::YouTube), true);
        sim.set_fault(Fault::Unplug(DeviceKind::Ndi), true);
        assert!(matches!(sim.encoder().start().await, Err(DeviceError::Disconnected(_))));
        assert!(matches!(sim.encoder().status().await, Err(DeviceError::Disconnected(_))));
        assert!(matches!(sim.broadcasts().upcoming().await, Err(DeviceError::Disconnected(_))));
        assert!(matches!(sim.counter().show(Duration::from_secs(1)).await, Err(DeviceError::Disconnected(_))));
        sim.set_fault(Fault::Unplug(DeviceKind::DeckLink), true);
        let r = sim.player().load(&PathBuf::from("x.mp4"), 0.0).await;
        assert!(matches!(r, Err(DeviceError::Disconnected(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn player_ends_at_clip_length_at_speed() {
        // A missing file is 30 s long, so use the 60 s fixture.
        let dir = clip::test_support::temp_dir("player");
        let path = dir.join("sixty.mp4");
        std::fs::write(&path, clip::test_support::mp4_bytes(60_000)).unwrap();
        let sim = SimHandle::new(&cfg(), Speed::X10);
        let p = sim.player();
        let mut rx = p.events();
        assert_eq!(p.load(&path, 0.0).await.unwrap(), Duration::from_secs(60));
        assert_eq!(p.position(), None);
        p.play().await.unwrap();
        let t = Instant::now();
        ms(3000).await;
        let pos = p.position().unwrap();
        assert!(pos >= Duration::from_millis(29_990) && pos <= Duration::from_millis(30_010), "{pos:?}");
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.unwrap().unwrap();
        assert_eq!(ev, PlayerEvent::Ended);
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(5990) && took <= Duration::from_millis(6010), "{took:?}");
        assert_eq!(sim.lock().player, PlayerState::Empty);
    }

    #[tokio::test(start_paused = true)]
    async fn stopped_or_reloaded_player_never_emits_a_stale_end() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let p = sim.player();
        let mut rx = p.events();
        // Missing file: 30 s.
        p.load(&PathBuf::from("none.mp4"), 0.0).await.unwrap();
        p.play().await.unwrap();
        ms(10_000).await;
        p.stop().await.unwrap();
        assert_eq!(p.position(), None);
        // A second video starts at once; the first one's timer would fire at 30 s.
        p.load(&PathBuf::from("none.mp4"), 0.0).await.unwrap();
        p.play().await.unwrap();
        ms(25_000).await; // t = 35 s: past the first video's end, before the second's
        assert!(rx.try_recv().is_err(), "stale Ended");
        ms(5_100).await;
        assert_eq!(rx.try_recv(), Ok(PlayerEvent::Ended));
        assert!(rx.try_recv().is_err(), "exactly one Ended");
    }

    #[tokio::test(start_paused = true)]
    async fn next_video_fails_fault_errors_once() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let p = sim.player();
        sim.set_fault(Fault::NextVideoFails, true);
        assert!(p.load(&PathBuf::from("a.mp4"), 0.0).await.is_err());
        assert!(!sim.lock().has_fault(Fault::NextVideoFails));
        assert_eq!(p.load(&PathBuf::from("a.mp4"), 0.0).await.unwrap(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn counter_records_last_value_and_formats_on_read() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let c = SimControl::from_handle(sim.clone(), cfg());
        assert_eq!(c.snapshot().counter_text, None);
        sim.counter().show(Duration::from_millis(61_500)).await.unwrap();
        assert_eq!(c.snapshot().counter_text.as_deref(), Some("1:02"));
        sim.counter().show(Duration::from_secs(9)).await.unwrap();
        assert_eq!(c.snapshot().counter_text.as_deref(), Some("0:09"));
        sim.counter().clear().await.unwrap();
        assert_eq!(c.snapshot().counter_text, None);
    }

    #[test]
    fn unreadable_clip_lasts_30s() {
        let dir = clip::test_support::temp_dir("unreadable");
        assert_eq!(clip_length(&dir.join("missing.mp4")), Duration::from_secs(30));
        let fake = dir.join("fake.mp4");
        std::fs::write(&fake, "this is not a movie").unwrap();
        assert_eq!(clip_length(&fake), Duration::from_secs(30));
    }

    /// The app's Videos tab reads lengths with no fallback, so it can say why it has none.
    #[test]
    fn read_length_has_no_fallback() {
        let dir = clip::test_support::temp_dir("read-length");
        assert_eq!(read_length(&dir.join("missing.mp4")), None);
        let fake = dir.join("fake.mp4");
        std::fs::write(&fake, "this is not a movie").unwrap();
        assert_eq!(read_length(&fake), None);
        let f = dir.join("clip.mp4");
        std::fs::write(&f, clip::test_support::mp4_bytes(12_000)).unwrap();
        assert_eq!(read_length(&f), Some(Duration::from_secs(12)));
        std::fs::write(&f, clip::test_support::mp4_bytes(0)).unwrap();
        assert_eq!(read_length(&f), None, "a zero length is not a length");
    }

    /// The speed scales the config durations the engine waits on: VideoHub settle,
    /// closing hold and mic fade, in the config the simulation engine shares.
    #[tokio::test(start_paused = true)]
    async fn speed_scales_settle_hold_and_fade() {
        let c = SimControl::new(&cfg(), Speed::X1);
        let shared = c.shared_config();
        let real = cfg();
        let read = || {
            let e = shared.read().unwrap();
            (e.videohub.settle, e.timings.closing_hold, e.stream_atem.fade)
        };
        assert_eq!(read(), (real.videohub.settle, real.timings.closing_hold, real.stream_atem.fade));
        c.set_speed(Speed::X10);
        assert_eq!(read(), (Duration::from_millis(150), Duration::from_secs(1), Duration::from_millis(100)));
        c.set_speed(Speed::X4);
        assert_eq!(read(), (Duration::from_millis(375), Duration::from_millis(2500), Duration::from_millis(250)));
        c.reset();
        assert_eq!(read().1, Duration::from_millis(2500), "a reset keeps the speed, so the scaled values too");
        c.set_speed(Speed::X1);
        assert_eq!(read(), (real.videohub.settle, real.timings.closing_hold, real.stream_atem.fade));
        // The unscaled values are what Settings shows.
        assert_eq!(c.config().timings.closing_hold, real.timings.closing_hold);
        // Built at a speed: scaled from the start.
        let fast = SimControl::new(&cfg(), Speed::X10);
        assert_eq!(fast.shared_config().read().unwrap().timings.closing_hold, Duration::from_secs(1));
    }

    #[test]
    fn readable_clip_reports_its_movie_duration() {
        let dir = clip::test_support::temp_dir("readable");
        let f = dir.join("clip.mov");
        std::fs::write(&f, clip::test_support::mp4_bytes(83_500)).unwrap();
        assert_eq!(clip_length(&f), Duration::from_millis(83_500));
    }

    #[tokio::test(start_paused = true)]
    async fn snapshot_reports_the_seeded_world() {
        let c = SimControl::new(&cfg(), Speed::X4);
        let v = c.snapshot();
        assert_eq!((v.stream_program, v.stream_preview, v.screens_program), (3010, 1, 6));
        assert_eq!(v.input8, Input8Source::Other(CAM8));
        assert!(!v.in8_audio_on, "input 8 audio starts off, as on the real Stream ATEM between videos");
        assert_eq!(v.mic_levels.len(), cfg().stream_atem.mic_sources.len());
        assert!(v.mic_levels.iter().all(|(_, db)| *db == -3.0));
        assert_eq!(v.encoder, EncoderStatus::Idle);
        assert_eq!(v.yt_life_cycle.as_deref(), Some("ready"));
        assert_eq!(v.yt_health, Health::NotReceiving);
        assert_eq!((v.last_go_public, v.player, v.counter_text), (None, PlayerView::Empty, None));
        assert!(v.faults.is_empty());
        assert_eq!(v.speed, Speed::X4);
    }

    #[tokio::test(start_paused = true)]
    async fn controls_drive_the_stream_switcher_and_faults() {
        let c = SimControl::new(&cfg(), Speed::X1);
        c.set_preview(2);
        c.auto();
        c.auto(); // ignored while mixing
        assert_eq!(c.snapshot().stream_program, 3010);
        ms(1000).await;
        let v = c.snapshot();
        assert_eq!((v.stream_program, v.stream_preview), (2, 3010));
        c.cut();
        let v = c.snapshot();
        assert_eq!((v.stream_program, v.stream_preview), (3010, 2));
        c.set_program(7);
        assert_eq!(c.snapshot().stream_program, 7);
        c.toggle_fault(Fault::YouTubeSlow);
        assert_eq!(c.snapshot().faults, vec![Fault::YouTubeSlow]);
        c.toggle_fault(Fault::YouTubeSlow);
        assert!(c.snapshot().faults.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn speed_change_keeps_player_position_continuous() {
        let c = SimControl::new(&cfg(), Speed::X1);
        let d = c.devices();
        d.player.load(&PathBuf::from("none.mp4"), 0.0).await.unwrap();
        d.player.play().await.unwrap();
        ms(5000).await;
        c.set_speed(Speed::X10);
        let v = c.snapshot();
        assert_eq!(v.speed, Speed::X10);
        assert_eq!(
            v.player,
            PlayerView::Playing { position: Duration::from_secs(5), duration: Duration::from_secs(30) }
        );
        ms(1000).await;
        assert_eq!(
            c.snapshot().player,
            PlayerView::Playing { position: Duration::from_secs(15), duration: Duration::from_secs(30) }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reset_restores_seeded_world() {
        let c = SimControl::new(&cfg(), Speed::X4);
        let fresh = c.snapshot();
        let d = c.devices();
        d.encoder.start().await.unwrap();
        d.yt.transition("sim-test", Phase::Testing).await.unwrap();
        d.hub.route(OUT, DK).await.unwrap();
        d.stream.set_audio_level(cfg().stream_atem.mic_sources[0], -40.0).await.unwrap();
        d.player.load(&PathBuf::from("none.mp4"), 0.0).await.unwrap();
        d.player.play().await.unwrap();
        d.counter.show(Duration::from_secs(5)).await.unwrap();
        c.set_program(9);
        c.toggle_fault(Fault::MicFadeFails);
        ms(10_000).await;
        assert_ne!(c.snapshot(), fresh);
        let mut rx = d.player.events();
        c.reset();
        assert_eq!(c.snapshot(), fresh);
        // The old video's timer must not announce an end after the reset.
        ms(60_000).await;
        assert!(rx.try_recv().is_err());
        assert_eq!(d.player.position(), None);
    }

    #[test]
    fn zero_timescale_header_gives_30s() {
        let dir = clip::test_support::temp_dir("zero-timescale");
        let f = dir.join("zero.mp4");
        std::fs::write(&f, clip::test_support::mp4_bytes_with_timescale(5000, 0)).unwrap();
        assert_eq!(clip_length(&f), Duration::from_secs(30));
        // A duration that would overflow the arithmetic is no better.
        std::fs::write(&f, clip::test_support::mp4_bytes_with_timescale(u32::MAX, 1)).unwrap();
        let d = clip_length(&f);
        assert!(d == Duration::from_secs(30) || d == Duration::from_secs(u32::MAX as u64), "{d:?}");
    }

    async fn live_broadcast(yt: &SimBroadcasts) {
        yt.transition(SEED_BROADCAST, Phase::Testing).await.unwrap();
        yt.transition(SEED_BROADCAST, Phase::Live).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn redundant_transition_is_instant_noop() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let yt = sim.broadcasts();
        live_broadcast(&yt).await;
        let first = sim.lock().last_go_public;
        assert!(first.is_some());
        ms(10_000).await;
        let t = Instant::now();
        yt.transition(SEED_BROADCAST, Phase::Live).await.unwrap();
        assert_eq!(t.elapsed(), Duration::ZERO);
        assert_eq!(sim.lock().last_go_public, first);
        assert!(sim.lock().yt_pending.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn testing_on_live_does_not_regress() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let yt = sim.broadcasts();
        live_broadcast(&yt).await;
        assert!(yt.transition(SEED_BROADCAST, Phase::Testing).await.is_err());
        ms(10_000).await;
        assert_eq!(yt.life_cycle(SEED_BROADCAST).await.unwrap(), "live");
    }

    #[tokio::test(start_paused = true)]
    async fn ready_to_live_is_refused() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let yt = sim.broadcasts();
        let r = yt.transition(SEED_BROADCAST, Phase::Live).await;
        assert!(matches!(&r, Err(DeviceError::Refused(m)) if m.contains("invalid transition")), "{r:?}");
        let r = yt.transition(SEED_BROADCAST, Phase::Complete).await;
        assert!(matches!(r, Err(DeviceError::Refused(_))), "{r:?}");
        assert_eq!(yt.life_cycle(SEED_BROADCAST).await.unwrap(), "ready");
        assert!(sim.lock().yt_pending.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn overlapping_same_target_waits() {
        let sim = SimHandle::new(&cfg(), Speed::X1);
        let yt = sim.broadcasts();
        let t = Instant::now();
        let first = {
            let yt = yt.clone();
            tokio::spawn(async move { yt.transition(SEED_BROADCAST, Phase::Testing).await })
        };
        ms(1000).await;
        // Same target while pending: returns when it lands, at 2.5 s.
        yt.transition(SEED_BROADCAST, Phase::Testing).await.unwrap();
        assert_eq!(t.elapsed(), Duration::from_millis(2500));
        assert_eq!(yt.life_cycle(SEED_BROADCAST).await.unwrap(), "testing");
        first.await.unwrap().unwrap();
        // A different target while one is pending is refused.
        let second = {
            let yt = yt.clone();
            tokio::spawn(async move { yt.transition(SEED_BROADCAST, Phase::Live).await })
        };
        ms(500).await;
        let r = yt.transition(SEED_BROADCAST, Phase::Complete).await;
        assert!(matches!(r, Err(DeviceError::Refused(_))), "{r:?}");
        second.await.unwrap().unwrap();
        assert_eq!(yt.life_cycle(SEED_BROADCAST).await.unwrap(), "live");
    }

    #[tokio::test(start_paused = true)]
    async fn play_after_ended_does_not_end_again() {
        let sim = SimHandle::new(&cfg(), Speed::X10);
        let p = sim.player();
        let mut rx = p.events();
        p.load(&PathBuf::from("none.mp4"), 0.0).await.unwrap();
        p.play().await.unwrap();
        assert_eq!(rx.recv().await, Ok(PlayerEvent::Ended));
        assert!(p.play().await.is_err(), "play with nothing loaded");
        ms(10_000).await;
        assert!(rx.try_recv().is_err(), "second Ended");
    }

    /// A crash (the app dying) in the middle of a video: the next start offers Recover,
    /// and Recover puts input 8 back on Camera 8 with the mics up.
    #[tokio::test(start_paused = true)]
    async fn restart_mid_video_offers_recover() {
        let c = cfg();
        let sim = SimControl::new(&c, Speed::X1);
        let d = sim.devices();
        // On a camera (not the holding still), so the mics are live and get ducked.
        sim.set_program(1);
        sim.set_preview(2);
        let cue = Cue {
            path: PathBuf::from("hymn.mp4"),
            name: "Hymn".into(),
            destination: Destination::HallAndOnline,
            gain_db: 0.0,
            return_input: None,
        };
        let play_task = {
            let (d, c) = (d.clone(), c.clone());
            tokio::spawn(async move {
                let mut levels = LiveLevels::default();
                play(&d, &c, &cue, &mut levels, CancellationToken::new(), Arc::new(Notify::new())).await
            })
        };
        ms(10_000).await;
        let v = sim.snapshot();
        assert_eq!(v.stream_program, c.stream_atem.decklink_input, "video not on air: {v:?}");
        assert!(matches!(v.player, PlayerView::Playing { .. }));
        play_task.abort(); // the crash
        let _ = play_task.await;

        // A new process: new Devices over the same equipment.
        let d2 = build_devices(sim.handle());
        // The encoder was never started in this test; the real one would be streaming.
        d2.encoder.start().await.unwrap();
        ms(4000).await;
        let a = assess(&d2, &c, None, false).await;
        assert!(a.offer.contains(&Offer::Recover), "{a:?}");
        recover(&d2, &c, &LiveLevels::default()).await.unwrap();
        ms(3000).await;
        let v = sim.snapshot();
        assert_eq!(v.input8, Input8Source::Other(CAM8));
        assert_ne!(v.stream_program, c.stream_atem.decklink_input);
        let down = down_threshold(c.stream_atem.duck_db);
        assert!(v.mic_levels.iter().all(|(_, db)| *db > down), "{:?}", v.mic_levels);
        assert!(!v.in8_audio_on);
    }
}
