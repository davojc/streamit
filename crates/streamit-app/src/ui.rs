//! The window: a Run tab (what the operator uses during a service) and a Settings tab.
//! Plain egui; every press becomes the same `Command` the Stream Deck would send.
use crate::discovery::{self, BmdKind, DiscoveredDevice};
use eframe::egui::{self, Color32, RichText, Ui, vec2};
use std::future::Future;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use streamit_core::config::{AudioSourceId, Config};
use streamit_core::devices::{BroadcastSummary, Broadcasts, Fix};
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, Snapshot};
use streamit_core::rollin::{Cue, CueState, Destination, MAX_CUES};
use streamit_deck::layout::{self, KEYS, Rgb};
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

pub struct Started {
    pub tx: mpsc::Sender<Command>,
    pub snap: watch::Receiver<Snapshot>,
    pub yt: Arc<dyn Broadcasts>,
}

/// A background job whose result the UI picks up on a later frame.
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
    Run,
    Settings,
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
        c.playout.clone(),
        c.ndi.clone(),
    )
}

pub struct StreamItApp {
    rt: Handle,
    tx: mpsc::Sender<Command>,
    snap: watch::Receiver<Snapshot>,
    yt: Arc<dyn Broadcasts>,
    config: Arc<RwLock<Config>>,
    config_path: PathBuf,
    /// The config the devices were built from.
    startup: Config,
    draft: Config,
    tab: Tab,
    notice: Option<String>,

    broadcasts: Vec<BroadcastSummary>,
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
}

impl StreamItApp {
    pub fn new(
        rt: Handle,
        s: Started,
        config: Arc<RwLock<Config>>,
        config_path: PathBuf,
        startup: Config,
        load_error: Option<String>,
    ) -> StreamItApp {
        let draft = startup.clone();
        let mut app = StreamItApp {
            rt,
            tx: s.tx,
            snap: s.snap,
            yt: s.yt,
            config,
            config_path,
            live_inputs_text: join_inputs(&draft.stream_atem.live_inputs),
            cues: startup.cues.clone(),
            engine_cues: startup.cues.clone(),
            startup,
            draft,
            tab: Tab::Run,
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
        };
        app.load_broadcasts(None);
        app.scan(None);
        app
    }

    fn send(&self, c: Command) {
        if let Err(e) = self.tx.try_send(c) {
            warn!("could not send a command to the engine: {e}");
        }
    }

    fn load_broadcasts(&mut self, ctx: Option<&egui::Context>) {
        let yt = self.yt.clone();
        let ctx = ctx.cloned().unwrap_or_default();
        self.broadcasts_job
            .start(&self.rt, &ctx, async move { yt.upcoming().await.map_err(|e| e.to_string()) });
    }

