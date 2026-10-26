//! Key map and key appearance for the 8x4 Stream Deck XL. Pure functions of the
//! engine `Snapshot` and the page; no hardware here. Key index = row * 8 + col.
//!
//! The deck shows the page the window is on, and the top row is the same everywhere:
//! SERVICE, VIDEOS, SCENES and ROUTING, then (on Routing) MULTI VIEW, STREAM ATEM and
//! SCREENS ATEM, and the stage top right. The Service page's keys follow the stage.
use std::time::{Duration, SystemTime};
use streamit_core::engine::{Command, FlashKey, Snapshot};
use streamit_core::golive::{Readiness, Stage};
use streamit_core::recovery::Offer;
use streamit_core::rollin::CueState;
use streamit_core::routing::{PRESET_SLOTS, Target, TargetState, VIEW_KEYS, locked};
use streamit_core::scenes::MAX_SCENES;

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

/// The pages, one per window tab: the window and the deck always show the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    /// The service, with keys for the stage it is at.
    #[default]
    Service,
    Videos,
    /// The scenes, one key each.
    Scenes,
    /// One routing target: its presets and what each destination shows.
    Routing(Target),
}

/// A press that asks first: the key below it (or beside it) becomes a blinking CONFIRM
/// for `CONFIRM_FOR`, and only that does it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    EndService,
    /// Start service now from Idle: nothing has been prepared.
    StartNow,
    /// Back to Idle from Prepared or the countdown.
    Abandon,
}

impl Asked {
    /// Where its CONFIRM key appears.
    pub fn confirm_key(self) -> u8 {
        match self {
            Asked::EndService => CONFIRM_END,
            Asked::StartNow => CONFIRM_START,
            Asked::Abandon => CONFIRM_ABANDON,
        }
    }

    /// What the CONFIRM press sends.
    pub fn command(self) -> Command {
        match self {
            Asked::EndService => Command::EndService,
            Asked::StartNow => Command::GoLive,
            Asked::Abandon => Command::Abandon,
        }
    }
}

/// What a key press does: an engine command, a move to another page, or a preset to
/// recall (the deck asks for a second press first when it would change what is on air).
#[derive(Debug, Clone, PartialEq)]
pub enum Press {
    Command(Command),
    Go(Page),
    Recall(Target, u8),
    /// The CONFIRM key for this appears; doing it needs a press of that.
    Ask(Asked),
    /// A CONFIRM key: does what was asked, if it was asked just before.
    Confirm(Asked),
}

// The top row.
const GO_SERVICE: u8 = 0;
const GO_VIDEOS: u8 = 1;
const GO_SCENES: u8 = 2;
const GO_ROUTING: u8 = 3;
const GO_MULTIVIEW: u8 = 4;
const GO_STREAM_ATEM: u8 = 5;
const GO_SCREENS_ATEM: u8 = 6;
/// The key that shows the service stage (top right), on every page.
pub const PHASE_KEY: u8 = 7;

// Service, before the service (Idle or Ended) and prepared.
const PREPARE: u8 = 8;
const COUNTDOWN: u8 = 9;
const START_NOW: u8 = 11;
const END_BROADCAST: u8 = 14;
const RECOVER: u8 = 15;
/// Below START SERVICE NOW, when it asks first.
pub const CONFIRM_START: u8 = 19;
/// Prepared or counting down, while YouTube is still private.
const ABANDON: u8 = 13;
/// Below ABANDON.
pub const CONFIRM_ABANDON: u8 = 21;
// Service, countdown.
const CD_START_NOW: u8 = 8;
const STOP_COUNTDOWN: u8 = 9;
const GO_PUBLIC: u8 = 10;
// Service, ending.
const SKIP_HOLD: u8 = 8;
const END_BROADCAST_ENDING: u8 = 9;
// Service, live: the videos as on the Videos page, five scenes along the bottom, a key
// for whatever is offered, END SERVICE and its CONFIRM.
const LIVE_FIRST_SCENE: u8 = 24;
const LIVE_SCENES: usize = 5;
const OFFER_KEY: u8 = 29;
const END_SERVICE: u8 = 30;
/// Beside END SERVICE, when it asks first.
pub const CONFIRM_END: u8 = 31;

