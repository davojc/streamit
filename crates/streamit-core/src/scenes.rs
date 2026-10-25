//! Scenes: one press puts an input on the hall screens (a Screens ATEM cut) and fires a
//! lighting cue. A video can name a scene to go to while it plays; when it ends StreamIt
//! goes back to the scene before it.
use serde::{Deserialize, Serialize};

/// The most scenes there can be (one Stream Deck page of them).
pub const MAX_SCENES: usize = 12;
/// The most characters a scene's name keeps.
pub const SCENE_NAME_MAX: usize = 24;

/// A lighting cue: cue `cue` (e.g. "5" or "5.5") in cue list `list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightCue {
    pub list: u32,
    pub cue: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scene {
    /// Stays the same when the scene is renamed or moved, so a video keeps its scene.
    pub id: u32,
    pub name: String,
    /// The text on its Stream Deck key; the name in capitals when blank.
    #[serde(default)]
    pub key: String,
    /// The Screens ATEM input it puts on the hall screens; `None` leaves them as they are.
    #[serde(default)]
    pub screens: Option<u16>,
    /// The lighting cue it fires; `None` changes no lights.
    #[serde(default)]
    pub cue: Option<LightCue>,
}

impl Scene {
    /// A new scene with the next free id, named "Scene n", that changes nothing yet.
    pub fn new(scenes: &[Scene]) -> Scene {
        let id = scenes.iter().map(|s| s.id).max().map_or(1, |m| m + 1);
        Scene { id, name: format!("Scene {}", scenes.len() + 1), key: String::new(), screens: None, cue: None }
    }

    /// What its Stream Deck key says.
    pub fn key_text(&self) -> String {
        let k = self.key.trim();
        if k.is_empty() { self.name.trim().to_uppercase() } else { k.to_string() }
    }
}

/// The scene with `id`.
pub fn find(scenes: &[Scene], id: u32) -> Option<&Scene> {
    scenes.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_scenes_get_fresh_ids_and_keys_default_to_the_name() {
        let mut list = vec![Scene::new(&[])];
        assert_eq!((list[0].id, list[0].name.as_str()), (1, "Scene 1"));
        list[0].id = 7;
        let s = Scene::new(&list);
        assert_eq!((s.id, s.name.as_str()), (8, "Scene 2"));
        let mut worship = Scene { name: "Worship".into(), ..s };
        assert_eq!(worship.key_text(), "WORSHIP");
        worship.key = " SING ".into();
        assert_eq!(worship.key_text(), "SING");
        assert_eq!(find(&[worship.clone()], 8), Some(&worship));
        assert_eq!(find(&[worship], 9), None);
    }
}
