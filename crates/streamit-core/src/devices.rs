//! Device traits implemented by the real device crates and by the recording fakes.
use crate::config::AudioSourceId;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("disconnected: {0}")]
    Disconnected(String),
    #[error("refused: {0}")]
    Refused(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, DeviceError>;

#[async_trait]
pub trait Switcher: Send + Sync {
    async fn program(&self) -> Result<u16>;
    async fn preview(&self) -> Result<u16>;
    async fn set_preview(&self, input: u16) -> Result<()>;
    async fn set_program(&self, input: u16) -> Result<()>;
    async fn auto(&self) -> Result<()>;
    async fn wait_transition_done(&self, timeout: Duration) -> Result<()>;
    /// The media pool's still slots as last reported, from slot 1: each one's name, `None`
    /// when empty. Empty when the switcher has not said.
    fn stills(&self) -> Vec<Option<String>> {
        Vec::new()
    }
    /// The size a still must be: the switcher's video format. `None` until it has said.
    fn still_size(&self) -> Option<(u32, u32)> {
        None
    }
    /// Downloads the still in media pool `slot` (from 0).
    async fn download_still(&self, slot: u16) -> Result<Still> {
        let _ = slot;
        Err(DeviceError::Other("this switcher cannot send its stills".into()))
    }
    /// Puts `still` (already `still_size`) in media pool `slot` (from 0), named `name`.
    async fn upload_still(&self, slot: u16, still: Still, name: &str) -> Result<()> {
        let _ = (slot, still, name);
        Err(DeviceError::Other("this switcher cannot take stills".into()))
    }
    /// Empties media pool `slot` (from 0).
    async fn clear_still(&self, slot: u16) -> Result<()> {
        let _ = slot;
        Err(DeviceError::Other("this switcher cannot clear stills".into()))
    }
    /// What the switcher calls each of its inputs, by input number, as last reported.
    fn input_names(&self) -> std::collections::BTreeMap<u16, InputName> {
        Default::default()
    }
    /// Renames `input` on the switcher: `long` on the multiview, `short` on its buttons.
    async fn set_input_name(&self, input: u16, long: &str, short: &str) -> Result<()> {
        let _ = (input, long, short);
        Err(DeviceError::Other("this switcher cannot rename its inputs".into()))
    }
}

/// What a switcher calls one of its inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputName {
    /// Up to `INPUT_NAME_MAX` characters: shown on the multiview.
    pub long: String,
    /// Up to `SHORT_NAME_MAX` characters: shown on the switcher's buttons.
    pub short: String,
    /// Still the name the switcher came with ("Camera 3").
    pub default: bool,
}

/// The longest input name an ATEM keeps.
pub const INPUT_NAME_MAX: usize = 20;
/// The longest short name an ATEM keeps.
pub const SHORT_NAME_MAX: usize = 4;

/// A short name made from a long one: the first word's first four letters or digits,
/// upper case ("Pulpit Cam" is "PULP"); "" for a blank name.
pub fn short_name(long: &str) -> String {
    let word = long.split_whitespace().next().unwrap_or("");
    word.chars().filter(|c| c.is_ascii_alphanumeric()).take(SHORT_NAME_MAX).collect::<String>().to_ascii_uppercase()
}

/// A picture as 8-bit RGBA, row by row.
#[derive(Clone, PartialEq, Eq)]
pub struct Still {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Still {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Still({}x{})", self.width, self.height)
    }
}

#[async_trait]
pub trait StreamSwitcher: Switcher {
    async fn set_media_player_still(&self, player: u8, still: u16) -> Result<()>;
    fn media_player_input(&self, player: u8) -> u16;
    async fn audio_level(&self, src: AudioSourceId) -> Result<f32>;
    async fn set_audio_level(&self, src: AudioSourceId, db: f32) -> Result<()>;
    async fn set_audio_on(&self, src: AudioSourceId, on: bool) -> Result<()>;
    /// An audio input's mix state and fader, for the Live screen.
    async fn audio_input(&self, src: AudioSourceId) -> Result<AudioInput> {
        let _ = src;
        Err(DeviceError::Refused("this switcher cannot report its audio inputs".into()))
    }
}

/// How an audio input is mixed into the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioMix {
    On,
    Off,
    /// Audio follows video: heard only while the input is on program.
    FollowVideo,
}

/// An audio input as the switcher reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioInput {
    pub mix: AudioMix,
    pub fader_db: f32,
}

/// A VideoHub label: an input's or an output's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabelKind {
    Input,
    Output,
}

