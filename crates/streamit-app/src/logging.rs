//! Logging: daily files under `%APPDATA%\StreamIt\logs\`, plus the terminal for probes.
//! The window never writes to stderr (I6): it has no console, and a blocked console
//! write must never stall the service.
//!
//! Two files: `streamit.<date>.log` for the real service and `sim-streamit.<date>.log`
//! for practice. Every event inside the `simulation` span (see [`simulation_span`]) goes
//! to the sim file only, and every other event to the real file only, so a practice run
//! never muddles the record of a real service. The simulation session runs its engine,
//! snapshot log and health poll inside that span (the engine's tasks carry it on), and
//! the session's router logs each command in the span of the session it goes to.
//!
//! Every Command (logged by the session's router), device health change, device
//! error and engine warning is logged at `info` or above. Tokens and the OAuth client
//! secret are never logged: nothing here formats the config, and the YouTube crate logs
//! no credentials. `RUST_LOG` overrides the default `info` level.
use std::path::{Path, PathBuf};
use tracing::{Metadata, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, RollingFileAppender, Rotation};
use tracing_subscriber::filter::DynFilterFn;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// The span whose events go to the simulation log.
pub const SIMULATION: &str = "simulation";

/// `%APPDATA%\StreamIt\logs`, or `logs` in the working directory without APPDATA.
pub fn log_dir() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(dir) => PathBuf::from(dir).join("StreamIt").join("logs"),
        None => PathBuf::from("logs"),
    }
}

/// The simulation span's own target. Nothing else logs under it, and [`env_filter`]
/// always enables it, so no `RUST_LOG` (a level or a list of targets) can switch the span
/// off and send practice events to the real log.
pub const SIM_TARGET: &str = "streamit_simulation";

/// The span a simulation session runs in.
pub fn simulation_span() -> tracing::Span {
    tracing::span!(target: SIM_TARGET, tracing::Level::ERROR, "simulation")
}

/// The event happens inside the simulation span (at any depth).
fn in_simulation<S>(cx: &Context<'_, S>) -> bool
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    cx.lookup_current().is_some_and(|s| s.scope().any(|s| s.name() == SIMULATION))
}

/// The two file layers: events outside the simulation span to `real`, events inside it
/// to `sim`. Spans themselves pass both filters, so each layer can see whether an event
/// is inside the simulation span.
pub fn file_layers<S, R, M>(real: R, sim: M) -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    R: for<'w> MakeWriter<'w> + Send + Sync + 'static,
    M: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let real_only = DynFilterFn::new(|m: &Metadata<'_>, cx: &Context<'_, S>| m.is_span() || !in_simulation(cx));
    let sim_only = DynFilterFn::new(|m: &Metadata<'_>, cx: &Context<'_, S>| m.is_span() || in_simulation(cx));
    fmt::layer()
        .with_ansi(false)
        .with_writer(real)
        .with_filter(real_only)
        .and_then(fmt::layer().with_ansi(false).with_writer(sim).with_filter(sim_only))
}

fn daily(dir: &Path, prefix: &str) -> Option<RollingFileAppender> {
    Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(prefix)
        .filename_suffix("log")
        .max_log_files(60)
        .build(dir)
        .ok()
}

/// The level filter from `spec` (RUST_LOG syntax), always enabling the simulation span
/// (its own target, see [`simulation_span`]) whatever `spec` names, so the routing holds.
pub fn env_filter(spec: &str) -> EnvFilter {
    let always = format!("{SIM_TARGET}=error").parse().expect("a valid directive");
    EnvFilter::new(spec).add_directive(always)
}

/// Installs the global subscriber. `console` adds a stderr layer (probes run from a
/// terminal). Keep the guards alive until exit so the files are flushed.
pub fn init(console: bool) -> Vec<WorkerGuard> {
    // The graphics stack logs its adapter list at info; keep it out of the service log.
    const DEFAULT: &str = "info,wgpu_hal=warn,wgpu_core=warn,egui_wgpu=warn,naga=warn";
    let spec = std::env::var("RUST_LOG").ok().filter(|s| EnvFilter::try_new(s).is_ok());
    let filter = env_filter(spec.as_deref().unwrap_or(DEFAULT));
    let dir = log_dir();
    let made = std::fs::create_dir_all(&dir).is_ok();
    let real = if made { daily(&dir, "streamit") } else { None };
    let sim = if made { daily(&dir, "sim-streamit") } else { None };
    let console = console.then(|| fmt::layer().with_writer(std::io::stderr));
    match real {
        Some(real) => {
            let (real, real_guard) = tracing_appender::non_blocking(real);
            let mut guards = vec![real_guard];
            let sim_ok = sim.is_some();
            let sim = match sim {
                Some(sim) => {
                    let (sim, guard) = tracing_appender::non_blocking(sim);
                    guards.push(guard);
                    BoxMakeWriter::new(sim)
                }
                // Practice events are dropped rather than mixed into the real log.
                None => BoxMakeWriter::new(std::io::sink),
            };
            tracing_subscriber::registry().with(filter).with(console).with(file_layers(real, sim)).init();
            tracing::info!(dir = %dir.display(), "logging to daily files");
            if !sim_ok {
                tracing::warn!(dir = %dir.display(), "could not open the simulation log; practice is not logged");
            }
            guards
        }
        None => {
            tracing_subscriber::registry().with(filter).with(console).init();
            tracing::warn!(dir = %dir.display(), "could not open the log folder; logging to the terminal only");
            Vec::new()
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing::Instrument;

    /// An in-memory log file.
    #[derive(Clone, Default)]
    pub struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Buf {
        pub fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn simulation_events_go_only_to_the_sim_log() {
        let (real, sim) = (Buf::default(), Buf::default());
        let (r, s) = (real.clone(), sim.clone());
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new("info"))
            .with(file_layers(move || r.clone(), move || s.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("real-one");
            simulation_span().in_scope(|| {
                tracing::info!("sim-one");
                tracing::info_span!("inner").in_scope(|| tracing::warn!("sim-nested"));
            });
            // A task spawned inside the span carries it (the engine's tasks do this).
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let task = tokio::spawn(async { tracing::info!("sim-task") }.instrument(simulation_span()));
                task.await.unwrap();
                tracing::info!("real-two");
            });
        });
        let (real, sim) = (real.text(), sim.text());
        for line in ["real-one", "real-two"] {
            assert!(real.contains(line) && !sim.contains(line), "{line}: real {real:?} sim {sim:?}");
        }
        for line in ["sim-one", "sim-nested", "sim-task"] {
            assert!(sim.contains(line) && !real.contains(line), "{line}: real {real:?} sim {sim:?}");
        }
    }

    /// A RUST_LOG that names only some targets must not switch the simulation span off,
    /// or practice events would land in the real log.
    #[test]
    fn target_scoped_rust_log_keeps_the_routing() {
        let (real, sim) = (Buf::default(), Buf::default());
        let (r, s) = (real.clone(), sim.clone());
        let subscriber = tracing_subscriber::registry()
            .with(env_filter("off,streamit::logging::tests=info"))
            .with(file_layers(move || r.clone(), move || s.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("real-one");
            simulation_span().in_scope(|| tracing::info!("sim-one"));
        });
        let (real, sim) = (real.text(), sim.text());
        assert!(real.contains("real-one") && !real.contains("sim-one"), "real {real:?}");
        assert!(sim.contains("sim-one"), "sim {sim:?}");
    }
}
