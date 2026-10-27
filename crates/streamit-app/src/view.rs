//! What the screens show, worked out from the snapshot and the settings. No egui here:
//! the drawing lives in `ui/`, so these choices can be tested.
use crate::session::{Mode, REFUSED};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use streamit_core::config::Config;
use streamit_core::devices::{AudioInput, AudioMix, EncoderStatus, Health};
use streamit_core::engine::{DeviceHealth, DeviceKind, FlashKey, Snapshot};
use streamit_core::golive::{Readiness, Stage};
use streamit_core::recovery::Offer;
use streamit_core::rollin::{Cue, CueState, Destination};
use streamit_core::routing::Target;
use streamit_core::steps::{self, StepGroup, StepId, StepMark};
use streamit_sim::{Fault, Input8Source, PlayerView, Speed};

/// The colour family of a badge, a dot or a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Grey,
    Green,
    Amber,
    Red,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderView {
    pub badge: String,
    pub badge_tone: Tone,
    /// How long the service has been on air ("00:32:15"), while it is.
    pub timer: Option<String>,
    pub broadcast: String,
    /// "All equipment OK", or the first device with a problem and what it is.
    pub equipment: String,
    pub equipment_tone: Tone,
    pub simulation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Done,
    Current,
    Todo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceScreen {
    BeforeService,
    Countdown,
    Live,
    VideoPlaying,
    Ending,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CountdownView {
    /// What is left of the countdown video ("0:42").
    pub remaining: String,
    pub file_name: String,
    /// When YouTube goes public, as countdown time ("0:10").
    pub public_at_text: String,
    pub public: bool,
    /// "NDI counter sending 0:42 as “StreamIt Countdown”".
    pub ndi_text: String,
    pub ndi_ok: bool,
    /// How much of the video has played, 0..=1 (0 while its length is unknown).
    pub progress: f32,
    /// Going public failed and can be retried.
    pub show_retry_public: bool,
}

/// Every device, in the order the screens list them. The lighting console shows only once
/// one is set up (see `shown_kinds`).
pub const ALL_KINDS: [DeviceKind; 10] = [
    DeviceKind::StreamAtem,
    DeviceKind::ScreensAtem,
    DeviceKind::VideoHub,
    DeviceKind::WebPresenter,
    DeviceKind::YouTube,
    DeviceKind::DeckLink,
    DeviceKind::Ndi,
    DeviceKind::StreamDeck,
    DeviceKind::Lighting,
    DeviceKind::MultiView,
];

/// The devices to list: all of them, but the lighting console and the MultiView only when
/// they are set up (their health is then reported).
pub fn shown_kinds(s: &Snapshot) -> Vec<DeviceKind> {
    ALL_KINDS
        .into_iter()
        .filter(|k| !matches!(k, DeviceKind::Lighting | DeviceKind::MultiView) || s.device_status.contains_key(k))
        .collect()
}

pub fn kind_name(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::StreamAtem => "Stream ATEM",
        DeviceKind::ScreensAtem => "Screens ATEM",
        DeviceKind::VideoHub => "VideoHub",
        DeviceKind::WebPresenter => "Web Presenter",
        DeviceKind::YouTube => "YouTube",
        DeviceKind::DeckLink => "DeckLink",
        DeviceKind::Ndi => "NDI counter",
        DeviceKind::StreamDeck => "Stream Deck",
        DeviceKind::Lighting => "Lighting",
        DeviceKind::MultiView => "MultiView",
    }
}

/// A device's colour and, when it is not OK, what is wrong.
pub fn device_state(s: &Snapshot, k: DeviceKind) -> (Tone, Option<String>) {
    match s.device_status.get(&k) {
        Some(DeviceHealth::Ok) => (Tone::Green, None),
        Some(DeviceHealth::Warning(w)) => (Tone::Amber, Some(w.clone())),
        Some(DeviceHealth::Down(e)) => (Tone::Red, Some(e.clone())),
        None => (Tone::Grey, Some("not checked yet".into())),
    }
}

/// Going public failed (or was never done) and can be retried.
fn public_failed(s: &Snapshot) -> bool {
    s.offers.contains(&Offer::GoPublic)
}

pub fn header(s: &Snapshot, _c: &Config) -> HeaderView {
    let (badge, badge_tone) = match &s.stage {
        Stage::Idle => ("IDLE".to_string(), Tone::Grey),
        Stage::Prepared(Readiness::Ready) => ("READY".into(), Tone::Green),
        Stage::Prepared(Readiness::Waiting(_)) => ("PREPARING".into(), Tone::Amber),
        Stage::Prepared(Readiness::Warning(_)) => ("READY".into(), Tone::Amber),
        Stage::Countdown { public, .. } => {
            (format!("COUNTDOWN · {}", if *public { "PUBLIC" } else { "PRIVATE" }), Tone::Amber)
        }
        Stage::Live => (format!("LIVE · {}", if public_failed(s) { "PRIVATE" } else { "PUBLIC" }), Tone::Red),
        Stage::Ending { .. } => ("ENDING".into(), Tone::Red),
        Stage::Ended => ("ENDED".into(), Tone::Grey),
    };
    let timer = s
        .on_air_since
        .filter(|_| matches!(s.stage, Stage::Live | Stage::Ending { .. }))
        .map(|t| fmt_hhmmss(SystemTime::now().duration_since(t).unwrap_or_default()));
    let broadcast = s.broadcast.as_ref().map(|b| b.title.clone()).unwrap_or_else(|| "No broadcast selected".into());
    // The worst problem first (red before amber, then list order), and how many more.
    let problems: Vec<(DeviceKind, Tone, String)> = shown_kinds(s)
        .into_iter()
        .filter_map(|k| match device_state(s, k) {
            (tone, Some(why)) => Some((k, tone, why)),
            _ => None,
        })
        .collect();
    let worst = problems.iter().find(|p| p.1 == Tone::Red).or_else(|| problems.first());
    let (equipment, equipment_tone) = match worst {
        None => ("All equipment OK".to_string(), Tone::Green),
        Some((k, tone, why)) => {
            let more = match problems.len() - 1 {
                0 => String::new(),
                n => format!(" (+{n} more)"),
            };
            (format!("{} · {why}{more}", kind_name(*k)), *tone)
        }
    };
    HeaderView { badge, badge_tone, timer, broadcast, equipment, equipment_tone, simulation: s.simulation }
}

/// What the Service tab's More menu may send now (the rest is always enabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoreMenu {
    pub start_now: bool,
    /// Why Start service now is disabled, when there is a reason to give.
    pub start_now_reason: Option<&'static str>,
    /// From Idle, Start service now takes a second (confirm) click.
    pub start_now_confirm: bool,
    pub start_now_hint: &'static str,
    pub end_service: bool,
}

/// Why Prepare and Start service now wait while Recover is offered before the service.
pub const RECOVER_FIRST: &str = "Recover first";

/// Mirrors the engine: Start service now runs from Idle or Prepared when nothing else
/// runs (the countdown has its own button); End Service starts when nothing runs, stops
/// a video first, and a second press during End Service skips the closing countdown or hold. Before
/// the service, a pending Recover comes first (S6), and from Idle Start service now asks
/// for a confirm: the encoder is not streaming yet.
pub fn more_menu(s: &Snapshot) -> MoreMenu {
    let recover = recover_first(s);
    let idle = s.stage == Stage::Idle;
    MoreMenu {
        start_now: matches!(s.stage, Stage::Idle | Stage::Prepared(_)) && s.job.is_none() && !recover,
        start_now_reason: recover.then_some(RECOVER_FIRST),
        start_now_confirm: idle,
        start_now_hint: if idle {
            "Prepare first is safer — the encoder isn’t streaming yet."
        } else {
            "Cut to the camera and go public, skipping the countdown."
        },
        end_service: s.playing.is_some() || matches!(s.job, None | Some("End Service") | Some("a video")),
    }
}

/// Before the service with Recover offered: a restart found something left on the
/// DeckLink (or the mics down). Prepare would put the opening still on air and floor the
/// mics, so Recover goes first.
pub fn recover_first(s: &Snapshot) -> bool {
    s.stage == Stage::Idle && s.offers.contains(&Offer::Recover)
}

/// The big step on the "before the service" screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep {
    Prepare,
    Recover,
    Countdown,
}

pub fn next_step(s: &Snapshot) -> NextStep {
    match s.stage {
        Stage::Prepared(_) => NextStep::Countdown,
        _ if recover_first(s) => NextStep::Recover,
        _ => NextStep::Prepare,
    }
}

