//! The MultiView tab. The MultiView 4 is not controlled itself: each of its views is fed
//! by a VideoHub output, so dropping an input on a view routes that output. VideoHub
//! inputs run down both sides, the four views sit in the middle, and the eight presets
//! run along the bottom. What the views show always comes from the VideoHub.
use super::{Section, StreamItApp, Tab};
use crate::theme::{self, Kind};
use eframe::egui::{self, Align, Align2, Color32, Layout, Order, Rect, RichText, Sense, Stroke, Ui, vec2};
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, Snapshot};
use streamit_core::multiview::{
    MultiView, PRESET_SLOTS, Preset, VIEW_KEYS, VIEWS, preset_name, with_preset, without_preset,
};
use tracing::info;

/// Each view's colour, as on the Stream Deck.
pub(super) const VIEW_COLOURS: [Color32; 4] = [
    Color32::from_rgb(76, 154, 255),
    Color32::from_rgb(245, 165, 36),
    Color32::from_rgb(61, 214, 140),
    Color32::from_rgb(232, 121, 201),
];
/// How many inputs to list before the VideoHub has said.
const DEFAULT_INPUTS: u16 = 40;
const COL_W: f32 = 230.0;
const GAP: f32 = 28.0;
const CHIP_H: f32 = 24.0;
const SLOT_H: f32 = 64.0;
const TILE_MAX_W: f32 = 290.0;

/// An input being dragged onto a view.
struct InputDrag(u16);

/// The window's own MultiView state: nothing here is saved.
#[derive(Default)]
pub(super) struct MvUi {
    /// An input clicked (rather than dragged): clicking a view routes it there.
    picked: Option<u16>,
    /// Save preset was pressed: the next slot clicked is saved.
    saving: bool,
    /// The slot being named, and the name typed so far.
    naming: Option<(u8, String)>,
    /// The slot whose Clear is being confirmed.
    clearing: Option<u8>,
}

/// The inputs listed on the left and on the right: the first half and the rest.
pub(super) fn input_columns(count: u16) -> (std::ops::RangeInclusive<u16>, std::ops::RangeInclusive<u16>) {
    let left = count.div_ceil(2);
    (1..=left, left + 1..=count)
}

/// The views showing `input` now.
pub(super) fn views_on(mv: &MultiView, input: u16) -> Vec<usize> {
    (0..VIEWS.len()).filter(|&v| mv.views[v] == Some(input)).collect()
}

