//! Settings › Profiles: save all of StreamIt's settings (and a backup of each device) as a
//! named profile, and load one back. Loading asks which device backups to put back.
//! Profiles are about the real equipment, so they are off in simulation; loading is refused
//! during a service.
use super::{Pending, StreamItApp};
use crate::profiles::{self, DEVICE_NAMES, ProfileInfo, Profiles, VIDEOHUB, WEB_PRESENTER};
use crate::session::Mode;
use crate::theme::{self, Kind};
use crate::view;
use eframe::egui::{self, Align, Color32, Layout, RichText, Stroke, Ui};
use std::path::Path;
use streamit_core::config::Config;
use streamit_core::engine::{Command, Snapshot};
use streamit_core::routing::Target;
use tracing::{info, warn};

/// A question the Profiles section is asking.
enum Ask {
    SaveNew { name: String, error: Option<String> },
    Load { info: ProfileInfo, videohub: bool, web: bool },
    Delete { name: String },
}

/// What a dialog does once it has closed.
type Then = Box<dyn FnOnce(&mut StreamItApp)>;

/// A save that finished: the profile and the settings it holds.
struct Saved {
    info: ProfileInfo,
    config: Config,
}

pub(super) struct ProfilesUi {
    store: Profiles,
    list: Vec<ProfileInfo>,
    /// The profile in use: the last one saved or loaded.
    current: Option<String>,
    /// The settings as they were when the profile in use was saved or loaded.
    baseline: Option<Config>,
    ask: Option<Ask>,
    save_job: Pending<Result<Saved, String>>,
    restore_job: Pending<Result<String, String>>,
}

impl ProfilesUi {
    /// The profiles beside the settings file at `config_path`.
    pub(super) fn new(config_path: &Path) -> ProfilesUi {
        let store = Profiles::beside(config_path);
        let current = store.current();
        let baseline = current.as_deref().and_then(|n| store.load(n).ok()).map(|(c, _)| c);
        let list = store.list();
        ProfilesUi {
            store,
            list,
            current,
            baseline,
            ask: None,
            save_job: Pending::default(),
            restore_job: Pending::default(),
        }
    }

    /// The profile in use, for the header.
    pub(super) fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }
}

/// Why profiles cannot be saved or loaded now, if they cannot.
pub(super) fn blocked(simulating: bool, dirty: bool) -> Option<&'static str> {
    if simulating {
        Some("Leave simulation to save or load profiles: they back up the real equipment.")
    } else if dirty {
        Some("Save or discard your Settings changes first.")
    } else {
        None
    }
}

fn device_line(info: &ProfileInfo) -> String {
    let have: Vec<&str> = DEVICE_NAMES.iter().copied().filter(|d| info.has(d)).collect();
    if have.is_empty() { "no device backups".into() } else { have.join(", ") }
}