/// The next step's heading and explanation.
pub fn next_step_text(s: &Snapshot, c: &Config) -> (&'static str, String) {
    match next_step(s) {
        NextStep::Countdown if s.countdown_off => (
            "Start the service",
            "The countdown is turned off (Videos page). Start service now cuts the stream to the camera and \
             goes public."
                .to_string(),
        ),
        NextStep::Countdown => (
            "Start the countdown",
            format!(
                "{} YouTube goes public at {} and the stream cuts to the camera at 0:00.",
                if c.playout.countdown_to_stream {
                    "Plays the countdown video on the hall screens and online, with its sound."
                } else {
                    "Plays the countdown video on the hall screens; the stream stays on the opening still."
                },
                fmt_mmss(c.timings.go_public_offset)
            ),
        ),
        NextStep::Recover => (
            "Recover first",
            "StreamIt restarted with the equipment left mid-video (or the stream audio down). Recover puts it \
             back: the stream and hall off the video, stream audio up, input 8 back on its camera. Then carry on."
                .to_string(),
        ),
        NextStep::Prepare => (
            "Prepare the stream",
            "Puts the “Stream starting soon” still on air in silence, starts the Web Presenter, \
             and waits for YouTube to receive it privately. Do this 10–20 minutes before the service."
                .to_string(),
        ),
    }
}

/// A step button: whether it can be pressed, and why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepButton {
    pub enabled: bool,
    pub reason: Option<&'static str>,
}

/// Prepare waits for any running sequence, and for Recover when it is offered first.
pub fn prepare_button(s: &Snapshot) -> StepButton {
    let recover = recover_first(s);
    StepButton { enabled: s.job.is_none() && !recover, reason: recover.then_some(RECOVER_FIRST) }
}

/// The stream audio bar on the Service screens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicBar {
    pub fraction: f32,
    pub tone: Tone,
    pub text: &'static str,
}

/// Before T-0 and during the close the stream audio is down by design. Live, "down for
/// video" only while a video plays; down with no video (a restart, a failed fade up) is a
/// problem Recover fixes.
pub fn mic_bar(s: &Snapshot) -> MicBar {
    match s.stage {
        Stage::Prepared(_) | Stage::Countdown { .. } => {
            return MicBar { fraction: 0.04, tone: Tone::Grey, text: "silent until live" };
        }
        Stage::Ending { .. } => return MicBar { fraction: 0.04, tone: Tone::Grey, text: "down for the close" },
        _ => {}
    }
    match (s.mics_ducked, s.playing.is_some()) {
        (false, _) => MicBar { fraction: 0.72, tone: Tone::Green, text: "live" },
        (true, true) => MicBar { fraction: 0.04, tone: Tone::Amber, text: "down for video" },
        (true, false) => MicBar { fraction: 0.04, tone: Tone::Red, text: "down — press Recover" },
    }
}

/// Under a setting the running simulation took from the settings file when it opened:
/// said when the saved value has changed since.
pub const SIM_NEXT_TIME: &str = "applies next time you open simulation";

pub fn sim_next_time_note(running: &Path, saved: &Path) -> Option<&'static str> {
    (running != saved).then_some(SIM_NEXT_TIME)
}

/// What to say when the engine ignored a key press.
pub fn flash_key_text(k: FlashKey) -> String {
    let name = match k {
        FlashKey::GoLive => "Start service now",
        FlashKey::Recover => "Recover",
    };
    format!("{name} is not available right now")
}

/// Prepare → Countdown → Live → End service.
pub fn steps(s: &Snapshot) -> [StepState; 4] {
    use StepState::*;
    let current = match s.stage {
        Stage::Idle => 0,
        Stage::Prepared(_) | Stage::Countdown { .. } => 1,
        Stage::Live => 2,
        Stage::Ending { .. } => 3,
        Stage::Ended => 4,
    };
    std::array::from_fn(|i| match i.cmp(&current) {
        std::cmp::Ordering::Less => Done,
        std::cmp::Ordering::Equal => Current,
        std::cmp::Ordering::Greater => Todo,
    })
}

/// Which Service screen to show. Chosen by stage: `countdown_total` lingers into Live.
pub fn service_screen(s: &Snapshot) -> ServiceScreen {
    match s.stage {
        Stage::Countdown { .. } => ServiceScreen::Countdown,
        Stage::Ending { .. } => ServiceScreen::Ending,
        _ if s.playing.is_some() => ServiceScreen::VideoPlaying,
        Stage::Live => ServiceScreen::Live,
        Stage::Idle | Stage::Prepared(_) | Stage::Ended => ServiceScreen::BeforeService,
    }
}

pub fn countdown(s: &Snapshot, c: &Config) -> Option<CountdownView> {
    let Stage::Countdown { remaining, public } = s.stage else { return None };
    let text = fmt_mmss(remaining);
    let progress = match s.countdown_total {
        Some(total) if !total.is_zero() => (1.0 - remaining.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0),
        _ => 0.0,
    };
    Some(CountdownView {
        file_name: c
            .playout
            .countdown_file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| c.playout.countdown_file.display().to_string()),
        public_at_text: fmt_mmss(c.timings.go_public_offset),
        public,
        ndi_text: format!("NDI counter sending {text} as “{}”", c.ndi.source_name),
        ndi_ok: matches!(s.device_status.get(&DeviceKind::Ndi), Some(DeviceHealth::Ok)),
        progress,
        // A public countdown has nothing to retry, whatever an older offer says.
        show_retry_public: public_failed(s) && !public,
        remaining: text,
    })
}

/// `M:SS`, rounding a part second up, as a countdown does (41.2 s shows 0:42).
pub fn fmt_mmss(d: Duration) -> String {
    let secs = d.as_secs() + u64::from(d.subsec_nanos() > 0);
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// `M:SS` of elapsed time, whole seconds (1:12.8 played shows 1:12).
pub fn fmt_mmss_floor(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The first part of a device problem, for tight spaces ("disconnected: atem not
/// connected" shows "disconnected").
pub fn short_problem(why: &str) -> &str {
    why.split(':').next().unwrap_or(why).trim()
}

/// Where a video plays, as its chip says.
pub fn dest_short(d: Destination) -> &'static str {
    match d {
        Destination::HallAndOnline => "Hall + Online",
        Destination::HallOnly => "Hall only",
        Destination::OnlineOnly => "Online only",
        Destination::AudioOnly => "Audio only",
    }
}

/// A level in whole dB with a real minus sign ("−2 dB", "0 dB", "+3 dB").
pub fn fmt_db(db: f32) -> String {
    let r = db.round() as i32;
    match r {
        0 => "0 dB".into(),
        r if r < 0 => format!("−{} dB", -r),
        r => format!("+{r} dB"),
    }
}

/// What a length shows until the file has been read.
pub const LENGTH_UNKNOWN: &str = "–:––";

/// The Stream Deck key of video `number` (1-based) on the deck's Videos page: keys 9–15
/// for 1–7, then 17–23 for 8–14, because key 16 is Stop video.
pub fn deck_key(number: usize) -> u8 {
    streamit_deck::layout::cue_key_number(number.saturating_sub(1))
}

/// What reading a clip's length found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipLength {
    Known(Duration),
    /// No file at that path.
    Missing,
    /// The file is there but its length can't be read (not an mp4/mov StreamIt reads).
    Unreadable,
}

/// Reads a clip's length from its header (blocking: call off the UI thread). Never the
/// simulation's made-up 30 s.
pub fn read_clip(path: &Path) -> ClipLength {
    if !path.exists() {
        return ClipLength::Missing;
    }
    streamit_sim::clip::read_length(path).map_or(ClipLength::Unreadable, ClipLength::Known)
}

/// `M:SS` of a clip, "file missing" or "length unknown", or "–:––" until it has been read.
pub fn length_text(l: Option<ClipLength>) -> String {
    match l {
        None => LENGTH_UNKNOWN.into(),
        Some(ClipLength::Known(d)) => fmt_mmss(d),
        Some(ClipLength::Missing) => "file missing".into(),
        Some(ClipLength::Unreadable) => "length unknown".into(),
    }
}

/// The file's own name, for the small line under a video's name.
pub fn file_name(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// One line of the Videos list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueRow {
    /// 1-based, as the operator counts.
    pub number: usize,
    pub deck_key: u8,
    pub name: String,
    pub file_name: String,
    pub length_text: String,
    pub destination_text: String,
    pub level_text: String,
}

/// The row for the cue at `index` (0-based).
pub fn cue_row(index: usize, cue: &Cue, lengths: &HashMap<PathBuf, ClipLength>) -> CueRow {
    CueRow {
        number: index + 1,
        deck_key: deck_key(index + 1),
        name: cue.name.clone(),
        file_name: file_name(&cue.path),
        length_text: length_text(lengths.get(&cue.path).copied()),
        destination_text: dest_short(cue.destination).into(),
        level_text: fmt_db(cue.gain_db),
    }
}

/// The Videos list for these cues (the window's copy while it is being edited). `lengths` are
/// the clip lengths read so far.
pub fn cue_rows(cues: &[Cue], lengths: &HashMap<PathBuf, ClipLength>) -> Vec<CueRow> {
    cues.iter().enumerate().map(|(i, c)| cue_row(i, c, lengths)).collect()
}

/// Where the window's copy of the video list stands against the engine's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueSync {
    /// In step, nothing pending.
    Idle,
    /// An edit is waiting to be sent, or sent and not yet in the engine's list.
    Applying,
    /// The engine refused our edit because a video is playing: fall back to its list.
    Reverted,
    /// Not reflected after the settle time and nothing is playing: send it once more.
    Resend,
    /// Not reflected even after a resend: fall back to the engine's list, neutral notice.
    GaveUp,
    /// The lists differ with nothing of ours pending: take the engine's silently.
    Resync,
}

