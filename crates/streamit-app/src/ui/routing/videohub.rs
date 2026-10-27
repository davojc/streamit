//! The VideoHub sub-tab: every input down the left and every output down the right, one
//! column each. Clicking an input shows the outputs it feeds (with a line to each);
//! clicking an output shows its input. Dragging an input onto an output routes it.
//! Double-clicking a name edits it in place; it goes to the VideoHub on Enter (or a click
//! away) and Escape puts it back. A row's menu notes its cable run, shown under its name and
//! kept only in StreamIt. A VideoHub output always shows some input, so
//! disconnecting one routes the input marked as blank to it.
use super::super::StreamItApp;
use super::{CLEAR_W, InputDrag, cleared_label, colour, label_to_send, port_count, setup};
use crate::theme;
use eframe::egui::{self, Align2, Color32, Rect, RichText, Sense, Stroke, Ui, vec2};
use streamit_core::devices::LabelKind;
use streamit_core::engine::{Command, Snapshot};
use streamit_core::routing::RoutingState;
use tracing::info;

const ROW_H: f32 = 30.0;
/// A row with its cable run under its name.
const CABLE_ROW_H: f32 = 42.0;
const ROW_GAP: f32 = 4.0;
/// The gap between the columns, where the lines run.
const LINK_W: f32 = 140.0;
const NUM_W: f32 = 30.0;
/// Room on an output row for its input ("← 12 Camera 1 Wide").
const SRC_W: f32 = 190.0;
const BLANK: Color32 = Color32::from_rgb(150, 156, 168);

/// A row picked on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum Pick {
    Input(u16),
    Output(u16),
}

/// What a pick lights up: the inputs and outputs, and a line for each (input, output).
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::ui) struct Lit {
    pub inputs: Vec<u16>,
    pub outputs: Vec<u16>,
    pub links: Vec<(u16, u16)>,
}

pub(in crate::ui) fn lit(routing: &RoutingState, pick: Option<Pick>) -> Lit {
    match pick {
        Some(Pick::Input(n)) => {
            let outputs = routing.outputs_of(n);
            Lit { links: outputs.iter().map(|&o| (n, o)).collect(), inputs: vec![n], outputs }
        }
        Some(Pick::Output(o)) => match routing.route(o) {
            Some(n) => Lit { inputs: vec![n], outputs: vec![o], links: vec![(n, o)] },
            None => Lit { outputs: vec![o], ..Lit::default() },
        },
        None => Lit::default(),
    }
}

/// The input that disconnects output `o`: the blank input, unless none is marked or the
/// output already shows it.
pub(in crate::ui) fn disconnect_input(routing: &RoutingState, o: u16, blank: u16) -> Option<u16> {
    (blank != 0 && routing.route(o) != Some(blank)).then_some(blank)
}

/// "→ 3 outputs" under an input; "not routed" when it feeds none.
pub(in crate::ui) fn feeds_text(count: usize) -> String {
    match count {
        0 => "not routed".into(),
        1 => "→ 1 output".into(),
        n => format!("→ {n} outputs"),
    }
}

