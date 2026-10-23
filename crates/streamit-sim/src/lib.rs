//! Pretend equipment so the operator can practise a service with no hardware.
//!
//! One shared [`SimWorld`] holds every pretend device's state; each device handle
//! (switcher, router, and in Task 5 the encoder, YouTube, player and counter) is a
//! thin view over it plus the shared [`SimClock`]. Delayed effects are stored with a
//! deadline and applied lazily when read, so no background tasks are needed.
pub mod clock;
pub mod switcher;

pub use clock::{SimClock, Speed};
pub use switcher::{SimRouter, SimSwitcher, Which};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use streamit_core::config::{AudioSourceId, Config};
use streamit_core::devices::{BroadcastSummary, DeviceError, EncoderStatus};
use streamit_core::engine::DeviceKind;
use tokio::time::Instant;

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
    Loaded { duration: Duration },
    Playing { duration: Duration, started: Instant },
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
    pub broadcasts: Vec<BroadcastSummary>,
    pub player: PlayerState,
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
            input8_audio_on: true,
            routes,
            pending_routes: BTreeMap::new(),
            encoder: EncoderStatus::Idle,
            broadcasts: Vec::new(),
            player: PlayerState::Empty,
            counter: None,
            faults: HashSet::new(),
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
}

/// Everything a pretend device needs: the shared world and the shared clock.
#[derive(Clone)]
pub struct SimHandle {
    pub world: Shared,
    pub clock: SimClock,
}

impl SimHandle {
    pub fn new(cfg: &Config, speed: Speed) -> Self {
        Self { world: Arc::new(Mutex::new(SimWorld::seeded(cfg))), clock: SimClock::new(speed) }
    }

    pub fn lock(&self) -> MutexGuard<'_, SimWorld> {
        self.world.lock().unwrap_or_else(|e| e.into_inner())
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
}
