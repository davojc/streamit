//! Pretend Web Presenter, DeckLink player and NDI counter.
use crate::{Fault, PlayerState, SimHandle, clip};
use async_trait::async_trait;
use std::path::Path;
use std::time::Duration;
use streamit_core::devices::*;
use streamit_core::engine::DeviceKind;
use tokio::time::Instant;
use tracing::Instrument;

/// Connecting becomes Streaming after this long at 1x.
const CONNECT: Duration = Duration::from_secs(3);
/// The stream's health is Good this long after the encoder starts streaming (1x).
const HEALTH_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct SimEncoder {
    sim: SimHandle,
}

impl SimEncoder {
    pub fn new(sim: SimHandle) -> Self {
        Self { sim }
    }
}

#[async_trait]
impl Encoder for SimEncoder {
    /// Starting an encoder that is already connecting or streaming changes nothing.
    async fn start(&self) -> Result<()> {
        let mut w = self.sim.lock();
        w.check_plugged(DeviceKind::WebPresenter)?;
        if matches!(w.encoder, EncoderStatus::Connecting | EncoderStatus::Streaming) {
            return Ok(());
        }
        let streaming_at = Instant::now() + self.sim.clock.scale(CONNECT);
        w.encoder = EncoderStatus::Connecting;
        w.encoder_at = Some(streaming_at);
        w.health_at = Some(streaming_at + self.sim.clock.scale(HEALTH_DELAY));
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let mut w = self.sim.lock();
        w.check_plugged(DeviceKind::WebPresenter)?;
        w.encoder = EncoderStatus::Idle;
        w.encoder_at = None;
        w.health_at = None;
        Ok(())
    }

    async fn status(&self) -> Result<EncoderStatus> {
        let w = self.sim.lock();
        w.check_plugged(DeviceKind::WebPresenter)?;
        Ok(w.encoder.clone())
    }
}

/// Longest the end-of-video timer sleeps in one go, so a speed change is noticed quickly.
const TIMER_STEP: Duration = Duration::from_millis(200);
/// A video this close to its end has ended (timer granularity is a millisecond).
const END_SLACK: Duration = Duration::from_millis(2);

#[derive(Clone)]
pub struct SimPlayer {
    sim: SimHandle,
}

impl SimPlayer {
    pub fn new(sim: SimHandle) -> Self {
        Self { sim }
    }

    /// The end-of-video timer for play number `gen`. A deadline cannot emit an event
    /// unprompted, so this small task does. Every check and the send happen under the
    /// world lock, and `load`, `stop` and `reset` bump the generation under the same
    /// lock, so a timer from an earlier play can never emit.
    fn spawn_end_timer(&self, gen_: u64) {
        let sim = self.sim.clone();
        tokio::spawn(
            async move {
                loop {
                    let wait = {
                        let mut w = sim.lock();
                        if w.player_gen != gen_ {
                            return;
                        }
                        let PlayerState::Playing { duration, .. } = w.player else { return };
                        let position = w.player.position(sim.clock.speed().factor()).unwrap_or_default();
                        let left = duration - position;
                        if left <= END_SLACK {
                            // Ended: nothing is loaded any more, so a further play() is refused.
                            w.player = PlayerState::Empty;
                            let _ = w.player_events.send(PlayerEvent::Ended);
                            return;
                        }
                        sim.clock.scale(left).min(TIMER_STEP)
                    };
                    tokio::time::sleep(wait).await;
                }
            }
            .in_current_span(),
        );
    }
}

#[async_trait]
impl Player for SimPlayer {
    async fn load(&self, path: &Path, _gain_db: f32) -> Result<Duration> {
        {
            let mut w = self.sim.lock();
            w.check_plugged(DeviceKind::DeckLink)?;
            if w.faults.remove(&Fault::NextVideoFails) {
                return Err(DeviceError::Other("could not open the video (simulation)".into()));
            }
        }
        // File I/O, so not under the world lock.
        let duration = clip::clip_length(path);
        let mut w = self.sim.lock();
        w.player_gen += 1;
        w.player = PlayerState::Loaded { duration };
        Ok(duration)
    }

    async fn play(&self) -> Result<()> {
        let gen_ = {
            let mut w = self.sim.lock();
            w.check_plugged(DeviceKind::DeckLink)?;
            let duration = match w.player {
                PlayerState::Loaded { duration } => duration,
                PlayerState::Playing { .. } => return Ok(()),
                PlayerState::Empty => return Err(DeviceError::Other("no video loaded (simulation)".into())),
            };
            w.player = PlayerState::Playing { duration, anchor_pos: Duration::ZERO, anchor_at: Instant::now() };
            w.player_gen
        };
        self.spawn_end_timer(gen_);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let mut w = self.sim.lock();
        w.player_gen += 1;
        w.player = PlayerState::Empty;
        Ok(())
    }

    fn position(&self) -> Option<Duration> {
        self.sim.lock().player.position(self.sim.clock.speed().factor())
    }

    fn events(&self) -> tokio::sync::broadcast::Receiver<PlayerEvent> {
        self.sim.lock().player_events.subscribe()
    }
}

#[derive(Clone)]
pub struct SimCounter {
    sim: SimHandle,
}

impl SimCounter {
    pub fn new(sim: SimHandle) -> Self {
        Self { sim }
    }
}

/// A pretend ETC Eos console: firing a cue makes it the active one at once.
#[derive(Clone)]
pub struct SimLighting {
    sim: SimHandle,
}

impl SimLighting {
    pub fn new(sim: SimHandle) -> Self {
        Self { sim }
    }
}

#[async_trait]
impl Lighting for SimLighting {
    async fn fire_cue(&self, list: u32, cue: &str) -> Result<()> {
        let mut w = self.sim.lock();
        w.check_plugged(DeviceKind::Lighting)?;
        w.lighting_cue = Some(format!("{list}/{}", cue.trim()));
        Ok(())
    }

    async fn status(&self) -> Result<LightingStatus> {
        let w = self.sim.lock();
        w.check_plugged(DeviceKind::Lighting)?;
        Ok(LightingStatus { show: Some("Pretend show".into()), active_cue: w.lighting_cue.clone() })
    }
}

#[async_trait]
impl Counter for SimCounter {
    async fn show(&self, remaining: Duration) -> Result<()> {
        let mut w = self.sim.lock();
        w.check_plugged(DeviceKind::Ndi)?;
        w.counter = Some(remaining);
        Ok(())
    }

    async fn clear(&self) -> Result<()> {
        let mut w = self.sim.lock();
        w.check_plugged(DeviceKind::Ndi)?;
        w.counter = None;
        Ok(())
    }
}
