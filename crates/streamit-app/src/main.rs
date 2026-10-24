//! StreamIt: runs the church YouTube live stream from a Stream Deck XL.
//!
//!   streamit                      open the window, start the engine and the deck
//!   streamit probe <device> ...   hardware probes (see `streamit probe`), run from a terminal
//!
//! I6: a Windows GUI app, so no console window opens with it (closing a console window
//! would kill StreamIt mid-service, and a console paused by a click would block its
//! logging). The window logs to files only. Probes attach to the terminal they were
//! started from, or open a console of their own.
//!
//! Startup never changes a device: it opens the real session (which builds the devices,
//! which only connect and listen, and spawns the engine, which runs its read-only Stage 0
//! / assessment), starts the deck and opens the window. The deck and the window hold the
//! session's `Front`, so a switch to simulation and back keeps them working. The egui loop runs on the main thread; tokio runs on worker threads.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod discovery;
mod logged;
mod logging;
mod probe;
mod session;
mod theme;
mod ui;
mod update;
mod view;
mod wiring;

use anyhow::anyhow;
use session::{Paths, RealFactory, Sessions, SimFactory};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use streamit_core::config::{Config, config_path};
use streamit_core::engine::EngineHandle;
use streamit_deck::DeckSurface;
use tracing::{error, info, warn};

/// Probes and the usage text write to a terminal; the window does not.
fn terminal_mode(args: &[String]) -> bool {
    !args.is_empty()
}

/// Send this process's output to the terminal it was started from, or, with none,
/// to a new console. Only for the terminal modes: the window never has a console.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole};
    // SAFETY: plain Win32 calls with no pointers; failure just leaves output unseen.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            AllocConsole();
        }
    }
}

#[cfg(not(windows))]
fn attach_console() {}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let terminal = terminal_mode(&args);
    if terminal {
        attach_console();
    }
    let _guard = logging::init(terminal);
    if args.first().map(String::as_str) == Some("probe") {
        return probe::main(&args[1..]);
    }
    if !args.is_empty() {
        eprintln!("usage: streamit            (open the window)\n{}", probe::USAGE);
        return ExitCode::FAILURE;
    }
    match run_app() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_app() -> anyhow::Result<()> {
    info!(version = env!("CARGO_PKG_VERSION"), "StreamIt starting");
    let path = config_path();
    let Loaded { config, notice: load_error, writable } = load_config(&path);
    info!(path = %path.display(), writable, "config loaded");
    let shared = Arc::new(RwLock::new(config.clone()));

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("streamit-rt").build()?;
    // Always real mode first: the normal read-only startup.
    let (sessions, front) = rt.block_on(async {
        let (sessions, front) =
            Sessions::start(Arc::new(RealFactory), Arc::new(SimFactory), shared.clone(), Paths::beside(&path)).await;
        // The deck holds the front, so it keeps working across a mode switch.
        DeckSurface::run(EngineHandle { tx: front.tx.clone(), rx: front.rx.clone() });
        (Arc::new(sessions), front)
    });

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("StreamIt")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    let handle = rt.handle().clone();
    let window_sessions = sessions.clone();
    let result = eframe::run_native(
        "StreamIt",
        options,
        Box::new(move |cc| {
            let file = ui::ConfigFile { path, writable };
            Ok(Box::new(ui::StreamItApp::new(
                &cc.egui_ctx,
                handle,
                window_sessions,
                front,
                shared,
                file,
                config,
                load_error,
            )))
        }),
    );
    info!("window closed; StreamIt stopping");
    drop(sessions);
    rt.shutdown_timeout(Duration::from_secs(2));
    result.map_err(|e| anyhow!("the window failed: {e}"))
}

struct Loaded {
    config: Config,
    /// Shown in the window when the file could not be read.
    notice: Option<String>,
    /// False when an unreadable file could not be backed up: the window then never
    /// writes the file, so the user's settings are not lost.
    writable: bool,
}

/// Loads the settings. A file that cannot be read is first copied to `config.toml.bad`
/// (or `.bad.2`, `.bad.3`, ... if that exists), before anything can write over it; the
/// defaults are used meanwhile.
fn load_config(path: &Path) -> Loaded {
    let err = match Config::load(path) {
        Ok(config) => return Loaded { config, notice: None, writable: true },
        Err(e) => e,
    };
    // The error names only the line, column and kind, never the line's text.
    warn!("{err:#}");
    let (notice, writable) = match backup_bad(path) {
        Ok(Some(bad)) => (
            format!(
                "{err:#}. Your settings file was copied to {} and StreamIt is using the defaults \
                 until you Save.",
                bad.display()
            ),
            true,
        ),
        Ok(None) => (format!("{err:#}. StreamIt is using the defaults."), true),
        Err(e) => (
            format!(
                "{err:#}. It could not be backed up ({e}), so StreamIt will not change it: \
                 settings and cue edits are not saved this session."
            ),
            false,
        ),
    };
    warn!("{notice}");
    Loaded { config: Config::default(), notice: Some(notice), writable }
}

/// Copies `path` to the first free `<name>.bad[.N]` next to it. `None` if there is no file.
fn backup_bad(path: &Path) -> std::io::Result<Option<PathBuf>> {
    if !path.is_file() {
        return Ok(None);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bad = (1..1000)
        .map(|n| path.with_file_name(if n == 1 { format!("{name}.bad") } else { format!("{name}.bad.{n}") }))
        .find(|p| !p.exists())
        .ok_or_else(|| std::io::Error::other("too many .bad copies"))?;
    std::fs::copy(path, &bad)?;
    Ok(Some(bad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_probes_and_usage_use_the_terminal() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!terminal_mode(&args(&[])), "the window logs to files only");
        assert!(terminal_mode(&args(&["probe", "atem", "1.2.3.4", "state"])));
        assert!(terminal_mode(&args(&["--help"])), "the usage text goes to the terminal");
    }

    #[test]
    fn bad_config_is_backed_up_before_first_write() {
        let dir = std::env::temp_dir().join("streamit-app-badcfg");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original = "[youtube]\nclient_id = \"id\"\nclient_secret = super-secret-value\n";
        std::fs::write(&path, original).unwrap();

        let loaded = load_config(&path);
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.writable);
        let bad = dir.join("config.toml.bad");
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), original);
        let notice = loaded.notice.unwrap();
        assert!(notice.contains("config.toml.bad"), "{notice}");
        assert!(notice.contains("line 3"), "{notice}");
        assert!(!notice.contains("super-secret-value"), "{notice}");

        // The first write (a cue auto-save or Save) replaces config.toml; the copy stays.
        loaded.config.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), original);

        // A second bad file does not overwrite the first copy.
        std::fs::write(&path, "not toml at all [").unwrap();
        let again = load_config(&path);
        assert!(again.notice.unwrap().contains("config.toml.bad.2"));
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn good_or_missing_config_needs_no_backup() {
        let dir = std::env::temp_dir().join("streamit-app-goodcfg");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        let l = load_config(&path);
        assert!(l.notice.is_none() && l.writable);
        Config::default().save(&path).unwrap();
        assert!(load_config(&path).notice.is_none());
        assert!(!dir.join("config.toml.bad").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
