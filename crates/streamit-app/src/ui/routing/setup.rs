//! What the Routing tab sets up besides routes: each ATEM input's job (Go Live, Safe,
//! Media on the Stream ATEM; Videos on the Screens ATEM), the Default preset, the media
//! pool's stills, the hall's Go Live scene and the VideoHub's video source. Each change is
//! saved at once, like the presets. At start-up StreamIt offers to restore the Default
//! presets when the routing differs from them.
use super::super::StreamItApp;
use super::pool::{self, Thumb};
use crate::theme;
use eframe::egui::{self, Align, Align2, Color32, Layout, Rect, RichText, Sense, Stroke, Ui, vec2};
use streamit_core::config::Config;
use streamit_core::engine::{Command, Snapshot};
use streamit_core::golive::Stage;
use streamit_core::routing::Target;
use tracing::info;

/// Media pool slots shown on an ATEM page before it lists its own (most have 20 or more).
const POOL_SHOWN: usize = 16;
const POOL_COLS: usize = 8;
/// A slot's number line and name line, around its picture.
const POOL_SLOT_TEXT_H: f32 = 48.0;
/// Media players offered for the stills.
const MEDIA_PLAYERS: u8 = 4;

pub(in crate::ui) const GO_LIVE: Color32 = Color32::from_rgb(255, 154, 160);
pub(in crate::ui) const SAFE: Color32 = Color32::from_rgb(127, 240, 176);
pub(in crate::ui) const MEDIA: Color32 = Color32::from_rgb(212, 198, 255);
const OPENING: Color32 = Color32::from_rgb(143, 211, 255);
const CLOSING: Color32 = Color32::from_rgb(228, 168, 255);
pub(in crate::ui) const DEFAULT_STAR: Color32 = Color32::from_rgb(255, 210, 122);

/// The jobs of ATEM input `i` (an index) on `target`, as badge texts in order.
pub(in crate::ui) fn input_jobs(c: &Config, target: Target, i: usize) -> Vec<(&'static str, Color32)> {
    let n = i as u16 + 1;
    let mut jobs = Vec::new();
    match target {
        Target::StreamAtem => {
            if c.stream_atem.live_inputs.contains(&n) {
                jobs.push(("GO LIVE", GO_LIVE));
            }
            if c.stream_atem.default_live_input == n {
                jobs.push(("SAFE", SAFE));
            }
            if c.stream_atem.decklink_input == n {
                jobs.push(("MEDIA", MEDIA));
            }
        }
        Target::ScreensAtem if c.screens_atem.decklink_input == n => jobs.push(("VIDEOS", MEDIA)),
        _ => {}
    }
    jobs
}

/// `list` with `n` in it or out of it, in order.
pub(in crate::ui) fn toggled(list: &[u16], n: u16, on: bool) -> Vec<u16> {
    let mut v: Vec<u16> = list.iter().copied().filter(|&x| x != n).collect();
    if on {
        v.push(n);
        v.sort_unstable();
    }
    v
}

/// How many still slots to show: as many as the ATEM lists.
pub(in crate::ui) fn pool_slots(listed: usize) -> usize {
    if listed == 0 { POOL_SHOWN } else { listed }
}

/// The opening and closing stills marked on `target` (0-based slots), if any.
pub(in crate::ui) fn marked_stills(c: &Config, target: Target) -> (Option<u16>, Option<u16>) {
    match target {
        Target::StreamAtem => (Some(c.stream_atem.opening_still), Some(c.stream_atem.closing_still)),
        Target::ScreensAtem => (c.screens_atem.opening_still, c.screens_atem.closing_still),
        Target::MultiView => (None, None),
    }
}

/// A target that differs from its Default preset: its preset slot and the routes back.
pub(in crate::ui) struct OffDefault {
    pub target: Target,
    pub slot: u8,
    pub routes: Vec<(usize, u16)>,
}

