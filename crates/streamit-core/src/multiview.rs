//! The MultiView 4. StreamIt does not control it: its four views are fed by four VideoHub
//! outputs, so changing what a view shows is a VideoHub route. Presets save the input on
//! each view and put them back.
use serde::{Deserialize, Serialize};

/// The views in the order the config and the commands number them (0..4).
pub const VIEWS: [&str; 4] = ["Top left", "Top right", "Bottom left", "Bottom right"];
/// Short names, for the Stream Deck and the input badges.
pub const VIEW_KEYS: [&str; 4] = ["TL", "TR", "BL", "BR"];
/// Preset slots, numbered from 1.
pub const PRESET_SLOTS: u8 = 8;
/// The most characters a preset name keeps.
pub const PRESET_NAME_MAX: usize = 24;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MultiViewConfig {
    /// The VideoHub output feeding each view (top left, top right, bottom left, bottom
    /// right); 0 means not set.
    pub outputs: [u16; 4],
    /// Saved in place, like the cue list: Save in Settings does not cover them.
    pub presets: Vec<Preset>,
}

/// A saved set of inputs, one per view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    /// 1..=PRESET_SLOTS.
    pub slot: u8,
    pub name: String,
    pub inputs: [u16; 4],
}

/// What the window and the Stream Deck show of the MultiView.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MultiView {
    /// The VideoHub input on each view, as the VideoHub reports it; `None` when the view
    /// has no output set or the VideoHub has not said.
    pub views: [Option<u16>; 4],
    /// The VideoHub's input labels; index 0 is input 1. Empty until the VideoHub answers.
    pub labels: Vec<String>,
    pub presets: Vec<Preset>,
}

impl MultiView {
    /// The input's VideoHub label, or "Input n" when it has none.
    pub fn label(&self, input: u16) -> String {
        input
            .checked_sub(1)
            .and_then(|i| self.labels.get(i as usize))
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map_or_else(|| format!("Input {input}"), str::to_owned)
    }

    pub fn preset(&self, slot: u8) -> Option<&Preset> {
        self.presets.iter().find(|p| p.slot == slot)
    }

    /// The preset the views show now: every view known and equal.
    pub fn is_current(&self, p: &Preset) -> bool {
        self.views.iter().zip(p.inputs).all(|(v, i)| *v == Some(i))
    }
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

    fn p(slot: u8, name: &str, inputs: [u16; 4]) -> Preset {
        Preset { slot, name: name.into(), inputs }
    }

    #[test]
    fn labels_fall_back_to_the_input_number() {
        let mv = MultiView { labels: vec!["Cam 1".into(), "  ".into()], ..MultiView::default() };
        assert_eq!(mv.label(1), "Cam 1");
        assert_eq!(mv.label(2), "Input 2", "a blank label");
        assert_eq!(mv.label(30), "Input 30", "past the labels the VideoHub sent");
        assert_eq!(mv.label(0), "Input 0");
    }

    #[test]
    fn saving_replaces_the_slot_and_keeps_slot_order() {
        let a = vec![p(3, "C", [1, 2, 3, 4]), p(1, "A", [1, 1, 1, 1])];
        let b = with_preset(&a, p(2, "B", [2, 2, 2, 2]));
        assert_eq!(b.iter().map(|x| x.slot).collect::<Vec<_>>(), [1, 2, 3]);
        let c = with_preset(&b, p(3, "New", [5, 6, 7, 8]));
        assert_eq!(c.len(), 3);
        assert_eq!(c[2], p(3, "New", [5, 6, 7, 8]));
        assert_eq!(without_preset(&c, 2).iter().map(|x| x.slot).collect::<Vec<_>>(), [1, 3]);
    }

    #[test]
    fn the_current_preset_needs_every_view_to_match() {
        let pre = p(1, "A", [1, 2, 3, 4]);
        let mut mv = MultiView { views: [Some(1), Some(2), Some(3), Some(4)], ..MultiView::default() };
        assert!(mv.is_current(&pre));
        mv.views[3] = None;
        assert!(!mv.is_current(&pre), "an unknown view is not a match");
        mv.views[3] = Some(9);
        assert!(!mv.is_current(&pre));
    }

    #[test]
    fn preset_names_are_trimmed_capped_and_never_blank() {
        assert_eq!(preset_name("  Sermon  ", 4), "Sermon");
        assert_eq!(preset_name("   ", 4), "Preset 4");
        assert_eq!(preset_name(&"x".repeat(40), 1).chars().count(), PRESET_NAME_MAX);
    }
}
