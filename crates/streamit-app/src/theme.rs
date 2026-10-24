//! The control-room look: the palette, the bundled fonts, and the few widgets every
//! screen is built from (badge, panel, step strip, simulation banner, big buttons).
use crate::view::{HeaderView, StepState, Tone};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, Margin, Response,
    RichText, Sense, Shape, Stroke, Ui, pos2, vec2,
};
use eframe::epaint::text::{FontTweak, VariationCoords};
use std::sync::Arc;

// The palette (common.md, verbatim).
pub const GROUND: Color32 = Color32::from_rgb(0x0E, 0x11, 0x16);
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x1B, 0x22);
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x2D, 0x36);
pub const HEADER: Color32 = Color32::from_rgb(0x12, 0x16, 0x1C);
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xEA, 0xF0);
pub const MUTED: Color32 = Color32::from_rgb(0x9A, 0xA4, 0xB2);
pub const RED: Color32 = Color32::from_rgb(0xE5, 0x48, 0x4D);
pub const GREEN: Color32 = Color32::from_rgb(0x3D, 0xD6, 0x8C);
pub const AMBER: Color32 = Color32::from_rgb(0xF5, 0xA5, 0x24);
pub const BLUE: Color32 = Color32::from_rgb(0x4C, 0x9A, 0xFF);

// Shades the mockups use around those.
pub const SOFT: Color32 = Color32::from_rgb(0xB6, 0xBF, 0xCC);
pub const DIM: Color32 = Color32::from_rgb(0x7B, 0x85, 0x94);
pub const INSET: Color32 = Color32::from_rgb(0x10, 0x15, 0x1B);
pub const SCREEN: Color32 = Color32::from_rgb(0x1A, 0x22, 0x30);
pub const SCREEN_TEXT: Color32 = Color32::from_rgb(0xC9, 0xD3, 0xE0);
pub const BUTTON: Color32 = Color32::from_rgb(0x1C, 0x23, 0x2D);
pub const BUTTON_BORDER: Color32 = Color32::from_rgb(0x33, 0x40, 0x4F);
pub const BLUE_FILL: Color32 = Color32::from_rgb(0x25, 0x63, 0xEB);
pub const BLUE_DARK: Color32 = Color32::from_rgb(0x1D, 0x3B, 0x6E);
pub const BLUE_SOFT: Color32 = Color32::from_rgb(0x9C, 0xC2, 0xFF);
pub const BLUE_PANEL: Color32 = Color32::from_rgb(0x12, 0x1A, 0x26);
pub const RED_FILL: Color32 = Color32::from_rgb(0xB4, 0x23, 0x18);
pub const RED_DARK: Color32 = Color32::from_rgb(0x4A, 0x15, 0x12);
pub const RED_BORDER: Color32 = Color32::from_rgb(0x7A, 0x25, 0x22);
pub const RED_SOFT: Color32 = Color32::from_rgb(0xFF, 0xB4, 0xB1);
pub const RED_TEXT: Color32 = Color32::from_rgb(0xF0, 0x8A, 0x8D);
pub const AMBER_DARK: Color32 = Color32::from_rgb(0x3A, 0x2A, 0x0C);
pub const AMBER_STRIPE: Color32 = Color32::from_rgb(0x2E, 0x22, 0x09);
pub const AMBER_BORDER: Color32 = Color32::from_rgb(0x7A, 0x5A, 0x1A);
pub const AMBER_PANEL_BORDER: Color32 = Color32::from_rgb(0x6B, 0x53, 0x20);
pub const AMBER_SOFT: Color32 = Color32::from_rgb(0xF5, 0xC2, 0x6B);
pub const AMBER_TEXT: Color32 = Color32::from_rgb(0xFF, 0xE2, 0xA8);
pub const GREEN_BORDER: Color32 = Color32::from_rgb(0x1F, 0x5B, 0x3E);
pub const GREEN_DARK: Color32 = Color32::from_rgb(0x15, 0x3A, 0x2A);