/// The targets whose router-fed destinations differ from their Default preset, or `None`
/// while a destination the Default sets is not known yet (so the answer would be a guess).
pub(in crate::ui) fn off_default(snap: &Snapshot, c: &Config) -> Option<Vec<OffDefault>> {
    let mut off = Vec::new();
    for t in Target::ALL {
        let Some(slot) = c.routing(t).default_preset else { continue };
        let state = snap.routing.target(t);
        let Some(p) = state.preset(slot) else { continue };
        let unknown =
            (0..state.outputs.len()).any(|i| state.output(i) != 0 && p.input(i) != 0 && state.dest(i).is_none());
        if unknown {
            return None;
        }
        let routes = state.changes(p);
        if !routes.is_empty() {
            off.push(OffDefault { target: t, slot, routes });
        }
    }
    Some(off)
}

/// Where the start-up question about the Default presets is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::ui) enum DefaultsAsk {
    /// Waiting for the routing to be read.
    #[default]
    Waiting,
    Asking,
    /// Answered, or nothing to ask: not asked again this run.
    Done,
}

/// The line asking to restore the Default presets.
pub(in crate::ui) fn defaults_text(off: &[OffDefault]) -> String {
    let parts: Vec<String> = off
        .iter()
        .map(|o| {
            let dests: Vec<String> = o.routes.iter().map(|&(i, _)| o.target.dest_name(i)).collect();
            format!("{}: {}", o.target.name(), dests.join(", "))
        })
        .collect();
    format!("The routing is not as the Default presets have it ({}).", parts.join("; "))
}

impl StreamItApp {
    /// Badges for an ATEM input's jobs, along the bottom left of its tile.
    pub(super) fn job_badges(&self, ui: &Ui, rect: Rect, target: Target, i: usize) {
        let c = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mut x = rect.left() + 8.0;
        for (text, colour) in input_jobs(&c, target, i) {
            x += badge(ui, egui::pos2(x, rect.bottom() - 21.0), text, colour) + 4.0;
        }
    }

    /// The right-click menu of ATEM input `i`: its jobs.
    pub(super) fn input_menu(&mut self, ui: &mut Ui, target: Target, i: usize, busy: bool) {
        let n = i as u16 + 1;
        let c = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        ui.label(RichText::new(format!("{} input {n}", target.name()).to_uppercase()).size(11.0).color(theme::MUTED));
        match target {
            Target::StreamAtem => {
                let mut live = c.stream_atem.live_inputs.contains(&n);
                if ui
                    .checkbox(&mut live, "Go Live input")
                    .on_hover_text("Kept if it is on preview at Go Live; otherwise the stream cuts to the Safe input")
                    .changed()
                {
                    info!(input = n, live, "Stream ATEM: Go Live input changed");
                    self.save_setup(move |c| c.stream_atem.live_inputs = toggled(&c.stream_atem.live_inputs, n, live));
                }
                let safe = c.stream_atem.default_live_input == n;
                if ui
                    .add_enabled(!safe, egui::Button::new("Mark as Safe input"))
                    .on_hover_text(format!(
                        "The input StreamIt can always cut to. Now: input {}",
                        c.stream_atem.default_live_input
                    ))
                    .clicked()
                {
                    info!(input = n, "Stream ATEM: Safe input marked");
                    self.save_setup(move |c| c.stream_atem.default_live_input = n);
                    ui.close();
                }
                let media = c.stream_atem.decklink_input == n;
                let r = ui.add_enabled(!media && !busy, egui::Button::new("Mark as Media input"));
                let r = if busy {
                    r.on_disabled_hover_text("Not while a sequence runs")
                } else {
                    r.on_hover_text(format!(
                        "Videos and the countdown come in here. Now: input {}",
                        c.stream_atem.decklink_input
                    ))
                };
                if r.clicked() {
                    info!(input = n, "Stream ATEM: Media input marked");
                    self.save_setup(move |c| c.set_media_input(n));
                    ui.close();
                }
            }
            Target::ScreensAtem => {
                let videos = c.screens_atem.decklink_input == n;
                let r = ui.add_enabled(!videos && !busy, egui::Button::new("Mark as Videos input"));
                let r = if busy {
                    r.on_disabled_hover_text("Not while a sequence runs")
                } else {
                    r.on_hover_text(format!(
                        "Videos and the countdown show on the hall screens here. Now: input {}",
                        c.screens_atem.decklink_input
                    ))
                };
                if r.clicked() {
                    info!(input = n, "Screens ATEM: Videos input marked");
                    self.save_setup(move |c| c.screens_atem.decklink_input = n);
                    ui.close();
                }
            }
            Target::MultiView => {}
        }
    }

