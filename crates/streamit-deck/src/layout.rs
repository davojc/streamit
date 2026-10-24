//! Key map and key appearance for the 8x4 Stream Deck XL. Pure functions of the
//! engine `Snapshot`; no hardware here. Key index = row * 8 + col.
use std::time::Duration;
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, FlashKey, Snapshot};
use streamit_core::golive::{Readiness, Stage};
use streamit_core::multiview::PRESET_SLOTS;
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

/// The pages. StreamIt starts on Home; HOME (top left) on every other page goes back.
/// The deck never changes page by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Home,
    Stream,
    Videos,
    MultiView,
}

/// What a key press does: an engine command, or a move to another page.
#[derive(Debug, Clone, PartialEq)]
pub enum Press {
    Command(Command),
    Go(Page),
}

/// Every device's health, across the top of Home.
const HOME_STATUS: [(DeviceKind, &str); 7] = [
    (DeviceKind::StreamAtem, "STREAM\nATEM"),
    (DeviceKind::ScreensAtem, "SCREENS\nATEM"),
    (DeviceKind::VideoHub, "VIDEO\nHUB"),
    (DeviceKind::WebPresenter, "WEB\nPRESENTER"),
    (DeviceKind::YouTube, "YOUTUBE"),
    (DeviceKind::DeckLink, "DECKLINK"),
    (DeviceKind::Ndi, "NDI"),
];
/// The other pages show the devices they need, from key 1 (key 0 is HOME).
const STREAM_STATUS: [(DeviceKind, &str); 5] = [
    (DeviceKind::StreamAtem, "STREAM\nATEM"),
    (DeviceKind::ScreensAtem, "SCREENS\nATEM"),
    (DeviceKind::VideoHub, "VIDEO\nHUB"),
    (DeviceKind::WebPresenter, "WEB\nPRESENTER"),
    (DeviceKind::YouTube, "YOUTUBE"),
];
const VIDEOS_STATUS: [(DeviceKind, &str); 4] = [
    (DeviceKind::StreamAtem, "STREAM\nATEM"),
    (DeviceKind::VideoHub, "VIDEO\nHUB"),
    (DeviceKind::DeckLink, "DECKLINK"),
    (DeviceKind::Ndi, "NDI"),
];
const MULTIVIEW_STATUS: [(DeviceKind, &str); 1] = [(DeviceKind::VideoHub, "VIDEO\nHUB")];

/// The key that shows the service stage (top right), on every page.
pub const PHASE_KEY: u8 = 7;
/// Back to Home, on every page but Home.
pub const HOME_KEY: u8 = 0;

// Home: the three pages.
const GO_STREAM: u8 = 17;
const GO_MULTIVIEW: u8 = 19;
const GO_VIDEOS: u8 = 21;

// Stream: the service keys (row 1).
const PREPARE: u8 = 8;
const COUNTDOWN: u8 = 9;
const STOP_COUNTDOWN: u8 = 10;
const GO_PUBLIC: u8 = 11;
const GO_LIVE: u8 = 12;
const END_SERVICE: u8 = 13;
const END_BROADCAST: u8 = 14;
const RECOVER: u8 = 15;

// Videos: the cues (rows 1 and 2, columns 0-6), STOP VIDEO and UNMUTE MICS.
const STOP_VIDEO: u8 = 15;
const UNMUTE: u8 = 23;

// MultiView: the presets (row 1), and the four views laid out like the MultiView.
const FIRST_PRESET: u8 = 8;
const VIEW_KEYS: [u8; 4] = [19, 20, 27, 28];
/// Each view's colour, as in the window.
pub const VIEW_COLOURS: [Rgb; 4] = [Rgb(76, 154, 255), Rgb(245, 165, 36), Rgb(61, 214, 140), Rgb(232, 121, 201)];
const NAV: Rgb = Rgb(59, 42, 107);
const HOME_BG: Rgb = Rgb(43, 43, 43);

/// Cue index for a key on the Videos page (rows 1 and 2, columns 0-6).
fn cue_slot(key: u8) -> Option<usize> {
    let (row, col) = (key / COLS, key % COLS);
    match (row, col) {
        (1, 0..=6) => Some(col as usize),
        (2, 0..=6) => Some(7 + col as usize),
        _ => None,
    }
}