/// `dirty`: an edit waits for the debounce. `sent_age`: how long ago the last SetCues was
/// sent. `differs`: the window's list is not the engine's. `playing`: a video plays now or
/// did when the edit was sent. `resent`: the edit has already been sent a second time.
pub fn cue_sync(
    dirty: bool,
    sent_age: Option<Duration>,
    differs: bool,
    settle: Duration,
    playing: bool,
    resent: bool,
) -> CueSync {
    match (dirty, sent_age) {
        (true, _) => CueSync::Applying,
        (false, _) if !differs => CueSync::Idle,
        (false, Some(a)) if a < settle => CueSync::Applying,
        (false, Some(_)) if playing => CueSync::Reverted,
        (false, Some(_)) if !resent => CueSync::Resend,
        (false, Some(_)) => CueSync::GaveUp,
        (false, None) => CueSync::Resync,
    }
}

pub const CUES_NOT_APPLIED_NOTICE: &str = "Your change to the video list wasn't applied. Please make it again.";

pub const CUES_REVERTED_NOTICE: &str =
    "Your change to the video list wasn't applied because a video started. Make it again when the video ends.";

/// Whether the video list is still unsaved after a save attempt: the file must have been
/// writable (backed up) and the write must have succeeded.
pub fn cues_unsaved_after(writable: bool, save_ok: bool) -> bool {
    !(writable && save_ok)
}

/// An unsaved list is tried again once `every` has passed since the last attempt.
pub fn should_retry_save(unsaved: bool, since_last: Option<Duration>, every: Duration) -> bool {
    unsaved && since_last.is_none_or(|d| d >= every)
}

/// After removing row `removed`, where the selection (`selected`, in a list now `len` long) points.
pub fn selection_after_remove(selected: usize, removed: usize, len: usize) -> usize {
    let s = if removed < selected { selected - 1 } else { selected };
    s.min(len.saturating_sub(1))
}

/// What each place a video can play does, shown under the four buttons.
pub fn destination_help(d: Destination) -> &'static str {
    match d {
        Destination::HallAndOnline => {
            "Hall screens and the stream both switch to the video. The stream audio fades down and comes back after."
        }
        Destination::HallOnly => {
            "Only the hall screens switch. The stream stays on the cameras with the stream audio up."
        }
        Destination::OnlineOnly => {
            "Only the stream switches to the video. The stream audio fades down and comes back after."
        }
        Destination::AudioOnly => "No picture changes. The video's sound plays and the stream audio fades down.",
    }
}

/// The first video not yet played: its Play button is the blue one.
pub fn next_cue(s: &Snapshot) -> Option<usize> {
    s.cues.iter().position(|(_, st)| *st == CueState::Cued)
}

/// `HH:MM:SS`, whole seconds, for the on-air timer.
/// "1,234" for 1234.
pub fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The YouTube quota line for Settings: units used today out of the daily quota, the
/// number of calls, and when it resets.
pub fn quota_text(units: u32, calls: u32, until_reset: Duration) -> String {
    let mins = until_reset.as_secs() / 60;
    format!(
        "{} of {} units used today · {} calls · resets at midnight Pacific time, in {} h {:02} min",
        thousands(units),
        thousands(streamit_youtube::quota::DAILY_LIMIT),
        thousands(calls),
        mins / 60,
        mins % 60
    )
}

/// How full the quota is: amber from 80 %, red once it is used up.
pub fn quota_tone(units: u32) -> Tone {
    let limit = streamit_youtube::quota::DAILY_LIMIT;
    if units >= limit {
        Tone::Red
    } else if units * 5 >= limit * 4 {
        Tone::Amber
    } else {
        Tone::Green
    }
}

pub fn fmt_hhmmss(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

// ---------------------------------------------------------------- Settings (screen 6)

/// The Simulation card's button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchState {
    pub label: &'static str,
    pub enabled: bool,
    /// Why it is disabled.
    pub reason: Option<&'static str>,
}

/// Mirrors `Sessions::switch`: allowed only at Idle or Ended with no sequence running.
/// Nothing is happening: no service under way and no sequence running. Switching to or
/// from simulation and installing an update wait for this.
pub fn between_services(s: &Snapshot) -> bool {
    matches!(s.stage, Stage::Idle | Stage::Ended) && s.job.is_none()
}

pub fn switch_state(s: &Snapshot, mode: Mode) -> SwitchState {
    let label = match mode {
        Mode::Real => "Switch to simulation",
        Mode::Simulation => "Leave simulation",
    };
    let enabled = between_services(s);
    SwitchState { label, enabled, reason: (!enabled).then_some(REFUSED) }
}

/// Under a VideoHub number the simulation filled in itself (ruling S4), what it uses.
pub fn sim_default_note(defaulted: &[&'static str], field: &str, value: u16) -> Option<String> {
    defaulted.contains(&field).then(|| format!("Simulation uses {value} (simulated default)"))
}

// ---------------------------------------------------------------- Simulation (screen 7)

/// The faults the "Make something go wrong" buttons toggle, in the mockup's order.
pub const FAULTS: [Fault; 7] = [
    Fault::Unplug(DeviceKind::VideoHub),
    Fault::Unplug(DeviceKind::StreamAtem),
    Fault::Unplug(DeviceKind::WebPresenter),
    Fault::YouTubeRefusesPublic,
    Fault::YouTubeSlow,
    Fault::NextVideoFails,
    Fault::MicFadeFails,
];

pub fn fault_label(f: Fault) -> &'static str {
    match f {
        Fault::Unplug(DeviceKind::VideoHub) => "Unplug VideoHub",
        Fault::Unplug(DeviceKind::StreamAtem) => "Unplug Stream ATEM",
        Fault::Unplug(DeviceKind::WebPresenter) => "Unplug Web Presenter",
        Fault::Unplug(_) => "Unplug",
        Fault::YouTubeRefusesPublic => "YouTube refuses public",
        Fault::YouTubeSlow => "YouTube very slow",
        Fault::NextVideoFails => "Next video won’t load",
        Fault::MicFadeFails => "Stream audio fade fails",
    }
}

/// The Stream ATEM sources the Simulation tab offers: inputs 1–5, 8 and media player 1.
pub fn sim_sources(media_player_input: u16) -> [u16; 7] {
    [1, 2, 3, 4, 5, 8, media_player_input]
}

pub fn source_label(input: u16, media_player_input: u16) -> String {
    if input == media_player_input { "MP1".into() } else { input.to_string() }
}

/// What input 8 carries: the DeckLink, Cam 8 on its normal router input, or another input.
pub fn input8_text(src: Input8Source, cam8_input: u16) -> String {
    match src {
        Input8Source::DeckLink => "DeckLink".into(),
        Input8Source::Other(0) => "Nothing routed".into(),
        Input8Source::Other(n) if n == cam8_input => format!("Cam 8 (router in {n})"),
        Input8Source::Other(n) => format!("Router in {n}"),
    }
}

pub fn player_text(p: &PlayerView) -> String {
    match p {
        PlayerView::Empty => "idle".into(),
        PlayerView::Loaded { duration } => format!("loaded · {}", fmt_mmss(*duration)),
        PlayerView::Playing { position, duration } => {
            format!("playing {} / {}", fmt_mmss_floor(*position), fmt_mmss(*duration))
        }
    }
}

