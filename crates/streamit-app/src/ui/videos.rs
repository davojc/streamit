//! Screen 5: the Videos tab. The opening and closing countdowns (each can be turned off),
//! the roll-in videos in playing order (each on a Stream Deck key), and the settings of
//! the selected one.
use super::StreamItApp;
use crate::theme::{self, Kind};
use crate::view::{self, ClipLength, CueRow};
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Stroke, Ui, UiBuilder, vec2};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use streamit_core::config::Config;
use streamit_core::engine::{Command, Snapshot};
use streamit_core::rollin::{Destination, MAX_CUES, TestTo};
use tokio::runtime::Handle;

const ROW_H: f32 = 84.0;
const ASIDE_W: f32 = 440.0;
const DESTINATIONS: [Destination; 4] =
    [Destination::HallAndOnline, Destination::HallOnly, Destination::OnlineOnly, Destination::AudioOnly];

/// Clip lengths, read once per file on a background thread (never on the UI thread) and
/// remembered by path. A length shows as "–:––" until its file has been read, then as
/// its length, "file missing" or "length unknown" (never the simulation's 30 s).
#[derive(Default)]
pub(super) struct Lengths {
    known: Arc<Mutex<HashMap<PathBuf, ClipLength>>>,
    asked: HashSet<PathBuf>,
}

impl Lengths {
    /// Starts reading any of `paths` not yet asked for, and returns what is known so far.
    pub(super) fn ask<'a>(
        &mut self,
        rt: &Handle,
        ctx: &egui::Context,
        paths: impl IntoIterator<Item = &'a Path>,
    ) -> HashMap<PathBuf, ClipLength> {
        for p in paths {
            if self.asked.insert(p.to_path_buf()) {
                let (p, known, ctx) = (p.to_path_buf(), self.known.clone(), ctx.clone());
                rt.spawn_blocking(move || {
                    let d = view::read_clip(&p);
                    known.lock().unwrap_or_else(|e| e.into_inner()).insert(p, d);
                    ctx.request_repaint();
                });
            }
        }
        self.known.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[derive(Clone, Copy)]
enum Action {
    Up(usize),
    Down(usize),
    Remove(usize),
    Test(usize),
    StopTest,
}

/// A row's Test button: this row is under test (with its time left), or why it cannot
/// start one (`None`: it can).
enum TestButton {
    Testing(Option<String>),
    Test(Option<String>),
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Up,
    Down,
    Cross,
}

/// A 36 px square icon button, drawn by hand (the bundled fonts have no arrows).
fn icon_button(ui: &mut Ui, icon: Icon, enabled: bool, label: &str) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, r) = ui.allocate_exact_size(vec2(36.0, 36.0), sense);
    let hot = enabled && r.hovered();
    let p = ui.painter();
    p.rect(
        rect,
        7.0,
        if hot { theme::BORDER } else { theme::BUTTON },
        Stroke::new(1.0, theme::BUTTON_BORDER),
        egui::StrokeKind::Inside,
    );
    let color = if enabled { theme::SOFT } else { Color32::from_rgb(0x4A, 0x52, 0x5E) };
    let stroke = Stroke::new(2.0, color);
    let c = rect.center();
    match icon {
        Icon::Up => {
            p.line_segment([c + vec2(-4.0, 2.0), c + vec2(0.0, -2.0)], stroke);
            p.line_segment([c + vec2(0.0, -2.0), c + vec2(4.0, 2.0)], stroke);
        }
        Icon::Down => {
            p.line_segment([c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0)], stroke);
            p.line_segment([c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)], stroke);
        }
        Icon::Cross => {
            p.line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], stroke);
            p.line_segment([c + vec2(4.0, -4.0), c + vec2(-4.0, 4.0)], stroke);
        }
    }
    r.on_hover_text(label)
}

/// A small outlined tag, sized to its text so it stays centred in a tall row.
fn chip(ui: &mut Ui, text: &str, color: Color32, border: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_uppercase(), theme::label_font(13.0), color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(18.0, 8.0), Sense::hover());
    ui.painter().rect(rect, 5.0, Color32::TRANSPARENT, Stroke::new(1.0, border), egui::StrokeKind::Inside);
    ui.painter().galley(rect.min + vec2(9.0, 4.0), galley, color);
}