    /// The scene the hall goes to at Go Live, above the Screens ATEM's inputs.
    pub(super) fn go_live_scene(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let c = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        let now = c.screens_atem.t0_scene.filter(|&id| snap.scenes.iter().any(|s| s.id == id));
        let none = format!("No scene: input {}", c.screens_atem.t0_input);
        let shown = now.and_then(|id| snap.scenes.iter().find(|s| s.id == id)).map_or(none.clone(), |s| s.name.clone());
        let mut picked = now;
        ui.horizontal(|ui| {
            ui.label(RichText::new("At Go Live the hall goes to scene").size(14.0).color(theme::SOFT));
            egui::ComboBox::from_id_salt("t0-scene").selected_text(shown).width(220.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut picked, None, none);
                for s in &snap.scenes {
                    ui.selectable_value(&mut picked, Some(s.id), &s.name);
                }
            });
            if snap.scenes.is_empty() {
                ui.label(RichText::new("Make scenes on the Scenes tab").size(13.0).color(theme::MUTED));
            }
        });
        if picked != now {
            info!(scene = ?picked, "Go Live scene changed");
            self.save_setup(move |c| c.screens_atem.t0_scene = picked);
        }
    }

    /// The ATEM's media pool, every still slot it lists, with the opening and closing stills marked.
    pub(super) fn media_pool(&mut self, ui: &mut Ui, snap: &Snapshot, target: Target) {
        let c = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        let stills = &snap.routing.target(target).stills;
        let (opening, closing) = marked_stills(&c, target);
        let player = match target {
            Target::StreamAtem => c.stream_atem.media_player,
            _ => c.screens_atem.media_player,
        };
        self.pool_tick(&ui.ctx().clone(), target, stills, &snap.stage);
        let busy = pool::under_way(&snap.stage);
        theme::panel().inner_margin(20).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::lbl(ui, &format!("Media pool stills · slots 1–{}", pool_slots(stills.len())));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mark = |s: Option<u16>| s.map_or("not set".to_string(), |s| format!("slot {}", s + 1));
                    ui.label(RichText::new(format!("closing {}", mark(closing))).size(14.0).color(CLOSING));
                    ui.add_space(10.0);
                    ui.label(RichText::new(format!("opening {}", mark(opening))).size(14.0).color(OPENING));
                    ui.add_space(16.0);
                    let mut mp = player;
                    egui::ComboBox::from_id_salt(("media-player", target.index()))
                        .selected_text(format!("Media player {mp}"))
                        .show_ui(ui, |ui| {
                            for p in 1..=MEDIA_PLAYERS {
                                ui.selectable_value(&mut mp, p, format!("Media player {p}"));
                            }
                        });
                    ui.label(RichText::new("Stills play on").size(14.0).color(theme::SOFT));
                    if mp != player {
                        info!(target = target.name(), player = mp, "media player for stills changed");
                        self.save_setup(move |c| match target {
                            Target::StreamAtem => c.stream_atem.media_player = mp,
                            _ => c.screens_atem.media_player = mp,
                        });
                    }
                });
            });
            let note = match (target, stills.is_empty()) {
                (_, true) => "The ATEM has not listed its media pool yet.",
                (Target::StreamAtem, false) => {
                    "Right-click a slot to use it as the opening or closing still, put a picture in it, save it to a \
                     file or clear it."
                }
                _ => {
                    "Right-click a slot to mark it, put a picture in it, save it or clear it. The Screens ATEM's \
                     stills are not used by StreamIt yet."
                }
            };
            ui.label(RichText::new(note).size(13.0).color(theme::MUTED));
            ui.add_space(8.0);
            let gap = 8.0;
            let w = (ui.available_width() - gap * (POOL_COLS as f32 - 1.0)) / POOL_COLS as f32;
            let slots = pool_slots(stills.len());
            for row in 0..slots.div_ceil(POOL_COLS) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for col in 0..POOL_COLS {
                        let slot = (row * POOL_COLS + col) as u16;
                        if usize::from(slot) >= slots {
                            break;
                        }
                        let name = stills.get(usize::from(slot)).cloned().flatten();
                        let marks = (opening == Some(slot), closing == Some(slot));
                        self.pool_slot(ui, target, slot, name, w, marks, busy);
                    }
                });
                ui.add_space(gap);
            }
        });
    }

    /// `under_way`: a service is under way, so pictures cannot be put in or cleared.
    #[allow(clippy::too_many_arguments)]
    fn pool_slot(
        &mut self,
        ui: &mut Ui,
        target: Target,
        slot: u16,
        name: Option<String>,
        w: f32,
        marks: (bool, bool),
        under_way: bool,
    ) {
        let pic_h = ((w - 12.0) * 9.0 / 16.0).max(20.0);
        let (rect, r) = ui.allocate_exact_size(vec2(w, POOL_SLOT_TEXT_H + pic_h), Sense::click());
        let (is_open, is_close) = marks;
        let border = if is_open || is_close { theme::BLUE } else { theme::BORDER };
        let p = ui.painter();
        p.rect(rect, 6.0, theme::INSET, Stroke::new(1.0, border), egui::StrokeKind::Inside);
        p.text(
            rect.left_top() + vec2(8.0, 6.0),
            Align2::LEFT_TOP,
            (slot + 1).to_string(),
            theme::mono_bold(11.0),
            theme::DIM,
        );
        let mut x = rect.left() + 26.0;
        if is_open {
            x += badge(ui, egui::pos2(x, rect.top() + 5.0), "OPENING", OPENING) + 4.0;
        }
        if is_close {
            badge(ui, egui::pos2(x, rect.top() + 5.0), "CLOSING", CLOSING);
        }
        let (text, colour) = match &name {
            Some(n) if !n.is_empty() => (n.clone(), theme::TEXT),
            Some(_) => (format!("Still {}", slot + 1), theme::TEXT),
            None => ("Empty".to_string(), theme::DIM),
        };
        let galley = ui.painter().layout_no_wrap(text, theme::body_bold(13.0), colour);
        let pic = Rect::from_min_size(rect.left_top() + vec2(6.0, 24.0), vec2(w - 12.0, pic_h));
        ui.painter().with_clip_rect(rect.shrink(3.0)).galley(pic.left_bottom() + vec2(2.0, 4.0), galley, colour);
        let p = ui.painter();
        p.rect_filled(pic, 4.0, theme::SCREEN);
        let note = |p: &egui::Painter, s: &str, c: Color32| {
            p.text(pic.center(), Align2::CENTER_CENTER, s, egui::FontId::proportional(11.5), c);
        };
        let failed = match self.pool_thumb(target, slot, name.as_ref()) {
            Thumb::Picture(tex) => {
                let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                p.image(tex.id(), pic, uv, Color32::WHITE);
                None
            }
            Thumb::Working(what) => {
                note(p, what, theme::AMBER_SOFT);
                None
            }
            Thumb::Failed(why) => {
                note(p, "No picture", theme::DIM);
                Some(why.to_string())
            }
            Thumb::Waiting if under_way => None,
            Thumb::Waiting => {
                note(p, "Loading…", theme::DIM);
                None
            }
            Thumb::Empty => None,
        };
        let r = match failed {
            Some(why) => r.on_hover_text(format!("Could not read the picture: {why}")),
            None => r,
        };
        let used = name.is_some();
        let can_change = !under_way && !self.pool_busy();
        let still_name = name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| format!("Still {}", slot + 1));
        r.context_menu(|ui| {
            ui.label(RichText::new(format!("MEDIA SLOT {}", slot + 1)).size(11.0).color(theme::MUTED));
            let optional = target == Target::ScreensAtem;
            if ui.add_enabled(!is_open, egui::Button::new("Use as the opening still")).clicked() {
                info!(target = target.name(), slot, "opening still marked");
                self.save_setup(move |c| set_still(c, target, true, Some(slot)));
                ui.close();
            }
            if ui.add_enabled(!is_close, egui::Button::new("Use as the closing still")).clicked() {
                info!(target = target.name(), slot, "closing still marked");
                self.save_setup(move |c| set_still(c, target, false, Some(slot)));
                ui.close();
            }
            if optional && (is_open || is_close) && ui.button("Not used as a still").clicked() {
                self.save_setup(move |c| {
                    if is_open {
                        set_still(c, target, true, None);
                    }
                    if is_close {
                        set_still(c, target, false, None);
                    }
                });
                ui.close();
            }
            ui.separator();
            let why = if under_way { "Not during a service." } else { "Waiting for the last one to finish." };
            let put = ui.add_enabled(can_change, egui::Button::new("Put a picture in this slot…"));
            if put
                .on_hover_text("A PNG or JPEG, fitted to the switcher's format on black (no stretching).")
                .on_disabled_hover_text(why)
                .clicked()
            {
                self.put_picture(target, slot);
                ui.close();
            }
            if ui.add_enabled(used && !self.pool_busy(), egui::Button::new("Save this still to a file…")).clicked() {
                self.save_still(target, slot, still_name.clone());
                ui.close();
            }
            ui.add_enabled_ui(used && can_change, |ui| {
                ui.menu_button("Clear the slot", |ui| {
                    if ui.button(format!("Empty slot {}", slot + 1)).clicked() {
                        self.clear_slot(target, slot);
                        ui.close();
                    }
                });
            });
        });
    }

    /// At start-up, once the routing is read: when it differs from the Default presets,
    /// ask (once) whether to put them back. Only before a service has started.
    pub(in crate::ui) fn defaults_prompt(&mut self, ctx_ui: &mut Ui, snap: &Snapshot) {
        if self.defaults_ask == DefaultsAsk::Done {
            return;
        }
        let c = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        if snap.stage != Stage::Idle {
            self.defaults_ask = DefaultsAsk::Done;
            return;
        }
        let Some(off) = off_default(snap, &c) else { return };
        if off.is_empty() {
            self.defaults_ask = DefaultsAsk::Done;
            return;
        }
        self.defaults_ask = DefaultsAsk::Asking;
        let text = defaults_text(&off);
        let mut answer = None;
        egui::Panel::top("defaults")
            .frame(egui::Frame::new().fill(theme::AMBER_DARK).inner_margin(egui::Margin::symmetric(28, 8)))
            .show(ctx_ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(text).color(theme::AMBER_TEXT));
                    if ui.small_button("Restore the defaults").clicked() {
                        answer = Some(true);
                    }
                    if ui.small_button("Leave it").clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => {
                for o in &off {
                    info!(target = o.target.name(), slot = o.slot, "restoring the Default preset at start-up");
                    self.send(Command::RecallPreset(o.target, o.slot));
                }
                self.defaults_ask = DefaultsAsk::Done;
            }
            Some(false) => {
                info!("Default presets left as they are at start-up");
                self.defaults_ask = DefaultsAsk::Done;
            }
            None => {}
        }
    }
}