pub fn encoder_text(e: &EncoderStatus) -> String {
    match e {
        EncoderStatus::Unknown(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// The pretend broadcast's life cycle, saying whether viewers can see it.
pub fn yt_state_text(life_cycle: Option<&str>) -> String {
    match life_cycle {
        None => "no broadcast".into(),
        Some("live") => "live (public)".into(),
        Some("testing") => "testing (not public)".into(),
        Some(other) => other.into(),
    }
}

/// The Service screens' YouTube panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YouTubePanel {
    pub tone: Tone,
    pub headline: String,
    pub detail: String,
    /// "Public" or "Private", while the service is under way and it is known.
    pub visibility: Option<&'static str>,
    pub viewers: Option<u64>,
}

/// What the YouTube panel says: is YouTube receiving the stream, is the broadcast public,
/// how many are watching.
pub fn youtube_panel(s: &Snapshot) -> YouTubePanel {
    let (tone, headline, detail) = match &s.stream_health {
        None => (Tone::Grey, "Checking…".to_string(), "Asking YouTube about the stream".to_string()),
        Some(Health::Good) => (Tone::Green, "Receiving".into(), "Stream health good".into()),
        Some(Health::Bad(why)) if why.starts_with("could not check") => {
            (Tone::Amber, "Can’t check".into(), format!("Stream health {why}"))
        }
        Some(Health::Bad(why)) => (Tone::Amber, "Receiving, poor".into(), format!("YouTube says: {why}")),
        Some(Health::NotReceiving) if s.stream_alarm => {
            (Tone::Red, "NOT RECEIVING".into(), "Nothing is arriving. Check the Web Presenter and the internet.".into())
        }
        Some(Health::NotReceiving) if s.stage == Stage::Live => {
            (Tone::Amber, "Not receiving".into(), "For a moment: the alarm sounds after 20 s".into())
        }
        Some(Health::NotReceiving) => (Tone::Amber, "Not receiving yet".into(), "Waiting for the stream".into()),
    };
    let visibility = match s.stage {
        Stage::Prepared(_) => Some("Private"),
        Stage::Countdown { public, .. } => Some(if public { "Public" } else { "Private" }),
        Stage::Live => Some(if s.offers.contains(&Offer::GoPublic) { "Not public" } else { "Public" }),
        _ => None,
    };
    YouTubePanel { tone, headline, detail, visibility, viewers: s.live.as_ref().and_then(|l| l.viewers) }
}

/// What an ATEM input is called on the Service screens and in the pickers: the video
/// playing into it, the media player's still, the ATEM's own name for it, or the VideoHub
/// input that feeds it.
pub fn atem_input_name(s: &Snapshot, c: &Config, target: Target, input: u16) -> String {
    let video_input = match target {
        Target::StreamAtem => c.stream_atem.decklink_input,
        _ => c.screens_atem.decklink_input,
    };
    if input == video_input
        && let Some((cue, _)) = s.playing.and_then(|i| s.cues.get(i))
    {
        return format!("Video: {}", cue.name);
    }
    if target == Target::StreamAtem && input == 3000 + 10 * u16::from(c.stream_atem.media_player) {
        return "Holding still".into();
    }
    let t = s.routing.target(target);
    if let Some(name) = t.own_name(input) {
        return name.to_string();
    }
    let fed = input.checked_sub(1).map(usize::from).filter(|&i| i < t.outputs.len() && t.output(i) != 0);
    match fed.and_then(|i| t.dest(i)) {
        Some(hub_input) => s.routing.label(hub_input),
        None if (1..=99).contains(&input) => format!("Input {input}"),
        None => format!("Source {input}"),
    }
}

/// An ATEM's program and preview as the Service screens name them; `None` until it says.
pub fn atem_sees(s: &Snapshot, c: &Config, target: Target) -> (Option<String>, Option<String>) {
    let t = s.routing.target(target);
    let name = |n: Option<u16>| n.map(|n| atem_input_name(s, c, target, n));
    (name(t.program), name(t.preview))
}

/// A shown audio input's state as a short tag and its colour.
pub fn audio_tag(input: Option<AudioInput>) -> (&'static str, Tone) {
    match input.map(|i| i.mix) {
        Some(AudioMix::On) => ("ON", Tone::Green),
        Some(AudioMix::FollowVideo) => ("AFV", Tone::Grey),
        Some(AudioMix::Off) => ("OFF", Tone::Grey),
        None => ("—", Tone::Grey),
    }
}

/// A chat message's time as the window shows it: local hours and minutes.
pub fn chat_time(published: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(published)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

/// The broadcast's live chat on YouTube, in its own window.
pub fn chat_url(broadcast_id: &str) -> String {
    format!("https://www.youtube.com/live_chat?is_popout=1&v={broadcast_id}")
}

/// A fader position: "−∞" at the floor, "0 dB" at unity, otherwise signed whole dB.
pub fn fader_text(db: f32) -> String {
    if db <= -100.0 {
        "−∞".into()
    } else if db.abs() < 0.5 {
        "0 dB".into()
    } else {
        format!("{db:+.0} dB")
    }
}

/// The YouTube Studio page for running a live broadcast.
pub fn studio_url(broadcast_id: &str) -> String {
    format!("https://studio.youtube.com/video/{broadcast_id}/livestreaming")
}

pub fn health_text(h: &Health) -> String {
    match h {
        Health::Good => "good".into(),
        Health::NotReceiving => "not receiving".into(),
        Health::Bad(why) => format!("bad: {why}"),
    }
}

/// How long the last go-public took. Pretend delays are scaled by the speed (a 3 s
/// transition takes 0.3 s at 10×), so the time is labelled with the current speed.
pub fn went_public_text(d: Option<Duration>, speed: Speed) -> String {
    match d {
        Some(d) => format!("{:.1} s at {}×", d.as_secs_f32(), speed.factor()),
        None => "—".into(),
    }
}

/// How long a pretend mix takes at `speed` (1 s at 1×).
pub fn mix_text(speed: Speed) -> String {
    let secs = 1.0 / speed.factor() as f32;
    format!("Mix takes {} s at {}×", format!("{secs:.2}").trim_end_matches('0').trim_end_matches('.'), speed.factor())
}

/// How full the mic bar is: −40 dB (or off) empty, +6 dB full.
pub fn mic_fraction(db: f32) -> f32 {
    ((db + 40.0) / 46.0).clamp(0.0, 1.0)
}

/// One circle in the list of steps: grey to do, amber now, green done, red failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub label: &'static str,
    pub detail: Option<String>,
    /// When it was done ("10:18:24").
    pub time: Option<String>,
    pub tone: Tone,
}

/// The list's heading, as the steps bar names the stage.
pub fn step_heading(g: StepGroup) -> &'static str {
    match g {
        StepGroup::Prepare => "1 Prepare",
        StepGroup::Countdown => "2 Countdown",
        StepGroup::End => "4 End service",
    }
}

/// `g`'s steps as they stand, with a line under each saying what it waits on, why it
/// failed or (not started yet) what to expect.
pub fn step_rows(s: &Snapshot, c: &Config, g: StepGroup) -> Vec<StepRow> {
    steps::of_group(&s.steps, g)
        .into_iter()
        // The stream step only when the countdown goes online too; the closing countdown's
        // only when it plays.
        .filter(|st| st.id != StepId::StreamCountdown || c.playout.countdown_to_stream)
        .filter(|st| st.id != StepId::ClosingVideo || c.playout.closing_plays())
        .map(|st| {
            let (tone, detail, time) = match st.mark {
                StepMark::Todo => (Tone::Grey, todo_detail(st.id, c), None),
                StepMark::Now(d) => (Tone::Amber, d.or_else(|| now_detail(st.id, s)), None),
                StepMark::Done(at) => (
                    Tone::Green,
                    None,
                    Some(chrono::DateTime::<chrono::Local>::from(at).format("%H:%M:%S").to_string()),
                ),
                StepMark::Failed(why) => (Tone::Red, Some(why), None),
            };
            StepRow { label: st.id.label(), detail, time, tone }
        })
        .collect()
}

/// The heading's circle: green once every step is done, red if one failed, amber
/// while under way.
pub fn steps_tone(rows: &[StepRow]) -> Tone {
    if rows.iter().all(|r| r.tone == Tone::Green) {
        Tone::Green
    } else if rows.iter().any(|r| r.tone == Tone::Amber) {
        Tone::Amber
    } else if rows.iter().any(|r| r.tone == Tone::Red) {
        Tone::Red
    } else if rows.iter().any(|r| r.tone == Tone::Green) {
        Tone::Amber
    } else {
        Tone::Grey
    }
}

fn todo_detail(id: StepId, c: &Config) -> Option<String> {
    Some(match id {
        StepId::OpeningStill => "Online sees “Stream starting soon…”".into(),
        StepId::YouTubeReceiving => "Waits up to 60 s".into(),
        StepId::YouTubeTesting => "Still private".into(),
        StepId::HallCountdown => format!("Screens ATEM input {}", c.screens_atem.decklink_input),
        StepId::StreamCountdown => format!("Stream ATEM input {}, with its sound", c.stream_atem.decklink_input),
        StepId::YouTubePublic => format!("At {} left", fmt_mmss(c.timings.go_public_offset)),
        StepId::ClosingVideo => closing_where(c).into(),
        StepId::ClosingHold => format!("{}, can be skipped", fmt_mmss(c.timings.closing_hold)),
        _ => return None,
    })
}

fn now_detail(id: StepId, s: &Snapshot) -> Option<String> {
    match (id, &s.stage) {
        (StepId::CountdownVideo, Stage::Countdown { remaining, .. }) => Some(format!("{} left", fmt_mmss(*remaining))),
        (StepId::ClosingVideo | StepId::ClosingHold, Stage::Ending { remaining }) => {
            Some(format!("{} left", fmt_mmss(*remaining)))
        }
        _ => None,
    }
}

/// Where the closing countdown plays.
pub fn closing_where(c: &Config) -> &'static str {
    match (c.playout.closing_to_hall, c.playout.closing_to_stream) {
        (true, true) => "Hall screens and online, with its sound",
        (true, false) => "Hall screens only",
        (false, true) => "Online only, with its sound",
        (false, false) => "Nowhere: pick the hall, online or both",
    }
}

/// End Service is playing the closing countdown (before the closing still and the hold).
pub fn closing_playing(s: &Snapshot) -> bool {
    s.steps.iter().any(|st| st.id == StepId::ClosingVideo && matches!(st.mark, StepMark::Now(_)))
}