impl StreamItApp {
    pub(super) fn videohub_page(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let routing = &snap.routing;
        if ui.input(|i| i.key_pressed(egui::Key::Escape))
            && self.routes_ui.vh_editing.is_none()
            && self.routes_ui.vh_cable.is_none()
        {
            self.routes_ui.vh_pick = None;
        }
        let lit = lit(routing, self.routes_ui.vh_pick);
        theme::panel().inner_margin(20).show(ui, |ui| {
            theme::lbl(ui, "VideoHub");
            ui.label(
                RichText::new(
                    "Click an input to see every output it feeds; click an output to see its input. Drag an input \
                     onto an output to route it. Double-click a name to rename it (Enter sends it to the VideoHub, \
                     Escape puts it back); × clears it to its number. Right-click an input to mark it as the \
                     video source or the blank input; right-click an output to disconnect it (it then shows the \
                     blank input). Right-click either to note its cable run, shown under its name and kept only \
                     in StreamIt.",
                )
                .color(theme::SOFT),
            );
            if let Some(w) = snap.last_warning.as_deref().filter(|w| w.starts_with("VideoHub")) {
                ui.label(RichText::new(w).size(14.0).color(theme::AMBER_SOFT));
            }
            ui.add_space(12.0);
            let col_w = (ui.available_width() - LINK_W) / 2.0;
            let (inputs, outputs) = (port_count(&routing.labels), port_count(&routing.output_labels));
            let mut in_rects = Vec::new();
            let mut out_rects = Vec::new();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(vec2(col_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = ROW_GAP;
                    header(ui, "Inputs", &format!("1–{inputs}"));
                    for n in 1..=inputs {
                        in_rects.push(self.input_row(ui, snap, n, &lit));
                    }
                });
                ui.add_space(LINK_W);
                ui.allocate_ui_with_layout(vec2(col_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = ROW_GAP;
                    header(ui, "Outputs · shows ← input · feeds", &format!("1–{outputs}"));
                    for o in 1..=outputs {
                        out_rects.push(self.output_row(ui, snap, o, &lit));
                    }
                });
            });
            let stroke = Stroke::new(2.0, theme::BLUE);
            for &(n, o) in &lit.links {
                let (Some(a), Some(b)) = (in_rects.get(usize::from(n) - 1), out_rects.get(usize::from(o) - 1)) else {
                    continue;
                };
                let (p0, p3) = (a.right_center(), b.left_center());
                let mid = (p3.x - p0.x) / 2.0;
                let curve = egui::epaint::CubicBezierShape::from_points_stroke(
                    [p0, p0 + vec2(mid, 0.0), p3 - vec2(mid, 0.0), p3],
                    false,
                    Color32::TRANSPARENT,
                    stroke,
                );
                ui.painter().add(curve);
            }
        });
        super::drag_ghost(ui, routing);
        self.on_air_question(ui.ctx());
    }

    fn input_row(&mut self, ui: &mut Ui, snap: &Snapshot, n: u16, lit: &Lit) -> Rect {
        let routing = &snap.routing;
        let cable = self.cable_of(LabelKind::Input, n);
        let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), row_h(&cable)), Sense::click_and_drag());
        let picked = self.routes_ui.vh_pick == Some(Pick::Input(n));
        let shown = lit.inputs.contains(&n);
        let (source, blank) = {
            let c = self.config.read().unwrap_or_else(|e| e.into_inner());
            (n == c.videohub.decklink_input, n == c.videohub.blank_input)
        };
        row_frame(ui, rect, picked, shown, false);
        number(ui, rect.left_center() + vec2(NUM_W - 6.0, 0.0), n);
        let count = routing.outputs_of(n).len();
        let right = rect.right() - CLEAR_W;
        let feeds = feeds_text(count);
        let feeds_colour = if count == 0 { theme::DIM } else { theme::MUTED };
        let g = ui.painter().layout_no_wrap(feeds, theme::mono(11.5), feeds_colour);
        let feeds_x = right - g.size().x - 8.0;
        ui.painter().galley(egui::pos2(feeds_x, rect.center().y - g.size().y / 2.0), g, feeds_colour);
        let mut name_right = feeds_x - 8.0;
        if source {
            name_right -= 58.0;
            setup::badge(ui, egui::pos2(name_right + 4.0, rect.center().y - 8.0), "VIDEOS", setup::MEDIA);
        }
        if blank {
            name_right -= 54.0;
            setup::badge(ui, egui::pos2(name_right + 4.0, rect.center().y - 8.0), "BLANK", BLANK);
        }
        let name = Rect::from_min_max(rect.min + vec2(NUM_W + 8.0, 3.0), egui::pos2(name_right, rect.bottom() - 3.0));
        let name = self.cable_cell(ui, LabelKind::Input, n, &cable, name);
        self.name_cell(ui, routing, LabelKind::Input, n, name);
        self.clear_button(ui, routing, LabelKind::Input, n, rect);
        if r.drag_started() {
            r.dnd_set_drag_payload(InputDrag(n));
        }
        r.context_menu(|ui| {
            ui.label(RichText::new(format!("ROUTER INPUT {n}")).size(11.0).color(theme::MUTED));
            if ui
                .add_enabled(!source, egui::Button::new("Mark as the video source"))
                .on_hover_text(
                    "The DeckLink player's output. StreamIt routes this input to the Media input for a video.",
                )
                .clicked()
            {
                info!(input = n, "VideoHub: video source marked");
                self.save_setup(move |c| c.videohub.decklink_input = n);
                ui.close();
            }
            let (text, mark) = if blank { ("Unmark as the blank input", 0) } else { ("Mark as the blank input", n) };
            if ui
                .button(text)
                .on_hover_text("An unused (black) input. Disconnecting an output routes this input to it.")
                .clicked()
            {
                info!(input = mark, "VideoHub: blank input marked");
                self.save_setup(move |c| c.videohub.blank_input = mark);
                ui.close();
            }
            self.cable_menu_item(ui, LabelKind::Input, n, &cable);
        });
        self.pick_or_edit(&r, Pick::Input(n), LabelKind::Input, n, routing);
        rect
    }

    fn output_row(&mut self, ui: &mut Ui, snap: &Snapshot, o: u16, lit: &Lit) -> Rect {
        let routing = &snap.routing;
        let cable = self.cable_of(LabelKind::Output, o);
        let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), row_h(&cable)), Sense::click());
        let picked = self.routes_ui.vh_pick == Some(Pick::Output(o));
        let shown = lit.outputs.contains(&o);
        let dropping = r.dnd_hover_payload::<InputDrag>().is_some();
        row_frame(ui, rect, picked, shown, dropping);
        number(ui, rect.right_center() - vec2(8.0, 0.0), o);
        // What it feeds, left of its number.
        let fed = routing.fed_by(o);
        let mut x = rect.right() - NUM_W - 6.0;
        for &(t, i) in fed.iter().rev() {
            x -= 26.0;
            let b = Rect::from_min_size(egui::pos2(x, rect.center().y - 8.0), vec2(24.0, 16.0));
            ui.painter().rect_filled(b, 4.0, colour(t, i));
            ui.painter().text(b.center(), Align2::CENTER_CENTER, t.dest_key(i), theme::mono_bold(10.0), Color32::BLACK);
        }
        // Its input, after the ×.
        let src_rect = Rect::from_min_size(rect.min + vec2(CLEAR_W + 4.0, 0.0), vec2(SRC_W, rect.height()));
        let blank = self.config.read().unwrap_or_else(|e| e.into_inner()).videohub.blank_input;
        let (src, src_colour) = match routing.route(o) {
            Some(n) if n == blank => ("— disconnected".to_string(), theme::DIM),
            Some(n) => (
                format!("← {n} {}", routing.own_label(n).unwrap_or("")),
                if shown { theme::BLUE } else { theme::MUTED },
            ),
            None => ("—".to_string(), theme::DIM),
        };
        let g = ui.painter().layout_no_wrap(src, theme::mono(11.5), src_colour);
        ui.painter().with_clip_rect(src_rect).galley(
            egui::pos2(src_rect.left(), rect.center().y - g.size().y / 2.0),
            g,
            src_colour,
        );
        let name = Rect::from_min_max(
            egui::pos2(src_rect.right() + 6.0, rect.top() + 3.0),
            egui::pos2(x - 6.0, rect.bottom() - 3.0),
        );
        let name = self.cable_cell(ui, LabelKind::Output, o, &cable, name);
        self.name_cell(ui, routing, LabelKind::Output, o, name);
        self.clear_button(
            ui,
            routing,
            LabelKind::Output,
            o,
            Rect::from_min_size(rect.min, vec2(CLEAR_W, rect.height())),
        );
        if let Some(d) = r.dnd_release_payload::<InputDrag>() {
            self.route_output(snap, o, d.0);
        }
        r.context_menu(|ui| {
            ui.label(RichText::new(format!("ROUTER OUTPUT {o}")).size(11.0).color(theme::MUTED));
            let to = disconnect_input(routing, o, blank);
            let hint = match (blank, to) {
                (0, _) => "Mark an unused input as the blank input first (right-click an input).".to_string(),
                (_, None) => "It already shows the blank input.".to_string(),
                (b, Some(_)) => format!("Routes the blank input ({b}) to it."),
            };
            if let Some(b) = ui
                .add_enabled(to.is_some(), egui::Button::new("Disconnect"))
                .on_hover_text(hint)
                .clicked()
                .then_some(to)
                .flatten()
            {
                info!(output = o, blank = b, "VideoHub: disconnecting an output");
                self.route_output(snap, o, b);
                ui.close();
            }
            self.cable_menu_item(ui, LabelKind::Output, o, &cable);
        });
        self.pick_or_edit(&r, Pick::Output(o), LabelKind::Output, o, routing);
        rect
    }

    /// Routes `input` to `output`. An output feeding a destination goes through that
    /// destination, so an ATEM input on program asks first and a locked one is refused.
    fn route_output(&mut self, snap: &Snapshot, output: u16, input: u16) {
        if snap.routing.route(output) == Some(input) {
            return;
        }
        match snap.routing.fed_by(output).first() {
            Some(&(t, d)) => self.request_routes(snap, t, None, vec![(d, input)]),
            None => {
                info!(output, input, "routing from the VideoHub page");
                self.send(Command::RouteOutput { output, input });
            }
        }
    }

    /// A click picks the row (again clears it); a double-click starts editing its name.
    fn pick_or_edit(&mut self, r: &egui::Response, pick: Pick, kind: LabelKind, n: u16, routing: &RoutingState) {
        if r.double_clicked() {
            self.routes_ui.vh_pick = Some(pick);
            let current = label_of(routing, kind, n);
            self.routes_ui.drafts.insert((kind, n), current);
            self.routes_ui.vh_editing = Some((kind, n));
        } else if r.clicked() {
            self.routes_ui.vh_pick = if self.routes_ui.vh_pick == Some(pick) { None } else { Some(pick) };
        }
    }

    /// The name, or (while it is being edited) the field to type it in.
    fn name_cell(&mut self, ui: &mut Ui, routing: &RoutingState, kind: LabelKind, n: u16, rect: Rect) {
        let current = label_of(routing, kind, n);
        if self.routes_ui.vh_editing != Some((kind, n)) {
            let (text, c) =
                if current.is_empty() { ("No label".to_string(), theme::DIM) } else { (current, theme::TEXT) };
            let g = ui.painter().layout_no_wrap(text, egui::FontId::proportional(13.5), c);
            ui.painter().with_clip_rect(rect).galley(egui::pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, c);
            return;
        }
        let key = (kind, n);
        let mut text = self.routes_ui.drafts.get(&key).cloned().unwrap_or_else(|| current.clone());
        let r = ui.put(
            rect,
            egui::TextEdit::singleline(&mut text)
                .id(egui::Id::unique(("vh-edit", kind == LabelKind::Output, n)))
                .font(egui::FontId::proportional(13.5))
                .margin(vec2(6.0, 1.0))
                .desired_width(f32::INFINITY)
                .char_limit(40),
        );
        if !r.has_focus() && !r.lost_focus() {
            r.request_focus();
        }
        self.routes_ui.drafts.insert(key, text.clone());
        if r.lost_focus() {
            let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
            self.routes_ui.drafts.remove(&key);
            self.routes_ui.vh_editing = None;
            if let Some(name) = label_to_send(&text, &current, escaped) {
                info!(?kind, n, name = %name, "VideoHub: renaming from the window");
                self.send(Command::SetLabel { kind, n, text: name });
            }
        }
    }

    /// × at the row's right (an input) or left (an output): clears the name to its number.
    fn clear_button(&mut self, ui: &mut Ui, routing: &RoutingState, kind: LabelKind, n: u16, row: Rect) {
        let cleared = cleared_label(n);
        let at = if kind == LabelKind::Input {
            row.right_center() - vec2(CLEAR_W / 2.0, 0.0)
        } else {
            row.left_center() + vec2(CLEAR_W / 2.0, 0.0)
        };
        let r = ui.put(
            Rect::from_center_size(at, vec2(20.0, 20.0)),
            egui::Button::new(RichText::new("×").size(14.0).color(theme::MUTED)).frame(false).small(),
        );
        if label_of(routing, kind, n) != cleared
            && r.on_hover_text(format!("Clear the label: it becomes {cleared}")).clicked()
        {
            info!(?kind, n, "VideoHub: clearing a label from the window");
            self.routes_ui.drafts.remove(&(kind, n));
            self.routes_ui.vh_editing = None;
            self.send(Command::SetLabel { kind, n, text: cleared });
        }
    }
}

