//! Key map and key appearance for the 8x4 Stream Deck XL. Pure functions of the
//! engine `Snapshot`; no hardware here. Key index = row * 8 + col.
use std::time::Duration;
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, FlashKey, Snapshot};
use streamit_core::golive::{Readiness, Stage};
use streamit_core::recovery::Offer;
use streamit_core::rollin::CueState;

pub const KEYS: u8 = 32;
pub const COLS: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub const BLACK: Rgb = Rgb(0, 0, 0);
pub const WHITE: Rgb = Rgb(255, 255, 255);
pub const GREEN: Rgb = Rgb(0, 140, 60);
pub const AMBER: Rgb = Rgb(220, 150, 0);
pub const RED: Rgb = Rgb(200, 30, 30);
pub const GREY: Rgb = Rgb(90, 90, 90);
pub const BLUE: Rgb = Rgb(30, 80, 200);
pub const SLATE: Rgb = Rgb(45, 55, 85);
pub const DIM_BG: Rgb = Rgb(28, 28, 28);
pub const DIM_FG: Rgb = Rgb(95, 95, 95);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyLook {
    pub text: String,
    pub bg: Rgb,
    pub fg: Rgb,
    /// This key is blinking right now (the colours already reflect the phase).
    pub blink: bool,
}

const STATUS_KEYS: [(DeviceKind, &str); 7] = [
    (DeviceKind::StreamAtem, "STREAM\nATEM"),
    (DeviceKind::ScreensAtem, "SCREENS\nATEM"),
    (DeviceKind::VideoHub, "VIDEO\nHUB"),
    (DeviceKind::WebPresenter, "WEB\nPRESENTER"),
    (DeviceKind::YouTube, "YOUTUBE"),
    (DeviceKind::DeckLink, "DECKLINK"),
    (DeviceKind::Ndi, "NDI"),
];
const PHASE_KEY: u8 = 7;

const PREPARE: u8 = 8;
const COUNTDOWN: u8 = 9;
const STOP_COUNTDOWN: u8 = 10;
const GO_PUBLIC: u8 = 11;
const GO_LIVE: u8 = 12;
const END_SERVICE: u8 = 13;
const END_BROADCAST: u8 = 14;
const RECOVER: u8 = 15;
const STOP_VIDEO: u8 = 23;
const UNMUTE: u8 = 31;

/// Cue index for a key, if the key is a cue position (rows 2 and 3, columns 0-6).
fn cue_slot(key: u8) -> Option<usize> {
    let (row, col) = (key / COLS, key % COLS);
    match (row, col) {
        (2, 0..=6) => Some(col as usize),
        (3, 0..=6) => Some(7 + col as usize),
        _ => None,
    }
}

fn offered(snap: &Snapshot, o: Offer) -> bool {
    snap.offers.contains(&o)
}

/// The engine command a press of `key` should send, if any.
pub fn command_for(key: u8, snap: &Snapshot) -> Option<Command> {
    if let Some(i) = cue_slot(key) {
        return (i < snap.cues.len()).then_some(Command::PlayCue(i));
    }
    match key {
        PREPARE => Some(Command::Prepare),
        COUNTDOWN => Some(Command::Countdown),
        STOP_COUNTDOWN => Some(Command::StopCountdown),
        GO_PUBLIC => offered(snap, Offer::GoPublic).then_some(Command::GoPublic),
        GO_LIVE => offered(snap, Offer::GoLive).then_some(Command::GoLive),
        END_SERVICE => Some(Command::EndService),
        END_BROADCAST => Some(Command::EndBroadcast),
        RECOVER => offered(snap, Offer::Recover).then_some(Command::Recover),
        STOP_VIDEO => snap.playing.is_some().then_some(Command::StopVideo),
        UNMUTE => snap.mics_ducked.then_some(Command::UnmuteMics),
        _ => None,
    }
}

fn look(text: impl Into<String>, bg: Rgb, fg: Rgb) -> KeyLook {
    KeyLook { text: text.into(), bg, fg, blink: false }
}

fn dim(text: &str) -> KeyLook {
    look(text, DIM_BG, DIM_FG)
}

fn whole_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