/// A settings change, saved at once.
type Change = Box<dyn Fn(&mut Config)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Countdown {
    Opening,
    Closing,
}

/// What a countdown does, under its name. `length`: its video's length ("4:30").
pub(super) fn countdown_text(which: Countdown, c: &Config, length: &str) -> String {
    let p = &c.playout;
    match which {
        Countdown::Opening if !p.countdown_on => {
            "Off: there is no countdown; Start service now starts the service.".into()
        }
        Countdown::Opening if p.countdown_to_stream => {
            format!("{length} · plays to the hall and online before the service. When it ends, the service goes live.")
        }
        Countdown::Opening => format!(
            "{length} · plays to the hall before the service; online sees the “starting soon” still. When it ends, \
             the service goes live."
        ),
        Countdown::Closing if !p.closing_plays() => "Off: End Service goes straight to the closing still.".into(),
        Countdown::Closing => format!(
            "{length} · plays at End Service, before the closing still: {}. Pressing End Service again skips it.",
            view::closing_where(c).to_lowercase()
        ),
    }
}

/// The name a countdown's test play goes by.
pub(super) fn countdown_name(which: Countdown) -> &'static str {
    match which {
        Countdown::Opening => "Opening countdown",
        Countdown::Closing => "Closing countdown",
    }
}

/// Where a countdown is tested until changed: where it plays in the service.
pub(super) fn countdown_plays_to(which: Countdown, c: &Config) -> TestTo {
    let p = &c.playout;
    match which {
        Countdown::Opening => TestTo { hall: true, online: p.countdown_to_stream },
        Countdown::Closing => TestTo { hall: p.closing_to_hall, online: p.closing_to_stream },
    }
}

/// A rounded box with a time or a name in it, standing in for a video's picture.
fn thumb(ui: &mut Ui, size: egui::Vec2, text: &str, font: egui::FontId, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 5.0, theme::SCREEN);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, font, color);
}

impl StreamItApp {
    /// `running`: in simulation, the config it opened with (its countdown videos are the
    /// ones it plays).
    pub(super) fn videos_tab(&mut self, ui: &mut Ui, snap: &Snapshot, config: &Config, running: Option<&Config>) {
        let engine = running.unwrap_or(config);
        let lengths = {
            let (opening, closing) = (&engine.playout.countdown_file, &engine.playout.closing_file);
            let paths = self.cues.iter().map(|c| c.path.as_path()).chain([opening.as_path(), closing.as_path()]);
            self.lengths.ask(&self.rt, &self.ctx, paths)
        };
        if !self.cues.is_empty() {
            self.selected = self.selected.min(self.cues.len() - 1);
        }
        let locked = snap.playing.is_some();
        self.columns(
            ui,
            ASIDE_W,
            |app, ui| {
                app.countdown_panel(ui, snap, Countdown::Opening, config, engine, &lengths);
                app.countdown_panel(ui, snap, Countdown::Closing, config, engine, &lengths);
                app.list_panel(ui, snap, engine, locked, &lengths);
            },
            |app, ui| {
                if snap.test.is_some() {
                    app.test_panel(ui, snap, engine);
                }
                app.selected_panel(ui, config, locked)
            },
        );
    }

