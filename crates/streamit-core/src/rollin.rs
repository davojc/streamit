//! Cue list and the roll-in sequencer.
//!
//! Stream ATEM input 8 is shared by Camera 8 and the DeckLink through the VideoHub,
//! and the VideoHub shows about a second of black when it switches. So the route is
//! only ever changed while input 8 is off Stream program: it is switched to the
//! DeckLink before the ATEM cuts to input 8, and put back only once input 8 has left
//! program again.
//!
//! Device errors once the roll-in has started never stop the return path; they are
//! collected and the outcome becomes `Failed { recover_needed: true, .. }`.
use crate::audio::{LiveLevels, Target, all_down, down_threshold, fade};
use crate::config::{AudioSourceId, Config};
use crate::devices::{Devices, PlayerEvent};
use crate::golive::{COUNTER_TICK, cut};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{MissedTickBehavior, interval, sleep};
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Most videos the cue list holds in v1.
pub const MAX_CUES: usize = 14;
/// Fairlight source id of an input's stereo pair.
pub(crate) const STEREO_SOURCE: i64 = -65280;
/// Longest wait for the return transition off input 8.
pub const TRANSITION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Destination {
    HallAndOnline,
    HallOnly,
    OnlineOnly,
    AudioOnly,
}

impl Destination {
    /// Needs the Stream ATEM (and so the VideoHub route for input 8).
    fn uses_stream(self) -> bool {
        !matches!(self, Destination::HallOnly)
    }
    /// Puts the video on the Stream ATEM picture.
    fn stream_picture(self) -> bool {
        matches!(self, Destination::HallAndOnline | Destination::OnlineOnly)
    }
    /// Puts the video on the hall screens.
    pub fn screens_picture(self) -> bool {
        matches!(self, Destination::HallAndOnline | Destination::HallOnly)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub path: PathBuf,
    pub name: String,
    pub destination: Destination,
    /// Gain trim applied by the player, in dB.
    #[serde(default)]
    pub gain_db: f32,
    /// Stream ATEM input to go back to after the video; `None` means "whatever is on Preview".
    #[serde(default)]
    pub return_input: Option<u16>,
    /// The scene (its id) to go to while the video plays; StreamIt goes back to the scene
    /// before it when the video ends.
    #[serde(default)]
    pub scene: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CueState {
    Cued,
    Playing,
    Played,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the cue list is full ({MAX_CUES} videos)")]
pub struct CueListFull;

/// Up to `MAX_CUES` videos in the order they were added, each with its state.
/// `state[i]` belongs to the cue at `i` and moves with it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CueList {
    cues: Vec<Cue>,
    pub state: Vec<CueState>,
}

impl CueList {
    /// Build from saved cues (e.g. `Config.cues`), all `Cued`. Cues past `MAX_CUES` are dropped.
    pub fn from_cues(mut cues: Vec<Cue>) -> CueList {
        cues.truncate(MAX_CUES);
        let state = vec![CueState::Cued; cues.len()];
        CueList { cues, state }
    }

    /// The cues in order, for saving into `Config.cues`.
    pub fn cues(&self) -> &[Cue] {
        &self.cues
    }

    pub fn len(&self) -> usize {
        self.cues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cues.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<&Cue> {
        self.cues.get(i)
    }

    pub fn push(&mut self, cue: Cue) -> Result<(), CueListFull> {
        if self.cues.len() >= MAX_CUES {
            return Err(CueListFull);
        }
        self.sync_state();
        self.cues.push(cue);
        self.state.push(CueState::Cued);
        Ok(())
    }

    pub fn remove(&mut self, i: usize) -> Option<Cue> {
        if i >= self.cues.len() {
            return None;
        }
        self.sync_state();
        self.state.remove(i);
        Some(self.cues.remove(i))
    }

    /// Swap cue `i` with the one before it. `false` if there is none.
    pub fn move_up(&mut self, i: usize) -> bool {
        if i == 0 || i >= self.cues.len() {
            return false;
        }
        self.swap(i - 1, i);
        true
    }

    /// Swap cue `i` with the one after it. `false` if there is none.
    pub fn move_down(&mut self, i: usize) -> bool {
        if i + 1 >= self.cues.len() {
            return false;
        }
        self.swap(i, i + 1);
        true
    }

    fn swap(&mut self, a: usize, b: usize) {
        self.sync_state();
        self.cues.swap(a, b);
        self.state.swap(a, b);
    }

    /// `state` is public, so keep it the same length as `cues` before indexing it.
    fn sync_state(&mut self) {
        self.state.resize(self.cues.len(), CueState::Cued);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollinOutcome {
    Finished,
    Stopped,
    Refused(String),
    Failed { recover_needed: bool, reason: String },
}

/// What `play_reporting` reports while a roll-in runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollinReport {
    /// `true` as the mic fade down starts; `false` only once a fade back up has succeeded.
    MicsDucked(bool),
    /// The video is playing: how far in it is, out of its length. Sent as it starts
    /// and every `COUNTER_TICK` until it ends or is stopped.
    Position { position: Duration, duration: Duration },
}

/// Why the video stopped running.
enum End {
    Ended,
    Stopped,
    VideoFailed(String),
    NotPlayed,
}

fn note(problems: &mut Vec<String>, msg: String) {
    problems.push(msg);
}

/// Consume an Unmute press left over from before this video, so it cannot
/// unmute the mics the moment this one starts.
fn drain_unmute(unmute: &Notify) {
    let pending = unmute.notified();
    tokio::pin!(pending);
    pending.as_mut().enable();
}

/// Refuse after a successful load: stop the player so the DeckLink is not left prerolled.
async fn refuse(d: &Devices, why: &str) -> RollinOutcome {
    let _ = d.player.stop().await;
    RollinOutcome::Refused(why.to_string())
}

/// Put input 8's VideoHub route back to `prev`, but only once Stream program is
/// known not to be input 8. A skipped restore is a problem.
async fn restore_route(d: &Devices, c: &Config, prev: u16, problems: &mut Vec<String>) {
    let out = c.videohub.input8_output;
    match d.stream.program().await {
        Ok(p) if p != c.stream_atem.decklink_input => {
            if let Err(e) = d.hub.route(out, prev).await {
                note(problems, format!("Could not restore the VideoHub route for input 8: {e}"));
            }
        }
        Ok(_) => note(problems, "Input 8 is still on air, so its VideoHub route was not restored".into()),
        Err(e) => note(
            problems,
            format!("Could not read the Stream ATEM program, so the VideoHub route for input 8 was not restored: {e}"),
        ),
    }
}

/// Play one roll-in. See the module docs for the safety rule. Order:
/// load; (Stream destinations) check, record and switch the route, settle, capture
/// mic levels; switch pictures and audio; play; wait for the end, Stop or Unmute;
/// then put everything back, the route last.
///
/// Playback starts as soon as the pictures are switched; the mic fade-down runs
/// alongside it, so the video does not sit paused on air for the length of the fade.
pub async fn play(
    d: &Devices,
    c: &Config,
    cue: &Cue,
    levels: &mut LiveLevels,
    stop: CancellationToken,
    unmute: Arc<Notify>,
) -> RollinOutcome {
    play_reporting(d, c, cue, levels, stop, unmute, |_| {}).await
}

/// `play`, also reporting the mics and the video's position (see `RollinReport`).
/// While the video plays the NDI counter shows the time left every `COUNTER_TICK`; it is
/// cleared at the start of the return path, however the video ended.
pub async fn play_reporting(
    d: &Devices,
    c: &Config,
    cue: &Cue,
    levels: &mut LiveLevels,
    stop: CancellationToken,
    unmute: Arc<Notify>,
    report: impl Fn(RollinReport),
) -> RollinOutcome {
    let mics_ducked = |down: bool| report(RollinReport::MicsDucked(down));
    let sa = &c.stream_atem;
    let hub = &c.videohub;
    let dest = cue.destination;
    let in8 = AudioSourceId { input: sa.decklink_input, source: STEREO_SOURCE };
    drain_unmute(&unmute);
    // Subscribe before playing so `Ended` cannot be missed.
    let mut events = d.player.events();

    // 1. Load, paused on the first frame.
    let duration = match d.player.load(&cue.path, cue.gain_db).await {
        Ok(duration) => duration,
        Err(e) => {
            return RollinOutcome::Failed {
                recover_needed: false,
                reason: format!("Could not load {}: {e}", cue.name),
            };
        }
    };

    // 2. Route input 8 to the DeckLink while it is off program.
    let mut problems = Vec::new();
    let mut prev = None;
    // R24: the mics were already down (the holding still) before the video.
    let mut held_down = false;
    if dest.uses_stream() {
        if hub.input8_output == 0 || hub.decklink_input == 0 {
            return refuse(d, "VideoHub not configured").await;
        }
        match d.stream.program().await {
            Ok(p) if p == sa.decklink_input => return refuse(d, "Input 8 is on air").await,
            Ok(_) => {}
            Err(e) => return refuse(d, &format!("Could not read the Stream ATEM program: {e}")).await,
        }
        let p = match d.hub.route_of(hub.input8_output).await {
            Ok(p) => p,
            Err(e) => return refuse(d, &format!("Could not read the VideoHub route for input 8: {e}")).await,
        };
        prev = Some(p);
        if let Err(e) = d.hub.route(hub.input8_output, hub.decklink_input).await {
            // Input 8 may now carry anything: switch nothing, put the route back.
            note(&mut problems, format!("Could not route the DeckLink to input 8: {e}"));
            if let Err(e) = d.player.stop().await {
                note(&mut problems, format!("Could not stop the video: {e}"));
            }
            restore_route(d, c, p, &mut problems).await;
            return RollinOutcome::Failed { recover_needed: true, reason: problems.join("; ") };
        }
        sleep(hub.settle).await;
        let down = down_threshold(sa.duck_db);
        if let Err(e) = levels.recapture(&*d.stream, &sa.mic_sources, down).await {
            note(&mut problems, format!("Could not read the stream audio levels: {e}"));
        }
        held_down = all_down(&*d.stream, &sa.mic_sources, down).await;
    }

    // Stop pressed during load or settle: nothing has gone on air, so change no
    // picture or audio; just un-preroll the player and put the route back.
    if stop.is_cancelled() {
        if let Err(e) = d.player.stop().await {
            note(&mut problems, format!("Could not stop the video: {e}"));
        }
        if let Some(p) = prev {
            restore_route(d, c, p, &mut problems).await;
        }
        return if problems.is_empty() {
            RollinOutcome::Stopped
        } else {
            RollinOutcome::Failed { recover_needed: true, reason: problems.join("; ") }
        };
    }

    // 3. Pictures and audio.
    let mut stream_cut = false;
    if dest.stream_picture() {
        match cut(&*d.stream, sa.decklink_input).await {
            Ok(()) => stream_cut = true,
            Err(e) => note(&mut problems, format!("Could not switch the stream to the video: {e}")),
        }
    }
    // The video's sound goes to the stream; the mics are ducked under it, unless they
    // were already down (R24: then they are left exactly as they were, before and after).
    let video_audio = dest.uses_stream();
    let mics_down = video_audio && !held_down;
    if video_audio {
        if let Err(e) = d.stream.set_audio_on(in8, true).await {
            note(&mut problems, format!("Could not turn on input 8 audio: {e}"));
        }
        if let Err(e) = d.stream.set_audio_level(in8, 0.0).await {
            note(&mut problems, format!("Could not set the input 8 audio level: {e}"));
        }
    }
    // 4. Play, with the mic fade-down running alongside.
    if mics_down {
        mics_ducked(true);
    }
    let fade_down = async {
        // R21: roll-ins duck to duck_db (the floor when duck_db is at or below it).
        if mics_down { fade(&*d.stream, &sa.mic_sources, Target::duck(sa.duck_db), sa.fade).await } else { Ok(()) }
    };
    let screens_and_play = async {
        let screens =
            if dest.screens_picture() { Some(cut(&*d.screens, c.screens_atem.decklink_input).await) } else { None };
        (screens, d.player.play().await)
    };
    let (faded, (screens, played)) = tokio::join!(fade_down, screens_and_play);
    if let Err(e) = faded {
        note(&mut problems, format!("Could not fade the stream audio down: {e}"));
    }
    let screens_cut = matches!(screens, Some(Ok(())));
    if let Some(Err(e)) = screens {
        note(&mut problems, format!("Could not switch the hall screens to the video: {e}"));
    }

    // 5. Wait for the end, Stop, or Unmute.
    let mut unmuted = false;
    let end = if let Err(e) = played {
        note(&mut problems, format!("Could not play {}: {e}", cue.name));
        End::NotPlayed
    } else {
        let mut counter_warned = false;
        let mut tick = interval(COUNTER_TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break End::Stopped,
                ev = events.recv() => match ev {
                    Ok(PlayerEvent::Ended) => break End::Ended,
                    Ok(PlayerEvent::Error(e)) => break End::VideoFailed(e),
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break End::VideoFailed("the player went away".into()),
                },
                _ = unmute.notified(), if mics_down && !unmuted => {
                    // Only a fade that worked counts: otherwise Unmute stays armed
                    // and the return path still fades the stream audio up.
                    match fade(&*d.stream, &sa.mic_sources, Target::Live(levels), sa.fade).await {
                        Ok(()) => {
                            unmuted = true;
                            mics_ducked(false);
                        }
                        Err(e) => note(&mut problems, format!("Could not fade the stream audio up: {e}")),
                    }
                }
                _ = tick.tick() => {
                    let position = d.player.position().unwrap_or_default().min(duration);
                    // The NDI counter is a convenience: a failure is logged once, never a problem.
                    if let Err(e) = d.counter.show(duration - position).await
                        && !counter_warned
                    {
                        counter_warned = true;
                        warn!(error = %e, "could not update the NDI counter");
                    }
                    report(RollinReport::Position { position, duration });
                }
            }
        }
    };

    // 6. Return path. Every step runs whatever failed before it. The counter goes first.
    if let Err(e) = d.counter.clear().await {
        warn!(error = %e, "could not clear the NDI counter");
    }
    // R10: when the video was cut short, its sound goes first so it never lingers online.
    let cut_short = !matches!(end, End::Ended);
    let audio_off_first = video_audio && cut_short;
    if audio_off_first && let Err(e) = d.stream.set_audio_on(in8, false).await {
        note(&mut problems, format!("Could not turn off input 8 audio: {e}"));
    }
    // R11: the hall leaves the DeckLink first, before the stream's return transition.
    // I3: a real ATEM reports the old program until a mix completes, so let the forward
    // transition land before reading where each switcher is.
    if dest.screens_picture() {
        if let Err(e) = d.screens.wait_transition_done(TRANSITION_TIMEOUT).await {
            note(&mut problems, format!("The hall screens transition to the video did not finish: {e}"));
        }
        let on_video = match d.screens.program().await {
            Ok(p) => p == c.screens_atem.decklink_input,
            Err(e) => {
                note(&mut problems, format!("Could not read the hall screens program: {e}"));
                screens_cut
            }
        };
        // After the cut to the video, Preview holds what was on before; one Auto returns to it.
        if on_video && let Err(e) = d.screens.auto().await {
            note(&mut problems, format!("Could not switch the hall screens off the video: {e}"));
        }
    }
    if dest.stream_picture() {
        if let Err(e) = d.stream.wait_transition_done(TRANSITION_TIMEOUT).await {
            note(&mut problems, format!("The stream transition to the video did not finish: {e}"));
        }
        let on8 = match d.stream.program().await {
            Ok(p) => p == sa.decklink_input,
            Err(e) => {
                note(&mut problems, format!("Could not read the Stream ATEM program: {e}"));
                stream_cut
            }
        };
        if on8 {
            if let Some(i) = cue.return_input
                && let Err(e) = d.stream.set_preview(i).await
            {
                note(&mut problems, format!("Could not set the stream preview to input {i}: {e}"));
            }
            match d.stream.auto().await {
                Ok(()) => {
                    if let Err(e) = d.stream.wait_transition_done(TRANSITION_TIMEOUT).await {
                        note(&mut problems, format!("The stream transition off the video did not finish: {e}"));
                    }
                }
                Err(e) => note(&mut problems, format!("Could not switch the stream off the video: {e}")),
            }
        }
    }
    if mics_down && !unmuted {
        match fade(&*d.stream, &sa.mic_sources, Target::Live(levels), sa.fade).await {
            Ok(()) => mics_ducked(false),
            Err(e) => note(&mut problems, format!("Could not fade the stream audio up: {e}")),
        }
    }
    if video_audio
        && !audio_off_first
        && let Err(e) = d.stream.set_audio_on(in8, false).await
    {
        note(&mut problems, format!("Could not turn off input 8 audio: {e}"));
    }
    if cut_short && let Err(e) = d.player.stop().await {
        note(&mut problems, format!("Could not stop the video: {e}"));
    }
    if let Some(p) = prev {
        restore_route(d, c, p, &mut problems).await;
    }

    let reason = problems.join("; ");
    match end {
        End::VideoFailed(e) => RollinOutcome::Failed {
            recover_needed: !problems.is_empty(),
            reason: if problems.is_empty() {
                format!("The video failed: {e}")
            } else {
                format!("The video failed: {e}; {reason}")
            },
        },
        _ if !problems.is_empty() => RollinOutcome::Failed { recover_needed: true, reason },
        End::Ended => RollinOutcome::Finished,
        End::Stopped => RollinOutcome::Stopped,
        End::NotPlayed => RollinOutcome::Failed { recover_needed: true, reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FLOOR_DB;
    use crate::config::AudioSourceId;
    use crate::devices::{PlayerEvent, Router, StreamSwitcher, Switcher};
    use crate::fakes::{Call, CallLog, FakeHandles, fake_devices};
    use std::time::Duration;
    use tokio::task::JoinHandle;
    use tokio::time::sleep;

    const OUT: u16 = 20;
    const DK: u16 = 30;
    const CAM8: u16 = 5;
    const MIC: AudioSourceId = AudioSourceId { input: 1001, source: -65280 };
    const IN8: AudioSourceId = AudioSourceId { input: 8, source: -65280 };
    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn cfg() -> Config {
        let mut c = Config::default();
        c.videohub.input8_output = OUT;
        c.videohub.decklink_input = DK;
        c
    }

    fn cue(destination: Destination) -> Cue {
        Cue {
            path: PathBuf::from("hymn.mp4"),
            name: "Hymn".into(),
            destination,
            gain_db: -2.0,
            return_input: None,
            scene: None,
        }
    }

    /// Fakes as they stand in a service: Cam 8 routed to input 8, mics live at -3 dB.
    fn rig() -> (Devices, CallLog, FakeHandles) {
        let (d, log, h) = fake_devices();
        h.hub.set_route(OUT, CAM8);
        h.stream.set_audio_level_state(MIC, -3.0);
        (d, log, h)
    }

    struct Run {
        task: JoinHandle<(RollinOutcome, LiveLevels)>,
        stop: CancellationToken,
        unmute: Arc<Notify>,
    }

    fn spawn_play(d: &Devices, c: &Config, cue: Cue) -> Run {
        let stop = CancellationToken::new();
        let unmute = Arc::new(Notify::new());
        let (d, c, s, u) = (d.clone(), c.clone(), stop.clone(), unmute.clone());
        let task = tokio::spawn(async move {
            let mut levels = LiveLevels::default();
            let r = play(&d, &c, &cue, &mut levels, s, u).await;
            (r, levels)
        });
        Run { task, stop, unmute }
    }

    async fn until(log: &CallLog, call: Call) {
        for _ in 0..2000 {
            if log.calls().contains(&call) {
                return;
            }
            sleep(MS(10)).await;
        }
        panic!("{call:?} never happened: {:?}", log.calls());
    }

    /// Play `cue` until the player is playing, then let it end.
    async fn play_to_end(d: &Devices, c: &Config, cue: Cue, log: &CallLog, h: &FakeHandles) -> RollinOutcome {
        let run = spawn_play(d, c, cue);
        until(log, Call::PlayerPlay).await;
        h.player.emit(PlayerEvent::Ended);
        run.task.await.unwrap().0
    }

    fn pos(calls: &[Call], c: &Call) -> usize {
        calls.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c:?} not in {calls:?}"))
    }

    /// `want` appears in `calls` as a subsequence, in order.
    fn assert_in_order(calls: &[Call], want: &[Call]) {
        let mut it = calls.iter();
        for w in want {
            assert!(it.any(|c| c == w), "{w:?} missing or out of order in {calls:?}");
        }
    }

    fn after_play(calls: &[Call]) -> Vec<Call> {
        calls[pos(calls, &Call::PlayerPlay) + 1..].to_vec()
    }

    fn is_stream(c: &Call) -> bool {
        matches!(
            c,
            Call::StreamSetPreview(_)
                | Call::StreamSetProgram(_)
                | Call::StreamAuto
                | Call::StreamSetMediaStill(..)
                | Call::StreamSetAudioLevel(..)
                | Call::StreamSetAudioOn(..)
        )
    }

    fn is_screens(c: &Call) -> bool {
        matches!(c, Call::ScreensSetPreview(_) | Call::ScreensSetProgram(_) | Call::ScreensAuto)
    }

    #[tokio::test(start_paused = true)]
    async fn hall_and_online_routes_before_switching_and_restores_last() {
        let (d, log, h) = rig();
        let r = play_to_end(&d, &cfg(), cue(Destination::HallAndOnline), &log, &h).await;
        assert_eq!(r, RollinOutcome::Finished);
        let calls = log.calls();
        assert_eq!(calls[0], Call::PlayerLoad(PathBuf::from("hymn.mp4"), -2.0));
        assert_in_order(
            &calls,
            &[
                Call::HubRoute { output: OUT, input: DK },
                Call::StreamSetPreview(8),
                Call::StreamAuto,
                Call::StreamSetAudioOn(IN8, true),
                Call::StreamSetAudioLevel(IN8, 0.0),
                Call::ScreensSetPreview(8),
                Call::ScreensAuto,
                Call::PlayerPlay,
            ],
        );
        assert!(calls.contains(&Call::StreamSetAudioLevel(MIC, FLOOR_DB)));
        let tail = after_play(&calls);
        assert_in_order(
            &tail,
            &[
                Call::ScreensAuto,
                Call::StreamAuto,
                Call::StreamSetAudioLevel(MIC, -3.0),
                Call::StreamSetAudioOn(IN8, false),
                Call::HubRoute { output: OUT, input: CAM8 },
            ],
        );
        assert_eq!(calls.last(), Some(&Call::HubRoute { output: OUT, input: CAM8 }));
        assert!(!calls.contains(&Call::PlayerStop));
        assert_eq!(h.stream.program().await.unwrap(), 1);
        assert_eq!(h.screens.program().await.unwrap(), 1);
        assert_eq!(h.hub.route_of(OUT).await.unwrap(), CAM8);
    }

    #[tokio::test(start_paused = true)]
    async fn hall_and_online_waits_settle_before_transition() {
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        sleep(MS(1499)).await;
        let calls = log.calls();
        assert!(calls.contains(&Call::HubRoute { output: OUT, input: DK }), "{calls:?}");
        assert!(!calls.contains(&Call::StreamSetPreview(8)), "{calls:?}");
        assert!(!calls.contains(&Call::StreamAuto), "{calls:?}");
        sleep(MS(2)).await;
        assert!(log.calls().contains(&Call::StreamAuto), "{:?}", log.calls());
        until(&log, Call::PlayerPlay).await;
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().0, RollinOutcome::Finished);
    }

    #[tokio::test(start_paused = true)]
    async fn hall_only_touches_screens_only() {
        let (d, log, h) = rig();
        let r = play_to_end(&d, &cfg(), cue(Destination::HallOnly), &log, &h).await;
        assert_eq!(r, RollinOutcome::Finished);
        let calls = log.calls();
        assert!(!calls.iter().any(|c| is_stream(c) || matches!(c, Call::HubRoute { .. })), "{calls:?}");
        assert_in_order(&calls, &[Call::ScreensSetPreview(8), Call::ScreensAuto, Call::PlayerPlay, Call::ScreensAuto]);
        assert_eq!(h.screens.program().await.unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn online_only_leaves_screens() {
        let (d, log, h) = rig();
        let r = play_to_end(&d, &cfg(), cue(Destination::OnlineOnly), &log, &h).await;
        assert_eq!(r, RollinOutcome::Finished);
        let calls = log.calls();
        assert!(!calls.iter().any(is_screens), "{calls:?}");
        assert_in_order(
            &calls,
            &[
                Call::HubRoute { output: OUT, input: DK },
                Call::StreamSetPreview(8),
                Call::StreamAuto,
                Call::StreamSetAudioOn(IN8, true),
                Call::PlayerPlay,
                Call::StreamAuto,
                Call::StreamSetAudioOn(IN8, false),
                Call::HubRoute { output: OUT, input: CAM8 },
            ],
        );
        assert_eq!(h.stream.program().await.unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn audio_only_routes_and_fades_but_keeps_picture() {
        let (d, log, h) = rig();
        let r = play_to_end(&d, &cfg(), cue(Destination::AudioOnly), &log, &h).await;
        assert_eq!(r, RollinOutcome::Finished);
        let calls = log.calls();
        assert!(!calls.iter().any(is_screens), "{calls:?}");
        assert!(!calls.iter().any(|c| matches!(c, Call::StreamSetPreview(_) | Call::StreamAuto)), "{calls:?}");
        assert_in_order(
            &calls,
            &[
                Call::HubRoute { output: OUT, input: DK },
                Call::StreamSetAudioOn(IN8, true),
                Call::StreamSetAudioLevel(MIC, FLOOR_DB),
                Call::StreamSetAudioLevel(MIC, -3.0),
                Call::StreamSetAudioOn(IN8, false),
                Call::HubRoute { output: OUT, input: CAM8 },
            ],
        );
        assert_eq!(calls.last(), Some(&Call::HubRoute { output: OUT, input: CAM8 }));
        assert_eq!(h.stream.program().await.unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn refuses_when_input8_on_program() {
        let (d, log, h) = rig();
        h.stream.set_program_state(8);
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        let (r, _) = run.task.await.unwrap();
        assert_eq!(r, RollinOutcome::Refused("Input 8 is on air".into()));
        // Only the load, and the stop that un-prerolls the DeckLink.
        assert_eq!(log.calls(), vec![Call::PlayerLoad(PathBuf::from("hymn.mp4"), -2.0), Call::PlayerStop]);
    }

    #[tokio::test(start_paused = true)]
    async fn refuses_when_videohub_unset() {
        let (d, log, _h) = rig();
        let run = spawn_play(&d, &Config::default(), cue(Destination::OnlineOnly));
        let (r, _) = run.task.await.unwrap();
        assert_eq!(r, RollinOutcome::Refused("VideoHub not configured".into()));
        assert_eq!(log.calls(), vec![Call::PlayerLoad(PathBuf::from("hymn.mp4"), -2.0), Call::PlayerStop]);
    }

    #[tokio::test(start_paused = true)]
    async fn returns_to_preview_set_by_operator_during_video() {
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::PlayerPlay).await;
        d.stream.set_preview(3).await.unwrap(); // another operator, mid-video
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().0, RollinOutcome::Finished);
        assert_eq!(h.stream.program().await.unwrap(), 3);
        assert_eq!(h.hub.route_of(OUT).await.unwrap(), CAM8);
    }

    #[tokio::test(start_paused = true)]
    async fn return_input_overrides_preview() {
        let (d, log, h) = rig();
        let mut q = cue(Destination::HallAndOnline);
        q.return_input = Some(2);
        let r = play_to_end(&d, &cfg(), q, &log, &h).await;
        assert_eq!(r, RollinOutcome::Finished);
        let tail = after_play(&log.calls());
        assert_in_order(&tail, &[Call::StreamSetPreview(2), Call::StreamAuto]);
        assert_eq!(h.stream.program().await.unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_runs_same_return_path() {
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::PlayerPlay).await;
        run.stop.cancel();
        let (r, _) = run.task.await.unwrap();
        assert_eq!(r, RollinOutcome::Stopped);
        let calls = log.calls();
        assert_in_order(
            &after_play(&calls),
            &[
                Call::StreamSetAudioOn(IN8, false),
                Call::ScreensAuto,
                Call::StreamAuto,
                Call::StreamSetAudioLevel(MIC, -3.0),
                Call::PlayerStop,
                Call::HubRoute { output: OUT, input: CAM8 },
            ],
        );
        assert_eq!(calls.last(), Some(&Call::HubRoute { output: OUT, input: CAM8 }));
        assert_eq!(h.stream.program().await.unwrap(), 1);
        assert_eq!(h.screens.program().await.unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn unmute_fades_mics_up_and_video_continues() {
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::PlayerPlay).await;
        until(&log, Call::StreamSetAudioLevel(MIC, FLOOR_DB)).await;
        run.unmute.notify_one();
        until(&log, Call::StreamSetAudioLevel(MIC, -3.0)).await;
        sleep(Duration::from_secs(5)).await;
        assert!(!run.task.is_finished());
        let calls = log.calls();
        assert!(!calls.contains(&Call::PlayerStop), "{calls:?}");
        assert!(!calls.contains(&Call::StreamSetAudioOn(IN8, false)), "{calls:?}");
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().0, RollinOutcome::Finished);
        // The return path does not fade the mics a second time.
        let ups = log.calls().iter().filter(|c| **c == Call::StreamSetAudioLevel(MIC, -3.0)).count();
        assert_eq!(ups, 1);
        assert_eq!(log.calls().last(), Some(&Call::HubRoute { output: OUT, input: CAM8 }));
    }

    #[tokio::test(start_paused = true)]
    async fn load_failure_switches_nothing() {
        let (d, log, h) = rig();
        h.player.fail_next("load");
        let (r, _) = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline)).task.await.unwrap();
        assert!(matches!(r, RollinOutcome::Failed { recover_needed: false, .. }), "{r:?}");
        assert_eq!(log.calls(), vec![Call::PlayerLoad(PathBuf::from("hymn.mp4"), -2.0)]);
    }

    #[tokio::test(start_paused = true)]
    async fn device_error_mid_rollin_lights_recover() {
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::PlayerPlay).await;
        h.hub.fail_next("route");
        h.player.emit(PlayerEvent::Ended);
        let (r, _) = run.task.await.unwrap();
        assert!(matches!(r, RollinOutcome::Failed { recover_needed: true, .. }), "{r:?}");
        let tail = after_play(&log.calls());
        assert!(tail.contains(&Call::StreamSetAudioOn(IN8, false)), "{tail:?}");
        assert!(tail.contains(&Call::StreamSetAudioLevel(MIC, -3.0)), "{tail:?}");
        assert!(tail.contains(&Call::ScreensAuto), "{tail:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn route_not_restored_while_input8_still_on_program() {
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::PlayerPlay).await;
        h.stream.fail_next("auto");
        h.player.emit(PlayerEvent::Ended);
        let (r, _) = run.task.await.unwrap();
        assert!(matches!(r, RollinOutcome::Failed { recover_needed: true, .. }), "{r:?}");
        assert!(!log.calls().contains(&Call::HubRoute { output: OUT, input: CAM8 }), "{:?}", log.calls());
        assert_eq!(h.hub.route_of(OUT).await.unwrap(), DK);
    }

    #[tokio::test(start_paused = true)]
    async fn unmute_pressed_before_the_video_is_ignored() {
        let (d, log, h) = rig();
        let stop = CancellationToken::new();
        let unmute = Arc::new(Notify::new());
        unmute.notify_one(); // stale press, no video playing
        let (d2, c, u) = (d.clone(), cfg(), unmute.clone());
        let task = tokio::spawn(async move {
            let mut levels = LiveLevels::default();
            play(&d2, &c, &cue(Destination::HallAndOnline), &mut levels, stop, u).await
        });
        until(&log, Call::PlayerPlay).await;
        sleep(Duration::from_secs(3)).await;
        assert!(!log.calls().contains(&Call::StreamSetAudioLevel(MIC, -3.0)), "{:?}", log.calls());
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(task.await.unwrap(), RollinOutcome::Finished);
    }

    #[tokio::test(start_paused = true)]
    async fn video_error_runs_return_path_and_stops_player() {
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::PlayerPlay).await;
        h.player.emit(PlayerEvent::Error("decode".into()));
        let (r, _) = run.task.await.unwrap();
        assert!(
            matches!(&r, RollinOutcome::Failed { recover_needed: false, reason } if reason.contains("decode")),
            "{r:?}"
        );
        let calls = log.calls();
        assert_in_order(
            &after_play(&calls),
            &[Call::StreamSetAudioOn(IN8, false), Call::ScreensAuto, Call::StreamAuto, Call::PlayerStop],
        );
        assert_eq!(calls.last(), Some(&Call::HubRoute { output: OUT, input: CAM8 }));
        assert_eq!(h.stream.program().await.unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn forward_route_failure_switches_nothing() {
        let (d, log, h) = rig();
        h.hub.fail_next("route");
        let (r, _) = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline)).task.await.unwrap();
        assert!(matches!(r, RollinOutcome::Failed { recover_needed: true, .. }), "{r:?}");
        let calls = log.calls();
        assert!(!calls.iter().any(|c| is_stream(c) || is_screens(c)), "{calls:?}");
        assert!(!calls.contains(&Call::PlayerPlay), "{calls:?}");
        assert_eq!(calls.last(), Some(&Call::HubRoute { output: OUT, input: CAM8 }));
    }

    #[tokio::test(start_paused = true)]
    async fn refuses_when_route_unreadable() {
        let (d, log, h) = rig();
        h.hub.fail_next("route_of");
        let (r, _) = spawn_play(&d, &cfg(), cue(Destination::OnlineOnly)).task.await.unwrap();
        assert!(matches!(r, RollinOutcome::Refused(_)), "{r:?}");
        assert_eq!(log.calls(), vec![Call::PlayerLoad(PathBuf::from("hymn.mp4"), -2.0), Call::PlayerStop]);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_before_play_switches_nothing() {
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::HubRoute { output: OUT, input: DK }).await; // now settling
        run.stop.cancel();
        let (r, _) = run.task.await.unwrap();
        assert_eq!(r, RollinOutcome::Stopped);
        let calls = log.calls();
        for c in [Call::StreamSetPreview(8), Call::StreamAuto, Call::ScreensAuto, Call::PlayerPlay] {
            assert!(!calls.contains(&c), "{c:?} in {calls:?}");
        }
        assert!(
            !calls.iter().any(|c| matches!(c, Call::StreamSetAudioLevel(..) | Call::StreamSetAudioOn(..))),
            "{calls:?}"
        );
        assert!(calls.contains(&Call::PlayerStop), "{calls:?}");
        assert_eq!(calls.last(), Some(&Call::HubRoute { output: OUT, input: CAM8 }));
        assert_eq!(h.hub.route_of(OUT).await.unwrap(), CAM8);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_kills_video_audio_first() {
        let (d, log, _h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::PlayerPlay).await;
        until(&log, Call::StreamSetAudioLevel(MIC, FLOOR_DB)).await;
        run.stop.cancel();
        assert_eq!(run.task.await.unwrap().0, RollinOutcome::Stopped);
        let tail = after_play(&log.calls());
        let off = pos(&tail, &Call::StreamSetAudioOn(IN8, false));
        assert!(off < pos(&tail, &Call::StreamAuto), "{tail:?}");
        assert!(off < pos(&tail, &Call::StreamSetAudioLevel(MIC, -3.0)), "{tail:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn failed_unmute_can_be_retried_and_return_still_fades_up() {
        let up = Call::StreamSetAudioLevel(MIC, -3.0);
        // A failed Unmute leaves Unmute armed: a second press fades the stream audio up.
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::StreamSetAudioLevel(MIC, FLOOR_DB)).await;
        h.stream.fail_next("set_audio_level");
        run.unmute.notify_one();
        sleep(Duration::from_secs(2)).await;
        assert!(!log.calls().contains(&up), "{:?}", log.calls());
        run.unmute.notify_one();
        until(&log, up.clone()).await;
        assert!(!log.calls().contains(&Call::PlayerStop));
        h.player.emit(PlayerEvent::Ended);
        let (r, _) = run.task.await.unwrap();
        assert!(matches!(r, RollinOutcome::Failed { recover_needed: true, .. }), "{r:?}");
        assert_eq!(log.calls().iter().filter(|c| **c == up).count(), 1);

        // A failed Unmute with no retry: the return path still fades the stream audio up.
        let (d, log, h) = rig();
        let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
        until(&log, Call::StreamSetAudioLevel(MIC, FLOOR_DB)).await;
        h.stream.fail_next("set_audio_level");
        run.unmute.notify_one();
        sleep(Duration::from_secs(2)).await;
        h.player.emit(PlayerEvent::Ended);
        run.task.await.unwrap();
        let tail = after_play(&log.calls());
        assert_in_order(&tail, &[Call::StreamAuto, up, Call::StreamSetAudioOn(IN8, false)]);
        assert_eq!(h.stream.audio_level(MIC).await.unwrap(), -3.0);
    }

    #[tokio::test(start_paused = true)]
    async fn screens_returns_before_stream_return_path() {
        for end in ["ended", "stop"] {
            let (d, log, h) = rig();
            let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
            until(&log, Call::PlayerPlay).await;
            until(&log, Call::StreamSetAudioLevel(MIC, FLOOR_DB)).await;
            if end == "ended" {
                h.player.emit(PlayerEvent::Ended);
            } else {
                run.stop.cancel();
            }
            run.task.await.unwrap();
            let tail = after_play(&log.calls());
            let screens = pos(&tail, &Call::ScreensAuto);
            assert!(screens < pos(&tail, &Call::StreamAuto), "{end}: {tail:?}");
            assert!(screens < pos(&tail, &Call::StreamSetAudioLevel(MIC, -3.0)), "{end}: {tail:?}");
            assert_eq!(tail.last(), Some(&Call::HubRoute { output: OUT, input: CAM8 }), "{end}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn rollin_fades_mics_to_duck_db() {
        let (d, log, h) = rig();
        let mut c = cfg();
        c.stream_atem.duck_db = -20.0;
        let r = play_to_end(&d, &c, cue(Destination::HallAndOnline), &log, &h).await;
        assert_eq!(r, RollinOutcome::Finished);
        let calls = log.calls();
        assert!(calls.contains(&Call::StreamSetAudioLevel(MIC, -20.0)), "{calls:?}");
        assert!(!calls.contains(&Call::StreamSetAudioLevel(MIC, FLOOR_DB)), "{calls:?}");
        assert_eq!(h.stream.audio_level(MIC).await.unwrap(), -3.0, "back to the live level");
    }

    #[tokio::test(start_paused = true)]
    async fn video_during_holding_leaves_mics_down() {
        let is_mic = |c: &Call| matches!(c, Call::StreamSetAudioLevel(m, _) if *m == MIC);
        for duck in [FLOOR_DB, -20.0] {
            let (d, log, h) = rig();
            h.stream.set_audio_level_state(MIC, FLOOR_DB); // Prepare's holding still: mics down
            let mut c = cfg();
            c.stream_atem.duck_db = duck;
            let r = play_to_end(&d, &c, cue(Destination::HallAndOnline), &log, &h).await;
            assert_eq!(r, RollinOutcome::Finished);
            let calls = log.calls();
            assert!(!calls.iter().any(is_mic), "duck {duck}: the stream audio was touched: {calls:?}");
            assert_eq!(h.stream.audio_level(MIC).await.unwrap(), FLOOR_DB, "duck {duck}");
            // The video's own sound still comes and goes.
            assert_in_order(&calls, &[Call::StreamSetAudioOn(IN8, true), Call::StreamSetAudioOn(IN8, false)]);
        }
    }

    /// I3: a real ATEM reports the old program until a mix completes. Stop pressed while
    /// the forward transitions are still running must still take both switchers back.
    #[tokio::test(start_paused = true)]
    async fn early_stop_during_forward_transition_still_returns() {
        for dest in [Destination::HallAndOnline, Destination::HallOnly] {
            let (d, log, h) = rig();
            h.stream.set_auto_delay(MS(2000)); // longer than the 1 s mic fade
            h.screens.set_auto_delay(MS(2000));
            let run = spawn_play(&d, &cfg(), cue(dest));
            until(&log, Call::PlayerPlay).await;
            // The forward mixes are still running.
            assert_eq!(h.screens.program().await.unwrap(), 1, "{dest:?}");
            run.stop.cancel();
            let (r, _) = run.task.await.unwrap();
            assert_eq!(r, RollinOutcome::Stopped, "{dest:?}: {:?}", log.calls());
            sleep(Duration::from_secs(3)).await;
            assert_eq!(h.screens.program().await.unwrap(), 1, "{dest:?}: the hall is left on the stopped video");
            assert_eq!(h.stream.program().await.unwrap(), 1, "{dest:?}: the stream is left on input 8");
            if dest == Destination::HallAndOnline {
                assert_eq!(h.hub.route_of(OUT).await.unwrap(), CAM8, "{dest:?}");
                let calls = log.calls();
                assert_eq!(calls.last(), Some(&Call::HubRoute { output: OUT, input: CAM8 }), "{calls:?}");
            }
        }
    }

    /// The NDI counter shows the time left from the first tick, follows the position,
    /// and is cleared once, at the start of the return path, however the video ends.
    #[tokio::test(start_paused = true)]
    async fn rollin_drives_ndi_counter_and_clears_it() {
        for end in ["ended", "stop", "error"] {
            let (d, log, h) = rig();
            let run = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline));
            until(&log, Call::PlayerPlay).await;
            until(&log, Call::CounterShow(Duration::from_secs(60))).await;
            h.player.set_position(Some(Duration::from_secs(20)));
            until(&log, Call::CounterShow(Duration::from_secs(40))).await;
            sleep(MS(1000)).await;
            let shows = log.calls().iter().filter(|c| matches!(c, Call::CounterShow(_))).count();
            assert!(shows >= 10, "{end}: {shows} counter updates in over a second");
            match end {
                "ended" => h.player.emit(PlayerEvent::Ended),
                "stop" => run.stop.cancel(),
                _ => h.player.emit(PlayerEvent::Error("decode".into())),
            }
            run.task.await.unwrap();
            let tail = after_play(&log.calls());
            let clear = pos(&tail, &Call::CounterClear);
            assert_eq!(tail.iter().filter(|c| **c == Call::CounterClear).count(), 1, "{end}: {tail:?}");
            assert!(!tail[clear..].iter().any(|c| matches!(c, Call::CounterShow(_))), "{end}: {tail:?}");
            // Before any return step.
            assert!(clear < pos(&tail, &Call::ScreensAuto), "{end}: {tail:?}");
            assert!(clear < pos(&tail, &Call::StreamSetAudioOn(IN8, false)), "{end}: {tail:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn rollin_clears_counter_when_play_fails() {
        let (d, log, h) = rig();
        h.player.fail_next("play");
        let (r, _) = spawn_play(&d, &cfg(), cue(Destination::HallAndOnline)).task.await.unwrap();
        assert!(matches!(r, RollinOutcome::Failed { .. }), "{r:?}");
        let tail = after_play(&log.calls());
        assert!(pos(&tail, &Call::CounterClear) < pos(&tail, &Call::ScreensAuto), "{tail:?}");
    }

    #[test]
    fn cue_list_rejects_fifteenth_and_keeps_order() {
        let mut l = CueList::default();
        for i in 0..MAX_CUES {
            let mut q = cue(Destination::HallAndOnline);
            q.name = format!("v{i}");
            l.push(q).unwrap();
        }
        assert_eq!(l.push(cue(Destination::HallOnly)), Err(CueListFull));
        assert_eq!(l.len(), 14);
        assert_eq!(l.state, vec![CueState::Cued; 14]);
        let names = |l: &CueList| l.cues().iter().map(|q| q.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&l), (0..14).map(|i| format!("v{i}")).collect::<Vec<_>>());

        l.state[1] = CueState::Played;
        assert!(l.move_up(1));
        assert_eq!(l.get(0).unwrap().name, "v1");
        assert_eq!(l.state[0], CueState::Played);
        assert!(!l.move_up(0));
        assert!(l.move_down(0));
        assert_eq!(l.get(1).unwrap().name, "v1");
        assert_eq!(l.state[1], CueState::Played);
        assert!(!l.move_down(13));
        assert_eq!(l.remove(1).unwrap().name, "v1");
        assert_eq!(l.state.len(), 13);
        assert!(l.state.iter().all(|s| *s == CueState::Cued));
        assert!(l.remove(20).is_none());
        assert!(l.get(13).is_none());
        l.push(cue(Destination::AudioOnly)).unwrap();
        assert_eq!(l.get(13).unwrap().destination, Destination::AudioOnly);
        assert_eq!(CueList::from_cues(l.cues().to_vec()).cues(), l.cues());
    }
}
