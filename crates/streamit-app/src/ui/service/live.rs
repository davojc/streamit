//! Screen 3: live, the video list, and the closing hold.
use super::*;

impl StreamItApp {
    pub(super) fn live_screen(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config) {
        self.columns(
            ui,
            480.0,
            |app, ui| {
                ui.columns(2, |cols| {
                    on_air_panel(&mut cols[0], "Online · on air", true, |ui| {
                        big_value(ui, "Cameras", 40.0);
                        ui.label(
                            RichText::new("Program and preview are yours on the Stream ATEM")
                                .size(15.0)
                                .color(theme::MUTED),
                        );
                        ui.add_space(10.0);
                        mics(ui, snap);
                    });
                    on_air_panel(&mut cols[1], "Hall screens", false, |ui| {
                        big_value(ui, "As usual", 40.0);
                        ui.label(
                            RichText::new("Lyrics and slides from the Screens ATEM").size(15.0).color(theme::MUTED),
                        );
                        ui.add_space(10.0);
                        theme::lbl(ui, "YouTube");
                        let (tone, text) = youtube_status(snap);
                        status_line(ui, tone, &text);
                    });
                });
                theme::panel().inner_margin(22).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    theme::lbl(ui, "Next step");
                    ui.horizontal_wrapped(|ui| {
                        let text_w = (ui.available_width() - 220.0).max(260.0);
                        ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                            ui.label(RichText::new("End service when it finishes").font(theme::heading_font(30.0)));
                            ui.label(
                                RichText::new(format!(
                                    "Closing still for {} s, then YouTube ends and the Web Presenter stops. \
                                     Nothing to trim.",
                                    c.timings.closing_hold.as_secs()
                                ))
                                .size(15.0)
                                .color(theme::SOFT),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let busy = snap.job.is_some();
                            if theme::sized_button(ui, "End service", Kind::DangerOutline, 60.0, 19.0, !busy).clicked()
                            {
                                app.send(Command::EndService);
                            }
                        });
                    });
                });
                activity(ui, snap);
            },
            |app, ui| app.videos_panel(ui, snap, None),
        );
    }

    /// The service's videos with a Play button each. While one plays (`playing`), it is
    /// lit and the others wait.
    pub(super) fn videos_panel(&mut self, ui: &mut Ui, snap: &Snapshot, playing: Option<(usize, Option<String>)>) {
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::lbl(ui, "Videos");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let note = if playing.is_some() {
                        "Others wait until this one ends"
                    } else {
                        "Press to play · one at a time"
                    };
                    ui.label(RichText::new(note).size(14.0).color(theme::MUTED));
                });
            });
            ui.add_space(4.0);
            if snap.cues.is_empty() {
                ui.label(RichText::new("No videos yet.").color(theme::MUTED));
            }
            let next = view::next_cue(snap);
            for (i, (cue, state)) in snap.cues.iter().enumerate() {
                let this = playing.as_ref().is_some_and(|(p, _)| *p == i);
                let waiting = playing.is_some() && !this;
                let mut frame = theme::inset().inner_margin(egui::Margin::symmetric(14, 12));
                if this {
                    frame = frame.fill(theme::BLUE_PANEL).stroke(egui::Stroke::new(1.0, theme::BLUE));
                } else if playing.is_none() && next == Some(i) {
                    frame = frame.stroke(egui::Stroke::new(1.0, theme::BLUE));
                }
                frame.multiply_with_opacity(if waiting { 0.55 } else { 1.0 }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        let num = if this { theme::BLUE_SOFT } else { theme::DIM };
                        ui.label(RichText::new(format!("{:<2}", i + 1)).font(theme::mono(15.0)).color(num));
                        let text_w = (ui.available_width() - 230.0).max(120.0);
                        ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.add(egui::Label::new(RichText::new(&cue.name).font(theme::body_bold(17.0))).truncate());
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if this {
                                    theme::chip(ui, "Playing", theme::BLUE_SOFT, theme::BLUE);
                                } else {
                                    theme::chip(
                                        ui,
                                        view::dest_short(cue.destination),
                                        theme::SOFT,
                                        theme::BUTTON_BORDER,
                                    );
                                    theme::chip(ui, &view::fmt_db(cue.gain_db), theme::SOFT, theme::BUTTON_BORDER);
                                }
                            });
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if this {
                                let left = playing.as_ref().and_then(|(_, r)| r.clone()).unwrap_or_default();
                                ui.label(RichText::new(left).font(theme::mono(15.0)).color(theme::BLUE_SOFT));
                                return;
                            }
                            match state {
                                // Right to left: the button, then its state. Both can be
                                // played again (a stop by mistake, a load that failed once).
                                CueState::Played => {
                                    if theme::button_enabled(ui, !waiting, "Play again", Kind::Plain).clicked() {
                                        self.send(Command::PlayCue(i));
                                    }
                                    ui.label(RichText::new("Played").size(14.0).color(theme::DIM));
                                }
                                CueState::Error(e) => {
                                    if theme::button_enabled(ui, !waiting, "Retry", Kind::Plain)
                                        .on_hover_text(e)
                                        .clicked()
                                    {
                                        self.send(Command::PlayCue(i));
                                    }
                                    ui.label(RichText::new("Can’t play").size(14.0).color(theme::RED_TEXT))
                                        .on_hover_text(e);
                                }
                                _ => {
                                    let kind = if next == Some(i) && !waiting { Kind::Primary } else { Kind::Plain };
                                    if theme::button_enabled(ui, !waiting, "Play", kind).clicked() {
                                        self.send(Command::PlayCue(i));
                                    }
                                }
                            }
                        });
                    });
                });
            }
            if playing.is_none() {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new("Change the list on the").size(14.0).color(theme::MUTED));
                    if ui.link(RichText::new("Videos").size(14.0)).clicked() {
                        self.tab = Tab::Videos;
                    }
                    ui.label(
                        RichText::new(format!("tab. Up to {MAX_CUES}, in the order shown."))
                            .size(14.0)
                            .color(theme::MUTED),
                    );
                });
            }
        });
    }

    pub(super) fn ending_screen(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let remaining = match snap.stage {
            Stage::Ending { remaining } => remaining,
            _ => return,
        };
        self.columns(
            ui,
            440.0,
            |app, ui| {
                theme::panel().inner_margin(28).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    theme::lbl(ui, "Ending the service");
                    ui.label(RichText::new("Closing still on air").font(theme::heading_font(44.0)));
                    ui.label(RichText::new(view::fmt_mmss(remaining)).font(theme::mono_bold(72.0)));
                    ui.label(
                        RichText::new("Then YouTube ends and the Web Presenter stops.").size(17.0).color(theme::SOFT),
                    );
                    ui.add_space(12.0);
                    if theme::sized_button(ui, "Skip the closing hold", Kind::WarnOutline, 60.0, 19.0, true).clicked() {
                        app.send(Command::EndService);
                    }
                });
            },
            |_, ui| activity(ui, snap),
        );
    }
}

/// YouTube's line on the Live screen.
fn youtube_status(snap: &Snapshot) -> (Tone, String) {
    let public = !snap.offers.contains(&Offer::GoPublic);
    match (view::device_state(snap, DeviceKind::YouTube), public) {
        ((_, None), true) => (Tone::Green, "Public · stream healthy".into()),
        ((tone, Some(why)), true) => (tone, format!("Public · {why}")),
        (_, false) => (Tone::Amber, "Not public — retry going public above".into()),
    }
}
