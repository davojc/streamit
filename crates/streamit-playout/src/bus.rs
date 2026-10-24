//! Bus draining for the long-running pipelines (the NDI counter and the DeckLink black).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gstreamer as gst;

/// How often bus threads wake to check whether they should exit.
pub(crate) const BUS_POLL: gst::ClockTime = gst::ClockTime::from_mseconds(100);

/// `element: message (debug)` for an error message.
pub(crate) fn describe(e: &gst::message::Error) -> String {
    use gstreamer::prelude::*;
    let src = e.src().map(|s| s.name().to_string()).unwrap_or_default();
    match e.debug() {
        Some(debug) => format!("{src}: {} ({debug})", e.error()),
        None => format!("{src}: {}", e.error()),
    }
}

/// Drains a pipeline's bus on a thread so messages never pile up, logs errors and records
/// that one happened. The thread exits when this is dropped.
pub(crate) struct BusWatch {
    failed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl BusWatch {
    pub(crate) fn spawn(bus: gst::Bus, what: &'static str) -> std::io::Result<BusWatch> {
        let failed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (f, s) = (failed.clone(), stop.clone());
        std::thread::Builder::new().name(format!("{what}-bus")).spawn(move || {
            while !s.load(Ordering::SeqCst) {
                let Some(msg) = bus.timed_pop(BUS_POLL) else {
                    continue;
                };
                if let gst::MessageView::Error(e) = msg.view() {
                    tracing::warn!(pipeline = what, error = %describe(e), "pipeline error");
                    f.store(true, Ordering::SeqCst);
                }
            }
        })?;
        Ok(BusWatch { failed, stop })
    }

    /// True once the pipeline has posted an error.
    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }
}

impl Drop for BusWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}