pub const SIM_TEXT: &str = "SIMULATION — nothing here is live. No equipment or YouTube is touched.";

const LABEL: &str = "label";
const LABEL_BOLD: &str = "label-bold";
const BODY_BOLD: &str = "body-bold";
const MONO_BOLD: &str = "mono-bold";

/// Barlow Semi Condensed SemiBold: small upper-case labels.
pub fn label_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(LABEL.into()))
}

/// Barlow Semi Condensed Bold: headings, buttons, badges.
pub fn heading_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(LABEL_BOLD.into()))
}

/// Atkinson Hyperlegible Bold.
pub fn body_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BODY_BOLD.into()))
}

/// JetBrains Mono Medium: times.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// JetBrains Mono Bold: big times.
pub fn mono_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MONO_BOLD.into()))
}

pub fn tone_color(t: Tone) -> Color32 {
    match t {
        Tone::Grey => MUTED,
        Tone::Green => GREEN,
        Tone::Amber => AMBER,
        Tone::Red => RED,
    }
}

/// Sets the fonts and the dark visuals. egui's own fonts stay as fallbacks for the
/// few symbols the bundled ones lack.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8], weight: Option<f32>| {
        let mut data = FontData::from_static(bytes);
        if let Some(w) = weight {
            data = data.tweak(FontTweak { coords: VariationCoords::new([(b"wght", w)]), ..Default::default() });
        }
        fonts.font_data.insert(name.to_owned(), Arc::new(data));
    };
    add("barlow-semibold", include_bytes!("../assets/fonts/barlow/BarlowSemiCondensed-SemiBold.ttf"), None);
    add("barlow-bold", include_bytes!("../assets/fonts/barlow/BarlowSemiCondensed-Bold.ttf"), None);
    add("atkinson", include_bytes!("../assets/fonts/atkinson/AtkinsonHyperlegible-Regular.ttf"), None);
    add("atkinson-bold", include_bytes!("../assets/fonts/atkinson/AtkinsonHyperlegible-Bold.ttf"), None);
    let jetbrains: &'static [u8] = include_bytes!("../assets/fonts/jetbrains/JetBrainsMono-wght.ttf");
    add("jetbrains-medium", jetbrains, Some(500.0));
    add("jetbrains-bold", jetbrains, Some(700.0));

    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let family = |first: &str| std::iter::once(first.to_owned()).chain(fallbacks.iter().cloned()).collect::<Vec<_>>();
    fonts.families.insert(FontFamily::Proportional, family("atkinson"));
    fonts.families.insert(FontFamily::Monospace, family("jetbrains-medium"));
    fonts.families.insert(FontFamily::Name(LABEL.into()), family("barlow-semibold"));
    fonts.families.insert(FontFamily::Name(LABEL_BOLD.into()), family("barlow-bold"));
    fonts.families.insert(FontFamily::Name(BODY_BOLD.into()), family("atkinson-bold"));
    fonts.families.insert(FontFamily::Name(MONO_BOLD.into()), family("jetbrains-bold"));
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        use egui::TextStyle as T;
        style.text_styles = [
            (T::Small, FontId::proportional(13.0)),
            (T::Body, FontId::proportional(16.0)),
            (T::Monospace, mono(14.0)),
            (T::Button, heading_font(16.0)),
            (T::Heading, heading_font(28.0)),
        ]
        .into();
        style.spacing.item_spacing = vec2(10.0, 8.0);
        style.spacing.button_padding = vec2(14.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = Some(TEXT);
        v.panel_fill = GROUND;
        v.window_fill = PANEL;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.extreme_bg_color = INSET;
        v.faint_bg_color = Color32::from_rgb(0x13, 0x18, 0x1F);
        v.hyperlink_color = Color32::from_rgb(0x7F, 0xB2, 0xFF);
        v.warn_fg_color = AMBER;
        v.error_fg_color = RED;
        v.selection.bg_fill = BLUE_DARK;
        v.selection.stroke = Stroke::new(1.0, BLUE);
        let w = &mut v.widgets;
        for (state, fill, border) in [
            (&mut w.inactive, BUTTON, BUTTON_BORDER),
            (&mut w.hovered, Color32::from_rgb(0x24, 0x2D, 0x39), Color32::from_rgb(0x45, 0x55, 0x68)),
            (&mut w.active, Color32::from_rgb(0x2A, 0x34, 0x42), BLUE),
            (&mut w.open, BUTTON, BLUE),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, border);
            state.corner_radius = CornerRadius::same(8);
            state.fg_stroke.color = TEXT;
        }
        w.noninteractive.bg_fill = PANEL;
        w.noninteractive.weak_bg_fill = PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke.color = TEXT;
    });
}