/// A row's cable run, as shown under its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum Cable {
    None,
    Noted(String),
    Editing,
}

/// Taller with a cable run (or its field) under the name.
fn row_h(cable: &Cable) -> f32 {
    if *cable == Cable::None { ROW_H } else { CABLE_ROW_H }
}

impl StreamItApp {
    fn cable_of(&self, kind: LabelKind, n: u16) -> Cable {
        if self.routes_ui.vh_cable == Some((kind, n)) {
            return Cable::Editing;
        }
        let c = self.config.read().unwrap_or_else(|e| e.into_inner());
        c.videohub.cable(kind, n).map_or(Cable::None, |r| Cable::Noted(r.to_string()))
    }

    fn cable_menu_item(&mut self, ui: &mut Ui, kind: LabelKind, n: u16, cable: &Cable) {
        let text = if matches!(cable, Cable::Noted(_)) { "Edit the cable run…" } else { "Cable run…" };
        if ui
            .button(text)
            .on_hover_text("Which cable this is, to tell it apart. Kept in StreamIt only, not on the VideoHub.")
            .clicked()
        {
            self.routes_ui.vh_cable_draft = match cable {
                Cable::Noted(r) => r.clone(),
                _ => String::new(),
            };
            self.routes_ui.vh_cable = Some((kind, n));
            ui.close();
        }
    }

