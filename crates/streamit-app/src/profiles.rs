//! Profiles: saved copies of all of StreamIt's settings, plus a backup of each device's
//! own settings where StreamIt can read them. Each profile is a folder under
//! `<settings folder>\profiles\<name>`:
//!
//! - `profile.toml`: its name, when it was saved, and which devices were backed up;
//! - `settings.toml`: everything in Settings, the Routing presets and the cue list;
//! - `devices\videohub.txt`: input and output names and the routing (can be put back);
//! - `devices\web-presenter.txt`: the streaming settings, never the stream key (can be put
//!   back);
//! - `devices\stream-atem.txt`, `devices\screens-atem.txt`: what StreamIt can read of the
//!   ATEMs (program, preview, stream audio levels), for reference only.
//!
//! Any other file in the folder (an ATEM Software Control save, say) is left alone.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use streamit_core::config::{Config, write_atomic};
use streamit_core::devices::{Devices, LabelKind};
use streamit_videohub::parse_blocks;

/// The most characters a profile name keeps.
pub const NAME_MAX: usize = 40;
const META: &str = "profile.toml";
const SETTINGS: &str = "settings.toml";
const DEVICES: &str = "devices";
const CURRENT: &str = "current.toml";

pub const VIDEOHUB: &str = "VideoHub";
pub const WEB_PRESENTER: &str = "Web Presenter";
pub const STREAM_ATEM: &str = "Stream ATEM";
pub const SCREENS_ATEM: &str = "Screens ATEM";
/// The devices a profile backs up, in the order they are shown.
pub const DEVICE_NAMES: [&str; 4] = [VIDEOHUB, WEB_PRESENTER, STREAM_ATEM, SCREENS_ATEM];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Meta {
    name: String,
    saved: String,
    /// Device name -> backed up (false: it was not connected when the profile was saved).
    #[serde(default)]
    devices: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct Current {
    name: String,
}

/// A profile as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInfo {
    pub name: String,
    pub dir: PathBuf,
    pub saved: String,
    /// Each device in `DEVICE_NAMES` order, and whether it was backed up.
    pub devices: Vec<(&'static str, bool)>,
}

impl ProfileInfo {
    pub fn has(&self, device: &str) -> bool {
        self.devices.iter().any(|(d, ok)| *d == device && *ok)
    }
}

/// The VideoHub's names and routing; index 0 is input/output 1.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VideoHubBackup {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// The input on each output; 0 where not known.
    pub routes: Vec<u16>,
}

impl VideoHubBackup {
    /// The backup file: the VideoHub's own protocol blocks (numbered from 0, as the
    /// VideoHub sends them), so it reads like a dump from the device.
    pub fn to_text(&self) -> String {
        let mut s = String::from("StreamIt VideoHub backup (Blackmagic protocol blocks, numbered from 0)\n\n");
        let labels = |s: &mut String, block: &str, list: &[String]| {
            s.push_str(&format!("{block}:\n"));
            for (i, l) in list.iter().enumerate() {
                s.push_str(&format!("{i} {}\n", l.replace(['\r', '\n'], " ").trim()));
            }
            s.push('\n');
        };
        labels(&mut s, "INPUT LABELS", &self.inputs);
        labels(&mut s, "OUTPUT LABELS", &self.outputs);
        s.push_str("VIDEO OUTPUT ROUTING:\n");
        for (o, &i) in self.routes.iter().enumerate() {
            if i != 0 {
                s.push_str(&format!("{o} {}\n", i - 1));
            }
        }
        s.push('\n');
        s
    }