/// A small upper-case label, like the mockups' `.lbl`.
pub fn lbl(ui: &mut Ui, text: &str) -> Response {
    ui.label(lbl_text(text, MUTED))
}

pub fn lbl_text(text: &str, color: Color32) -> RichText {
    RichText::new(text.to_uppercase()).font(label_font(13.0)).color(color).extra_letter_spacing(1.0)
}

/// A panel: `#161B22`, 1 px border, rounded.
pub fn panel() -> Frame {
    Frame::new().fill(PANEL).stroke(Stroke::new(1.0, BORDER)).corner_radius(10).inner_margin(Margin::same(20))
}

/// A box inside a panel (the previews, the video rows).
pub fn inset() -> Frame {
    Frame::new().fill(INSET).stroke(Stroke::new(1.0, BORDER)).corner_radius(8).inner_margin(Margin::same(14))
}

pub fn dot(ui: &mut Ui, color: Color32) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
    r
}

/// A small outlined tag ("Hall + Online", "−2 dB", "Playing").
pub fn chip(ui: &mut Ui, text: &str, color: Color32, border: Color32) -> Response {
    Frame::new()
        .stroke(Stroke::new(1.0, border))
        .corner_radius(5)
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(lbl_text(text, color)))
        .response
}

/// A thin rounded bar, `frac` of it filled.
pub fn progress(ui: &mut Ui, frac: f32, color: Color32) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 10.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 5.0, BORDER);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.0, 1.0));
    if fill.width() > 0.0 {
        p.rect_filled(fill, 5.0, color);
    }
}

/// The stage badge: a pill with a dot, the stage, and the on-air timer when live.
pub fn badge(ui: &mut Ui, h: &HeaderView) -> Response {
    let (fill, border, dot_color, text_color) = match h.badge_tone {
        Tone::Grey => (BUTTON, BUTTON_BORDER, MUTED, TEXT),
        Tone::Green => (GREEN_DARK, GREEN, GREEN, TEXT),
        Tone::Amber => (AMBER_DARK, AMBER, AMBER, AMBER_SOFT),
        Tone::Red => (RED_FILL, RED, Color32::WHITE, Color32::WHITE),
    };
    pill(ui, fill, border, |ui| {
        dot(ui, dot_color);
        ui.label(RichText::new(&h.badge).font(heading_font(16.0)).color(text_color).extra_letter_spacing(1.2));
        if let Some(t) = &h.timer {
            ui.label(RichText::new(t).font(mono(15.0)).color(text_color));
        }
    })
}

/// The header badge while one mode closes and the other opens.
pub fn switching_badge(ui: &mut Ui) -> Response {
    pill(ui, AMBER_DARK, AMBER, |ui| {
        ui.spinner();
        ui.label(RichText::new("SWITCHING…").font(heading_font(16.0)).color(AMBER_SOFT).extra_letter_spacing(1.2));
    })
}

fn pill(ui: &mut Ui, fill: Color32, border: Color32, add: impl FnOnce(&mut Ui)) -> Response {
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(255)
        .inner_margin(Margin::symmetric(14, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                add(ui);
            });
        })
        .response
}

