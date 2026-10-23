//! StreamIt configuration: device addresses, input numbers and timings.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::rollin::Cue;

/// Serialize a `Duration` as whole milliseconds.
mod millis {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(d)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioSourceId {
    pub input: u16,
    pub source: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamAtemConfig {
    pub address: Option<String>,
    pub media_player: u8,
    pub opening_still: u16,
    pub closing_still: u16,
    pub decklink_input: u16,
    pub live_inputs: Vec<u16>,
    pub default_live_input: u16,
    pub mic_sources: Vec<AudioSourceId>,
    pub duck_db: f32,
    #[serde(with = "millis")]
    pub fade: Duration,
}

impl Default for StreamAtemConfig {
    fn default() -> Self {
        Self {
            address: Some("192.168.4.123".into()),
            media_player: 1,
            opening_still: 0,
            closing_still: 1,
            decklink_input: 8,
            live_inputs: vec![1, 2, 3],
            default_live_input: 1,
            mic_sources: vec![AudioSourceId { input: 1001, source: -65280 }],
            duck_db: -100.0,
            fade: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreensAtemConfig {
    pub address: Option<String>,
    pub decklink_input: u16,
    pub t0_input: u16,
}

impl Default for ScreensAtemConfig {
    fn default() -> Self {
        Self { address: Some("192.168.4.98".into()), decklink_input: 8, t0_input: 6 }
    }
}

/// Router input/output numbers of 0 mean "not set".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoHubConfig {
    pub address: Option<String>,
    pub input8_output: u16,
    pub decklink_input: u16,
    #[serde(with = "millis")]
    pub settle: Duration,
    pub fallback_normal_input: u16,
}

impl Default for VideoHubConfig {
    fn default() -> Self {
        Self {
            address: Some("192.168.4.100".into()),
            input8_output: 0,
            decklink_input: 0,
            settle: Duration::from_millis(1500),
            fallback_normal_input: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebPresenterConfig {
    pub address: Option<String>,
}

impl Default for WebPresenterConfig {
    fn default() -> Self {
        Self { address: Some("192.168.4.93".into()) }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct YouTubeConfig {
    pub stream_id: Option<String>,
    /// OAuth "Desktop app" client id. Not confidential, per Google.
    #[serde(default)]
    pub client_id: String,
    /// OAuth "Desktop app" client secret. Google documents this as not confidential.
    #[serde(default)]
    pub client_secret: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timings {
    #[serde(with = "millis")]
    pub go_public_offset: Duration,
    #[serde(with = "millis")]
    pub closing_hold: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self { go_public_offset: Duration::from_secs(10), closing_hold: Duration::from_secs(10) }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayoutConfig {
    pub decklink_device: u32,
    pub video_mode: String,
    pub countdown_file: PathBuf,
}

impl Default for PlayoutConfig {
    fn default() -> Self {
        Self {
            decklink_device: 0,
            video_mode: "1080p2997".into(),
            countdown_file: PathBuf::from("countdown.mp4"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NdiConfig {
    pub source_name: String,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
}

impl Default for NdiConfig {
    fn default() -> Self {
        Self { source_name: "StreamIt Countdown".into(), fg: [255, 214, 0], bg: [0, 0, 0] }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub stream_atem: StreamAtemConfig,
    pub screens_atem: ScreensAtemConfig,
    pub videohub: VideoHubConfig,
    pub web_presenter: WebPresenterConfig,
    pub youtube: YouTubeConfig,
    pub timings: Timings,
    pub playout: PlayoutConfig,
    pub ndi: NdiConfig,
    /// The roll-in cue list, saved so it survives a restart.
    #[serde(default)]
    pub cues: Vec<Cue>,
}

impl Config {
    /// Load from `path`. A missing file yields the defaults.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("parsing config {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading config {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(path, text).with_context(|| format!("writing config {}", path.display()))
    }
}

/// Default config location: `%APPDATA%\StreamIt\config.toml`, falling back to a
/// file in the current directory when `APPDATA` is unset.
pub fn config_path() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(dir) => PathBuf::from(dir).join("StreamIt").join("config.toml"),
        None => PathBuf::from("streamit.toml"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rollin::Destination;
    use std::time::Duration;

    #[test]
    fn defaults_match_spec() {
        let c = Config::default();
        assert_eq!(c.stream_atem.address.as_deref(), Some("192.168.4.123"));
        assert_eq!(c.stream_atem.media_player, 1);
        assert_eq!(c.stream_atem.decklink_input, 8);
        assert_eq!(c.stream_atem.live_inputs, vec![1, 2, 3]);
        assert_eq!(c.stream_atem.default_live_input, 1);
        assert_eq!(c.stream_atem.mic_sources, vec![AudioSourceId { input: 1001, source: -65280 }]);
        assert_eq!(c.stream_atem.duck_db, -100.0);
        assert_eq!(c.stream_atem.fade, Duration::from_secs(1));
        assert_eq!(c.screens_atem.address.as_deref(), Some("192.168.4.98"));
        assert_eq!(c.screens_atem.decklink_input, 8);
        assert_eq!(c.screens_atem.t0_input, 6);
        assert_eq!(c.videohub.address.as_deref(), Some("192.168.4.100"));
        assert_eq!(c.videohub.settle, Duration::from_millis(1500));
        assert_eq!(c.web_presenter.address.as_deref(), Some("192.168.4.93"));
        assert_eq!(c.timings.go_public_offset, Duration::from_secs(10));
        assert_eq!(c.timings.closing_hold, Duration::from_secs(10));
        assert_eq!(c.playout.video_mode, "1080p2997");
        assert_eq!(c.ndi.source_name, "StreamIt Countdown");
        assert_eq!(c.ndi.fg, [255, 214, 0]);
        assert_eq!(c.ndi.bg, [0, 0, 0]);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = std::env::temp_dir().join("streamit-cfg-missing");
        let c = Config::load(&dir.join("nope.toml")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join("streamit-cfg-roundtrip");
        let path = dir.join("config.toml");
        let mut c = Config::default();
        c.stream_atem.address = Some("10.0.0.5".into());
        c.timings.closing_hold = Duration::from_millis(12_500);
        c.cues = vec![
            Cue {
                path: PathBuf::from(r"C:\Videos\notices.mp4"),
                name: "Notices".into(),
                destination: Destination::HallAndOnline,
                gain_db: -2.5,
                return_input: Some(2),
            },
            Cue {
                path: PathBuf::from("hymn.mp4"),
                name: "Hymn".into(),
                destination: Destination::AudioOnly,
                gain_db: 0.0,
                return_input: None,
            },
        ];
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
