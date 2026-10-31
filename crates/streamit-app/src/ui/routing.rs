//! The Routing tab: what the VideoHub feeds. Three sub-tabs, MultiView, Stream ATEM and
//! Screens ATEM, work the same way: VideoHub inputs run down both sides, the destinations
//! (the MultiView's views, or an ATEM's inputs) sit in the middle with eight presets
//! under them, and dropping an input on a destination routes the VideoHub output feeding
//! it. What the destinations show comes from the VideoHub, under an ATEM input's own name
//! (see `names`). Changing an ATEM input that is on program asks first. Right-clicking an
//! ATEM input, a preset or a media pool slot sets up what StreamIt uses it for (see `setup`).
mod names;
pub(super) mod pool;
mod setup;
mod videohub;

use super::StreamItApp;
use crate::theme::{self, Kind};
use eframe::egui::{self, Align, Align2, Color32, Layout, Order, Rect, RichText, Sense, Stroke, Ui, vec2};
pub(super) use setup::DefaultsAsk;
use std::collections::HashMap;
use streamit_core::devices::LabelKind;
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, Snapshot};
use streamit_core::routing::{
    PRESET_SLOTS, Preset, RoutingState, Target, TargetState, VIEW_MV_INPUT, locked, preset_name, with_preset,
    without_preset,
};
use tracing::info;

/// The MultiView's view colours, as on the Stream Deck.
pub(super) const VIEW_COLOURS: [Color32; 4] = [
    Color32::from_rgb(76, 154, 255),
    Color32::from_rgb(245, 165, 36),
    Color32::from_rgb(61, 214, 140),
    Color32::from_rgb(232, 121, 201),
];
/// Each ATEM's colour on its tiles, badges and preset chips: violet for the Stream ATEM,
/// cyan for the Screens ATEM, apart from each other and from the MultiView's four.
pub(super) const STREAM_ATEM_COLOUR: Color32 = Color32::from_rgb(167, 139, 250);
pub(super) const SCREENS_ATEM_COLOUR: Color32 = Color32::from_rgb(34, 211, 238);
/// How many inputs to list before the VideoHub has said.
const DEFAULT_INPUTS: u16 = 40;
const COL_W: f32 = 230.0;
const GAP: f32 = 28.0;
const CHIP_H: f32 = 24.0;
const SLOT_H: f32 = 64.0;
const MV_TILE_MAX_W: f32 = 290.0;
const ATEM_TILE_MAX_W: f32 = 200.0;

/// An input being dragged onto a destination.
struct InputDrag(u16);

/// Routes waiting for "Change it" because they change an input on program.
struct OnAir {
    target: Target,
    /// The preset to recall, or the single route to make.
    preset: Option<u8>,
    routes: Vec<(usize, u16)>,
    title: String,
    text: String,
}

/// A preset to recall: its target, its slot and the routes it needs.
type Recall = (Target, u8, Vec<(usize, u16)>);

/// The sub-tabs besides the three targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Extra {
    /// Rename the VideoHub's inputs and outputs.
    VideoHub,
    /// Which router output feeds each destination.
    Config,
}

/// The window's own routing state: nothing here is saved.
pub(super) struct RtUi {
    target: Target,
    /// The VideoHub or Config sub-tab, when one is open instead of a target.
    extra: Option<Extra>,
    /// Labels being typed on the VideoHub sub-tab, until sent or dropped.
    drafts: HashMap<(LabelKind, u16), String>,
    /// An input clicked (rather than dragged): clicking a destination routes it there.
    picked: Option<u16>,
    /// Save preset was pressed: the next slot clicked is saved.
    saving: bool,
    /// The slot being named, and the name typed so far.
    naming: Option<(u8, String)>,
    /// The slot whose Clear is being confirmed.
    clearing: Option<u8>,
    on_air: Option<OnAir>,
    /// A preset clicked this frame: recalled once the frame is drawn.
    recall: Option<Recall>,
    /// The input or output picked on the VideoHub sub-tab.
    vh_pick: Option<videohub::Pick>,
    /// The VideoHub name being edited (after a double-click).
    vh_editing: Option<(LabelKind, u16)>,
    /// The cable run being edited (from the row's menu), and its text so far.
    vh_cable: Option<(LabelKind, u16)>,
    vh_cable_draft: String,
    /// The ATEM input name being typed on its tile.
    atem_naming: Option<names::Naming>,
}

impl RtUi {
    /// The routing target shown (kept while the VideoHub or Config sub-tab is open).
    pub(super) fn target(&self) -> Target {
        self.target
    }

    /// Show `t`, as when its sub-tab is clicked.
    pub(super) fn show_target(&mut self, t: Target) {
        self.target = t;
        self.extra = None;
    }
}

impl Default for RtUi {
    fn default() -> Self {
        Self {
            target: Target::MultiView,
            extra: None,
            drafts: HashMap::new(),
            picked: None,
            saving: false,
            naming: None,
            clearing: None,
            on_air: None,
            recall: None,
            vh_pick: None,
            vh_editing: None,
            vh_cable: None,
            vh_cable_draft: String::new(),
            atem_naming: None,
        }
    }
}

/// The inputs listed on the left and on the right: the first half and the rest.
pub(super) fn input_columns(count: u16) -> (std::ops::RangeInclusive<u16>, std::ops::RangeInclusive<u16>) {
    let left = count.div_ceil(2);
    (1..=left, left + 1..=count)
}