const STEP_NAMES: [(&str, &str); 4] =
    [("Prepare", "Prepared"), ("Countdown", "Countdown"), ("Live", "Live"), ("End service", "Ended")];

/// Prepare → Countdown → Live → End service: done steps ticked green, the current one lit
/// in its own colour (blue, amber, red, red).
/// Step `button` (if any) is a button, drawn in red outline; returns true when it is clicked.
pub fn step_strip(ui: &mut Ui, steps: [StepState; 4], button: Option<usize>) -> bool {
    let mut clicked = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        for (i, (state, (todo, done))) in steps.iter().zip(STEP_NAMES).enumerate() {
            let (text, fill, border, color) = match state {
                StepState::Done => (format!("✓  {done}"), Color32::TRANSPARENT, GREEN_BORDER, GREEN),
                StepState::Current => {
                    let (fill, border, color) = match i {
                        0 => (BLUE_DARK, BLUE, Color32::WHITE),
                        1 => (AMBER_DARK, AMBER, AMBER_TEXT),
                        _ => (RED_DARK, RED, Color32::from_rgb(0xFF, 0xD3, 0xD1)),
                    };
                    (format!("{}  {todo}", i + 1), fill, border, color)
                }
                StepState::Todo if button == Some(i) => (format!("{}  {todo}", i + 1), RED_DARK, RED, RED_SOFT),
                StepState::Todo => (format!("{}  {todo}", i + 1), Color32::TRANSPARENT, BORDER, DIM),
            };
            let mut job = egui::text::LayoutJob::default();
            let mut format = egui::TextFormat::simple(heading_font(17.0), color);
            format.extra_letter_spacing = 1.0;
            job.append(&text.to_uppercase(), 0.0, format);
            let galley = ui.painter().layout_job(job);
            let sense = if button == Some(i) { Sense::click() } else { Sense::hover() };
            let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 36.0, 52.0), sense);
            let width = if button == Some(i) { 2.0 } else { 1.0 };
            let fill = if button == Some(i) && resp.hovered() { RED_FILL } else { fill };
            if button == Some(i) {
                clicked |= resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
            }
            let p = ui.painter();
            p.rect(rect, 8.0, fill, Stroke::new(width, border), egui::StrokeKind::Inside);
            let pos = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), rect.shrink2(vec2(18.0, 0.0))).min;
            p.galley(pos, galley, color);
        }
    });
    clicked
}

/// The striped amber banner shown on every screen in simulation. Returns true when its
/// "Settings › Equipment" link is pressed.
pub fn sim_banner(ui: &mut Ui) -> bool {
    // The stripes go behind the text, once its size is known.
    let background = ui.painter().add(Shape::Noop);
    let mut clicked = false;
    let rect = Frame::new()
        .inner_margin(Margin::symmetric(28, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(SIM_TEXT).font(heading_font(16.0)).color(AMBER_TEXT).extra_letter_spacing(1.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let link = egui::Label::new(
                        RichText::new("Leave simulation in Settings › Equipment")
                            .font(heading_font(16.0))
                            .color(AMBER_TEXT)
                            .underline(),
                    )
                    .sense(Sense::click());
                    clicked = ui.add(link).on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
                });
            });
        })
        .response
        .rect;
    // 135° stripes, 18 px each (the panel clips the ends).
    let mut shapes = vec![Shape::rect_filled(rect, 0.0, AMBER_DARK)];
    let h = rect.height();
    let mut x = rect.left() - h;
    while x < rect.right() + h {
        shapes.push(Shape::convex_polygon(
            vec![
                pos2(x + 18.0, rect.top()),
                pos2(x + 36.0, rect.top()),
                pos2(x + 36.0 - h, rect.bottom()),
                pos2(x + 18.0 - h, rect.bottom()),
            ],
            AMBER_STRIPE,
            Stroke::NONE,
        ));
        x += 36.0;
    }
    shapes.push(Shape::hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, AMBER)));
    ui.painter().set(background, Shape::Vec(shapes));
    clicked
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Blue: the next thing to do.
    Primary,
    /// Red: changes what is on air.
    Danger,
    /// Red outline: ends something, but not urgent.
    DangerOutline,
    /// Amber, filled.
    Warn,
    /// Amber outline.
    WarnOutline,
    Plain,
}