/// The 1-based number of the Videos page key that plays cue `index` (0-based).
pub fn cue_key_number(index: usize) -> u8 {
    let i = index as u8;
    if i < 7 { 9 + i } else { 17 + (i - 7) }
}

fn preset_slot(key: u8) -> Option<u8> {
    (FIRST_PRESET..FIRST_PRESET + PRESET_SLOTS).contains(&key).then(|| key - FIRST_PRESET + 1)
}

fn offered(snap: &Snapshot, o: Offer) -> bool {
    snap.offers.contains(&o)
}

/// R22: GO LIVE can be pressed before and during the countdown (the engine pre-empts a
/// running countdown), and whenever it is offered (e.g. live on the holding still after a
/// restart). Once live, ending or ended it is inert; the engine also ignores it there.
fn go_live_pressable(snap: &Snapshot) -> bool {
    matches!(snap.stage, Stage::Idle | Stage::Prepared(_) | Stage::Countdown { .. }) || offered(snap, Offer::GoLive)
}

/// UNMUTE lifts a roll-in's mic duck, so it only works while a video plays. The mics
/// are also down on the holding still or after a failed T-0; UNMUTE is inert then.
fn unmute_pressable(snap: &Snapshot) -> bool {
    snap.playing.is_some() && snap.mics_ducked
}

/// What a press of `key` on `page` does, if anything.
pub fn press_for(page: Page, key: u8, snap: &Snapshot) -> Option<Press> {
    if page != Page::Home && key == HOME_KEY {
        return Some(Press::Go(Page::Home));
    }
    let cmd = match page {
        Page::Home => {
            return match key {
                GO_STREAM => Some(Press::Go(Page::Stream)),
                GO_MULTIVIEW => Some(Press::Go(Page::MultiView)),
                GO_VIDEOS => Some(Press::Go(Page::Videos)),
                _ => None,
            };
        }
        Page::Stream => match key {
            PREPARE => Some(Command::Prepare),
            COUNTDOWN => Some(Command::Countdown),
            STOP_COUNTDOWN => Some(Command::StopCountdown),
            // Always available (spec): a redundant transition is harmless.
            GO_PUBLIC => Some(Command::GoPublic),
            GO_LIVE => go_live_pressable(snap).then_some(Command::GoLive),
            END_SERVICE => Some(Command::EndService),
            END_BROADCAST => Some(Command::EndBroadcast),
            RECOVER => offered(snap, Offer::Recover).then_some(Command::Recover),
            _ => None,
        },
        Page::Videos => match (cue_slot(key), key) {
            (Some(i), _) => (i < snap.cues.len()).then_some(Command::PlayCue(i)),
            (None, STOP_VIDEO) => snap.playing.is_some().then_some(Command::StopVideo),
            (None, UNMUTE) => unmute_pressable(snap).then_some(Command::UnmuteMics),
            _ => None,
        },
        Page::MultiView => {
            preset_slot(key).filter(|&slot| snap.multiview.preset(slot).is_some()).map(Command::RecallPreset)
        }
    };
    cmd.map(Press::Command)
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

/// How `key` on `page` should look. `blink_phase` is the 2 Hz toggle: true is the "on" half.
pub fn appearance(page: Page, key: u8, snap: &Snapshot, blink_phase: bool) -> KeyLook {
    if key == PHASE_KEY {
        let mut l = phase_look(&snap.stage);
        if snap.simulation {
            // Simulation is always visible: SIM under the stage, colours unchanged.
            l.text.push_str("\nSIM");
        }
        return l;
    }
    if page != Page::Home && key == HOME_KEY {
        return look("HOME", HOME_BG, WHITE);
    }
    let status: &[(DeviceKind, &str)] = match page {
        Page::Home => &HOME_STATUS,
        Page::Stream => &STREAM_STATUS,
        Page::Videos => &VIDEOS_STATUS,
        Page::MultiView => &MULTIVIEW_STATUS,
    };
    let first_status = if page == Page::Home { 0 } else { 1 };
    if let Some((kind, label)) = key.checked_sub(first_status).and_then(|i| status.get(i as usize)) {
        return status_look(snap, *kind, label);
    }
    match page {
        Page::Home => home_look(key, snap),
        Page::Stream => stream_look(key, snap, blink_phase),
        Page::Videos => videos_look(key, snap, blink_phase),
        Page::MultiView => multiview_look(key, snap),
    }
}

fn blank() -> KeyLook {
    look("", BLACK, BLACK)
}

fn status_look(snap: &Snapshot, kind: DeviceKind, label: &str) -> KeyLook {
    match snap.device_status.get(&kind) {
        Some(DeviceHealth::Ok) => look(label, GREEN, WHITE),
        Some(DeviceHealth::Warning(_)) => look(label, AMBER, BLACK),
        Some(DeviceHealth::Down(_)) => look(label, RED, WHITE),
        None => dim(label),
    }
}

fn home_look(key: u8, snap: &Snapshot) -> KeyLook {
    match key {
        GO_STREAM => look("STREAM", NAV, WHITE),
        GO_MULTIVIEW => look("MULTI\nVIEW", NAV, WHITE),
        // A playing video shows here too, so it is not forgotten on another page.
        GO_VIDEOS if snap.playing.is_some() => look("VIDEOS\nPLAYING", RED, WHITE),
        GO_VIDEOS => look("VIDEOS", NAV, WHITE),
        _ => blank(),
    }
}

fn stream_look(key: u8, snap: &Snapshot, blink_phase: bool) -> KeyLook {
    match key {
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
            let lit = match key {
                GO_PUBLIC => true,
                GO_LIVE => go_live_pressable(snap),
                _ => offered(snap, o),
            };
            let mut l = if lit { look(label, bg, fg) } else { dim(label) };
            if fk.is_some() && snap.flash_key == fk {
                l.blink = true;
                if !blink_phase {
                    l.bg = DIM_BG;
                    l.fg = WHITE;
                }
            }
            l
        }
        _ => blank(),
    }
}