    pub fn parse(text: &str) -> VideoHubBackup {
        let mut b = VideoHubBackup::default();
        for block in parse_blocks(text) {
            let pairs = block.lines.iter().filter_map(|l| {
                let (n, rest) = l.split_once(' ').unwrap_or((l.as_str(), ""));
                n.trim().parse::<usize>().ok().map(|n| (n, rest.trim().to_string()))
            });
            let put = |list: &mut Vec<String>, n: usize, v: String| {
                if list.len() <= n {
                    list.resize(n + 1, String::new());
                }
                list[n] = v;
            };
            match block.name.as_str() {
                "INPUT LABELS" => pairs.for_each(|(n, v)| put(&mut b.inputs, n, v)),
                "OUTPUT LABELS" => pairs.for_each(|(n, v)| put(&mut b.outputs, n, v)),
                "VIDEO OUTPUT ROUTING" => {
                    for (o, i) in pairs {
                        if let Ok(i) = i.parse::<u16>() {
                            if b.routes.len() <= o {
                                b.routes.resize(o + 1, 0);
                            }
                            b.routes[o] = i + 1;
                        }
                    }
                }
                _ => {}
            }
        }
        b
    }
}

/// What a profile holds of each device; `None` where the device was not backed up.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceBackups {
    pub videohub: Option<VideoHubBackup>,
    /// The Web Presenter's streaming settings, never the key.
    pub web_presenter: Option<Vec<(String, String)>>,
    /// What StreamIt could read of each ATEM, as text, for reference.
    pub stream_atem: Option<String>,
    pub screens_atem: Option<String>,
}

impl DeviceBackups {
    fn status(&self) -> BTreeMap<String, bool> {
        [
            (VIDEOHUB, self.videohub.is_some()),
            (WEB_PRESENTER, self.web_presenter.is_some()),
            (STREAM_ATEM, self.stream_atem.is_some()),
            (SCREENS_ATEM, self.screens_atem.is_some()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }
}

fn web_text(settings: &[(String, String)]) -> String {
    let mut s = String::from("StreamIt Web Presenter backup (the stream key is never kept)\n\nSTREAM SETTINGS:\n");
    for (k, v) in settings.iter().filter(|(k, _)| !streamit_webpresenter::is_private_or_list(k)) {
        s.push_str(&format!("{k}: {v}\n"));
    }
    s.push('\n');
    s
}

fn parse_web(text: &str) -> Vec<(String, String)> {
    parse_blocks(text)
        .iter()
        .filter(|b| b.name == "STREAM SETTINGS")
        .flat_map(streamit_webpresenter::parse_stream_settings)
        .collect()
}

/// A profile name, trimmed, or why it cannot be one. It becomes a folder name.
pub fn valid_name(typed: &str) -> Result<String, String> {
    let name = typed.trim();
    if name.is_empty() {
        return Err("Give the profile a name.".into());
    }
    if name.chars().count() > NAME_MAX {
        return Err(format!("Keep the name to {NAME_MAX} characters."));
    }
    if name.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()) {
        return Err("A name can't use \\ / : * ? \" < > |".into());
    }
    if name.ends_with('.') || name.starts_with('.') {
        return Err("A name can't start or end with a full stop.".into());
    }
    let upper = name.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err("Windows keeps that name for itself; pick another.".into());
    }
    Ok(name.to_string())
}

/// The profiles folder and what is in it.
#[derive(Debug, Clone)]
pub struct Profiles {
    root: PathBuf,
}

