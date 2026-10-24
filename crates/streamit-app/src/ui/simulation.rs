//! Screen 7: the Simulation tab, shown only while simulating. The operator plays the
//! vision mixer on the pretend Stream ATEM, watches the other pretend equipment and the
//! pretend YouTube, breaks things on purpose, changes the speed and starts again. It
//! reads and drives the pretend world through `Front.sim` (`SimControl`); nothing here
//! goes through the engine except "Crash & restart app" (`Sessions::restart_engine`).
use super::StreamItApp;
use crate::session::Mode;
use crate::theme::{self, Kind};
use crate::view;
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Stroke, Ui, vec2};
use streamit_core::config::Config;
use streamit_core::engine::Snapshot;
use streamit_sim::{SimControl, SimView, Speed, media_player_input};
use tracing::info;

const ASIDE_W: f32 = 420.0;
const ROW_LABEL_W: f32 = 72.0;

/// A source button's colours: program red, preview green, speed blue, else plain.
#[derive(Clone, Copy, PartialEq)]
enum Lit {
    Off,
    Program,
    Preview,
    Selected,
}

/// A 56 × 44 source button, as the mockup's `.src`.
fn src_button(ui: &mut Ui, text: &str, lit: Lit) -> egui::Response {
    let (fill, border, color) = match lit {
        Lit::Off => (theme::INSET, theme::BUTTON_BORDER, theme::SOFT),
        Lit::Program => (theme::RED_FILL, theme::RED, Color32::WHITE),
        Lit::Preview => (Color32::from_rgb(0x15, 0x52, 0x3A), theme::GREEN, Color32::WHITE),
        Lit::Selected => (theme::BLUE_DARK, theme::BLUE, Color32::WHITE),
    };
    let label = RichText::new(text).font(theme::heading_font(15.0)).color(color);
    ui.add(
        egui::Button::new(label)
            .fill(fill)
            .stroke(Stroke::new(1.0, border))
            .corner_radius(7)
            .min_size(vec2(56.0, 44.0)),
    )
}

fn row_label(ui: &mut Ui, text: &str) {
    ui.allocate_ui_with_layout(vec2(ROW_LABEL_W, 44.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(ROW_LABEL_W);
        theme::lbl(ui, text);
    });
}

/// One "name … value" line of a panel.
fn kv(ui: &mut Ui, key: &str, value: &str, mono: bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(15.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let v = RichText::new(value).size(15.0);
            ui.label(if mono { v.font(theme::mono(15.0)) } else { v });
        });
    });
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, Color32::from_rgb(0x1F, 0x26, 0x2F)));
}

fn hint(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(14.0).color(theme::SOFT)).wrap());
}

impl StreamItApp {
    pub(super) fn simulation_tab(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let Some(sim) = self.sim.borrow().clone() else {
            theme::panel().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("The simulation is opening…").color(theme::SOFT));
            });
            return;
        };
        // What the running simulation was built from (its effective config, with any
        // simulated defaults), not the settings file as edited since.
        let effective = sim.config().clone();
        let v = sim.snapshot();
        // A reset under a running sequence would pull its devices out from under it.
        let reset = view::switch_state(snap, Mode::Simulation);
        self.columns(
            ui,
            ASIDE_W,
            |app, ui| {
                app.mixer_panel(ui, &sim, &v, &effective);
                let w = ui.available_width();
                ui.columns(2, |c| {
                    c[0].set_max_width(w / 2.0);
                    theme::panel().show(&mut c[0], |ui| other_equipment(ui, &v));
                    theme::panel().show(&mut c[1], |ui| pretend_youtube(ui, snap, &v));
                });
            },
            |app, ui| {
                app.faults_panel(ui, &sim, &v);
                speed_panel(ui, &sim, &v);
                theme::panel().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    theme::lbl(ui, "Start again");
                    hint(ui, "Puts every pretend device back to before the service.");
                    ui.add_space(4.0);
                    let r = theme::button_enabled(ui, reset.enabled, "Reset simulation", Kind::Plain);
                    let r = match reset.reason {
                        Some(why) => r.on_disabled_hover_text(why),
                        None => r,
                    };
                    if r.clicked() {
                        info!("simulation: reset");
                        sim.reset();
                    }
                    if let Some(why) = reset.reason {
                        ui.label(RichText::new(why).size(13.0).color(theme::AMBER_SOFT));
                    }
                });
            },
        );
    }

    fn mixer_panel(&mut self, ui: &mut Ui, sim: &SimControl, v: &SimView, c: &Config) {
        let mp = media_player_input(c.stream_atem.media_player);
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::lbl(ui, "Stream ATEM · you are the vision mixer");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(view::mix_text(v.speed)).size(14.0).color(theme::MUTED));
                });
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                row_label(ui, "Program");
                for input in view::sim_sources(mp) {
                    let lit = if v.stream_program == input { Lit::Program } else { Lit::Off };
                    if src_button(ui, &view::source_label(input, mp), lit).clicked() {
                        info!(input, "simulation: program");
                        sim.set_program(input);
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                row_label(ui, "Preview");
                for input in view::sim_sources(mp) {
                    let lit = if v.stream_preview == input { Lit::Preview } else { Lit::Off };
                    if src_button(ui, &view::source_label(input, mp), lit).clicked() {
                        info!(input, "simulation: preview");
                        sim.set_preview(input);
                    }
                }
                ui.add_space(8.0);
                if theme::button(ui, "Cut", Kind::Plain).clicked() {
                    info!("simulation: cut");
                    sim.cut();
                }
                if theme::button(ui, "Auto", Kind::Plain).clicked() {
                    info!("simulation: auto");
                    sim.auto();
                }
            });
            ui.add_space(10.0);
            ui.columns(3, |col| {
                let mic = v.mic_levels.iter().find(|(s, _)| c.stream_atem.mic_sources.contains(s)).map(|(_, db)| *db);
                col[0].vertical(|ui| {
                    theme::lbl(ui, "Mics (XLR)");
                    theme::progress(ui, mic.map(view::mic_fraction).unwrap_or(0.0), theme::GREEN);
                    let text = mic.map(|db| format!("{db:.1} dB").replace('-', "−")).unwrap_or_else(|| "—".into());
                    ui.label(RichText::new(text).font(theme::mono(13.0)).color(theme::SOFT));
                });
                col[1].vertical(|ui| {
                    theme::lbl(ui, "Input 8 audio");
                    ui.label(RichText::new(if v.in8_audio_on { "on" } else { "off" }).font(theme::mono(15.0)));
                });
                col[2].vertical(|ui| {
                    theme::lbl(ui, "Input 8 carries");
                    let text = view::input8_text(v.input8, c.videohub.fallback_normal_input);
                    ui.label(RichText::new(text).font(theme::mono(15.0)));
                });
            });
        });
    }

    fn faults_panel(&mut self, ui: &mut Ui, sim: &SimControl, v: &SimView) {
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::lbl(ui, "Make something go wrong");
            hint(ui, "Each button breaks one thing, so you can practise the recovery. Press it again to put it right.");
            ui.add_space(6.0);
            let restarting = self.restart_job.running || *self.switching.borrow();
            let w = (ui.available_width() - 8.0) / 2.0;
            let mut buttons: Vec<(String, Kind, bool)> = view::FAULTS
                .iter()
                .map(|f| {
                    let active = v.faults.contains(f);
                    match active {
                        true => (format!("{} · active", view::fault_label(*f)), Kind::Warn, true),
                        false => (view::fault_label(*f).to_string(), Kind::Plain, true),
                    }
                })
                .collect();
            buttons.push(("Crash & restart app".into(), Kind::DangerOutline, !restarting));
            for (row, pair) in buttons.chunks(2).enumerate() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (col, (text, kind, enabled)) in pair.iter().enumerate() {
                        if fault_button(ui, w, text, *kind, *enabled).clicked() {
                            match view::FAULTS.get(row * 2 + col) {
                                Some(f) => {
                                    info!(fault = ?f, on = !v.faults.contains(f), "simulation: fault");
                                    sim.toggle_fault(*f);
                                }
                                None => {
                                    info!("simulation: crash & restart app");
                                    let sessions = self.sessions.clone();
                                    self.restart_job
                                        .start(&self.rt, &self.ctx, async move { sessions.restart_engine().await });
                                }
                            }
                        }
                    }
                });
            }
        });
    }
}

