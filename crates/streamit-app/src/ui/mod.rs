//! The window: the header (stage badge, broadcast, equipment, tabs), the simulation
//! banner, and the tabs: Service (`service/`), Videos (`videos.rs`), Settings
//! (`settings.rs`) and, in simulation only, Simulation (`simulation.rs`). Every press
//! becomes the same `Command` the Stream Deck would send.
mod service;
mod settings;
mod simulation;
mod videos;

use crate::discovery::{BmdKind, DiscoveredDevice};
use crate::session::{Front, Mode, Sessions, SwitchRefused};
use crate::theme;
use crate::view::{self, CueSync, ServiceScreen};
use eframe::egui::{self, Color32, Margin, RichText, Ui};
use settings::{RESTART_NOTE, Section, device_settings, join_inputs};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use streamit_core::config::Config;
use streamit_core::devices::{BroadcastSummary, Broadcasts};
use streamit_core::engine::{Command, Snapshot};
use streamit_core::golive::Stage;
use streamit_core::rollin::{Cue, Destination, MAX_CUES};
use streamit_sim::SimControl;
use streamit_youtube::StreamSummary;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

/// Cue edits are sent to the engine once typing/dragging pauses for this long.
const CUE_DEBOUNCE: Duration = Duration::from_millis(400);
/// After a SetCues, wait this long for the engine before trusting its list again.
const CUE_SETTLE: Duration = Duration::from_secs(1);
/// An unsaved cue list is written again this often.
const SAVE_RETRY: Duration = Duration::from_secs(5);
const SIM_UNAVAILABLE: &str = "Not available in simulation";

/// Says why a control that would touch real equipment or YouTube is off in simulation.
trait SimTip {
    fn sim_tip(self, simulating: bool) -> Self;
}

impl SimTip for egui::Response {
    fn sim_tip(self, simulating: bool) -> Self {
        if simulating { self.on_disabled_hover_text(SIM_UNAVAILABLE) } else { self }
    }
}

/// A background job whose result the UI picks up on a later frame. Each `start` gets a
/// fresh slot, so a job started earlier can never overwrite the latest one's result.
struct Pending<T> {
    slot: Arc<Mutex<Option<T>>>,
    running: bool,
}

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Self { slot: Arc::default(), running: false }
    }
}

impl<T: Send + 'static> Pending<T> {
    fn start(&mut self, rt: &Handle, ctx: &egui::Context, fut: impl Future<Output = T> + Send + 'static) {
        self.running = true;
        self.slot = Arc::default();
        let (slot, ctx) = (self.slot.clone(), ctx.clone());
        rt.spawn(async move {
            let v = fut.await;
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
            ctx.request_repaint();
        });
    }

    fn take(&mut self) -> Option<T> {
        let v = self.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        if v.is_some() {
            self.running = false;
        }
        v
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Service,
    Videos,
    Settings,
    /// Only offered in simulation mode.
    Simulation,
}

/// Where the settings live, and whether the window may write them (false when an
/// unreadable file could not be backed up).
pub struct ConfigFile {
    pub path: PathBuf,
    pub writable: bool,
}

/// Closing now could leave the service half done: the service has started and not
/// ended, or a sequence or a video is running.
fn service_at_risk(s: &Snapshot) -> bool {
    !matches!(s.stage, Stage::Idle | Stage::Ended) || s.job.is_some() || s.playing.is_some()
}

