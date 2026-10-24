//! The Service tab: one of the four mockup screens (before the service, countdown, live,
//! live with a video playing), plus the closing hold, chosen by `view::service_screen`.
use super::{StreamItApp, Tab};
use crate::theme::{self, Kind};
use crate::view::{self, ServiceScreen, Tone};
use eframe::egui::{self, Align, Layout, RichText, Ui, vec2};
use streamit_core::config::Config;
use streamit_core::devices::Fix;
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, Snapshot};
use streamit_core::golive::{Readiness, Stage};
use streamit_core::recovery::Offer;
use streamit_core::rollin::{CueState, Destination, MAX_CUES};

const GAP: f32 = 20.0;
/// Activity lines shown on a screen.
const ACTIVITY_SHOWN: usize = 8;

/// Lays out a row of big buttons (64 px) so it ends at `bottom` (a screen y), or just
/// below the content when that is already lower.
fn at_bottom(ui: &mut Ui, bottom: f32, add: impl FnOnce(&mut Ui)) {
    ui.add_space((bottom - ui.cursor().top() - 64.0).max(8.0));
    add(ui);
}

impl StreamItApp {
    /// `viewport` is the height the tab is shown in: the main panel fills what the alerts
    /// leave of it, with its buttons at the bottom, as in the mockups.
    pub(super) fn service_tab(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, viewport: f32) {
        let screen = view::service_screen(snap);
        let top = ui.cursor().top();
        self.alerts(ui, snap, screen);
        let tall = (viewport - (ui.cursor().top() - top) - 2.0).max(480.0);
        match screen {
            ServiceScreen::BeforeService => self.before_service(ui, snap, c, tall),
            ServiceScreen::Countdown => self.countdown_screen(ui, snap, c, tall),
            ServiceScreen::Live => self.live_screen(ui, snap, c),
            ServiceScreen::VideoPlaying => self.video_screen(ui, snap, c),
            ServiceScreen::Ending => self.ending_screen(ui, snap),
        }
    }

