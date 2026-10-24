//! Pretend ATEM switchers and VideoHub router.
use crate::{Fault, Mix, PendingRoute, SimHandle, SimWorld, SwitcherState};
use async_trait::async_trait;
use std::time::Duration;
use streamit_core::config::AudioSourceId;
use streamit_core::devices::{DeviceError, LabelKind, Result, Router, StreamSwitcher, Switcher};
use streamit_core::engine::DeviceKind;
use tokio::time::Instant;

/// A pretend mix and a pretend route both take this long at 1x.
pub(crate) const PRETEND_DELAY: Duration = Duration::from_secs(1);

/// The pretend switcher's input number for media player `player` (as on an ATEM: 3010
/// for player 1).
pub fn media_player_input(player: u8) -> u16 {
    3000 + 10 * player as u16
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Stream,
    Screens,
}

impl Which {
    fn kind(self) -> DeviceKind {
        match self {
            Which::Stream => DeviceKind::StreamAtem,
            Which::Screens => DeviceKind::ScreensAtem,
        }
    }
}

#[derive(Clone)]
pub struct SimSwitcher {
    sim: SimHandle,
    which: Which,
}

impl SimSwitcher {
    pub fn new(sim: SimHandle, which: Which) -> Self {
        Self { sim, which }
    }

    /// Runs `f` on the settled world, or fails if this switcher is unplugged.
    fn with<T>(&self, f: impl FnOnce(&mut SimWorld) -> T) -> Result<T> {
        let mut w = self.sim.lock();
        w.check_plugged(self.which.kind())?;
        Ok(f(&mut w))
    }

    fn state<T>(&self, f: impl FnOnce(&mut SwitcherState) -> T) -> Result<T> {
        self.with(|w| f(w.switcher_mut(self.which)))
    }
}

#[async_trait]
impl Switcher for SimSwitcher {
    async fn program(&self) -> Result<u16> {
        self.state(|s| s.program)
    }

    async fn preview(&self) -> Result<u16> {
        self.state(|s| s.preview)
    }

    async fn set_preview(&self, input: u16) -> Result<()> {
        self.state(|s| s.preview = input)
    }

    /// A cut: takes effect at once and cancels any mix in progress.
    async fn set_program(&self, input: u16) -> Result<()> {
        self.state(|s| {
            s.program = input;
            s.mix = None;
        })
    }

    async fn auto(&self) -> Result<()> {
        let done_at = Instant::now() + self.sim.clock.scale(PRETEND_DELAY);
        // A real ATEM ignores an auto while a transition is running.
        self.state(|s| {
            if s.mix.is_none() {
                s.mix = Some(Mix { to: s.preview, done_at });
            }
        })
    }

    async fn wait_transition_done(&self, timeout: Duration) -> Result<()> {
        let done_at = self.state(|s| s.mix.map(|m| m.done_at))?;
        // No mix in progress: nothing to wait for.
        let Some(done_at) = done_at else { return Ok(()) };
        if done_at > Instant::now() + timeout {
            tokio::time::sleep(timeout).await;
            return Err(DeviceError::Timeout("transition did not finish (simulation)".into()));
        }
        tokio::time::sleep_until(done_at).await;
        self.with(|_| ())
    }
}

#[async_trait]
impl StreamSwitcher for SimSwitcher {
    async fn set_media_player_still(&self, _player: u8, _still: u16) -> Result<()> {
        self.with(|_| ())
    }

    fn media_player_input(&self, player: u8) -> u16 {
        media_player_input(player)
    }

    async fn audio_level(&self, src: AudioSourceId) -> Result<f32> {
        self.with(|w| w.fairlight.get(&src).copied().unwrap_or(0.0))
    }

    async fn set_audio_level(&self, src: AudioSourceId, db: f32) -> Result<()> {
        self.with(|w| {
            if w.has_fault(Fault::MicFadeFails) {
                return Err(DeviceError::Refused("stream audio fade failed (simulation)".into()));
            }
            w.fairlight.insert(src, db);
            Ok(())
        })?
    }

    async fn set_audio_on(&self, src: AudioSourceId, on: bool) -> Result<()> {
        self.with(|w| {
            if src.input == w.decklink_input {
                w.input8_audio_on = on;
            }
        })
    }
}

/// The pretend VideoHub's input labels (input 1 first); the rest have none.
const SIM_LABELS: [&str; 10] = [
    "Camera 1 Wide",
    "Camera 2 Pulpit",
    "Camera 3 Choir",
    "Camera 4 Organ",
    "ProPresenter",
    "Stream ATEM PGM",
    "Screens ATEM PGM",
    "Stream ATEM PVW",
    "Lectern laptop",
    "Hall camera",
];
/// How many inputs and outputs the pretend VideoHub has.
pub(crate) const SIM_PORTS: usize = 40;

