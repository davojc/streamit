//! Screen 6: the Settings tab. A section list on the left; Equipment (the devices found
//! on the network and their jobs, the restart note and the Simulation card), Inputs &
//! routing, Timings & audio, YouTube, and Playout & NDI counter on the right; Save and
//! Discard changes at the bottom.
//!
//! Every edit goes to the draft until Save. Save writes the shared config and the file
//! (never when the file is not `writable`); timings and inputs apply at once, device
//! settings when StreamIt restarts (R19). While simulating, Save still writes the real
//! settings: the simulation's own copy is rebuilt from them the next time it is opened.
use super::{Pending, SimTip, StreamItApp};
use crate::discovery::{self, BmdKind, DiscoveredDevice};
use crate::session::{Mode, SwitchRefused};
use crate::theme::{self, Kind};
use crate::view::{self, Tone};
use eframe::egui::{self, Align, Color32, Layout, Margin, RichText, Shape, Stroke, Ui, vec2};
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;
use streamit_core::config::{AudioSourceId, Config};
use streamit_core::engine::{DeviceKind, Snapshot};
use streamit_youtube::{API_BASE, Auth, StreamSummary, YouTubeClient};
use tracing::{info, warn};

pub(super) const SCAN_FOR: Duration = Duration::from_secs(4);
pub(super) const RESTART_NOTE: &str = "Restart StreamIt to apply device changes";
/// R19, in the mockup's words.
const RESTART_CARD: &str = "Changes to equipment take effect when StreamIt restarts.";
/// What Save means while simulating (Task 6's config rule).
const SIM_SAVE_NOTE: &str = "You are in simulation. Save still writes your real settings: equipment changes take \
     effect when StreamIt restarts, and the simulation uses your changes next time you open simulation.";
const NAV_W: f32 = 260.0;
const GAP: f32 = 20.0;
const FIELD_H: f32 = 40.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum Section {
    #[default]
    Equipment,
    Routing,
    Timings,
    YouTube,
    Playout,
}

const SECTIONS: [(Section, &str); 5] = [
    (Section::Equipment, "Equipment"),
    (Section::Routing, "Inputs & routing"),
    (Section::Timings, "Timings & audio"),
    (Section::YouTube, "YouTube"),
    (Section::Playout, "Playout & NDI counter"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Role {
    StreamAtem,
    ScreensAtem,
    VideoHub,
    WebPresenter,
}

/// Each role and the job name the mockup gives it.
const ROLES: [(Role, &str); 4] = [
    (Role::StreamAtem, "Stream switcher"),
    (Role::ScreensAtem, "Screens switcher"),
    (Role::VideoHub, "Router"),
    (Role::WebPresenter, "Stream encoder"),
];
const NOT_USED: &str = "Not used";

fn role_address(c: &mut Config, r: Role) -> &mut Option<String> {
    match r {
        Role::StreamAtem => &mut c.stream_atem.address,
        Role::ScreensAtem => &mut c.screens_atem.address,
        Role::VideoHub => &mut c.videohub.address,
        Role::WebPresenter => &mut c.web_presenter.address,
    }
}

fn role_kind(r: Role) -> DeviceKind {
    match r {
        Role::StreamAtem => DeviceKind::StreamAtem,
        Role::ScreensAtem => DeviceKind::ScreensAtem,
        Role::VideoHub => DeviceKind::VideoHub,
        Role::WebPresenter => DeviceKind::WebPresenter,
    }
}

fn roles_for(kind: Option<BmdKind>) -> Vec<(Role, &'static str)> {
    ROLES
        .iter()
        .copied()
        .filter(|(r, _)| match kind {
            Some(BmdKind::Atem) => matches!(r, Role::StreamAtem | Role::ScreensAtem),
            Some(BmdKind::VideoHub) => *r == Role::VideoHub,
            Some(BmdKind::WebPresenter) => *r == Role::WebPresenter,
            None => true,
        })
        .collect()
}

/// The settings that are only read when the devices are built at startup.
pub(super) fn device_settings(c: &Config) -> impl PartialEq + use<> {
    (
        (
            c.stream_atem.address.clone(),
            c.screens_atem.address.clone(),
            c.videohub.address.clone(),
            c.web_presenter.address.clone(),
        ),
        c.youtube.clone(),
        // The countdown file is read when the countdown starts, so it is not listed.
        (c.playout.decklink_device, c.playout.video_mode.clone()),
        c.ndi.clone(),
    )
}

pub(super) fn join_inputs(v: &[u16]) -> String {
    v.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
}

pub(super) fn parse_inputs(s: &str) -> Result<Vec<u16>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| x.parse::<u16>().map_err(|_| format!("Live cameras: {x:?} is not an input number")))
        .collect()
}