pub struct StreamItApp {
    rt: Handle,
    /// For the real ↔ simulation switch and "Crash & restart app"; both run on `rt`.
    sessions: Arc<Sessions>,
    tx: mpsc::Sender<Command>,
    snap: watch::Receiver<Snapshot>,
    /// The current session's YouTube (real or pretend); the broadcast list reloads when
    /// it changes.
    yt: watch::Receiver<Arc<dyn Broadcasts>>,
    mode: watch::Receiver<Mode>,
    /// True while one session is being stopped and the next opened.
    switching: watch::Receiver<bool>,
    /// The pretend world, while simulating (the Simulation tab).
    sim: watch::Receiver<Option<SimControl>>,
    /// Settings the simulation filled in with a simulated default.
    sim_defaults: watch::Receiver<Vec<&'static str>>,
    switch_job: Pending<Result<(), SwitchRefused>>,
    restart_job: Pending<Result<(), SwitchRefused>>,
    /// The Settings section last picked, and the one to scroll to on the next frame.
    section: Section,
    scroll_to: Option<Section>,
    show_add_ip: bool,
    /// The mode the window title was last set for.
    titled: Option<Mode>,
    config: Arc<RwLock<Config>>,
    file: ConfigFile,
    ctx: egui::Context,
    /// The close button was pressed during a service: ask first.
    confirm_close: bool,
    close_confirmed: bool,
    /// "Start service now" from Idle was clicked once in the More menu: the next click sends it.
    confirm_start_now: bool,
    /// The config the devices were built from.
    startup: Config,
    draft: Config,
    tab: Tab,
    notice: Option<String>,

    broadcasts: Vec<BroadcastSummary>,
    /// Restarting it (after a mode switch) drops the old session's result unseen.
    broadcasts_job: Pending<Result<Vec<BroadcastSummary>, String>>,
    broadcasts_error: Option<String>,

    /// The window's copy of the cue list; edits go to the engine with SetCues.
    cues: Vec<Cue>,
    engine_cues: Vec<Cue>,
    cues_dirty_at: Option<Instant>,
    cues_sent_at: Option<Instant>,
    /// An edit is pending or not yet in the engine's list.
    cues_applying: bool,
    /// A video was playing when the last SetCues was sent.
    cues_sent_playing: bool,
    /// The last SetCues has been sent a second time.
    cues_resent: bool,
    /// The list could not be written to the settings file (yet).
    cues_unsaved: bool,
    cues_save_tried_at: Option<Instant>,

    discovered: Vec<DiscoveredDevice>,
    scan_job: Pending<Vec<DiscoveredDevice>>,
    add_ip: String,
    add_kind: Option<BmdKind>,
    live_inputs_text: String,
    signin_job: Pending<Result<(), String>>,
    signin_status: String,
    streams: Vec<StreamSummary>,
    streams_job: Pending<Result<Vec<StreamSummary>, String>>,
    streams_status: String,
    /// File dialogs run on the runtime (they would block the window); results land here.
    add_files_job: Pending<Vec<PathBuf>>,
    countdown_job: Pending<Option<PathBuf>>,
    /// The Videos tab's "Change…" for the countdown video: saved as soon as it is picked.
    countdown_pick_job: Pending<Option<PathBuf>>,
    /// The video selected on the Videos tab.
    selected: usize,
    lengths: videos::Lengths,
}

impl StreamItApp {
    #[allow(clippy::too_many_arguments)] // called once, from main
    pub fn new(
        ctx: &egui::Context,
        rt: Handle,
        sessions: Arc<Sessions>,
        s: Front,
        config: Arc<RwLock<Config>>,
        file: ConfigFile,
        startup: Config,
        load_error: Option<String>,
    ) -> StreamItApp {
        theme::install(ctx);
        let draft = startup.clone();
        let mut app = StreamItApp {
            rt,
            sessions,
            tx: s.tx,
            snap: s.rx,
            yt: s.yt,
            mode: s.mode,
            switching: s.switching,
            sim: s.sim,
            sim_defaults: s.sim_defaults,
            switch_job: Pending::default(),
            restart_job: Pending::default(),
            section: Section::default(),
            scroll_to: None,
            show_add_ip: false,
            titled: None,
            config,
            file,
            ctx: ctx.clone(),
            confirm_close: false,
            close_confirmed: false,
            confirm_start_now: false,
            live_inputs_text: join_inputs(&draft.stream_atem.live_inputs),
            cues: startup.cues.clone(),
            engine_cues: startup.cues.clone(),
            startup,
            draft,
            tab: Tab::Service,
            notice: load_error,
            broadcasts: Vec::new(),
            broadcasts_job: Pending::default(),
            broadcasts_error: None,
            cues_dirty_at: None,
            cues_sent_at: None,
            cues_applying: false,
            cues_sent_playing: false,
            cues_resent: false,
            cues_unsaved: false,
            cues_save_tried_at: None,
            discovered: Vec::new(),
            scan_job: Pending::default(),
            add_ip: String::new(),
            add_kind: None,
            signin_job: Pending::default(),
            signin_status: String::new(),
            streams: Vec::new(),
            streams_job: Pending::default(),
            streams_status: String::new(),
            add_files_job: Pending::default(),
            countdown_job: Pending::default(),
            countdown_pick_job: Pending::default(),
            selected: 0,
            lengths: videos::Lengths::default(),
        };
        app.load_broadcasts();
        app.scan();
        app
    }

