//! Starting GStreamer. In the Windows app its DLLs are delay-loaded (streamit-app's
//! build.rs): nothing of GStreamer loads until first use, so the same exe runs on a PC
//! without it. Calling into a delay-loaded DLL that is missing crashes the process, so
//! every GStreamer use starts with `init`, which checks the DLLs load first.
use crate::dlls::GST_DLLS;
use gstreamer as gst;

/// Set by the GStreamer installer; its `bin` need not be on PATH.
#[cfg(windows)]
const ROOT_VAR: &str = "GSTREAMER_1_0_ROOT_MSVC_X86_64";

/// What the DeckLink and NDI counter show on a PC without GStreamer.
pub const NOT_INSTALLED: &str = "GStreamer not installed (needed to play videos)";

/// Whether GStreamer's DLLs can be loaded on this PC. Loads them if so.
#[cfg(windows)]
pub fn available() -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{LoadLibraryW, SetDllDirectoryW};
    let wide = |s: &OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    if let Some(root) = std::env::var_os(ROOT_VAR) {
        let bin = std::path::Path::new(&root).join("bin");
        if bin.is_dir() {
            // SAFETY: a NUL-terminated wide string that outlives the call.
            unsafe { SetDllDirectoryW(wide(bin.as_os_str()).as_ptr()) };
        }
    }
    for dll in GST_DLLS {
        // SAFETY: as above. The module stays loaded for the life of the process.
        let module = unsafe { LoadLibraryW(wide(OsStr::new(dll)).as_ptr()) };
        if module.is_null() {
            return Err(format!("{NOT_INSTALLED}: {dll} not found"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn available() -> Result<(), String> {
    let _ = GST_DLLS;
    Ok(())
}

/// Checks the DLLs, then initialises GStreamer.
pub fn init() -> Result<(), String> {
    available()?;
    gst::init().map_err(|e| format!("GStreamer init failed: {e}"))
}