/// Marks the opening (`opening`) or closing still of `target`; `None` unmarks it where
/// that is allowed (the Screens ATEM).
fn set_still(c: &mut Config, target: Target, opening: bool, slot: Option<u16>) {
    match (target, opening) {
        (Target::StreamAtem, true) => c.stream_atem.opening_still = slot.unwrap_or(c.stream_atem.opening_still),
        (Target::StreamAtem, false) => c.stream_atem.closing_still = slot.unwrap_or(c.stream_atem.closing_still),
        (Target::ScreensAtem, true) => c.screens_atem.opening_still = slot,
        (Target::ScreensAtem, false) => c.screens_atem.closing_still = slot,
        (Target::MultiView, _) => {}
    }
}

/// A small filled badge with its top left at `at`; returns its width.
pub(in crate::ui) fn badge(ui: &Ui, at: egui::Pos2, text: &str, colour: Color32) -> f32 {
    let p = ui.painter();
    let g = p.layout_no_wrap(text.into(), theme::heading_font(10.5), Color32::from_rgb(10, 12, 16));
    let r = Rect::from_min_size(at, g.size() + vec2(8.0, 3.0));
    p.rect_filled(r, 3.0, colour);
    p.galley(r.min + vec2(4.0, 1.5), g, Color32::from_rgb(10, 12, 16));
    r.width()
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamit_core::routing::{Preset, RoutingState, TargetState};

    #[test]
    fn badges_follow_the_input_jobs() {
        let mut c = Config::default();
        c.stream_atem.live_inputs = vec![1, 2, 3];
        c.stream_atem.default_live_input = 2;
        c.stream_atem.decklink_input = 8;
        c.screens_atem.decklink_input = 4;
        let names = |t, i| input_jobs(&c, t, i).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(names(Target::StreamAtem, 1), vec!["GO LIVE", "SAFE"]);
        assert_eq!(names(Target::StreamAtem, 7), vec!["MEDIA"]);
        assert!(names(Target::StreamAtem, 4).is_empty());
        assert_eq!(names(Target::ScreensAtem, 3), vec!["VIDEOS"]);
        assert!(names(Target::MultiView, 0).is_empty());
    }

    #[test]
    fn go_live_inputs_toggle_in_order() {
        assert_eq!(toggled(&[1, 3], 2, true), vec![1, 2, 3]);
        assert_eq!(toggled(&[1, 2, 3], 2, false), vec![1, 3]);
        assert_eq!(toggled(&[1, 2], 2, true), vec![1, 2], "no duplicate");
    }

    #[test]
    fn stills_unmark_only_on_the_screens_atem() {
        let mut c = Config::default();
        set_still(&mut c, Target::StreamAtem, true, Some(4));
        set_still(&mut c, Target::StreamAtem, true, None);
        assert_eq!(c.stream_atem.opening_still, 4, "the Stream ATEM always has one");
        set_still(&mut c, Target::ScreensAtem, false, Some(2));
        assert_eq!(marked_stills(&c, Target::ScreensAtem), (None, Some(2)));
        set_still(&mut c, Target::ScreensAtem, false, None);
        assert_eq!(marked_stills(&c, Target::ScreensAtem), (None, None));
    }

    fn snap_with(dests: Vec<Option<u16>>, preset: Vec<u16>) -> Snapshot {
        let mut s = Snapshot::default();
        let mut routing = RoutingState::default();
        routing.targets[Target::ScreensAtem.index()] = TargetState {
            outputs: vec![21, 22, 0, 0, 0, 0, 0, 0],
            dests,
            presets: vec![Preset { slot: 3, name: "Service".into(), inputs: preset }],
            ..TargetState::default()
        };
        s.routing = routing;
        s
    }

    #[test]
    fn off_default_waits_then_lists_what_differs() {
        let mut c = Config::default();
        let s = snap_with(vec![Some(5), Some(2)], vec![5, 9, 0, 0, 0, 0, 0, 0]);
        assert_eq!(off_default(&s, &c).map(|v| v.len()), Some(0), "no Default preset: nothing to ask");
        c.screens_routing.default_preset = Some(3);
        let off = off_default(&s, &c).expect("known");
        assert_eq!((off.len(), off[0].slot, off[0].routes.clone()), (1, 3, vec![(1, 9)]));
        assert_eq!(defaults_text(&off), "The routing is not as the Default presets have it (Screens ATEM: Input 2).");
        let unknown = snap_with(vec![Some(5), None], vec![5, 9, 0, 0, 0, 0, 0, 0]);
        assert!(off_default(&unknown, &c).is_none(), "input 2 not read yet");
        let same = snap_with(vec![Some(5), Some(9)], vec![5, 9, 0, 0, 0, 0, 0, 0]);
        assert!(off_default(&same, &c).unwrap().is_empty());
    }
}
