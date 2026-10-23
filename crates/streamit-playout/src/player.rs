//! `GstPlayer`: plays a file out of the DeckLink (or a fakesink in tests).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_pbutils as pbutils;
use streamit_core::config::PlayoutConfig;
use streamit_core::devices::{DeviceError, Player, PlayerEvent, Result};
use tokio::sync::broadcast;

use crate::pipeline::{
    AudioSource, SinkKind, VideoMode, black_pipeline, file_uri, player_pipeline_full,
};

/// How long the Discoverer may take to probe a file.
const DISCOVER_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(10);
/// How long preroll (the first frame, paused) may take.
const PREROLL_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(15);
/// How often the bus thread checks whether its clip was replaced.
const BUS_POLL: gst::ClockTime = gst::ClockTime::from_mseconds(100);

fn other(e: impl std::fmt::Display) -> DeviceError {
    DeviceError::Other(e.to_string())
}

fn to_duration(t: gst::ClockTime) -> Duration {
    Duration::from_nanos(t.nseconds())
}

/// A loaded clip and the flag that retires its bus thread.
struct Clip {
    pipeline: gst::Pipeline,
    retired: Arc<AtomicBool>,
}

impl Clip {
    fn shut_down(self) {
        self.retired.store(true, Ordering::SeqCst);
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[derive(Default)]
struct Outputs {
    clip: Option<Clip>,
    /// Black and silence on the DeckLink after `stop`.
    black: Option<gst::Pipeline>,
}

impl Outputs {
    /// Releases the DeckLink: only one pipeline may hold the device at a time.
    fn release(&mut self) {
        if let Some(clip) = self.clip.take() {
            clip.shut_down();
        }
        if let Some(black) = self.black.take() {
            let _ = black.set_state(gst::State::Null);
        }
    }
}

pub struct GstPlayer {
    sink: SinkKind,
    device: u32,
    mode: VideoMode,
    outputs: Mutex<Outputs>,
    events: broadcast::Sender<PlayerEvent>,
}

impl GstPlayer {
    /// Initialises GStreamer. Nothing is sent to the DeckLink until the first `load`.
    pub fn new(cfg: &PlayoutConfig, sink: SinkKind) -> Result<Arc<GstPlayer>> {
        gst::init().map_err(|e| other(format!("GStreamer init failed: {e}")))?;
        let mode = VideoMode::from_nick(&cfg.video_mode).ok_or_else(|| {
            other(format!(
                "unsupported playout.video_mode {:?} (expected a 1080p DeckLink mode such as 1080p2997)",
                cfg.video_mode
            ))
        })?;
        let (events, _) = broadcast::channel(16);
        Ok(Arc::new(GstPlayer {
            sink,
            device: cfg.decklink_device,
            mode,
            outputs: Mutex::new(Outputs::default()),
            events,
        }))
    }

    fn outputs(&self) -> std::sync::MutexGuard<'_, Outputs> {
        self.outputs.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Drop for GstPlayer {
    fn drop(&mut self) {
        self.outputs().release();
    }
}

/// Probes the file, builds its pipeline and prerolls it paused. Blocking.
fn prepare(
    path: &Path,
    gain_db: f32,
    sink: SinkKind,
    device: u32,
    mode: VideoMode,
) -> Result<(gst::Pipeline, Duration)> {
    let uri = file_uri(path);
    let discoverer = pbutils::Discoverer::new(DISCOVER_TIMEOUT).map_err(other)?;
    let info = discoverer
        .discover_uri(&uri)
        .map_err(|e| other(format!("cannot read {}: {e}", path.display())))?;
    if info.video_streams().is_empty() {
        return Err(other(format!("{} has no video", path.display())));
    }
    let probed = info.duration().map(to_duration);
    let audio = if info.audio_streams().is_empty() {
        let duration = probed
            .ok_or_else(|| other(format!("{} has no audio and no duration", path.display())))?;
        AudioSource::Silence { duration }
    } else {
        AudioSource::File
    };

    let desc = player_pipeline_full(&uri, gain_db, sink, device, mode, audio);
    tracing::debug!(%desc, "playout pipeline");
    let pipeline = gst::parse::launch(&desc)
        .map_err(other)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| other("pipeline description did not yield a pipeline"))?;

    if let Err(e) = preroll(&pipeline) {
        let _ = pipeline.set_state(gst::State::Null);
        return Err(e);
    }
    let duration = pipeline
        .query_duration::<gst::ClockTime>()
        .map(to_duration)
        .or(probed);
    match duration {
        Some(d) => Ok((pipeline, d)),
        None => {
            let _ = pipeline.set_state(gst::State::Null);
            Err(other(format!("{} has no duration", path.display())))
        }
    }
}

/// Sets PAUSED and waits for AsyncDone (the first frame is up) or an error.
fn preroll(pipeline: &gst::Pipeline) -> Result<()> {
    let bus = pipeline.bus().ok_or_else(|| other("pipeline has no bus"))?;
    let change = pipeline.set_state(gst::State::Paused);
    if matches!(
        change,
        Ok(gst::StateChangeSuccess::Success | gst::StateChangeSuccess::NoPreroll)
    ) {
        return Ok(());
    }
    // Async, or a failure whose reason is on the bus.
    let timeout = if change.is_ok() {
        PREROLL_TIMEOUT
    } else {
        gst::ClockTime::ZERO
    };
    match bus.timed_pop_filtered(
        timeout,
        &[gst::MessageType::AsyncDone, gst::MessageType::Error],
    ) {
        Some(msg) => match msg.view() {
            gst::MessageView::Error(e) => Err(other(describe(e))),
            _ => Ok(()),
        },
        None if change.is_err() => Err(other("pipeline refused to pause")),
        None => Err(DeviceError::Timeout("first frame did not preroll".into())),
    }
}

fn describe(e: &gst::message::Error) -> String {
    let src = e.src().map(|s| s.name().to_string()).unwrap_or_default();
    match e.debug() {
        Some(debug) => format!("{src}: {} ({debug})", e.error()),
        None => format!("{src}: {}", e.error()),
    }
}

/// Forwards the clip's end or error as a `PlayerEvent`, once, until the clip is retired.
fn watch(
    bus: gst::Bus,
    retired: Arc<AtomicBool>,
    tx: broadcast::Sender<PlayerEvent>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("playout-bus".into())
        .spawn(move || {
            while !retired.load(Ordering::SeqCst) {
                let Some(msg) = bus.timed_pop_filtered(
                    BUS_POLL,
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                ) else {
                    continue;
                };
                if retired.load(Ordering::SeqCst) {
                    break;
                }
                let event = match msg.view() {
                    gst::MessageView::Eos(_) => PlayerEvent::Ended,
                    gst::MessageView::Error(e) => PlayerEvent::Error(describe(e)),
                    _ => continue,
                };
                let _ = tx.send(event);
                break;
            }
        })?;
    Ok(())
}

#[async_trait]
impl Player for GstPlayer {
    async fn load(&self, path: &Path, gain_db: f32) -> Result<Duration> {
        let path: PathBuf = std::path::absolute(path).map_err(other)?;
        if !path.is_file() {
            return Err(other(format!("no such file: {}", path.display())));
        }
        // Free the DeckLink before the new pipeline opens it.
        self.outputs().release();

        let (sink, device, mode) = (self.sink, self.device, self.mode);
        let (pipeline, duration) =
            tokio::task::spawn_blocking(move || prepare(&path, gain_db, sink, device, mode))
                .await
                .map_err(other)??;

        let retired = Arc::new(AtomicBool::new(false));
        let bus = pipeline.bus().ok_or_else(|| other("pipeline has no bus"))?;
        let clip = Clip {
            pipeline,
            retired: retired.clone(),
        };
        if let Err(e) = watch(bus, retired, self.events.clone()) {
            clip.shut_down();
            return Err(other(e));
        }
        let mut outputs = self.outputs();
        outputs.release();
        outputs.clip = Some(clip);
        Ok(duration)
    }

    async fn play(&self) -> Result<()> {
        let outputs = self.outputs();
        let clip = outputs
            .clip
            .as_ref()
            .ok_or_else(|| DeviceError::Refused("nothing loaded".into()))?;
        clip.pipeline
            .set_state(gst::State::Playing)
            .map_err(other)?;
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let mut outputs = self.outputs();
        outputs.release();
        if self.sink == SinkKind::Fake {
            return Ok(());
        }
        let black = gst::parse::launch(&black_pipeline(self.device, self.mode))
            .map_err(other)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| other("black description did not yield a pipeline"))?;
        if let Err(e) = black.set_state(gst::State::Playing) {
            let _ = black.set_state(gst::State::Null);
            return Err(other(format!("cannot output black: {e}")));
        }
        outputs.black = Some(black);
        Ok(())
    }

    fn position(&self) -> Option<Duration> {
        let outputs = self.outputs();
        let clip = outputs.clip.as_ref()?;
        clip.pipeline
            .query_position::<gst::ClockTime>()
            .map(to_duration)
    }

    fn events(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }
}
