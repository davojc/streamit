//! Pretend ATEM switchers and VideoHub router.
use crate::{Fault, Mix, PendingRoute, SimHandle, SimWorld, SwitcherState};
use async_trait::async_trait;
use std::time::Duration;
use streamit_core::config::AudioSourceId;
use streamit_core::devices::{DeviceError, Result, Router, StreamSwitcher, Switcher};
use streamit_core::engine::DeviceKind;
use tokio::time::Instant;

/// A pretend mix and a pretend route both take this long at 1x.
const PRETEND_DELAY: Duration = Duration::from_secs(1);

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

    /// Runs `f` on this switcher's settled state, or fails if it is unplugged.
    fn with<T>(&self, f: impl FnOnce(&mut SwitcherState, &mut SimWorld) -> T) -> Result<T> {
        let mut w = self.sim.lock();
        w.check_plugged(self.which.kind())?;
        // Take the state out so `f` can also touch the rest of the world.
        let blank = SwitcherState::new(0, 0);
        let mut st = match self.which {
            Which::Stream => std::mem::replace(&mut w.stream, blank),
            Which::Screens => std::mem::replace(&mut w.screens, blank),
        };
        st.settle();
        let out = f(&mut st, &mut w);
        match self.which {
            Which::Stream => w.stream = st,
            Which::Screens => w.screens = st,
        }
        Ok(out)
    }
}

#[async_trait]
impl Switcher for SimSwitcher {
    async fn program(&self) -> Result<u16> {
        self.with(|s, _| s.program)
    }

    async fn preview(&self) -> Result<u16> {
        self.with(|s, _| s.preview)
    }

    async fn set_preview(&self, input: u16) -> Result<()> {
        self.with(|s, _| s.preview = input)
    }

    /// A cut: takes effect at once and cancels any mix in progress.
    async fn set_program(&self, input: u16) -> Result<()> {
        self.with(|s, _| {
            s.program = input;
            s.mix = None;
        })
    }

    async fn auto(&self) -> Result<()> {
        let done_at = Instant::now() + self.sim.clock.scale(PRETEND_DELAY);
        self.with(|s, _| s.mix = Some(Mix { to: s.preview, done_at }))
    }

    async fn wait_transition_done(&self, timeout: Duration) -> Result<()> {
        let done_at = self.with(|s, _| s.mix.map(|m| m.done_at))?;
        let Some(done_at) = done_at else { return Ok(()) };
        if done_at > Instant::now() + timeout {
            tokio::time::sleep(timeout).await;
            return Err(DeviceError::Timeout("transition did not finish (simulation)".into()));
        }
        tokio::time::sleep_until(done_at).await;
        self.with(|_, _| ())
    }
}

#[async_trait]
impl StreamSwitcher for SimSwitcher {
    async fn set_media_player_still(&self, _player: u8, _still: u16) -> Result<()> {
        self.with(|_, _| ())
    }

    fn media_player_input(&self, player: u8) -> u16 {
        3000 + 10 * player as u16
    }

    async fn audio_level(&self, src: AudioSourceId) -> Result<f32> {
        self.with(|_, w| w.fairlight.get(&src).copied().unwrap_or(0.0))
    }

    async fn set_audio_level(&self, src: AudioSourceId, db: f32) -> Result<()> {
        self.with(|_, w| {
            if w.has_fault(Fault::MicFadeFails) {
                return Err(DeviceError::Refused("mic fade failed (simulation)".into()));
            }
            w.fairlight.insert(src, db);
            Ok(())
        })?
    }

    async fn set_audio_on(&self, src: AudioSourceId, on: bool) -> Result<()> {
        self.with(|_, w| {
            if src.input == w.decklink_input {
                w.input8_audio_on = on;
            }
        })
    }
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
        let mut w = self.sim.lock();
        w.check_plugged(DeviceKind::VideoHub)?;
        if let Some(p) = w.pending_routes.get(&output).copied()
            && Instant::now() >= p.done_at
        {
            w.pending_routes.remove(&output);
            w.routes.insert(output, p.input);
        }
        Ok(w.routes.get(&output).copied().unwrap_or(0))
    }

    /// Accepted at once; the route applies after the (scaled) pretend delay.
    async fn route(&self, output: u16, input: u16) -> Result<()> {
        let done_at = Instant::now() + self.sim.clock.scale(PRETEND_DELAY);
        let mut w = self.sim.lock();
        w.check_plugged(DeviceKind::VideoHub)?;
        w.pending_routes.insert(output, PendingRoute { input, done_at });
        Ok(())
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
        a.set_audio_on(AudioSourceId { input: 8, source: 0 }, false).await.unwrap();
        assert!(!s.lock().input8_audio_on);
    }
}