/// A fault button `w` wide; a long label (or one marked active) wraps onto two lines.
fn fault_button(ui: &mut Ui, w: f32, text: &str, kind: Kind, enabled: bool) -> egui::Response {
    let (fill, border, color) = match kind {
        Kind::Warn => (theme::AMBER_DARK, theme::AMBER, theme::AMBER_TEXT),
        Kind::DangerOutline => (theme::BUTTON, theme::RED_BORDER, theme::RED_SOFT),
        _ => (theme::BUTTON, theme::BUTTON_BORDER, theme::TEXT),
    };
    let label =
        RichText::new(text.to_uppercase()).font(theme::heading_font(14.0)).color(color).extra_letter_spacing(0.6);
    let b = egui::Button::new(label).fill(fill).stroke(Stroke::new(1.0, border)).corner_radius(8).wrap();
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = vec2(8.0, 2.0);
        ui.add_enabled_ui(enabled, |ui| ui.add_sized([w, 44.0], b)).inner
    })
    .inner
}

fn speed_panel(ui: &mut Ui, sim: &SimControl, v: &SimView) {
    theme::panel().show(ui, |ui| {
        ui.set_width(ui.available_width());
        theme::lbl(ui, "Speed");
        hint(ui, "Run videos and waits faster to practise quickly. The real service always runs at 1×.");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (speed, text) in [(Speed::X1, "1×"), (Speed::X4, "4×"), (Speed::X10, "10×")] {
                let lit = if v.speed == speed { Lit::Selected } else { Lit::Off };
                if src_button(ui, text, lit).clicked() && v.speed != speed {
                    info!(speed = ?speed, "simulation: speed");
                    sim.set_speed(speed);
                }
            }
        });
    });
}

fn other_equipment(ui: &mut Ui, v: &SimView) {
    ui.set_width(ui.available_width());
    theme::lbl(ui, "Other equipment");
    ui.add_space(8.0);
    kv(ui, "Screens ATEM program", &v.screens_program.to_string(), true);
    kv(ui, "Web Presenter", &view::encoder_text(&v.encoder), true);
    kv(ui, "DeckLink", &view::player_text(&v.player), true);
    kv(ui, "NDI counter", v.counter_text.as_deref().unwrap_or("—"), true);
}

fn pretend_youtube(ui: &mut Ui, snap: &Snapshot, v: &SimView) {
    ui.set_width(ui.available_width());
    theme::lbl(ui, "Pretend YouTube");
    ui.add_space(8.0);
    let title = snap.broadcast.as_ref().map(|b| b.title.as_str()).unwrap_or("—");
    kv(ui, "Broadcast", title, false);
    kv(ui, "State", &view::yt_state_text(v.yt_life_cycle.as_deref()), true);
    kv(ui, "Stream health", &view::health_text(&v.yt_health), true);
    kv(ui, "Went public in", &view::went_public_text(v.last_go_public, v.speed), true);
}
