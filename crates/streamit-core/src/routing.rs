//! Routing through the VideoHub. Three things are fed by VideoHub outputs: the MultiView 4's
//! four views and the inputs of the two ATEMs. StreamIt controls none of them directly for
//! this: changing what a destination shows is a VideoHub route to the output feeding it.
//! Each target has eight presets that save the input on each destination and put them back.
use serde::{Deserialize, Serialize};

/// What the VideoHub feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Target {
    MultiView,
    StreamAtem,
    ScreensAtem,
}

/// The MultiView's views in the order the config and the commands number them (0..4).
pub const VIEWS: [&str; 4] = ["Top left", "Top right", "Bottom left", "Bottom right"];
/// Short names of the views, for the Stream Deck and the input badges.
pub const VIEW_KEYS: [&str; 4] = ["TL", "TR", "BL", "BR"];
/// Inputs on each ATEM that can be fed by the VideoHub.
pub const ATEM_INPUTS: usize = 8;
/// The Stream ATEM input StreamIt plays videos into (its VideoHub output is
/// `videohub.input8_output`). It is locked while a StreamIt sequence runs.
pub const STREAM_VIDEO_INPUT: usize = 7;
/// Preset slots per target, numbered from 1.
pub const PRESET_SLOTS: u8 = 8;
/// The most characters a preset name keeps.
pub const PRESET_NAME_MAX: usize = 24;

impl Target {
    pub const ALL: [Target; 3] = [Target::MultiView, Target::StreamAtem, Target::ScreensAtem];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Target::MultiView => "MultiView",
            Target::StreamAtem => "Stream ATEM",
            Target::ScreensAtem => "Screens ATEM",
        }
    }

    /// How many destinations it has.
    pub fn size(self) -> usize {
        match self {
            Target::MultiView => VIEWS.len(),
            _ => ATEM_INPUTS,
        }
    }

    pub fn is_atem(self) -> bool {
        self != Target::MultiView
    }

    /// The destination's name: a view, or an ATEM input.
    pub fn dest_name(self, i: usize) -> String {
        match self {
            Target::MultiView => VIEWS.get(i).map_or_else(String::new, |v| v.to_string()),
            _ => format!("Input {}", i + 1),
        }
    }

    /// A short name for a destination: TL..BR, S1..S8 (Stream ATEM), H1..H8 (hall screens).
    pub fn dest_key(self, i: usize) -> String {
        match self {
            Target::MultiView => VIEW_KEYS.get(i).map_or_else(String::new, |v| v.to_string()),
            Target::StreamAtem => format!("S{}", i + 1),
            Target::ScreensAtem => format!("H{}", i + 1),
        }
    }
}

/// Is the destination locked now? Stream ATEM input 8 carries StreamIt's videos and the
/// holding still, so it cannot be routed while a sequence (`busy`) runs.
pub fn locked(target: Target, dest: usize, busy: bool) -> bool {
    busy && target == Target::StreamAtem && dest == STREAM_VIDEO_INPUT
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingConfig {
    /// The VideoHub output feeding each destination, in order; 0 (or missing) means not
    /// fed by the router.
    pub outputs: Vec<u16>,
    /// Saved in place, like the cue list: Save in Settings does not cover them.
    pub presets: Vec<Preset>,
}

impl RoutingConfig {
    pub fn output(&self, i: usize) -> u16 {
        self.outputs.get(i).copied().unwrap_or(0)
    }

    /// `outputs`, exactly `size` long.
    pub fn outputs_for(&self, size: usize) -> Vec<u16> {
        (0..size).map(|i| self.output(i)).collect()
    }
}

/// A saved set of inputs, one per destination; 0 where the destination is not routed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    /// 1..=PRESET_SLOTS.
    pub slot: u8,
    pub name: String,
    pub inputs: Vec<u16>,
}

impl Preset {
    pub fn input(&self, i: usize) -> u16 {
        self.inputs.get(i).copied().unwrap_or(0)
    }
}

/// One target as the VideoHub and the ATEM report it, for the window and the deck.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetState {
    /// The VideoHub output feeding each destination (0 = not from the router).
    pub outputs: Vec<u16>,
    /// The VideoHub input on each destination now; `None` when not fed or not known.
    pub dests: Vec<Option<u16>>,
    pub presets: Vec<Preset>,
    /// The ATEM's program and preview inputs (1-based); `None` for the MultiView or when
    /// the ATEM has not said.
    pub program: Option<u16>,
    pub preview: Option<u16>,
}

impl TargetState {
    pub fn output(&self, i: usize) -> u16 {
        self.outputs.get(i).copied().unwrap_or(0)
    }