    /// The main column and the side column; stacked when the window is narrow.
    fn columns(
        &mut self,
        ui: &mut Ui,
        aside_w: f32,
        main: impl FnOnce(&mut Self, &mut Ui),
        aside: impl FnOnce(&mut Self, &mut Ui),
    ) {
        let w = ui.available_width();
        if w < 900.0 {
            main(self, ui);
            ui.add_space(GAP);
            aside(self, ui);
            return;
        }
        let aside_w = aside_w.min(w * 0.42);
        let main_w = w - GAP - aside_w;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.allocate_ui_with_layout(vec2(main_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(main_w);
                ui.spacing_mut().item_spacing.y = GAP;
                main(self, ui);
            });
            ui.allocate_ui_with_layout(vec2(aside_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(aside_w);
                ui.spacing_mut().item_spacing.y = GAP;
                aside(self, ui);
            });
        });
    }

    /// Warnings, ignored presses, and what the engine offers (Recover, a retry...).
    fn alerts(&mut self, ui: &mut Ui, snap: &Snapshot, screen: ServiceScreen) {
        if let Some(w) = &snap.last_warning {
            warning_bar(ui, &format!("Warning: {w}"));
        }
        if let Some(i) = snap.flash {
            warning_bar(ui, &format!("Video {} ignored: something else is running", i + 1));
        }
        if let Some(k) = snap.flash_key {
            warning_bar(ui, &format!("{k:?} ignored right now"));
        }
        // On the countdown screen the retry has its own box.
        let offers: Vec<Offer> = snap
            .offers
            .iter()
            .copied()
            .filter(|o| !(screen == ServiceScreen::Countdown && *o == Offer::GoPublic))
            .collect();
        if offers.is_empty() {
            return;
        }
        theme::panel().stroke(egui::Stroke::new(1.0, theme::AMBER_PANEL_BORDER)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::lbl_text("StreamIt can put this right", theme::AMBER_SOFT));
            ui.horizontal_wrapped(|ui| {
                for o in offers {
                    let (text, kind, cmd, why) = match o {
                        Offer::Recover => (
                            "Recover",
                            Kind::Primary,
                            Command::Recover,
                            "Puts the equipment back to how it should be at this stage.",
                        ),
                        Offer::GoLive => (
                            "Start service now",
                            Kind::Danger,
                            Command::GoLive,
                            "Cuts the stream to the camera and goes public.",
                        ),
                        Offer::GoPublic => (
                            "Retry going public",
                            Kind::Warn,
                            Command::GoPublic,
                            "Makes the broadcast visible without touching the picture or sound.",
                        ),
                        Offer::EndBroadcast => (
                            "End the YouTube broadcast",
                            Kind::DangerOutline,
                            Command::EndBroadcast,
                            "The service has ended here but YouTube is still live.",
                        ),
                    };
                    if theme::button(ui, text, kind).on_hover_text(why).clicked() {
                        self.send(cmd);
                    }
                }
            });
        });
        ui.add_space(GAP);
    }

    // ------------------------------------------------------------ 1. Before the service

    fn before_service(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, tall: f32) {
        self.columns(
            ui,
            440.0,
            |app, ui| {
                theme::panel().inner_margin(28).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(tall - 58.0);
                    let bottom = ui.cursor().top() + tall - 58.0;
                    app.next_step_panel(ui, snap, c, bottom);
                });
            },
            |app, ui| {
                app.preflight(ui, snap);
                equipment(ui, snap);
                lined_up(ui, snap);
            },
        );
    }

    fn next_step_panel(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, bottom: f32) {
        let prepared = matches!(snap.stage, Stage::Prepared(_));
        ui.spacing_mut().item_spacing.y = 6.0;
        theme::lbl(ui, "Next step");
        let (title, text) = if prepared {
            (
                "Start the countdown",
                format!(
                    "Plays the countdown video on the hall screens and the stream. YouTube goes public \
                     at {} and the stream cuts to the camera at 0:00.",
                    view::fmt_mmss(c.timings.go_public_offset)
                ),
            )
        } else {
            (
                "Prepare the stream",
                "Puts the “Stream starting soon” still on air in silence, starts the Web Presenter, \
                 and waits for YouTube to receive it privately. Do this 10–20 minutes before the service."
                    .to_string(),
            )
        };
        ui.label(RichText::new(title).font(theme::heading_font(44.0)));
        ui.add_space(4.0);
        ui.label(RichText::new(text).size(17.0).color(theme::SOFT));
        if snap.stage == Stage::Ended {
            ui.label(RichText::new("The last service has ended.").size(15.0).color(theme::MUTED));
        }
        match &snap.stage {
            Stage::Prepared(Readiness::Waiting(w)) => status_line(ui, Tone::Amber, &format!("Waiting: {w}")),
            Stage::Prepared(Readiness::Warning(w)) => status_line(ui, Tone::Amber, w),
            Stage::Prepared(Readiness::Ready) => status_line(ui, Tone::Green, "Ready: YouTube is receiving, privately"),
            _ => {}
        }
        if let Some(job) = snap.job {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(format!("{job} is running…")).color(theme::SOFT));
            });
        }
        ui.add_space(18.0);
        ui.columns(2, |cols| {
            preview(
                &mut cols[0],
                "Online will see",
                "Stream starting soon…",
                true,
                &format!("Opening still · media slot {} · mics silent", c.stream_atem.opening_still + 1),
            );
            preview(&mut cols[1], "Hall screens", "Unchanged", false, "The hall isn’t touched until the countdown");
        });
        at_bottom(ui, bottom, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let busy = snap.job.is_some();
                if prepared {
                    if theme::big_button(ui, "Start countdown", Kind::Warn, !busy).clicked() {
                        self.send(Command::Countdown);
                    }
                    if theme::big_button(ui, "Start service now", Kind::DangerOutline, !busy).clicked() {
                        self.send(Command::GoLive);
                    }
                    deck_hint(ui, "COUNTDOWN", "row 2, key 2");
                } else {
                    if theme::big_button(ui, "Prepare", Kind::Primary, !busy).clicked() {
                        self.send(Command::Prepare);
                    }
                    deck_hint(ui, "PREPARE", "row 2, key 1");
                }
            });
        });
    }

    fn preflight(&mut self, ui: &mut Ui, snap: &Snapshot) {
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::lbl(ui, "Pre-flight checks");
            ui.add_space(4.0);
            self.broadcast_picker(ui, snap);
            ui.add_space(4.0);
            let (yt, why) = view::device_state(snap, DeviceKind::YouTube);
            match why {
                None => check_row(ui, Tone::Green, "YouTube signed in"),
                Some(w) => check_row(ui, yt, &format!("YouTube · {w}")),
            };
            match snap.check {
                Some(c) => {
                    let rows = [
                        (
                            c.bound_to_ours,
                            "Broadcast uses our stream key",
                            "Broadcast is not on our stream key",
                            Fix::Bind,
                        ),
                        (
                            !c.auto_start,
                            "Auto Start is off",
                            "Auto Start is ON — would go public too early",
                            Fix::AutoStartOff,
                        ),
                        (
                            c.auto_stop,
                            "Auto Stop is on (safety net)",
                            "Auto Stop is off — no safety net",
                            Fix::AutoStopOn,
                        ),
                        (c.monitor_stream, "Preview stream enabled", "Preview stream is off", Fix::MonitorOn),
                    ];
                    for (ok, good, bad, fix) in rows {
                        if ok {
                            check_row(ui, Tone::Green, good);
                        } else if check_row_fix(ui, bad) {
                            self.send(Command::FixBroadcast(fix));
                        }
                    }
                }
                None if snap.broadcast.is_some() => check_row(ui, Tone::Grey, "Checking the broadcast…"),
                None => check_row(ui, Tone::Amber, "Choose this service’s broadcast above"),
            }
        });
    }

    /// Which YouTube broadcast this service uses, and a refresh.
    fn broadcast_picker(&mut self, ui: &mut Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            let selected =
                snap.broadcast.as_ref().map(|b| b.title.clone()).unwrap_or_else(|| "Choose a broadcast".into());
            let mut choice: Option<String> = None;
            let busy = self.broadcasts_loading;
            let width = (ui.available_width() - 110.0).max(120.0);
            egui::ComboBox::from_id_salt("broadcast").selected_text(selected).width(width).show_ui(ui, |ui| {
                for b in &self.broadcasts {
                    let label = match &b.scheduled_start {
                        Some(s) => format!("{}  ({s})", b.title),
                        None => b.title.clone(),
                    };
                    let on = snap.broadcast.as_ref().is_some_and(|x| x.id == b.id);
                    if ui.selectable_label(on, label).clicked() {
                        choice = Some(b.id.clone());
                    }
                }
            });
            if let Some(id) = choice {
                self.send(Command::SelectBroadcast(id));
            }
            if busy {
                ui.spinner();
            } else if ui.button("Refresh").clicked() {
                self.send(Command::Refresh);
                self.load_broadcasts();
            }
        });
        if let Some(e) = &self.broadcasts_error {
            ui.label(RichText::new(format!("Could not list broadcasts: {e}")).size(14.0).color(theme::AMBER_SOFT));
        }
    }

    // ------------------------------------------------------------ 2. Countdown

    fn countdown_screen(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, tall: f32) {
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

    // ------------------------------------------------------------ 3. Live

    fn live_screen(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config) {
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
                        mics(ui, snap.mics_ducked);
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

    // ------------------------------------------------------------ 4. Live, video playing

    fn video_screen(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config) {
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
                            if theme::sized_button(ui, "Unmute mics", Kind::Warn, 60.0, 19.0, snap.mics_ducked)
                                .on_disabled_hover_text("The mics are not turned down")
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
                    on_air_panel(&mut cols[0], "Online · on air", true, |ui| {
                        if online {
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
                        mics(ui, snap.mics_ducked);
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

    /// The service's videos with a Play button each. While one plays (`playing`), it is
    /// lit and the others wait.
    fn videos_panel(&mut self, ui: &mut Ui, snap: &Snapshot, playing: Option<(usize, Option<String>)>) {
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
                        let text_w = (ui.available_width() - 110.0).max(120.0);
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
                                CueState::Played if !waiting => {
                                    ui.label(RichText::new("Played").size(14.0).color(theme::DIM));
                                }
                                CueState::Error(e) => {
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

    // ------------------------------------------------------------ Closing hold

    fn ending_screen(&mut self, ui: &mut Ui, snap: &Snapshot) {
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

// ---------------------------------------------------------------- pieces

fn warning_bar(ui: &mut Ui, text: &str) {
    theme::panel()
        .fill(theme::AMBER_DARK)
        .stroke(egui::Stroke::new(1.0, theme::AMBER))
        .inner_margin(egui::Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(theme::AMBER_TEXT));
        });
    ui.add_space(10.0);
}

fn deck_hint(ui: &mut Ui, key: &str, place: &str) {
    ui.label(RichText::new("Stream Deck:").size(15.0).color(theme::MUTED));
    ui.label(RichText::new(key).font(theme::body_bold(15.0)));
    ui.label(RichText::new(format!("({place})")).size(15.0).color(theme::MUTED));
}

/// A dot and a line of text.
fn status_line(ui: &mut Ui, tone: Tone, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        theme::dot(ui, theme::tone_color(tone));
        ui.add(egui::Label::new(RichText::new(text).size(17.0)).wrap());
    });
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

fn check_row(ui: &mut Ui, tone: Tone, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        theme::dot(ui, theme::tone_color(tone));
        ui.add(egui::Label::new(RichText::new(text).size(16.0)).wrap());
    });
}

/// An amber check with its Fix button; true when Fix is pressed.
fn check_row_fix(ui: &mut Ui, text: &str) -> bool {
    let mut fix = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        theme::dot(ui, theme::AMBER);
        let w = (ui.available_width() - 80.0).max(80.0);
        ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::left_to_right(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(text).size(16.0)).wrap());
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            fix = theme::sized_button(ui, "Fix", Kind::WarnOutline, 34.0, 14.0, true).clicked();
        });
    });
    fix
}