/// The pretend VideoHub's input labels at the start.
pub(crate) fn sim_input_labels() -> Vec<String> {
    (0..SIM_PORTS).map(|i| SIM_LABELS.get(i).map_or_else(String::new, |l| l.to_string())).collect()
}

#[derive(Clone)]
pub struct SimRouter {
    sim: SimHandle,
}

impl SimRouter {
    pub fn new(sim: SimHandle) -> Self {
        Self { sim }
    }
}

#[async_trait]
impl Router for SimRouter {
    async fn route_of(&self, output: u16) -> Result<u16> {
        let w = self.sim.lock();
        w.check_plugged(DeviceKind::VideoHub)?;
        Ok(w.routes.get(&output).copied().unwrap_or(0))
    }

    /// Accepted at once; the route applies after the (scaled) pretend delay.
    async fn route(&self, output: u16, input: u16) -> Result<()> {
        let done_at = Instant::now() + self.sim.clock.scale(PRETEND_DELAY);
        let mut w = self.sim.lock(); // settles any route already due
        w.check_plugged(DeviceKind::VideoHub)?;
        w.pending_routes.insert(output, PendingRoute { input, done_at });
        Ok(())
    }

    async fn input_labels(&self) -> Result<Vec<String>> {
        let w = self.sim.lock();
        w.check_plugged(DeviceKind::VideoHub)?;
        Ok(w.input_labels.clone())
    }

    async fn output_labels(&self) -> Result<Vec<String>> {
        let w = self.sim.lock();
        w.check_plugged(DeviceKind::VideoHub)?;
        Ok(w.output_labels.clone())
    }