    fn send(&self, c: Command) {
        if let Err(e) = self.tx.try_send(c) {
            warn!("could not send a command to the engine: {e}");
        }
    }

    fn load_broadcasts(&mut self) {
        let yt = self.yt.borrow_and_update().clone();
        self.broadcasts_job.start(&self.rt, &self.ctx, async move { yt.upcoming().await.map_err(|e| e.to_string()) });
    }

    /// A mode switch brings another YouTube: forget the old list and load the new one.
    fn follow_youtube(&mut self) {
        if self.yt.has_changed().unwrap_or(false) {
            info!("the session's YouTube changed; reloading the broadcast list");
            self.broadcasts.clear();
            self.broadcasts_error = None;
            self.load_broadcasts();
        }
    }

    /// The window title says when nothing is live; the Simulation tab goes with the mode.
    fn follow_mode(&mut self, ctx: &egui::Context) {
        let mode = *self.mode.borrow();
        if self.titled == Some(mode) {
            return;
        }
        self.titled = Some(mode);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(window_title(mode).into()));
        if mode == Mode::Real && self.tab == Tab::Simulation {
            self.tab = Tab::Service;
        }
    }

    fn poll_jobs(&mut self) {
        Self::take_switch_result(&mut self.switch_job, &mut self.notice);
        Self::take_switch_result(&mut self.restart_job, &mut self.notice);
        if let Some(r) = self.broadcasts_job.take() {
            match r {
                Ok(list) => {
                    self.broadcasts = list;
                    self.broadcasts_error = None;
                }
                Err(e) => self.broadcasts_error = Some(e),
            }
        }
        if let Some(found) = self.scan_job.take() {
            for d in found {
                if !self.discovered.iter().any(|x| x.address == d.address) {
                    self.discovered.push(d);
                }
            }
        }
        if let Some(r) = self.signin_job.take() {
            self.signin_status = match r {
                Ok(()) => {
                    // The running YouTube client reads the new sign-in from the Credential
                    // Manager; refresh the engine and the broadcast list.
                    self.send(Command::Refresh);
                    self.load_broadcasts();
                    if device_settings(&self.draft) != device_settings(&self.startup) {
                        format!("Signed in. {RESTART_NOTE} (YouTube settings changed).")
                    } else {
                        "Signed in.".into()
                    }
                }
                Err(e) => format!("Sign-in failed: {e}"),
            };
        }
        if let Some(r) = self.streams_job.take() {
            match r {
                Ok(list) => {
                    self.streams_status = format!("{} stream key(s)", list.len());
                    self.streams = list;
                }
                Err(e) => self.streams_status = format!("Could not list streams: {e}"),
            }
        }
        if let Some(f) = self.countdown_job.take().flatten() {
            self.draft.playout.countdown_file = f;
        }
        if let Some(f) = self.countdown_pick_job.take().flatten() {
            self.save_countdown(f);
        }
    }

    /// Adds the files the dialog returned, unless a video started meanwhile.
    fn take_added_files(&mut self, snap: &Snapshot) {
        let Some(files) = self.add_files_job.take() else { return };
        if files.is_empty() {
            return;
        }
        if snap.playing.is_some() {
            self.notice = Some("Videos not added: the cue list is locked while a video plays.".into());
            return;
        }
        let room = MAX_CUES.saturating_sub(self.cues.len());
        if files.len() > room {
            self.notice = Some(format!("Only {room} more video(s) fit in the cue list ({MAX_CUES} at most)."));
        }
        for f in files.into_iter().take(room) {
            self.cues.push(new_cue(&f));
        }
        self.cues_dirty_at = Some(Instant::now());
    }

    /// Keeps the window's cue list and the engine's in step, and saves the engine's list.
    fn sync_cues(&mut self, snap: &Snapshot) {
        let engine: Vec<Cue> = snap.cues.iter().map(|(c, _)| c.clone()).collect();
        if engine != self.engine_cues {
            self.engine_cues = engine.clone();
            self.save_cues(&engine);
            if self.cues_dirty_at.is_none() {
                self.cues = engine.clone();
            }
        }
        if let Some(t) = self.cues_dirty_at
            && t.elapsed() >= CUE_DEBOUNCE
        {
            self.cues_dirty_at = None;
            self.cues_sent_at = Some(Instant::now());
            self.cues_sent_playing = snap.playing.is_some();
            self.cues_resent = false;
            self.send(Command::SetCues(self.cues.clone()));
        }
        // The engine ignores SetCues while a video plays: fall back to its list. If
        // nothing is playing, the edit is sent once more before it is given up.
        let sent_age = self.cues_sent_at.map(|t| t.elapsed());
        let playing = snap.playing.is_some() || self.cues_sent_playing;
        let sync = view::cue_sync(
            self.cues_dirty_at.is_some(),
            sent_age,
            self.cues != engine,
            CUE_SETTLE,
            playing,
            self.cues_resent,
        );
        self.cues_applying = matches!(sync, CueSync::Applying | CueSync::Resend);
        match sync {
            CueSync::Reverted | CueSync::GaveUp => {
                // A newer notice stays: it is more recent than this edit.
                if self.notice.is_none() {
                    self.notice = Some(
                        if sync == CueSync::Reverted {
                            view::CUES_REVERTED_NOTICE
                        } else {
                            view::CUES_NOT_APPLIED_NOTICE
                        }
                        .into(),
                    );
                }
                self.cues = engine.clone();
                self.cues_sent_at = None;
            }
            CueSync::Resend => {
                self.cues_resent = true;
                self.cues_sent_at = Some(Instant::now());
                self.send(Command::SetCues(self.cues.clone()));
            }
            CueSync::Resync => self.cues = engine.clone(),
            _ => {}
        }
        // A list that could not be written is tried again every few seconds.
        if view::should_retry_save(self.cues_unsaved, self.cues_save_tried_at.map(|t| t.elapsed()), SAVE_RETRY) {
            self.save_cues(&engine);
        }
    }

    /// Saves the engine's list. `cues_unsaved` stays set until a write succeeds.
    fn save_cues(&mut self, cues: &[Cue]) {
        let (c, changed) = {
            let mut c = self.config.write().unwrap_or_else(|e| e.into_inner());
            let changed = c.cues != cues;
            if changed {
                c.cues = cues.to_vec();
            }
            (c.clone(), changed)
        };
        if !changed && !self.cues_unsaved {
            return;
        }
        self.draft.cues = cues.to_vec();
        self.cues_save_tried_at = Some(Instant::now());
        let result = if self.file.writable {
            c.save(&self.file.path).map_err(|e| format!("{e:#}"))
        } else {
            Err("the settings file could not be backed up".to_string())
        };
        self.cues_unsaved = view::cues_unsaved_after(self.file.writable, result.is_ok());
        if let Err(e) = result {
            warn!("saving the cue list: {e}");
            if changed {
                self.notice = Some(format!("Could not save the cue list: {e}"));
            }
        }
    }

    /// Writes the countdown video choice to the shared settings and the file, leaving the
    /// Settings tab's unsaved edits alone.
    fn save_countdown(&mut self, file: PathBuf) {
        let c = {
            let mut shared = self.config.write().unwrap_or_else(|e| e.into_inner());
            shared.playout.countdown_file = file.clone();
            shared.clone()
        };
        self.draft.playout.countdown_file = file;
        if !self.file.writable {
            self.notice = Some("Not saved: the settings file could not be backed up (see the log).".into());
            return;
        }
        if let Err(e) = c.save(&self.file.path) {
            warn!("saving the countdown video: {e:#}");
            self.notice = Some(format!("Could not save the countdown video: {e:#}"));
        }
    }

}

