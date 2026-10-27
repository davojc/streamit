//! The Scenes tab: the scenes in order, with GO on each, and the selected one's settings.
//! A scene puts a Screens ATEM input on the hall screens and fires a lighting cue.
//! Changes save straight away, like the Routing presets.
use super::StreamItApp;
use crate::theme::{self, Kind};
use crate::view;
use eframe::egui::{self, Align, Layout, RichText, Ui, vec2};
use streamit_core::config::Config;
use streamit_core::engine::{Command, Snapshot};
use streamit_core::routing::{ATEM_INPUTS, Target};
use streamit_core::scenes::{self, LightCue, MAX_SCENES, SCENE_NAME_MAX, Scene};

const GAP: f32 = 20.0;

/// What a scene changes, in a few words: "Screens: Lyrics · Lights: cue 1/2".
pub(super) fn scene_summary(snap: &Snapshot, c: &Config, s: &Scene) -> (String, String) {
    let screens = match s.screens {
        Some(n) => view::atem_input_name(snap, c, Target::ScreensAtem, n),
        None => "as they are".into(),
    };
    let lights = match &s.cue {
        Some(q) if q.cue.trim().is_empty() => "no cue number yet".into(),
        Some(q) => format!("cue {}/{}", q.list, q.cue),
        None => "no change".into(),
    };
    (format!("Screens: {screens}"), format!("Lights: {lights}"))
}

