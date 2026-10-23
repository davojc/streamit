//! Playout: the roll-in and countdown videos out of the DeckLink (via GStreamer), and the
//! NDI "time remaining" counter for ProPresenter.
//!
//! The GStreamer code sits behind the cargo feature `gst` (off by default). Without it only
//! the pure pieces build: the pipeline descriptions, gain conversion and the counter text.

pub mod pipeline;

#[cfg(feature = "gst")]
mod counter;
#[cfg(feature = "gst")]
mod player;

pub use pipeline::{SinkKind, counter_text, gain_db_to_linear};

#[cfg(feature = "gst")]
pub use counter::NdiCounter;
#[cfg(feature = "gst")]
pub use player::GstPlayer;