fn phase_look(stage: &Stage) -> KeyLook {
    match stage {
        Stage::Idle => look("IDLE", GREY, WHITE),
        Stage::Prepared(Readiness::Ready) => look("READY", GREEN, WHITE),
        Stage::Prepared(_) => look("WAIT", AMBER, BLACK),
        Stage::Countdown { remaining, public } => {
            let s = whole_secs(*remaining);
            let (bg, fg) = if *public { (RED, WHITE) } else { (AMBER, BLACK) };
            look(format!("T-{}:{:02}", s / 60, s % 60), bg, fg)
        }
        Stage::Live => look("LIVE", RED, WHITE),
        Stage::Ending { remaining } => look(format!("END {}", whole_secs(*remaining)), AMBER, BLACK),
        Stage::Ended => look("ENDED", GREY, WHITE),
    }
}

/// Word-wrap `name` to at most 3 lines of `width` chars; the last line ends in "…" if cut.
pub fn wrap_cue_name(name: &str, width: usize) -> String {
    const MAX_LINES: usize = 3;
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut truncated = false;
    'words: for word in name.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        loop {
            let need = if cur.is_empty() { word.len() } else { cur.chars().count() + 1 + word.len() };
            if need <= width {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.extend(word.iter());
                continue 'words;
            }
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            } else {
                // A single word longer than a line: break it.
                lines.push(word.drain(..width).collect());
            }
            if lines.len() == MAX_LINES {
                truncated = true;
                break 'words;
            }
        }
    }
    if !truncated && !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > MAX_LINES {
        lines.truncate(MAX_LINES);
        truncated = true;
    }
    if truncated {
        let last = lines.last_mut().expect("truncated implies a line");
        while last.chars().count() >= width {
            last.pop();
        }
        last.push('…');
    }
    lines.join("\n")
}

const CUE_LINE_WIDTH: usize = 9;