    /// The opening or closing countdown: its video, whether it is used and where it plays.
    /// The switches save straight away. `engine`: the config the engine runs with (in
    /// simulation, the one it opened with, whose video it plays).
    fn countdown_panel(
        &mut self,
        ui: &mut Ui,
        snap: &Snapshot,
        which: Countdown,
        config: &Config,
        engine: &Config,
        lengths: &HashMap<PathBuf, ClipLength>,
    ) {
        let p = &config.playout;
        let (title, file, saved, on) = match which {
            Countdown::Opening => {
                ("Opening countdown", &engine.playout.countdown_file, &p.countdown_file, p.countdown_on)
            }
            Countdown::Closing => ("Closing countdown", &engine.playout.closing_file, &p.closing_file, p.closing_on),
        };
        let note = view::sim_next_time_note(file, saved);
        let length = view::length_text(lengths.get(file).copied());
        let about = countdown_text(which, config, &length);
        theme::panel().inner_margin(20).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                let shown = if on { theme::SCREEN_TEXT } else { theme::DIM };
                thumb(ui, vec2(160.0, 90.0), if on { &length } else { "Off" }, theme::heading_font(22.0), shown);
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 150.0).max(200.0));
                    theme::lbl(ui, title);
                    ui.add(
                        egui::Label::new(RichText::new(view::file_name(file)).font(theme::body_bold(19.0))).truncate(),
                    );
                    ui.label(RichText::new(about).size(15.0).color(theme::SOFT));
                    if let Some(n) = note {
                        ui.label(
                            RichText::new(format!("The simulation plays this one; the new choice {n}."))
                                .size(14.0)
                                .color(theme::AMBER_SOFT),
                        );
                    }
                    ui.horizontal_wrapped(|ui| self.countdown_switches(ui, which, config));
                    ui.horizontal(|ui| self.countdown_level(ui, which, config));
                    ui.horizontal_wrapped(|ui| self.countdown_test(ui, snap, which, config, engine));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let job = match which {
                        Countdown::Opening => &mut self.countdown_pick_job,
                        Countdown::Closing => &mut self.closing_pick_job,
                    };
                    if theme::button_enabled(ui, !job.running, "Change…", Kind::Plain).clicked() {
                        job.start(&self.rt, &self.ctx, async {
                            rfd::AsyncFileDialog::new()
                                .add_filter("Video", &["mp4", "mov", "m4v", "mkv"])
                                .pick_file()
                                .await
                                .map(|f| f.path().to_path_buf())
                        });
                    }
                });
            });
        });
    }

    /// "Use it" and where it plays; each change is saved at once.
    fn countdown_switches(&mut self, ui: &mut Ui, which: Countdown, config: &Config) {
        let p = &config.playout;
        let mut changed: Option<Change> = None;
        match which {
            Countdown::Opening => {
                let (mut on, mut online) = (p.countdown_on, p.countdown_to_stream);
                if ui
                    .checkbox(&mut on, "Use it")
                    .on_hover_text("Off: the service starts with Start service now.")
                    .changed()
                {
                    changed = Some(Box::new(move |c| c.playout.countdown_on = on));
                }
                if ui
                    .add_enabled(on, egui::Checkbox::new(&mut online, "Goes online too"))
                    .on_hover_text(
                        "Off: online sees the opening still, in silence, until the service starts. On: the countdown \
                         is routed to Stream ATEM input 8 by the VideoHub, as a video is, with its sound.",
                    )
                    .changed()
                {
                    changed = Some(Box::new(move |c| c.playout.countdown_to_stream = online));
                }
            }
            Countdown::Closing => {
                let (mut on, mut hall, mut online) = (p.closing_on, p.closing_to_hall, p.closing_to_stream);
                if ui
                    .checkbox(&mut on, "Use it")
                    .on_hover_text("Off: End Service goes straight to the closing still.")
                    .changed()
                {
                    changed = Some(Box::new(move |c| c.playout.closing_on = on));
                }
                if ui
                    .add_enabled(on, egui::Checkbox::new(&mut hall, "Hall screens"))
                    .on_hover_text(
                        "The stream audio fades down as it starts, so its sound from the hall speakers does not go                          online.",
                    )
                    .changed()
                {
                    changed = Some(Box::new(move |c| c.playout.closing_to_hall = hall));
                }
                if ui
                    .add_enabled(on, egui::Checkbox::new(&mut online, "Online"))
                    .on_hover_text(
                        "The stream audio fades down, then the VideoHub routes the video to Stream ATEM input 8, \
                         with its sound.",
                    )
                    .changed()
                {
                    changed = Some(Box::new(move |c| c.playout.closing_to_stream = online));
                }
            }
        }
        if let Some(change) = changed {
            self.save_setup(change);
        }
    }

    /// The countdown's level, as a video's: a trim the player applies. Saved at once.
    fn countdown_level(&mut self, ui: &mut Ui, which: Countdown, config: &Config) {
        let p = &config.playout;
        let mut db = match which {
            Countdown::Opening => p.countdown_gain_db,
            Countdown::Closing => p.closing_gain_db,
        };
        ui.label(RichText::new("Level").size(14.0).color(theme::MUTED));
        let changed = ui
            .scope(|ui| {
                ui.visuals_mut().selection.bg_fill = theme::BLUE;
                ui.spacing_mut().slider_width = 220.0;
                ui.add(egui::Slider::new(&mut db, -20.0..=6.0).step_by(1.0).show_value(false).trailing_fill(true))
                    .on_hover_text("Turn down a countdown that came in too loud.")
                    .changed()
            })
            .inner;
        ui.label(RichText::new(view::fmt_db(db)).font(theme::mono(15.0)));
        if changed {
            self.save_setup(move |c| match which {
                Countdown::Opening => c.playout.countdown_gain_db = db,
                Countdown::Closing => c.playout.closing_gain_db = db,
            });
        }
    }

    /// "Test on" the hall screens and online, and Test (or Stop while it plays).
    fn countdown_test(&mut self, ui: &mut Ui, snap: &Snapshot, which: Countdown, config: &Config, engine: &Config) {
        let k = which as usize;
        let mut to = self.countdown_test_to[k].unwrap_or_else(|| countdown_plays_to(which, config));
        let testing = snap.test.as_ref().is_some_and(|t| t.cue.name == countdown_name(which));
        ui.label(RichText::new("Test on").size(14.0).color(theme::MUTED));
        let mut changed = ui.add_enabled(!testing, egui::Checkbox::new(&mut to.hall, "Hall screens")).changed();
        changed |= ui
            .add_enabled(!testing, egui::Checkbox::new(&mut to.online, "Online"))
            .on_hover_text(
                "Routed to Stream ATEM input 8: see it on the multiview and hear it on input 8's meter. It is \
                 never put on the stream.",
            )
            .changed();
        if changed {
            self.countdown_test_to[k] = Some(to);
        }
        if testing {
            if theme::sized_button(ui, "Stop", Kind::WarnOutline, 34.0, 14.0, true).clicked() {
                self.send(Command::StopVideo);
            }
            let text = view::test_left(snap).map_or("Testing".into(), |l| format!("Testing · {l} left"));
            chip(ui, &text, theme::AMBER_SOFT, theme::AMBER_BORDER);
            return;
        }
        let block = view::test_block(snap, engine, to);
        let r = theme::sized_button(ui, "Test", Kind::Plain, 34.0, 14.0, block.is_none());
        let r = match &block {
            Some(why) => r.on_disabled_hover_text(why.as_str()),
            None => r.on_hover_text("Play it now where ticked, to check it before the service"),
        };
        if r.clicked() {
            self.send(Command::TestCountdown { closing: which == Countdown::Closing, to });
        }
    }

    fn list_panel(
        &mut self,
        ui: &mut Ui,
        snap: &Snapshot,
        engine: &Config,
        locked: bool,
        lengths: &HashMap<PathBuf, ClipLength>,
    ) {
        let mut action: Option<Action> = None;
        let mut select: Option<usize> = None;
        theme::panel().inner_margin(20).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Videos for this service").font(theme::heading_font(30.0)));
                        if self.cues_applying {
                            chip(ui, "Applying…", theme::AMBER_SOFT, theme::AMBER_BORDER);
                        }
                        if self.cues_unsaved {
                            chip(ui, "Not saved to disk", theme::RED_SOFT, theme::RED_BORDER);
                        }
                    });
                    ui.label(
                        RichText::new(format!(
                            "In playing order. Each gets a Stream Deck key, left to right. {} of {MAX_CUES}.",
                            self.cues.len()
                        ))
                        .size(15.0)
                        .color(theme::SOFT),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let can_add = !locked && self.cues.len() < MAX_CUES && !self.add_files_job.running;
                    if theme::button_enabled(ui, can_add, "Add videos…", Kind::Primary).clicked() {
                        self.add_files_job.start(&self.rt, &self.ctx, async {
                            rfd::AsyncFileDialog::new()
                                .add_filter("Video", &["mp4", "mov", "m4v", "mkv", "mxf", "avi", "ts"])
                                .add_filter("All files", &["*"])
                                .pick_files()
                                .await
                                .map(|v| v.iter().map(|f| f.path().to_path_buf()).collect())
                                .unwrap_or_default()
                        });
                    }
                });
            });
            if locked {
                ui.label(RichText::new("Others wait until this one ends").color(theme::AMBER_SOFT).size(15.0));
            }
            if self.cues.is_empty() {
                ui.add_space(12.0);
                ui.label(
                    RichText::new("No videos yet. Add the videos to roll in during the service.")
                        .size(16.0)
                        .color(theme::SOFT),
                );
            }
            let rows = view::cue_rows(&self.cues, lengths);
            let n = rows.len();
            let block = view::test_block(snap, engine, TestTo::BOTH);
            for (i, row) in rows.iter().enumerate() {
                let state = snap.cues.get(i).filter(|(c, _)| *c == self.cues[i]).map(|(_, s)| s);
                let test = if snap.test.as_ref().is_some_and(|t| t.cue == self.cues[i]) {
                    TestButton::Testing(view::test_left(snap))
                } else {
                    TestButton::Test(block.clone())
                };
                self.video_row(ui, row, i == self.selected, i + 1 < n, locked, state, test, &mut action, &mut select);
            }
        });
        if let Some(i) = select {
            self.selected = i;
        }
        let mut changed = false;
        match action {
            Some(Action::Up(i)) if i > 0 => {
                self.cues.swap(i - 1, i);
                self.selected = i - 1;
                changed = true;
            }
            Some(Action::Down(i)) if i + 1 < self.cues.len() => {
                self.cues.swap(i, i + 1);
                self.selected = i + 1;
                changed = true;
            }
            Some(Action::Remove(i)) if i < self.cues.len() => {
                self.cues.remove(i);
                self.selected = view::selection_after_remove(self.selected, i, self.cues.len());
                changed = true;
            }
            Some(Action::Test(i)) if i < self.cues.len() => self.send(Command::TestVideo(self.cues[i].clone())),
            Some(Action::StopTest) => self.send(Command::StopVideo),
            _ => {}
        }
        if changed {
            self.cues_dirty_at = Some(Instant::now());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn video_row(
        &self,
        ui: &mut Ui,
        row: &CueRow,
        selected: bool,
        has_next: bool,
        locked: bool,
        state: Option<&streamit_core::rollin::CueState>,
        test: TestButton,
        action: &mut Option<Action>,
        select: &mut Option<usize>,
    ) {
        let i = row.number - 1;
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
        // The whole row selects; the buttons laid over it still take their own presses.
        let click = ui.interact(rect, ui.scope_id().with(("video-row", i)), Sense::click());
        if click.clicked() {
            *select = Some(i);
        }
        let (fill, border) = if selected { (theme::BLUE_PANEL, theme::BLUE) } else { (theme::INSET, theme::BORDER) };
        ui.painter().rect(rect, 8.0, fill, Stroke::new(1.0, border), egui::StrokeKind::Inside);
        let mut inner = ui.new_child(
            UiBuilder::new().max_rect(rect.shrink2(vec2(14.0, 0.0))).layout(Layout::left_to_right(Align::Center)),
        );
        let ui = &mut inner;
        ui.spacing_mut().item_spacing.x = 14.0;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if icon_button(ui, Icon::Up, !locked && i > 0, "Move up").clicked() {
                *action = Some(Action::Up(i));
            }
            if icon_button(ui, Icon::Down, !locked && has_next, "Move down").clicked() {
                *action = Some(Action::Down(i));
            }
        });
        ui.allocate_ui_with_layout(vec2(44.0, 44.0), Layout::top_down(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new(row.number.to_string()).font(theme::mono_bold(20.0)));
            ui.label(RichText::new(format!("KEY {}", row.deck_key)).size(11.0).color(theme::MUTED));
        });
        thumb(ui, vec2(96.0, 54.0), &row.length_text, theme::mono(12.0), theme::DIM);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, Icon::Cross, !locked, &format!("Remove {}", row.name)).clicked() {
                *action = Some(Action::Remove(i));
            }
            match &test {
                TestButton::Testing(left) => {
                    if theme::sized_button(ui, "Stop", Kind::WarnOutline, 40.0, 15.0, true).clicked() {
                        *action = Some(Action::StopTest);
                    }
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let text = left.as_ref().map_or("Testing".into(), |l| format!("Testing · {l} left"));
                    chip(ui, &text, theme::AMBER_SOFT, theme::AMBER_BORDER);
                }
                TestButton::Test(block) => {
                    let r = theme::sized_button(ui, "Test", Kind::Plain, 40.0, 15.0, block.is_none());
                    let r = match block {
                        Some(why) => r.on_disabled_hover_text(why.as_str()),
                        None => r.on_hover_text("Play it in the hall and on Stream ATEM input 8, not on the stream"),
                    };
                    if r.clicked() {
                        *action = Some(Action::Test(i));
                    }
                }
            }
            ui.spacing_mut().item_spacing.x = 6.0;
            chip(ui, &row.level_text, theme::SOFT, theme::BUTTON_BORDER);
            let (c, b) = if selected { (theme::BLUE_SOFT, theme::BLUE) } else { (theme::SOFT, theme::BUTTON_BORDER) };
            chip(ui, &row.destination_text, c, b);
            if let Some(s) = state {
                use streamit_core::rollin::CueState;
                match s {
                    CueState::Playing => chip(ui, "Playing", theme::BLUE_SOFT, theme::BLUE),
                    CueState::Played => chip(ui, "Played", theme::MUTED, theme::BORDER),
                    CueState::Error(_) => chip(ui, "Error", theme::RED_SOFT, theme::RED_BORDER),
                    CueState::Cued => {}
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add_space(20.0);
                    ui.add(egui::Label::new(RichText::new(&row.name).font(theme::body_bold(17.0))).truncate());
                    ui.add(egui::Label::new(RichText::new(&row.file_name).size(13.0).color(theme::MUTED)).truncate());
                });
            });
        });
    }

    /// The test play running now: how far it is, where to watch it, and Stop.
    fn test_panel(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config) {
        let Some(t) = &snap.test else { return };
        let mut stop = false;
        theme::panel().fill(theme::AMBER_STRIPE).stroke(Stroke::new(1.0, theme::AMBER_BORDER)).inner_margin(22).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 10.0;
                let where_ = match (t.to.hall, t.to.online) {
                    (true, true) => "TEST PLAY · HALL AND INPUT 8, NOT ON THE STREAM",
                    (true, false) => "TEST PLAY · HALL ONLY",
                    _ => "TEST PLAY · INPUT 8 ONLY, NOT ON THE STREAM",
                };
                ui.label(
                    RichText::new(where_)
                        .font(theme::label_font(13.0))
                        .color(theme::AMBER_SOFT)
                        .extra_letter_spacing(1.5),
                );
                ui.add(egui::Label::new(RichText::new(&t.cue.name).font(theme::heading_font(30.0))).wrap());
                if let Some(left) = view::test_left(snap) {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(left).font(theme::mono_bold(40.0)).color(theme::AMBER));
                        ui.label(
                            RichText::new(format!("left of {}", view::fmt_mmss(t.duration)))
                                .size(15.0)
                                .color(theme::AMBER_SOFT),
                        );
                    });
                    let frac = (t.position.as_secs_f32() / t.duration.as_secs_f32()).clamp(0.0, 1.0);
                    theme::progress(ui, frac, theme::AMBER);
                } else {
                    ui.label(RichText::new("Loading…").size(15.0).color(theme::AMBER_SOFT));
                }
                ui.add_space(4.0);
                theme::lbl(ui, "Where to watch it");
                let place = |ui: &mut Ui, name: &str, what: String, after: &str| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.label(RichText::new(name).font(theme::heading_font(22.0)));
                        ui.add(egui::Label::new(RichText::new(what).size(14.0).color(theme::SOFT)).wrap());
                        ui.add(egui::Label::new(RichText::new(after).size(14.0).color(theme::MUTED)).wrap());
                    });
                };
                if t.to.hall {
                    place(
                        ui,
                        "Hall screens",
                        format!(
                            "Screens ATEM cut to input {}, as for a Hall video in the service.",
                            c.screens_atem.decklink_input
                        ),
                        "The hall goes back to what it showed when the test stops.",
                    );
                }
                if t.to.online {
                    place(
                        ui,
                        &format!("Stream ATEM input {}", c.stream_atem.decklink_input),
                        format!(
                            "Routed to the DeckLink but not put on program. See it in input {0}’s window on the \
                             multiview and its sound on input {0}’s audio meter.",
                            c.stream_atem.decklink_input
                        ),
                        "Its router route goes back when the test stops.",
                    );
                }
                ui.separator();
                if t.to.online {
                    let (tone, text) = if view::test_on_stream(snap, c) {
                        (theme::RED, "Input 8 is on the stream: take it off program".to_string())
                    } else {
                        (theme::GREEN, "Not on the stream: input 8 is not on program".to_string())
                    };
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 5.0, tone);
                        ui.label(RichText::new(text).size(15.0));
                    });
                }
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 5.0, theme::GREEN);
                    ui.label(
                        RichText::new(format!("Playing at the video’s level, {}", view::fmt_db(t.cue.gain_db)))
                            .size(15.0),
                    );
                });
                ui.add_space(4.0);
                let w = ui.available_width();
                ui.scope(|ui| {
                    ui.spacing_mut().button_padding.x = 0.0;
                    let r = ui.add_sized(
                        vec2(w, 52.0),
                        egui::Button::new(
                            RichText::new("STOP TEST")
                                .font(theme::heading_font(20.0))
                                .color(theme::AMBER_SOFT)
                                .extra_letter_spacing(1.0),
                        )
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(2.0, theme::AMBER))
                        .corner_radius(8),
                    );
                    stop = r.clicked();
                });
                ui.label(
                    RichText::new(
                        "Test play is off from the countdown until the service ends. \
                         It shows in the hall, so test before people arrive.",
                    )
                    .size(14.0)
                    .color(theme::MUTED),
                );
            },
        );
        if stop {
            self.send(Command::StopVideo);
        }
    }

    fn selected_panel(&mut self, ui: &mut Ui, config: &Config, locked: bool) {
        let mut changed = false;
        let sel = self.selected;
        theme::panel().inner_margin(22).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 20.0;
            let Some(cue) = self.cues.get_mut(sel) else {
                theme::lbl(ui, "Selected video");
                ui.label(RichText::new("Add a video to set where it plays and how loud.").color(theme::SOFT));
                return;
            };
            ui.add_enabled_ui(!locked, |ui| {
                ui.spacing_mut().item_spacing.y = 20.0;
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    theme::lbl(ui, &format!("Selected · video {}", sel + 1));
                    ui.add_space(6.0);
                    theme::lbl(ui, "Name on the Stream Deck key");
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut cue.name)
                                .desired_width(f32::INFINITY)
                                .margin(vec2(12.0, 10.0)),
                        )
                        .changed();
                });
                theme::screen_box(
                    ui,
                    180.0,
                    &format!("First frame of {}", view::file_name(&cue.path)),
                    egui::FontId::proportional(15.0),
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    theme::lbl(ui, "Where it plays");
                    let w = (ui.available_width() - 6.0) / 2.0;
                    egui::Grid::new("destinations").spacing(vec2(6.0, 6.0)).show(ui, |ui| {
                        for (k, d) in DESTINATIONS.into_iter().enumerate() {
                            let on = cue.destination == d;
                            let label = RichText::new(view::dest_short(d).to_uppercase())
                                .font(theme::heading_font(14.0))
                                .color(if on { Color32::WHITE } else { theme::MUTED })
                                .extra_letter_spacing(0.6);
                            let b = egui::Button::new(label)
                                .fill(if on { theme::BLUE_DARK } else { theme::INSET })
                                .stroke(Stroke::new(1.0, if on { theme::BLUE } else { theme::BUTTON_BORDER }))
                                .corner_radius(7)
                                .min_size(vec2(w, 40.0));
                            if ui.add(b).clicked() && !on {
                                cue.destination = d;
                                changed = true;
                            }
                            if k % 2 == 1 {
                                ui.end_row();
                            }
                        }
                    });
                    ui.label(RichText::new(view::destination_help(cue.destination)).size(14.0).color(theme::SOFT));
                });
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal(|ui| {
                        theme::lbl(ui, "Level");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(view::fmt_db(cue.gain_db)).font(theme::mono(15.0)));
                        });
                    });
                    ui.scope(|ui| {
                        ui.visuals_mut().selection.bg_fill = theme::BLUE;
                        ui.spacing_mut().slider_width = ui.available_width();
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut cue.gain_db, -20.0..=6.0)
                                    .step_by(1.0)
                                    .show_value(false)
                                    .trailing_fill(true),
                            )
                            .changed();
                    });
                    ui.label(RichText::new("Turn down a video that came in too loud.").size(14.0).color(theme::SOFT));
                });
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    theme::lbl(ui, "Afterwards, the stream goes back to");
                    let mut inputs = config.stream_atem.live_inputs.clone();
                    if let Some(r) = cue.return_input
                        && !inputs.contains(&r)
                    {
                        inputs.push(r);
                    }
                    let text = match cue.return_input {
                        None => "Whatever is on preview (usual)".to_string(),
                        Some(n) => format!("Input {n}"),
                    };
                    egui::ComboBox::from_id_salt("return-to").selected_text(text).width(ui.available_width()).show_ui(
                        ui,
                        |ui| {
                            changed |= ui
                                .selectable_value(&mut cue.return_input, None, "Whatever is on preview (usual)")
                                .changed();
                            for n in inputs {
                                changed |=
                                    ui.selectable_value(&mut cue.return_input, Some(n), format!("Input {n}")).changed();
                            }
                        },
                    );
                });
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    theme::lbl(ui, "Scene while it plays");
                    let none = "None: leave the scene as it is";
                    let name = |id: u32| {
                        streamit_core::scenes::find(&config.scenes, id)
                            .map_or("(a removed scene)".into(), |s| s.name.clone())
                    };
                    let text = cue.scene.map_or_else(|| none.to_string(), name);
                    egui::ComboBox::from_id_salt("video-scene")
                        .selected_text(text)
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            changed |= ui.selectable_value(&mut cue.scene, None, none).changed();
                            for s in &config.scenes {
                                changed |= ui.selectable_value(&mut cue.scene, Some(s.id), &s.name).changed();
                            }
                        });
                    let help = if config.scenes.is_empty() {
                        "Make scenes on the Scenes tab."
                    } else {
                        "When the video ends (or is stopped), StreamIt goes back to the scene before it. \
                         If it plays in the hall, the screens show the video; only the scene's lighting cue fires."
                    };
                    ui.label(RichText::new(help).size(14.0).color(theme::SOFT));
                });
            });
        });
        if changed {
            self.cues_dirty_at = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_countdown_says_what_it_does() {
        let mut c = Config::default();
        assert!(countdown_text(Countdown::Opening, &c, "4:30").starts_with("4:30 · plays to the hall before"));
        assert!(countdown_text(Countdown::Closing, &c, "1:00").starts_with("Off:"), "closing is off by default");
        c.playout.closing_on = true;
        c.playout.closing_to_stream = false;
        assert!(countdown_text(Countdown::Closing, &c, "1:00").contains("hall screens only"));
        c.playout.countdown_on = false;
        assert!(countdown_text(Countdown::Opening, &c, "4:30").starts_with("Off:"));
    }
}