/// The destinations of a target showing `input` now.
pub(super) fn dests_on(t: &TargetState, input: u16) -> Vec<usize> {
    (0..t.dests.len()).filter(|&i| t.dest(i) == Some(input)).collect()
}

fn colour(target: Target, i: usize) -> Color32 {
    match target {
        Target::MultiView => VIEW_COLOURS[i % VIEW_COLOURS.len()],
        Target::StreamAtem => STREAM_ATEM_COLOUR,
        Target::ScreensAtem => SCREENS_ATEM_COLOUR,
    }
}

/// What the hall or the stream sees, for the on-air question.
fn audience(target: Target) -> &'static str {
    if target == Target::StreamAtem { "the stream shows" } else { "the hall sees" }
}

impl StreamItApp {
    pub(super) fn routing_tab(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let routing = &snap.routing;
        let target = self.routes_ui.target;
        let state = routing.target(target);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Routing").font(theme::heading_font(30.0)));
            ui.add_space(12.0);
            let extra = self.routes_ui.extra;
            for t in Target::ALL {
                if theme::tab(ui, t.name(), extra.is_none() && t == target, false).clicked()
                    && (t != target || extra.is_some())
                {
                    self.routes_ui = RtUi { target: t, ..RtUi::default() };
                }
            }
            ui.add_space(8.0);
            for (e, name) in [(Extra::VideoHub, "VideoHub"), (Extra::Config, "Config")] {
                if theme::tab(ui, name, extra == Some(e), false).clicked() && extra != Some(e) {
                    self.routes_ui = RtUi { target, extra: Some(e), ..RtUi::default() };
                }
            }
            ui.add_space(12.0);
            let (tone, text) = match snap.device_status.get(&DeviceKind::VideoHub) {
                Some(DeviceHealth::Ok) => (theme::GREEN, "VideoHub connected"),
                Some(DeviceHealth::Warning(_)) => (theme::AMBER, "VideoHub slow to answer"),
                Some(DeviceHealth::Down(_)) => (theme::RED, "VideoHub not connected · nothing can be routed"),
                None => (theme::DIM, "Checking the VideoHub…"),
            };
            theme::dot(ui, tone);
            ui.label(RichText::new(text).size(14.0).color(theme::SOFT));
        });
        ui.add_space(12.0);
        match self.routes_ui.extra {
            Some(Extra::VideoHub) => return self.videohub_page(ui, snap),
            Some(Extra::Config) => return self.config_page(ui, snap),
            None => {}
        }
        if state.outputs.iter().all(|&o| o == 0) {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Set which router output feeds the {} first.", target.name()))
                        .color(theme::AMBER_SOFT),
                );
                if theme::button(ui, "Open Config", Kind::Plain).clicked() {
                    self.routes_ui.extra = Some(Extra::Config);
                }
            });
            ui.add_space(12.0);
        }
        let count = if routing.labels.is_empty() { DEFAULT_INPUTS } else { routing.labels.len() as u16 };
        let (left, right) = input_columns(count);
        theme::panel().inner_margin(20).show(ui, |ui| {
            let centre_w = (ui.available_width() - 2.0 * (COL_W + GAP)).max(320.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                ui.allocate_ui_with_layout(vec2(COL_W, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.input_column(ui, routing, left, false);
                });
                ui.allocate_ui_with_layout(vec2(centre_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.dest_grid(ui, snap);
                    ui.add_space(14.0);
                    self.presets_row(ui, state);
                });
                ui.allocate_ui_with_layout(vec2(COL_W, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.input_column(ui, routing, right, true);
                });
            });
        });
        if target.is_atem() {
            ui.add_space(16.0);
            self.media_pool(ui, snap, target);
        }
        drag_ghost(ui, routing);
        self.take_routing_recall(snap);
        self.on_air_question(ui.ctx());
    }

    fn input_column(
        &mut self,
        ui: &mut Ui,
        routing: &RoutingState,
        inputs: std::ops::RangeInclusive<u16>,
        right: bool,
    ) {
        ui.spacing_mut().item_spacing.y = 3.0;
        let (first, last) = (*inputs.start(), *inputs.end());
        let head = if last >= first { format!("{first}–{last}") } else { String::new() };
        ui.horizontal(|ui| {
            theme::lbl(ui, "Router inputs");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| theme::lbl(ui, &head));
        });
        for n in inputs {
            self.input_chip(ui, routing, n, right);
        }
    }

    fn input_chip(&mut self, ui: &mut Ui, routing: &RoutingState, n: u16, right: bool) {
        let target = self.routes_ui.target;
        let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), CHIP_H), Sense::click_and_drag());
        let on = dests_on(routing.target(target), n);
        let picked = self.routes_ui.picked == Some(n);
        let border = if picked || r.hovered() { theme::BLUE } else { theme::BUTTON_BORDER };
        let fill = if on.is_empty() { theme::INSET } else { Color32::from_rgb(0x18, 0x20, 0x2B) };
        let p = ui.painter();
        p.rect(rect, 6.0, fill, Stroke::new(if picked { 2.0 } else { 1.0 }, border), egui::StrokeKind::Inside);
        let num_color = if on.is_empty() { theme::DIM } else { theme::TEXT };
        let label_color = if on.is_empty() { theme::SOFT } else { theme::TEXT };
        let (num_pos, num_align, label_x) = if right {
            (rect.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, rect.right() - 44.0)
        } else {
            (rect.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, rect.left() + 44.0)
        };
        p.text(num_pos, num_align, n.to_string(), theme::mono_bold(13.0), num_color);
        // Badges for the destinations this input is on, on the side nearest them.
        let badge_w = 24.0;
        let badges_w = on.len() as f32 * (badge_w + 3.0);
        for (k, &i) in on.iter().enumerate() {
            let x = if right {
                rect.left() + 6.0 + k as f32 * (badge_w + 3.0)
            } else {
                rect.right() - 6.0 - badges_w + k as f32 * (badge_w + 3.0)
            };
            let b = Rect::from_min_size(egui::pos2(x, rect.center().y - 8.0), vec2(badge_w, 16.0));
            p.rect_filled(b, 4.0, colour(target, i));
            p.text(b.center(), Align2::CENTER_CENTER, target.dest_key(i), theme::mono_bold(10.0), Color32::BLACK);
        }
        let room = (rect.width() - 44.0 - 12.0 - badges_w).max(10.0);
        // Only the VideoHub's own label: the number is already on the chip.
        let own = routing.own_label(n).unwrap_or("").to_string();
        let galley = ui.painter().layout(own, egui::FontId::proportional(13.0), label_color, f32::INFINITY);
        let clipped = ui.painter().with_clip_rect(if right {
            Rect::from_min_max(egui::pos2(label_x - room, rect.top()), egui::pos2(label_x, rect.bottom()))
        } else {
            Rect::from_min_max(egui::pos2(label_x, rect.top()), egui::pos2(label_x + room, rect.bottom()))
        });
        let y = rect.center().y - galley.size().y / 2.0;
        let x = if right { label_x - galley.size().x.min(room) } else { label_x };
        clipped.galley(egui::pos2(x, y), galley, label_color);

        if r.drag_started() {
            egui::DragAndDrop::set_payload(ui.ctx(), InputDrag(n));
            self.routes_ui.picked = None;
        }
        if r.clicked() {
            self.routes_ui.picked = if picked { None } else { Some(n) };
        }
        r.on_hover_cursor(egui::CursorIcon::Grab);
    }

    fn dest_grid(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let target = self.routes_ui.target;
        ui.horizontal(|ui| {
            theme::lbl(ui, if target == Target::MultiView { "MultiView 4" } else { target.name() });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let sub = if target.is_atem() {
                    "Red: on program · green: on preview · right-click an input to give it a job"
                } else {
                    "Each view is fed by one router output"
                };
                ui.label(RichText::new(sub).size(13.0).color(theme::MUTED));
            });
        });
        if target == Target::ScreensAtem {
            ui.add_space(4.0);
            self.go_live_scene(ui, snap);
        }
        ui.add_space(6.0);
        // 16:9 tiles, small enough that the presets below stay on screen.
        let (cols, max_w) = if target.is_atem() { (4, ATEM_TILE_MAX_W) } else { (2, MV_TILE_MAX_W) };
        let rows = target.size().div_ceil(cols);
        let pad = 12.0;
        let w = ui.available_width();
        let tile_w = ((w - (cols as f32 + 1.0) * pad) / cols as f32).min(max_w);
        let tile = vec2(tile_w, tile_w * 9.0 / 16.0);
        let (area, _) = ui.allocate_exact_size(vec2(w, rows as f32 * (tile.y + pad) + pad), Sense::hover());
        let frame = Rect::from_center_size(area.center(), vec2(cols as f32 * (tile.x + pad) + pad, area.height()));
        ui.painter().rect(
            frame,
            10.0,
            Color32::from_rgb(7, 9, 12),
            Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        for i in 0..target.size() {
            let (col, row) = ((i % cols) as f32, (i / cols) as f32);
            let min = frame.min + vec2(pad + col * (tile.x + pad), pad + row * (tile.y + pad));
            self.dest_tile(ui, Rect::from_min_size(min, tile), i, snap);
        }
        ui.add_space(8.0);
        if target.is_atem() {
            self.names_from_hub_button(ui, snap, target);
        }
        if let Some(w) = snap.last_warning.as_deref().filter(|w| w.starts_with(target.name())) {
            ui.label(RichText::new(w).size(14.0).color(theme::AMBER_SOFT));
        } else if let Some(n) = self.routes_ui.picked {
            let text = format!("Input {n} ({}) picked. Click a destination to send it there.", snap.routing.label(n));
            ui.label(RichText::new(text).size(14.0).color(theme::SOFT));
        }
    }

    fn dest_tile(&mut self, ui: &mut Ui, rect: Rect, i: usize, snap: &Snapshot) {
        let target = self.routes_ui.target;
        let state = snap.routing.target(target);
        let output = state.output(i);
        let is_locked = locked(target, i, snap.job.is_some(), snap.routing.media);
        let r = ui.interact(rect, ui.scope_id().with(("rt-dest", target.index(), i)), Sense::click());
        let col = if output == 0 { theme::BORDER } else { colour(target, i) };
        let hovering = output != 0
            && !is_locked
            && (r.dnd_hover_payload::<InputDrag>().is_some() || (self.routes_ui.picked.is_some() && r.hovered()));
        let fill = if hovering {
            Color32::from_rgb(0x24, 0x31, 0x4A)
        } else if output == 0 {
            theme::INSET
        } else {
            theme::SCREEN
        };
        let (pgm, pvw) = (target.is_atem() && state.on_air(i), target.is_atem() && state.on_preview(i));
        let p = ui.painter();
        if pgm {
            p.rect_stroke(rect.expand(3.0), 8.0, Stroke::new(3.0, theme::RED), egui::StrokeKind::Inside);
        }
        p.rect(rect, 6.0, fill, Stroke::new(2.0, col), egui::StrokeKind::Inside);
        let small_font = if target.is_atem() { 11.0 } else { 12.0 };
        let input = i as u16 + 1;
        let own = target.is_atem().then(|| state.own_name(input)).flatten();
        let where_ = match (target, own.and_then(|_| state.names.get(&input))) {
            (Target::MultiView, _) => format!("{} · MV in {}", target.dest_name(i).to_uppercase(), VIEW_MV_INPUT[i]),
            (_, Some(n)) if !n.short.trim().is_empty() => {
                format!("{} · {}", target.dest_name(i).to_uppercase(), n.short.trim())
            }
            _ => target.dest_name(i).to_uppercase(),
        };
        p.text(rect.left_top() + vec2(8.0, 6.0), Align2::LEFT_TOP, where_, theme::heading_font(small_font), col);
        if pgm || pvw {
            let (text, bg, fg) = if pgm {
                ("ON AIR", theme::RED, Color32::WHITE)
            } else {
                ("PREVIEW", theme::GREEN, Color32::from_rgb(6, 19, 12))
            };
            let g = p.layout_no_wrap(text.into(), theme::heading_font(10.0), fg);
            let tag = Rect::from_min_size(rect.right_top() + vec2(-g.size().x - 14.0, 5.0), g.size() + vec2(8.0, 2.0));
            p.rect_filled(tag, 3.0, bg);
            p.galley(tag.min + vec2(4.0, 1.0), g, fg);
        }
        let out = if output == 0 { String::new() } else { format!("Router out {output}") };
        p.text(rect.right_bottom() - vec2(8.0, 5.0), Align2::RIGHT_BOTTOM, out, theme::mono(10.5), theme::DIM);
        if target.is_atem() {
            self.job_badges(ui, rect, target, i);
            r.context_menu(|ui| {
                self.input_menu(ui, target, i, snap.job.is_some(), snap.emergency_audio);
                ui.separator();
                self.name_menu(ui, snap, target, input);
            });
            if r.double_clicked() && !state.names.is_empty() {
                self.start_naming(snap, target, input);
            }
        }
        let feed = if output == 0 { None } else { state.dest(i) };
        // An ATEM's own name goes first, with the VideoHub input feeding it under it.
        let (small, big, under) = match (own, feed) {
            (Some(name), Some(n)) => {
                let label = snap.routing.own_label(n).map(|l| format!(" {l}")).unwrap_or_default();
                (String::new(), name.to_string(), format!("← VH {n}{label}"))
            }
            (Some(name), None) if output == 0 => (String::new(), name.to_string(), "Not from the router".into()),
            (Some(name), None) => (String::new(), name.to_string(), String::new()),
            (None, Some(n)) => (format!("INPUT {n}"), snap.routing.label(n), String::new()),
            (None, None) if output == 0 => (String::new(), "Not from the router".to_string(), String::new()),
            (None, None) => (String::new(), "Not known yet".to_string(), String::new()),
        };
        let naming = target.is_atem() && self.naming_field(ui, rect, target, input);
        let p = ui.painter();
        let big_size = if target.is_atem() { 17.0 } else { 24.0 };
        let mut job = egui::text::LayoutJob { halign: Align::Center, ..Default::default() };
        job.wrap.max_width = rect.width() - 16.0;
        if !small.is_empty() {
            job.append(&small, 0.0, egui::TextFormat::simple(theme::mono_bold(12.0), col));
            job.append("\n", 0.0, egui::TextFormat::simple(theme::mono_bold(5.0), col));
        }
        let big_colour = if output == 0 && own.is_none() { theme::DIM } else { theme::SCREEN_TEXT };
        job.append(&big, 0.0, egui::TextFormat::simple(theme::heading_font(big_size), big_colour));
        if !under.is_empty() {
            job.append("\n", 0.0, egui::TextFormat::simple(theme::mono_bold(5.0), col));
            let under_colour = if output == 0 { theme::DIM } else { col };
            job.append(&under, 0.0, egui::TextFormat::simple(theme::mono(11.0), under_colour));
        }
        if is_locked {
            job.append("\nLocked while ", 0.0, egui::TextFormat::simple(theme::body_bold(11.0), theme::AMBER_SOFT));
            job.append(
                snap.job.unwrap_or("a sequence"),
                0.0,
                egui::TextFormat::simple(theme::body_bold(11.0), theme::AMBER_SOFT),
            );
            job.append(" runs", 0.0, egui::TextFormat::simple(theme::body_bold(11.0), theme::AMBER_SOFT));
        }
        if !naming {
            let galley = p.layout_job(job);
            let top = rect.center().y - galley.size().y / 2.0 + 4.0;
            p.with_clip_rect(rect.shrink(4.0)).galley(egui::pos2(rect.center().x, top), galley, theme::SCREEN_TEXT);
        }

        let dropped = r.dnd_release_payload::<InputDrag>().map(|d| d.0);
        let clicked = r.clicked().then_some(self.routes_ui.picked).flatten();
        if let Some(input) = dropped.or(clicked) {
            self.routes_ui.picked = None;
            if output == 0 {
                self.notice = Some(format!("{} {} is not fed by the router.", target.name(), target.dest_name(i)));
            } else if is_locked {
                self.notice = Some(format!(
                    "Stream ATEM input {} carries StreamIt's videos: it can't be routed while {} runs.",
                    i + 1,
                    snap.job.unwrap_or("a sequence")
                ));
            } else if state.dest(i) != Some(input) {
                self.request_routes(snap, target, None, vec![(i, input)]);
            }
        }
    }

    /// Sends routes (or a preset recall), asking first when they change an ATEM input that
    /// is on program.
    fn request_routes(&mut self, snap: &Snapshot, target: Target, preset: Option<u8>, routes: Vec<(usize, u16)>) {
        let state = snap.routing.target(target);
        if let Some(&(i, input)) = routes.iter().find(|&&(i, _)| target.is_atem() && state.on_air(i)) {
            let what = match preset.and_then(|s| state.preset(s)) {
                Some(p) => format!("The preset {} changes it to {}", p.name, snap.routing.label(input)),
                None => format!("Changing it to {}", snap.routing.label(input)),
            };
            self.routes_ui.on_air = Some(OnAir {
                target,
                preset,
                routes,
                title: format!("{} {} is on air", target.name(), target.dest_name(i)),
                text: format!("It is on program now. {what} changes what {} straight away.", audience(target)),
            });
            return;
        }
        self.send_routes(target, preset, routes);
    }

    fn send_routes(&mut self, target: Target, preset: Option<u8>, routes: Vec<(usize, u16)>) {
        match preset {
            Some(slot) => {
                info!(target = target.name(), slot, "routing: recalling a preset from the window");
                self.send(Command::RecallPreset(target, slot));
            }
            None => {
                for (dest, input) in routes {
                    info!(target = target.name(), dest, input, "routing from the window");
                    self.send(Command::Route { target, dest, input });
                }
            }
        }
    }

    fn on_air_question(&mut self, ctx: &egui::Context) {
        let Some(q) = &self.routes_ui.on_air else { return };
        let (title, text) = (q.title.clone(), q.text.clone());
        let mut answer = None;
        egui::Modal::new(egui::Id::unique("rt-on-air")).show(ctx, |ui| {
            ui.set_max_width(440.0);
            ui.heading(title);
            ui.label(text);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Keep it").strong()).clicked() {
                    answer = Some(false);
                }
                if ui.button(RichText::new("Change it").color(Color32::RED)).clicked() {
                    answer = Some(true);
                }
            });
        });
        if answer == Some(true)
            && let Some(q) = self.routes_ui.on_air.take()
        {
            info!(target = q.target.name(), "routing: on-air change confirmed");
            self.send_routes(q.target, q.preset, q.routes);
        } else if answer == Some(false) {
            self.routes_ui.on_air = None;
        }
    }

    // ---- presets

    fn presets_row(&mut self, ui: &mut Ui, state: &TargetState) {
        ui.horizontal(|ui| {
            theme::lbl(ui, "Presets");
            ui.add_space(10.0);
            let msg = if self.routes_ui.saving {
                "Pick the slot to save in. A used slot is replaced."
            } else {
                "Click a preset to put its inputs back."
            };
            ui.label(RichText::new(msg).size(14.0).color(theme::MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, kind) =
                    if self.routes_ui.saving { ("Cancel", Kind::Plain) } else { ("Save preset", Kind::Primary) };
                if theme::button(ui, text, kind).clicked() {
                    self.routes_ui.saving = !self.routes_ui.saving;
                    self.routes_ui.naming = None;
                    self.routes_ui.clearing = None;
                }
            });
        });
        ui.add_space(8.0);
        // Two rows of four.
        let (gap, per_row) = (8.0, PRESET_SLOTS / 2);
        let w = (ui.available_width() - gap * (per_row as f32 - 1.0)) / per_row as f32;
        ui.spacing_mut().item_spacing.y = gap;
        for row in 0..2 {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for slot in row * per_row + 1..=(row + 1) * per_row {
                    ui.allocate_ui_with_layout(vec2(w, SLOT_H), Layout::top_down(Align::Min), |ui| {
                        ui.set_min_size(vec2(w, SLOT_H));
                        self.preset_slot(ui, state, slot);
                    });
                }
            });
        }
    }

    fn preset_slot(&mut self, ui: &mut Ui, state: &TargetState, slot: u8) {
        let target = self.routes_ui.target;
        let preset = state.preset(slot).cloned();
        if let Some((s, _)) = &self.routes_ui.naming
            && *s == slot
        {
            return self.preset_namer(ui, state, slot);
        }
        if self.routes_ui.clearing == Some(slot)
            && let Some(p) = &preset
        {
            return self.preset_clear_confirm(ui, state, p);
        }
        let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), SLOT_H));
        let r = ui.allocate_rect(
            rect,
            if preset.is_some() || self.routes_ui.saving { Sense::click() } else { Sense::hover() },
        );
        let current = preset.as_ref().is_some_and(|p| state.is_current(p)) && !self.routes_ui.saving;
        let default = self.config.read().unwrap_or_else(|e| e.into_inner()).routing(target).default_preset;
        let is_default = preset.is_some() && default == Some(slot);
        let (fill, border) = match (&preset, self.routes_ui.saving, current) {
            (_, true, _) => (theme::AMBER_DARK, theme::AMBER),
            (Some(_), false, true) => (theme::INSET, theme::GREEN),
            (Some(_), false, false) => (theme::INSET, if r.hovered() { theme::BLUE } else { theme::BUTTON_BORDER }),
            (None, false, _) => (theme::GROUND, theme::BORDER),
        };
        let p = ui.painter();
        p.rect(rect, 8.0, fill, Stroke::new(if current { 2.0 } else { 1.0 }, border), egui::StrokeKind::Inside);
        let num_colour = if current { theme::GREEN } else { theme::DIM };
        p.text(
            rect.left_top() + vec2(10.0, 8.0),
            Align2::LEFT_TOP,
            slot.to_string(),
            theme::mono_bold(11.0),
            num_colour,
        );
        match &preset {
            Some(pr) => {
                let name = p.layout(pr.name.clone(), theme::heading_font(17.0), theme::TEXT, rect.width() - 56.0);
                p.with_clip_rect(rect.shrink(2.0)).galley(rect.left_top() + vec2(26.0, 6.0), name, theme::TEXT);
                // The input on each destination it covers, in the destination's colour.
                let chip_w = if target.is_atem() { 20.0 } else { 26.0 };
                let mut x = rect.left() + 10.0;
                for (i, &input) in pr.inputs.iter().enumerate().filter(|(_, n)| **n != 0) {
                    let b = Rect::from_min_size(egui::pos2(x, rect.bottom() - 24.0), vec2(chip_w, 15.0));
                    p.rect_filled(b, 3.0, colour(target, i));
                    p.text(b.center(), Align2::CENTER_CENTER, input.to_string(), theme::mono_bold(9.5), Color32::BLACK);
                    x += chip_w + 2.0;
                }
            }
            None => {
                let text = if self.routes_ui.saving { "Save here" } else { "Empty" };
                let colour = if self.routes_ui.saving { theme::TEXT } else { theme::DIM };
                p.text(
                    rect.left_center() + vec2(26.0, 0.0),
                    Align2::LEFT_CENTER,
                    text,
                    theme::heading_font(16.0),
                    colour,
                );
            }
        }
        if is_default {
            setup::badge(ui, rect.right_bottom() + vec2(-78.0, -22.0), "★ DEFAULT", setup::DEFAULT_STAR);
        }
        if let Some(pr) = &preset
            && !self.routes_ui.saving
        {
            let name = pr.name.clone();
            r.context_menu(|ui| {
                ui.label(RichText::new(format!("PRESET {slot} · {}", name.to_uppercase())).size(11.0).color(theme::MUTED));
                let (text, new) = if is_default {
                    ("No longer the default", None)
                } else {
                    ("Make the default", Some(slot))
                };
                if ui
                    .button(text)
                    .on_hover_text("Recover puts the Media input back to the Default, and at start-up StreamIt offers to restore it")
                    .clicked()
                {
                    info!(target = target.name(), slot, default = new.is_some(), "Default preset changed");
                    self.save_routing(target, move |r| r.default_preset = new);
                    ui.close();
                }
                if ui.button("Clear…").clicked() {
                    self.routes_ui.clearing = Some(slot);
                    ui.close();
                }
            });
        }
        // × clears the preset (after asking). It never changes the routing.
        if let Some(pr) = &preset
            && !self.routes_ui.saving
        {
            // The × is part of the slot: a click on it never also recalls the preset.
            let x = Rect::from_min_size(rect.right_top() + vec2(-30.0, 4.0), vec2(26.0, 26.0));
            let over = ui.rect_contains_pointer(x);
            let colour = if over { theme::TEXT } else { theme::DIM };
            ui.painter().text(x.center(), Align2::CENTER_CENTER, "×", egui::FontId::proportional(18.0), colour);
            if r.clicked() && over {
                info!(target = target.name(), slot, name = %pr.name, "asking before clearing a preset");
                self.routes_ui.clearing = Some(slot);
                return;
            }
        }
        if r.clicked() {
            if self.routes_ui.saving {
                if state.snapshot_inputs().is_some() {
                    let name = preset.map(|p| p.name).unwrap_or_default();
                    self.routes_ui.naming = Some((slot, name));
                } else {
                    self.notice = Some(not_saved(target));
                    self.routes_ui.saving = false;
                }
            } else if let Some(pr) = &preset {
                // The snapshot shown is the one the routes are judged against.
                self.routes_ui.recall = Some((target, pr.slot, state.changes(pr)));
            }
        }
    }

    fn preset_namer(&mut self, ui: &mut Ui, state: &TargetState, slot: u8) {
        let target = self.routes_ui.target;
        slot_box(ui, theme::AMBER_DARK, theme::AMBER, |ui| {
            let Some((_, name)) = self.routes_ui.naming.as_mut() else { return };
            let edit = ui.add(
                egui::TextEdit::singleline(name)
                    .id(egui::Id::unique(("preset-name", target.index(), slot)))
                    .hint_text(format!("Preset {slot}"))
                    .desired_width(f32::INFINITY),
            );
            if !edit.has_focus() && !edit.lost_focus() {
                edit.request_focus();
            }
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let (mut save, mut cancel) = (enter, escape);
            ui.horizontal(|ui| {
                cancel |= ui.small_button("Cancel").clicked();
                save |= ui.small_button("Save").clicked();
            });
            if cancel {
                self.routes_ui.naming = None;
                self.routes_ui.saving = false;
            } else if save {
                let typed = self.routes_ui.naming.take().map(|(_, n)| n).unwrap_or_default();
                self.routes_ui.saving = false;
                let Some(inputs) = state.snapshot_inputs() else {
                    self.notice = Some(not_saved(target));
                    return;
                };
                let p = Preset { slot, name: preset_name(&typed, slot), inputs };
                info!(target = target.name(), slot, name = %p.name, "routing preset saved");
                self.save_presets(target, with_preset(&state.presets, p));
            }
        });
    }

    fn preset_clear_confirm(&mut self, ui: &mut Ui, state: &TargetState, p: &Preset) {
        let target = self.routes_ui.target;
        slot_box(ui, Color32::from_rgb(0x2A, 0x12, 0x14), theme::RED, |ui| {
            ui.add(egui::Label::new(RichText::new(format!("Clear {}?", p.name)).strong()).truncate())
                .on_hover_text("Clears the preset and its name. The routing stays as it is.");
            ui.horizontal(|ui| {
                if ui.small_button("Keep").clicked() {
                    self.routes_ui.clearing = None;
                }
                if ui.small_button(RichText::new("Clear").color(theme::RED)).clicked() {
                    self.routes_ui.clearing = None;
                    info!(target = target.name(), slot = p.slot, name = %p.name, "routing preset cleared");
                    let slot = p.slot;
                    self.save_routing(target, move |r| {
                        if r.default_preset == Some(slot) {
                            r.default_preset = None;
                        }
                    });
                    self.save_presets(target, without_preset(&state.presets, p.slot));
                }
            });
        });
    }

    /// A preset clicked this frame is recalled after the frame's drawing, with the
    /// snapshot it was judged against.
    fn take_routing_recall(&mut self, snap: &Snapshot) {
        if let Some((target, slot, routes)) = self.routes_ui.recall.take() {
            if routes.is_empty() {
                return;
            }
            self.request_routes(snap, target, Some(slot), routes);
        }
    }
}