/// The digits in a typed field ("Input 13" → 13); `None` if there are none.
fn digits(s: &str) -> Option<f64> {
    let d: String = s.chars().filter(char::is_ascii_digit).collect();
    d.parse().ok()
}

// ---------------------------------------------------------------- fields

/// A field: its small upper-case label above the control.
fn field(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        theme::lbl(ui, label);
        add(ui);
    });
}

fn text(ui: &mut Ui, s: &mut String, hint: &str) -> egui::Response {
    let w = ui.available_width();
    ui.add_sized([w, FIELD_H], egui::TextEdit::singleline(s).hint_text(hint).align(egui::Align2::LEFT_CENTER))
}

fn opt_text(ui: &mut Ui, a: &mut Option<String>, hint: &str) {
    let mut s = a.clone().unwrap_or_default();
    if text(ui, &mut s, hint).changed() {
        *a = (!s.trim().is_empty()).then(|| s.trim().to_string());
    }
}

/// A number shown as "<prefix><n>".
fn number<N: egui::emath::Numeric>(ui: &mut Ui, v: &mut N, prefix: &str) {
    let w = ui.available_width();
    ui.add_sized([w, FIELD_H], egui::DragValue::new(v).prefix(prefix));
}

/// A router number, where 0 means "not set".
fn router_number(ui: &mut Ui, v: &mut u16, prefix: &'static str) {
    let w = ui.available_width();
    let d = egui::DragValue::new(v)
        .range(0..=999)
        .custom_formatter(move |n, _| if n == 0.0 { "not set".into() } else { format!("{prefix}{n}") })
        .custom_parser(|s| Some(digits(s).unwrap_or(0.0)));
    ui.add_sized([w, FIELD_H], d);
}

/// A media-pool still, stored from 0 and shown from 1 as on the ATEM ("Slot 1").
fn slot(ui: &mut Ui, v: &mut u16) {
    let w = ui.available_width();
    let d = egui::DragValue::new(v)
        .range(0..=63)
        .custom_formatter(|n, _| format!("Slot {}", n as u64 + 1))
        .custom_parser(|s| digits(s).map(|n| (n - 1.0).max(0.0)));
    ui.add_sized([w, FIELD_H], d);
}

fn seconds(ui: &mut Ui, d: &mut Duration) {
    let w = ui.available_width();
    let mut s = d.as_secs_f64();
    let r = ui.add_sized(
        [w, FIELD_H],
        egui::DragValue::new(&mut s).range(0.0..=600.0).speed(0.05).min_decimals(1).max_decimals(2).suffix(" s"),
    );
    if r.changed() {
        *d = Duration::from_millis((s * 1000.0).round() as u64);
    }
}

/// A note under a field, in amber.
fn amber_note(ui: &mut Ui, s: &str) {
    ui.label(RichText::new(s).size(13.0).color(theme::AMBER_SOFT));
}

fn heading(ui: &mut Ui, s: &str) {
    ui.label(RichText::new(s).font(theme::heading_font(28.0)));
}

fn sub(ui: &mut Ui, s: &str) {
    ui.label(RichText::new(s).size(15.0).color(theme::SOFT));
}

/// A rounded box with a dot and a sentence: the restart note.
fn note_box(ui: &mut Ui, s: &str, dot: Color32) {
    egui::Frame::new()
        .fill(Color32::from_rgb(0x2A, 0x22, 0x10))
        .stroke(Stroke::new(1.0, theme::AMBER_PANEL_BORDER))
        .corner_radius(8)
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::dot(ui, dot);
                ui.add(egui::Label::new(RichText::new(s).size(15.0).color(Color32::from_rgb(0xF5, 0xD5, 0x8E))).wrap());
            });
        });
}

