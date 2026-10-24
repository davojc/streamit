//! `GstPlayer`: plays a file out of the DeckLink (or a fakesink in tests).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_pbutils as pbutils;
use streamit_core::config::PlayoutConfig;
use streamit_core::devices::{DeviceError, Player, PlayerEvent, Result};
use tokio::sync::broadcast;

use crate::bus::{BUS_POLL, BusWatch, describe};
use crate::pipeline::{AudioSource, SinkKind, VideoMode, black_pipeline, file_uri, player_pipeline_full};

/// How long the Discoverer may take to probe a file.
const DISCOVER_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(10);
/// How long preroll (the first frame, paused) may take.
const PREROLL_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(15);

fn other(e: impl std::fmt::Display) -> DeviceError {
    DeviceError::Other(e.to_string())
}

fn to_duration(t: gst::ClockTime) -> Duration {
    Duration::from_nanos(t.nseconds())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs blocking GStreamer work (state changes, preroll) off the async worker threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(other)?
}

/// Black and silence on the DeckLink after `stop`, with its bus drained and logged.
struct Black {
    pipeline: gst::Pipeline,
    _watch: BusWatch,
}

struct Outputs {
    clip: Option<gst::Pipeline>,
    black: Option<Black>,
    /// The generation of the loaded clip. Retiring a clip bumps it under this lock, and a
    /// clip's bus thread only sends its event while holding the lock with its generation
    /// still current, so a retired clip can never report `Ended` or `Error`.
    gate: Arc<Mutex<u64>>,
}

impl Outputs {
    /// Releases the DeckLink: only one pipeline may hold the device at a time. Blocking.
    fn release(&mut self) {
        if let Some(clip) = self.clip.take() {
            *lock(&self.gate) += 1;
            let _ = clip.set_state(gst::State::Null);
        }
        if let Some(black) = self.black.take() {
            let _ = black.pipeline.set_state(gst::State::Null);
        }
    }
}

pub struct GstPlayer {
    sink: SinkKind,
    device: u32,
    mode: VideoMode,
    outputs: Arc<Mutex<Outputs>>,
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
        let outputs = Outputs { clip: None, black: None, gate: Arc::new(Mutex::new(0)) };
        Ok(Arc::new(GstPlayer {
            sink,
            device: cfg.decklink_device,
            mode,
            outputs: Arc::new(Mutex::new(outputs)),
            events,
        }))
    }
}

impl Drop for GstPlayer {
    fn drop(&mut self) {
        lock(&self.outputs).release();
    }
}