// ---------------------------------------------------------------- VideoHub and Config

/// How many inputs or outputs to list: as many as the VideoHub has labels for, else 40.
fn port_count(labels: &[String]) -> u16 {
    if labels.is_empty() { DEFAULT_INPUTS } else { labels.len() as u16 }
}

/// What a label field does when it loses focus: send the typed name if it changed, or drop
/// it (Escape, or nothing changed).
pub(super) fn label_to_send(typed: &str, current: &str, escaped: bool) -> Option<String> {
    let typed = typed.trim();
    (!escaped && typed != current.trim()).then(|| typed.to_string())
}

/// Room for the × that clears a label.
const CLEAR_W: f32 = 24.0;

/// What a cleared label becomes: its number, "#5".
fn cleared_label(n: u16) -> String {
    format!("#{n}")
}

impl StreamItApp {
    /// Which router output feeds each destination. A change applies and is saved at once.
    fn config_page(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let routing = &snap.routing;
        let (config, video_out, media) = {
            let c = self.config.read().unwrap_or_else(|e| e.into_inner());
            (Target::ALL.map(|t| c.routing(t).outputs_for(t.size())), c.videohub.input8_output, c.media_dest())
        };
        let outputs = port_count(&routing.output_labels);
        theme::panel().inner_margin(20).show(ui, |ui| {
            theme::lbl(ui, "Routing config");
            self.sim_router_notes(ui);
            ui.label(
                RichText::new(
                    "Which router output feeds each destination, or \"Not from the router\" (plugged straight \
                     in). A change applies and is saved straight away.",
                )
                .color(theme::SOFT),
            );
            for t in Target::ALL {
                ui.add_space(12.0);
                ui.label(RichText::new(t.name()).font(theme::heading_font(18.0)));
                ui.add_space(4.0);
                let per_row = 4;
                for row in 0..t.size().div_ceil(per_row) {
                    ui.columns(per_row, |c| {
                        for (k, col) in c.iter_mut().enumerate() {
                            let i = row * per_row + k;
                            if i >= t.size() {
                                continue;
                            }
                            // The Media input's output is the one StreamIt routes videos through.
                            let is_media = t == Target::StreamAtem && Some(i) == media;
                            let name = if t == Target::MultiView {
                                format!("{} (MV in {})", t.dest_name(i), VIEW_MV_INPUT[i])
                            } else if is_media {
                                format!("{} (media)", t.dest_name(i))
                            } else {
                                t.dest_name(i)
                            };
                            col.label(
                                RichText::new(name.to_uppercase()).font(theme::label_font(11.0)).color(theme::MUTED),
                            );
                            let now = if is_media { video_out } else { config[t.index()][i] };
                            let shown =
                                if now == 0 { "Not from the router".to_string() } else { routing.output_name(now) };
                            let mut picked = now;
                            egui::ComboBox::from_id_salt(("rt-cfg", t.index(), i))
                                .selected_text(shown)
                                .width(col.available_width())
                                .height(400.0)
                                .show_ui(col, |ui| {
                                    ui.selectable_value(&mut picked, 0, "Not from the router");
                                    for o in 1..=outputs {
                                        ui.selectable_value(&mut picked, o, routing.output_name(o));
                                    }
                                });
                            if picked != now && is_media {
                                info!(output = picked, "routing config: the Media input's output changed");
                                self.save_setup(move |c| c.videohub.input8_output = picked);
                            } else if picked != now {
                                info!(target = t.name(), dest = i, output = picked, "routing config changed");
                                self.save_routing(t, |r| {
                                    if r.outputs.len() < t.size() {
                                        r.outputs.resize(t.size(), 0);
                                    }
                                    r.outputs[i] = picked;
                                });
                            }
                        }
                    });
                }
            }
        });
    }
}