// Videos (and Service while live): the cues (rows 1 and 2, columns 0-6), STOP VIDEO and
// STREAM AUDIO UP.
const STOP_VIDEO: u8 = 15;
const UNMUTE: u8 = 23;

// A routing page: the presets (row 1); below, the MultiView's four views laid out like the
// MultiView, or an ATEM's eight inputs along row 2.
const FIRST_PRESET: u8 = 8;
const MV_KEYS: [u8; 4] = [19, 20, 27, 28];
const FIRST_ATEM_INPUT: u8 = 16;
/// An ATEM input that is not on program or preview, in a dark shade of its ATEM's colour
/// (violet for the Stream ATEM, cyan for the Screens ATEM, as in the window).
const STREAM_INPUT_BG: Rgb = Rgb(76, 52, 140);
const SCREENS_INPUT_BG: Rgb = Rgb(14, 102, 120);
/// Each view's colour, as in the window.
pub const VIEW_COLOURS: [Rgb; 4] = [Rgb(76, 154, 255), Rgb(245, 165, 36), Rgb(61, 214, 140), Rgb(232, 121, 201)];
const NAV: Rgb = Rgb(43, 43, 43);
const NAV_ON: Rgb = Rgb(30, 58, 138);
const SUB: Rgb = Rgb(36, 36, 52);
const SUB_ON: Rgb = Rgb(59, 42, 107);

// Scenes: rows 1 and 2, twelve keys (all of row 1, the first four of row 2).
const FIRST_SCENE: u8 = 8;
/// A scene's key, and the scene on now.
const SCENE_BG: Rgb = Rgb(96, 70, 12);

/// Where the service is, for the Service page's keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceStep {
    Before,
    Prepared,
    Countdown,
    Live,
    Ending,
}

fn service_step(stage: &Stage) -> ServiceStep {
    match stage {
        Stage::Idle | Stage::Ended => ServiceStep::Before,
        Stage::Prepared(_) => ServiceStep::Prepared,
        Stage::Countdown { .. } => ServiceStep::Countdown,
        Stage::Live => ServiceStep::Live,
        Stage::Ending { .. } => ServiceStep::Ending,
    }
}

/// Scene index for a key on the Scenes page.
fn scene_slot(key: u8) -> Option<usize> {
    let i = key.checked_sub(FIRST_SCENE)? as usize;
    (i < MAX_SCENES).then_some(i)
}

/// Scene index for a key along the bottom of the live Service page.
fn live_scene_slot(key: u8) -> Option<usize> {
    let i = key.checked_sub(LIVE_FIRST_SCENE)? as usize;
    (i < LIVE_SCENES).then_some(i)
}

/// Cue index for a video key (rows 1 and 2, columns 0-6).
fn cue_slot(key: u8) -> Option<usize> {
    let (row, col) = (key / COLS, key % COLS);
    match (row, col) {
        (1, 0..=6) => Some(col as usize),
        (2, 0..=6) => Some(7 + col as usize),
        _ => None,
    }
}

/// The 1-based number of the key that plays cue `index` (0-based).
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

/// END SERVICE needs confirming: the service is not already ending (pressed again while
/// it ends, END SERVICE skips the closing hold straight away).
pub fn end_needs_confirm(snap: &Snapshot) -> bool {
    !matches!(snap.stage, Stage::Ending { .. }) && snap.job != Some("End Service")
}

/// Recalling the preset in `slot` of `t` would change an input on program: the deck asks
/// for a second press first.
pub fn needs_second_press(snap: &Snapshot, t: Target, slot: u8) -> bool {
    let state = snap.routing.target(t);
    state.preset(slot).is_some_and(|p| state.touches_on_air(&state.changes(p)))
}

/// UNMUTE lifts a roll-in's mic duck, so it only works while a video plays. The mics
/// are also down on the holding still or after a failed T-0; UNMUTE is inert then.
fn unmute_pressable(snap: &Snapshot) -> bool {
    snap.playing.is_some() && snap.mics_ducked
}

/// Before the service a video key test-plays it (hall and input 8, never online); from
/// the countdown on it plays it.
fn tests_videos(snap: &Snapshot) -> bool {
    matches!(service_step(&snap.stage), ServiceStep::Before | ServiceStep::Prepared)
}

