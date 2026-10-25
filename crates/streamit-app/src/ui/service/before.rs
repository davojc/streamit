//! Screen 1: before the service (also Prepared and Ended).
use super::*;
use streamit_core::steps::StepGroup;

/// The next-step panel's height, buttons included.
const NEXT_STEP_H: f32 = 500.0;

impl StreamItApp {
    pub(super) fn before_service(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, tall: f32) {
        // The next step fills about half the height; the checks follow below it.
        let h = (tall - 58.0).min(NEXT_STEP_H);
        self.columns(
            ui,
            440.0,
            |app, ui| {
                theme::panel().inner_margin(28).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(h);
                    let bottom = ui.cursor().top() + h;
                    app.next_step_panel(ui, snap, c, bottom);
                });
                app.preflight(ui, snap);
                ui.columns(2, |cols| {
                    cols[0].spacing_mut().item_spacing.y = GAP;
                    equipment(&mut cols[0], snap);
                    lined_up(&mut cols[1], snap);
                });
            },
            |_, ui| steps::steps_panel(ui, snap, c, StepGroup::Prepare),
        );
    }

    fn next_step_panel(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, bottom: f32) {
        let step = view::next_step(snap);
        ui.spacing_mut().item_spacing.y = 6.0;
        theme::lbl(ui, "Next step");
        let (title, text) = view::next_step_text(snap, c);
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
        // The previews show what Prepare puts on air: not what Recover does.
        if step != view::NextStep::Recover {
            ui.columns(2, |cols| {
                preview(
                    &mut cols[0],
                    "Online will see",
                    "Stream starting soon…",
                    true,
                    &format!("Opening still · media slot {} · stream audio silent", c.stream_atem.opening_still + 1),
                );
                preview(&mut cols[1], "Hall screens", "Unchanged", false, "The hall isn’t touched until the countdown");
            });
        }
        at_bottom(ui, bottom, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let busy = snap.job.is_some();
                match step {
                    view::NextStep::Countdown => {
                        if theme::big_button(ui, "Start countdown", Kind::Warn, !busy).clicked() {
                            self.send(Command::Countdown);
                        }
                        if theme::big_button(ui, "Start service now", Kind::DangerOutline, !busy).clicked() {
                            self.send(Command::GoLive);
                        }
                        deck_hint(ui, "START COUNTDOWN", "Service page, row 2, key 2");
                    }
                    // S6: Recover before Prepare, which would put the still on air.
                    view::NextStep::Recover => {
                        if theme::big_button(ui, "Recover", Kind::Primary, !busy).clicked() {
                            self.send(Command::Recover);
                        }
                        let prepare = view::prepare_button(snap);
                        let r = theme::big_button(ui, "Prepare", Kind::Plain, prepare.enabled);
                        if let Some(why) = prepare.reason {
                            r.on_disabled_hover_text(why);
                            ui.label(RichText::new(why).size(15.0).color(theme::AMBER_SOFT));
                        }
                    }
                    view::NextStep::Prepare => {
                        let prepare = view::prepare_button(snap);
                        if theme::big_button(ui, "Prepare", Kind::Primary, prepare.enabled).clicked() {
                            self.send(Command::Prepare);
                        }
                        deck_hint(ui, "PREPARE", "Service page, row 2, key 1");
                    }
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
            let busy = self.broadcasts_job.running;
            let width = (ui.available_width() - 130.0).max(120.0);
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
}

fn deck_hint(ui: &mut Ui, key: &str, place: &str) {
    // On a line of its own when it would not fit beside the buttons (it would widen the panel).
    if ui.available_size_before_wrap().x < 360.0 {
        ui.end_row();
    }
    ui.label(RichText::new("Stream Deck:").size(15.0).color(theme::MUTED));
    ui.label(RichText::new(key).font(theme::body_bold(15.0)));
    ui.label(RichText::new(format!("({place})")).size(15.0).color(theme::MUTED));
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

fn equipment(ui: &mut Ui, snap: &Snapshot) {
    theme::panel().show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::lbl(ui, "Equipment");
        ui.add_space(4.0);
        ui.columns(2, |cols| {
            for (n, k) in view::shown_kinds(snap).iter().enumerate() {
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
                        )
                        .on_hover_text(&why);
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