fn videos_look(key: u8, snap: &Snapshot, blink_phase: bool) -> KeyLook {
    if let Some(i) = cue_slot(key) {
        let Some((cue, state)) = snap.cues.get(i) else {
            return blank();
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
        STOP_VIDEO if snap.playing.is_some() => look("STOP\nVIDEO", RED, WHITE),
        STOP_VIDEO => dim("STOP\nVIDEO"),
        UNMUTE if unmute_pressable(snap) => look("UNMUTE\nMICS", AMBER, BLACK),
        UNMUTE => dim("UNMUTE\nMICS"),
        _ => blank(),
    }
}

fn multiview_look(key: u8, snap: &Snapshot) -> KeyLook {
    let mv = &snap.multiview;
    if let Some(slot) = preset_slot(key) {
        return match mv.preset(slot) {
            Some(p) => {
                let bg = if mv.is_current(p) { GREEN } else { SLATE };
                look(wrap_cue_name(&p.name.to_uppercase(), CUE_LINE_WIDTH), bg, WHITE)
            }
            None => look(format!("{slot}\nEMPTY"), DIM_BG, DIM_FG),
        };
    }
    if let Some(v) = VIEW_KEYS.iter().position(|&k| k == key) {
        let shows = mv.views[v].map_or_else(|| "?".to_string(), |i| mv.label(i).to_uppercase());
        let name = streamit_core::multiview::VIEW_KEYS[v];
        // In the view's own colour, so the key matches that view in the window.
        return look(format!("{name}\n{}", wrap_cue_name(&shows, CUE_LINE_WIDTH)), VIEW_COLOURS[v], BLACK);
    }
    blank()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use streamit_core::multiview::{MultiView, Preset};
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
        Snapshot { cues: (0..n).map(|i| (cue(&format!("Video {i}")), CueState::Cued)).collect(), ..Snapshot::default() }
    }

    fn cmd(page: Page, key: u8, s: &Snapshot) -> Option<Command> {
        match press_for(page, key, s) {
            Some(Press::Command(c)) => Some(c),
            _ => None,
        }
    }

    // ---- pages

    #[test]
    fn home_opens_the_three_pages_and_home_comes_back() {
        let s = Snapshot::default();
        assert_eq!(press_for(Page::Home, 17, &s), Some(Press::Go(Page::Stream)));
        assert_eq!(press_for(Page::Home, 19, &s), Some(Press::Go(Page::MultiView)));
        assert_eq!(press_for(Page::Home, 21, &s), Some(Press::Go(Page::Videos)));
        for page in [Page::Stream, Page::Videos, Page::MultiView] {
            assert_eq!(press_for(page, HOME_KEY, &s), Some(Press::Go(Page::Home)), "{page:?}");
            assert_eq!(appearance(page, HOME_KEY, &s, true).text, "HOME");
        }
        assert_eq!(press_for(Page::Home, HOME_KEY, &s), None, "key 0 on Home is a status key");
        assert_eq!(Page::default(), Page::Home);
    }

    #[test]
    fn home_does_nothing_else() {
        let s = Snapshot { offers: vec![Offer::GoLive, Offer::Recover], ..snap_with_cues(14) };
        for k in (0..KEYS).filter(|k| ![17, 19, 21].contains(k)) {
            assert_eq!(press_for(Page::Home, k, &s), None, "key {k}");
        }
    }

    #[test]
    fn the_videos_key_on_home_shows_a_playing_video() {
        let mut s = snap_with_cues(1);
        assert_eq!(appearance(Page::Home, 21, &s, true).text, "VIDEOS");
        s.playing = Some(0);
        let l = appearance(Page::Home, 21, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("VIDEOS\nPLAYING", RED));
    }

    #[test]
    fn the_stage_key_is_on_every_page() {
        let s = Snapshot { stage: Stage::Live, ..Snapshot::default() };
        for page in [Page::Home, Page::Stream, Page::Videos, Page::MultiView] {
            assert_eq!(appearance(page, PHASE_KEY, &s, true).text, "LIVE", "{page:?}");
            assert_eq!(press_for(page, PHASE_KEY, &s), None);
        }
    }

    #[test]
    fn each_page_shows_the_devices_it_needs() {
        let mut s = Snapshot::default();
        for k in [DeviceKind::StreamAtem, DeviceKind::VideoHub, DeviceKind::DeckLink, DeviceKind::YouTube] {
            s.device_status.insert(k, DeviceHealth::Ok);
        }
        let text = |page, key| appearance(page, key, &s, true).text;
        assert_eq!(text(Page::Home, 0), "STREAM\nATEM");
        assert_eq!(text(Page::Home, 6), "NDI");
        assert_eq!(text(Page::Stream, 1), "STREAM\nATEM");
        assert_eq!(text(Page::Stream, 5), "YOUTUBE");
        assert_eq!(text(Page::Stream, 6), "");
        assert_eq!(text(Page::Videos, 3), "DECKLINK");
        assert_eq!(text(Page::Videos, 4), "NDI");
        assert_eq!(text(Page::MultiView, 1), "VIDEO\nHUB");
        assert_eq!(appearance(Page::MultiView, 1, &s, true).bg, GREEN);
        assert_eq!(text(Page::MultiView, 2), "");
    }

    #[test]
    fn status_keys_follow_device_health() {
        let mut s = Snapshot::default();
        s.device_status.insert(DeviceKind::StreamAtem, DeviceHealth::Ok);
        s.device_status.insert(DeviceKind::VideoHub, DeviceHealth::Warning("w".into()));
        s.device_status.insert(DeviceKind::Ndi, DeviceHealth::Down("d".into()));
        assert_eq!(appearance(Page::Home, 0, &s, true).bg, GREEN);
        assert_eq!(appearance(Page::Home, 2, &s, true).bg, AMBER);
        assert_eq!(appearance(Page::Home, 6, &s, true).bg, RED);
        assert_eq!(appearance(Page::Home, 1, &s, true).bg, DIM_BG);
    }

    // ---- Stream

    #[test]
    fn stream_row1_maps_actions() {
        let s = Snapshot::default();
        assert_eq!(cmd(Page::Stream, 8, &s), Some(Command::Prepare));
        assert_eq!(cmd(Page::Stream, 9, &s), Some(Command::Countdown));
        assert_eq!(cmd(Page::Stream, 10, &s), Some(Command::StopCountdown));
        assert_eq!(cmd(Page::Stream, 13, &s), Some(Command::EndService));
        assert_eq!(cmd(Page::Stream, 14, &s), Some(Command::EndBroadcast));
        let s = Snapshot { offers: vec![Offer::GoPublic, Offer::GoLive, Offer::Recover], ..s };
        assert_eq!(cmd(Page::Stream, 11, &s), Some(Command::GoPublic));
        assert_eq!(cmd(Page::Stream, 12, &s), Some(Command::GoLive));
        assert_eq!(cmd(Page::Stream, 15, &s), Some(Command::Recover));
        for k in 1..8 {
            assert_eq!(press_for(Page::Stream, k, &s), None, "status key {k} is read-only");
        }
        for k in 16..KEYS {
            assert_eq!(press_for(Page::Stream, k, &s), None, "key {k}: nothing below row 1");
        }
    }

    #[test]
    fn service_keys_are_only_on_the_stream_page() {
        let s = Snapshot { offers: vec![Offer::GoLive, Offer::Recover], ..Snapshot::default() };
        for page in [Page::Home, Page::Videos, Page::MultiView] {
            for k in [8, 9, 11, 12, 13, 14] {
                assert_ne!(cmd(page, k, &s), Some(Command::Prepare), "{page:?} {k}");
                assert!(!matches!(cmd(page, k, &s), Some(Command::GoLive | Command::EndService)), "{page:?} {k}");
            }
        }
    }

    #[test]
    fn go_live_pressable_in_prepared_and_countdown() {
        let stages = [
            Stage::Idle,
            Stage::Prepared(Readiness::Ready),
            Stage::Prepared(Readiness::Warning("w".into())),
            Stage::Countdown { remaining: Duration::from_secs(30), public: false },
            Stage::Countdown { remaining: Duration::from_secs(5), public: true },
        ];
        for stage in stages {
            let s = Snapshot { stage: stage.clone(), ..Snapshot::default() };
            assert_eq!(cmd(Page::Stream, 12, &s), Some(Command::GoLive), "{stage:?}");
            assert_eq!(appearance(Page::Stream, 12, &s, true).bg, RED, "{stage:?}");
        }
        for stage in [Stage::Live, Stage::Ending { remaining: Duration::from_secs(3) }, Stage::Ended] {
            let s = Snapshot { stage: stage.clone(), ..Snapshot::default() };
            assert_eq!(cmd(Page::Stream, 12, &s), None, "{stage:?}");
            assert_eq!(appearance(Page::Stream, 12, &s, true).bg, DIM_BG, "{stage:?}");
        }
        // Live on the holding still (assessed after a restart): Go Live is offered.
        let s = Snapshot { stage: Stage::Live, offers: vec![Offer::GoLive], ..Snapshot::default() };
        assert_eq!(cmd(Page::Stream, 12, &s), Some(Command::GoLive));
    }

    #[test]
    fn go_public_always_pressable() {
        let stages = [
            Stage::Idle,
            Stage::Prepared(Readiness::Ready),
            Stage::Countdown { remaining: Duration::from_secs(30), public: false },
            Stage::Live,
            Stage::Ending { remaining: Duration::from_secs(3) },
            Stage::Ended,
        ];
        for stage in stages {
            let s = Snapshot { stage: stage.clone(), ..Snapshot::default() };
            assert_eq!(cmd(Page::Stream, 11, &s), Some(Command::GoPublic), "{stage:?}");
            assert_ne!(appearance(Page::Stream, 11, &s, true).bg, DIM_BG, "{stage:?}");
        }
    }

    #[test]
    fn recover_dim_and_inert_unless_offered() {
        let s = Snapshot::default();
        assert_eq!(cmd(Page::Stream, 15, &s), None);
        assert_eq!(appearance(Page::Stream, 15, &s, true).bg, DIM_BG);
        let s = Snapshot { offers: vec![Offer::Recover], ..s };
        assert_eq!(cmd(Page::Stream, 15, &s), Some(Command::Recover));
        assert_eq!(appearance(Page::Stream, 15, &s, true).bg, AMBER);
    }

    #[test]
    fn flash_key_blinks_go_live_and_recover() {
        let mut s = Snapshot { offers: vec![Offer::GoLive], flash_key: Some(FlashKey::GoLive), ..Snapshot::default() };
        assert!(appearance(Page::Stream, 12, &s, true).blink);
        assert_eq!(appearance(Page::Stream, 12, &s, true).bg, RED);
        assert_eq!(appearance(Page::Stream, 12, &s, false).bg, DIM_BG);
        assert!(!appearance(Page::Stream, 15, &s, false).blink);
        s.flash_key = Some(FlashKey::Recover);
        assert!(appearance(Page::Stream, 15, &s, false).blink);
        assert!(!appearance(Page::Stream, 12, &s, false).blink);
    }

    #[test]
    fn phase_key_shows_countdown_remaining() {
        let s = Snapshot {
            stage: Stage::Countdown { remaining: Duration::from_secs(42), public: false },
            ..Snapshot::default()
        };
        let l = appearance(Page::Stream, 7, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("T-0:42", AMBER));
        let s = Snapshot {
            stage: Stage::Countdown { remaining: Duration::from_secs(9), public: true },
            ..Snapshot::default()
        };
        let l = appearance(Page::Stream, 7, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("T-0:09", RED));
    }

    #[test]
    fn phase_key_other_stages() {
        let mut s = Snapshot::default();
        let look_of = |s: &Snapshot| appearance(Page::Home, 7, s, true);
        assert_eq!(look_of(&s).text, "IDLE");
        s.stage = Stage::Prepared(Readiness::Ready);
        assert_eq!((look_of(&s).text.as_str(), look_of(&s).bg), ("READY", GREEN));
        s.stage = Stage::Prepared(Readiness::Waiting("x".into()));
        assert_eq!((look_of(&s).text.as_str(), look_of(&s).bg), ("WAIT", AMBER));
        s.stage = Stage::Prepared(Readiness::Warning("x".into()));
        assert_eq!(look_of(&s).text, "WAIT");
        s.stage = Stage::Live;
        assert_eq!((look_of(&s).text.as_str(), look_of(&s).bg), ("LIVE", RED));
        s.stage = Stage::Ending { remaining: Duration::from_secs(7) };
        assert_eq!(look_of(&s).text, "END 7");
        s.stage = Stage::Ended;
        assert_eq!(look_of(&s).text, "ENDED");
    }

    #[test]
    fn deck_phase_key_shows_sim() {
        let mut s = Snapshot { simulation: true, ..Snapshot::default() };
        assert_eq!(appearance(Page::Home, 7, &s, true).text, "IDLE\nSIM");
        s.stage = Stage::Countdown { remaining: Duration::from_secs(42), public: false };
        let l = appearance(Page::Videos, 7, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("T-0:42\nSIM", AMBER), "the stage colours stay");
        s.simulation = false;
        assert_eq!(appearance(Page::Videos, 7, &s, true).text, "T-0:42", "real mode shows no SIM");
    }

    // ---- Videos

    #[test]
    fn cue_keys_map_in_added_order() {
        let s = snap_with_cues(14);
        assert_eq!(cmd(Page::Videos, 8, &s), Some(Command::PlayCue(0)));
        assert_eq!(cmd(Page::Videos, 14, &s), Some(Command::PlayCue(6)));
        assert_eq!(cmd(Page::Videos, 16, &s), Some(Command::PlayCue(7)));
        assert_eq!(cmd(Page::Videos, 22, &s), Some(Command::PlayCue(13)));
        for i in 0..14 {
            assert_eq!(cmd(Page::Videos, cue_key_number(i) - 1, &s), Some(Command::PlayCue(i)), "cue {i}");
        }
        assert_eq!(cue_key_number(0), 9);
        assert_eq!(cue_key_number(7), 17);
    }

    #[test]
    fn empty_cue_slot_does_nothing() {
        let s = snap_with_cues(2);
        assert_eq!(press_for(Page::Videos, 10, &s), None);
        let l = appearance(Page::Videos, 10, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("", BLACK));
        assert_eq!(press_for(Page::Videos, 32, &s), None);
    }

    #[test]
    fn cue_colours_and_blinking() {
        let mut s = snap_with_cues(4);
        s.cues[0].1 = CueState::Playing;
        s.cues[1].1 = CueState::Played;
        s.cues[2].1 = CueState::Error("e".into());
        assert_eq!(appearance(Page::Videos, 8, &s, true).bg, RED);
        assert_eq!(appearance(Page::Videos, 9, &s, true).bg, GREY);
        assert_eq!(appearance(Page::Videos, 11, &s, true).bg, BLUE);
        let on = appearance(Page::Videos, 10, &s, true);
        let off = appearance(Page::Videos, 10, &s, false);
        assert!(on.blink && off.blink);
        assert_eq!(on.fg, WHITE);
        assert_eq!(off.fg, off.bg, "error text blinks off");
        s.flash = Some(3);
        assert_eq!(appearance(Page::Videos, 11, &s, true).bg, BLUE);
        let f = appearance(Page::Videos, 11, &s, false);
        assert!(f.blink);
        assert_eq!(f.bg, DIM_BG);
    }

    #[test]
    fn stop_video_lit_while_playing() {
        let mut s = snap_with_cues(1);
        assert_eq!(cmd(Page::Videos, 15, &s), None);
        assert_eq!(appearance(Page::Videos, 15, &s, true).bg, DIM_BG);
        s.playing = Some(0);
        assert_eq!(cmd(Page::Videos, 15, &s), Some(Command::StopVideo));
        assert_eq!(appearance(Page::Videos, 15, &s, true).bg, RED);
        assert_eq!(cmd(Page::Stream, 15, &s), None, "STOP VIDEO is only on the Videos page");
    }

    #[test]
    fn unmute_lit_only_when_ducked() {
        let s = Snapshot::default();
        assert_eq!(cmd(Page::Videos, 23, &s), None);
        assert_eq!(appearance(Page::Videos, 23, &s, true).bg, DIM_BG);
        let s = Snapshot { mics_ducked: true, playing: Some(0), ..s };
        assert_eq!(cmd(Page::Videos, 23, &s), Some(Command::UnmuteMics));
        assert_eq!(appearance(Page::Videos, 23, &s, true).bg, AMBER);
    }

    /// Mics are also down on the holding still and after a failed T-0 (Recover's job):
    /// UNMUTE only lifts a roll-in's duck, so it is inert outside a video.
    #[test]
    fn unmute_inert_outside_a_video_even_if_ducked() {
        let s = Snapshot { mics_ducked: true, ..snap_with_cues(1) };
        assert_eq!(cmd(Page::Videos, 23, &s), None);
        assert_eq!(appearance(Page::Videos, 23, &s, true).bg, DIM_BG);
        let s = Snapshot { mics_ducked: false, playing: Some(0), ..s };
        assert_eq!(cmd(Page::Videos, 23, &s), None);
        assert_eq!(appearance(Page::Videos, 23, &s, true).bg, DIM_BG);
    }

    // ---- MultiView

    fn mv_snap() -> Snapshot {
        Snapshot {
            multiview: MultiView {
                views: [Some(1), Some(2), Some(3), None],
                labels: vec!["Cam 1".into(), "Cam 2".into(), "Choir".into()],
                presets: vec![
                    Preset { slot: 1, name: "Service".into(), inputs: [1, 2, 3, 4] },
                    Preset { slot: 3, name: "Cameras".into(), inputs: [5, 6, 7, 8] },
                ],
            },
            ..Snapshot::default()
        }
    }

    #[test]
    fn preset_keys_recall_saved_presets_only() {
        let s = mv_snap();
        assert_eq!(cmd(Page::MultiView, 8, &s), Some(Command::RecallPreset(1)));
        assert_eq!(cmd(Page::MultiView, 10, &s), Some(Command::RecallPreset(3)));
        assert_eq!(press_for(Page::MultiView, 9, &s), None, "slot 2 is empty");
        assert_eq!(press_for(Page::MultiView, 15, &s), None, "slot 8 is empty");
        assert_eq!(appearance(Page::MultiView, 9, &s, true).text, "2\nEMPTY");
        assert_eq!(appearance(Page::MultiView, 8, &s, true).text, "SERVICE");
    }

    #[test]
    fn the_preset_on_the_views_now_is_green() {
        let mut s = mv_snap();
        assert_eq!(appearance(Page::MultiView, 8, &s, true).bg, SLATE, "view 4 is not known yet");
        s.multiview.views[3] = Some(4);
        assert_eq!(appearance(Page::MultiView, 8, &s, true).bg, GREEN);
        assert_eq!(appearance(Page::MultiView, 10, &s, true).bg, SLATE);
    }

    #[test]
    fn view_keys_show_what_each_view_has_on_it() {
        let s = mv_snap();
        let l = appearance(Page::MultiView, 19, &s, true);
        assert_eq!((l.text.as_str(), l.bg), ("TL\nCAM 1", VIEW_COLOURS[0]));
        assert_eq!(appearance(Page::MultiView, 20, &s, true).text, "TR\nCAM 2");
        assert_eq!(appearance(Page::MultiView, 27, &s, true).text, "BL\nCHOIR");
        assert_eq!(appearance(Page::MultiView, 28, &s, true).text, "BR\n?", "not known");
        for k in [19, 20, 27, 28] {
            assert_eq!(press_for(Page::MultiView, k, &s), None, "the view keys only show");
        }
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