    /// Draws the cable run (or its field) on the row's second line; returns the room left
    /// for the name above it. Enter (or a click away) saves the field, Escape leaves it as
    /// it was; saving it blank removes the cable run.
    fn cable_cell(&mut self, ui: &mut Ui, kind: LabelKind, n: u16, cable: &Cable, name: Rect) -> Rect {
        if *cable == Cable::None {
            return name;
        }
        let split = name.top() + 21.0;
        let line = Rect::from_min_max(egui::pos2(name.left(), split), name.max);
        match cable {
            Cable::Noted(run) => {
                let text = format!("Cable: {run}");
                let g = ui.painter().layout_no_wrap(text, egui::FontId::proportional(12.0), theme::MUTED);
                ui.painter().with_clip_rect(line).galley(
                    egui::pos2(line.left(), line.center().y - g.size().y / 2.0),
                    g,
                    theme::MUTED,
                );
            }
            Cable::Editing => {
                let mut text = std::mem::take(&mut self.routes_ui.vh_cable_draft);
                let r = ui.put(
                    line,
                    egui::TextEdit::singleline(&mut text)
                        .id(egui::Id::unique(("vh-cable", kind == LabelKind::Output, n)))
                        .hint_text("Cable run, e.g. C-104 · stage left box 2")
                        .font(egui::FontId::proportional(12.0))
                        .margin(vec2(6.0, 0.0))
                        .desired_width(f32::INFINITY)
                        .char_limit(60),
                );
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                if r.lost_focus() {
                    self.routes_ui.vh_cable = None;
                    if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        info!(?kind, n, run = %text, "VideoHub: cable run noted");
                        self.save_setup(move |c| c.videohub.set_cable(kind, n, &text));
                    }
                } else {
                    self.routes_ui.vh_cable_draft = text;
                }
            }
            Cable::None => {}
        }
        Rect::from_min_max(name.min, egui::pos2(name.right(), split))
    }
}