impl StreamItApp {
    pub(super) fn profiles_section(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let simulating = *self.mode.borrow() == Mode::Simulation;
        let why_not = blocked(simulating, self.settings_dirty());
        ui.label(RichText::new("Profiles").font(theme::heading_font(28.0)));
        ui.label(
            RichText::new(
                "A profile is a saved copy of all of StreamIt's settings, plus a backup of each device's own \
                 settings where StreamIt can read them, in a folder of its own. Save your normal set-up, change \
                 things for someone else's event, then load it back.",
            )
            .color(theme::SOFT),
        );
        ui.add_space(10.0);
        let saving = self.profiles.save_job.running;
        let shared = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        theme::inset().inner_margin(14).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    theme::lbl(ui, "In use");
                    match self.profiles.current.clone() {
                        Some(name) => {
                            ui.label(RichText::new(&name).font(theme::heading_font(22.0)));
                            if let Some(p) = self.profiles.list.iter().find(|p| p.name == name) {
                                ui.label(RichText::new(format!("Saved {}", p.saved)).size(13.0).color(theme::MUTED));
                            }
                            let (text, colour) = if self.profiles.baseline.as_ref() == Some(&shared) {
                                ("Matches the saved profile.", theme::GREEN)
                            } else {
                                ("Changed since it was saved or loaded.", theme::AMBER_SOFT)
                            };
                            ui.label(RichText::new(text).size(14.0).color(colour));
                        }
                        None => {
                            ui.label(RichText::new("No profile yet").font(theme::heading_font(22.0)));
                            ui.label(
                                RichText::new("Save the settings as a profile to keep a copy you can come back to.")
                                    .size(14.0)
                                    .color(theme::MUTED),
                            );
                        }
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let ok = why_not.is_none() && !saving;
                    if theme::button_enabled(ui, ok, "Save as new…", Kind::Plain).clicked() {
                        self.profiles.ask = Some(Ask::SaveNew { name: String::new(), error: None });
                    }
                    if let Some(name) = self.profiles.current.clone()
                        && theme::button_enabled(ui, ok, &format!("Save into {name}"), Kind::Primary).clicked()
                    {
                        self.save_profile(name);
                    }
                    if saving {
                        ui.spinner();
                        ui.label(RichText::new("Reading the devices…").color(theme::SOFT));
                    }
                });
            });
        });
        if let Some(why) = why_not {
            ui.add_space(6.0);
            ui.label(RichText::new(why).color(theme::AMBER_SOFT));
        }
        ui.add_space(12.0);
        if self.profiles.list.is_empty() {
            ui.label(RichText::new("No profiles saved yet.").color(theme::MUTED));
            return;
        }
        let between = view::between_services(snap);
        let list = self.profiles.list.clone();
        // One row per profile, two lines each, so the list fits the Settings column: the name,
        // when it was saved and its device backups; the buttons on the right.
        theme::lbl(ui, "Saved profiles");
        ui.add_space(4.0);
        for p in list {
            let in_use = self.profiles.current.as_deref() == Some(p.name.as_str());
            let fill = if in_use { Color32::from_rgb(0x14, 0x20, 0x33) } else { theme::INSET };
            egui::Frame::new().fill(fill).corner_radius(8).inner_margin(12).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&p.name).font(theme::heading_font(18.0)));
                            if in_use {
                                egui::Frame::new()
                                    .fill(theme::BLUE_DARK)
                                    .corner_radius(4)
                                    .inner_margin(egui::Margin::symmetric(6, 1))
                                    .show(ui, |ui| ui.label(RichText::new("IN USE").font(theme::label_font(10.0))));
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(format!("Saved {}", p.saved)).size(13.0).color(theme::MUTED));
                            ui.add_space(6.0);
                            for (d, ok) in &p.devices {
                                let (fill, text) = if *ok {
                                    (Color32::from_rgb(0x1E, 0x2A, 0x22), Color32::from_rgb(0x8F, 0xE3, 0xB5))
                                } else {
                                    (Color32::from_rgb(0x2A, 0x2A, 0x2A), theme::DIM)
                                };
                                let tip = if *ok { "backed up" } else { "not connected when this was saved" };
                                egui::Frame::new()
                                    .fill(fill)
                                    .corner_radius(4)
                                    .inner_margin(egui::Margin::symmetric(5, 1))
                                    .show(ui, |ui| ui.label(RichText::new(*d).font(theme::mono_bold(10.5)).color(text)))
                                    .response
                                    .on_hover_text(tip);
                            }
                        });
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if theme::button_enabled(ui, !in_use && !saving, "Delete…", Kind::DangerOutline).clicked() {
                            self.profiles.ask = Some(Ask::Delete { name: p.name.clone() });
                        }
                        if theme::button(ui, "Open folder", Kind::Plain).clicked()
                            && let Err(e) = std::process::Command::new("explorer").arg(&p.dir).spawn()
                        {
                            self.notice = Some(format!("Could not open {}: {e}", p.dir.display()));
                        }
                        let can_load = why_not.is_none() && between && !in_use && !saving;
                        let load = theme::button_enabled(ui, can_load, "Load…", Kind::Plain);
                        let load = if !between { load.on_disabled_hover_text("Not during a service") } else { load };
                        if load.clicked() {
                            let (videohub, web) = (p.has(VIDEOHUB), p.has(WEB_PRESENTER));
                            self.profiles.ask = Some(Ask::Load { info: p.clone(), videohub, web });
                        }
                    });
                });
            });
            ui.add_space(6.0);
        }
    }

    /// Saves the settings in use (and what StreamIt can read of each device) as `name`.
    fn save_profile(&mut self, name: String) {
        let c = self.config.read().unwrap_or_else(|e| e.into_inner()).clone();
        let devices = self.devices.borrow().clone();
        let store = self.profiles.store.clone();
        info!(name = %name, "saving a profile");
        self.profiles.save_job.start(&self.rt, &self.ctx, async move {
            let backups = match &devices {
                Some(d) => profiles::read_devices(d, &c).await,
                None => profiles::DeviceBackups::default(),
            };
            let saved = chrono::Local::now().format("%-d %b %Y %H:%M").to_string();
            let config = c.clone();
            let info = tokio::task::spawn_blocking(move || store.save(&name, &c, &backups, &saved))
                .await
                .map_err(|e| e.to_string())??;
            Ok(Saved { info, config })
        });
    }

    /// Loads profile `info`: its settings always; each ticked device backup is put back.
    fn load_profile(&mut self, info: &ProfileInfo, videohub: bool, web: bool) {
        let (cfg, backups) = match self.profiles.store.load(&info.name) {
            Ok(x) => x,
            Err(e) => {
                warn!("loading profile {}: {e}", info.name);
                self.notice = Some(format!("Could not load {}: {e}", info.name));
                return;
            }
        };
        info!(name = %info.name, videohub, web, "loading a profile");
        {
            let mut shared = self.config.write().unwrap_or_else(|e| e.into_inner());
            *shared = cfg.clone();
        }
        if self.file.writable
            && let Err(e) = cfg.save(&self.file.path)
        {
            warn!("saving the loaded profile's settings: {e:#}");
            self.notice = Some(format!("Loaded, but could not save the settings file: {e:#}"));
        }
        self.draft = cfg.clone();
        self.live_inputs_text = super::settings::join_inputs(&cfg.stream_atem.live_inputs);
        self.cues = cfg.cues.clone();
        self.send(Command::SetCues(cfg.cues.clone()));
        for t in Target::ALL {
            self.send(Command::SetPresets(t, cfg.routing(t).presets.clone()));
        }
        if let Err(e) = self.profiles.store.set_current(&info.name) {
            warn!("remembering the profile in use: {e}");
        }
        self.profiles.current = Some(info.name.clone());
        self.profiles.baseline = Some(cfg.clone());
        let restart = super::settings::device_settings(&cfg) != super::settings::device_settings(&self.startup);
        let mut said = format!("Loaded {}.", info.name);
        if restart {
            said.push_str(" Equipment settings changed: restart StreamIt to use them.");
        }
        let devices = self.devices.borrow().clone();
        if (videohub || web)
            && let Some(d) = devices
        {
            said.push_str(" Putting the device backups back…");
            self.profiles
                .restore_job
                .start(&self.rt, &self.ctx, async move { profiles::restore(&d, &backups, videohub, web).await });
        }
        self.notice = Some(said);
    }

    pub(super) fn poll_profiles(&mut self) {
        if let Some(r) = self.profiles.save_job.take() {
            match r {
                Ok(Saved { info, config }) => {
                    info!(name = %info.name, devices = %device_line(&info), "profile saved");
                    if let Err(e) = self.profiles.store.set_current(&info.name) {
                        warn!("remembering the profile in use: {e}");
                    }
                    self.notice = Some(format!("Saved {}: settings and {}.", info.name, device_line(&info)));
                    self.profiles.current = Some(info.name);
                    self.profiles.baseline = Some(config);
                    self.profiles.list = self.profiles.store.list();
                }
                Err(e) => {
                    warn!("saving a profile: {e}");
                    self.notice = Some(format!("Could not save the profile: {e}"));
                }
            }
        }
        if let Some(r) = self.profiles.restore_job.take() {
            self.notice = Some(match r {
                Ok(done) if done.is_empty() => "The device backups were already in place.".into(),
                Ok(done) => {
                    info!(done = %done, "device backups put back");
                    format!("Device backups put back: {done}.")
                }
                Err(e) => {
                    warn!("putting device backups back: {e}");
                    format!("Could not put every device backup back: {e}")
                }
            });
        }
    }

    /// The Profiles section's questions: a new profile's name, which backups to put back on
    /// loading, and whether to delete one.
    pub(super) fn profile_dialogs(&mut self, ctx: &egui::Context) {
        let Some(ask) = self.profiles.ask.take() else { return };
        let mut keep = true;
        let mut then: Option<Then> = None;
        let ask = match ask {
            Ask::SaveNew { mut name, mut error } => {
                egui::Modal::new(egui::Id::unique("profile-save")).show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    ui.heading("Save as a new profile");
                    ui.label("Saves StreamIt's settings and reads every connected device now.");
                    ui.add_space(6.0);
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .hint_text("e.g. Wedding 9 Nov")
                            .char_limit(profiles::NAME_MAX)
                            .desired_width(f32::INFINITY),
                    );
                    if !r.has_focus() && !r.lost_focus() {
                        r.request_focus();
                    }
                    if let Some(e) = &error {
                        ui.label(RichText::new(e).color(theme::AMBER_SOFT));
                    }
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                        }
                        if ui.button(RichText::new("Save").strong()).clicked() || enter {
                            match profiles::valid_name(&name) {
                                Ok(n) if self.profiles.store.exists(&n) => {
                                    error = Some(format!("There is already a profile called {n}."));
                                }
                                Ok(n) => {
                                    keep = false;
                                    then = Some(Box::new(move |app| app.save_profile(n)));
                                }
                                Err(e) => error = Some(e),
                            }
                        }
                    });
                });
                Ask::SaveNew { name, error }
            }
            Ask::Load { info, mut videohub, mut web } => {
                egui::Modal::new(egui::Id::unique("profile-load")).show(ctx, |ui| {
                    ui.set_max_width(520.0);
                    ui.heading(format!("Load {}?", info.name));
                    // The profile in use, when its settings have changed since it was saved or loaded.
                    let unsaved = self
                        .profiles
                        .current
                        .clone()
                        .filter(|_| self.profiles.baseline.as_ref() != Some(&*self.config.read().unwrap_or_else(|e| e.into_inner())));
                    if let Some(cur) = &unsaved {
                        ui.label(
                            RichText::new(format!("{cur} has changes that are not saved in it. They are lost unless you save them first."))
                                .color(theme::AMBER_SOFT),
                        );
                    }
                    ui.label("StreamIt's settings, the Routing presets and the cue list are replaced. Equipment addresses apply after a restart.");
                    ui.add_space(6.0);
                    let tick = |ui: &mut Ui, on: &mut bool, have: bool, text: &str, sub: &str| {
                        ui.add_enabled_ui(have, |ui| {
                            ui.checkbox(on, RichText::new(text).strong());
                            let sub = if have { sub } else { "No backup: it was not connected when this profile was saved." };
                            ui.label(RichText::new(sub).size(13.0).color(theme::MUTED));
                        });
                    };
                    tick(ui, &mut videohub, info.has(VIDEOHUB), "Put the VideoHub back", "Its input and output names, and every route.");
                    tick(ui, &mut web, info.has(WEB_PRESENTER), "Put the Web Presenter back", "Its streaming service, server and quality (never the stream key).");
                    ui.label(RichText::new("The ATEM notes are kept for reference only: StreamIt cannot put an ATEM's set-up back.").size(13.0).color(theme::MUTED));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                        }
                        if let Some(cur) = unsaved.clone()
                            && ui.button(format!("Save {cur} first")).clicked()
                        {
                            keep = false;
                            then = Some(Box::new(move |app| app.save_profile(cur)));
                        }
                        if ui.button(RichText::new("Load").strong()).clicked() {
                            keep = false;
                            let (i, v, w) = (info.clone(), videohub && info.has(VIDEOHUB), web && info.has(WEB_PRESENTER));
                            then = Some(Box::new(move |app| app.load_profile(&i, v, w)));
                        }
                    });
                });
                Ask::Load { info, videohub, web }
            }
            Ask::Delete { name } => {
                egui::Modal::new(egui::Id::unique("profile-delete")).show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    ui.heading(format!("Delete {name}?"));
                    ui.label("Its folder and every backup in it go to the Recycle Bin. The settings in use now are not touched.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Keep it").strong()).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            keep = false;
                        }
                        if ui.add(egui::Button::new(RichText::new("Delete").color(Color32::WHITE)).fill(theme::RED_FILL).stroke(Stroke::NONE)).clicked() {
                            keep = false;
                            let n = name.clone();
                            then = Some(Box::new(move |app| {
                                match app.profiles.store.delete(&n) {
                                    Ok(()) => {
                                        info!(name = %n, "profile deleted (Recycle Bin)");
                                        if app.profiles.current.as_deref() == Some(n.as_str()) {
                                            app.profiles.current = None;
                                            app.profiles.baseline = None;
                                        }
                                        app.notice = Some(format!("{n} is in the Recycle Bin."));
                                    }
                                    Err(e) => app.notice = Some(format!("Could not delete {n}: {e}")),
                                }
                                app.profiles.list = app.profiles.store.list();
                            }));
                        }
                    });
                });
                Ask::Delete { name }
            }
        };
        if keep {
            self.profiles.ask = Some(ask);
        }
        if let Some(f) = then {
            f(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_wait_for_simulation_and_unsaved_settings() {
        assert!(blocked(true, false).unwrap().contains("simulation"));
        assert!(blocked(false, true).unwrap().contains("Settings changes"));
        assert!(blocked(true, true).unwrap().contains("simulation"), "simulation is said first");
        assert_eq!(blocked(false, false), None);
    }
}
