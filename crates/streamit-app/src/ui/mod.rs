//! The window: the header (stage badge, broadcast, equipment, tabs), the simulation
//! banner, and the tabs: Service (`service.rs`), Videos, Settings and, in simulation
//! only, Simulation. Every press becomes the same `Command` the Stream Deck would send.
//! The Settings tab is v1's, restyled later.
mod service;
mod videos;

use crate::discovery::{self, BmdKind, DiscoveredDevice};
use crate::session::{Front, Mode};
use crate::theme;
use crate::view::{self, ServiceScreen};
use eframe::egui::{self, Color32, Margin, RichText, Ui};
use std::future::Future;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use streamit_core::config::{AudioSourceId, Config};
use streamit_core::devices::{BroadcastSummary, Broadcasts};
use streamit_core::engine::{Command, Snapshot};
use streamit_core::golive::Stage;
use streamit_core::rollin::{Cue, Destination, MAX_CUES};
use streamit_youtube::{API_BASE, Auth, StreamSummary, YouTubeClient};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

/// Cue edits are sent to the engine once typing/dragging pauses for this long.
const CUE_DEBOUNCE: Duration = Duration::from_millis(400);
/// After a SetCues, wait this long for the engine before trusting its list again.
const CUE_SETTLE: Duration = Duration::from_secs(1);
const SCAN_FOR: Duration = Duration::from_secs(4);
const RESTART_NOTE: &str = "Restart StreamIt to apply device changes";
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    StreamAtem,
    ScreensAtem,
    VideoHub,
    WebPresenter,
}

const ROLES: [(Role, &str); 4] = [
    (Role::StreamAtem, "Stream ATEM"),
    (Role::ScreensAtem, "Screens ATEM"),
    (Role::VideoHub, "VideoHub"),
    (Role::WebPresenter, "Web Presenter"),
];

fn role_address(c: &mut Config, r: Role) -> &mut Option<String> {
    match r {
        Role::StreamAtem => &mut c.stream_atem.address,
        Role::ScreensAtem => &mut c.screens_atem.address,
        Role::VideoHub => &mut c.videohub.address,
        Role::WebPresenter => &mut c.web_presenter.address,
    }
}

fn roles_for(kind: Option<BmdKind>) -> Vec<(Role, &'static str)> {
    ROLES
        .iter()
        .copied()
        .filter(|(r, _)| match kind {
            Some(BmdKind::Atem) => matches!(r, Role::StreamAtem | Role::ScreensAtem),
            Some(BmdKind::VideoHub) => *r == Role::VideoHub,
            Some(BmdKind::WebPresenter) => *r == Role::WebPresenter,
            None => true,
        })
        .collect()
}