    pub fn dest(&self, i: usize) -> Option<u16> {
        self.dests.get(i).copied().flatten()
    }

    pub fn preset(&self, slot: u8) -> Option<&Preset> {
        self.presets.iter().find(|p| p.slot == slot)
    }

    /// Destination `i` is on program on its ATEM now.
    pub fn on_air(&self, i: usize) -> bool {
        self.program == Some(i as u16 + 1)
    }

    pub fn on_preview(&self, i: usize) -> bool {
        self.preview == Some(i as u16 + 1)
    }

    /// The routes recalling `p` needs: router-fed destinations whose input differs.
    pub fn changes(&self, p: &Preset) -> Vec<(usize, u16)> {
        (0..self.outputs.len())
            .filter(|&i| self.output(i) != 0 && p.input(i) != 0 && self.dest(i) != Some(p.input(i)))
            .map(|i| (i, p.input(i)))
            .collect()
    }

    /// The preset is what the destinations show now (every router-fed one it covers).
    pub fn is_current(&self, p: &Preset) -> bool {
        let fed: Vec<usize> = (0..self.outputs.len()).filter(|&i| self.output(i) != 0 && p.input(i) != 0).collect();
        !fed.is_empty() && fed.iter().all(|&i| self.dest(i) == Some(p.input(i)))
    }

    /// Any of `routes` changes the input on program.
    pub fn touches_on_air(&self, routes: &[(usize, u16)]) -> bool {
        routes.iter().any(|&(i, _)| self.on_air(i))
    }

    /// The inputs to save as a preset, or `None` while a router-fed destination is unknown.
    pub fn snapshot_inputs(&self) -> Option<Vec<u16>> {
        (0..self.outputs.len())
            .map(|i| if self.output(i) == 0 { Some(0) } else { self.dest(i) })
            .collect::<Option<Vec<u16>>>()
            .filter(|v| v.iter().any(|&x| x != 0))
    }
}

/// What the window and the deck show of the routing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingState {
    /// The VideoHub's input labels; index 0 is input 1. Empty until the VideoHub answers.
    pub labels: Vec<String>,
    pub targets: [TargetState; 3],
}

impl RoutingState {
    pub fn target(&self, t: Target) -> &TargetState {
        &self.targets[t.index()]
    }

    pub fn target_mut(&mut self, t: Target) -> &mut TargetState {
        &mut self.targets[t.index()]
    }

    /// The input's VideoHub label, or "Input n" when it has none.
    pub fn label(&self, input: u16) -> String {
        self.own_label(input).map_or_else(|| format!("Input {input}"), str::to_owned)
    }

    /// The input's VideoHub label, if it has one.
    pub fn own_label(&self, input: u16) -> Option<&str> {
        input.checked_sub(1).and_then(|i| self.labels.get(i as usize)).map(|l| l.trim()).filter(|l| !l.is_empty())
    }
}

/// One target as read from the devices (the health poll sends these).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetRead {
    pub outputs: Vec<u16>,
    pub dests: Vec<Option<u16>>,
    pub program: Option<u16>,
    pub preview: Option<u16>,
}

/// `presets` with `p` in its slot (replacing what was there), in slot order.
pub fn with_preset(presets: &[Preset], p: Preset) -> Vec<Preset> {
    let mut out: Vec<Preset> = presets.iter().filter(|x| x.slot != p.slot).cloned().collect();
    out.push(p);
    out.sort_by_key(|x| x.slot);
    out
}

/// `presets` without the one in `slot`.
pub fn without_preset(presets: &[Preset], slot: u8) -> Vec<Preset> {
    presets.iter().filter(|x| x.slot != slot).cloned().collect()
}