impl StreamItApp {
    /// The window's close button during a service asks first; it closes only on an
    /// explicit confirm.
    fn close_guard(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_confirmed && service_at_risk(snap) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            info!("close requested during a service; asking for confirmation");
            self.confirm_close = true;
        }
        if !self.confirm_close {
            return;
        }
        let what = match (&snap.job, snap.playing) {
            (_, Some(i)) => format!("Video {} is playing.", i + 1),
            (Some(job), None) => format!("{job} is running."),
            (None, None) => format!("The service is at stage {:?}.", snap.stage),
        };
        egui::Modal::new(egui::Id::unique("confirm_close")).show(&ctx, |ui| {
            ui.heading("Close StreamIt during the service?");
            ui.label(what);
            ui.label(
                "Closing stops the Stream Deck and leaves the devices as they are now: a video, \
                 the countdown or End Service stops part-way, and nothing returns the switchers, \
                 the mics or the stream to normal. The YouTube broadcast and the Web Presenter keep \
                 going until someone ends them.",
            );
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Keep StreamIt open").strong()).clicked() {
                    self.confirm_close = false;
                }
                if ui.button(RichText::new("Close anyway").color(Color32::RED)).clicked() {
                    warn!("closing during the service (confirmed)");
                    self.close_confirmed = true;
                    self.confirm_close = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }
}

fn window_title(mode: Mode) -> &'static str {
    match mode {
        Mode::Real => "StreamIt",
        Mode::Simulation => "StreamIt — SIMULATION",
    }
}