/// What is offered on the live page's spare key, most pressing first.
fn live_offer(snap: &Snapshot) -> Option<Offer> {
    [Offer::Recover, Offer::GoLive, Offer::GoPublic, Offer::EndBroadcast].into_iter().find(|o| offered(snap, *o))
}

fn offer_command(o: Offer) -> Command {
    match o {
        Offer::Recover => Command::Recover,
        Offer::GoLive => Command::GoLive,
        Offer::GoPublic => Command::GoPublic,
        Offer::EndBroadcast => Command::EndBroadcast,
    }
}

/// What a press of `key` on `page` does, if anything.
pub fn press_for(page: Page, key: u8, snap: &Snapshot) -> Option<Press> {
    let go = |p| Some(Press::Go(p));
    match key {
        GO_SERVICE => return go(Page::Service),
        GO_VIDEOS => return go(Page::Videos),
        GO_SCENES => return go(Page::Scenes),
        // Routing opens on the target it was last on (MultiView from elsewhere).
        GO_ROUTING => return go(Page::Routing(if let Page::Routing(t) = page { t } else { Target::MultiView })),
        GO_MULTIVIEW | GO_STREAM_ATEM | GO_SCREENS_ATEM if matches!(page, Page::Routing(_)) => {
            return go(Page::Routing(Target::ALL[(key - GO_MULTIVIEW) as usize]));
        }
        PHASE_KEY => return None,
        _ => {}
    }
    let cmd = match page {
        Page::Service => return service_press(key, snap),
        Page::Videos => video_press(key, snap),
        Page::Scenes => scene_slot(key).and_then(|i| snap.scenes.get(i)).map(|s| Command::GoScene(s.id)),
        Page::Routing(t) => {
            return preset_slot(key)
                .filter(|&slot| snap.routing.target(t).preset(slot).is_some())
                .map(|slot| Press::Recall(t, slot));
        }
    };
    cmd.map(Press::Command)
}

/// The video keys, STOP VIDEO and STREAM AUDIO UP: on the Videos page, and on Service
/// while live.
fn video_press(key: u8, snap: &Snapshot) -> Option<Command> {
    match (cue_slot(key), key) {
        (Some(i), _) => {
            let (cue, _) = snap.cues.get(i)?;
            Some(if tests_videos(snap) { Command::TestVideo(cue.clone()) } else { Command::PlayCue(i) })
        }
        // STOP VIDEO stops a test play too.
        (None, STOP_VIDEO) => (snap.playing.is_some() || snap.test.is_some()).then_some(Command::StopVideo),
        (None, UNMUTE) => unmute_pressable(snap).then_some(Command::UnmuteMics),
        _ => None,
    }
}

fn service_press(key: u8, snap: &Snapshot) -> Option<Press> {
    let cmd = |c| Some(Press::Command(c));
    match service_step(&snap.stage) {
        ServiceStep::Before => match key {
            PREPARE => cmd(Command::Prepare),
            // From Idle nothing is prepared: it asks first, as in the window. After a
            // service has ended, Start service now is not offered.
            START_NOW if snap.stage == Stage::Idle => Some(Press::Ask(Asked::StartNow)),
            CONFIRM_START => Some(Press::Confirm(Asked::StartNow)),
            END_BROADCAST if offered(snap, Offer::EndBroadcast) => cmd(Command::EndBroadcast),
            RECOVER if offered(snap, Offer::Recover) => cmd(Command::Recover),
            _ => None,
        },
        ServiceStep::Prepared => match key {
            PREPARE => cmd(Command::Prepare),
            COUNTDOWN => cmd(Command::Countdown),
            START_NOW => cmd(Command::GoLive),
            RECOVER if offered(snap, Offer::Recover) => cmd(Command::Recover),
            ABANDON if snap.can_abandon() => Some(Press::Ask(Asked::Abandon)),
            CONFIRM_ABANDON => Some(Press::Confirm(Asked::Abandon)),
            _ => None,
        },
        ServiceStep::Countdown => match key {
            CD_START_NOW => cmd(Command::GoLive),
            // Once YouTube is going public the service goes on.
            STOP_COUNTDOWN if !snap.public_before_live() => cmd(Command::StopCountdown),
            ABANDON if snap.can_abandon() => Some(Press::Ask(Asked::Abandon)),
            CONFIRM_ABANDON => Some(Press::Confirm(Asked::Abandon)),
            // A redundant transition is harmless.
            GO_PUBLIC => cmd(Command::GoPublic),
            _ => None,
        },
        ServiceStep::Live => match key {
            END_SERVICE if end_needs_confirm(snap) => Some(Press::Ask(Asked::EndService)),
            END_SERVICE => cmd(Command::EndService),
            CONFIRM_END => Some(Press::Confirm(Asked::EndService)),
            OFFER_KEY => live_offer(snap).map(|o| Press::Command(offer_command(o))),
            k if live_scene_slot(k).is_some() => {
                live_scene_slot(k).and_then(|i| snap.scenes.get(i)).map(|s| Press::Command(Command::GoScene(s.id)))
            }
            k => video_press(k, snap).map(Press::Command),
        },
        ServiceStep::Ending => match key {
            SKIP_HOLD => cmd(Command::EndService),
            END_BROADCAST_ENDING => cmd(Command::EndBroadcast),
            _ => None,
        },
    }
}