fn label_of(routing: &RoutingState, kind: LabelKind, n: u16) -> String {
    n.checked_sub(1).and_then(|i| routing.labels_of(kind).get(usize::from(i))).cloned().unwrap_or_default()
}

fn header(ui: &mut Ui, title: &str, range: &str) {
    ui.horizontal(|ui| {
        theme::lbl(ui, title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| theme::lbl(ui, range));
    });
}

fn number(ui: &Ui, right_center: egui::Pos2, n: u16) {
    ui.painter().text(right_center, Align2::RIGHT_CENTER, n.to_string(), theme::mono_bold(12.0), theme::DIM);
}

/// A row's box: blue when picked or lit by the pick, green while an input is dragged over it.
fn row_frame(ui: &Ui, rect: Rect, picked: bool, shown: bool, dropping: bool) {
    let (fill, border, width) = if dropping {
        (Color32::from_rgb(0x13, 0x25, 0x1C), theme::GREEN, 2.0)
    } else if picked {
        (Color32::from_rgb(0x18, 0x23, 0x3A), theme::BLUE, 2.0)
    } else if shown {
        (Color32::from_rgb(0x14, 0x1D, 0x2E), theme::BLUE, 1.0)
    } else {
        (theme::INSET, theme::BORDER, 1.0)
    };
    ui.painter().rect(rect, 6.0, fill, Stroke::new(width, border), egui::StrokeKind::Inside);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routing() -> RoutingState {
        RoutingState { routes: vec![1, 2, 0, 1, 5], ..RoutingState::default() }
    }

    #[test]
    fn an_input_lights_every_output_it_feeds() {
        let l = lit(&routing(), Some(Pick::Input(1)));
        assert_eq!(l, Lit { inputs: vec![1], outputs: vec![1, 4], links: vec![(1, 1), (1, 4)] });
    }

    #[test]
    fn an_output_lights_its_input_and_an_unrouted_one_only_itself() {
        assert_eq!(
            lit(&routing(), Some(Pick::Output(5))),
            Lit { inputs: vec![5], outputs: vec![5], links: vec![(5, 5)] }
        );
        assert_eq!(lit(&routing(), Some(Pick::Output(3))), Lit { outputs: vec![3], ..Lit::default() });
        assert_eq!(lit(&routing(), None), Lit::default());
    }

    #[test]
    fn disconnecting_routes_the_blank_input_once_one_is_marked() {
        assert_eq!(disconnect_input(&routing(), 1, 9), Some(9));
        assert_eq!(disconnect_input(&routing(), 1, 0), None, "no blank input marked");
        assert_eq!(disconnect_input(&routing(), 5, 5), None, "already shows it");
        assert_eq!(disconnect_input(&routing(), 3, 9), Some(9), "an output whose input is unknown");
    }

    #[test]
    fn a_row_with_a_cable_run_is_taller() {
        assert_eq!(row_h(&Cable::None), ROW_H);
        assert_eq!(row_h(&Cable::Noted("C-104".into())), CABLE_ROW_H);
        assert_eq!(row_h(&Cable::Editing), CABLE_ROW_H);
    }

    #[test]
    fn an_input_says_how_many_outputs_it_feeds() {
        assert_eq!(feeds_text(0), "not routed");
        assert_eq!(feeds_text(1), "→ 1 output");
        assert_eq!(feeds_text(3), "→ 3 outputs");
    }
}