impl eframe::App for StreamItApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.follow_youtube();
        self.poll_jobs();
        let snap = self.snap.borrow().clone();
        self.take_added_files(&snap);
        self.sync_cues(&snap);
        self.close_guard(ui, &snap);
        self.follow_mode(&ui.ctx().clone());
        // Keep the timers, the countdown and device colours current.
        ui.ctx().request_repaint_after(Duration::from_millis(100));

        let config = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        // What the running simulation was built from (M6): the Service screens describe
        // what the engine does, so in simulation they use this; the settings file's newer
        // edits apply the next time simulation opens.
        let running = self.sim.borrow().as_ref().map(|s| s.config().clone());
        let simulating = *self.mode.borrow() == Mode::Simulation || snap.simulation;
        if simulating {
            egui::Panel::top("sim_banner").frame(egui::Frame::new()).show_separator_line(false).show(ui, |ui| {
                if theme::sim_banner(ui) {
                    self.tab = Tab::Settings;
                    self.section = Section::Equipment;
                    self.scroll_to = Some(Section::Equipment);
                }
            });
        }
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(theme::HEADER).inner_margin(Margin::symmetric(28, 16)))
            .show(ui, |ui| self.header(ui, &snap, &config));
        if let Some(n) = self.notice.clone() {
            egui::Panel::top("notice")
                .frame(egui::Frame::new().fill(theme::AMBER_DARK).inner_margin(Margin::symmetric(28, 8)))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(n).color(theme::AMBER_TEXT));
                        if ui.small_button("Dismiss").clicked() {
                            self.notice = None;
                        }
                    });
                });
        }
        if self.tab == Tab::Service {
            egui::Panel::top("steps")
                .frame(egui::Frame::new().fill(theme::GROUND).inner_margin(Margin::symmetric(28, 14)))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        theme::step_strip(ui, view::steps(&snap));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            self.more_menu(ui, &snap)
                        });
                    });
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::GROUND).inner_margin(Margin::symmetric(28, 24)))
            .show(ui, |ui| {
                let viewport = ui.available_height();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                    Tab::Service => self.service_tab(ui, &snap, running.as_ref().unwrap_or(&config), &config, viewport),
                    Tab::Videos => self.videos_tab(ui, &snap, &config, running.as_ref()),
                    Tab::Settings => self.settings_tab(ui, &snap),
                    Tab::Simulation => self.simulation_tab(ui, &snap),
                });
            });
    }
}

