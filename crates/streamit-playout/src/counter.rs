//! `NdiCounter`: the yellow-on-black "time remaining" counter, sent over NDI.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gstreamer as gst;
use gstreamer::prelude::*;
use streamit_core::config::NdiConfig;
use streamit_core::devices::{Counter, DeviceError, Result};

use crate::bus::{BusWatch, describe};
use crate::pipeline::{counter_pipeline, counter_text};

/// How long the NDI pipeline may take to start.
const START_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(5);

fn other(e: impl std::fmt::Display) -> DeviceError {
    DeviceError::Other(e.to_string())
}

/// A pipeline that runs for the life of the counter; only the overlay's text changes.
pub struct NdiCounter {
    pipeline: gst::Pipeline,
    overlay: gst::Element,
    watch: BusWatch,
}

impl NdiCounter {
    /// Starts the NDI source (`ndi.source_name`) showing an empty frame. Blocks for up to
    /// 5 s while the pipeline starts.
    pub fn new(cfg: &NdiConfig) -> Result<Arc<NdiCounter>> {
        crate::loader::init().map_err(other)?;
        let pipeline = gst::parse::launch(&counter_pipeline(&cfg.source_name, cfg.fg, cfg.bg))
            .map_err(other)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| other("counter description did not yield a pipeline"))?;
        let overlay = pipeline.by_name("counter").ok_or_else(|| other("counter overlay missing"))?;
        let bus = pipeline.bus().ok_or_else(|| other("counter pipeline has no bus"))?;

        let changed = pipeline.set_state(gst::State::Playing).is_ok();
        // A timeout returns Ok(Async) with the pipeline not yet PLAYING: that is a failure.
        let (result, current, _pending) = pipeline.state(START_TIMEOUT);
        if !(changed && result.is_ok() && current == gst::State::Playing) {
            let reason = bus
                .timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Error])
                .and_then(|msg| match msg.view() {
                    gst::MessageView::Error(e) => Some(describe(e)),
                    _ => None,
                })
                .unwrap_or_else(|| format!("did not reach PLAYING (state {current:?})"));
            let _ = pipeline.set_state(gst::State::Null);
            return Err(other(format!("NDI counter: {reason}")));
        }
        let watch = match BusWatch::spawn(bus, "ndi-counter") {
            Ok(watch) => watch,
            Err(e) => {
                let _ = pipeline.set_state(gst::State::Null);
                return Err(other(e));
            }
        };
        Ok(Arc::new(NdiCounter { pipeline, overlay, watch }))
    }

    fn set_text(&self, text: &str) -> Result<()> {
        if self.watch.failed() {
            return Err(DeviceError::Disconnected("NDI counter pipeline failed".into()));
        }
        if self.pipeline.current_state() != gst::State::Playing {
            return Err(DeviceError::Disconnected("NDI counter pipeline stopped".into()));
        }
        self.overlay.set_property("text", text);
        Ok(())
    }
}

impl Drop for NdiCounter {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[async_trait]
impl Counter for NdiCounter {
    async fn show(&self, remaining: Duration) -> Result<()> {
        self.set_text(&counter_text(remaining))
    }

    async fn clear(&self) -> Result<()> {
        self.set_text("")
    }
}
