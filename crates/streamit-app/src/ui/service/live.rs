//! Screen 3: live, with the videos (one may be playing), and the closing hold.
use super::*;

/// The height of the live chat's message list.
const CHAT_H: f32 = 460.0;

impl StreamItApp {
    /// Live, and any video playing (a rehearsal before live too): the videos and the
    /// activity, with the live chat down the side. End service is the fourth step in the
    /// steps bar.
    pub(super) fn live_screen(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let playing = snap.playing.map(|i| {
            let left = snap
                .video
                .filter(|v| v.index == i)
                .map(|v| format!("{} left", view::fmt_mmss(v.duration.saturating_sub(v.position))));
            (i, left)
        });
        self.columns(
            ui,
            400.0,
            |app, ui| {
                app.videos_panel(ui, snap, playing);
                activity(ui, snap);
            },
            |app, ui| app.chat_panel(ui, snap),
        );
    }

    /// The broadcast's live chat, newest at the bottom, and a reply box that posts as the
    /// channel.
    fn chat_panel(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let chat_id = snap.live.as_ref().and_then(|l| l.chat_id.clone());
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::lbl(ui, "Live chat");
                if let Some(b) = snap.broadcast.as_ref().filter(|_| !snap.simulation && chat_id.is_some()) {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.hyperlink_to(RichText::new("Open in YouTube ↗").size(14.0), view::chat_url(&b.id));
                    });
                }
            });
            ui.add_space(6.0);
            let note = match (&chat_id, &snap.chat_error) {
                (_, Some(e)) => Some((format!("Can’t read the chat: {e}"), theme::AMBER_SOFT)),
                (None, None) if snap.live.is_none() => {
                    Some(("The chat shows here once the service is prepared.".to_string(), theme::MUTED))
                }
                (None, None) => Some(("This broadcast has no live chat.".to_string(), theme::MUTED)),
                (Some(_), None) if snap.chat.is_empty() => Some(("No messages yet.".to_string(), theme::MUTED)),
                _ => None,
            };
            if let Some((text, color)) = note {
                ui.add(egui::Label::new(RichText::new(text).size(14.0).color(color)).wrap());
            }
            egui::ScrollArea::vertical()
                .max_height(CHAT_H)
                .min_scrolled_height(CHAT_H)
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    for m in &snap.chat {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                let color = if m.role == ChatRole::Viewer { theme::TEXT } else { theme::BLUE_SOFT };
                                ui.label(RichText::new(&m.author).font(theme::body_bold(14.0)).color(color));
                                match m.role {
                                    ChatRole::Owner => theme::chip(ui, "Channel", theme::BLUE_SOFT, theme::BLUE),
                                    ChatRole::Moderator => theme::chip(ui, "Moderator", theme::BLUE_SOFT, theme::BLUE),
                                    ChatRole::Viewer => ui.label(""),
                                };
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let at = view::chat_time(&m.published);
                                    ui.label(RichText::new(at).font(theme::mono(12.0)).color(theme::DIM));
                                });
                            });
                            ui.add(egui::Label::new(RichText::new(&m.text).size(14.0).color(theme::SOFT)).wrap());
                        });
                    }
                });
            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new("Reply as the channel").size(13.0).color(theme::MUTED));
            let can = chat_id.is_some();
            let mut send = false;
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 90.0).max(80.0);
                let edit = egui::TextEdit::singleline(&mut self.chat_reply)
                    .hint_text("Say something to everyone watching")
                    .char_limit(CHAT_REPLY_MAX)
                    .desired_width(w);
                let r = ui.add_enabled(can, edit);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    send = true;
                }
                let ready = can && !self.chat_reply.trim().is_empty();
                if theme::button_enabled(ui, ready, "Send", Kind::Primary).clicked() {
                    send = true;
                }
            });
            if send && can && !self.chat_reply.trim().is_empty() {
                let text = std::mem::take(&mut self.chat_reply);
                self.send(Command::SendChat(text));
            }
        });
    }

    /// The service's videos with a Play button each. While one plays (`playing`, with the
    /// time left), it is lit with Stop and its progress, and the others wait.
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
                        let text_w = (ui.available_width() - 260.0).max(120.0);
                        ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.add(egui::Label::new(RichText::new(&cue.name).font(theme::body_bold(17.0))).truncate());
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if this {
                                    theme::chip(ui, "Playing", theme::BLUE_SOFT, theme::BLUE);
                                }
                                theme::chip(ui, view::dest_short(cue.destination), theme::SOFT, theme::BUTTON_BORDER);
                                theme::chip(ui, &view::fmt_db(cue.gain_db), theme::SOFT, theme::BUTTON_BORDER);
                            });
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if this {
                                if theme::button(ui, "Stop", Kind::DangerOutline).clicked() {
                                    self.send(Command::StopVideo);
                                }
                                let left = playing.as_ref().and_then(|(_, r)| r.clone()).unwrap_or_default();
                                ui.label(RichText::new(left).font(theme::mono(17.0)).color(theme::BLUE_SOFT));
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
                    if this {
                        let frac = snap
                            .video
                            .filter(|v| v.index == i && !v.duration.is_zero())
                            .map(|v| v.position.as_secs_f32() / v.duration.as_secs_f32())
                            .unwrap_or(0.0);
                        ui.add_space(4.0);
                        theme::progress(ui, frac, theme::BLUE);
                    }
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