/// What viewers or the hall see, as a labelled screen with a note under it.
fn preview(ui: &mut Ui, label: &str, screen: &str, heading: bool, note: &str) {
    theme::inset().inner_margin(16).show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::lbl(ui, label);
        ui.add_space(4.0);
        let font = if heading { theme::heading_font(24.0) } else { egui::FontId::proportional(16.0) };
        theme::screen_box(ui, 140.0, screen, font);
        ui.label(RichText::new(note).size(14.0).color(theme::MUTED));
    });
}

fn on_air_panel(ui: &mut Ui, label: &str, on_air: bool, add: impl FnOnce(&mut Ui)) {
    let border = if on_air { theme::RED_BORDER } else { theme::BORDER };
    theme::panel().stroke(egui::Stroke::new(1.0, border)).inner_margin(18).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            theme::lbl(ui, label);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if on_air {
                    theme::chip(ui, "Program", theme::RED_SOFT, theme::RED);
                } else {
                    theme::chip(ui, "Program", theme::SOFT, theme::BUTTON_BORDER);
                }
            });
        });
        add(ui);
    });
}

fn big_value(ui: &mut Ui, text: &str, size: f32) {
    ui.add(egui::Label::new(RichText::new(text).font(theme::heading_font(size))).truncate());
}

fn mics(ui: &mut Ui, ducked: bool) {
    theme::lbl(ui, "Mics");
    ui.horizontal(|ui| {
        let (frac, color, text, text_color) = if ducked {
            (0.04, theme::AMBER, "down for video", theme::AMBER_SOFT)
        } else {
            (0.72, theme::GREEN, "live", theme::SOFT)
        };
        let bar_w = (ui.available_width() - 140.0).max(60.0);
        ui.allocate_ui_with_layout(vec2(bar_w, 10.0), Layout::left_to_right(Align::Center), |ui| {
            theme::progress(ui, frac, color);
        });
        ui.label(RichText::new(text).font(theme::mono(14.0)).color(text_color));
    });
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

fn equipment(ui: &mut Ui, snap: &Snapshot) {
    theme::panel().show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::lbl(ui, "Equipment");
        ui.add_space(4.0);
        ui.columns(2, |cols| {
            for (n, k) in view::ALL_KINDS.iter().enumerate() {
                let ui = &mut cols[n % 2];
                let (tone, why) = view::device_state(snap, *k);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    theme::dot(ui, theme::tone_color(tone));
                    let r = ui.label(RichText::new(view::kind_name(*k)).size(15.0));
                    if let Some(why) = why {
                        let color = if tone == Tone::Red { theme::RED_TEXT } else { theme::AMBER_SOFT };
                        r.on_hover_text(&why);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("· {}", view::short_problem(&why))).size(13.0).color(color),
                            )
                            .truncate(),
                        );
                    }
                });
            }
        });
    });
}

