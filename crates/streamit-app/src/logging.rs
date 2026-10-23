//! Logging: daily files under `%APPDATA%\StreamIt\logs\`, plus the terminal for probes.
//! The window never writes to stderr (I6): it has no console, and a blocked console
//! write must never stall the service.
//!
//! Every Command (through the command log in main), device health change, device error
//! and engine warning is logged at `info` or above. Tokens and the OAuth client secret
//! are never logged: nothing here formats the config, and the YouTube crate logs no
//! credentials. `RUST_LOG` overrides the default `info` level.
use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// `%APPDATA%\StreamIt\logs`, or `logs` in the working directory without APPDATA.
pub fn log_dir() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(dir) => PathBuf::from(dir).join("StreamIt").join("logs"),
        None => PathBuf::from("logs"),
    }
}

/// Installs the global subscriber. `console` adds a stderr layer (probes run from a
/// terminal). Keep the guard alive until exit so the file is flushed.
pub fn init(console: bool) -> Option<WorkerGuard> {
    // The graphics stack logs its adapter list at info; keep it out of the service log.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,wgpu_hal=warn,wgpu_core=warn,egui_wgpu=warn,naga=warn"));
    let dir = log_dir();
    let appender = std::fs::create_dir_all(&dir).ok().and_then(|_| {
        Builder::new()
            .rotation(Rotation::DAILY)
            .filename_prefix("streamit")
            .filename_suffix("log")
            .max_log_files(60)
            .build(&dir)
            .ok()
    });
    let console = console.then(|| fmt::layer().with_writer(std::io::stderr));
    match appender {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            tracing_subscriber::registry()
                .with(filter)
                .with(console)
                .with(fmt::layer().with_ansi(false).with_writer(writer))
                .init();
            tracing::info!(dir = %dir.display(), "logging to daily files");
            Some(guard)
        }
        None => {
            tracing_subscriber::registry().with(filter).with(console).init();
            tracing::warn!(dir = %dir.display(), "could not open the log folder; logging to the terminal only");
            None
        }
    }
}