impl StreamItApp {
    fn header(&mut self, ui: &mut Ui, snap: &Snapshot, config: &Config) {
        let h = view::header(snap, config);
        let switching = *self.switching.borrow();
        let simulating = *self.mode.borrow() == Mode::Simulation;
        // The first screen lists every device, so its header leaves the summary out.
        let summary = !(self.tab == Tab::Service && view::service_screen(snap) == ServiceScreen::BeforeService);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 24.0;
            let mut name = egui::text::LayoutJob::default();
            name.append("Stream", 0.0, egui::TextFormat::simple(theme::heading_font(24.0), theme::TEXT));
            name.append("It", 0.0, egui::TextFormat::simple(theme::heading_font(24.0), theme::RED));
            ui.label(name);
            if switching {
                theme::switching_badge(ui);
            } else {
                theme::badge(ui, &h);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let tabs: &[(Tab, &str)] = if simulating {
                    &[
                        (Tab::Settings, "Settings"),
                        (Tab::Simulation, "Simulation"),
                        (Tab::Videos, "Videos"),
                        (Tab::Service, "Service"),
                    ]
                } else {
                    &[(Tab::Settings, "Settings"), (Tab::Videos, "Videos"), (Tab::Service, "Service")]
                };
                for &(tab, name) in tabs {
                    if theme::tab(ui, name, self.tab == tab, tab == Tab::Simulation).clicked() {
                        self.tab = tab;
                    }
                }
                if summary {
                    ui.add_space(18.0);
                    ui.label(RichText::new(&h.equipment).size(14.0).color(theme::MUTED));
                    theme::dot(ui, theme::tone_color(h.equipment_tone));
                    ui.add_space(6.0);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        theme::lbl(ui, "Broadcast");
                        ui.add(egui::Label::new(RichText::new(&h.broadcast).size(16.0)).truncate());
                    });
                });
            });
        });
    }
}

// ---------------------------------------------------------------- Videos tab (videos.rs)

fn new_cue(path: &Path) -> Cue {
    Cue {
        path: path.to_path_buf(),
        name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        destination: Destination::HallAndOnline,
        gain_db: 0.0,
        return_input: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_is_risky_only_during_a_service() {
        let idle = Snapshot::default();
        assert!(!service_at_risk(&idle));
        assert!(!service_at_risk(&Snapshot { stage: Stage::Ended, ..Snapshot::default() }));
        assert!(service_at_risk(&Snapshot { stage: Stage::Live, ..Snapshot::default() }));
        assert!(service_at_risk(&Snapshot { job: Some("Prepare"), ..Snapshot::default() }));
        assert!(service_at_risk(&Snapshot { playing: Some(0), ..Snapshot::default() }));
    }

    #[test]
    fn a_restarted_job_ignores_the_earlier_result() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let ctx = egui::Context::default();
        let mut job: Pending<&'static str> = Pending::default();
        let (release_old, old_gate) = std::sync::mpsc::channel::<()>();
        // The old session's load hangs until released; the new one finishes at once.
        job.start(rt.handle(), &ctx, async move {
            tokio::task::spawn_blocking(move || old_gate.recv()).await.ok();
            "old"
        });
        job.start(rt.handle(), &ctx, async { "new" });
        let deadline = Instant::now() + Duration::from_secs(5);
        let first = loop {
            if let Some(v) = job.take() {
                break v;
            }
            assert!(Instant::now() < deadline, "the new result never arrived");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(first, "new");
        assert!(!job.running, "the picker is not left loading");
        release_old.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(job.take(), None, "the old result never lands");
        assert!(!job.running);
        rt.shutdown_timeout(Duration::from_secs(1));
    }
}