    async fn set_label(&self, kind: LabelKind, n: u16, text: &str) -> Result<()> {
        let mut w = self.sim.lock();
        w.check_plugged(DeviceKind::VideoHub)?;
        let labels = match kind {
            LabelKind::Input => &mut w.input_labels,
            LabelKind::Output => &mut w.output_labels,
        };
        match n.checked_sub(1).and_then(|i| labels.get_mut(i as usize)) {
            Some(l) => {
                *l = text.trim().to_string();
                Ok(())
            }
            None => Err(DeviceError::Refused(format!("there is no {kind:?} {n}").to_lowercase())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Speed;
    use streamit_core::config::Config;

    fn sim(speed: Speed) -> SimHandle {
        let mut cfg = Config::default();
        cfg.videohub.input8_output = 5;
        SimHandle::new(&cfg, speed)
    }

    #[tokio::test(start_paused = true)]
    async fn seeded_world_matches_defaults() {
        let s = sim(Speed::X1);
        let a = s.stream_switcher();
        assert_eq!(a.program().await.unwrap(), 3010);
        assert_eq!(a.preview().await.unwrap(), 1);
        let mic = Config::default().stream_atem.mic_sources[0];
        assert_eq!(a.audio_level(mic).await.unwrap(), -3.0);
        assert_eq!(s.screens_switcher().program().await.unwrap(), 6);
        assert_eq!(s.router().route_of(5).await.unwrap(), 13);
        assert_eq!(a.media_player_input(1), 3010);
    }

    #[tokio::test(start_paused = true)]
    async fn auto_reports_old_program_until_mix_completes() {
        let s = sim(Speed::X1);
        let a = s.stream_switcher();
        a.set_preview(2).await.unwrap();
        a.auto().await.unwrap();
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(a.program().await.unwrap(), 3010);
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(a.program().await.unwrap(), 2);
        assert_eq!(a.preview().await.unwrap(), 3010);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_transition_done_waits_for_mix() {
        let s = sim(Speed::X1);
        let a = s.stream_switcher();
        a.set_preview(2).await.unwrap();
        a.auto().await.unwrap();
        let t = Instant::now();
        a.wait_transition_done(Duration::from_secs(5)).await.unwrap();
        assert_eq!(t.elapsed(), Duration::from_secs(1));
        assert_eq!(a.program().await.unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn speed_x4_mix_takes_250ms() {
        let s = sim(Speed::X4);
        let a = s.stream_switcher();
        a.set_preview(2).await.unwrap();
        a.auto().await.unwrap();
        tokio::time::advance(Duration::from_millis(200)).await;
        assert_eq!(a.program().await.unwrap(), 3010);
        tokio::time::advance(Duration::from_millis(50)).await;
        assert_eq!(a.program().await.unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn route_applies_after_one_second() {
        let s = sim(Speed::X1);
        let r = s.router();
        r.route(5, 8).await.unwrap();
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(r.route_of(5).await.unwrap(), 13);
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(r.route_of(5).await.unwrap(), 8);
    }

    #[tokio::test(start_paused = true)]
    async fn unplugged_switcher_reports_disconnected() {
        let s = sim(Speed::X1);
        s.set_fault(Fault::Unplug(DeviceKind::StreamAtem), true);
        let err = DeviceError::Disconnected("unplugged (simulation)".into());
        let a = s.stream_switcher();
        assert_eq!(a.program().await, Err(err.clone()));
        assert_eq!(a.auto().await, Err(err.clone()));
        // The other switcher and the router are unaffected.
        assert!(s.screens_switcher().program().await.is_ok());
        s.set_fault(Fault::Unplug(DeviceKind::VideoHub), true);
        assert_eq!(s.router().route(5, 1).await, Err(err));
    }

    #[tokio::test(start_paused = true)]
    async fn mic_fade_fault_fails_set_audio_level() {
        let s = sim(Speed::X1);
        let a = s.stream_switcher();
        let mic = Config::default().stream_atem.mic_sources[0];
        a.set_audio_level(mic, -10.0).await.unwrap();
        assert_eq!(a.audio_level(mic).await.unwrap(), -10.0);
        s.set_fault(Fault::MicFadeFails, true);
        assert!(a.set_audio_level(mic, -20.0).await.is_err());
        assert_eq!(a.audio_level(mic).await.unwrap(), -10.0);
    }

    #[tokio::test(start_paused = true)]
    async fn cut_cancels_mix_and_audio_on_tracks_input8() {
        let s = sim(Speed::X1);
        let a = s.stream_switcher();
        a.set_preview(2).await.unwrap();
        a.auto().await.unwrap();
        a.set_program(3).await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(a.program().await.unwrap(), 3);
        a.set_audio_on(AudioSourceId { input: 8, source: 0 }, true).await.unwrap();
        assert!(s.lock().input8_audio_on);
        a.set_audio_on(AudioSourceId { input: 8, source: 0 }, false).await.unwrap();
        assert!(!s.lock().input8_audio_on);
    }

    #[tokio::test(start_paused = true)]
    async fn second_auto_during_mix_is_ignored() {
        let s = sim(Speed::X1);
        let a = s.stream_switcher();
        a.set_preview(2).await.unwrap();
        a.auto().await.unwrap();
        tokio::time::advance(Duration::from_millis(500)).await;
        a.auto().await.unwrap();
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(a.program().await.unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn preview_change_mid_mix_keeps_original_target() {
        let s = sim(Speed::X1);
        let a = s.stream_switcher();
        a.set_preview(2).await.unwrap();
        a.auto().await.unwrap();
        a.set_preview(3).await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(a.program().await.unwrap(), 2);
        assert_eq!(a.preview().await.unwrap(), 3010);
    }

    #[tokio::test(start_paused = true)]
    async fn due_route_is_not_lost_by_a_new_route() {
        let s = sim(Speed::X1);
        let r = s.router();
        r.route(5, 8).await.unwrap();
        tokio::time::advance(Duration::from_millis(1500)).await;
        r.route(5, 9).await.unwrap();
        assert_eq!(r.route_of(5).await.unwrap(), 8);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(r.route_of(5).await.unwrap(), 9);
    }

    #[tokio::test(start_paused = true)]
    async fn lock_shows_settled_state_without_a_device_call() {
        let s = sim(Speed::X1);
        let a = s.stream_switcher();
        a.set_preview(2).await.unwrap();
        a.auto().await.unwrap();
        s.router().route(5, 8).await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        let w = s.lock();
        assert_eq!(w.stream.program, 2);
        assert_eq!(w.routes[&5], 8);
    }

    #[tokio::test(start_paused = true)]
    async fn unplug_covers_router_screens_and_audio_level() {
        let s = sim(Speed::X1);
        let err = DeviceError::Disconnected("unplugged (simulation)".into());
        let mic = Config::default().stream_atem.mic_sources[0];
        s.set_fault(Fault::Unplug(DeviceKind::VideoHub), true);
        assert_eq!(s.router().route_of(5).await, Err(err.clone()));
        s.set_fault(Fault::Unplug(DeviceKind::ScreensAtem), true);
        assert_eq!(s.screens_switcher().set_program(1).await, Err(err.clone()));
        s.set_fault(Fault::Unplug(DeviceKind::StreamAtem), true);
        assert_eq!(s.stream_switcher().set_audio_level(mic, -5.0).await, Err(err));
    }
}