impl StreamItApp {
    /// `c` is the config the engine runs on, for naming the Screens ATEM's inputs.
    pub(super) fn scenes_tab(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config) {
        let list = snap.scenes.clone();
        if self.scene_sel.is_none_or(|id| scenes::find(&list, id).is_none()) {
            self.scene_sel = list.first().map(|s| s.id);
        }
        // The text being typed lives in the draft until the field is left.
        if self.scene_draft.as_ref().map(|d| d.id) != self.scene_sel {
            self.scene_draft = self.scene_sel.and_then(|id| scenes::find(&list, id).cloned());
        }
        let w = ui.available_width();
        let side = if w < 900.0 { w } else { 440.0_f32.min(w * 0.42) };
        let main = if w < 900.0 { w } else { w - GAP - side };
        let mut new_list: Option<Vec<Scene>> = None;
        let layout = if w < 900.0 { Layout::top_down(Align::Min) } else { Layout::left_to_right(Align::Min) };
        ui.with_layout(layout, |ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            ui.allocate_ui_with_layout(vec2(main, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(main);
                if let Some(l) = self.scene_list(ui, snap, c, &list) {
                    new_list = Some(l);
                }
            });
            ui.allocate_ui_with_layout(vec2(side, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(side);
                if let Some(l) = self.scene_editor(ui, snap, c, &list) {
                    new_list = Some(l);
                }
            });
        });
        if let Some(l) = new_list {
            self.save_scenes(l);
        }
        if let Some(id) = self.pending_go.take() {
            self.send(Command::GoScene(id));
        }
    }

    /// The scenes in order; returns the new list when one is added or moved.
    fn scene_list(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, list: &[Scene]) -> Option<Vec<Scene>> {
        let mut out = None;
        theme::panel().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::lbl(ui, &format!("Scenes · {} of {MAX_SCENES}", list.len()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let can = list.len() < MAX_SCENES;
                    let b = theme::button_enabled(ui, can, "Add scene", Kind::Plain);
                    if b.on_disabled_hover_text("Twelve is the most: one Stream Deck page").clicked() {
                        let s = Scene::new(list);
                        self.scene_sel = Some(s.id);
                        let mut l = list.to_vec();
                        l.push(s);
                        out = Some(l);
                    }
                });
            });
            ui.label(
                RichText::new(
                    "A scene puts an input on the hall screens and fires a lighting cue. GO uses it now; on the \
                     Service screen and the Stream Deck's Scenes page each is one press.",
                )
                .size(14.0)
                .color(theme::MUTED),
            );
            ui.add_space(6.0);
            if list.is_empty() {
                ui.label(RichText::new("No scenes yet. Add one to start.").color(theme::SOFT));
            }
            for (i, s) in list.iter().enumerate() {
                let selected = self.scene_sel == Some(s.id);
                let mut frame = theme::inset().inner_margin(egui::Margin::symmetric(14, 10));
                if selected {
                    frame = frame.fill(theme::BLUE_PANEL).stroke(egui::Stroke::new(1.0, theme::BLUE));
                }
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        ui.label(RichText::new(format!("{:<2}", i + 1)).font(theme::mono(15.0)).color(theme::DIM));
                        let text_w = (ui.available_width() - 200.0).max(120.0);
                        // The name and what it does select the scene; the buttons stay buttons.
                        let text = ui.allocate_ui_with_layout(vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            // Selectable text would take the click before the row sees it.
                            ui.style_mut().interaction.selectable_labels = false;
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(&s.name).font(theme::body_bold(17.0))).truncate(),
                                );
                                if snap.active_scene == Some(s.id) {
                                    theme::chip(ui, "On now", theme::AMBER_SOFT, theme::AMBER);
                                }
                            });
                            let (screens, lights) = scene_summary(snap, c, s);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                theme::chip(ui, &screens, theme::SOFT, theme::BUTTON_BORDER);
                                theme::chip(ui, &lights, theme::SOFT, theme::BUTTON_BORDER);
                            });
                        });
                        if text.response.interact(egui::Sense::click()).clicked() {
                            self.scene_sel = Some(s.id);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::button(ui, "GO", Kind::Warn).clicked() {
                                self.send(Command::GoScene(s.id));
                            }
                            if theme::button_enabled(ui, i + 1 < list.len(), "↓", Kind::Plain).clicked() {
                                let mut l = list.to_vec();
                                l.swap(i, i + 1);
                                out = Some(l);
                            }
                            if theme::button_enabled(ui, i > 0, "↑", Kind::Plain).clicked() {
                                let mut l = list.to_vec();
                                l.swap(i, i - 1);
                                out = Some(l);
                            }
                        });
                    });
                });
            }
        });
        out
    }

    /// The selected scene's settings; returns the new list when something changed.
    fn scene_editor(&mut self, ui: &mut Ui, snap: &Snapshot, c: &Config, list: &[Scene]) -> Option<Vec<Scene>> {
        let mut commit = false;
        let mut remove = false;
        let Some(sel) = self.scene_sel else {
            theme::panel().inner_margin(22).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::lbl(ui, "Selected scene");
                ui.label(RichText::new("Add a scene to set what it does.").color(theme::SOFT));
            });
            return None;
        };
        let number = list.iter().position(|s| s.id == sel).map_or(0, |i| i + 1);
        let mut draft = self.scene_draft.take()?;
        let d = &mut draft;
        let armed = self.scene_remove == Some(sel);
        let (mut go, mut arm) = (false, false);
        theme::panel().stroke(egui::Stroke::new(1.0, theme::BLUE)).inner_margin(22).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            theme::lbl(ui, &format!("Selected · scene {number}"));
            ui.add_space(4.0);
            theme::lbl(ui, "Name");
            let r =
                ui.add(egui::TextEdit::singleline(&mut d.name).char_limit(SCENE_NAME_MAX).desired_width(f32::INFINITY));
            commit |= r.lost_focus();
            theme::lbl(ui, "Name on the Stream Deck key");
            let r = ui.add(
                egui::TextEdit::singleline(&mut d.key)
                    .hint_text(d.name.trim().to_uppercase())
                    .char_limit(SCENE_NAME_MAX)
                    .desired_width(f32::INFINITY),
            );
            commit |= r.lost_focus();
            ui.add_space(8.0);
            theme::lbl(ui, "Hall screens show (Screens ATEM input)");
            let name = |n: u16| format!("{n} · {}", view::atem_input_name(snap, c, Target::ScreensAtem, n));
            let shown = d.screens.map_or_else(|| "Leave as they are".to_string(), name);
            egui::ComboBox::from_id_salt("scene-screens").selected_text(shown).width(ui.available_width()).show_ui(
                ui,
                |ui| {
                    commit |= ui.selectable_value(&mut d.screens, None, "Leave as they are").changed();
                    for n in 1..=ATEM_INPUTS as u16 {
                        commit |= ui.selectable_value(&mut d.screens, Some(n), name(n)).changed();
                    }
                },
            );
            ui.label(
                RichText::new("Names come from what the VideoHub feeds each input (Routing tab).")
                    .size(13.0)
                    .color(theme::MUTED),
            );
            ui.add_space(8.0);
            theme::lbl(ui, "Lighting cue (ETC Eos)");
            let mut lights = d.cue.is_some();
            if ui.checkbox(&mut lights, "Fire a lighting cue").changed() {
                d.cue = lights.then(|| LightCue { list: 1, cue: String::new() });
                commit = true;
            }
            if let Some(q) = d.cue.as_mut() {
                ui.horizontal(|ui| {
                    commit |= ui.add(egui::DragValue::new(&mut q.list).range(1..=999).prefix("List ")).changed();
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut q.cue).hint_text("Cue, e.g. 5 or 5.5").desired_width(140.0),
                    );
                    commit |= r.lost_focus();
                });
            }
            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                if theme::button(ui, "GO now", Kind::Warn).clicked() {
                    commit = true;
                    go = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let text = if armed { "Click again to remove" } else { "Remove" };
                    if theme::button(ui, text, Kind::DangerOutline).clicked() {
                        if armed {
                            remove = true;
                        } else {
                            arm = true;
                        }
                    }
                });
            });
            ui.label(
                RichText::new("Changes save straight away. Scenes are kept in the profile.")
                    .size(13.0)
                    .color(theme::MUTED),
            );
        });
        if arm {
            self.scene_remove = Some(sel);
        }
        if remove {
            self.scene_remove = None;
            return Some(list.iter().filter(|s| s.id != sel).cloned().collect());
        }
        self.scene_draft = Some(draft.clone());
        // The scene goes after its changes are saved, so it uses them.
        let changed = commit && scenes::find(list, sel) != Some(&draft);
        if go {
            self.pending_go = Some(sel);
        }
        if !changed {
            return None;
        }
        let mut d = draft;
        d.name = d.name.trim().to_string();
        if d.name.is_empty() {
            d.name = format!("Scene {number}");
        }
        if let Some(q) = d.cue.as_mut() {
            q.cue = q.cue.trim().to_string();
        }
        self.scene_draft = Some(d.clone());
        Some(list.iter().map(|s| if s.id == sel { d.clone() } else { s.clone() }).collect())
    }
}