    fn scan(&mut self, ctx: Option<&egui::Context>) {
        let ctx = ctx.cloned().unwrap_or_default();
        self.scan_job.start(&self.rt, &ctx, async move {
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
                    self.load_broadcasts(None);
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
        if let Err(e) = c.save(&self.config_path) {
            warn!("saving the cue list: {e:#}");
            self.notice = Some(format!("Could not save the cue list: {e:#}"));
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
        match c.save(&self.config_path) {
            Ok(()) => {
                info!(path = %self.config_path.display(), "settings saved");
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

impl eframe::App for StreamItApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.poll_jobs();
        let snap = self.snap.borrow().clone();
        self.sync_cues(&snap);
        // Keep blinking keys, the countdown and device colours current.
        ui.ctx().request_repaint_after(Duration::from_millis(250));

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Run, RichText::new("Run").size(18.0));
                ui.selectable_value(&mut self.tab, Tab::Settings, RichText::new("Settings").size(18.0));
                if let Some(n) = self.notice.clone() {
                    ui.separator();
                    ui.label(RichText::new(n).color(Color32::from_rgb(220, 150, 0)));
                    if ui.small_button("x").clicked() {
                        self.notice = None;
                    }
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                Tab::Run => self.run_tab(ui, &snap),
                Tab::Settings => self.settings_tab(ui),
            });
        });
    }
}

// ---------------------------------------------------------------- Run tab

fn color(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

fn kind_name(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::StreamAtem => "Stream ATEM",
        DeviceKind::ScreensAtem => "Screens ATEM",
        DeviceKind::VideoHub => "VideoHub",
        DeviceKind::WebPresenter => "Web Presenter",
        DeviceKind::YouTube => "YouTube",
        DeviceKind::DeckLink => "DeckLink",
        DeviceKind::Ndi => "NDI",
        DeviceKind::StreamDeck => "Stream Deck",
    }
}

const ALL_KINDS: [DeviceKind; 8] = [
    DeviceKind::StreamAtem,
    DeviceKind::ScreensAtem,
    DeviceKind::VideoHub,
    DeviceKind::WebPresenter,
    DeviceKind::YouTube,
    DeviceKind::DeckLink,
    DeviceKind::Ndi,
    DeviceKind::StreamDeck,
];

fn dest_name(d: Destination) -> &'static str {
    match d {
        Destination::HallAndOnline => "Hall and online",
        Destination::HallOnly => "Hall only",
        Destination::OnlineOnly => "Online only",
        Destination::AudioOnly => "Audio only (online)",
    }
}

const DESTINATIONS: [Destination; 4] =
    [Destination::HallAndOnline, Destination::HallOnly, Destination::OnlineOnly, Destination::AudioOnly];