/// Why a test play cannot start now, or `None` when it can. `c` is the config the
/// engine runs with (the simulation's own, with its router numbers filled in).
pub fn test_block(s: &Snapshot, c: &Config) -> Option<String> {
    if matches!(s.stage, Stage::Countdown { .. } | Stage::Live | Stage::Ending { .. }) {
        return Some("Test play is off from the countdown until the service ends".into());
    }
    if let Some(job) = s.job {
        return Some(format!("Waits until {job} ends"));
    }
    if c.videohub.input8_output == 0 || c.videohub.decklink_input == 0 {
        return Some(
            "Set the Media input's router output (Routing › Config) and the video source (Routing › VideoHub)".into(),
        );
    }
    if test_on_stream(s, c) {
        return Some(format!("Stream ATEM input {} is on the stream", c.stream_atem.decklink_input));
    }
    None
}

/// The Stream ATEM has its DeckLink input on program, so a test would be seen online.
pub fn test_on_stream(s: &Snapshot, c: &Config) -> bool {
    s.routing.target(Target::StreamAtem).program == Some(c.stream_atem.decklink_input)
}

/// The test play's time left ("1:42"), once it is playing.
pub fn test_left(s: &Snapshot) -> Option<String> {
    s.test.as_ref().filter(|t| t.duration > Duration::ZERO).map(|t| fmt_mmss(t.duration.saturating_sub(t.position)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamit_core::devices::{BroadcastSummary, LiveDetails};

    #[test]
    fn test_play_says_why_it_cannot_start() {
        let mut c = Config::default();
        let mut s = Snapshot::default();
        assert!(test_block(&s, &c).unwrap().contains("Routing › Config"), "router numbers unset");
        c.videohub.input8_output = 11;
        c.videohub.decklink_input = 39;
        assert_eq!(test_block(&s, &c), None);
        s.routing.target_mut(Target::StreamAtem).program = Some(8);
        assert_eq!(test_block(&s, &c).as_deref(), Some("Stream ATEM input 8 is on the stream"));
        s.job = Some("a test play");
        assert_eq!(test_block(&s, &c).as_deref(), Some("Waits until a test play ends"));
        s.stage = Stage::Live;
        assert!(test_block(&s, &c).unwrap().starts_with("Test play is off"));
        assert_eq!(test_left(&s), None);
        s.test = Some(streamit_core::engine::TestPlay {
            cue: cue_at("Hymn", "hymn.mp4", 0.0),
            position: Duration::from_millis(126_200),
            duration: Duration::from_secs(228),
        });
        assert_eq!(test_left(&s).as_deref(), Some("1:42"));
    }

    fn cue_at(name: &str, path: &str, gain: f32) -> streamit_core::rollin::Cue {
        streamit_core::rollin::Cue {
            path: PathBuf::from(path),
            name: name.into(),
            destination: Destination::HallAndOnline,
            gain_db: gain,
            return_input: None,
            scene: None,
        }
    }

    #[test]
    fn deck_keys_skip_the_stop_video_key() {
        assert_eq!(deck_key(1), 9);
        assert_eq!(deck_key(7), 15);
        assert_eq!(deck_key(8), 17);
        assert_eq!(deck_key(14), 23);
    }

    #[test]
    fn cue_row_texts() {
        let cues = [cue_at("Notices", r"C:\v\Notices 26-10.mp4", -2.0), cue_at("Mission", r"C:\v\Mission.mov", 0.0)];
        let lengths =
            HashMap::from([(PathBuf::from(r"C:\v\Notices 26-10.mp4"), ClipLength::Known(Duration::from_secs(150)))]);
        let rows = cue_rows(&cues, &lengths);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].number, 1);
        assert_eq!(rows[0].deck_key, 9, "row 2 of the Videos page");
        assert_eq!(rows[0].name, "Notices");
        assert_eq!(rows[0].file_name, "Notices 26-10.mp4");
        assert_eq!(rows[0].length_text, "2:30");
        assert_eq!(rows[0].destination_text, "Hall + Online");
        assert_eq!(rows[0].level_text, "−2 dB");
        assert_eq!(rows[1].level_text, "0 dB");
        assert_eq!(rows[1].length_text, "–:––", "unknown until read");
    }

    #[test]
    fn cue_sync_decisions() {
        let settle = Duration::from_secs(1);
        let s = |ms| Some(Duration::from_millis(ms));
        assert_eq!(cue_sync(true, None, true, settle, false, false), CueSync::Applying);
        assert_eq!(cue_sync(false, s(300), true, settle, false, false), CueSync::Applying);
        assert_eq!(cue_sync(false, s(300), false, settle, false, false), CueSync::Idle);
        // A video is (or was at refusal) playing: the revert is honest.
        assert_eq!(cue_sync(false, s(1500), true, settle, true, false), CueSync::Reverted);
        assert_eq!(cue_sync(false, s(1500), true, settle, true, true), CueSync::Reverted);
        // Nothing playing: resend once, then give up with the neutral notice.
        assert_eq!(cue_sync(false, s(1500), true, settle, false, false), CueSync::Resend);
        assert_eq!(cue_sync(false, s(1500), true, settle, false, true), CueSync::GaveUp);
        // Reflected after all: nothing to do, however slow.
        assert_eq!(cue_sync(false, s(1500), false, settle, false, true), CueSync::Idle);
        assert_eq!(cue_sync(false, None, true, settle, false, false), CueSync::Resync);
        assert_eq!(cue_sync(false, None, false, settle, false, false), CueSync::Idle);
    }

    #[test]
    fn unsaved_flag_and_retry() {
        assert!(!cues_unsaved_after(true, true));
        assert!(cues_unsaved_after(true, false));
        assert!(cues_unsaved_after(false, true), "a settings file that could not be backed up is not written");
        let every = Duration::from_secs(5);
        assert!(!should_retry_save(false, None, every));
        assert!(should_retry_save(true, None, every));
        assert!(!should_retry_save(true, Some(Duration::from_secs(2)), every));
        assert!(should_retry_save(true, Some(Duration::from_secs(5)), every));
    }

    #[test]
    fn selection_follows_its_video_when_a_row_above_goes() {
        assert_eq!(selection_after_remove(3, 1, 3), 2);
        assert_eq!(selection_after_remove(1, 3, 3), 1);
        assert_eq!(selection_after_remove(2, 2, 2), 1, "the selected one removed: last row");
        assert_eq!(selection_after_remove(0, 0, 0), 0);
    }

    #[test]
    fn destination_help_texts_match_spec() {
        assert_eq!(
            destination_help(Destination::HallAndOnline),
            "Hall screens and the stream both switch to the video. The stream audio fades down and comes back after."
        );
        assert_eq!(
            destination_help(Destination::HallOnly),
            "Only the hall screens switch. The stream stays on the cameras with the stream audio up."
        );
        assert_eq!(
            destination_help(Destination::OnlineOnly),
            "Only the stream switches to the video. The stream audio fades down and comes back after."
        );
        assert_eq!(
            destination_help(Destination::AudioOnly),
            "No picture changes. The video's sound plays and the stream audio fades down."
        );
    }

    fn countdown_at(secs: u64) -> Snapshot {
        Snapshot {
            stage: Stage::Countdown { remaining: Duration::from_secs(secs), public: false },
            countdown_total: Some(Duration::from_secs(300)),
            ..Snapshot::default()
        }
    }

    #[test]
    fn badge_live_public_red() {
        let s = Snapshot {
            stage: Stage::Live,
            on_air_since: Some(SystemTime::now() - Duration::from_secs(3 * 60 + 5)),
            broadcast: Some(BroadcastSummary {
                id: "b".into(),
                title: "Sunday Service".into(),
                scheduled_start: None,
                life_cycle: "live".into(),
                bound_stream_id: None,
            }),
            ..Snapshot::default()
        };
        let h = header(&s, &Config::default());
        assert_eq!(h.badge, "LIVE · PUBLIC");
        assert_eq!(h.badge_tone, Tone::Red);
        assert_eq!(h.broadcast, "Sunday Service");
        let t = h.timer.unwrap();
        assert!(t == "00:03:05" || t == "00:03:06", "{t}");

        let idle = header(&Snapshot::default(), &Config::default());
        assert_eq!((idle.badge.as_str(), idle.badge_tone, idle.timer), ("IDLE", Tone::Grey, None));
        let cd = header(&countdown_at(42), &Config::default());
        assert_eq!((cd.badge.as_str(), cd.badge_tone), ("COUNTDOWN · PRIVATE", Tone::Amber));
        let ready = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() };
        assert_eq!(header(&ready, &Config::default()).badge_tone, Tone::Green);
        let not_public = Snapshot { stage: Stage::Live, offers: vec![Offer::GoPublic], ..Snapshot::default() };
        assert_eq!(header(&not_public, &Config::default()).badge, "LIVE · PRIVATE");
    }

    #[test]
    fn steps_during_countdown_are_done_current_todo_todo() {
        use StepState::*;
        assert_eq!(steps(&countdown_at(42)), [Done, Current, Todo, Todo]);
        assert_eq!(steps(&Snapshot::default()), [Current, Todo, Todo, Todo]);
        assert_eq!(steps(&Snapshot { stage: Stage::Live, ..Snapshot::default() }), [Done, Done, Current, Todo]);
        let ending = Snapshot { stage: Stage::Ending { remaining: Duration::from_secs(4) }, ..Snapshot::default() };
        assert_eq!(steps(&ending), [Done, Done, Done, Current]);
        assert_eq!(steps(&Snapshot { stage: Stage::Ended, ..Snapshot::default() }), [Done; 4]);
    }

    #[test]
    fn screen_is_video_playing_when_playing_and_live() {
        let live = Snapshot { stage: Stage::Live, ..Snapshot::default() };
        assert_eq!(service_screen(&live), ServiceScreen::Live);
        assert_eq!(service_screen(&Snapshot { playing: Some(1), ..live }), ServiceScreen::VideoPlaying);
        assert_eq!(service_screen(&Snapshot::default()), ServiceScreen::BeforeService);
        // The countdown screen comes from the stage, not from countdown_total.
        assert_eq!(service_screen(&countdown_at(5)), ServiceScreen::Countdown);
        let lingering =
            Snapshot { stage: Stage::Live, countdown_total: Some(Duration::from_secs(300)), ..Snapshot::default() };
        assert_eq!(service_screen(&lingering), ServiceScreen::Live);
        let ending = Snapshot { stage: Stage::Ending { remaining: Duration::from_secs(4) }, ..Snapshot::default() };
        assert_eq!(service_screen(&ending), ServiceScreen::Ending);
    }

    #[test]
    fn countdown_view_shows_file_name_and_remaining() {
        let mut c = Config::default();
        c.playout.countdown_file = PathBuf::from(r"C:\Videos\Countdown – Autumn.mp4");
        let v = countdown(&countdown_at(42), &c).unwrap();
        assert_eq!(v.remaining, "0:42");
        assert_eq!(v.file_name, "Countdown – Autumn.mp4");
        assert_eq!(v.public_at_text, "0:10");
        assert_eq!(v.ndi_text, "NDI counter sending 0:42 as “StreamIt Countdown”");
        assert!((v.progress - 0.86).abs() < 0.001, "{}", v.progress);
        assert!(!v.public);
        // Only during the countdown stage, even though countdown_total lingers.
        let live =
            Snapshot { stage: Stage::Live, countdown_total: Some(Duration::from_secs(300)), ..Snapshot::default() };
        assert_eq!(countdown(&live, &c), None);
    }

    #[test]
    fn retry_public_shown_only_when_offered() {
        let c = Config::default();
        assert!(!countdown(&countdown_at(30), &c).unwrap().show_retry_public);
        let failed = Snapshot { offers: vec![Offer::GoPublic], ..countdown_at(30) };
        assert!(countdown(&failed, &c).unwrap().show_retry_public);
        let other = Snapshot { offers: vec![Offer::Recover], ..countdown_at(30) };
        assert!(!countdown(&other, &c).unwrap().show_retry_public);
    }

    #[test]
    fn equipment_summary_names_first_problem() {
        let mut s = Snapshot::default();
        for k in ALL_KINDS {
            s.device_status.insert(k, DeviceHealth::Ok);
        }
        let h = header(&s, &Config::default());
        assert_eq!((h.equipment.as_str(), h.equipment_tone), ("All equipment OK", Tone::Green));
        s.device_status.insert(DeviceKind::VideoHub, DeviceHealth::Warning("slow to answer".into()));
        let h = header(&s, &Config::default());
        assert_eq!((h.equipment.as_str(), h.equipment_tone), ("VideoHub · slow to answer", Tone::Amber));
        s.device_status.insert(DeviceKind::ScreensAtem, DeviceHealth::Warning("late".into()));
        let h = header(&s, &Config::default());
        assert_eq!((h.equipment.as_str(), h.equipment_tone), ("Screens ATEM · late (+1 more)", Tone::Amber));
    }

    #[test]
    fn equipment_summary_red_wins_over_amber() {
        let mut s = Snapshot::default();
        for k in ALL_KINDS {
            s.device_status.insert(k, DeviceHealth::Ok);
        }
        s.device_status.insert(DeviceKind::StreamAtem, DeviceHealth::Warning("slow".into()));
        s.device_status.insert(DeviceKind::StreamDeck, DeviceHealth::Down("unplugged".into()));
        s.device_status.insert(DeviceKind::Ndi, DeviceHealth::Down("stopped".into()));
        let h = header(&s, &Config::default());
        assert_eq!((h.equipment.as_str(), h.equipment_tone), ("NDI counter · stopped (+2 more)", Tone::Red));
    }

    #[test]
    fn a_video_playing_before_live_still_has_its_screen() {
        let idle_video = Snapshot { playing: Some(0), ..Snapshot::default() };
        assert_eq!(service_screen(&idle_video), ServiceScreen::VideoPlaying);
    }

    #[test]
    fn more_menu_enables_what_the_engine_accepts() {
        let idle = more_menu(&Snapshot::default());
        assert!(idle.start_now && idle.end_service);
        let prepared = more_menu(&Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() });
        assert!(prepared.start_now);
        let live = Snapshot { stage: Stage::Live, ..Snapshot::default() };
        assert!(!more_menu(&live).start_now && more_menu(&live).end_service);
        let video = Snapshot { playing: Some(1), job: Some("a video"), ..live.clone() };
        assert!(more_menu(&video).end_service, "End Service stops the video first");
        let ending = Snapshot {
            stage: Stage::Ending { remaining: Duration::from_secs(3) },
            job: Some("End Service"),
            ..live.clone()
        };
        assert!(more_menu(&ending).end_service, "a second press skips the closing hold");
        let preparing = Snapshot { job: Some("Prepare"), ..Snapshot::default() };
        assert!(!more_menu(&preparing).start_now && !more_menu(&preparing).end_service);
        let ended = more_menu(&Snapshot { stage: Stage::Ended, ..Snapshot::default() });
        assert!(!ended.start_now);
    }

    fn idle_recover() -> Snapshot {
        Snapshot { offers: vec![Offer::Recover], ..Snapshot::default() }
    }

    /// S6/I1: a restart mid-video left something on the DeckLink. Recover is the next
    /// step; Prepare (which would put the opening still on air) waits for it.
    #[test]
    fn recover_is_the_next_step_when_offered_at_idle() {
        let c = Config::default();
        let s = idle_recover();
        assert_eq!(next_step(&s), NextStep::Recover);
        let (title, text) = next_step_text(&s, &c);
        assert_eq!(title, "Recover first");
        assert!(text.contains("Recover"), "{text}");
        assert_eq!(prepare_button(&s), StepButton { enabled: false, reason: Some("Recover first") });
        // Nothing to recover: Prepare as usual.
        assert_eq!(next_step(&Snapshot::default()), NextStep::Prepare);
        assert_eq!(prepare_button(&Snapshot::default()), StepButton { enabled: true, reason: None });
        // Busy: disabled, no reason needed (the spinner says what runs).
        let busy = Snapshot { job: Some("Recover"), ..idle_recover() };
        assert!(!prepare_button(&busy).enabled);
        let prepared = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() };
        assert_eq!(next_step(&prepared), NextStep::Countdown);
        // The More menu agrees: no Start service now until Recover has run.
        let m = more_menu(&s);
        assert!(!m.start_now);
        assert_eq!(m.start_now_reason, Some("Recover first"));
        assert!(m.end_service);
    }

    /// M3: the stream stays on the opening still during the countdown.
    #[test]
    fn prepared_text_matches_the_countdown_screen() {
        let prepared = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() };
        let (title, text) = next_step_text(&prepared, &Config::default());
        assert_eq!(title, "Start the countdown");
        assert!(!text.contains("hall screens and the stream"), "{text}");
        assert!(text.contains("hall screens") && text.contains("opening still"), "{text}");
        assert!(text.contains("0:10") && text.contains("0:00"), "{text}");
        let mut online = Config::default();
        online.playout.countdown_to_stream = true;
        let (_, text) = next_step_text(&prepared, &online);
        assert!(text.contains("hall screens and online") && !text.contains("opening still"), "{text}");
    }

    #[test]
    fn the_stream_step_shows_only_when_the_countdown_goes_online() {
        let s = Snapshot::default();
        let mut c = Config::default();
        let labels = |c: &Config| step_rows(&s, c, StepGroup::Countdown).iter().map(|r| r.label).collect::<Vec<_>>();
        assert!(!labels(&c).contains(&"Stream to the countdown"));
        c.playout.countdown_to_stream = true;
        assert_eq!(labels(&c)[1], "Stream to the countdown");
    }

    /// M4: Start service now from Idle needs a second click, with the reason.
    #[test]
    fn start_now_from_idle_asks_for_a_confirm() {
        let idle = more_menu(&Snapshot::default());
        assert!(idle.start_now && idle.start_now_confirm);
        assert_eq!(idle.start_now_hint, "Prepare first is safer — the encoder isn’t streaming yet.");
        let prepared = more_menu(&Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() });
        assert!(prepared.start_now && !prepared.start_now_confirm);
        assert_eq!(prepared.start_now_hint, "Cut to the camera and go public, skipping the countdown.");
    }

    /// I2: a public countdown has nothing to retry.
    #[test]
    fn no_retry_box_once_the_countdown_is_public() {
        let c = Config::default();
        let public = Snapshot {
            stage: Stage::Countdown { remaining: Duration::from_secs(5), public: true },
            offers: vec![Offer::GoPublic],
            ..Snapshot::default()
        };
        assert!(!countdown(&public, &c).unwrap().show_retry_public);
    }

    /// I6: the mic bar says why the stream audio is down.
    #[test]
    fn mic_bar_wording() {
        let live = Snapshot { stage: Stage::Live, ..Snapshot::default() };
        assert_eq!(mic_bar(&live), MicBar { fraction: 0.72, tone: Tone::Green, text: "live" });
        let video = Snapshot { mics_ducked: true, playing: Some(0), ..live.clone() };
        assert_eq!(mic_bar(&video), MicBar { fraction: 0.04, tone: Tone::Amber, text: "down for video" });
        let left_down = Snapshot { mics_ducked: true, ..live };
        assert_eq!(mic_bar(&left_down), MicBar { fraction: 0.04, tone: Tone::Red, text: "down — press Recover" });
        let prepared = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() };
        assert_eq!(mic_bar(&prepared).text, "silent until live");
    }

    /// I5: a length the app can't read says why, never a made-up 0:30.
    #[test]
    fn clip_length_texts() {
        let dir = std::env::temp_dir().join(format!("streamit-view-{}-lengths", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing.mp4");
        let junk = dir.join("junk.mp4");
        std::fs::write(&junk, "not a movie").unwrap();
        assert_eq!(read_clip(&missing), ClipLength::Missing);
        assert_eq!(read_clip(&junk), ClipLength::Unreadable);
        assert_eq!(length_text(Some(ClipLength::Missing)), "file missing");
        assert_eq!(length_text(Some(ClipLength::Unreadable)), "length unknown");
        assert_eq!(length_text(Some(ClipLength::Known(Duration::from_secs(150)))), "2:30");
        assert_eq!(length_text(None), "–:––", "not read yet");
        let cues = [cue_at("Gone", missing.to_str().unwrap(), 0.0)];
        let lengths = HashMap::from([(missing.clone(), ClipLength::Missing)]);
        assert_eq!(cue_rows(&cues, &lengths)[0].length_text, "file missing");
    }

    /// M6: in simulation the countdown names the file the simulation plays.
    #[test]
    fn simulation_names_its_own_countdown_file() {
        let mut saved = Config::default();
        saved.playout.countdown_file = PathBuf::from(r"C:\v\New countdown.mp4");
        let mut running = Config::default();
        running.playout.countdown_file = PathBuf::from(r"C:\v\Old countdown.mp4");
        assert_eq!(countdown(&countdown_at(42), &running).unwrap().file_name, "Old countdown.mp4");
        assert_eq!(
            sim_next_time_note(&running.playout.countdown_file, &saved.playout.countdown_file),
            Some("applies next time you open simulation")
        );
        assert_eq!(sim_next_time_note(&saved.playout.countdown_file, &saved.playout.countdown_file), None);
    }

    #[test]
    fn ignored_keys_use_the_operator_words() {
        assert_eq!(flash_key_text(FlashKey::GoLive), "Start service now is not available right now");
        assert_eq!(flash_key_text(FlashKey::Recover), "Recover is not available right now");
    }

    #[test]
    fn quota_reads_plainly() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(thousands(10_000), "10,000");
        assert_eq!(
            quota_text(1234, 856, Duration::from_secs(5 * 3600 + 7 * 60 + 30)),
            "1,234 of 10,000 units used today · 856 calls · resets at midnight Pacific time, in 5 h 07 min"
        );
        assert_eq!(quota_tone(7_999), Tone::Green);
        assert_eq!(quota_tone(8_000), Tone::Amber);
        assert_eq!(quota_tone(10_000), Tone::Red);
    }

    #[test]
    fn fmt_mmss_rounds_up_partial_seconds() {
        assert_eq!(fmt_mmss(Duration::from_millis(41_200)), "0:42");
        assert_eq!(fmt_mmss(Duration::ZERO), "0:00");
        assert_eq!(fmt_mmss(Duration::from_secs(3605)), "60:05");
        assert_eq!(fmt_mmss(Duration::from_secs(42)), "0:42");
    }

    #[test]
    fn small_helpers() {
        assert_eq!(short_problem("disconnected: atem not connected"), "disconnected");
        assert_eq!(short_problem("unplugged"), "unplugged");
        assert_eq!((fmt_db(-2.0), fmt_db(0.04), fmt_db(3.0)), ("−2 dB".into(), "0 dB".into(), "+3 dB".into()));
        assert_eq!(fmt_hhmmss(Duration::from_secs(32 * 60 + 15)), "00:32:15");
        assert_eq!(fmt_mmss_floor(Duration::from_millis(72_800)), "1:12");
    }

    // ---- Settings and the Simulation tab (Task 9)

    #[test]
    fn switch_disabled_with_reason_while_live() {
        let live = Snapshot { stage: Stage::Live, ..Snapshot::default() };
        let st = switch_state(&live, Mode::Real);
        assert_eq!(
            st,
            SwitchState {
                label: "Switch to simulation",
                enabled: false,
                reason: Some("Finish or end the service first")
            }
        );
        // A job running in Idle (a video) also blocks it.
        let video = Snapshot { job: Some("a video"), ..Snapshot::default() };
        assert!(!switch_state(&video, Mode::Real).enabled);
        let prepared = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() };
        assert!(!switch_state(&prepared, Mode::Simulation).enabled);
        let idle = switch_state(&Snapshot::default(), Mode::Real);
        assert_eq!((idle.enabled, idle.reason), (true, None));
        let ended = Snapshot { stage: Stage::Ended, ..Snapshot::default() };
        assert!(switch_state(&ended, Mode::Real).enabled);
    }

    #[test]
    fn switch_label_flips_in_simulation() {
        assert_eq!(switch_state(&Snapshot::default(), Mode::Real).label, "Switch to simulation");
        assert_eq!(switch_state(&Snapshot::default(), Mode::Simulation).label, "Leave simulation");
        let live = Snapshot { stage: Stage::Live, simulation: true, ..Snapshot::default() };
        assert_eq!(switch_state(&live, Mode::Simulation).label, "Leave simulation");
    }

    #[test]
    fn went_public_is_pretend_time_at_the_speed() {
        assert_eq!(went_public_text(Some(Duration::from_secs(3)), Speed::X1), "3.0 s at 1×");
        assert_eq!(went_public_text(Some(Duration::from_millis(300)), Speed::X10), "0.3 s at 10×");
        assert_eq!(went_public_text(None, Speed::X4), "—");
        assert_eq!(mix_text(Speed::X1), "Mix takes 1 s at 1×");
        assert_eq!(mix_text(Speed::X4), "Mix takes 0.25 s at 4×");
        assert_eq!(mix_text(Speed::X10), "Mix takes 0.1 s at 10×");
    }

    #[test]
    fn simulation_tab_texts() {
        assert_eq!(FAULTS.len(), 7, "seven faults, plus Crash & restart app");
        let labels: Vec<&str> = FAULTS.iter().map(|f| fault_label(*f)).collect();
        assert_eq!(
            labels,
            [
                "Unplug VideoHub",
                "Unplug Stream ATEM",
                "Unplug Web Presenter",
                "YouTube refuses public",
                "YouTube very slow",
                "Next video won’t load",
                "Stream audio fade fails",
            ]
        );
        assert_eq!(sim_sources(3010), [1, 2, 3, 4, 5, 8, 3010]);
        assert_eq!((source_label(8, 3010), source_label(3010, 3010)), ("8".to_string(), "MP1".to_string()));
        assert_eq!(input8_text(Input8Source::Other(13), 13), "Cam 8 (router in 13)");
        assert_eq!(input8_text(Input8Source::Other(20), 13), "Router in 20");
        assert_eq!(input8_text(Input8Source::Other(0), 13), "Nothing routed");
        assert_eq!(input8_text(Input8Source::DeckLink, 13), "DeckLink");
        assert_eq!(player_text(&PlayerView::Empty), "idle");
        assert_eq!(player_text(&PlayerView::Loaded { duration: Duration::from_secs(30) }), "loaded · 0:30");
        let playing =
            PlayerView::Playing { position: Duration::from_millis(12_500), duration: Duration::from_secs(30) };
        assert_eq!(player_text(&playing), "playing 0:12 / 0:30");
        assert_eq!(yt_state_text(Some("live")), "live (public)");
        assert_eq!(yt_state_text(Some("testing")), "testing (not public)");
        assert_eq!(yt_state_text(Some("ready")), "ready");
        assert_eq!(yt_state_text(None), "no broadcast");
        assert_eq!(health_text(&Health::Good), "good");
        assert_eq!(health_text(&Health::NotReceiving), "not receiving");
        assert_eq!(encoder_text(&EncoderStatus::Streaming), "Streaming");
        assert_eq!(mic_fraction(-100.0), 0.0);
        assert_eq!(mic_fraction(6.0), 1.0);
        assert!(mic_fraction(-3.0) > 0.5 && mic_fraction(-3.0) < 1.0);
    }

    #[test]
    fn simulated_defaults_are_named_in_settings() {
        let fields = vec!["videohub.input8_output", "videohub.decklink_input"];
        assert_eq!(
            sim_default_note(&fields, "videohub.input8_output", 11),
            Some("Simulation uses 11 (simulated default)".into())
        );
        assert_eq!(sim_default_note(&fields, "videohub.fallback_normal_input", 13), None);
        assert_eq!(sim_default_note(&[], "videohub.input8_output", 11), None);
    }

    #[test]
    fn the_youtube_panel_says_what_youtube_sees() {
        let live = |h: Health, alarm: bool| Snapshot {
            stage: Stage::Live,
            stream_health: Some(h),
            stream_alarm: alarm,
            ..Snapshot::default()
        };
        let p = youtube_panel(&live(Health::Good, false));
        assert_eq!((p.tone, p.headline.as_str(), p.visibility), (Tone::Green, "Receiving", Some("Public")));
        let p = youtube_panel(&live(Health::Bad("bad".into()), false));
        assert_eq!((p.tone, p.detail.as_str()), (Tone::Amber, "YouTube says: bad"));
        assert_eq!(youtube_panel(&live(Health::NotReceiving, false)).tone, Tone::Amber, "before the alarm");
        let p = youtube_panel(&live(Health::NotReceiving, true));
        assert_eq!((p.tone, p.headline.as_str()), (Tone::Red, "NOT RECEIVING"));
        let p = youtube_panel(&live(Health::Bad("could not check: offline".into()), false));
        assert_eq!(p.headline, "Can’t check");
        let mut s = live(Health::Good, false);
        s.offers = vec![Offer::GoPublic];
        s.live = Some(LiveDetails { viewers: Some(37), chat_id: None });
        let p = youtube_panel(&s);
        assert_eq!((p.visibility, p.viewers), (Some("Not public"), Some(37)));
        let prepared = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() };
        let p = youtube_panel(&prepared);
        assert_eq!((p.tone, p.visibility), (Tone::Grey, Some("Private")));
    }

    #[test]
    fn atem_inputs_are_named_by_what_feeds_them() {
        let c = Config::default();
        let mut s = Snapshot::default();
        s.routing.labels = vec!["Camera 1 Wide".into(), "Camera 2 Pulpit".into()];
        let t = s.routing.target_mut(Target::StreamAtem);
        t.outputs = vec![5, 6, 0];
        t.dests = vec![Some(2), None, None];
        t.program = Some(1);
        t.preview = Some(3);
        assert_eq!(atem_sees(&s, &c, Target::StreamAtem), (Some("Camera 2 Pulpit".into()), Some("Input 3".into())));
        assert_eq!(atem_input_name(&s, &c, Target::StreamAtem, 2), "Input 2", "fed, but the route is not known");
        assert_eq!(atem_input_name(&s, &c, Target::StreamAtem, 3010), "Holding still");
        assert_eq!(atem_input_name(&s, &c, Target::ScreensAtem, 3010), "Source 3010");
        s.cues = vec![(cue_at("Mission update", "m.mp4", 0.0), CueState::Playing)];
        s.playing = Some(0);
        assert_eq!(atem_input_name(&s, &c, Target::StreamAtem, c.stream_atem.decklink_input), "Video: Mission update");
        assert_eq!(atem_sees(&Snapshot::default(), &c, Target::ScreensAtem), (None, None));
    }

    #[test]
    fn an_atem_inputs_own_name_comes_before_the_videohubs() {
        use streamit_core::devices::InputName;
        let c = Config::default();
        let mut s = Snapshot::default();
        s.routing.labels = vec!["Camera 1 Wide".into(), "Camera 2 Pulpit".into()];
        let t = s.routing.target_mut(Target::ScreensAtem);
        t.outputs = vec![5, 6];
        t.dests = vec![Some(1), Some(2)];
        t.names.insert(1, InputName { long: "Wide Cam".into(), short: "WIDE".into(), default: false });
        t.names.insert(2, InputName { long: "Camera 2".into(), short: "CAM2".into(), default: true });
        assert_eq!(atem_input_name(&s, &c, Target::ScreensAtem, 1), "Wide Cam");
        assert_eq!(atem_input_name(&s, &c, Target::ScreensAtem, 2), "Camera 2 Pulpit", "the ATEM's own default name");
    }

    #[test]
    fn audio_inputs_are_tagged_on_afv_or_off() {
        let at = |mix| Some(AudioInput { mix, fader_db: 0.0 });
        assert_eq!(audio_tag(at(AudioMix::On)), ("ON", Tone::Green));
        assert_eq!(audio_tag(at(AudioMix::FollowVideo)).0, "AFV");
        assert_eq!(audio_tag(at(AudioMix::Off)).0, "OFF");
        assert_eq!(audio_tag(None).0, "—");
        assert_eq!(fader_text(-100.0), "−∞");
        assert_eq!(fader_text(0.2), "0 dB");
        assert_eq!(fader_text(-3.0), "-3 dB");
        assert_eq!(studio_url("abc"), "https://studio.youtube.com/video/abc/livestreaming");
        assert_eq!(chat_url("abc"), "https://www.youtube.com/live_chat?is_popout=1&v=abc");
        assert_eq!(chat_time("not a time"), "");
        assert_eq!(chat_time("2026-10-25T10:31:00Z").len(), 5);
    }

    #[test]
    fn step_rows_show_how_far_each_step_has_got() {
        use streamit_core::steps::{Step, StepGroup, StepId, StepMark};
        let c = Config::default();
        let mut s = Snapshot::default();
        let rows = step_rows(&s, &c, StepGroup::Countdown);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.tone == Tone::Grey));
        assert_eq!(rows[2].detail.as_deref(), Some("At 0:10 left"));
        assert_eq!(steps_tone(&rows), Tone::Grey);

        s.stage = Stage::Countdown { remaining: Duration::from_secs(192), public: false };
        s.steps = vec![
            Step { id: StepId::HallCountdown, mark: StepMark::done() },
            Step { id: StepId::CountdownVideo, mark: StepMark::Now(None) },
            Step { id: StepId::YouTubePublic, mark: StepMark::Todo },
        ];
        let rows = step_rows(&s, &c, StepGroup::Countdown);
        assert_eq!(rows[0].tone, Tone::Green);
        assert_eq!(rows[0].time.as_ref().map(|t| t.len()), Some(8));
        assert_eq!((rows[1].tone, rows[1].detail.as_deref()), (Tone::Amber, Some("3:12 left")));
        assert_eq!(steps_tone(&rows), Tone::Amber);

        s.steps[1].mark = StepMark::Failed("no signal".into());
        s.steps[2].mark = StepMark::done();
        let rows = step_rows(&s, &c, StepGroup::Countdown);
        assert_eq!((rows[1].tone, rows[1].detail.as_deref()), (Tone::Red, Some("no signal")));
        assert_eq!(steps_tone(&rows), Tone::Red);
    }

    #[test]
    fn the_closing_countdown_step_shows_only_when_it_plays() {
        let mut s = Snapshot::default();
        steps::reset(&mut s.steps, StepGroup::End);
        let mut c = Config::default();
        let labels = |c: &Config| step_rows(&s, c, StepGroup::End).into_iter().map(|r| r.label).collect::<Vec<_>>();
        assert!(!labels(&c).contains(&"Closing countdown playing"), "off by default");
        c.playout.closing_on = true;
        assert_eq!(labels(&c)[0], "Closing countdown playing");
        c.playout.closing_to_hall = false;
        c.playout.closing_to_stream = false;
        assert!(!labels(&c).contains(&"Closing countdown playing"), "going nowhere");
    }

    #[test]
    fn the_ending_screen_knows_the_closing_countdown_is_playing() {
        let mut s = Snapshot::default();
        steps::reset(&mut s.steps, StepGroup::End);
        assert!(!closing_playing(&s));
        steps::mark(&mut s.steps, StepId::ClosingVideo, StepMark::Now(None));
        assert!(closing_playing(&s));
        steps::mark(&mut s.steps, StepId::ClosingVideo, StepMark::done());
        assert!(!closing_playing(&s));
    }

    #[test]
    fn with_the_countdown_off_the_next_step_is_starting_the_service() {
        let mut s = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() };
        assert_eq!(next_step_text(&s, &Config::default()).0, "Start the countdown");
        s.countdown_off = true;
        assert_eq!(next_step_text(&s, &Config::default()).0, "Start the service");
    }
}
