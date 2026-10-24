//! Playout: the roll-in and countdown videos out of the DeckLink (via GStreamer), and the
//! NDI "time remaining" counter for ProPresenter.
//!
//! The GStreamer code sits behind the cargo feature `gst` (off by default). Without it only
//! the pure pieces build: the pipeline descriptions, gain conversion and the counter text.

mod dlls;
mod find;
pub mod pipeline;

#[cfg(feature = "gst")]
mod bus;
#[cfg(feature = "gst")]
mod counter;
#[cfg(feature = "gst")]
mod loader;
#[cfg(feature = "gst")]
mod player;
#[cfg(feature = "gst")]
pub mod probe;

pub use pipeline::{SinkKind, counter_text, gain_db_to_linear};

#[cfg(feature = "gst")]
pub use counter::NdiCounter;
pub use dlls::GST_DLLS;
pub use find::{NOT_INSTALLED, available};
#[cfg(feature = "gst")]
pub use loader::init;
#[cfg(feature = "gst")]
pub use player::GstPlayer;