/// Probes the file, builds its pipeline and prerolls it paused. Blocking.
fn prepare(
    path: &Path,
    gain_db: f32,
    sink: SinkKind,
    device: u32,
    mode: VideoMode,
) -> Result<(gst::Pipeline, gst::Bus, Duration)> {
    let uri = file_uri(path);
    let discoverer = pbutils::Discoverer::new(DISCOVER_TIMEOUT).map_err(other)?;
    let info = discoverer.discover_uri(&uri).map_err(|e| other(format!("cannot read {}: {e}", path.display())))?;
    if info.video_streams().is_empty() {
        return Err(other(format!("{} has no video", path.display())));
    }
    let probed = info.duration().map(to_duration);
    let audio = if info.audio_streams().is_empty() {
        let duration = probed.ok_or_else(|| other(format!("{} has no audio and no duration", path.display())))?;
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
    let Some(bus) = pipeline.bus() else {
        let _ = pipeline.set_state(gst::State::Null);
        return Err(other("pipeline has no bus"));
    };

    if let Err(e) = preroll(&pipeline, &bus) {
        let _ = pipeline.set_state(gst::State::Null);
        return Err(e);
    }
    let duration = pipeline.query_duration::<gst::ClockTime>().map(to_duration).or(probed);
    match duration {
        Some(d) => Ok((pipeline, bus, d)),
        None => {
            let _ = pipeline.set_state(gst::State::Null);
            Err(other(format!("{} has no duration", path.display())))
        }
    }
}

/// Sets PAUSED and waits for AsyncDone (the first frame is up) or an error.
fn preroll(pipeline: &gst::Pipeline, bus: &gst::Bus) -> Result<()> {
    let change = pipeline.set_state(gst::State::Paused);
    if matches!(change, Ok(gst::StateChangeSuccess::Success | gst::StateChangeSuccess::NoPreroll)) {
        return Ok(());
    }
    // Async, or a failure whose reason is on the bus.
    let timeout = if change.is_ok() { PREROLL_TIMEOUT } else { gst::ClockTime::ZERO };
    match bus.timed_pop_filtered(timeout, &[gst::MessageType::AsyncDone, gst::MessageType::Error]) {
        Some(msg) => match msg.view() {
            gst::MessageView::Error(e) => Err(other(describe(e))),
            _ => Ok(()),
        },
        None if change.is_err() => Err(other("pipeline refused to pause")),
        None => Err(DeviceError::Timeout("first frame did not preroll".into())),
    }
}

/// Forwards clip `generation`'s end or error as a `PlayerEvent`, once, unless the clip has
/// been retired. Other messages are drained (dropped) by the filtered pop.
fn watch(
    bus: gst::Bus,
    gate: Arc<Mutex<u64>>,
    generation: u64,
    tx: broadcast::Sender<PlayerEvent>,
) -> std::io::Result<()> {
    std::thread::Builder::new().name("playout-bus".into()).spawn(move || {
        while *lock(&gate) == generation {
            let Some(msg) = bus.timed_pop_filtered(BUS_POLL, &[gst::MessageType::Eos, gst::MessageType::Error]) else {
                continue;
            };
            let event = match msg.view() {
                gst::MessageView::Eos(_) => PlayerEvent::Ended,
                gst::MessageView::Error(e) => PlayerEvent::Error(describe(e)),
                _ => continue,
            };
            let live = lock(&gate);
            if *live == generation {
                let _ = tx.send(event);
            }
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
        let (sink, device, mode) = (self.sink, self.device, self.mode);
        let (outputs, events) = (self.outputs.clone(), self.events.clone());
        blocking(move || {
            // Free the DeckLink before the new pipeline opens it.
            lock(&outputs).release();
            let (pipeline, bus, duration) = prepare(&path, gain_db, sink, device, mode)?;
            let mut outputs = lock(&outputs);
            outputs.release();
            let generation = *lock(&outputs.gate);
            if let Err(e) = watch(bus, outputs.gate.clone(), generation, events) {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(other(e));
            }
            outputs.clip = Some(pipeline);
            Ok(duration)
        })
        .await
    }

    async fn play(&self) -> Result<()> {
        let outputs = self.outputs.clone();
        blocking(move || {
            let outputs = lock(&outputs);
            let clip = outputs.clip.as_ref().ok_or_else(|| DeviceError::Refused("nothing loaded".into()))?;
            clip.set_state(gst::State::Playing).map_err(other)?;
            Ok(())
        })
        .await
    }

    async fn stop(&self) -> Result<()> {
        let (outputs, sink, device, mode) = (self.outputs.clone(), self.sink, self.device, self.mode);
        blocking(move || {
            let mut outputs = lock(&outputs);
            outputs.release();
            if sink == SinkKind::Fake {
                return Ok(());
            }
            let pipeline = gst::parse::launch(&black_pipeline(device, mode))
                .map_err(other)?
                .downcast::<gst::Pipeline>()
                .map_err(|_| other("black description did not yield a pipeline"))?;
            let watch = match pipeline.bus().map(|bus| BusWatch::spawn(bus, "decklink-black")) {
                Some(Ok(watch)) => watch,
                Some(Err(e)) => return Err(other(e)),
                None => return Err(other("black pipeline has no bus")),
            };
            if let Err(e) = pipeline.set_state(gst::State::Playing) {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(other(format!("cannot output black: {e}")));
            }
            outputs.black = Some(Black { pipeline, _watch: watch });
            Ok(())
        })
        .await
    }

    fn position(&self) -> Option<Duration> {
        let outputs = lock(&self.outputs);
        let clip = outputs.clip.as_ref()?;
        clip.query_position::<gst::ClockTime>().map(to_duration)
    }

    fn events(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }
}
