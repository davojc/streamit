//! StreamIt: runs the church YouTube live stream from a Stream Deck XL.
//!
//!   streamit                      open the window, start the engine and the deck
//!   streamit probe <device> ...   hardware probes (see `streamit probe`)
//!
//! Startup never changes a device: it builds the devices (which only connect and listen),
//! spawns the engine (which runs its read-only Stage 0 / assessment), starts the deck and
//! opens the window. The egui loop runs on the main thread; tokio runs on worker threads.
mod discovery;
mod logging;
mod probe;
mod ui;
mod wiring;

use anyhow::anyhow;
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use streamit_core::config::{Config, config_path};
use streamit_core::engine::{Command, Engine, EngineHandle, Snapshot};
use streamit_core::golive::Stage;
use streamit_deck::DeckSurface;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _guard = logging::init();
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
    let (config, load_error) = match Config::load(&path) {
        Ok(c) => (c, None),
        Err(e) => {
            let msg = format!("{e:#}. Using the defaults; Save will overwrite the file.");
            warn!("{msg}");
            (Config::default(), Some(msg))
        }
    };
    info!(path = %path.display(), "config loaded");
    let shared = Arc::new(RwLock::new(config.clone()));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("streamit-rt")
        .build()?;
    let started = rt.block_on(async {
        let wired = wiring::build(&config).await;
        let engine = Engine::spawn(wired.devices.clone(), shared.clone());
        let tx = spawn_command_log(engine.tx.clone());
        for (kind, health) in &wired.health {
            let _ = tx.send(Command::DeviceHealth(*kind, health.clone())).await;
        }
        wiring::spawn_health_poll(wired.probes, shared.clone(), tx.clone(), &wired.health);
        spawn_snapshot_log(engine.rx.clone());
        DeckSurface::run(EngineHandle { tx: tx.clone(), rx: engine.rx.clone() });
        ui::Started { tx, snap: engine.rx, yt: wired.devices.yt.clone() }
    });

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("StreamIt")
            .with_inner_size([1100.0, 860.0]),
        ..Default::default()
    };
    let handle = rt.handle().clone();
    let result = eframe::run_native(
        "StreamIt",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(ui::StreamItApp::new(handle, started, shared, path, config, load_error)))
        }),
    );
    info!("window closed; StreamIt stopping");
    rt.shutdown_timeout(Duration::from_secs(2));
    result.map_err(|e| anyhow!("the window failed: {e}"))
}

/// Every command from the window, the deck and the health poll passes through here and
/// is logged before it reaches the engine.
fn spawn_command_log(engine: mpsc::Sender<Command>) -> mpsc::Sender<Command> {
    let (tx, mut rx) = mpsc::channel::<Command>(64);
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match &cmd {
                // Health changes are logged by the poll with their reason.
                Command::DeviceHealth(..) => {}
                Command::SetCues(cues) => {
                    let names: Vec<&str> = cues.iter().map(|c| c.name.as_str()).collect();
                    info!(cues = ?names, "command SetCues");
                }
                other => info!(command = ?other, "command"),
            }
            if engine.send(cmd).await.is_err() {
                warn!("the engine has stopped; dropping commands");
                return;
            }
        }
    });
    tx
}

/// The stage without its ticking clock, so the log has one line per real change.
fn stage_label(s: &Stage) -> String {
    match s {
        Stage::Countdown { public, .. } => format!("Countdown (public: {public})"),
        Stage::Ending { .. } => "Ending".into(),
        other => format!("{other:?}"),
    }
}

/// Logs stage changes, offers and every new engine warning.
fn spawn_snapshot_log(mut rx: watch::Receiver<Snapshot>) {
    tokio::spawn(async move {
        let mut last = rx.borrow().clone();
        while rx.changed().await.is_ok() {
            let s = rx.borrow_and_update().clone();
            if stage_label(&s.stage) != stage_label(&last.stage) {
                info!(stage = %stage_label(&s.stage), "stage");
            }
            if s.last_warning != last.last_warning
                && let Some(w) = &s.last_warning
            {
                warn!(warning = %w, "engine warning");
            }
            if s.offers != last.offers {
                info!(offers = ?s.offers, "offers");
            }
            if s.playing != last.playing {
                info!(playing = ?s.playing, "video");
            }
            last = s;
        }
    });
}