impl Profiles {
    /// The profiles beside `config_path` (`…\StreamIt\profiles`).
    pub fn beside(config_path: &Path) -> Profiles {
        let dir = config_path.parent().map(Path::to_path_buf).unwrap_or_default();
        Profiles { root: dir.join("profiles") }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Every profile, by name. A folder without a readable `profile.toml` is left out.
    pub fn list(&self) -> Vec<ProfileInfo> {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return Vec::new() };
        let mut out: Vec<ProfileInfo> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let text = std::fs::read_to_string(e.path().join(META)).ok()?;
                let meta: Meta = toml::from_str(&text).ok()?;
                Some(info(&meta, e.path()))
            })
            .collect();
        out.sort_by_key(|p| p.name.to_lowercase());
        out
    }

    pub fn exists(&self, name: &str) -> bool {
        self.list().iter().any(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Saves `config` and `backups` as profile `name` (replacing what it held; other files in
    /// its folder stay). A device not backed up this time loses its old backup.
    pub fn save(
        &self,
        name: &str,
        config: &Config,
        backups: &DeviceBackups,
        saved: &str,
    ) -> Result<ProfileInfo, String> {
        let name = valid_name(name)?;
        let dir = self.dir(&name);
        let devices = dir.join(DEVICES);
        std::fs::create_dir_all(&devices).map_err(|e| format!("could not make {}: {e}", devices.display()))?;
        let write = |path: PathBuf, text: &str| write_atomic(&path, text).map_err(|e| format!("{e:#}"));
        config.save(&dir.join(SETTINGS)).map_err(|e| format!("{e:#}"))?;
        let files: [(&str, Option<String>); 4] = [
            ("videohub.txt", backups.videohub.as_ref().map(VideoHubBackup::to_text)),
            ("web-presenter.txt", backups.web_presenter.as_deref().map(web_text)),
            ("stream-atem.txt", backups.stream_atem.clone()),
            ("screens-atem.txt", backups.screens_atem.clone()),
        ];
        for (file, text) in files {
            let path = devices.join(file);
            match text {
                Some(t) => write(path, &t)?,
                None => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let meta = Meta { name: name.clone(), saved: saved.to_string(), devices: backups.status() };
        let text = toml::to_string_pretty(&meta).map_err(|e| e.to_string())?;
        write(dir.join(META), &text)?;
        Ok(info(&meta, dir))
    }

    /// The settings and device backups in profile `name`.
    pub fn load(&self, name: &str) -> Result<(Config, DeviceBackups), String> {
        let dir = self.dir(name);
        let config = Config::load(&dir.join(SETTINGS)).map_err(|e| format!("{e:#}"))?;
        let devices = dir.join(DEVICES);
        let read = |file: &str| std::fs::read_to_string(devices.join(file)).ok();
        let backups = DeviceBackups {
            videohub: read("videohub.txt").map(|t| VideoHubBackup::parse(&t)),
            web_presenter: read("web-presenter.txt").map(|t| parse_web(&t)),
            stream_atem: read("stream-atem.txt"),
            screens_atem: read("screens-atem.txt"),
        };
        Ok((config, backups))
    }

    /// Moves the profile's folder to the Recycle Bin.
    pub fn delete(&self, name: &str) -> Result<(), String> {
        let dir = self.dir(name);
        trash::delete(&dir).map_err(|e| format!("could not delete {}: {e}", dir.display()))?;
        if self.current().as_deref() == Some(name) {
            let _ = std::fs::remove_file(self.root.join(CURRENT));
        }
        Ok(())
    }

    /// The profile in use, if one was saved or loaded.
    pub fn current(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.root.join(CURRENT)).ok()?;
        let c: Current = toml::from_str(&text).ok()?;
        self.dir(&c.name).join(META).is_file().then_some(c.name)
    }

    pub fn set_current(&self, name: &str) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let text = toml::to_string_pretty(&Current { name: name.to_string() }).map_err(|e| e.to_string())?;
        write_atomic(&self.root.join(CURRENT), &text).map_err(|e| format!("{e:#}"))
    }
}

fn info(meta: &Meta, dir: PathBuf) -> ProfileInfo {
    ProfileInfo {
        name: meta.name.clone(),
        dir,
        saved: meta.saved.clone(),
        devices: DEVICE_NAMES.iter().map(|d| (*d, meta.devices.get(*d).copied().unwrap_or(false))).collect(),
    }
}

// ---------------------------------------------------------------- devices

/// Reads what StreamIt can of each device now. A device that does not answer is left out
/// (`None`); the rest are still read.
pub async fn read_devices(d: &Devices, c: &Config) -> DeviceBackups {
    let hub = async {
        let routes = d.hub.routing_table().await.ok()?;
        let inputs = d.hub.input_labels().await.unwrap_or_default();
        let outputs = d.hub.output_labels().await.unwrap_or_default();
        Some(VideoHubBackup { inputs, outputs, routes })
    };
    let web = async { d.encoder.stream_settings().await.ok().filter(|s| !s.is_empty()) };
    let stream = async {
        let program = d.stream.program().await.ok()?;
        let preview = d.stream.preview().await.ok();
        let mut s = format!(
            "StreamIt Stream ATEM notes (for reference: StreamIt cannot put these back)\n\nProgram: {program}\n"
        );
        if let Some(p) = preview {
            s.push_str(&format!("Preview: {p}\n"));
        }
        for src in &c.stream_atem.mic_sources {
            if let Ok(db) = d.stream.audio_level(*src).await {
                s.push_str(&format!("Stream audio {}/{}: {db:.1} dB\n", src.input, src.source));
            }
        }
        Some(s)
    };
    let screens = async {
        let program = d.screens.program().await.ok()?;
        let preview = d.screens.preview().await.ok();
        let mut s = format!(
            "StreamIt Screens ATEM notes (for reference: StreamIt cannot put these back)\n\nProgram: {program}\n"
        );
        if let Some(p) = preview {
            s.push_str(&format!("Preview: {p}\n"));
        }
        Some(s)
    };
    let (videohub, web_presenter, stream_atem, screens_atem) = tokio::join!(hub, web, stream, screens);
    DeviceBackups { videohub, web_presenter, stream_atem, screens_atem }
}

/// Input names, output names and routes (output, input) to send, all 1-based.
pub type VideoHubChanges = (Vec<(u16, String)>, Vec<(u16, String)>, Vec<(u16, u16)>);

/// What restoring a VideoHub backup sends: the names that differ from `now`, and the routes
/// that differ (outputs the backup knows).
pub fn videohub_changes(backup: &VideoHubBackup, now: &VideoHubBackup) -> VideoHubChanges {
    let differ = |want: &[String], have: &[String]| -> Vec<(u16, String)> {
        want.iter()
            .enumerate()
            .filter(|(i, w)| have.get(*i).map(|h| h.trim()) != Some(w.trim()))
            .map(|(i, w)| (i as u16 + 1, w.trim().to_string()))
            .collect()
    };
    let routes = backup
        .routes
        .iter()
        .enumerate()
        .filter(|(o, i)| **i != 0 && now.routes.get(*o) != Some(*i))
        .map(|(o, i)| (o as u16 + 1, *i))
        .collect();
    (differ(&backup.inputs, &now.inputs), differ(&backup.outputs, &now.outputs), routes)
}

/// Puts the ticked device backups back. Returns what was done, in plain words.
pub async fn restore(d: &Devices, b: &DeviceBackups, videohub: bool, web_presenter: bool) -> Result<String, String> {
    let mut done = Vec::new();
    if videohub && let Some(want) = &b.videohub {
        let now = VideoHubBackup {
            inputs: d.hub.input_labels().await.unwrap_or_default(),
            outputs: d.hub.output_labels().await.unwrap_or_default(),
            routes: d.hub.routing_table().await.map_err(|e| format!("VideoHub: {e}"))?,
        };
        let (inputs, outputs, routes) = videohub_changes(want, &now);
        d.hub.set_labels(LabelKind::Input, &inputs).await.map_err(|e| format!("VideoHub input names: {e}"))?;
        d.hub.set_labels(LabelKind::Output, &outputs).await.map_err(|e| format!("VideoHub output names: {e}"))?;
        d.hub.route_many(&routes).await.map_err(|e| format!("VideoHub routing: {e}"))?;
        done.push(format!("VideoHub: {} names, {} routes changed", inputs.len() + outputs.len(), routes.len()));
    }
    if web_presenter && let Some(want) = &b.web_presenter {
        d.encoder.set_stream_settings(want).await.map_err(|e| format!("Web Presenter: {e}"))?;
        done.push("Web Presenter: streaming settings put back".to_string());
    }
    Ok(done.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamit_core::fakes::fake_devices;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("streamit-profiles-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn hub_backup() -> VideoHubBackup {
        VideoHubBackup {
            inputs: vec!["Cam 1".into(), "".into(), "Lectern".into()],
            outputs: vec!["".into(), "Stream ATEM 2".into()],
            routes: vec![1, 3, 0, 2],
        }
    }

    #[test]
    fn names_must_make_a_folder() {
        assert_eq!(valid_name("  Normal Sunday "), Ok("Normal Sunday".into()));
        assert!(valid_name("").is_err());
        assert!(valid_name("a/b").is_err());
        assert!(valid_name("Why?").is_err());
        assert!(valid_name("con").is_err());
        assert!(valid_name("COM1").is_err());
        assert!(valid_name("Communion").is_ok());
        assert!(valid_name(".hidden").is_err());
        assert!(valid_name(&"x".repeat(41)).is_err());
    }

    #[test]
    fn a_videohub_backup_reads_back_as_it_was_written() {
        let b = hub_backup();
        let text = b.to_text();
        assert!(text.contains("INPUT LABELS:\n0 Cam 1\n1 \n2 Lectern\n"), "{text}");
        assert!(text.contains("VIDEO OUTPUT ROUTING:\n0 0\n1 2\n3 1\n"), "{text}");
        let back = VideoHubBackup::parse(&text);
        assert_eq!(back.inputs, b.inputs);
        assert_eq!(back.outputs, b.outputs);
        assert_eq!(back.routes, b.routes);
    }

    #[test]
    fn restoring_sends_only_what_differs() {
        let want = hub_backup();
        let now = VideoHubBackup {
            inputs: vec!["Cam 1".into(), "Spare".into(), "".into()],
            outputs: vec!["".into(), "Stream ATEM 2".into()],
            routes: vec![1, 9, 4, 2],
        };
        let (inputs, outputs, routes) = videohub_changes(&want, &now);
        assert_eq!(inputs, vec![(2, String::new()), (3, "Lectern".into())]);
        assert!(outputs.is_empty());
        assert_eq!(routes, vec![(2, 3)], "output 3 is not known in the backup, output 4 already matches");
    }

    #[test]
    fn save_list_load_and_current() {
        let root = temp("save");
        let profiles = Profiles::beside(&root.join("config.toml"));
        assert!(profiles.list().is_empty());
        let mut c = Config::default();
        c.videohub.input8_output = 11;
        let backups = DeviceBackups {
            videohub: Some(hub_backup()),
            web_presenter: Some(vec![("Service".into(), "YouTube".into()), ("Key".into(), "secret".into())]),
            stream_atem: Some("Program: 2\n".into()),
            screens_atem: None,
        };
        let p = profiles.save("Normal Sunday", &c, &backups, "24 Oct 2026 20:40").unwrap();
        assert!(p.has(VIDEOHUB) && p.has(WEB_PRESENTER) && p.has(STREAM_ATEM) && !p.has(SCREENS_ATEM));
        // A file the user put in the folder stays through a second save.
        std::fs::write(p.dir.join("atem-full.xml"), "x").unwrap();
        profiles.save("Normal Sunday", &c, &backups, "later").unwrap();
        assert!(p.dir.join("atem-full.xml").is_file());
        assert_eq!(profiles.list().len(), 1);
        assert_eq!(profiles.list()[0].saved, "later");
        assert!(profiles.exists("normal sunday"));

        let (loaded, b) = profiles.load("Normal Sunday").unwrap();
        assert_eq!(loaded, c);
        assert_eq!(b.videohub, Some(hub_backup()));
        assert_eq!(b.web_presenter, Some(vec![("Service".into(), "YouTube".into())]), "the key is never kept");
        let web = std::fs::read_to_string(p.dir.join("devices").join("web-presenter.txt")).unwrap();
        assert!(!web.contains("secret"), "{web}");
        assert_eq!(b.screens_atem, None);

        assert_eq!(profiles.current(), None);
        profiles.set_current("Normal Sunday").unwrap();
        assert_eq!(profiles.current().as_deref(), Some("Normal Sunday"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn reading_devices_skips_the_ones_that_do_not_answer() {
        let (d, _log, h) = fake_devices();
        h.hub.set_route(11, 13);
        h.stream.set_program_state(2);
        h.screens.fail_next("program");
        let b = read_devices(&d, &Config::default()).await;
        assert_eq!(b.videohub.map(|v| v.routes), Some(vec![]), "the fake router reports no table");
        assert!(b.stream_atem.unwrap().contains("Program: 2"));
        assert_eq!(b.screens_atem, None, "the Screens ATEM did not answer");
        assert_eq!(b.web_presenter, None, "the fake encoder has no streaming settings");
    }
}