impl StreamItApp {
    /// In simulation: the router numbers it fills in itself because they are not set yet.
    fn sim_router_notes(&self, ui: &mut Ui) {
        let defaulted = self.sim_defaults.borrow().clone();
        let Some(hub) = self.sim.borrow().as_ref().map(|s| s.config().videohub.clone()) else { return };
        let fields = [
            ("videohub.input8_output", "the Media input's router output", hub.input8_output),
            ("videohub.decklink_input", "the video source", hub.decklink_input),
            ("videohub.fallback_normal_input", "what the Media input normally shows", hub.fallback_normal_input),
        ];
        for (field, what, value) in fields {
            if let Some(n) = crate::view::sim_default_note(&defaulted, field, value) {
                ui.label(
                    RichText::new(format!("Not set: {what}. {n}; it is not saved."))
                        .size(13.0)
                        .color(theme::AMBER_SOFT),
                );
            }
        }
    }
}

/// The input being dragged follows the pointer.
fn drag_ghost(ui: &Ui, routing: &RoutingState) {
    if let (Some(d), Some(pos)) = (egui::DragAndDrop::payload::<InputDrag>(ui.ctx()), ui.ctx().pointer_interact_pos()) {
        let painter = ui.ctx().layer_painter(egui::LayerId::new(Order::Tooltip, egui::Id::unique("rt-drag")));
        let text = format!("{} · {}", d.0, routing.label(d.0));
        let galley = painter.layout_no_wrap(text, theme::body_bold(14.0), Color32::WHITE);
        let rect = Rect::from_center_size(pos - vec2(0.0, 28.0), galley.size() + vec2(20.0, 12.0));
        painter.rect_filled(rect, 6.0, theme::BLUE_FILL);
        painter.galley(rect.min + vec2(10.0, 6.0), galley, Color32::WHITE);
    }
}

