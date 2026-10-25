//! The side column on Prepare, the countdown and End Service: the stage's steps as circles
//! joined top to bottom, grey until done, green when done (amber now, red failed).
use super::*;
use eframe::egui::{Color32, Pos2, Stroke};
use streamit_core::steps::StepGroup;

/// Width of the circles' column.
const RAIL: f32 = 30.0;
const HEAD_R: f32 = 15.0;
const STEP_R: f32 = 11.0;
const LINE_GREY: Color32 = theme::BUTTON_BORDER;

/// A circle's centre, its radius and whether it is done (so the line below it can go green).
struct Circle {
    centre: Pos2,
    r: f32,
    tone: Tone,
}

pub(super) fn steps_panel(ui: &mut Ui, snap: &Snapshot, c: &Config, group: StepGroup) {
    let rows = view::step_rows(snap, c, group);
    theme::panel().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            theme::lbl(ui, "Steps");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let done = rows.iter().filter(|r| r.tone == Tone::Green).count();
                ui.label(RichText::new(format!("{done} of {} done", rows.len())).size(14.0).color(theme::MUTED));
            });
        });
        ui.add_space(10.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut prev: Option<Circle> = None;
        let head = row(ui, view::step_heading(group), None, None, view::steps_tone(&rows), true);
        join(ui, &mut prev, head);
        for r in &rows {
            let c = row(ui, r.label, r.detail.as_deref(), r.time.as_deref(), r.tone, false);
            join(ui, &mut prev, c);
        }
    });
}

/// Draws the line from the circle above to `next`: green when both are done (or the
/// lower one is under way), grey otherwise.
fn join(ui: &Ui, prev: &mut Option<Circle>, next: Circle) {
    if let Some(p) = prev.as_ref() {
        let green = p.tone == Tone::Green && matches!(next.tone, Tone::Green | Tone::Amber);
        let color = if green { theme::GREEN } else { LINE_GREY };
        let a = Pos2::new(p.centre.x, p.centre.y + p.r + 2.0);
        let b = Pos2::new(next.centre.x, next.centre.y - next.r - 2.0);
        if b.y > a.y {
            ui.painter().line_segment([a, b], Stroke::new(2.0, color));
        }
    }
    *prev = Some(next);
}

/// One row: the text beside the rail, then its circle drawn level with the first line.
fn row(ui: &mut Ui, label: &str, detail: Option<&str>, time: Option<&str>, tone: Tone, heading: bool) -> Circle {
    let w = ui.available_width();
    let time_w = if time.is_some() { 96.0 } else { 0.0 };
    let text_w = (w - RAIL - 26.0 - time_w).max(80.0);
    let r = if heading { HEAD_R } else { STEP_R };
    let top = ui.cursor().top();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.add_space(RAIL + 14.0);
        ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(text_w);
            ui.spacing_mut().item_spacing.y = 2.0;
            // Level the first line with the circle.
            ui.add_space(if heading { 4.0 } else { 1.0 });
            let color = match tone {
                Tone::Grey => theme::DIM,
                Tone::Amber => theme::AMBER_SOFT,
                Tone::Red => theme::RED_TEXT,
                Tone::Green => theme::TEXT,
            };
            let text = if heading {
                RichText::new(label.to_uppercase()).font(theme::heading_font(22.0)).color(color)
            } else {
                RichText::new(label).size(16.0).color(color)
            };
            ui.add(egui::Label::new(text).wrap());
            if let Some(d) = detail {
                let c = match tone {
                    Tone::Amber => theme::AMBER,
                    Tone::Red => theme::RED_TEXT,
                    _ => theme::DIM,
                };
                ui.add(egui::Label::new(RichText::new(d).size(13.0).color(c)).wrap());
            }
            ui.add_space(if heading { 12.0 } else { 14.0 });
        });
        if let Some(t) = time {
            ui.label(RichText::new(t).font(theme::mono(12.0)).color(theme::DIM));
        }
    });
    let x = ui.min_rect().left() + RAIL / 2.0;
    let centre = Pos2::new(x, top + r);
    circle(ui, centre, r, tone);
    Circle { centre, r, tone }
}

fn circle(ui: &Ui, centre: Pos2, r: f32, tone: Tone) {
    let p = ui.painter();
    match tone {
        Tone::Green => {
            p.circle_filled(centre, r, theme::GREEN);
            tick(ui, centre, r);
        }
        Tone::Red => {
            p.circle_filled(centre, r, theme::RED);
            let k = r * 0.38;
            let s = Stroke::new(2.4, theme::GROUND);
            p.line_segment([centre + vec2(-k, -k), centre + vec2(k, k)], s);
            p.line_segment([centre + vec2(k, -k), centre + vec2(-k, k)], s);
        }
        Tone::Amber => {
            p.circle_filled(centre, r, theme::AMBER_DARK);
            p.circle_stroke(centre, r - 1.0, Stroke::new(2.0, theme::AMBER));
            p.circle_filled(centre, r * 0.36, theme::AMBER);
        }
        Tone::Grey => {
            p.circle_filled(centre, r, theme::BUTTON);
            p.circle_stroke(centre, r - 1.0, Stroke::new(2.0, LINE_GREY));
        }
    }
}

fn tick(ui: &Ui, centre: Pos2, r: f32) {
    let k = r * 0.45;
    let pts = [centre + vec2(-k, 0.05 * r), centre + vec2(-0.25 * k, 0.75 * k), centre + vec2(k, -0.6 * k)];
    ui.painter().line_segment([pts[0], pts[1]], Stroke::new(2.4, theme::GROUND));
    ui.painter().line_segment([pts[1], pts[2]], Stroke::new(2.4, theme::GROUND));
}
