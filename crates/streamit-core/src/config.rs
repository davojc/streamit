//! StreamIt configuration: device addresses, input numbers and timings.

use crate::routing::{RoutingConfig, Target};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::rollin::Cue;
use crate::scenes::Scene;

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

/// An audio input shown on the Service screens, with the name to show it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownAudio {
    pub name: String,
    pub source: AudioSourceId,
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
    /// The audio inputs the Service screens show, in order.
    pub shown_audio: Vec<ShownAudio>,
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
            shown_audio: Vec::new(),
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
    /// The hall input at T-0 when no Go Live scene is set (from before scenes).
    pub t0_input: u16,
    /// The scene the hall goes to at Go Live (its screens input and lighting cue).
    pub t0_scene: Option<u32>,
    /// Media player and stills marked on the Screens ATEM's media pool. Not used yet.
    pub media_player: u8,
    pub opening_still: Option<u16>,
    pub closing_still: Option<u16>,
}

impl Default for ScreensAtemConfig {
    fn default() -> Self {
        Self {
            address: Some("192.168.4.98".into()),
            decklink_input: 8,
            t0_input: 6,
            t0_scene: None,
            media_player: 1,
            opening_still: None,
            closing_still: None,
        }
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
    /// An unused (black) input. A VideoHub output always shows some input, so
    /// disconnecting an output routes this one to it.
    pub blank_input: u16,
}

impl Default for VideoHubConfig {
    fn default() -> Self {
        Self {
            address: Some("192.168.4.100".into()),
            input8_output: 0,
            decklink_input: 0,
            settle: Duration::from_millis(1500),
            fallback_normal_input: 0,
            blank_input: 0,
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
    /// The countdown video also goes online: routed to Stream ATEM input 8 and put on
    /// program with its sound. Off: online sees the opening still until T-0.
    pub countdown_to_stream: bool,
}

impl Default for PlayoutConfig {
    fn default() -> Self {
        Self {
            decklink_device: 0,
            video_mode: "1080p2997".into(),
            countdown_file: PathBuf::from("countdown.mp4"),
            countdown_to_stream: false,
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

/// How OSC packets are framed on TCP: the lighting console's "TCP Format for OSC".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OscFraming {
    /// OSC 1.1 (SLIP).
    #[default]
    Slip,
    /// OSC 1.0 (packet length headers).
    Length,
}

/// The lighting console (ETC Eos family, OSC over TCP port 3032). Optional.
/// The MultiView 4 itself (Videohub Ethernet protocol, TCP port 9990). Optional: StreamIt
/// only checks that it answers. What feeds its views is the VideoHub's job (`multiview`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MultiViewDevice {
    pub address: Option<String>,
}

impl MultiViewDevice {
    /// A MultiView is set up.
    pub fn is_set(&self) -> bool {
        self.address.as_deref().is_some_and(|a| !a.trim().is_empty())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightingConfig {
    pub address: Option<String>,
    pub framing: OscFraming,
    /// Lighting control is turned off: StreamIt neither connects to the console nor sends
    /// it anything, and scenes change only the hall screens.
    pub off: bool,
}

impl LightingConfig {
    /// A console is set up and lighting control is on.
    pub fn is_set(&self) -> bool {
        !self.off && self.address.as_deref().is_some_and(|a| !a.trim().is_empty())
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
    pub lighting: LightingConfig,
    /// The MultiView 4 box, when one is given the MultiView job in Settings › Equipment.
    pub multiview_device: MultiViewDevice,
    /// Routing: what feeds the MultiView 4's views and the ATEMs' inputs, and the presets.
    pub multiview: RoutingConfig,
    pub stream_routing: RoutingConfig,
    pub screens_routing: RoutingConfig,
    /// The roll-in cue list, saved so it survives a restart.
    #[serde(default)]
    pub cues: Vec<Cue>,
    /// The scenes, in order. Saved in place, like the Routing presets.
    #[serde(default)]
    pub scenes: Vec<Scene>,
}

impl Config {
    pub fn routing(&self, t: Target) -> &RoutingConfig {
        match t {
            Target::MultiView => &self.multiview,
            Target::StreamAtem => &self.stream_routing,
            Target::ScreensAtem => &self.screens_routing,
        }
    }

    pub fn routing_mut(&mut self, t: Target) -> &mut RoutingConfig {
        match t {
            Target::MultiView => &mut self.multiview,
            Target::StreamAtem => &mut self.stream_routing,
            Target::ScreensAtem => &mut self.screens_routing,
        }
    }

    /// The Stream ATEM's Media input as a destination index (videos and the countdown
    /// come in there), while it is one of the inputs the router feeds.
    pub fn media_dest(&self) -> Option<usize> {
        let i = usize::from(self.stream_atem.decklink_input).checked_sub(1)?;
        (i < Target::StreamAtem.size()).then_some(i)
    }

    /// The VideoHub output feeding each destination of `t` (0 = not from the router).
    /// The Stream ATEM's Media input is always fed by the output StreamIt uses for videos.
    pub fn routing_outputs(&self, t: Target) -> Vec<u16> {
        let mut outputs = self.routing(t).outputs_for(t.size());
        if t == Target::StreamAtem
            && let Some(m) = self.media_dest()
        {
            outputs[m] = self.videohub.input8_output;
        }
        outputs
    }

    /// Make Stream ATEM input `input` (1-based) the Media input. Its router output becomes
    /// the videos output, and the input that was the Media input keeps the output that fed it.
    pub fn set_media_input(&mut self, input: u16) {
        let old = self.media_dest();
        let new = usize::from(input).saturating_sub(1);
        if old == Some(new) || new >= Target::StreamAtem.size() {
            return;
        }
        let r = &mut self.stream_routing;
        if r.outputs.len() < Target::StreamAtem.size() {
            r.outputs.resize(Target::StreamAtem.size(), 0);
        }
        let new_out = r.outputs[new];
        if let Some(o) = old {
            r.outputs[o] = self.videohub.input8_output;
        }
        r.outputs[new] = 0;
        self.videohub.input8_output = new_out;
        self.stream_atem.decklink_input = input;
    }

    /// The router input the Media input normally shows: the Default preset's, else the
    /// one set before Default presets (0 = not known).
    pub fn media_normal_input(&self) -> u16 {
        let from_default =
            self.media_dest().and_then(|m| self.stream_routing.normal_preset().map(|p| p.input(m))).filter(|&i| i != 0);
        from_default.unwrap_or(self.videohub.fallback_normal_input)
    }

    /// The Go Live scene, while it still exists.
    pub fn t0_scene(&self) -> Option<&Scene> {
        self.screens_atem.t0_scene.and_then(|id| crate::scenes::find(&self.scenes, id))
    }

    /// The Screens ATEM input the hall goes to at T-0: the Go Live scene's, else
    /// `t0_input`. A Go Live scene that leaves the screens alone gives `None`.
    pub fn hall_t0_input(&self) -> Option<u16> {
        match self.t0_scene() {
            Some(s) => s.screens,
            None => Some(self.screens_atem.t0_input),
        }
    }

    /// Load from `path`. A missing file yields the defaults.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                anyhow::anyhow!("cannot read settings {} ({})", path.display(), describe_toml_error(&text, &e))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading config {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let text = toml::to_string_pretty(self).context("serializing config")?;
        write_atomic(path, &text)
    }
}

/// Write `text` to `path` through a temp file next to it, then rename it over `path`, so
/// a failed or interrupted save never leaves a half-written file.
pub fn write_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = temp_path(path);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, text.as_bytes())?;
        f.sync_all()
    };
    if let Err(e) = write() {
        if tmp.is_file() {
            let _ = std::fs::remove_file(&tmp);
        }
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::Error::new(e).context(format!("replacing {}", path.display()))
    })
}

/// The temp file a save writes before renaming it over `path`: same directory.
pub fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

/// "line L, column C: <message>" for a TOML error, without the source line (which may
/// hold the client secret) and with quoted values replaced by "…".
fn describe_toml_error(text: &str, e: &toml::de::Error) -> String {
    let at = match e.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
            format!("line {line}, column {col}: ")
        }
        None => String::new(),
    };
    format!("{at}{}", redact(e.message().lines().next().unwrap_or("")))
}

