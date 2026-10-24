// The GStreamer DLLs StreamIt links. The app delay-loads them (streamit-app's build.rs
// includes this file), and `available` checks they load before any GStreamer call.
pub const GST_DLLS: [&str; 4] = ["glib-2.0-0.dll", "gobject-2.0-0.dll", "gstreamer-1.0-0.dll", "gstpbutils-1.0-0.dll"];
