//! Screen 5: the Videos tab. The countdown video, the roll-in videos in playing order
//! (each on a Stream Deck key), and the settings of the selected one.
use super::StreamItApp;
use crate::theme::{self, Kind};
use crate::view::{self, ClipLength, CueRow};
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Stroke, Ui, UiBuilder, vec2};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use streamit_core::config::Config;
use streamit_core::engine::Snapshot;
use streamit_core::rollin::{Destination, MAX_CUES};
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

/// A rounded box with a time or a name in it, standing in for a video's picture.
fn thumb(ui: &mut Ui, size: egui::Vec2, text: &str, font: egui::FontId, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 5.0, theme::SCREEN);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, font, color);
}

impl StreamItApp {
    /// `running`: in simulation, the config it opened with (its countdown video is the
    /// one it plays).
    pub(super) fn videos_tab(&mut self, ui: &mut Ui, snap: &Snapshot, config: &Config, running: Option<&Config>) {
        let countdown = running.unwrap_or(config).playout.countdown_file.clone();
        let note = view::sim_next_time_note(&countdown, &config.playout.countdown_file);
        let lengths = {
            let paths = self.cues.iter().map(|c| c.path.as_path()).chain([countdown.as_path()]);
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
                app.countdown_panel(ui, &countdown, note, &lengths);
                app.list_panel(ui, snap, locked, &lengths);
            },
            |app, ui| app.selected_panel(ui, config, locked),
        );
    }

    /// `note`: the saved countdown video differs from the one the simulation plays.
    fn countdown_panel(
        &mut self,
        ui: &mut Ui,
        file: &Path,
        note: Option<&str>,
        lengths: &HashMap<PathBuf, ClipLength>,
    ) {
        let length = view::length_text(lengths.get(file).copied());
        theme::panel().inner_margin(20).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                thumb(ui, vec2(160.0, 90.0), &length, theme::heading_font(22.0), theme::SCREEN_TEXT);
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 150.0).max(200.0));
                    theme::lbl(ui, "Countdown video");
                    ui.add(
                        egui::Label::new(RichText::new(view::file_name(file)).font(theme::body_bold(19.0))).truncate(),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{length} · plays to the hall before the service. Online sees the “starting soon” still. \
                             When it ends, the service goes live."
                        ))
                        .size(15.0)
                        .color(theme::SOFT),
                    );
                    if let Some(n) = note {
                        ui.label(
                            RichText::new(format!("The simulation plays this one; the new choice {n}."))
                                .size(14.0)
                                .color(theme::AMBER_SOFT),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if theme::button_enabled(ui, !self.countdown_pick_job.running, "Change…", Kind::Plain).clicked() {
                        self.countdown_pick_job.start(&self.rt, &self.ctx, async {
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

    fn list_panel(&mut self, ui: &mut Ui, snap: &Snapshot, locked: bool, lengths: &HashMap<PathBuf, ClipLength>) {
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
            for (i, row) in rows.iter().enumerate() {
                let state = snap.cues.get(i).filter(|(c, _)| *c == self.cues[i]).map(|(_, s)| s);
                self.video_row(ui, row, i == self.selected, i + 1 < n, locked, state, &mut action, &mut select);
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
            });
        });
        if changed {
            self.cues_dirty_at = Some(Instant::now());
        }
    }
}
