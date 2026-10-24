//! With `--features gst`, the GStreamer DLLs are delay-loaded: Windows loads them on first
//! use instead of at start, so the one exe also runs on a PC without GStreamer (the
//! DeckLink and NDI counter then show as down). streamit-playout checks they load before
//! calling into them.
include!("../streamit-playout/src/dlls.rs");

fn main() {
    println!("cargo:rerun-if-changed=../streamit-playout/src/dlls.rs");
    let gst = std::env::var_os("CARGO_FEATURE_GST").is_some();
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if gst && msvc {
        for dll in GST_DLLS {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
}
