//! An ATEM's own names for its inputs, kept on the ATEM so its multiview shows them. A
//! tile shows the ATEM's name with the VideoHub input feeding it under it; double-clicking
//! the name edits it in place (Enter sends it to the ATEM, Escape puts it back). The short
//! name on the ATEM's buttons is made from the long one.
use super::super::StreamItApp;
use crate::theme::{self, Kind};
use eframe::egui::{self, Rect, RichText, Ui, vec2};
use streamit_core::devices::{INPUT_NAME_MAX, short_name};
use streamit_core::engine::{Command, Snapshot};
use streamit_core::routing::{RoutingState, Target};
use tracing::info;

/// `text` cut to what an ATEM keeps.
pub(super) fn fit_name(text: &str) -> String {
    text.trim().chars().take(INPUT_NAME_MAX).collect::<String>().trim_end().to_string()
}

/// The VideoHub name for ATEM input `input` on `target`: the label of the router input
/// feeding it, if that has one.
pub(super) fn hub_name(routing: &RoutingState, target: Target, input: u16) -> Option<String> {
    let t = routing.target(target);
    let i = usize::from(input.checked_sub(1)?);
    (t.output(i) != 0).then_some(())?;
    routing.own_label(t.dest(i)?).map(fit_name).filter(|n| !n.is_empty())
}

/// The ATEM inputs whose name differs from the VideoHub's, with the VideoHub's name.
pub(super) fn names_from_hub(routing: &RoutingState, target: Target) -> Vec<(u16, String)> {
    let t = routing.target(target);
    (1..=target.size() as u16)
        .filter_map(|n| {
            let name = hub_name(routing, target, n)?;
            let now = t.names.get(&n).map(|x| x.long.trim());
            (now != Some(name.as_str())).then_some((n, name))
        })
        .collect()
}

/// An ATEM input's name being typed on its tile.
pub(super) struct Naming {
    target: Target,
    input: u16,
    text: String,
    /// Its name when editing started.
    was: String,
}

impl StreamItApp {
    fn rename_input(&mut self, target: Target, input: u16, long: String) {
        info!(target = target.name(), input, name = %long, "ATEM input renamed from the Routing tab");
        let short = short_name(&long);
        self.send(Command::SetInputName { target, input, long, short });
    }

    /// Starts editing ATEM input `input`'s name on its tile.
    pub(super) fn start_naming(&mut self, snap: &Snapshot, target: Target, input: u16) {
        let now = snap.routing.target(target).names.get(&input).map(|n| n.long.clone()).unwrap_or_default();
        self.routes_ui.atem_naming = Some(Naming { target, input, text: now.clone(), was: now });
    }

    /// The menu items for naming ATEM input `input`.
    pub(super) fn name_menu(&mut self, ui: &mut Ui, snap: &Snapshot, target: Target, input: u16) {
        let known = !snap.routing.target(target).names.is_empty();
        let r = ui.add_enabled(known, egui::Button::new("Rename on the ATEM…"));
        if r.on_disabled_hover_text("The ATEM has not said what its inputs are called yet.").clicked() {
            self.start_naming(snap, target, input);
            ui.close();
        }
        let hub = hub_name(&snap.routing, target, input);
        let text = match &hub {
            Some(n) => format!("Use the VideoHub name ({n})"),
            None => "Use the VideoHub name".to_string(),
        };
        let r = ui.add_enabled(known && hub.is_some(), egui::Button::new(text));
        if let Some(name) = hub.filter(|_| r.clicked()) {
            self.rename_input(target, input, name);
            ui.close();
        }
    }

    /// The name field over `rect` while input `input` is being named; true while it shows.
    pub(super) fn naming_field(&mut self, ui: &mut Ui, rect: Rect, target: Target, input: u16) -> bool {
        let Some(mut naming) = self.routes_ui.atem_naming.take() else { return false };
        if (naming.target, naming.input) != (target, input) {
            self.routes_ui.atem_naming = Some(naming);
            return false;
        }
        let text = &mut naming.text;
        let field = Rect::from_center_size(rect.center(), vec2(rect.width() - 16.0, 26.0));
        let r = ui.put(
            field,
            egui::TextEdit::singleline(text)
                .id(egui::Id::unique(("atem-name", target.index(), input)))
                .font(theme::body_bold(14.0))
                .align(egui::Align2::CENTER_CENTER)
                .desired_width(f32::INFINITY)
                .char_limit(INPUT_NAME_MAX),
        );
        if !r.has_focus() && !r.lost_focus() {
            r.request_focus();
        }
        if r.lost_focus() {
            let name = fit_name(text);
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && name != fit_name(&naming.was) {
                self.rename_input(target, input, name);
            }
        } else {
            self.routes_ui.atem_naming = Some(naming);
        }
        true
    }

    /// "Name every input from the VideoHub", under the tiles.
    pub(super) fn names_from_hub_button(&mut self, ui: &mut Ui, snap: &Snapshot, target: Target) {
        let known = !snap.routing.target(target).names.is_empty();
        let changes = names_from_hub(&snap.routing, target);
        ui.horizontal(|ui| {
            let r = theme::button_enabled(
                ui,
                known && !changes.is_empty(),
                "Name every input from the VideoHub",
                Kind::Plain,
            );
            let why = if known {
                "Every input already has its VideoHub name."
            } else {
                "The ATEM has not said what its inputs are called yet."
            };
            if r.on_disabled_hover_text(why).clicked() {
                for (n, name) in changes.clone() {
                    self.rename_input(target, n, name);
                }
            }
            ui.label(
                RichText::new("Double-click a name to rename it on the ATEM, for its multiview.")
                    .size(13.0)
                    .color(theme::MUTED),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamit_core::devices::InputName;

    fn routing() -> RoutingState {
        let mut r = RoutingState {
            labels: vec!["Camera 1".into(), "".into(), "A very long camera name here".into()],
            ..RoutingState::default()
        };
        let t = r.target_mut(Target::ScreensAtem);
        t.outputs = vec![5, 6, 7, 0, 0, 0, 0, 0];
        t.dests = vec![Some(1), Some(2), Some(3), Some(1), None, None, None, None];
        t.names.insert(1, InputName { long: "Camera 1".into(), short: "CAM1".into(), default: true });
        t.names.insert(3, InputName { long: "Lyrics".into(), short: "LYRI".into(), default: false });
        r
    }

    #[test]
    fn the_videohub_name_is_the_feeding_inputs_label() {
        let r = routing();
        assert_eq!(hub_name(&r, Target::ScreensAtem, 1).as_deref(), Some("Camera 1"));
        assert_eq!(hub_name(&r, Target::ScreensAtem, 2), None, "no label");
        assert_eq!(hub_name(&r, Target::ScreensAtem, 3).as_deref(), Some("A very long camera n"), "cut to 20");
        assert_eq!(hub_name(&r, Target::ScreensAtem, 4), None, "not fed by the router");
        assert_eq!(hub_name(&r, Target::ScreensAtem, 0), None);
    }

    #[test]
    fn naming_from_the_videohub_skips_inputs_already_named_so() {
        assert_eq!(names_from_hub(&routing(), Target::ScreensAtem), vec![(3, "A very long camera n".to_string())]);
    }

    #[test]
    fn names_are_cut_to_what_the_atem_keeps() {
        assert_eq!(fit_name("  Pulpit  "), "Pulpit");
        assert_eq!(fit_name("Twenty characters is  the most"), "Twenty characters is");
        assert_eq!(short_name("Pulpit Cam"), "PULP");
        assert_eq!(short_name("2nd camera"), "2ND");
        assert_eq!(short_name(""), "");
    }
}