fn not_saved(target: Target) -> String {
    format!("Not saved: the VideoHub has not said what every {} input shows yet.", target.name())
}

/// A preset slot's box, exactly one slot high, with `add` laid out inside it.
fn slot_box(ui: &mut Ui, fill: Color32, border: Color32, add: impl FnOnce(&mut Ui)) {
    let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), SLOT_H));
    ui.painter().rect(rect, 8.0, fill, Stroke::new(1.0, border), egui::StrokeKind::Inside);
    let inner = egui::UiBuilder::new().max_rect(rect.shrink(8.0)).layout(Layout::top_down(Align::Min));
    ui.scope_builder(inner, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        add(ui);
    });
    ui.advance_cursor_after_rect(rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cleared_label_is_its_number() {
        assert_eq!(cleared_label(5), "#5");
        assert_eq!(cleared_label(40), "#40");
    }

    #[test]
    fn inputs_split_down_both_sides() {
        assert_eq!(input_columns(40), (1..=20, 21..=40));
        assert_eq!(input_columns(16), (1..=8, 9..=16));
        assert_eq!(input_columns(9), (1..=5, 6..=9));
        let (l, r) = input_columns(0);
        assert!(l.is_empty() && r.is_empty());
    }

    #[test]
    fn an_input_can_be_on_several_destinations() {
        let t = TargetState { dests: vec![Some(3), None, Some(3), Some(4)], ..TargetState::default() };
        assert_eq!(dests_on(&t, 3), vec![0, 2]);
        assert_eq!(dests_on(&t, 4), vec![3]);
        assert!(dests_on(&t, 9).is_empty());
    }

    #[test]
    fn a_label_is_sent_only_when_it_changed_and_escape_was_not_pressed() {
        assert_eq!(label_to_send(" Pulpit ", "Cam 2", false), Some("Pulpit".into()));
        assert_eq!(label_to_send("Cam 2 ", "Cam 2", false), None, "unchanged");
        assert_eq!(label_to_send("Pulpit", "Cam 2", true), None, "Escape puts it back");
        assert_eq!(label_to_send("", "Cam 2", false), Some(String::new()), "clearing a label is a change");
        assert_eq!(port_count(&[]), 40);
        assert_eq!(port_count(&vec![String::new(); 16]), 16);
    }

    #[test]
    fn colours_follow_the_target() {
        assert_eq!(colour(Target::MultiView, 2), VIEW_COLOURS[2]);
        assert_eq!(colour(Target::StreamAtem, 2), STREAM_ATEM_COLOUR);
        assert_eq!(colour(Target::ScreensAtem, 2), SCREENS_ATEM_COLOUR);
        let all = [VIEW_COLOURS.to_vec(), vec![STREAM_ATEM_COLOUR, SCREENS_ATEM_COLOUR]].concat();
        for (i, a) in all.iter().enumerate() {
            assert!(all[i + 1..].iter().all(|b| b != a), "every routing colour is different");
        }
        assert_eq!(audience(Target::StreamAtem), "the stream shows");
        assert_eq!(audience(Target::ScreensAtem), "the hall sees");
    }
}