/// Replaces anything between quotes or backticks with "…", and numbers with "N".
fn redact(msg: &str) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    for ch in msg.chars() {
        match quote {
            Some(q) if ch == q => {
                out.push('…');
                out.push(ch);
                quote = None;
            }
            Some(_) => {}
            None if matches!(ch, '"' | '`' | '\'') => {
                out.push(ch);
                quote = Some(ch);
            }
            None if ch.is_ascii_digit() => {
                if !out.ends_with('N') {
                    out.push('N');
                }
            }
            None => out.push(ch),
        }
    }
    if quote.is_some() {
        out.push('…');
    }
    out
}

/// Where the mics' captured live levels are kept: `levels.toml` next to the config.
pub fn levels_path(config: &Path) -> PathBuf {
    config.with_file_name("levels.toml")
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
    #[test]
    fn the_media_input_moves_and_keeps_each_input_fed() {
        let mut c = Config::default();
        c.stream_atem.decklink_input = 8;
        c.videohub.input8_output = 11;
        c.stream_routing.outputs = vec![1, 2, 3, 4, 5, 6, 7, 0];
        assert_eq!(c.media_dest(), Some(7));
        assert_eq!(c.routing_outputs(Target::StreamAtem)[7], 11);
        c.set_media_input(6);
        assert_eq!((c.stream_atem.decklink_input, c.videohub.input8_output, c.media_dest()), (6, 6, Some(5)));
        assert_eq!(c.routing_outputs(Target::StreamAtem), vec![1, 2, 3, 4, 5, 6, 7, 11]);
        c.stream_atem.decklink_input = 9;
        assert_eq!(c.media_dest(), None, "not a routed input");
    }

    #[test]
    fn the_media_input_normally_shows_the_default_presets_input() {
        use crate::routing::Preset;
        let mut c = Config::default();
        c.stream_atem.decklink_input = 8;
        c.videohub.fallback_normal_input = 13;
        assert_eq!(c.media_normal_input(), 13, "no Default preset: the old setting");
        c.stream_routing.presets =
            vec![Preset { slot: 2, name: "Service".into(), inputs: vec![1, 2, 3, 4, 5, 6, 7, 5] }];
        assert_eq!(c.media_normal_input(), 13, "slot 2 is not the default");
        c.stream_routing.default_preset = Some(2);
        assert_eq!(c.media_normal_input(), 5);
    }

    #[test]
    fn the_hall_at_t0_follows_the_go_live_scene() {
        let mut c = Config::default();
        c.screens_atem.t0_input = 6;
        assert_eq!(c.hall_t0_input(), Some(6));
        let mut s = Scene::new(&[]);
        s.screens = Some(3);
        c.screens_atem.t0_scene = Some(s.id);
        c.scenes = vec![s.clone()];
        assert_eq!(c.hall_t0_input(), Some(3));
        c.scenes[0].screens = None;
        assert_eq!(c.hall_t0_input(), None, "the scene leaves the hall alone");
        c.scenes.clear();
        assert_eq!(c.hall_t0_input(), Some(6), "a removed scene falls back");
    }

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
                scene: None,
            },
            Cue {
                path: PathBuf::from("hymn.mp4"),
                name: "Hymn".into(),
                destination: Destination::AudioOnly,
                gain_db: 0.0,
                return_input: None,
                scene: None,
            },
        ];
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_is_atomic() {
        let dir = std::env::temp_dir().join("streamit-cfg-atomic");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        let mut c = Config::default();
        c.save(&path).unwrap();
        // A successful save goes through the temp file and leaves none behind.
        let tmp = temp_path(&path);
        assert_eq!(tmp.parent(), path.parent(), "temp file must be in the same directory");
        assert!(!tmp.exists());
        // A write that fails leaves the old file whole.
        let before = std::fs::read_to_string(&path).unwrap();
        std::fs::create_dir_all(&tmp).unwrap(); // the temp file cannot be created
        c.timings.closing_hold = Duration::from_secs(99);
        assert!(c.save(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_error_hides_the_source_line() {
        let dir = std::env::temp_dir().join("streamit-cfg-bad");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[youtube]\nclient_id = \"abc\"\nclient_secret = 12345678\n").unwrap();
        let msg = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(msg.contains("line 3"), "{msg}");
        assert!(!msg.contains("12345678"), "{msg}");
        assert!(!msg.contains("client_secret ="), "{msg}");
        std::fs::write(&path, "[youtube]\nclient_secret = \"GOCSPX-secret\nx = 1\n").unwrap();
        let msg = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(!msg.contains("GOCSPX"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
