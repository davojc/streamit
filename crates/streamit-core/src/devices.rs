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
}

#[async_trait]
pub trait StreamSwitcher: Switcher {
    async fn set_media_player_still(&self, player: u8, still: u16) -> Result<()>;
    fn media_player_input(&self, player: u8) -> u16;
    async fn audio_level(&self, src: AudioSourceId) -> Result<f32>;
    async fn set_audio_level(&self, src: AudioSourceId, db: f32) -> Result<()>;
    async fn set_audio_on(&self, src: AudioSourceId, on: bool) -> Result<()>;
}

#[async_trait]
pub trait Router: Send + Sync {
    async fn route_of(&self, output: u16) -> Result<u16>;
    async fn route(&self, output: u16, input: u16) -> Result<()>;
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

#[derive(Clone)]
pub struct Devices {
    pub stream: Arc<dyn StreamSwitcher>,
    pub screens: Arc<dyn Switcher>,
    pub hub: Arc<dyn Router>,
    pub encoder: Arc<dyn Encoder>,
    pub yt: Arc<dyn Broadcasts>,
    pub player: Arc<dyn Player>,
    pub counter: Arc<dyn Counter>,
}
