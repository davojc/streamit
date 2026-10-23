//! `NdiCounter`: the yellow-on-black "time remaining" counter, sent over NDI.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gstreamer as gst;
use gstreamer::prelude::*;
use streamit_core::config::NdiConfig;
use streamit_core::devices::{Counter, DeviceError, Result};

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
}

impl NdiCounter {
    /// Starts the NDI source (`ndi.source_name`) showing an empty frame.
    pub fn new(cfg: &NdiConfig) -> Result<Arc<NdiCounter>> {
        gst::init().map_err(|e| other(format!("GStreamer init failed: {e}")))?;
        let pipeline = gst::parse::launch(&counter_pipeline(&cfg.source_name, cfg.fg, cfg.bg))
            .map_err(other)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| other("counter description did not yield a pipeline"))?;
        let overlay = pipeline
            .by_name("counter")
            .ok_or_else(|| other("counter overlay missing"))?;
        let started = pipeline.set_state(gst::State::Playing).is_ok()
            && pipeline.state(START_TIMEOUT).0.is_ok();
        if !started {
            let reason = pipeline
                .bus()
                .and_then(|bus| {
                    bus.timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Error])
                })
                .and_then(|msg| match msg.view() {
                    gst::MessageView::Error(e) => Some(e.error().to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| "did not start".into());
            let _ = pipeline.set_state(gst::State::Null);
            return Err(other(format!("NDI counter: {reason}")));
        }
        Ok(Arc::new(NdiCounter { pipeline, overlay }))
    }

    fn set_text(&self, text: &str) -> Result<()> {
        if self.pipeline.current_state() != gst::State::Playing {
            return Err(DeviceError::Disconnected(
                "NDI counter pipeline stopped".into(),
            ));
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