/// A section panel; scrolled into view when its nav entry was pressed.
fn section(ui: &mut Ui, scroll: bool, add: impl FnOnce(&mut Ui)) {
    let r = theme::panel()
        .inner_margin(22)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        })
        .response;
    if scroll {
        r.scroll_to_me(Some(Align::TOP));
    }
}

impl StreamItApp {
    pub(super) fn settings_tab(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let w = ui.available_width();
        if w < 900.0 {
            self.settings_nav(ui);
            ui.add_space(GAP);
            self.settings_main(ui, snap);
            return;
        }
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.allocate_ui_with_layout(vec2(NAV_W, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(NAV_W);
                self.settings_nav(ui);
            });
            let main_w = ui.available_width();
            ui.allocate_ui_with_layout(vec2(main_w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(main_w);
                ui.spacing_mut().item_spacing.y = GAP;
                self.settings_main(ui, snap);
            });
        });
    }

    fn settings_nav(&mut self, ui: &mut Ui) {
        theme::panel().inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            for (s, name) in SECTIONS {
                let on = self.section == s;
                let b = egui::Button::new(RichText::new(name).size(16.0).color(if on {
                    Color32::WHITE
                } else {
                    theme::SOFT
                }))
                .fill(if on { theme::BUTTON } else { Color32::TRANSPARENT })
                .stroke(if on { Stroke::new(1.0, theme::BUTTON_BORDER) } else { Stroke::NONE })
                .corner_radius(8)
                .min_size(vec2(ui.available_width(), 44.0));
                let r = ui.scope(|ui| {
                    ui.spacing_mut().button_padding = vec2(14.0, 0.0);
                    ui.with_layout(Layout::top_down(Align::Min), |ui| ui.add(b)).inner
                });
                if r.inner.clicked() {
                    self.section = s;
                    self.scroll_to = Some(s);
                }
            }
        });
    }

    fn settings_main(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let target = self.scroll_to.take();
        let simulating = *self.mode.borrow() == Mode::Simulation;
        if simulating {
            note_box(ui, SIM_SAVE_NOTE, theme::AMBER);
        }
        section(ui, target == Some(Section::Equipment), |ui| self.equipment(ui, snap, simulating));
        section(ui, target == Some(Section::Routing), |ui| self.routing(ui, simulating));
        section(ui, target == Some(Section::Timings), |ui| self.timings(ui));
        section(ui, target == Some(Section::YouTube), |ui| self.youtube(ui, simulating));
        section(ui, target == Some(Section::Playout), |ui| self.playout(ui));
        self.save_bar(ui);
    }

    // ---- Equipment

    fn equipment(&mut self, ui: &mut Ui, snap: &Snapshot, simulating: bool) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                heading(ui, "Equipment");
                sub(ui, "Found on the network. Give each one its job.");
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if theme::button(ui, "Add by IP", Kind::Plain).clicked() {
                    self.show_add_ip = !self.show_add_ip;
                }
                // Simulation never touches the network: no mDNS scan.
                let busy = self.scan_job.running;
                if theme::button_enabled(ui, !busy && !simulating, "Search again", Kind::Plain)
                    .sim_tip(simulating)
                    .clicked()
                {
                    self.scan();
                }
                if busy {
                    ui.spinner();
                    ui.label(RichText::new("Looking for Blackmagic devices…").color(theme::SOFT));
                }
            });
        });
        if self.show_add_ip {
            ui.add_space(8.0);
            self.add_by_ip(ui);
        }
        ui.add_space(12.0);
        self.device_table(ui, snap, simulating);
        ui.add_space(12.0);
        theme::lbl(ui, "Addresses");
        ui.columns(4, |c| {
            let d = &mut self.draft;
            field(&mut c[0], "Stream switcher", |ui| opt_text(ui, &mut d.stream_atem.address, "not set"));
            field(&mut c[1], "Screens switcher", |ui| opt_text(ui, &mut d.screens_atem.address, "not set"));
            field(&mut c[2], "Router", |ui| opt_text(ui, &mut d.videohub.address, "not set"));
            field(&mut c[3], "Stream encoder", |ui| opt_text(ui, &mut d.web_presenter.address, "not set"));
        });
        ui.add_space(14.0);
        if simulating {
            note_box(
                ui,
                "Equipment changes you save now take effect when StreamIt restarts, and in the simulation next time you open simulation.",
                theme::AMBER,
            );
        } else {
            note_box(ui, RESTART_CARD, theme::AMBER);
            let waiting = device_settings(&self.config.read().unwrap_or_else(|e| e.into_inner()))
                != device_settings(&self.startup);
            if waiting {
                ui.label(
                    RichText::new(format!("Saved equipment changes are waiting: {RESTART_NOTE}.")).color(theme::RED),
                );
            }
        }
        ui.add_space(16.0);
        self.simulation_card(ui, snap);
    }

    fn add_by_ip(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.add_ip)
                    .hint_text("192.168.4.x")
                    .desired_width(180.0)
                    .min_size(vec2(0.0, FIELD_H))
                    .align(egui::Align2::LEFT_CENTER),
            );
            egui::ComboBox::from_id_salt("add_kind")
                .selected_text(self.add_kind.map(BmdKind::label).unwrap_or("any kind"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.add_kind, None, "any kind");
                    for k in [BmdKind::Atem, BmdKind::VideoHub, BmdKind::WebPresenter] {
                        ui.selectable_value(&mut self.add_kind, Some(k), k.label());
                    }
                });
            if theme::button(ui, "Add", Kind::Plain).clicked() {
                match self.add_ip.trim().parse::<IpAddr>() {
                    Ok(address) if !self.discovered.iter().any(|d| d.address == address) => {
                        self.discovered.push(DiscoveredDevice {
                            name: "Added by IP".into(),
                            model: String::new(),
                            address,
                            kind: self.add_kind,
                        });
                        self.add_ip.clear();
                    }
                    Ok(_) => self.notice = Some("That address is already in the list.".into()),
                    Err(_) => self.notice = Some(format!("{:?} is not an IP address.", self.add_ip.trim())),
                }
            }
        });
    }

    fn device_table(&mut self, ui: &mut Ui, snap: &Snapshot, simulating: bool) {
        if self.discovered.is_empty() {
            ui.label(
                RichText::new("No devices found yet. Search again, or add one by its IP address.").color(theme::SOFT),
            );
            return;
        }
        let mut assign: Option<(IpAddr, Option<Role>)> = None;
        let w = ui.available_width();
        egui::Grid::new("found").num_columns(4).min_col_width(60.0).spacing(vec2(24.0, 14.0)).show(ui, |ui| {
            for h in ["Status", "Device", "Address", "Job"] {
                theme::lbl(ui, h);
            }
            ui.end_row();
            for d in &self.discovered {
                let ip = d.address.to_string();
                let current = ROLES
                    .iter()
                    .find(|(r, _)| role_address(&mut self.draft, *r).as_deref() == Some(ip.as_str()))
                    .copied();
                // Green or red only for a device StreamIt is connected to in this job now.
                let tone = match current {
                    Some((r, _))
                        if !simulating && role_address(&mut self.startup, r).as_deref() == Some(ip.as_str()) =>
                    {
                        view::device_state(snap, role_kind(r)).0
                    }
                    _ => Tone::Grey,
                };
                theme::dot(ui, theme::tone_color(tone));
                ui.horizontal(|ui| {
                    ui.set_min_width((w * 0.35).min(380.0));
                    ui.label(RichText::new(&d.name).size(15.0));
                    if !d.model.is_empty() && d.model != d.name {
                        ui.label(RichText::new(&d.model).size(13.0).color(theme::MUTED));
                    }
                });
                ui.label(RichText::new(&ip).font(theme::mono(14.0)));
                egui::ComboBox::from_id_salt(("role", ip.clone()))
                    .selected_text(current.map(|(_, n)| n).unwrap_or(NOT_USED))
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(current.is_none(), NOT_USED).clicked() {
                            assign = Some((d.address, None));
                        }
                        for (r, name) in roles_for(d.kind) {
                            if ui.selectable_label(current.map(|c| c.0) == Some(r), name).clicked() {
                                assign = Some((d.address, Some(r)));
                            }
                        }
                    });
                ui.end_row();
            }
        });
        if let Some((ip, role)) = assign {
            let ip = ip.to_string();
            for (r, _) in ROLES {
                let a = role_address(&mut self.draft, r);
                if a.as_deref() == Some(ip.as_str()) && Some(r) != role {
                    *a = None;
                }
            }
            if let Some(r) = role {
                *role_address(&mut self.draft, r) = Some(ip);
            }
        }
    }

    /// The dashed-amber card that switches real ↔ simulation. The switch runs on the
    /// runtime (it takes seconds); the header shows SWITCHING… meanwhile.
    fn simulation_card(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let mode = *self.mode.borrow();
        let st = view::switch_state(snap, mode);
        let switching = *self.switching.borrow() || self.switch_job.running;
        let background = ui.painter().add(Shape::Noop);
        let rect = egui::Frame::new()
            .inner_margin(Margin::same(18))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_max_width((ui.available_width() - 260.0).max(240.0));
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.label(RichText::new("Simulation").font(theme::heading_font(20.0)).color(theme::AMBER_TEXT));
                        let body = match mode {
                            Mode::Real => {
                                "Swap all of the equipment and YouTube for pretend versions, to practise a service. \
                                 Nothing real is touched. Only available while no service is running."
                            }
                            Mode::Simulation => {
                                "You are practising with pretend equipment and a pretend YouTube. Leave simulation \
                                 to go back to the real equipment. Only available while no service is running."
                            }
                        };
                        ui.add(
                            egui::Label::new(RichText::new(body).size(15.0).color(Color32::from_rgb(0xD9, 0xC9, 0xA3)))
                                .wrap(),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if switching {
                            ui.label(
                                RichText::new("Switching…").font(theme::heading_font(16.0)).color(theme::AMBER_TEXT),
                            );
                            ui.spinner();
                            return;
                        }
                        ui.vertical(|ui| {
                            let r = theme::button_enabled(ui, st.enabled, st.label, Kind::Warn);
                            let r = match st.reason {
                                Some(why) => r.on_disabled_hover_text(why),
                                None => r,
                            };
                            if r.clicked() {
                                self.start_switch(mode);
                            }
                            if let Some(why) = st.reason {
                                amber_note(ui, why);
                            }
                        });
                    });
                });
            })
            .response
            .rect;
        let mut shapes = vec![Shape::rect_filled(rect, 8.0, Color32::from_rgb(0x1A, 0x16, 0x0C))];
        let r = rect.shrink(0.5);
        let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
        shapes.extend(Shape::dashed_line(&corners, Stroke::new(1.0, theme::AMBER), 6.0, 4.0));
        ui.painter().set(background, Shape::Vec(shapes));
    }

    fn start_switch(&mut self, from: Mode) {
        let to = match from {
            Mode::Real => Mode::Simulation,
            Mode::Simulation => Mode::Real,
        };
        info!(to = ?to, "switch pressed");
        let sessions = self.sessions.clone();
        self.switch_job.start(&self.rt, &self.ctx, async move { sessions.switch(to).await });
    }

    /// A refused or failed switch (or crash & restart) is shown as a notice.
    pub(super) fn take_switch_result(job: &mut Pending<Result<(), SwitchRefused>>, notice: &mut Option<String>) {
        if let Some(Err(e)) = job.take() {
            warn!("{e}");
            *notice = Some(e.0);
        }
    }

    // ---- Inputs & routing

    fn routing(&mut self, ui: &mut Ui, simulating: bool) {
        heading(ui, "Inputs & routing");
        ui.add_space(8.0);
        let defaulted = if simulating { self.sim_defaults.borrow().clone() } else { Vec::new() };
        let (sim, _) = crate::session::sim_config(&self.draft);
        let note = |ui: &mut Ui, field: &str, value: u16| {
            if let Some(n) = view::sim_default_note(&defaulted, field, value) {
                amber_note(ui, &n);
            }
        };
        let c = &mut self.draft;
        let live = &mut self.live_inputs_text;
        ui.columns(3, |col| {
            field(&mut col[0], "Live cameras", |ui| {
                text(ui, live, "1, 2, 3");
            });
            field(&mut col[1], "Safe camera (default at T-0)", |ui| {
                number(ui, &mut c.stream_atem.default_live_input, "Cam ")
            });
            field(&mut col[2], "Hall screens at T-0", |ui| number(ui, &mut c.screens_atem.t0_input, "Input "));
        });
        ui.columns(3, |col| {
            field(&mut col[0], "Video comes in on", |ui| number(ui, &mut c.stream_atem.decklink_input, "Input "));
            field(&mut col[1], "Router output to input 8", |ui| {
                router_number(ui, &mut c.videohub.input8_output, "Output ");
                note(ui, "videohub.input8_output", sim.videohub.input8_output);
            });
            field(&mut col[2], "Cam 8 normally on router input", |ui| {
                router_number(ui, &mut c.videohub.fallback_normal_input, "Input ");
                note(ui, "videohub.fallback_normal_input", sim.videohub.fallback_normal_input);
            });
        });
        ui.columns(3, |col| {
            field(&mut col[0], "Opening still", |ui| slot(ui, &mut c.stream_atem.opening_still));
            field(&mut col[1], "Closing still", |ui| slot(ui, &mut c.stream_atem.closing_still));
            field(&mut col[2], "DeckLink on router input", |ui| {
                router_number(ui, &mut c.videohub.decklink_input, "Input ");
                note(ui, "videohub.decklink_input", sim.videohub.decklink_input);
            });
        });
        ui.columns(3, |col| {
            field(&mut col[0], "Hall screens: video comes in on", |ui| {
                number(ui, &mut c.screens_atem.decklink_input, "Input ")
            });
            field(&mut col[1], "Media player", |ui| number(ui, &mut c.stream_atem.media_player, "MP"));
        });
        if !defaulted.is_empty() {
            amber_note(
                ui,
                "The simulation fills in router numbers that are not set yet. They are not saved: set the real ones above.",
            );
        }
    }

    // ---- Timings & audio

    fn timings(&mut self, ui: &mut Ui) {
        heading(ui, "Timings & audio");
        ui.add_space(8.0);
        let c = &mut self.draft;
        ui.columns(3, |col| {
            field(&mut col[0], "Go public before T-0", |ui| seconds(ui, &mut c.timings.go_public_offset));
            field(&mut col[1], "Closing still held for", |ui| seconds(ui, &mut c.timings.closing_hold));
            field(&mut col[2], "Router settle time", |ui| seconds(ui, &mut c.videohub.settle));
        });
        ui.columns(3, |col| {
            field(&mut col[0], "Mic fade", |ui| seconds(ui, &mut c.stream_atem.fade));
            field(&mut col[1], "Mics during a video (−100 = off)", |ui| {
                let w = ui.available_width();
                ui.add_sized(
                    [w, FIELD_H],
                    egui::DragValue::new(&mut c.stream_atem.duck_db).range(-100.0..=0.0).speed(0.5).suffix(" dB"),
                );
            });
        });
        ui.add_space(6.0);
        theme::lbl(ui, "Mic sources (Fairlight input / source)");
        let mut remove = None;
        for (i, m) in c.stream_atem.mic_sources.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut m.input).prefix("Input "));
                ui.add(egui::DragValue::new(&mut m.source).prefix("Source "));
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            c.stream_atem.mic_sources.remove(i);
        }
        if ui.button("Add mic source").clicked() {
            c.stream_atem.mic_sources.push(AudioSourceId { input: 1001, source: -65280 });
        }
    }

    // ---- YouTube

    fn youtube(&mut self, ui: &mut Ui, simulating: bool) {
        heading(ui, "YouTube");
        if simulating {
            sub(ui, "The simulation uses a pretend YouTube. Sign-in and stream keys are for the real one.");
        }
        ui.add_space(8.0);
        let y = &mut self.draft.youtube;
        ui.columns(3, |col| {
            field(&mut col[0], "OAuth client id", |ui| {
                text(ui, &mut y.client_id, "");
            });
            field(&mut col[1], "OAuth client secret", |ui| {
                let w = ui.available_width();
                ui.add_sized([w, FIELD_H], egui::TextEdit::singleline(&mut y.client_secret).password(true));
            });
            field(&mut col[2], "Stream key id", |ui| opt_text(ui, &mut y.stream_id, "pick below"));
        });
        ui.add_space(8.0);
        let (id, secret) = (self.draft.youtube.client_id.clone(), self.draft.youtube.client_secret.clone());
        let have_client = !id.trim().is_empty() && !secret.trim().is_empty();
        // Both build a real YouTube client: never in simulation.
        ui.horizontal(|ui| {
            let busy = self.signin_job.running;
            if theme::button_enabled(ui, have_client && !busy && !simulating, "Sign in", Kind::Plain)
                .sim_tip(simulating)
                .clicked()
            {
                let (id, secret, ctx) = (id.clone(), secret.clone(), ui.ctx().clone());
                self.signin_status = "Waiting for you to approve in the browser...".into();
                self.signin_job.start(&self.rt, &ctx, async move {
                    Auth::sign_in(&id, &secret).await.map(|_| ()).map_err(|e| e.to_string())
                });
            }
            if busy {
                ui.spinner();
            }
            ui.label(&self.signin_status);
        });
        ui.horizontal(|ui| {
            let busy = self.streams_job.running;
            if theme::button_enabled(ui, have_client && !busy && !simulating, "Load stream keys", Kind::Plain)
                .sim_tip(simulating)
                .clicked()
            {
                let (id, secret, ctx) = (id.clone(), secret.clone(), ui.ctx().clone());
                self.streams_job.start(&self.rt, &ctx, async move {
                    let auth = Auth::load(&id, &secret).map_err(|e| e.to_string())?;
                    let base = url::Url::parse(API_BASE).map_err(|e| e.to_string())?;
                    YouTubeClient::new(base, auth, None).list_streams().await.map_err(|e| e.to_string())
                });
            }
            let current = self.draft.youtube.stream_id.clone();
            let label = |s: &StreamSummary| format!("{}  {}", s.title, s.key_name);
            let selected = self
                .streams
                .iter()
                .find(|s| Some(&s.id) == current.as_ref())
                .map(label)
                .or(current.clone())
                .unwrap_or_else(|| "(none)".into());
            ui.add_enabled_ui(!simulating, |ui| {
                egui::ComboBox::from_id_salt("stream").selected_text(selected).width(320.0).show_ui(ui, |ui| {
                    for s in &self.streams {
                        if ui.selectable_label(current.as_ref() == Some(&s.id), label(s)).clicked() {
                            self.draft.youtube.stream_id = Some(s.id.clone());
                        }
                    }
                });
            });
            ui.label(&self.streams_status);
        });
    }

    // ---- Playout & NDI counter

    fn playout(&mut self, ui: &mut Ui) {
        heading(ui, "Playout & NDI counter");
        ui.add_space(8.0);
        let c = &mut self.draft;
        let (rt, ctx, job) = (&self.rt, &self.ctx, &mut self.countdown_job);
        ui.columns(3, |col| {
            field(&mut col[0], "DeckLink device", |ui| number(ui, &mut c.playout.decklink_device, "Device "));
            field(&mut col[1], "Video mode", |ui| {
                text(ui, &mut c.playout.video_mode, "");
            });
            field(&mut col[2], "NDI source name", |ui| {
                text(ui, &mut c.ndi.source_name, "");
            });
        });
        field(ui, "Countdown video", |ui| {
            ui.horizontal(|ui| {
                let mut s = c.playout.countdown_file.display().to_string();
                let w = (ui.available_width() - 140.0).max(200.0);
                if ui
                    .add_sized([w, FIELD_H], egui::TextEdit::singleline(&mut s).align(egui::Align2::LEFT_CENTER))
                    .changed()
                {
                    c.playout.countdown_file = PathBuf::from(s);
                }
                if theme::button_enabled(ui, !job.running, "Browse…", Kind::Plain).clicked() {
                    job.start(rt, ctx, async {
                        rfd::AsyncFileDialog::new()
                            .add_filter("Video", &["mp4", "mov", "m4v", "mkv"])
                            .pick_file()
                            .await
                            .map(|f| f.path().to_path_buf())
                    });
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            theme::lbl(ui, "Counter numbers");
            egui::color_picker::color_edit_button_srgb(ui, &mut c.ndi.fg);
            ui.add_space(18.0);
            theme::lbl(ui, "Counter background");
            egui::color_picker::color_edit_button_srgb(ui, &mut c.ndi.bg);
        });
    }

    // ---- Save / Discard

    /// The draft differs from the saved settings (cues are saved on their own).
    fn settings_dirty(&self) -> bool {
        let saved = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mut d = self.draft.clone();
        d.cues = saved.cues.clone();
        d != saved || self.live_inputs_text != join_inputs(&saved.stream_atem.live_inputs)
    }

    fn save_bar(&mut self, ui: &mut Ui) {
        let dirty = self.settings_dirty();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if theme::button(ui, "Save", Kind::Primary).clicked() {
                self.save_settings();
            }
            if theme::button_enabled(ui, dirty, "Discard changes", Kind::Plain).clicked() {
                self.draft = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
                self.live_inputs_text = join_inputs(&self.draft.stream_atem.live_inputs);
            }
            if dirty {
                ui.label(RichText::new("Unsaved changes").color(theme::AMBER_SOFT));
            }
        });
    }

    pub(super) fn save_settings(&mut self) {
        match parse_inputs(&self.live_inputs_text) {
            Ok(v) => self.draft.stream_atem.live_inputs = v,
            Err(e) => {
                self.notice = Some(e);
                return;
            }
        }
        let c = {
            let mut shared = self.config.write().unwrap_or_else(|e| e.into_inner());
            let mut c = self.draft.clone();
            c.cues = shared.cues.clone();
            *shared = c.clone();
            c
        };
        if !self.file.writable {
            self.notice = Some("Not saved: the settings file could not be backed up (see the log).".into());
            return;
        }
        match c.save(&self.file.path) {
            Ok(()) => {
                info!(path = %self.file.path.display(), "settings saved");
                let restart = device_settings(&c) != device_settings(&self.startup);
                let simulating = *self.mode.borrow() == Mode::Simulation;
                self.notice = Some(match (simulating, restart) {
                    (true, true) => format!(
                        "Saved to your real settings. {RESTART_NOTE}; the simulation uses them next time you open simulation."
                    ),
                    (true, false) => {
                        "Saved to your real settings. The simulation uses them next time you open simulation.".into()
                    }
                    (false, true) => format!("Saved. {RESTART_NOTE}."),
                    (false, false) => "Saved. Timings and other settings apply now.".into(),
                });
            }
            Err(e) => {
                warn!("saving settings: {e:#}");
                self.notice = Some(format!("Could not save: {e:#}"));
            }
        }
    }

    pub(super) fn scan(&mut self) {
        self.scan_job.start(&self.rt, &self.ctx, async move {
            tokio::task::spawn_blocking(|| discovery::browse(SCAN_FOR)).await.unwrap_or_default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_inputs_parse() {
        assert_eq!(parse_inputs("1, 2,3"), Ok(vec![1, 2, 3]));
        assert_eq!(parse_inputs(""), Ok(vec![]));
        assert!(parse_inputs("1, x").is_err());
    }

    #[test]
    fn typed_numbers_keep_their_digits() {
        assert_eq!(digits("Input 13"), Some(13.0));
        assert_eq!(digits("not set"), None);
        assert_eq!(digits("Slot 2"), Some(2.0));
    }

    #[test]
    fn device_changes_need_a_restart_but_timings_do_not() {
        let a = Config::default();
        let mut b = a.clone();
        b.timings.closing_hold = Duration::from_secs(3);
        b.videohub.input8_output = 7;
        assert!(device_settings(&a) == device_settings(&b));
        b.videohub.address = Some("10.0.0.1".into());
        assert!(device_settings(&a) != device_settings(&b));
        let mut c = a.clone();
        c.youtube.stream_id = Some("s".into());
        assert!(device_settings(&a) != device_settings(&c));
        let mut d = a.clone();
        d.playout.countdown_file = PathBuf::from("other.mp4");
        assert!(device_settings(&a) == device_settings(&d), "the countdown file is read live");
    }

    #[test]
    fn job_names_follow_the_mockup() {
        let names: Vec<&str> = ROLES.iter().map(|(_, n)| *n).collect();
        assert_eq!(names, ["Stream switcher", "Screens switcher", "Router", "Stream encoder"]);
        assert_eq!(roles_for(Some(BmdKind::VideoHub)), vec![(Role::VideoHub, "Router")]);
    }
}