/// The name a preset is saved under: trimmed, at most `PRESET_NAME_MAX` characters,
/// "Preset n" when blank.
pub fn preset_name(typed: &str, slot: u8) -> String {
    let t: String = typed.trim().chars().take(PRESET_NAME_MAX).collect();
    let t = t.trim_end().to_string();
    if t.is_empty() { format!("Preset {slot}") } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(slot: u8, name: &str, inputs: &[u16]) -> Preset {
        Preset { slot, name: name.into(), inputs: inputs.to_vec() }
    }

    fn atem(outputs: &[u16], dests: &[Option<u16>], program: u16) -> TargetState {
        TargetState {
            outputs: outputs.to_vec(),
            dests: dests.to_vec(),
            presets: vec![],
            program: Some(program),
            preview: None,
        }
    }

    #[test]
    fn targets_name_their_destinations() {
        assert_eq!(Target::MultiView.size(), 4);
        assert_eq!(Target::StreamAtem.size(), 8);
        assert_eq!(Target::MultiView.dest_name(2), "Bottom left");
        assert_eq!(Target::StreamAtem.dest_name(7), "Input 8");
        assert_eq!(Target::StreamAtem.dest_key(0), "S1");
        assert_eq!(Target::ScreensAtem.dest_key(4), "H5");
        assert_eq!(Target::MultiView.dest_key(3), "BR");
        assert!(!Target::MultiView.is_atem() && Target::ScreensAtem.is_atem());
        assert_eq!(Target::ALL.map(Target::index), [0, 1, 2]);
    }

    #[test]
    fn only_stream_input_8_locks_and_only_while_busy() {
        assert!(locked(Target::StreamAtem, 7, true));
        assert!(!locked(Target::StreamAtem, 7, false));
        assert!(!locked(Target::StreamAtem, 6, true));
        assert!(!locked(Target::ScreensAtem, 7, true));
        assert!(!locked(Target::MultiView, 3, true));
    }

    #[test]
    fn labels_fall_back_to_the_input_number() {
        let r = RoutingState { labels: vec!["Cam 1".into(), "  ".into()], ..RoutingState::default() };
        assert_eq!(r.label(1), "Cam 1");
        assert_eq!(r.label(2), "Input 2", "a blank label");
        assert_eq!(r.own_label(2), None);
        assert_eq!(r.label(30), "Input 30");
        assert_eq!(r.label(0), "Input 0");
    }

    #[test]
    fn a_preset_changes_only_router_fed_destinations_that_differ() {
        let t = atem(&[1, 2, 0, 11], &[Some(1), Some(2), None, Some(13)], 2);
        let pre = p(1, "Band", &[1, 25, 0, 15]);
        assert_eq!(t.changes(&pre), vec![(1, 25), (3, 15)]);
        assert!(t.touches_on_air(&t.changes(&pre)), "input 2 is on program");
        assert!(!t.touches_on_air(&[(3, 15)]));
        assert!(!t.is_current(&pre));
        assert!(t.is_current(&p(2, "Now", &[1, 2, 0, 13])));
    }

    #[test]
    fn saving_needs_every_router_fed_destination_known() {
        let mut t = atem(&[1, 0, 3], &[Some(5), None, None], 1);
        assert_eq!(t.snapshot_inputs(), None, "input 3 is fed but unknown");
        t.dests[2] = Some(7);
        assert_eq!(t.snapshot_inputs(), Some(vec![5, 0, 7]));
        let empty = atem(&[0, 0], &[None, None], 1);
        assert_eq!(empty.snapshot_inputs(), None, "nothing is fed by the router");
    }

    #[test]
    fn saving_replaces_the_slot_and_keeps_slot_order() {
        let a = vec![p(3, "C", &[1, 2, 3, 4]), p(1, "A", &[1, 1, 1, 1])];
        let b = with_preset(&a, p(2, "B", &[2, 2, 2, 2]));
        assert_eq!(b.iter().map(|x| x.slot).collect::<Vec<_>>(), [1, 2, 3]);
        let c = with_preset(&b, p(3, "New", &[5, 6, 7, 8]));
        assert_eq!(c.len(), 3);
        assert_eq!(c[2], p(3, "New", &[5, 6, 7, 8]));
        assert_eq!(without_preset(&c, 2).iter().map(|x| x.slot).collect::<Vec<_>>(), [1, 3]);
    }

    #[test]
    fn preset_names_are_trimmed_capped_and_never_blank() {
        assert_eq!(preset_name("  Sermon  ", 4), "Sermon");
        assert_eq!(preset_name("   ", 4), "Preset 4");
        assert_eq!(preset_name(&"x".repeat(40), 1).chars().count(), PRESET_NAME_MAX);
    }

    #[test]
    fn config_outputs_pad_to_the_target_size() {
        let c = RoutingConfig { outputs: vec![37, 38], presets: vec![] };
        assert_eq!(c.outputs_for(4), vec![37, 38, 0, 0]);
        assert_eq!(c.output(9), 0);
    }

    #[test]
    fn a_0_1_4_multiview_section_still_loads() {
        // 0.1.4 wrote fixed arrays of four; they read as lists now.
        let text = "outputs = [37, 38, 39, 40]\n[[presets]]\nslot = 1\nname = \"Service\"\ninputs = [6, 7, 39, 2]\n";
        let c: RoutingConfig = toml::from_str(text).unwrap();
        assert_eq!(c.outputs, vec![37, 38, 39, 40]);
        assert_eq!(c.presets, vec![p(1, "Service", &[6, 7, 39, 2])]);
    }
}