impl StreamItApp {
    fn run_tab(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let blink = ((ui.input(|i| i.time) * 4.0) as u64).is_multiple_of(2);

        // Stage, warning, flash.
        ui.horizontal(|ui| {
            let phase = layout::appearance(7, snap, blink);
            ui.label(
                RichText::new(format!(" {} ", phase.text.replace('\n', " ")))
                    .size(22.0)
                    .strong()
                    .color(color(phase.fg))
                    .background_color(color(phase.bg)),
            );
            ui.label(format!("{:?}", snap.stage));
            if snap.mics_ducked {
                ui.label(RichText::new("Mics ducked").color(Color32::from_rgb(220, 150, 0)).strong());
            }
        });
        if let Some(w) = &snap.last_warning {
            ui.label(RichText::new(format!("Warning: {w}")).color(Color32::from_rgb(220, 150, 0)).strong());
        }
        if let Some(i) = snap.flash {
            ui.label(RichText::new(format!("Cue {} ignored: something else is running", i + 1)).color(Color32::RED));
        }
        if let Some(k) = snap.flash_key {
            ui.label(RichText::new(format!("{k:?} ignored right now")).color(Color32::RED));
        }
        if !snap.offers.is_empty() {
            ui.label(format!("Offered: {:?}", snap.offers));
        }

        // Device status row.
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            for k in ALL_KINDS {
                let (bg, fg, why) = match snap.device_status.get(&k) {
                    Some(DeviceHealth::Ok) => (layout::GREEN, layout::WHITE, "OK".to_string()),
                    Some(DeviceHealth::Warning(w)) => (layout::AMBER, layout::BLACK, w.clone()),
                    Some(DeviceHealth::Down(e)) => (layout::RED, layout::WHITE, e.clone()),
                    None => (layout::GREY, layout::WHITE, "unknown".to_string()),
                };
                ui.label(RichText::new(format!(" {} ", kind_name(k))).color(color(fg)).background_color(color(bg)))
                    .on_hover_text(why);
            }
        });

        ui.separator();
        self.broadcast_section(ui, snap);
        ui.separator();

        // Every Stream Deck key, as the deck shows it; presses send what the deck would.
        ui.heading("Stream Deck");
        egui::Grid::new("deck").spacing(vec2(4.0, 4.0)).show(ui, |ui| {
            for key in 0..KEYS {
                let look = layout::appearance(key, snap, blink);
                let cmd = layout::command_for(key, snap);
                let text = RichText::new(look.text.clone()).color(color(look.fg)).size(12.0);
                let b = egui::Button::new(text).fill(color(look.bg)).min_size(vec2(112.0, 58.0));
                if ui.add_enabled(cmd.is_some(), b).clicked() {
                    // Decide from the latest snapshot, as the deck does.
                    let latest = layout::command_for(key, &self.snap.borrow());
                    if let Some(c) = latest {
                        self.send(c);
                    }
                }
                if (key + 1) % layout::COLS == 0 {
                    ui.end_row();
                }
            }
        });

        ui.separator();
        self.cue_editor(ui, snap);
    }

    fn broadcast_section(&mut self, ui: &mut Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label("Broadcast:");
            let selected = snap.broadcast.as_ref().map(|b| b.title.clone()).unwrap_or_else(|| "(none)".into());
            let mut choice: Option<String> = None;
            egui::ComboBox::from_id_salt("broadcast").selected_text(selected).width(360.0).show_ui(ui, |ui| {
                for b in &self.broadcasts {
                    let label = match &b.scheduled_start {
                        Some(s) => format!("{}  ({s})", b.title),
                        None => b.title.clone(),
                    };
                    let on = snap.broadcast.as_ref().is_some_and(|x| x.id == b.id);
                    if ui.selectable_label(on, label).clicked() {
                        choice = Some(b.id.clone());
                    }
                }
            });
            if let Some(id) = choice {
                self.send(Command::SelectBroadcast(id));
            }
            let busy = self.broadcasts_job.running;
            if ui.add_enabled(!busy, egui::Button::new("Refresh")).clicked() {
                self.send(Command::Refresh);
                let ctx = ui.ctx().clone();
                self.load_broadcasts(Some(&ctx));
            }
            if busy {
                ui.spinner();
            }
        });
        if let Some(e) = &self.broadcasts_error {
            ui.label(RichText::new(format!("Could not list broadcasts: {e}")).color(Color32::from_rgb(220, 150, 0)));
        }
        if let Some(c) = snap.check {
            let rows = [
                ("Bound to our stream key", c.bound_to_ours, Fix::Bind),
                ("Auto Start off", !c.auto_start, Fix::AutoStartOff),
                ("Auto Stop on (safety net)", c.auto_stop, Fix::AutoStopOn),
                ("Monitor stream on", c.monitor_stream, Fix::MonitorOn),
            ];
            egui::Grid::new("check").show(ui, |ui| {
                for (what, ok, fix) in rows {
                    let (mark, col) = if ok { ("OK", Color32::from_rgb(0, 160, 70)) } else { ("NO", Color32::RED) };
                    ui.label(RichText::new(mark).color(col).strong());
                    ui.label(what);
                    if !ok && ui.button("Fix").clicked() {
                        self.send(Command::FixBroadcast(fix));
                    }
                    ui.end_row();
                }
            });
        } else if snap.broadcast.is_some() {
            ui.label("Checking the broadcast...");
        }
    }

    fn cue_editor(&mut self, ui: &mut Ui, snap: &Snapshot) {
        ui.heading(format!("Cue list ({}/{MAX_CUES})", self.cues.len()));
        let locked = snap.playing.is_some();
        if locked {
            ui.label(RichText::new("The cue list is locked while a video plays.").color(Color32::from_rgb(220, 150, 0)));
        }
        let mut changed = false;
        ui.add_enabled_ui(!locked, |ui| {
            if ui.add_enabled(self.cues.len() < MAX_CUES, egui::Button::new("Add videos...")).clicked()
                && let Some(files) = rfd::FileDialog::new()
                    .add_filter("Video", &["mp4", "mov", "m4v", "mkv", "mxf", "avi", "ts"])
                    .add_filter("All files", &["*"])
                    .pick_files()
            {
                for f in files.into_iter().take(MAX_CUES - self.cues.len()) {
                    self.cues.push(new_cue(&f));
                    changed = true;
                }
            }
            let mut action: Option<(usize, i8)> = None; // -1 up, 1 down, 0 remove
            egui::Grid::new("cues").striped(true).show(ui, |ui| {
                ui.label("#");
                ui.label("Name");
                ui.label("Destination");
                ui.label("Level");
                ui.label("Return to input");
                ui.label("State");
                ui.label("");
                ui.end_row();
                let n = self.cues.len();
                for (i, cue) in self.cues.iter_mut().enumerate() {
                    ui.label(format!("{}", i + 1));
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut cue.name).desired_width(200.0))
                        .on_hover_text(cue.path.display().to_string())
                        .changed();
                    egui::ComboBox::from_id_salt(("dest", i)).selected_text(dest_name(cue.destination)).show_ui(
                        ui,
                        |ui| {
                            for d in DESTINATIONS {
                                changed |= ui.selectable_value(&mut cue.destination, d, dest_name(d)).changed();
                            }
                        },
                    );
                    changed |= ui
                        .add(egui::DragValue::new(&mut cue.gain_db).range(-60.0..=12.0).speed(0.1).suffix(" dB"))
                        .changed();
                    ui.horizontal(|ui| {
                        let mut on = cue.return_input.is_some();
                        if ui.checkbox(&mut on, "").changed() {
                            cue.return_input = on.then_some(1);
                            changed = true;
                        }
                        match &mut cue.return_input {
                            Some(input) => {
                                changed |= ui.add(egui::DragValue::new(input).range(1..=40)).changed();
                            }
                            None => {
                                ui.label("Preview");
                            }
                        }
                    });
                    let state = snap.cues.get(i).filter(|(c, _)| c == cue).map(|(_, s)| s.clone());
                    ui.label(match state {
                        Some(CueState::Cued) => "cued".to_string(),
                        Some(CueState::Playing) => "PLAYING".to_string(),
                        Some(CueState::Played) => "played".to_string(),
                        Some(CueState::Error(e)) => format!("error: {e}"),
                        None => "...".to_string(),
                    });
                    ui.horizontal(|ui| {
                        if ui.add_enabled(i > 0, egui::Button::new("Up")).clicked() {
                            action = Some((i, -1));
                        }
                        if ui.add_enabled(i + 1 < n, egui::Button::new("Down")).clicked() {
                            action = Some((i, 1));
                        }
                        if ui.button("Remove").clicked() {
                            action = Some((i, 0));
                        }
                    });
                    ui.end_row();
                }
            });
            match action {
                Some((i, -1)) => self.cues.swap(i - 1, i),
                Some((i, 1)) => self.cues.swap(i, i + 1),
                Some((i, _)) => {
                    self.cues.remove(i);
                }
                None => {}
            }
            changed |= action.is_some();
        });
        if changed {
            self.cues_dirty_at = Some(Instant::now());
        }
    }
}

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
            if ui.add_enabled(!busy, egui::Button::new("Scan")).clicked() {
                let ctx = ui.ctx().clone();
                self.scan(Some(&ctx));
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
            ui.add(egui::TextEdit::singleline(&mut self.draft.youtube.client_secret).password(true).desired_width(420.0));
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
        ui.horizontal(|ui| {
            let busy = self.signin_job.running;
            if ui.add_enabled(have_client && !busy, egui::Button::new("Sign in")).clicked() {
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
            if ui.add_enabled(have_client && !busy, egui::Button::new("Load stream keys")).clicked() {
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
            ui.label("Duck level");
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
                if ui.button("Browse...").clicked()
                    && let Some(f) = rfd::FileDialog::new().add_filter("Video", &["mp4", "mov", "m4v", "mkv"]).pick_file()
                {
                    c.playout.countdown_file = f;
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
    }
}