#[async_trait]
pub trait Router: Send + Sync {
    async fn route_of(&self, output: u16) -> Result<u16>;
    async fn route(&self, output: u16, input: u16) -> Result<()>;
    /// The input labels the router reports; index 0 is input 1.
    async fn input_labels(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    /// The output labels the router reports; index 0 is output 1.
    async fn output_labels(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    /// Renames input or output `n` (1-based) on the router.
    async fn set_label(&self, kind: LabelKind, n: u16, text: &str) -> Result<()> {
        let _ = (kind, n, text);
        Err(DeviceError::Refused("this router cannot rename its inputs and outputs".into()))
    }
    /// The input on every output; index 0 is output 1, 0 where not known.
    async fn routing_table(&self) -> Result<Vec<u16>> {
        Ok(Vec::new())
    }
    /// Renames many inputs or outputs at once (1-based numbers).
    async fn set_labels(&self, kind: LabelKind, labels: &[(u16, String)]) -> Result<()> {
        for (n, text) in labels {
            self.set_label(kind, *n, text).await?;
        }
        Ok(())
    }
    /// Makes many routes at once: (output, input), 1-based.
    async fn route_many(&self, routes: &[(u16, u16)]) -> Result<()> {
        for &(output, input) in routes {
            self.route(output, input).await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncoderStatus {
    Idle,
    Connecting,
    Streaming,
    Interrupted,
    Unknown(String),
}

#[async_trait]
pub trait Encoder: Send + Sync {
    async fn start(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    async fn status(&self) -> Result<EncoderStatus>;
    /// The streaming settings as name/value pairs (service, server, quality…), never the
    /// stream key.
    async fn stream_settings(&self) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }
    /// Puts streaming settings back (the stream key is never sent).
    async fn set_stream_settings(&self, settings: &[(String, String)]) -> Result<()> {
        let _ = settings;
        Err(DeviceError::Refused("this encoder cannot take streaming settings".into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastSummary {
    pub id: String,
    pub title: String,
    pub scheduled_start: Option<String>,
    pub life_cycle: String,
    pub bound_stream_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastCheck {
    pub bound_to_ours: bool,
    pub auto_start: bool,
    pub auto_stop: bool,
    pub monitor_stream: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    Bind,
    AutoStartOff,
    AutoStopOn,
    MonitorOn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Good,
    NotReceiving,
    Bad(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Testing,
    Live,
    Complete,
}

/// What YouTube says about a broadcast while it runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveDetails {
    /// People watching now, when YouTube shows it.
    pub viewers: Option<u64>,
    /// The broadcast's live chat, while it has one.
    pub chat_id: Option<String>,
}

/// Who wrote a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    Viewer,
    Moderator,
    /// The channel itself (replies sent from StreamIt come back as these).
    Owner,
}

/// One live chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: String,
    pub author: String,
    pub role: ChatRole,
    pub text: String,
    /// When it was sent, as YouTube says it (RFC 3339).
    pub published: String,
}

/// One read of a live chat: the messages since the last read, where to read from next,
/// and how long YouTube asks to wait before then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatPage {
    pub messages: Vec<ChatMessage>,
    pub next_page: Option<String>,
    pub wait: Duration,
}

#[async_trait]
pub trait Broadcasts: Send + Sync {
    async fn signed_in(&self) -> Result<bool>;
    /// The channel's upcoming and active broadcasts (R23), soonest first.
    async fn upcoming(&self) -> Result<Vec<BroadcastSummary>>;
    async fn check(&self, id: &str) -> Result<BroadcastCheck>;
    async fn fix(&self, id: &str, fix: Fix) -> Result<()>;
    async fn health(&self) -> Result<Health>;
    async fn life_cycle(&self, id: &str) -> Result<String>;
    async fn transition(&self, id: &str, to: Phase) -> Result<()>;
    /// The viewer count and the live chat of broadcast `id`.
    async fn live_details(&self, id: &str) -> Result<LiveDetails> {
        let _ = id;
        Ok(LiveDetails::default())
    }
    /// The chat messages after `page` (from the start when `None`).
    async fn chat_messages(&self, chat_id: &str, page: Option<&str>) -> Result<ChatPage> {
        let _ = (chat_id, page);
        Err(DeviceError::Refused("no live chat".into()))
    }
    /// Posts `text` to the chat as the signed-in channel.
    async fn send_chat(&self, chat_id: &str, text: &str) -> Result<()> {
        let _ = (chat_id, text);
        Err(DeviceError::Refused("no live chat".into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    Ended,
    Error(String),
}

#[async_trait]
pub trait Player: Send + Sync {
    /// Prerolls the first frame paused and returns the file's duration.
    async fn load(&self, path: &Path, gain_db: f32) -> Result<Duration>;
    async fn play(&self) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    fn position(&self) -> Option<Duration>;
    fn events(&self) -> tokio::sync::broadcast::Receiver<PlayerEvent>;
}

#[async_trait]
pub trait Counter: Send + Sync {
    async fn show(&self, remaining: Duration) -> Result<()>;
    async fn clear(&self) -> Result<()>;
}

/// What a lighting console reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LightingStatus {
    /// The show file open on the console.
    pub show: Option<String>,
    /// The active cue as the console describes it (e.g. "1/5 Sermon 3.0 100%").
    pub active_cue: Option<String>,
}

/// A lighting console (ETC Eos family over OSC).
#[async_trait]
pub trait Lighting: Send + Sync {
    /// Fires cue `cue` (e.g. "5" or "5.5") in cue list `list`.
    async fn fire_cue(&self, list: u32, cue: &str) -> Result<()>;
    /// `Ok` while the console answers; the show and active cue when it has said them.
    async fn status(&self) -> Result<LightingStatus>;
}

#[derive(Clone)]
pub struct Devices {
    pub stream: Arc<dyn StreamSwitcher>,
    pub screens: Arc<dyn Switcher>,
    pub hub: Arc<dyn Router>,
    pub encoder: Arc<dyn Encoder>,
    pub yt: Arc<dyn Broadcasts>,
    pub player: Arc<dyn Player>,
    pub counter: Arc<dyn Counter>,
    pub lights: Arc<dyn Lighting>,
}
