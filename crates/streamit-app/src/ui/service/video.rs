//! Screen 4: live with a video playing.
use super::*;

impl StreamItApp {
    pub(super) fn video_screen(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config) {
        let Some(index) = snap.playing else { return };
        let cue = snap.cues.get(index).map(|(c, _)| c.clone());
        let name = cue.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| format!("Video {}", index + 1));
        let destination = cue.as_ref().map(|c| c.destination).unwrap_or(Destination::HallAndOnline);
        let progress = snap.video.filter(|v| v.index == index);
        let remaining = progress.map(|v| view::fmt_mmss(v.duration.saturating_sub(v.position)));
        self.columns(
            ui,
            480.0,
            |app, ui| {
                theme::panel()
                    .fill(theme::BLUE_PANEL)
                    .stroke(egui::Stroke::new(1.0, theme::BLUE_BORDER))
                    .inner_margin(24)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_top(|ui| {
                            let text_w = (ui.available_width() - 240.0).max(240.0);
                            ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                                ui.label(theme::lbl_text(
                                    &format!("Now playing · video {}", index + 1),
                                    theme::BLUE_SOFT,
                                ));
                                ui.label(RichText::new(&name).font(theme::heading_font(44.0)));
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    theme::chip(ui, view::dest_short(destination), theme::SOFT, theme::BUTTON_BORDER);
                                    if let Some(c) = &cue {
                                        theme::chip(ui, &view::fmt_db(c.gain_db), theme::SOFT, theme::BUTTON_BORDER);
                                    }
                                });
                            });
                            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                                theme::lbl(ui, "Remaining");
                                let text = remaining.clone().unwrap_or_else(|| "–:––".into());
                                ui.label(RichText::new(text).font(theme::mono_bold(72.0)));
                            });
                        });
                        ui.add_space(12.0);
                        let frac = progress
                            .filter(|v| !v.duration.is_zero())
                            .map(|v| v.position.as_secs_f32() / v.duration.as_secs_f32())
                            .unwrap_or(0.0);
                        theme::progress(ui, frac, theme::BLUE);
                        if let Some(v) = progress {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(view::fmt_mmss_floor(v.position))
                                        .font(theme::mono(13.0))
                                        .color(theme::MUTED),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(view::fmt_mmss(v.duration))
                                            .font(theme::mono(13.0))
                                            .color(theme::MUTED),
                                    );
                                });
                            });
                        } else {
                            ui.label(RichText::new("Starting the video…").size(14.0).color(theme::MUTED));
                        }
                        let ndi_ok = matches!(snap.device_status.get(&DeviceKind::Ndi), Some(DeviceHealth::Ok));
                        let value = remaining.clone().unwrap_or_else(|| "–:––".into());
                        status_line(
                            ui,
                            if ndi_ok { Tone::Green } else { Tone::Grey },
                            &format!(
                                "NDI counter sending {value} as “{}” · clears when the video ends",
                                c.ndi.source_name
                            ),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            if theme::sized_button(ui, "Stop video", Kind::DangerOutline, 60.0, 19.0, true).clicked() {
                                app.send(Command::StopVideo);
                            }
                            if theme::sized_button(ui, "Stream audio up", Kind::Warn, 60.0, 19.0, snap.mics_ducked)
                                .on_disabled_hover_text("The stream audio is not turned down")
                                .clicked()
                            {
                                app.send(Command::UnmuteMics);
                            }
                        });
                    });
                let back = match cue.as_ref().and_then(|c| c.return_input) {
                    Some(n) => format!("Returns to input {n}"),
                    None => "Returns to whatever is on preview".to_string(),
                };
                let online = matches!(destination, Destination::HallAndOnline | Destination::OnlineOnly);
                let hall = matches!(destination, Destination::HallAndOnline | Destination::HallOnly);
                ui.columns(2, |cols| {
                    // Before the service goes live (a rehearsal), nothing is on air online.
                    let (online_label, on_air) = view::online_panel(snap);
                    on_air_panel(&mut cols[0], online_label, on_air, |ui| {
                        if online && !on_air {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("Video · input {}", c.stream_atem.decklink_input))
                                        .font(theme::heading_font(32.0))
                                        .color(theme::MUTED),
                                )
                                .truncate(),
                            );
                            ui.label(RichText::new("The service isn’t live yet").size(15.0).color(theme::MUTED));
                        } else if online {
                            big_value(ui, &format!("Video · input {}", c.stream_atem.decklink_input), 32.0);
                            ui.label(RichText::new(&back).size(15.0).color(theme::MUTED));
                        } else if destination == Destination::AudioOnly {
                            big_value(ui, "Video sound only", 32.0);
                            ui.label(RichText::new("Over the live picture").size(15.0).color(theme::MUTED));
                        } else {
                            big_value(ui, "Cameras", 32.0);
                            ui.label(RichText::new("The video plays in the hall only").size(15.0).color(theme::MUTED));
                        }
                        ui.add_space(10.0);
                        mics(ui, snap);
                    });
                    on_air_panel(&mut cols[1], "Hall screens", false, |ui| {
                        if hall {
                            big_value(ui, &format!("Video · input {}", c.screens_atem.decklink_input), 32.0);
                            ui.label(RichText::new("Returns to: what it showed before").size(15.0).color(theme::MUTED));
                            ui.add_space(10.0);
                            theme::lbl(ui, "Camera 8");
                            ui.label(
                                RichText::new("Off input 8 while the video plays. Back automatically after.")
                                    .size(15.0)
                                    .color(theme::SOFT),
                            );
                        } else {
                            big_value(ui, "As usual", 32.0);
                            ui.label(
                                RichText::new("The video doesn’t play in the hall").size(15.0).color(theme::MUTED),
                            );
                        }
                    });
                });
            },
            |app, ui| app.videos_panel(ui, snap, Some((index, remaining.clone()))),
        );
    }
}
