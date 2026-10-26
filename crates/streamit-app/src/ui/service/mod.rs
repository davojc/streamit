//! The Service tab: one of the four mockup screens (before the service, countdown, live,
//! live with a video playing), plus the closing hold, chosen by `view::service_screen`.
//! Each screen has its own file; this one holds the frame and the shared pieces.
mod before;
mod countdown;
mod live;
mod panels;
mod steps;

use super::{Section, StreamItApp, Tab};
use crate::theme::{self, Kind};
use crate::view::{self, ServiceScreen, Tone};
use eframe::egui::{self, Align, Layout, RichText, Ui, vec2};
use streamit_core::config::Config;
use streamit_core::devices::{AudioMix, ChatRole, Fix};
use streamit_core::engine::{CHAT_REPLY_MAX, Command, DeviceKind, Snapshot};
use streamit_core::golive::{Readiness, Stage};
use streamit_core::recovery::Offer;
use streamit_core::rollin::{CueState, MAX_CUES};

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
    /// leave of it, with its buttons at the bottom, as in the mockups. `c` is the config
    /// the engine runs on (in simulation, what it was opened with); `saved` the settings
    /// file's.
    pub(super) fn service_tab(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, saved: &Config, viewport: f32) {
        let screen = view::service_screen(snap);
        let top = ui.cursor().top();
        self.alerts(ui, snap, screen);
        // From Prepare until the service ends.
        if matches!(snap.stage, Stage::Prepared(_) | Stage::Countdown { .. } | Stage::Live | Stage::Ending { .. }) {
            self.status_panels(ui, snap, c);
        }
        let tall = (viewport - (ui.cursor().top() - top) - 2.0).max(480.0);
        match screen {
            ServiceScreen::BeforeService => self.before_service(ui, snap, c, tall),
            ServiceScreen::Countdown => self.countdown_screen(ui, snap, c, saved, tall),
            ServiceScreen::Live | ServiceScreen::VideoPlaying => self.live_screen(ui, snap, c),
            ServiceScreen::Ending => self.ending_screen(ui, snap, c),
        }
    }

    /// The main column and the side column; stacked when the window is narrow.
    pub(super) fn columns(
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

    /// Abandon, while the service is prepared or counting down and YouTube is still
    /// private. It takes a second click within `CONFIRM_FOR`, like the deck's CONFIRM.
    fn abandon_button(&mut self, ui: &mut Ui, snap: &Snapshot) {
        const CONFIRM_FOR: std::time::Duration = std::time::Duration::from_secs(5);
        let asked = self.confirm_abandon.filter(|t| t.elapsed() < CONFIRM_FOR);
        if !snap.can_abandon() || asked.is_none() {
            self.confirm_abandon = None;
        }
        if !snap.can_abandon() {
            return;
        }
        // While the countdown runs its job is the countdown, which Abandon stops.
        let enabled = snap.job.is_none() || matches!(snap.stage, Stage::Countdown { .. });
        let text = if asked.is_some() { "Click again to abandon" } else { "Abandon" };
        let r = theme::big_button(ui, text, Kind::DangerOutline, enabled);
        let r = r.on_hover_text(
            "Back to the start: the countdown stops and the Web Presenter stops. YouTube stays private, so you can \
             prepare again.",
        );
        if r.clicked() {
            if asked.is_some() {
                self.confirm_abandon = None;
                self.send(Command::Abandon);
            } else {
                self.confirm_abandon = Some(std::time::Instant::now());
            }
        }
        if let Some(t) = self.confirm_abandon {
            ui.ctx().request_repaint_after(CONFIRM_FOR.saturating_sub(t.elapsed()));
        }
    }

    /// The commands with no place on the current screen, so the window can do everything
    /// the Stream Deck can when the deck is down.
    pub(in crate::ui) fn more_menu(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let allowed = view::more_menu(snap);
        if !allowed.start_now_confirm || !allowed.start_now {
            self.confirm_start_now = false;
        }
        let label = RichText::new("MORE ▾").font(theme::heading_font(17.0)).color(theme::MUTED);
        // Clicks inside keep it open (each command closes it itself), so the confirm
        // click of Start service now can land.
        let config =
            egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let (_, menu) = egui::containers::menu::MenuButton::new(label).config(config).ui(ui, |ui| {
            ui.set_min_width(300.0);
            // From Idle, Start service now takes a confirm click: Prepare is safer.
            let start_text = if self.confirm_start_now { "Click again to start now" } else { "Start service now" };
            let start = ui.add_enabled(allowed.start_now, egui::Button::new(RichText::new(start_text).size(16.0)));
            let start = match allowed.start_now_reason {
                Some(why) => start.on_disabled_hover_text(why),
                None => start,
            };
            if start.clicked() {
                if allowed.start_now_confirm && !self.confirm_start_now {
                    self.confirm_start_now = true;
                } else {
                    self.confirm_start_now = false;
                    self.send(Command::GoLive);
                    ui.close();
                }
            }
            let hint = allowed.start_now_reason.unwrap_or(allowed.start_now_hint);
            let hint_color = if self.confirm_start_now { theme::AMBER_SOFT } else { theme::MUTED };
            ui.label(RichText::new(hint).size(13.0).color(hint_color));
            ui.add_space(4.0);
            let items = [
                (
                    "Go public now",
                    "Make the YouTube broadcast visible. Picture and sound stay.",
                    true,
                    Command::GoPublic,
                ),
                (
                    "End broadcast",
                    "End the YouTube broadcast only. The equipment stays as it is.",
                    true,
                    Command::EndBroadcast,
                ),
                (
                    "End service",
                    "Closing still, then YouTube ends and the Web Presenter stops.",
                    allowed.end_service,
                    Command::EndService,
                ),
            ];
            for (text, hint, enabled, cmd) in items {
                if ui.add_enabled(enabled, egui::Button::new(RichText::new(text).size(16.0))).clicked() {
                    if cmd == Command::EndService {
                        self.end_service(snap);
                    } else {
                        self.send(cmd);
                    }
                    ui.close();
                }
                ui.label(RichText::new(hint).size(13.0).color(theme::MUTED));
                ui.add_space(4.0);
            }
        });
        // Closing the menu forgets a half-made confirm.
        if menu.is_none() {
            self.confirm_start_now = false;
        }
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
            warning_bar(ui, &view::flash_key_text(k));
        }
        // On the countdown screen the retry has its own box; before the service a Recover
        // offered first is the screen's own next step.
        let recover_step = screen == ServiceScreen::BeforeService && view::recover_first(snap);
        let offers: Vec<Offer> = snap
            .offers
            .iter()
            .copied()
            .filter(|o| !(screen == ServiceScreen::Countdown && *o == Offer::GoPublic))
            .filter(|o| !(recover_step && *o == Offer::Recover))
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
}

// ---------------------------------------------------------------- shared pieces

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

/// A dot and a line of text.
fn status_line(ui: &mut Ui, tone: Tone, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        theme::dot(ui, theme::tone_color(tone));
        ui.add(egui::Label::new(RichText::new(text).size(17.0)).wrap());
    });
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