/// The settings that are only read when the devices are built at startup.
fn device_settings(c: &Config) -> impl PartialEq + use<> {
    (
        (
            c.stream_atem.address.clone(),
            c.screens_atem.address.clone(),
            c.videohub.address.clone(),
            c.web_presenter.address.clone(),
        ),
        c.youtube.clone(),
        // The countdown file is read when the countdown starts, so it is not listed.
        (c.playout.decklink_device, c.playout.video_mode.clone()),
        c.ndi.clone(),
    )
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
    tx: mpsc::Sender<Command>,
    snap: watch::Receiver<Snapshot>,
    /// The current session's YouTube (real or pretend); the broadcast list reloads when
    /// it changes.
    yt: watch::Receiver<Arc<dyn Broadcasts>>,
    mode: watch::Receiver<Mode>,
    /// True while one session is being stopped and the next opened.
    switching: watch::Receiver<bool>,
    /// The mode the window title was last set for.
    titled: Option<Mode>,
    config: Arc<RwLock<Config>>,
    file: ConfigFile,
    ctx: egui::Context,
    /// The close button was pressed during a service: ask first.
    confirm_close: bool,
    close_confirmed: bool,
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
    pub fn new(
        ctx: &egui::Context,
        rt: Handle,
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
            tx: s.tx,
            snap: s.rx,
            yt: s.yt,
            mode: s.mode,
            switching: s.switching,
            titled: None,
            config,
            file,
            ctx: ctx.clone(),
            confirm_close: false,
            close_confirmed: false,
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

    fn scan(&mut self) {
        self.scan_job.start(&self.rt, &self.ctx, async move {
            tokio::task::spawn_blocking(|| discovery::browse(SCAN_FOR)).await.unwrap_or_default()
        });
    }

    fn poll_jobs(&mut self) {
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
            self.send(Command::SetCues(self.cues.clone()));
        }
        // The engine ignores SetCues while a video plays: fall back to its list.
        if self.cues_dirty_at.is_none()
            && self.cues_sent_at.is_none_or(|t| t.elapsed() >= CUE_SETTLE)
            && self.cues != engine
        {
            self.cues = engine;
            self.cues_sent_at = None;
        }
    }

    fn save_cues(&mut self, cues: &[Cue]) {
        let c = {
            let mut c = self.config.write().unwrap_or_else(|e| e.into_inner());
            if c.cues == cues {
                return;
            }
            c.cues = cues.to_vec();
            c.clone()
        };
        self.draft.cues = cues.to_vec();
        if !self.file.writable {
            return;
        }
        if let Err(e) = c.save(&self.file.path) {
            warn!("saving the cue list: {e:#}");
            self.notice = Some(format!("Could not save the cue list: {e:#}"));
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

    fn save_settings(&mut self) {
        match parse_inputs(&self.live_inputs_text) {
            Ok(v) => self.draft.stream_atem.live_inputs = v,
            Err(e) => {
                self.notice = Some(e);
                return;
            }
        }
        let c = {
            let mut shared = self.config.write().unwrap_or_else(|e| e.into_inner());
            let mut c = self.draft.clone();
            c.cues = shared.cues.clone();
            *shared = c.clone();
            c
        };
        if !self.file.writable {
            self.notice = Some("Not saved: the settings file could not be backed up (see the log).".into());
            return;
        }
        match c.save(&self.file.path) {
            Ok(()) => {
                info!(path = %self.file.path.display(), "settings saved");
                let restart = device_settings(&c) != device_settings(&self.startup);
                self.notice = Some(if restart {
                    format!("Saved. {RESTART_NOTE}.")
                } else {
                    "Saved. Timings and other settings apply now.".into()
                });
            }
            Err(e) => {
                warn!("saving settings: {e:#}");
                self.notice = Some(format!("Could not save: {e:#}"));
            }
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
        let simulating = *self.mode.borrow() == Mode::Simulation || snap.simulation;
        if simulating {
            egui::Panel::top("sim_banner").frame(egui::Frame::new()).show_separator_line(false).show(ui, |ui| {
                if theme::sim_banner(ui) {
                    self.tab = Tab::Settings;
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
                    Tab::Service => self.service_tab(ui, &snap, &config, viewport),
                    Tab::Videos => self.videos_tab(ui, &snap, &config),
                    Tab::Settings => theme::panel().show(ui, |ui| self.settings_tab(ui)).inner,
                    Tab::Simulation => simulation_tab(ui),
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

/// Task 9 builds this tab; for now it only says where the controls will be.
fn simulation_tab(ui: &mut Ui) {
    theme::panel().show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::lbl(ui, "Simulation");
        ui.label(
            RichText::new("The pretend equipment controls arrive here in a later update. Leave simulation in Settings › Equipment.")
                .color(theme::SOFT),
        );
    });
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

// ---------------------------------------------------------------- Settings tab

fn join_inputs(v: &[u16]) -> String {
    v.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
}

fn parse_inputs(s: &str) -> Result<Vec<u16>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| x.parse::<u16>().map_err(|_| format!("Live inputs: {x:?} is not an input number")))
        .collect()
}

fn address(ui: &mut Ui, label: &str, a: &mut Option<String>) {
    ui.label(label);
    let mut s = a.clone().unwrap_or_default();
    if ui.add(egui::TextEdit::singleline(&mut s).hint_text("not set").desired_width(160.0)).changed() {
        *a = (!s.trim().is_empty()).then(|| s.trim().to_string());
    }
    ui.end_row();
}

fn num<N: egui::emath::Numeric>(ui: &mut Ui, label: &str, v: &mut N) {
    ui.label(label);
    ui.add(egui::DragValue::new(v));
    ui.end_row();
}

fn millis(ui: &mut Ui, label: &str, d: &mut Duration) {
    ui.label(label);
    let mut ms = d.as_millis() as u64;
    if ui.add(egui::DragValue::new(&mut ms).range(0..=600_000).speed(10).suffix(" ms")).changed() {
        *d = Duration::from_millis(ms);
    }
    ui.end_row();
}

impl StreamItApp {
    fn settings_tab(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new(format!(
                "{RESTART_NOTE}: device addresses and roles, the YouTube client and stream key, \
                 and the playout and NDI settings are read when StreamIt starts. Timings, inputs \
                 and audio settings apply as soon as you Save."
            ))
            .strong(),
        );
        if device_settings(&self.config.read().unwrap_or_else(|e| e.into_inner())) != device_settings(&self.startup) {
            ui.label(RichText::new(format!("Saved device changes are waiting: {RESTART_NOTE}.")).color(Color32::RED));
        }
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Save").strong().size(16.0)).clicked() {
                self.save_settings();
            }
            if ui.button("Revert to saved").clicked() {
                self.draft = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
                self.live_inputs_text = join_inputs(&self.draft.stream_atem.live_inputs);
            }
        });
        ui.separator();
        self.devices_section(ui);
        ui.separator();
        self.youtube_section(ui);
        ui.separator();
        self.config_fields(ui);
    }

    fn devices_section(&mut self, ui: &mut Ui) {
        ui.heading("Devices on the network");
        ui.horizontal(|ui| {
            let busy = self.scan_job.running;
            // Simulation never touches the network: no mDNS scan.
            let simulating = *self.mode.borrow() == Mode::Simulation;
            if ui.add_enabled(!busy && !simulating, egui::Button::new("Scan")).sim_tip(simulating).clicked() {
                self.scan();
            }
            if busy {
                ui.spinner();
                ui.label("Looking for Blackmagic devices...");
            }
        });
        let mut assign: Option<(IpAddr, Option<Role>)> = None;
        egui::Grid::new("found").striped(true).show(ui, |ui| {
            ui.label("Name");
            ui.label("Model");
            ui.label("Address");
            ui.label("Kind");
            ui.label("Role");
            ui.end_row();
            for d in &self.discovered {
                ui.label(&d.name);
                ui.label(&d.model);
                ui.label(d.address.to_string());
                ui.label(d.kind.map(BmdKind::label).unwrap_or("unknown"));
                let ip = d.address.to_string();
                let current = ROLES
                    .iter()
                    .find(|(r, _)| role_address(&mut self.draft, *r).as_deref() == Some(ip.as_str()))
                    .map(|(r, name)| (*r, *name));
                egui::ComboBox::from_id_salt(("role", ip.clone()))
                    .selected_text(current.map(|(_, n)| n).unwrap_or("(none)"))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(current.is_none(), "(none)").clicked() {
                            assign = Some((d.address, None));
                        }
                        for (r, name) in roles_for(d.kind) {
                            if ui.selectable_label(current.map(|c| c.0) == Some(r), name).clicked() {
                                assign = Some((d.address, Some(r)));
                            }
                        }
                    });
                ui.end_row();
            }
        });
        if let Some((ip, role)) = assign {
            let ip = ip.to_string();
            for (r, _) in ROLES {
                let a = role_address(&mut self.draft, r);
                if a.as_deref() == Some(ip.as_str()) && Some(r) != role {
                    *a = None;
                }
            }
            if let Some(r) = role {
                *role_address(&mut self.draft, r) = Some(ip);
            }
        }
        ui.horizontal(|ui| {
            ui.label("Add by IP:");
            ui.add(egui::TextEdit::singleline(&mut self.add_ip).hint_text("192.168.4.x").desired_width(120.0));
            egui::ComboBox::from_id_salt("add_kind")
                .selected_text(self.add_kind.map(BmdKind::label).unwrap_or("any"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.add_kind, None, "any");
                    for k in [BmdKind::Atem, BmdKind::VideoHub, BmdKind::WebPresenter] {
                        ui.selectable_value(&mut self.add_kind, Some(k), k.label());
                    }
                });
            if ui.button("Add").clicked() {
                match self.add_ip.trim().parse::<IpAddr>() {
                    Ok(address) if !self.discovered.iter().any(|d| d.address == address) => {
                        self.discovered.push(DiscoveredDevice {
                            name: "Added by IP".into(),
                            model: String::new(),
                            address,
                            kind: self.add_kind,
                        });
                        self.add_ip.clear();
                    }
                    Ok(_) => self.notice = Some("That address is already in the list.".into()),
                    Err(_) => self.notice = Some(format!("{:?} is not an IP address.", self.add_ip.trim())),
                }
            }
        });
    }

    fn youtube_section(&mut self, ui: &mut Ui) {
        ui.heading("YouTube");
        egui::Grid::new("yt").show(ui, |ui| {
            ui.label("OAuth client id");
            ui.add(egui::TextEdit::singleline(&mut self.draft.youtube.client_id).desired_width(420.0));
            ui.end_row();
            ui.label("OAuth client secret");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.youtube.client_secret).password(true).desired_width(420.0),
            );
            ui.end_row();
            ui.label("Stream key id");
            let mut s = self.draft.youtube.stream_id.clone().unwrap_or_default();
            if ui.add(egui::TextEdit::singleline(&mut s).hint_text("pick below").desired_width(420.0)).changed() {
                self.draft.youtube.stream_id = (!s.trim().is_empty()).then(|| s.trim().to_string());
            }
            ui.end_row();
        });
        let (id, secret) = (self.draft.youtube.client_id.clone(), self.draft.youtube.client_secret.clone());
        let have_client = !id.trim().is_empty() && !secret.trim().is_empty();
        // Both build a real YouTube client: never in simulation.
        let simulating = *self.mode.borrow() == Mode::Simulation;
        ui.horizontal(|ui| {
            let busy = self.signin_job.running;
            if ui
                .add_enabled(have_client && !busy && !simulating, egui::Button::new("Sign in"))
                .sim_tip(simulating)
                .clicked()
            {
                let (id, secret, ctx) = (id.clone(), secret.clone(), ui.ctx().clone());
                self.signin_status = "Waiting for you to approve in the browser...".into();
                self.signin_job.start(&self.rt, &ctx, async move {
                    Auth::sign_in(&id, &secret).await.map(|_| ()).map_err(|e| e.to_string())
                });
            }
            if busy {
                ui.spinner();
            }
            ui.label(&self.signin_status);
        });
        ui.horizontal(|ui| {
            let busy = self.streams_job.running;
            if ui
                .add_enabled(have_client && !busy && !simulating, egui::Button::new("Load stream keys"))
                .sim_tip(simulating)
                .clicked()
            {
                let (id, secret, ctx) = (id.clone(), secret.clone(), ui.ctx().clone());
                self.streams_job.start(&self.rt, &ctx, async move {
                    let auth = Auth::load(&id, &secret).map_err(|e| e.to_string())?;
                    let base = url::Url::parse(API_BASE).map_err(|e| e.to_string())?;
                    YouTubeClient::new(base, auth, None).list_streams().await.map_err(|e| e.to_string())
                });
            }
            let current = self.draft.youtube.stream_id.clone();
            let label = |s: &StreamSummary| format!("{}  {}", s.title, s.key_name);
            let selected = self
                .streams
                .iter()
                .find(|s| Some(&s.id) == current.as_ref())
                .map(label)
                .or(current.clone())
                .unwrap_or_else(|| "(none)".into());
            egui::ComboBox::from_id_salt("stream").selected_text(selected).width(320.0).show_ui(ui, |ui| {
                for s in &self.streams {
                    if ui.selectable_label(current.as_ref() == Some(&s.id), label(s)).clicked() {
                        self.draft.youtube.stream_id = Some(s.id.clone());
                    }
                }
            });
            ui.label(&self.streams_status);
        });
    }

    fn config_fields(&mut self, ui: &mut Ui) {
        let c = &mut self.draft;
        ui.heading("Stream ATEM");
        egui::Grid::new("stream_atem").show(ui, |ui| {
            address(ui, "Address", &mut c.stream_atem.address);
            num(ui, "Media player", &mut c.stream_atem.media_player);
            num(ui, "Opening still (0-based slot)", &mut c.stream_atem.opening_still);
            num(ui, "Closing still (0-based slot)", &mut c.stream_atem.closing_still);
            num(ui, "DeckLink input", &mut c.stream_atem.decklink_input);
            ui.label("Live inputs (comma separated)");
            ui.text_edit_singleline(&mut self.live_inputs_text);
            ui.end_row();
            num(ui, "Default live input", &mut c.stream_atem.default_live_input);
            ui.label("Mics during a video (-100 = off)");
            ui.add(egui::DragValue::new(&mut c.stream_atem.duck_db).range(-100.0..=0.0).speed(0.5).suffix(" dB"));
            ui.end_row();
            millis(ui, "Mic fade", &mut c.stream_atem.fade);
        });
        ui.label("Mic sources (Fairlight input / source):");
        let mut remove = None;
        for (i, m) in c.stream_atem.mic_sources.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut m.input));
                ui.add(egui::DragValue::new(&mut m.source));
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            c.stream_atem.mic_sources.remove(i);
        }
        if ui.button("Add mic source").clicked() {
            c.stream_atem.mic_sources.push(AudioSourceId { input: 1001, source: -65280 });
        }

        ui.heading("Screens ATEM");
        egui::Grid::new("screens_atem").show(ui, |ui| {
            address(ui, "Address", &mut c.screens_atem.address);
            num(ui, "DeckLink input", &mut c.screens_atem.decklink_input);
            num(ui, "Input at T-0", &mut c.screens_atem.t0_input);
        });

        ui.heading("VideoHub");
        egui::Grid::new("videohub").show(ui, |ui| {
            address(ui, "Address", &mut c.videohub.address);
            num(ui, "Output feeding ATEM input 8 (0 = not set)", &mut c.videohub.input8_output);
            num(ui, "DeckLink router input (0 = not set)", &mut c.videohub.decklink_input);
            millis(ui, "Settle time", &mut c.videohub.settle);
            num(ui, "Fallback normal input (0 = not set)", &mut c.videohub.fallback_normal_input);
        });

        ui.heading("Web Presenter");
        egui::Grid::new("webpresenter").show(ui, |ui| {
            address(ui, "Address", &mut c.web_presenter.address);
        });

        ui.heading("Timings");
        egui::Grid::new("timings").show(ui, |ui| {
            millis(ui, "Go public before T-0", &mut c.timings.go_public_offset);
            millis(ui, "Closing hold", &mut c.timings.closing_hold);
        });

        ui.heading("Playout");
        egui::Grid::new("playout").show(ui, |ui| {
            num(ui, "DeckLink device", &mut c.playout.decklink_device);
            ui.label("Video mode");
            ui.text_edit_singleline(&mut c.playout.video_mode);
            ui.end_row();
            ui.label("Countdown video");
            ui.horizontal(|ui| {
                let mut s = c.playout.countdown_file.display().to_string();
                if ui.add(egui::TextEdit::singleline(&mut s).desired_width(360.0)).changed() {
                    c.playout.countdown_file = PathBuf::from(s);
                }
                if ui.add_enabled(!self.countdown_job.running, egui::Button::new("Browse...")).clicked() {
                    self.countdown_job.start(&self.rt, &self.ctx, async {
                        rfd::AsyncFileDialog::new()
                            .add_filter("Video", &["mp4", "mov", "m4v", "mkv"])
                            .pick_file()
                            .await
                            .map(|f| f.path().to_path_buf())
                    });
                }
            });
            ui.end_row();
        });

        ui.heading("NDI counter");
        egui::Grid::new("ndi").show(ui, |ui| {
            ui.label("Source name");
            ui.text_edit_singleline(&mut c.ndi.source_name);
            ui.end_row();
            ui.label("Numbers colour");
            egui::color_picker::color_edit_button_srgb(ui, &mut c.ndi.fg);
            ui.end_row();
            ui.label("Background colour");
            egui::color_picker::color_edit_button_srgb(ui, &mut c.ndi.bg);
            ui.end_row();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_inputs_parse() {
        assert_eq!(parse_inputs("1, 2,3"), Ok(vec![1, 2, 3]));
        assert_eq!(parse_inputs(""), Ok(vec![]));
        assert!(parse_inputs("1, x").is_err());
    }

    #[test]
    fn device_changes_need_a_restart_but_timings_do_not() {
        let a = Config::default();
        let mut b = a.clone();
        b.timings.closing_hold = Duration::from_secs(3);
        b.videohub.input8_output = 7;
        assert!(device_settings(&a) == device_settings(&b));
        b.videohub.address = Some("10.0.0.1".into());
        assert!(device_settings(&a) != device_settings(&b));
        let mut c = a.clone();
        c.youtube.stream_id = Some("s".into());
        assert!(device_settings(&a) != device_settings(&c));
        let mut d = a.clone();
        d.playout.countdown_file = PathBuf::from("other.mp4");
        assert!(device_settings(&a) == device_settings(&d), "the countdown file is read live");
    }

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
