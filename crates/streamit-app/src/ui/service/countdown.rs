//! Screen 2: the countdown.
use super::*;

impl StreamItApp {
    pub(super) fn countdown_screen(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, tall: f32) {
        let Some(v) = view::countdown(snap, c) else { return };
        self.columns(
            ui,
            440.0,
            |app, ui| {
                theme::panel().inner_margin(28).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(tall - 58.0);
                    let bottom = ui.cursor().top() + tall - 58.0;
                    ui.spacing_mut().item_spacing.y = 18.0;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 28.0;
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            theme::lbl(ui, "Countdown video ends in");
                            ui.label(
                                RichText::new(&v.remaining).font(theme::mono_bold(150.0)).color(theme::AMBER_TEXT),
                            );
                            ui.label(
                                RichText::new(format!("{} · playing from the DeckLink", v.file_name))
                                    .size(15.0)
                                    .color(theme::MUTED),
                            );
                        });
                        ui.vertical(|ui| {
                            ui.add_space(40.0);
                            ui.spacing_mut().item_spacing.y = 12.0;
                            if v.public {
                                status_line(ui, Tone::Green, "YouTube is public");
                            } else {
                                rich_line(
                                    ui,
                                    Tone::Amber,
                                    &[("YouTube goes public at ", false), (&v.public_at_text, true)],
                                );
                            }
                            rich_line(
                                ui,
                                Tone::Grey,
                                &[("At ", false), ("0:00", true), (" the stream cuts to the camera on preview", false)],
                            );
                            status_line(
                                ui,
                                Tone::Grey,
                                &format!("Mics fade up, hall goes to input {}", c.screens_atem.t0_input),
                            );
                            status_line(ui, if v.ndi_ok { Tone::Green } else { Tone::Grey }, &v.ndi_text);
                        });
                    });
                    theme::progress(ui, v.progress, theme::AMBER);
                    ui.columns(2, |cols| {
                        let privacy = if v.public { "Public" } else { "Private" };
                        preview(
                            &mut cols[0],
                            "Online sees",
                            "Stream starting soon…",
                            true,
                            &format!("{privacy} · mics silent · ready on preview"),
                        );
                        preview(
                            &mut cols[1],
                            "Hall sees",
                            "▶  Countdown video",
                            false,
                            &format!("Playing on the hall screens · input {}", c.screens_atem.decklink_input),
                        );
                    });
                    at_bottom(ui, bottom, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            if theme::big_button(ui, "Start service now", Kind::Danger, true).clicked() {
                                app.send(Command::GoLive);
                            }
                            if theme::big_button(ui, "Stop countdown", Kind::WarnOutline, true).clicked() {
                                app.send(Command::StopCountdown);
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        "Start service now skips the rest of the video: YouTube goes public and \
                                         the stream cuts to the camera straight away.",
                                    )
                                    .size(14.0)
                                    .color(theme::MUTED),
                                )
                                .wrap(),
                            );
                        });
                    });
                });
            },
            |app, ui| {
                theme::panel().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    theme::lbl(ui, "What happens if you stop");
                    ui.label(
                        RichText::new(
                            "The countdown video stops and the hall goes back to what it showed. YouTube stays \
                             as it is. Start service then goes live straight away.",
                        )
                        .size(15.0)
                        .color(theme::SOFT),
                    );
                });
                if v.show_retry_public {
                    theme::panel().stroke(egui::Stroke::new(1.0, theme::AMBER_PANEL_BORDER)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(theme::lbl_text("YouTube didn’t go public", theme::AMBER_SOFT));
                        ui.label(
                            RichText::new(
                                "Going public failed. This retries making the broadcast visible without touching \
                                 the picture or sound.",
                            )
                            .size(15.0)
                            .color(theme::SOFT),
                        );
                        ui.add_space(4.0);
                        if theme::button(ui, "Retry going public", Kind::Warn).clicked() {
                            app.send(Command::GoPublic);
                        }
                    });
                }
                activity(ui, snap);
            },
        );
    }
}

/// A dot and a line with some parts in bold mono (times).
fn rich_line(ui: &mut Ui, tone: Tone, parts: &[(&str, bool)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        theme::dot(ui, theme::tone_color(tone));
        let mut job = egui::text::LayoutJob::default();
        for (text, time) in parts {
            let font = if *time { theme::mono_bold(17.0) } else { egui::FontId::proportional(17.0) };
            job.append(text, 0.0, egui::TextFormat::simple(font, theme::TEXT));
        }
        ui.label(job);
    });
}
