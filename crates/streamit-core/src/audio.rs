use crate::config::{AudioSourceId, write_atomic};
use crate::devices::{Result, StreamSwitcher};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// Fairlight floor (-infinity).
pub const FLOOR_DB: f32 = -100.0;

const STEPS_PER_SEC: u128 = 20;
const STEP: Duration = Duration::from_millis(50);

/// R21: a mic at or below this level counts as "down" (faded for a video or the
/// holding still), so it is neither captured as a live level nor treated as live:
/// 1 dB above the floor, or above the duck level when that is higher.
pub fn down_threshold(duck_db: f32) -> f32 {
    FLOOR_DB.max(duck_db) + 1.0
}

/// The "live" (un-faded) level of each mic, in dB.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveLevels(pub HashMap<AudioSourceId, f32>);

/// On-disk form of `LiveLevels`: one `[[mic]]` table per source.
#[derive(Serialize, Deserialize, Default)]
struct LevelsFile {
    #[serde(default)]
    mic: Vec<MicLevel>,
}

#[derive(Serialize, Deserialize)]
struct MicLevel {
    input: u16,
    source: i64,
    db: f32,
}

impl LiveLevels {
    /// I5: load the levels saved by `save`. A missing file is no levels.
    pub fn load(path: &Path) -> anyhow::Result<LiveLevels> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LiveLevels::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let f: LevelsFile = toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
        Ok(LiveLevels(f.mic.into_iter().map(|m| (AudioSourceId { input: m.input, source: m.source }, m.db)).collect()))
    }

    /// Save atomically (temp file, then rename), sorted so the file is stable.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut mic: Vec<MicLevel> =
            self.0.iter().map(|(s, db)| MicLevel { input: s.input, source: s.source, db: *db }).collect();
        mic.sort_by_key(|m| (m.input, m.source));
        let text = toml::to_string_pretty(&LevelsFile { mic }).context("serializing mic levels")?;
        write_atomic(path, &text)
    }

    /// Read current levels; sources at or below `down` (see `down_threshold`) are not stored.
    pub async fn capture(sw: &dyn StreamSwitcher, srcs: &[AudioSourceId], down: f32) -> Result<LiveLevels> {
        let mut l = LiveLevels::default();
        l.recapture(sw, srcs, down).await?;
        Ok(l)
    }

    /// Refresh levels, keeping the previous value for any source at or below `down`.
    pub async fn recapture(&mut self, sw: &dyn StreamSwitcher, srcs: &[AudioSourceId], down: f32) -> Result<()> {
        for &s in srcs {
            let db = sw.audio_level(s).await?;
            if db > down {
                self.0.insert(s, db);
            }
        }
        Ok(())
    }
}

/// True when every source reads at or below `down` (and there is at least one). A failed
/// read counts as "not down".
pub async fn all_down(sw: &dyn StreamSwitcher, srcs: &[AudioSourceId], down: f32) -> bool {
    if srcs.is_empty() {
        return false;
    }
    for &s in srcs {
        if !sw.audio_level(s).await.is_ok_and(|db| db <= down) {
            return false;
        }
    }
    true
}

pub enum Target<'a> {
    Floor,
    /// One level for every source (the duck level).
    Level(f32),
    Live(&'a LiveLevels),
}

impl Target<'_> {
    /// R21: where a roll-in fades the mics: `duck_db`, or the floor when that is at or below it.
    pub fn duck(duck_db: f32) -> Target<'static> {
        if duck_db <= FLOOR_DB { Target::Floor } else { Target::Level(duck_db) }
    }

    fn for_src(&self, s: AudioSourceId) -> f32 {
        match self {
            Target::Floor => FLOOR_DB,
            Target::Level(db) => *db,
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
    use crate::fakes::{Call, fake_devices};
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
        let live = LiveLevels::capture(&*d.stream, &[MIC], down_threshold(FLOOR_DB)).await.unwrap();
        fade(&*d.stream, &[MIC], Target::Floor, Duration::from_secs(1)).await.unwrap();
        fade(&*d.stream, &[MIC], Target::Live(&live), Duration::from_secs(1)).await.unwrap();
        assert_eq!(*level_calls(log.calls()).last().unwrap(), -3.5);
    }

    #[tokio::test(start_paused = true)]
    async fn capture_ignores_floored_source() {
        let (d, _log, h) = fake_devices();
        h.stream.set_audio_level_state(MIC, -6.0);
        let mut live = LiveLevels::capture(&*d.stream, &[MIC], down_threshold(FLOOR_DB)).await.unwrap();
        h.stream.set_audio_level_state(MIC, FLOOR_DB);
        live.recapture(&*d.stream, &[MIC], down_threshold(FLOOR_DB)).await.unwrap();
        assert_eq!(live.0[&MIC], -6.0);
    }

    #[test]
    fn down_threshold_is_one_db_above_floor_or_duck_level() {
        assert_eq!(down_threshold(FLOOR_DB), FLOOR_DB + 1.0);
        assert_eq!(down_threshold(-150.0), FLOOR_DB + 1.0);
        assert_eq!(down_threshold(-20.0), -19.0);
    }

    #[tokio::test(start_paused = true)]
    async fn duck_target_is_duck_db_or_floor() {
        for (duck, lands) in [(-20.0, -20.0), (FLOOR_DB, FLOOR_DB), (-120.0, FLOOR_DB)] {
            let (d, log, h) = fake_devices();
            h.stream.set_audio_level_state(MIC, 0.0);
            fade(&*d.stream, &[MIC], Target::duck(duck), Duration::from_secs(1)).await.unwrap();
            assert_eq!(*level_calls(log.calls()).last().unwrap(), lands, "duck {duck}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn capture_ignores_source_at_duck_level() {
        let (d, _log, h) = fake_devices();
        h.stream.set_audio_level_state(MIC, -6.0);
        let mut live = LiveLevels::capture(&*d.stream, &[MIC], down_threshold(-20.0)).await.unwrap();
        h.stream.set_audio_level_state(MIC, -20.0);
        live.recapture(&*d.stream, &[MIC], down_threshold(-20.0)).await.unwrap();
        assert_eq!(live.0[&MIC], -6.0, "a ducked mic is not its live level");
    }

    #[test]
    fn live_levels_save_load_roundtrip() {
        let dir = std::env::temp_dir().join("streamit-levels-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("levels.toml");
        // No file yet: no levels.
        assert_eq!(LiveLevels::load(&path).unwrap(), LiveLevels::default());
        let other = AudioSourceId { input: 1002, source: -256 };
        let l = LiveLevels([(MIC, -3.5), (other, -12.25)].into_iter().collect());
        l.save(&path).unwrap();
        assert_eq!(LiveLevels::load(&path).unwrap(), l);
        assert!(!crate::config::temp_path(&path).exists(), "saved through a temp file");
        std::fs::write(&path, "not [ toml").unwrap();
        assert!(LiveLevels::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(start_paused = true)]
    async fn zero_duration_is_single_set() {
        let (d, log, h) = fake_devices();
        h.stream.set_audio_level_state(MIC, 0.0);
        fade(&*d.stream, &[MIC], Target::Floor, Duration::ZERO).await.unwrap();
        assert_eq!(level_calls(log.calls()), vec![FLOOR_DB]);
    }
}