fn kind_colors(kind: Kind) -> (Color32, Color32, Color32) {
    match kind {
        Kind::Primary => (BLUE_FILL, BLUE, Color32::WHITE),
        Kind::Danger => (RED_FILL, RED, Color32::WHITE),
        Kind::DangerOutline => (BUTTON, RED_BORDER, RED_SOFT),
        Kind::Warn => (AMBER_DARK, AMBER, AMBER_TEXT),
        Kind::WarnOutline => (BUTTON, AMBER_BORDER, AMBER_SOFT),
        Kind::Plain => (BUTTON, BUTTON_BORDER, TEXT),
    }
}

/// A big upper-case button (64 px high) for the step's main action.
pub fn big_button(ui: &mut Ui, text: &str, kind: Kind, enabled: bool) -> Response {
    sized_button(ui, text, kind, 64.0, 21.0, enabled)
}

/// A normal upper-case button (44 px high).
pub fn button(ui: &mut Ui, text: &str, kind: Kind) -> Response {
    sized_button(ui, text, kind, 44.0, 16.0, true)
}

pub fn button_enabled(ui: &mut Ui, enabled: bool, text: &str, kind: Kind) -> Response {
    sized_button(ui, text, kind, 44.0, 16.0, enabled)
}

pub fn sized_button(ui: &mut Ui, text: &str, kind: Kind, height: f32, size: f32, enabled: bool) -> Response {
    let (fill, border, color) = kind_colors(kind);
    let label = RichText::new(text.to_uppercase()).font(heading_font(size)).color(color).extra_letter_spacing(1.0);
    let pad = if height >= 60.0 { 34.0 } else { 18.0 };
    let b = egui::Button::new(label)
        .fill(fill)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(8)
        .min_size(vec2(0.0, height));
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = vec2(pad, 0.0);
        ui.add_enabled(enabled, b)
    })
    .inner
}

/// A tab in the header: selected tabs are lit (blue, or amber for Simulation).
pub fn tab(ui: &mut Ui, text: &str, selected: bool, amber: bool) -> Response {
    let (fill, border, color) = match (selected, amber) {
        (true, false) => (BUTTON, BLUE, TEXT),
        (true, true) => (AMBER_DARK, AMBER, AMBER_TEXT),
        (false, false) => (Color32::TRANSPARENT, BORDER, MUTED),
        (false, true) => (Color32::TRANSPARENT, AMBER_BORDER, AMBER_SOFT),
    };
    let label = RichText::new(text.to_uppercase()).font(heading_font(16.0)).color(color).extra_letter_spacing(1.0);
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = vec2(16.0, 0.0);
        ui.add(
            egui::Button::new(label)
                .fill(fill)
                .stroke(Stroke::new(1.0, border))
                .corner_radius(8)
                .min_size(vec2(0.0, 40.0)),
        )
    })
    .inner
}

/// A "screen" placeholder: what viewers or the hall see, as words in a box.
pub fn screen_box(ui: &mut Ui, height: f32, text: &str, font: FontId) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
    ui.painter().rect_filled(rect, 6.0, SCREEN);
    // Long text (a long file name) wraps inside the box, centred, and is clipped to it.
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, SCREEN_TEXT, (rect.width() - 24.0).max(1.0));
    job.halign = egui::Align::Center;
    let galley = ui.painter().layout_job(job);
    let top = rect.center().y - galley.size().y / 2.0;
    ui.painter().with_clip_rect(rect.shrink(4.0)).galley(egui::pos2(rect.center().x, top), galley, SCREEN_TEXT);
}
