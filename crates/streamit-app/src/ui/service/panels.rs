//! The status panels across the top of the Service screens while a service is under way:
//! YouTube, what online sees, the stream audio and the hall screens.
use super::*;
use streamit_core::routing::Target;

/// Every panel is at least this tall, so a row of them lines up.
const PANEL_H: f32 = 128.0;

/// What a press in a panel asks for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
    Nothing,
    AudioUp,
    PickAudio,
}

impl StreamItApp {
    /// The four panels: one row on a wide window, two rows of two on a narrow one.
    pub(super) fn status_panels(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config) {
        let per_row = if ui.available_width() >= 1000.0 { 4 } else { 2 };
        let mut ask = Ask::Nothing;
        for row in [0usize, 1, 2, 3].chunks(per_row) {
            ui.columns(per_row, |cols| {
                for (col, &i) in cols.iter_mut().zip(row) {
                    let a = match i {
                        0 => youtube_panel(col, snap),
                        1 => atem_panel(col, snap, c, Target::StreamAtem, "Online sees"),
                        2 => audio_panel(col, snap),
                        _ => atem_panel(col, snap, c, Target::ScreensAtem, "Hall screens"),
                    };
                    if a != Ask::Nothing {
                        ask = a;
                    }
                }
            });
            ui.add_space(12.0);
        }
        ui.add_space(GAP - 12.0);
        match ask {
            Ask::Nothing => {}
            Ask::AudioUp => self.send(Command::UnmuteMics),
            Ask::PickAudio => {
                self.tab = Tab::Settings;
                self.section = Section::Timings;
                self.scroll_to = Some(Section::Timings);
            }
        }
    }
}

/// A panel's frame: its label, a note on the right, then `add`.
fn status_panel(ui: &mut Ui, label: &str, note: &str, tone: Tone, add: impl FnOnce(&mut Ui) -> Ask) -> Ask {
    let (fill, stroke) = match tone {
        Tone::Green => (theme::GREEN_DARK, theme::GREEN_BORDER),
        Tone::Amber => (theme::AMBER_DARK, theme::AMBER_BORDER),
        Tone::Red => (theme::RED_DARK, theme::RED),
        Tone::Grey => (theme::PANEL, theme::BORDER),
    };
    theme::panel()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke))
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(PANEL_H);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                theme::lbl(ui, label);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(note).size(12.0).color(theme::MUTED));
                });
            });
            add(ui)
        })
        .inner
}

/// Is YouTube receiving the stream, is the broadcast public, how many are watching.
fn youtube_panel(ui: &mut Ui, snap: &Snapshot) -> Ask {
    let p = view::youtube_panel(snap);
    status_panel(ui, "YouTube", "checked every 12 s", p.tone, |ui| {
        ui.horizontal(|ui| {
            theme::dot(ui, theme::tone_color(p.tone));
            let color = if p.tone == Tone::Red { theme::RED_SOFT } else { theme::TEXT };
            ui.add(
                egui::Label::new(RichText::new(&p.headline).font(theme::heading_font(26.0)).color(color)).truncate(),
            );
        });
        ui.add(egui::Label::new(RichText::new(&p.detail).size(14.0).color(theme::SOFT)).wrap());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if let Some(v) = p.visibility {
                if v == "Public" {
                    theme::chip(ui, v, theme::GREEN, theme::GREEN_BORDER);
                } else {
                    theme::chip(ui, v, theme::AMBER_SOFT, theme::AMBER_BORDER);
                }
            }
            if let Some(n) = p.viewers {
                theme::chip(ui, &format!("{n} watching"), theme::SOFT, theme::BUTTON_BORDER);
            }
        });
        Ask::Nothing
    })
}

/// What an ATEM has on program, and on preview.
fn atem_panel(ui: &mut Ui, snap: &Snapshot, c: &Config, target: Target, label: &str) -> Ask {
    let (program, preview) = view::atem_sees(snap, c, target);
    status_panel(ui, label, target.name(), Tone::Grey, |ui| {
        ui.label(RichText::new("On now").size(12.0).color(theme::MUTED));
        let (text, color) = match &program {
            Some(p) if p.starts_with("Video: ") => (p.as_str(), theme::BLUE_SOFT),
            Some(p) => (p.as_str(), theme::TEXT),
            None => ("Not answering", theme::MUTED),
        };
        ui.add(egui::Label::new(RichText::new(text).font(theme::heading_font(26.0)).color(color)).truncate());
        if let Some(p) = &preview {
            ui.add(egui::Label::new(RichText::new(format!("Preview · {p}")).size(14.0).color(theme::SOFT)).truncate());
        }
        Ask::Nothing
    })
}

/// The stream audio (up, or down for a video) and the audio inputs Settings picks.
fn audio_panel(ui: &mut Ui, snap: &Snapshot) -> Ask {
    status_panel(ui, "Stream audio", "Stream ATEM", Tone::Grey, |ui| {
        let mut ask = Ask::Nothing;
        let bar = view::mic_bar(snap);
        let (color, text_color) = match bar.tone {
            Tone::Red => (theme::RED, theme::RED_TEXT),
            Tone::Amber => (theme::AMBER, theme::AMBER_SOFT),
            Tone::Grey => (theme::DIM, theme::MUTED),
            Tone::Green => (theme::GREEN, theme::SOFT),
        };
        ui.horizontal(|ui| {
            let bar_w = (ui.available_width() - 130.0).max(40.0);
            ui.allocate_ui_with_layout(vec2(bar_w, 10.0), Layout::left_to_right(Align::Center), |ui| {
                theme::progress(ui, bar.fraction, color);
            });
            ui.label(RichText::new(bar.text).font(theme::mono(13.0)).color(text_color));
        });
        if snap.mics_ducked && theme::sized_button(ui, "Stream audio up", Kind::Warn, 36.0, 15.0, true).clicked() {
            ask = Ask::AudioUp;
        }
        if snap.audio.is_empty() {
            if ui.link(RichText::new("Choose inputs to show here").size(13.0)).clicked() {
                ask = Ask::PickAudio;
            }
            return ask;
        }
        for a in &snap.audio {
            let (tag, tone) = view::audio_tag(a.input);
            ui.horizontal(|ui| {
                let name_w = (ui.available_width() - 110.0).max(40.0);
                ui.allocate_ui_with_layout(vec2(name_w, 18.0), Layout::left_to_right(Align::Center), |ui| {
                    let color = if tone == Tone::Green { theme::TEXT } else { theme::MUTED };
                    ui.add(egui::Label::new(RichText::new(&a.name).size(14.0).color(color)).truncate());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if tone == Tone::Green {
                        theme::chip(ui, tag, theme::GREEN, theme::GREEN_BORDER);
                    } else {
                        theme::chip(ui, tag, theme::MUTED, theme::BUTTON_BORDER);
                    }
                    if let Some(i) = a.input.filter(|i| i.mix != AudioMix::Off) {
                        let db =
                            if i.fader_db <= -100.0 { "−∞".to_string() } else { format!("{:+.0} dB", i.fader_db) };
                        ui.label(RichText::new(db).font(theme::mono(12.0)).color(theme::MUTED));
                    }
                });
            });
        }
        ask
    })
}