impl StreamItApp {
    pub(super) fn multiview_tab(&mut self, ui: &mut Ui, snap: &Snapshot, outputs: [u16; 4]) {
        let mv = &snap.multiview;
        let unset = outputs.iter().all(|&o| o == 0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("MultiView").font(theme::heading_font(30.0)));
            ui.add_space(16.0);
            let (tone, text) = match snap.device_status.get(&DeviceKind::VideoHub) {
                Some(DeviceHealth::Ok) => (theme::GREEN, "VideoHub connected · the views show what it reports"),
                Some(DeviceHealth::Warning(_)) => (theme::AMBER, "VideoHub slow to answer"),
                Some(DeviceHealth::Down(_)) => (theme::RED, "VideoHub not connected · nothing can be routed"),
                None => (theme::DIM, "Checking the VideoHub…"),
            };
            theme::dot(ui, tone);
            ui.label(RichText::new(text).size(14.0).color(theme::SOFT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("Drag an input onto a view, or click an input and then a view")
                        .size(14.0)
                        .color(theme::MUTED),
                );
            });
        });
        ui.add_space(12.0);
        if unset {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Set which router output feeds each MultiView view first.").color(theme::AMBER_SOFT),
                );
                if theme::button(ui, "Open Settings", Kind::Plain).clicked() {
                    self.tab = Tab::Settings;
                    self.section = Section::Equipment;
                    self.scroll_to = Some(Section::Equipment);
                }
            });
            ui.add_space(12.0);
        }
        let count = if mv.labels.is_empty() { DEFAULT_INPUTS } else { mv.labels.len() as u16 };
        let (left, right) = input_columns(count);
        theme::panel().inner_margin(20).show(ui, |ui| {
            let centre_w = (ui.available_width() - 2.0 * (COL_W + GAP)).max(320.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                ui.allocate_ui_with_layout(vec2(COL_W, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.input_column(ui, mv, left, false);
                });
                ui.allocate_ui_with_layout(vec2(centre_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.view_grid(ui, snap, outputs);
                    ui.add_space(14.0);
                    self.presets_row(ui, mv);
                });
                ui.allocate_ui_with_layout(vec2(COL_W, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.input_column(ui, mv, right, true);
                });
            });
        });
        // The input being dragged follows the pointer.
        if let (Some(d), Some(pos)) =
            (egui::DragAndDrop::payload::<InputDrag>(ui.ctx()), ui.ctx().pointer_interact_pos())
        {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(Order::Tooltip, egui::Id::unique("mv-drag")));
            let text = format!("{} · {}", d.0, mv.label(d.0));
            let galley = painter.layout_no_wrap(text, theme::body_bold(14.0), Color32::WHITE);
            let rect = Rect::from_center_size(pos - vec2(0.0, 28.0), galley.size() + vec2(20.0, 12.0));
            painter.rect_filled(rect, 6.0, theme::BLUE_FILL);
            painter.galley(rect.min + vec2(10.0, 6.0), galley, Color32::WHITE);
        }
    }

    fn input_column(&mut self, ui: &mut Ui, mv: &MultiView, inputs: std::ops::RangeInclusive<u16>, right: bool) {
        ui.spacing_mut().item_spacing.y = 3.0;
        let first = *inputs.start();
        let last = *inputs.end();
        let head = if last >= first { format!("{first}–{last}") } else { String::new() };
        ui.horizontal(|ui| {
            theme::lbl(ui, "Router inputs");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| theme::lbl(ui, &head));
        });
        for n in inputs {
            self.input_chip(ui, mv, n, right);
        }
    }

    fn input_chip(&mut self, ui: &mut Ui, mv: &MultiView, n: u16, right: bool) {
        let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), CHIP_H), Sense::click_and_drag());
        let on = views_on(mv, n);
        let picked = self.mv.picked == Some(n);
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
        // Badges for the views this input is on, on the side nearest the views.
        let badge_w = 26.0;
        let badges_w = on.len() as f32 * (badge_w + 3.0);
        for (i, &v) in on.iter().enumerate() {
            let x = if right {
                rect.left() + 6.0 + i as f32 * (badge_w + 3.0)
            } else {
                rect.right() - 6.0 - badges_w + i as f32 * (badge_w + 3.0)
            };
            let b = Rect::from_min_size(egui::pos2(x, rect.center().y - 8.0), vec2(badge_w, 16.0));
            p.rect_filled(b, 4.0, VIEW_COLOURS[v]);
            p.text(b.center(), Align2::CENTER_CENTER, VIEW_KEYS[v], theme::mono_bold(10.0), Color32::BLACK);
        }
        let room = (rect.width() - 44.0 - 12.0 - badges_w).max(10.0);
        // Only the VideoHub's own label: the number is already on the chip.
        let own = n.checked_sub(1).and_then(|i| mv.labels.get(i as usize)).map_or("", |l| l.trim());
        let galley = ui.painter().layout(own.to_string(), egui::FontId::proportional(13.0), label_color, f32::INFINITY);
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
            self.mv.picked = None;
        }
        if r.clicked() {
            self.mv.picked = if picked { None } else { Some(n) };
        }
        r.on_hover_cursor(egui::CursorIcon::Grab);
    }

    fn view_grid(&mut self, ui: &mut Ui, snap: &Snapshot, outputs: [u16; 4]) {
        let mv = &snap.multiview;
        ui.horizontal(|ui| {
            theme::lbl(ui, "MultiView 4");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new("Each view is fed by one router output").size(13.0).color(theme::MUTED));
            });
        });
        ui.add_space(6.0);
        // 16:9 views, no wider than TILE_MAX_W so the presets below stay on screen.
        let pad = 14.0;
        let w = ui.available_width();
        let tile_w = ((w - 3.0 * pad) / 2.0).min(TILE_MAX_W);
        let tile = vec2(tile_w, tile_w * 9.0 / 16.0);
        let (area, _) = ui.allocate_exact_size(vec2(w, 2.0 * tile.y + 3.0 * pad), Sense::hover());
        let frame = Rect::from_center_size(area.center(), vec2(2.0 * tile.x + 3.0 * pad, area.height()));
        ui.painter().rect(
            frame,
            10.0,
            Color32::from_rgb(7, 9, 12),
            Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        for (v, output) in outputs.into_iter().enumerate() {
            let (col, row) = ((v % 2) as f32, (v / 2) as f32);
            let min = frame.min + vec2(pad + col * (tile.x + pad), pad + row * (tile.y + pad));
            self.view_tile(ui, Rect::from_min_size(min, tile), v, mv, output);
        }
        ui.add_space(8.0);
        if let Some(w) = snap.last_warning.as_deref().filter(|w| w.starts_with("MultiView")) {
            ui.label(RichText::new(w).size(14.0).color(theme::AMBER_SOFT));
        } else if let Some(n) = self.mv.picked {
            ui.label(
                RichText::new(format!("Input {n} ({}) picked. Click a view to show it there.", mv.label(n)))
                    .size(14.0)
                    .color(theme::SOFT),
            );
        }
    }

    fn view_tile(&mut self, ui: &mut Ui, rect: Rect, v: usize, mv: &MultiView, output: u16) {
        let id = ui.scope_id().with(("mv-view", v));
        let r = ui.interact(rect, id, Sense::click());
        let colour = VIEW_COLOURS[v];
        let hovering = r.dnd_hover_payload::<InputDrag>().is_some() || (self.mv.picked.is_some() && r.hovered());
        let fill = if hovering { Color32::from_rgb(0x24, 0x31, 0x4A) } else { theme::SCREEN };
        let p = ui.painter();
        p.rect(rect, 6.0, fill, Stroke::new(2.0, colour), egui::StrokeKind::Inside);
        p.text(
            rect.left_top() + vec2(10.0, 8.0),
            Align2::LEFT_TOP,
            format!("{} · MV in {}", VIEWS[v].to_uppercase(), v + 1),
            theme::heading_font(12.0),
            colour,
        );
        let out = if output == 0 { "Router output not set".to_string() } else { format!("Router out {output}") };
        p.text(rect.right_bottom() - vec2(10.0, 8.0), Align2::RIGHT_BOTTOM, out, theme::mono(11.0), theme::DIM);
        let (small, big) = match mv.views[v] {
            Some(n) => (format!("INPUT {n}"), mv.label(n)),
            None if output == 0 => (String::new(), "Not set up".to_string()),
            None => (String::new(), "Not known yet".to_string()),
        };
        let mut job = egui::text::LayoutJob { halign: Align::Center, ..Default::default() };
        job.wrap.max_width = rect.width() - 28.0;
        if !small.is_empty() {
            job.append(&small, 0.0, egui::TextFormat::simple(theme::mono_bold(13.0), colour));
            job.append("\n", 0.0, egui::TextFormat::simple(theme::mono_bold(6.0), colour));
        }
        job.append(&big, 0.0, egui::TextFormat::simple(theme::heading_font(24.0), theme::SCREEN_TEXT));
        let galley = p.layout_job(job);
        let top = rect.center().y - galley.size().y / 2.0;
        p.with_clip_rect(rect.shrink(4.0)).galley(egui::pos2(rect.center().x, top), galley, theme::SCREEN_TEXT);

        let dropped = r.dnd_release_payload::<InputDrag>().map(|d| d.0);
        let clicked = r.clicked().then_some(self.mv.picked).flatten();
        if let Some(input) = dropped.or(clicked) {
            self.mv.picked = None;
            info!(view = VIEWS[v], input, "MultiView: routing from the window");
            self.send(Command::RouteView { view: v, input });
        }
    }

    // ---- presets

    fn presets_row(&mut self, ui: &mut Ui, mv: &MultiView) {
        ui.horizontal(|ui| {
            theme::lbl(ui, "Presets");
            ui.add_space(10.0);
            let msg = if self.mv.saving {
                "Pick the slot to save the four views in. A used slot is replaced."
            } else {
                "Click a preset to put its four inputs back on the views."
            };
            ui.label(RichText::new(msg).size(14.0).color(theme::MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, kind) =
                    if self.mv.saving { ("Cancel", Kind::Plain) } else { ("Save preset", Kind::Primary) };
                if theme::button(ui, text, kind).clicked() {
                    self.mv.saving = !self.mv.saving;
                    self.mv.naming = None;
                    self.mv.clearing = None;
                }
            });
        });
        ui.add_space(8.0);
        // Two rows of four, under the views.
        let (gap, per_row) = (8.0, PRESET_SLOTS / 2);
        let w = (ui.available_width() - gap * (per_row as f32 - 1.0)) / per_row as f32;
        ui.spacing_mut().item_spacing.y = gap;
        for row in 0..2 {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for slot in row * per_row + 1..=(row + 1) * per_row {
                    ui.allocate_ui_with_layout(vec2(w, SLOT_H), Layout::top_down(Align::Min), |ui| {
                        ui.set_min_size(vec2(w, SLOT_H));
                        self.preset_slot(ui, mv, slot);
                    });
                }
            });
        }
    }

    fn preset_slot(&mut self, ui: &mut Ui, mv: &MultiView, slot: u8) {
        let preset = mv.preset(slot).cloned();
        if let Some((s, _)) = &self.mv.naming
            && *s == slot
        {
            return self.preset_namer(ui, mv, slot);
        }
        if self.mv.clearing == Some(slot)
            && let Some(p) = &preset
        {
            return self.preset_clear_confirm(ui, mv, p);
        }
        let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), SLOT_H));
        let r =
            ui.allocate_rect(rect, if preset.is_some() || self.mv.saving { Sense::click() } else { Sense::hover() });
        let current = preset.as_ref().is_some_and(|p| mv.is_current(p)) && !self.mv.saving;
        let (fill, border) = match (&preset, self.mv.saving, current) {
            (_, true, _) => (theme::AMBER_DARK, theme::AMBER),
            (Some(_), false, true) => (theme::INSET, theme::GREEN),
            (Some(_), false, false) => (theme::INSET, if r.hovered() { theme::BLUE } else { theme::BUTTON_BORDER }),
            (None, false, _) => (theme::GROUND, theme::BORDER),
        };
        let p = ui.painter();
        p.rect(rect, 8.0, fill, Stroke::new(if current { 2.0 } else { 1.0 }, border), egui::StrokeKind::Inside);
        let num_colour = if current { theme::GREEN } else { theme::DIM };
        p.text(
            rect.right_bottom() - vec2(8.0, 6.0),
            Align2::RIGHT_BOTTOM,
            slot.to_string(),
            theme::mono_bold(11.0),
            num_colour,
        );
        match &preset {
            Some(pr) => {
                let name = p.layout(pr.name.clone(), theme::heading_font(17.0), theme::TEXT, rect.width() - 34.0);
                p.with_clip_rect(rect.shrink(2.0)).galley(rect.left_top() + vec2(10.0, 12.0), name, theme::TEXT);
                for (v, input) in pr.inputs.iter().enumerate() {
                    let b =
                        Rect::from_min_size(rect.left_bottom() + vec2(10.0 + v as f32 * 30.0, -26.0), vec2(26.0, 16.0));
                    p.rect_filled(b, 4.0, VIEW_COLOURS[v]);
                    p.text(
                        b.center(),
                        Align2::CENTER_CENTER,
                        input.to_string(),
                        theme::mono_bold(10.0),
                        Color32::BLACK,
                    );
                }
            }
            None => {
                let text = if self.mv.saving { "Save here" } else { "Empty" };
                let colour = if self.mv.saving { theme::TEXT } else { theme::DIM };
                p.text(
                    rect.left_center() + vec2(10.0, 0.0),
                    Align2::LEFT_CENTER,
                    text,
                    theme::heading_font(16.0),
                    colour,
                );
            }
        }
        // × clears the preset (after asking). It never changes what the views show.
        if let Some(pr) = &preset
            && !self.mv.saving
        {
            // The × is part of the slot: a click on it never also recalls the preset.
            let x = Rect::from_min_size(rect.right_top() + vec2(-30.0, 4.0), vec2(26.0, 26.0));
            let over = ui.rect_contains_pointer(x);
            let colour = if over { theme::TEXT } else { theme::DIM };
            ui.painter().text(x.center(), Align2::CENTER_CENTER, "×", egui::FontId::proportional(18.0), colour);
            if r.clicked() && over {
                info!(slot, name = %pr.name, "asking before clearing a preset");
                self.mv.clearing = Some(slot);
                return;
            }
        }
        if r.clicked() {
            if self.mv.saving {
                if mv.views.iter().all(Option::is_some) {
                    let name = preset.map(|p| p.name).unwrap_or_default();
                    self.mv.naming = Some((slot, name));
                } else {
                    self.notice = Some("Not saved: the VideoHub has not said what every view shows yet.".into());
                    self.mv.saving = false;
                }
            } else if preset.is_some() {
                self.send(Command::RecallPreset(slot));
            }
        }
    }

    fn preset_namer(&mut self, ui: &mut Ui, mv: &MultiView, slot: u8) {
        slot_box(ui, theme::AMBER_DARK, theme::AMBER, |ui| {
            let Some((_, name)) = self.mv.naming.as_mut() else { return };
            let edit = ui.add(
                egui::TextEdit::singleline(name)
                    .id(egui::Id::unique(("preset-name", slot)))
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
                self.mv.naming = None;
                self.mv.saving = false;
            } else if save {
                let typed = self.mv.naming.take().map(|(_, n)| n).unwrap_or_default();
                self.mv.saving = false;
                let inputs = mv.views.map(|v| v.unwrap_or(0));
                if inputs.contains(&0) {
                    self.notice = Some("Not saved: the VideoHub has not said what every view shows yet.".into());
                    return;
                }
                let p = Preset { slot, name: preset_name(&typed, slot), inputs };
                info!(slot, name = %p.name, "MultiView preset saved");
                self.save_presets(with_preset(&mv.presets, p));
            }
        });
    }

    fn preset_clear_confirm(&mut self, ui: &mut Ui, mv: &MultiView, p: &Preset) {
        slot_box(ui, Color32::from_rgb(0x2A, 0x12, 0x14), theme::RED, |ui| {
            ui.add(egui::Label::new(RichText::new(format!("Clear {}?", p.name)).strong()).truncate())
                .on_hover_text("Clears the preset and its name. The views stay as they are.");
            ui.horizontal(|ui| {
                if ui.small_button("Keep").clicked() {
                    self.mv.clearing = None;
                }
                if ui.small_button(RichText::new("Clear").color(theme::RED)).clicked() {
                    self.mv.clearing = None;
                    info!(slot = p.slot, name = %p.name, "MultiView preset cleared");
                    self.save_presets(without_preset(&mv.presets, p.slot));
                }
            });
        });
    }
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
    fn inputs_split_down_both_sides() {
        assert_eq!(input_columns(40), (1..=20, 21..=40));
        assert_eq!(input_columns(16), (1..=8, 9..=16));
        assert_eq!(input_columns(9), (1..=5, 6..=9));
        let (l, r) = input_columns(0);
        assert!(l.is_empty() && r.is_empty());
    }

    #[test]
    fn an_input_can_be_on_several_views() {
        let mv = MultiView { views: [Some(3), None, Some(3), Some(4)], ..MultiView::default() };
        assert_eq!(views_on(&mv, 3), vec![0, 2]);
        assert_eq!(views_on(&mv, 4), vec![3]);
        assert!(views_on(&mv, 9).is_empty());
    }
}
