//! Finding GStreamer's DLLs on this PC. Builds without the `gst` feature too, so the
//! search is tested everywhere; only the gst build calls it.
#![cfg_attr(not(windows), allow(dead_code))]
use crate::dlls::GST_DLLS;

/// Set by the GStreamer installer; its `bin` need not be on PATH.
#[cfg(windows)]
const ROOT_VAR: &str = "GSTREAMER_1_0_ROOT_MSVC_X86_64";
/// Where the MSVC 64-bit installer puts GStreamer by default.
#[cfg(windows)]
const DEFAULT_ROOT: &str = r"C:\Program Files\gstreamer\1.0\msvc_x86_64";

/// What the DeckLink and NDI counter show on a PC without GStreamer.
pub const NOT_INSTALLED: &str = "GStreamer not installed (needed to play videos)";

/// The `bin` folders to try, first found first: the variable as StreamIt was started
/// with it, then as the registry has it now (the installer sets it, but a StreamIt
/// started before the install, or restarted by an update, has the old environment),
/// then the default install folder. Duplicates are dropped.
fn bin_candidates(roots: impl IntoIterator<Item = Option<std::path::PathBuf>>) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    for root in roots.into_iter().flatten() {
        let bin = root.join("bin");
        if !out.iter().any(|b| b.as_os_str().eq_ignore_ascii_case(bin.as_os_str())) {
            out.push(bin);
        }
    }
    out
}

/// Why the 64-bit MSVC GStreamer is not in `root`'s `bin`, when another build is.
fn other_build(bin: &std::path::Path) -> Option<&'static str> {
    if bin.join("libgstreamer-1.0-0.dll").is_file() {
        Some("the MinGW build of GStreamer is installed; StreamIt needs the MSVC 64-bit build")
    } else {
        None
    }
}

/// An environment variable as the registry holds it now (machine first, then user).
#[cfg(windows)]
fn registry_env(name: &str) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
    };
    let wide = |s: &str| std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let keys: [(HKEY, &str); 2] = [
        (HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
        (HKEY_CURRENT_USER, "Environment"),
    ];
    for (hive, key) in keys {
        let (key, value) = (wide(key), wide(name));
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: NUL-terminated strings and a buffer of `len` bytes; RRF_RT_REG_SZ also
        // expands a REG_EXPAND_SZ value.
        let rc = unsafe {
            RegGetValueW(
                hive,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if rc == 0 {
            let chars = (len as usize / 2).saturating_sub(1);
            return Some(std::ffi::OsString::from_wide(&buf[..chars]).into());
        }
    }
    None
}

/// Whether GStreamer's DLLs can be loaded on this PC. Loads them if so.
#[cfg(windows)]
pub fn available() -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{LoadLibraryW, SetDllDirectoryW};
    let wide = |s: &OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let candidates =
        bin_candidates([std::env::var_os(ROOT_VAR).map(Into::into), registry_env(ROOT_VAR), Some(DEFAULT_ROOT.into())]);
    let found = candidates.iter().find(|b| b.join(GST_DLLS[2]).is_file());
    if let Some(bin) = found {
        tracing::info!(bin = %bin.display(), "GStreamer found");
        // SAFETY: a NUL-terminated wide string that outlives the call.
        unsafe { SetDllDirectoryW(wide(bin.as_os_str()).as_ptr()) };
    }
    for dll in GST_DLLS {
        // SAFETY: as above. The module stays loaded for the life of the process.
        let module = unsafe { LoadLibraryW(wide(OsStr::new(dll)).as_ptr()) };
        if module.is_null() {
            let code = std::io::Error::last_os_error();
            if let Some(why) = candidates.iter().find_map(|b| other_build(b)) {
                return Err(format!("{NOT_INSTALLED}: {why}"));
            }
            let looked: Vec<String> = candidates.iter().map(|b| b.display().to_string()).collect();
            return Err(match found {
                Some(bin) => format!("{NOT_INSTALLED}: {dll} in {} could not be loaded ({code})", bin.display()),
                None => format!("{NOT_INSTALLED}: {dll} not found (looked in {} and on PATH)", looked.join(", ")),
            });
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn available() -> Result<(), String> {
    let _ = GST_DLLS;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn bin_folders_are_tried_in_order_without_repeats() {
        let got = bin_candidates([
            None,
            Some(PathBuf::from(r"D:\gst\1.0\msvc_x86_64")),
            Some(PathBuf::from(r"C:\Program Files\gstreamer\1.0\msvc_x86_64")),
            Some(PathBuf::from(r"c:\program files\gstreamer\1.0\MSVC_X86_64")),
        ]);
        assert_eq!(
            got,
            vec![
                PathBuf::from(r"D:\gst\1.0\msvc_x86_64").join("bin"),
                PathBuf::from(r"C:\Program Files\gstreamer\1.0\msvc_x86_64").join("bin"),
            ]
        );
    }

    #[test]
    fn a_mingw_install_is_named() {
        let dir = std::env::temp_dir().join(format!("streamit-find-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(other_build(&dir), None);
        std::fs::write(dir.join("libgstreamer-1.0-0.dll"), b"").unwrap();
        assert!(other_build(&dir).unwrap().contains("MinGW"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn the_registry_environment_is_read() {
        // Windows sets OS=Windows_NT in the machine environment.
        assert_eq!(registry_env("OS"), Some(PathBuf::from("Windows_NT")));
        assert_eq!(registry_env("STREAMIT_NO_SUCH_VARIABLE"), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_missing_gstreamer_says_where_it_looked() {
        if let Err(e) = available() {
            assert!(e.starts_with(NOT_INSTALLED), "{e}");
            assert!(e.contains("looked in") || e.contains("MinGW") || e.contains("could not be loaded"), "{e}");
            println!("{e}");
        }
    }
}