/// How `key` should look. `blink_phase` is the 2 Hz toggle: true is the "on" half.
pub fn appearance(key: u8, snap: &Snapshot, blink_phase: bool) -> KeyLook {
    if let Some(i) = cue_slot(key) {
        let Some((cue, state)) = snap.cues.get(i) else {
            return look("", BLACK, BLACK);
        };
        let text = wrap_cue_name(&cue.name, CUE_LINE_WIDTH);
        let mut l = match state {
            CueState::Cued => look(text, BLUE, WHITE),
            CueState::Playing => look(text, RED, WHITE),
            CueState::Played => look(text, GREY, WHITE),
            CueState::Error(_) => {
                let mut l = look(text, RED, WHITE);
                l.blink = true;
                if !blink_phase {
                    l.fg = l.bg; // the text blinks, the background stays
                }
                l
            }
        };
        if snap.flash == Some(i) {
            l.blink = true;
            if !blink_phase {
                l.bg = DIM_BG;
            }
        }
        return l;
    }
    match key {
        0..=6 => {
            let (kind, label) = STATUS_KEYS[key as usize];
            match snap.device_status.get(&kind) {
                Some(DeviceHealth::Ok) => look(label, GREEN, WHITE),
                Some(DeviceHealth::Warning(_)) => look(label, AMBER, BLACK),
                Some(DeviceHealth::Down(_)) => look(label, RED, WHITE),
                None => dim(label),
            }
        }
        PHASE_KEY => phase_look(&snap.stage),
        PREPARE => look("PREPARE", SLATE, WHITE),
        COUNTDOWN => look("COUNT\nDOWN", SLATE, WHITE),
        STOP_COUNTDOWN => look("STOP\nCOUNT", SLATE, WHITE),
        END_SERVICE => look("END\nSERVICE", Rgb(120, 30, 30), WHITE),
        END_BROADCAST => look("END\nBCAST", Rgb(120, 30, 30), WHITE),
        GO_PUBLIC | GO_LIVE | RECOVER => {
            let (o, label, bg, fg, fk) = match key {
                GO_PUBLIC => (Offer::GoPublic, "GO\nPUBLIC", Rgb(0, 110, 160), WHITE, None),
                GO_LIVE => (Offer::GoLive, "GO\nLIVE", RED, WHITE, Some(FlashKey::GoLive)),
                _ => (Offer::Recover, "RECOVER", AMBER, BLACK, Some(FlashKey::Recover)),
            };
            let mut l = if offered(snap, o) { look(label, bg, fg) } else { dim(label) };
            if fk.is_some() && snap.flash_key == fk {
                l.blink = true;
                if !blink_phase {
                    l.bg = DIM_BG;
                    l.fg = WHITE;
                }
            }
            l
        }
        STOP_VIDEO => {
            if snap.playing.is_some() { look("STOP\nVIDEO", RED, WHITE) } else { dim("STOP\nVIDEO") }
        }
        UNMUTE => {
            if snap.mics_ducked { look("UNMUTE\nMICS", AMBER, BLACK) } else { dim("UNMUTE\nMICS") }
        }
        _ => look("", BLACK, BLACK),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use streamit_core::rollin::{Cue, Destination};

    fn cue(name: &str) -> Cue {
        Cue {
            path: PathBuf::from(format!("{name}.mp4")),
            name: name.into(),
            destination: Destination::HallAndOnline,
            gain_db: 0.0,
            return_input: None,
        }
    }

    fn snap_with_cues(n: usize) -> Snapshot {
        Snapshot {
            cues: (0..n).map(|i| (cue(&format!("Video {i}")), CueState::Cued)).collect(),
            ..Snapshot::default()
        }
    }

    #[test]
    fn row1_maps_actions() {
        let s = Snapshot::default();
        assert_eq!(command_for(8, &s), Some(Command::Prepare));
        assert_eq!(command_for(9, &s), Some(Command::Countdown));
        assert_eq!(command_for(10, &s), Some(Command::StopCountdown));
        assert_eq!(command_for(13, &s), Some(Command::EndService));
        assert_eq!(command_for(14, &s), Some(Command::EndBroadcast));
        let s = Snapshot { offers: vec![Offer::GoPublic, Offer::GoLive, Offer::Recover], ..s };
        assert_eq!(command_for(11, &s), Some(Command::GoPublic));
        assert_eq!(command_for(12, &s), Some(Command::GoLive));
        assert_eq!(command_for(15, &s), Some(Command::Recover));
        for k in 0..8 {
            assert_eq!(command_for(k, &s), None, "status key {k} is read-only");
        }
    }

    #[test]
    fn cue_keys_map_in_added_order() {
        let s = snap_with_cues(14);
        assert_eq!(command_for(16, &s), Some(Command::PlayCue(0)));
        assert_eq!(command_for(22, &s), Some(Command::PlayCue(6)));
        assert_eq!(command_for(24, &s), Some(Command::PlayCue(7)));
        assert_eq!(command_for(30, &s), Some(Command::PlayCue(13)));
    }

    #[test]
    fn empty_cue_slot_does_nothing() {
        let s = snap_with_cues(2);
        assert_eq!(command_for(18, &s), None);
        let l = appearance(18, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("", BLACK));
        assert_eq!(command_for(32, &s), None);
    }

    #[test]
    fn recover_dim_and_inert_unless_offered() {
        let s = Snapshot::default();
        assert_eq!(command_for(15, &s), None);
        assert_eq!(appearance(15, &s, true).bg, DIM_BG);
        let s = Snapshot { offers: vec![Offer::Recover], ..s };
        assert_eq!(command_for(15, &s), Some(Command::Recover));
        assert_eq!(appearance(15, &s, true).bg, AMBER);
    }

    #[test]
    fn unmute_lit_only_when_ducked() {
        let s = Snapshot::default();
        assert_eq!(command_for(31, &s), None);
        assert_eq!(appearance(31, &s, true).bg, DIM_BG);
        let s = Snapshot { mics_ducked: true, ..s };
        assert_eq!(command_for(31, &s), Some(Command::UnmuteMics));
        assert_eq!(appearance(31, &s, true).bg, AMBER);
    }

    #[test]
    fn phase_key_shows_countdown_remaining() {
        let s = Snapshot {
            stage: Stage::Countdown { remaining: Duration::from_secs(42), public: false },
            ..Snapshot::default()
        };
        let l = appearance(7, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("T-0:42", AMBER));
        let s = Snapshot {
            stage: Stage::Countdown { remaining: Duration::from_secs(9), public: true },
            ..Snapshot::default()
        };
        let l = appearance(7, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("T-0:09", RED));
        assert_eq!(command_for(7, &s), None);
    }

    #[test]
    fn phase_key_other_stages() {
        let mut s = Snapshot::default();
        assert_eq!(appearance(7, &s, true).text, "IDLE");
        s.stage = Stage::Prepared(Readiness::Ready);
        let l = appearance(7, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("READY", GREEN));
        s.stage = Stage::Prepared(Readiness::Waiting("x".into()));
        let l = appearance(7, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("WAIT", AMBER));
        s.stage = Stage::Prepared(Readiness::Warning("x".into()));
        assert_eq!(appearance(7, &s, true).text, "WAIT");
        s.stage = Stage::Live;
        let l = appearance(7, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("LIVE", RED));
        s.stage = Stage::Ending { remaining: Duration::from_secs(7) };
        assert_eq!(appearance(7, &s, true).text, "END 7");
        s.stage = Stage::Ended;
        assert_eq!(appearance(7, &s, true).text, "ENDED");
    }

    #[test]
    fn status_keys_follow_device_health() {
        let mut s = Snapshot::default();
        s.device_status.insert(DeviceKind::StreamAtem, DeviceHealth::Ok);
        s.device_status.insert(DeviceKind::VideoHub, DeviceHealth::Warning("w".into()));
        s.device_status.insert(DeviceKind::Ndi, DeviceHealth::Down("d".into()));
        assert_eq!(appearance(0, &s, true).bg, GREEN);
        assert_eq!(appearance(2, &s, true).bg, AMBER);
        assert_eq!(appearance(6, &s, true).bg, RED);
        assert_eq!(appearance(1, &s, true).bg, DIM_BG);
    }

    #[test]
    fn cue_colours_and_blinking() {
        let mut s = snap_with_cues(4);
        s.cues[0].1 = CueState::Playing;
        s.cues[1].1 = CueState::Played;
        s.cues[2].1 = CueState::Error("e".into());
        assert_eq!(appearance(16, &s, true).bg, RED);
        assert_eq!(appearance(17, &s, true).bg, GREY);
        assert_eq!(appearance(19, &s, true).bg, BLUE);
        let on = appearance(18, &s, true);
        let off = appearance(18, &s, false);
        assert!(on.blink && off.blink);
        assert_eq!(on.fg, WHITE);
        assert_eq!(off.fg, off.bg, "error text blinks off");
        s.flash = Some(3);
        assert_eq!(appearance(19, &s, true).bg, BLUE);
        let f = appearance(19, &s, false);
        assert!(f.blink);
        assert_eq!(f.bg, DIM_BG);
    }

    #[test]
    fn flash_key_blinks_go_live_and_recover() {
        let mut s = Snapshot { offers: vec![Offer::GoLive], flash_key: Some(FlashKey::GoLive), ..Snapshot::default() };
        assert!(appearance(12, &s, true).blink);
        assert_eq!(appearance(12, &s, true).bg, RED);
        assert_eq!(appearance(12, &s, false).bg, DIM_BG);
        assert!(!appearance(15, &s, false).blink);
        s.flash_key = Some(FlashKey::Recover);
        assert!(appearance(15, &s, false).blink);
        assert!(!appearance(12, &s, false).blink);
    }

    #[test]
    fn stop_video_lit_while_playing() {
        let mut s = snap_with_cues(1);
        assert_eq!(command_for(23, &s), None);
        assert_eq!(appearance(23, &s, true).bg, DIM_BG);
        s.playing = Some(0);
        assert_eq!(command_for(23, &s), Some(Command::StopVideo));
        assert_eq!(appearance(23, &s, true).bg, RED);
    }

    #[test]
    fn cue_names_wrap_to_three_lines_with_ellipsis() {
        assert_eq!(wrap_cue_name("Welcome", 9), "Welcome");
        assert_eq!(wrap_cue_name("Welcome to church", 9), "Welcome\nto church");
        let w = wrap_cue_name("one two three four five six seven eight nine ten", 9);
        assert_eq!(w.lines().count(), 3);
        assert!(w.ends_with('…'));
        assert!(w.lines().all(|l| l.chars().count() <= 9));
        let long = wrap_cue_name("Supercalifragilisticexpialidocious", 9);
        assert!(long.lines().count() <= 3 && long.ends_with('…'));
    }
}