/// The videos lined up for the service, on the first screen.
fn lined_up(ui: &mut Ui, snap: &Snapshot) {
    theme::panel().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            theme::lbl(ui, "Videos for this service");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{} of {MAX_CUES}", snap.cues.len())).size(14.0).color(theme::MUTED));
            });
        });
        ui.add_space(4.0);
        if snap.cues.is_empty() {
            ui.label(RichText::new("None yet: add them on the Videos tab.").size(15.0).color(theme::MUTED));
        }
        for (i, (cue, _)) in snap.cues.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                ui.label(RichText::new(format!("{:<2}", i + 1)).size(15.0).color(theme::DIM));
                let w = (ui.available_width() - 120.0).max(80.0);
                ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&cue.name).size(15.0)).truncate());
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(view::dest_short(cue.destination)).size(15.0).color(theme::MUTED));
                });
            });
        }
    });
}

/// What has happened, newest first, with local times.
fn activity(ui: &mut Ui, snap: &Snapshot) {
    theme::panel().show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::lbl(ui, "Activity");
        ui.add_space(4.0);
        if snap.activity.is_empty() {
            ui.label(RichText::new("Nothing yet.").font(theme::mono(13.0)).color(theme::DIM));
        }
        for a in snap.activity.iter().take(ACTIVITY_SHOWN) {
            let at = chrono::DateTime::<chrono::Local>::from(a.at).format("%H:%M:%S").to_string();
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = ui.available_width();
            job.append(&at, 0.0, egui::TextFormat::simple(theme::mono(13.0), theme::DIM));
            job.append(&a.text, 10.0, egui::TextFormat::simple(theme::mono(13.0), theme::SOFT));
            ui.label(job);
        }
    });
}
