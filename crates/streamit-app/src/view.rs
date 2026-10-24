//! What the screens show, worked out from the snapshot and the settings. No egui here:
//! the drawing lives in `ui/`, so these choices can be tested.
use crate::session::{Mode, REFUSED};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use streamit_core::config::Config;
use streamit_core::devices::{EncoderStatus, Health};
use streamit_core::engine::{DeviceHealth, DeviceKind, FlashKey, Snapshot};
use streamit_core::golive::{Readiness, Stage};
use streamit_core::recovery::Offer;
use streamit_core::rollin::{Cue, CueState, Destination};
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

/// Every device, in the order the screens list them.
pub const ALL_KINDS: [DeviceKind; 8] = [
    DeviceKind::StreamAtem,
    DeviceKind::ScreensAtem,
    DeviceKind::VideoHub,
    DeviceKind::WebPresenter,
    DeviceKind::YouTube,
    DeviceKind::DeckLink,
    DeviceKind::Ndi,
    DeviceKind::StreamDeck,
];

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
    let problems: Vec<(DeviceKind, Tone, String)> = ALL_KINDS
        .iter()
        .filter_map(|&k| match device_state(s, k) {
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
    pub end_service: bool,
}

/// Mirrors the engine: Start service now runs from Idle or Prepared when nothing else
/// runs (the countdown has its own button); End Service starts when nothing runs, stops
/// a video first, and a second press during End Service skips the closing hold.
pub fn more_menu(s: &Snapshot) -> MoreMenu {
    MoreMenu {
        start_now: matches!(s.stage, Stage::Idle | Stage::Prepared(_)) && s.job.is_none(),
        end_service: s.playing.is_some() || matches!(s.job, None | Some("End Service") | Some("a video")),
    }
}

/// The label of the online panel while a video plays, and whether it is on air.
pub fn online_panel(s: &Snapshot) -> (&'static str, bool) {
    if s.stage == Stage::Live { ("Online · on air", true) } else { ("Online · not live yet", false) }
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
        show_retry_public: public_failed(s),
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

/// The Stream Deck key of video `number` (1-based): keys 17–23 for 1–7, then 25–31 for
/// 8–14, because key 24 is Stop video.
pub fn deck_key(number: usize) -> u8 {
    let n = number as u8;
    if number <= 7 { 16 + n } else { 17 + n }
}

/// `M:SS` of a clip, or "–:––" while its length is not known.
pub fn length_text(d: Option<Duration>) -> String {
    d.map(fmt_mmss).unwrap_or_else(|| LENGTH_UNKNOWN.into())
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
pub fn cue_row(index: usize, cue: &Cue, lengths: &HashMap<PathBuf, Duration>) -> CueRow {
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
pub fn cue_rows(cues: &[Cue], lengths: &HashMap<PathBuf, Duration>) -> Vec<CueRow> {
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
            "Hall screens and the stream both switch to the video. Stream mics fade down and come back after."
        }
        Destination::HallOnly => "Only the hall screens switch. The stream stays on the cameras with the mics up.",
        Destination::OnlineOnly => "Only the stream switches to the video. Stream mics fade down and come back after.",
        Destination::AudioOnly => "No picture changes. The video's sound plays and the stream mics fade down.",
    }
}

/// The first video not yet played: its Play button is the blue one.
pub fn next_cue(s: &Snapshot) -> Option<usize> {
    s.cues.iter().position(|(_, st)| *st == CueState::Cued)
}

/// `HH:MM:SS`, whole seconds, for the on-air timer.
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
pub fn switch_state(s: &Snapshot, mode: Mode) -> SwitchState {
    let label = match mode {
        Mode::Real => "Switch to simulation",
        Mode::Simulation => "Leave simulation",
    };
    let enabled = matches!(s.stage, Stage::Idle | Stage::Ended) && s.job.is_none();
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
        Fault::MicFadeFails => "Mic fade fails",
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

/// How full the mic bar is: −40 dB (or off) empty, +6 dB full.
pub fn mic_fraction(db: f32) -> f32 {
    ((db + 40.0) / 46.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamit_core::devices::BroadcastSummary;

    fn cue_at(name: &str, path: &str, gain: f32) -> streamit_core::rollin::Cue {
        streamit_core::rollin::Cue {
            path: PathBuf::from(path),
            name: name.into(),
            destination: Destination::HallAndOnline,
            gain_db: gain,
            return_input: None,
        }
    }

    #[test]
    fn deck_keys_skip_the_stop_video_key() {
        assert_eq!(deck_key(1), 17);
        assert_eq!(deck_key(7), 23);
        assert_eq!(deck_key(8), 25);
        assert_eq!(deck_key(14), 31);
    }

    #[test]
    fn cue_row_texts() {
        let cues = [cue_at("Notices", r"C:\v\Notices 26-10.mp4", -2.0), cue_at("Mission", r"C:\v\Mission.mov", 0.0)];
        let lengths = HashMap::from([(PathBuf::from(r"C:\v\Notices 26-10.mp4"), Duration::from_secs(150))]);
        let rows = cue_rows(&cues, &lengths);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].number, 1);
        assert_eq!(rows[0].deck_key, 17);
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
            "Hall screens and the stream both switch to the video. Stream mics fade down and come back after."
        );
        assert_eq!(
            destination_help(Destination::HallOnly),
            "Only the hall screens switch. The stream stays on the cameras with the mics up."
        );
        assert_eq!(
            destination_help(Destination::OnlineOnly),
            "Only the stream switches to the video. Stream mics fade down and come back after."
        );
        assert_eq!(
            destination_help(Destination::AudioOnly),
            "No picture changes. The video's sound plays and the stream mics fade down."
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
    fn online_panel_is_on_air_only_when_live() {
        let idle_video = Snapshot { playing: Some(0), ..Snapshot::default() };
        assert_eq!(service_screen(&idle_video), ServiceScreen::VideoPlaying);
        assert_eq!(online_panel(&idle_video), ("Online · not live yet", false));
        let live_video = Snapshot { stage: Stage::Live, playing: Some(0), ..Snapshot::default() };
        assert_eq!(online_panel(&live_video), ("Online · on air", true));
        let prepared = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..idle_video };
        assert!(!online_panel(&prepared).1);
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

    #[test]
    fn ignored_keys_use_the_operator_words() {
        assert_eq!(flash_key_text(FlashKey::GoLive), "Start service now is not available right now");
        assert_eq!(flash_key_text(FlashKey::Recover), "Recover is not available right now");
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
                "Mic fade fails",
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
}
