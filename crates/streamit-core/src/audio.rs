use crate::config::AudioSourceId;
use crate::devices::{Result, StreamSwitcher};
use std::collections::HashMap;
use std::time::Duration;

/// Fairlight floor (-infinity).
pub const FLOOR_DB: f32 = -100.0;

const STEPS_PER_SEC: u128 = 20;
const STEP: Duration = Duration::from_millis(50);

/// The "live" (un-faded) level of each mic, in dB.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveLevels(pub HashMap<AudioSourceId, f32>);

impl LiveLevels {
    /// Read current levels; floored sources are not stored.
    pub async fn capture(sw: &dyn StreamSwitcher, srcs: &[AudioSourceId]) -> Result<LiveLevels> {
        let mut l = LiveLevels::default();
        l.recapture(sw, srcs).await?;
        Ok(l)
    }

    /// Refresh levels, keeping the previous value for any source at/near the floor.
    pub async fn recapture(&mut self, sw: &dyn StreamSwitcher, srcs: &[AudioSourceId]) -> Result<()> {
        for &s in srcs {
            let db = sw.audio_level(s).await?;
            if db > FLOOR_DB + 1.0 {
                self.0.insert(s, db);
            }
        }
        Ok(())
    }
}

pub enum Target<'a> {
    Floor,
    Live(&'a LiveLevels),
}

impl Target<'_> {
    fn for_src(&self, s: AudioSourceId) -> f32 {
        match self {
            Target::Floor => FLOOR_DB,
            Target::Live(l) => l.0.get(&s).copied().unwrap_or(0.0),
        }
    }
}

/// Linear-in-dB fade at 20 steps/s from each source's current level.
pub async fn fade(sw: &dyn StreamSwitcher, srcs: &[AudioSourceId], to: Target<'_>, over: Duration) -> Result<()> {
    let mut starts = Vec::with_capacity(srcs.len());
    for &s in srcs {
        starts.push((s, sw.audio_level(s).await?, to.for_src(s)));
    }
    let steps = (over.as_millis() * STEPS_PER_SEC / 1000) as u32;
    if steps <= 1 {
        for &(s, _, t) in &starts {
            sw.set_audio_level(s, t).await?;
        }
        return Ok(());
    }
    let mut iv = tokio::time::interval(STEP);
    iv.tick().await; // first tick is immediate
    for k in 1..=steps {
        iv.tick().await;
        for &(s, from, t) in &starts {
            let db = if k == steps { t } else { from + (t - from) * k as f32 / steps as f32 };
            sw.set_audio_level(s, db).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AudioSourceId;
    use crate::fakes::{fake_devices, Call};
    use std::time::Duration;

    const MIC: AudioSourceId = AudioSourceId { input: 1001, source: -65280 };

    fn level_calls(calls: Vec<Call>) -> Vec<f32> {
        calls
            .into_iter()
            .filter_map(|c| match c {
                Call::StreamSetAudioLevel(_, db) => Some(db),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn fade_down_steps_at_20hz_and_lands_on_floor() {
        let (d, log, h) = fake_devices();
        h.stream.set_audio_level_state(MIC, 0.0);
        fade(&*d.stream, &[MIC], Target::Floor, Duration::from_secs(1)).await.unwrap();
        let v = level_calls(log.calls());
        assert_eq!(v.len(), 20);
        assert!((v[0] - -5.0).abs() < 1e-4);
        assert_eq!(*v.last().unwrap(), FLOOR_DB);
    }

    #[tokio::test(start_paused = true)]
    async fn fade_up_returns_to_captured_live_level() {
        let (d, log, h) = fake_devices();
        h.stream.set_audio_level_state(MIC, -3.5);
        let live = LiveLevels::capture(&*d.stream, &[MIC]).await.unwrap();
        fade(&*d.stream, &[MIC], Target::Floor, Duration::from_secs(1)).await.unwrap();
        fade(&*d.stream, &[MIC], Target::Live(&live), Duration::from_secs(1)).await.unwrap();
        assert_eq!(*level_calls(log.calls()).last().unwrap(), -3.5);
    }

    #[tokio::test(start_paused = true)]
    async fn capture_ignores_floored_source() {
        let (d, _log, h) = fake_devices();
        h.stream.set_audio_level_state(MIC, -6.0);
        let mut live = LiveLevels::capture(&*d.stream, &[MIC]).await.unwrap();
        h.stream.set_audio_level_state(MIC, FLOOR_DB);
        live.recapture(&*d.stream, &[MIC]).await.unwrap();
        assert_eq!(live.0[&MIC], -6.0);
    }

    #[tokio::test(start_paused = true)]
    async fn zero_duration_is_single_set() {
        let (d, log, h) = fake_devices();
        h.stream.set_audio_level_state(MIC, 0.0);
        fade(&*d.stream, &[MIC], Target::Floor, Duration::ZERO).await.unwrap();
        assert_eq!(level_calls(log.calls()), vec![FLOOR_DB]);
    }
}