fn look(text: impl Into<String>, bg: Rgb, fg: Rgb) -> KeyLook {
    KeyLook { text: text.into(), bg, fg, blink: false }
}

fn dim(text: &str) -> KeyLook {
    look(text, DIM_BG, DIM_FG)
}

fn blank() -> KeyLook {
    look("", BLACK, BLACK)
}

/// `l`, blinking: dimmed on the off half.
fn blinking(mut l: KeyLook, blink_phase: bool) -> KeyLook {
    l.blink = true;
    if !blink_phase {
        l.bg = DIM_BG;
        l.fg = WHITE;
    }
    l
}

fn whole_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

fn phase_look(snap: &Snapshot) -> KeyLook {
    match &snap.stage {
        Stage::Idle => look("IDLE", GREY, WHITE),
        Stage::Prepared(Readiness::Ready) => look("READY", GREEN, WHITE),
        Stage::Prepared(_) => look("WAIT", AMBER, BLACK),
        Stage::Countdown { remaining, public } => {
            let s = whole_secs(*remaining);
            let (bg, fg) = if *public { (RED, WHITE) } else { (AMBER, BLACK) };
            look(format!("T-{}:{:02}", s / 60, s % 60), bg, fg)
        }
        Stage::Live => match snap.on_air_since.and_then(|t| SystemTime::now().duration_since(t).ok()) {
            Some(d) => {
                let s = d.as_secs();
                look(format!("LIVE\n{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60), RED, WHITE)
            }
            None => look("LIVE", RED, WHITE),
        },
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
    appearance_with(page, key, snap, blink_phase, None, None)
}

/// `appearance`, with `armed` the preset waiting for its second press (it blinks PRESS
/// AGAIN) and `asking` the press waiting for its CONFIRM.
pub fn appearance_with(
    page: Page,
    key: u8,
    snap: &Snapshot,
    blink_phase: bool,
    armed: Option<(Target, u8)>,
    asking: Option<Asked>,
) -> KeyLook {
    if key == PHASE_KEY {
        let mut l = phase_look(snap);
        if snap.simulation {
            // Simulation is always visible: SIM under the stage, colours unchanged.
            l.text.push_str("\nSIM");
        }
        return l;
    }
    if key < COLS {
        return nav_look(page, key, snap);
    }
    if let Some(a) = asking.filter(|a| a.confirm_key() == key && page == Page::Service) {
        let text = match a {
            Asked::EndService => "CONFIRM\nEND",
            Asked::StartNow => "CONFIRM\nSTART",
            Asked::Abandon => "CONFIRM\nABANDON",
        };
        return blinking(look(text, RED, WHITE), blink_phase);
    }
    match page {
        Page::Service => service_look(key, snap, blink_phase),
        Page::Videos => video_look(key, snap, blink_phase),
        Page::Routing(t) => target_look(t, key, snap, blink_phase, armed),
        Page::Scenes => scene_slot(key).map_or_else(blank, |i| scene_look(i, snap)),
    }
}

/// The top row: the pages (the one shown lit), and on Routing its three targets.
fn nav_look(page: Page, key: u8, snap: &Snapshot) -> KeyLook {
    let lit = |on: bool| if on { NAV_ON } else { NAV };
    match key {
        GO_SERVICE => look("SERVICE", lit(page == Page::Service), WHITE),
        // A playing video shows here too, so it is not forgotten on another page.
        GO_VIDEOS if snap.playing.is_some() && page != Page::Videos => look("VIDEOS\nPLAYING", RED, WHITE),
        GO_VIDEOS => look("VIDEOS", lit(page == Page::Videos), WHITE),
        GO_SCENES => look("SCENES", lit(page == Page::Scenes), WHITE),
        GO_ROUTING => look("ROUTING", lit(matches!(page, Page::Routing(_))), WHITE),
        GO_MULTIVIEW | GO_STREAM_ATEM | GO_SCREENS_ATEM => {
            let Page::Routing(on) = page else { return blank() };
            let t = Target::ALL[(key - GO_MULTIVIEW) as usize];
            let text = match t {
                Target::MultiView => "MULTI\nVIEW",
                Target::StreamAtem => "STREAM\nATEM",
                Target::ScreensAtem => "SCREENS\nATEM",
            };
            look(text, if on == t { SUB_ON } else { SUB }, WHITE)
        }
        _ => blank(),
    }
}

fn scene_look(i: usize, snap: &Snapshot) -> KeyLook {
    let Some(scene) = snap.scenes.get(i) else { return blank() };
    let text = wrap_cue_name(&scene.key_text(), CUE_LINE_WIDTH);
    if snap.active_scene == Some(scene.id) { look(text, AMBER, BLACK) } else { look(text, SCENE_BG, WHITE) }
}

/// A key that is lit when it can be pressed, and blinks when the engine flashes it.
fn action(label: &str, bg: Rgb, fg: Rgb, lit: bool, flash: Option<FlashKey>, snap: &Snapshot, phase: bool) -> KeyLook {
    let l = if lit { look(label, bg, fg) } else { dim(label) };
    if flash.is_some() && snap.flash_key == flash { blinking(l, phase) } else { l }
}

fn service_look(key: u8, snap: &Snapshot, phase: bool) -> KeyLook {
    let recover = |snap: &Snapshot| {
        if offered(snap, Offer::Recover) {
            action("RECOVER", AMBER, BLACK, true, Some(FlashKey::Recover), snap, phase)
        } else {
            blank()
        }
    };
    let start_now =
        |snap: &Snapshot| action("START\nSERVICE\nNOW", RED, WHITE, true, Some(FlashKey::GoLive), snap, phase);
    let abandon = |snap: &Snapshot| if snap.can_abandon() { look("ABANDON", Rgb(120, 30, 30), WHITE) } else { blank() };
    match service_step(&snap.stage) {
        ServiceStep::Before => match key {
            PREPARE => look("PREPARE", BLUE, WHITE),
            START_NOW if snap.stage == Stage::Idle => start_now(snap),
            END_BROADCAST if offered(snap, Offer::EndBroadcast) => look("END\nBCAST", Rgb(120, 30, 30), WHITE),
            RECOVER => recover(snap),
            _ => blank(),
        },
        ServiceStep::Prepared => match key {
            PREPARE => look("PREPARE\nAGAIN", SLATE, WHITE),
            COUNTDOWN => look("START\nCOUNT\nDOWN", AMBER, BLACK),
            START_NOW => start_now(snap),
            RECOVER => recover(snap),
            ABANDON => abandon(snap),
            _ => blank(),
        },
        ServiceStep::Countdown => match key {
            CD_START_NOW => start_now(snap),
            STOP_COUNTDOWN if snap.public_before_live() => blank(),
            STOP_COUNTDOWN => look("STOP\nCOUNT\nDOWN", AMBER, BLACK),
            ABANDON => abandon(snap),
            GO_PUBLIC => {
                let public = matches!(snap.stage, Stage::Countdown { public: true, .. });
                if public { dim("PUBLIC") } else { look("GO\nPUBLIC\nNOW", Rgb(0, 110, 160), WHITE) }
            }
            _ => blank(),
        },
        ServiceStep::Live => match key {
            END_SERVICE => look("END\nSERVICE", Rgb(120, 30, 30), WHITE),
            OFFER_KEY => match live_offer(snap) {
                Some(Offer::Recover) => recover(snap),
                Some(Offer::GoLive) => action("GO\nLIVE", RED, WHITE, true, Some(FlashKey::GoLive), snap, phase),
                Some(Offer::GoPublic) => look("RETRY\nPUBLIC", Rgb(0, 110, 160), WHITE),
                Some(Offer::EndBroadcast) => look("END\nBCAST", Rgb(120, 30, 30), WHITE),
                None => blank(),
            },
            k => match live_scene_slot(k) {
                Some(i) => scene_look(i, snap),
                None => video_look(k, snap, phase),
            },
        },
        ServiceStep::Ending => match key {
            SKIP_HOLD => look("SKIP\nHOLD", RED, WHITE),
            END_BROADCAST_ENDING => look("END\nBCAST", Rgb(120, 30, 30), WHITE),
            _ => blank(),
        },
    }
}

fn video_look(key: u8, snap: &Snapshot, blink_phase: bool) -> KeyLook {
    if let Some(i) = cue_slot(key) {
        let Some((cue, state)) = snap.cues.get(i) else {
            return blank();
        };
        let text = wrap_cue_name(&cue.name, CUE_LINE_WIDTH);
        if snap.test.as_ref().is_some_and(|t| t.cue == *cue) {
            return look(format!("TEST\n{}", wrap_cue_name(&cue.name, CUE_LINE_WIDTH)), AMBER, BLACK);
        }
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
        STOP_VIDEO if snap.playing.is_some() || snap.test.is_some() => look("STOP\nVIDEO", RED, WHITE),
        STOP_VIDEO => dim("STOP\nVIDEO"),
        UNMUTE if unmute_pressable(snap) => look("STREAM\nAUDIO UP", AMBER, BLACK),
        UNMUTE => dim("STREAM\nAUDIO UP"),
        _ => blank(),
    }
}

fn target_look(t: Target, key: u8, snap: &Snapshot, blink_phase: bool, armed: Option<(Target, u8)>) -> KeyLook {
    let state = snap.routing.target(t);
    let label = |i: Option<u16>| i.map_or_else(|| "?".to_string(), |i| snap.routing.label(i).to_uppercase());
    if let Some(slot) = preset_slot(key) {
        return match state.preset(slot) {
            Some(_) if armed == Some((t, slot)) => blinking(look("PRESS\nAGAIN", AMBER, BLACK), blink_phase),
            Some(p) => {
                let bg = if state.is_current(p) { GREEN } else { SLATE };
                look(wrap_cue_name(&p.name.to_uppercase(), CUE_LINE_WIDTH), bg, WHITE)
            }
            None => look(format!("{slot}\nEMPTY"), DIM_BG, DIM_FG),
        };
    }
    if t == Target::MultiView {
        if let Some(v) = MV_KEYS.iter().position(|&k| k == key) {
            let text = format!("{}\n{}", VIEW_KEYS[v], wrap_cue_name(&label(state.dest(v)), CUE_LINE_WIDTH));
            // In the view's own colour, so the key matches that view in the window.
            return look(text, VIEW_COLOURS[v], BLACK);
        }
        return blank();
    }
    if let Some(i) = key.checked_sub(FIRST_ATEM_INPUT).map(usize::from).filter(|&i| i < t.size()) {
        return atem_input_look(t, i, state, snap, &label);
    }
    blank()
}

/// An ATEM input on a routing page: red on program, green on preview, dim when the router
/// does not feed it.
fn atem_input_look(
    t: Target,
    i: usize,
    state: &TargetState,
    snap: &Snapshot,
    label: &dyn Fn(Option<u16>) -> String,
) -> KeyLook {
    let name = format!("IN {}", i + 1);
    if state.output(i) == 0 {
        return look(format!("{name}\n—"), DIM_BG, DIM_FG);
    }
    if locked(t, i, snap.job.is_some(), snap.routing.media) {
        return look(format!("{name}\nLOCKED"), DIM_BG, AMBER);
    }
    let text = format!("{name}\n{}", wrap_cue_name(&label(state.dest(i)), CUE_LINE_WIDTH));
    if state.on_air(i) {
        look(text, RED, WHITE)
    } else if state.on_preview(i) {
        look(text, GREEN, WHITE)
    } else {
        look(text, if t == Target::StreamAtem { STREAM_INPUT_BG } else { SCREENS_INPUT_BG }, WHITE)
    }
}

#[cfg(test)]
mod tests;
