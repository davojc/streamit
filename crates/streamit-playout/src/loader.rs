//! Starting GStreamer. In the Windows app its DLLs are delay-loaded (streamit-app's
//! build.rs): nothing of GStreamer loads until first use, so the same exe runs on a PC
//! without it. Calling into a delay-loaded DLL that is missing crashes the process, so
//! every GStreamer use starts with `init`, which checks the DLLs load first.
use crate::find::available;
use gstreamer as gst;

/// Checks the DLLs, then initialises GStreamer.
pub fn init() -> Result<(), String> {
    available()?;
    gst::init().map_err(|e| format!("GStreamer init failed: {e}"))
}
