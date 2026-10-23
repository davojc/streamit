//! Go Live sequencer, stages 0-3: check, prepare, countdown, T-0.
//!
//! Device errors never abort `prepare`; they become warnings. In `countdown` only a
//! failure to load or play the countdown video (or the video itself failing) ends it
//! with `Err`; every other device error is a warning and the sequence carries on.
use crate::audio::{LiveLevels, Target, fade};
use crate::config::Config;
use crate::devices::{
    BroadcastCheck, BroadcastSummary, DeviceError, Devices, Health, Phase, PlayerEvent, Result, Switcher,
};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Instant, MissedTickBehavior, interval, sleep};
use tokio_util::sync::CancellationToken;

/// How often `prepare` polls YouTube stream health.
pub const HEALTH_POLL: Duration = Duration::from_secs(2);
/// How long `prepare` waits for the stream to become healthy.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(60);
/// Countdown counter update period.
pub const COUNTER_TICK: Duration = Duration::from_millis(100);
/// `go_public` retries after the first attempt.
pub const GO_PUBLIC_RETRIES: u32 = 3;
/// Gap between `go_public` attempts.
pub const GO_PUBLIC_RETRY_GAP: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    Waiting(String),
    Warning(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Idle,
    Prepared(Readiness),
    Countdown { remaining: Duration, public: bool },
    Live,
    Ending { remaining: Duration },
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage0Report {
    pub signed_in: bool,
    pub broadcasts: Vec<BroadcastSummary>,
    pub selected: Option<String>,
    pub check: Option<BroadcastCheck>,
    pub device_errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountdownOutcome {
    Live,
    Stopped,
}

/// Stage 0: read-only checks. Changes nothing on any device.
pub async fn stage0(d: &Devices) -> Stage0Report {
    let mut device_errors = Vec::new();
    let signed_in = match d.yt.signed_in().await {
        Ok(v) => v,
        Err(e) => {
            device_errors.push(format!("YouTube: {e}"));
            false
        }
    };
    let broadcasts = if signed_in {
        match d.yt.upcoming().await {
            Ok(v) => v,
            Err(e) => {
                device_errors.push(format!("YouTube broadcasts: {e}"));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let selected = broadcasts.first().map(|b| b.id.clone());
    let check = match &selected {
        Some(id) => match d.yt.check(id).await {
            Ok(c) => Some(c),
            Err(e) => {
                device_errors.push(format!("YouTube broadcast check: {e}"));
                None
            }
        },
        None => None,
    };
    if let Err(e) = d.stream.program().await {
        device_errors.push(format!("Stream ATEM: {e}"));
    }
    if let Err(e) = d.screens.program().await {
        device_errors.push(format!("Screens ATEM: {e}"));
    }
    if let Err(e) = d.encoder.status().await {
        device_errors.push(format!("Encoder: {e}"));
    }
    Stage0Report { signed_in, broadcasts, selected, check, device_errors }
}

fn note(problems: &mut Vec<String>, warn: &impl Fn(String), msg: String) {
    warn(msg.clone());
    problems.push(msg);
}

/// Stage 1: holding still on air, mics down, encoder on, YouTube in testing.
/// Never aborts early: each failed step becomes part of a `Warning`.
pub async fn prepare(
    d: &Devices,
    c: &Config,
    bid: &str,
    levels: &mut LiveLevels,
    progress: impl Fn(Readiness),
    warn: impl Fn(String),
) -> Readiness {
    let sa = &c.stream_atem;
    let mut problems = Vec::new();
    let waiting = |s: &str| progress(Readiness::Waiting(s.to_string()));

    waiting("Capturing mic levels");
    if let Err(e) = levels.recapture(&*d.stream, &sa.mic_sources).await {
        note(&mut problems, &warn, format!("Could not read mic levels: {e}"));
    }

    waiting("Putting the opening still on air");
    if let Err(e) = d.stream.set_media_player_still(sa.media_player, sa.opening_still).await {
        note(&mut problems, &warn, format!("Could not set the opening still: {e}"));
    }
    let still_input = d.stream.media_player_input(sa.media_player);
    if let Err(e) = cut(&*d.stream, still_input).await {
        note(&mut problems, &warn, format!("Could not cut the stream to the opening still: {e}"));
    }

    waiting("Fading the mics down");
    if let Err(e) = fade(&*d.stream, &sa.mic_sources, Target::Floor, sa.fade).await {
        note(&mut problems, &warn, format!("Could not fade the mics down: {e}"));
    }

    waiting("Starting the encoder");
    if let Err(e) = d.encoder.start().await {
        note(&mut problems, &warn, format!("Could not start the encoder: {e}"));
    }

    let deadline = Instant::now() + HEALTH_TIMEOUT;
    let healthy = loop {
        let last = match d.yt.health().await {
            Ok(Health::Good) => break true,
            Ok(h) => format!("{h:?}"),
            Err(e) => e.to_string(),
        };
        if Instant::now() >= deadline {
            let msg = format!("YouTube is not receiving a healthy stream after 60 s ({last})");
            note(&mut problems, &warn, msg);
            break false;
        }
        progress(Readiness::Waiting(format!("Waiting for YouTube to receive the stream ({last})")));
        sleep(HEALTH_POLL).await;
    };

    if healthy {
        let lc = match d.yt.life_cycle(bid).await {
            Ok(l) => Some(l),
            Err(e) => {
                note(&mut problems, &warn, format!("Could not read the broadcast state: {e}"));
                None
            }
        };
        // testStarting/liveStarting are YouTube's in-flight forms of testing/live.
        if !matches!(lc.as_deref(), Some("testing" | "live" | "testStarting" | "liveStarting")) {
            waiting("Starting the YouTube test");
            if let Err(e) = d.yt.transition(bid, Phase::Testing).await {
                note(&mut problems, &warn, format!("Could not put the broadcast into testing: {e}"));
            }
        }
    }

    let r = if problems.is_empty() { Readiness::Ready } else { Readiness::Warning(problems.join("; ")) };
    progress(r.clone());
    r
}

/// Preview then Auto. Auto is skipped when the preview could not be set, so a
/// failed preview never puts an unknown input on program.
async fn cut(sw: &dyn Switcher, input: u16) -> Result<()> {
    sw.set_preview(input).await?;
    sw.auto().await
}

fn joined(r: std::result::Result<Result<()>, JoinError>) -> Result<()> {
    r.unwrap_or_else(|e| Err(DeviceError::Other(format!("go public task failed: {e}"))))
}

fn spawn_go_public(d: &Devices, bid: &str) -> JoinHandle<Result<()>> {
    let (d, bid) = (d.clone(), bid.to_string());
    tokio::spawn(async move { go_public(&d, &bid).await })
}

/// Stages 2-3: countdown video to the hall, public at the offset, T-0 on `Ended`.
#[allow(clippy::too_many_arguments)]
pub async fn countdown(
    d: &Devices,
    c: &Config,
    bid: &str,
    ready: &Readiness,
    levels: &LiveLevels,
    cancel: CancellationToken,
    progress: impl Fn(Stage),
    warn: impl Fn(String),
) -> Result<CountdownOutcome> {
    let offset = c.timings.go_public_offset;
    if let Readiness::Warning(r) | Readiness::Waiting(r) = ready {
        warn(format!("Starting the countdown although not ready: {r}"));
    }
    // Subscribe before playing so `Ended` cannot be missed.
    let mut events = d.player.events();
    let duration = d.player.load(&c.playout.countdown_file, 0.0).await?;
    if duration <= offset {
        warn(format!(
            "The countdown video ({:.1} s) is not longer than the go-public offset ({:.1} s); going public now",
            duration.as_secs_f32(),
            offset.as_secs_f32()
        ));
    }
    if let Err(e) = cut(&*d.screens, c.screens_atem.decklink_input).await {
        warn(format!("Could not switch the hall to the countdown: {e}"));
    }
    d.player.play().await?;

    let mut public = false;
    let mut pub_started = false;
    let mut pub_task: Option<JoinHandle<Result<()>>> = None;
    let mut remaining = duration;
    let mut counter_warned = false;
    let mut tick = interval(COUNTER_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Leave YouTube as it is: abandon any go-public retries still pending.
                if let Some(t) = pub_task.take() {
                    t.abort();
                }
                if let Err(e) = d.player.stop().await {
                    warn(format!("Could not stop the countdown video: {e}"));
                }
                if let Err(e) = d.counter.clear().await {
                    warn(format!("Could not clear the counter: {e}"));
                }
                if let Err(e) = cut(&*d.screens, c.screens_atem.t0_input).await {
                    warn(format!("Could not return the hall screens: {e}"));
                }
                return Ok(CountdownOutcome::Stopped);
            }
            ev = events.recv() => match ev {
                Ok(PlayerEvent::Ended) => break,
                Ok(PlayerEvent::Error(e)) => return video_failed(d, &warn, e).await,
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return video_failed(d, &warn, "player went away".into()).await,
            },
            res = async {
                match pub_task.as_mut() {
                    Some(t) => t.await,
                    None => std::future::pending().await,
                }
            } => {
                pub_task = None;
                match joined(res) {
                    Ok(()) => public = true,
                    Err(e) => warn(format!("Could not make the YouTube broadcast public: {e}")),
                }
                progress(Stage::Countdown { remaining, public });
            }
            _ = tick.tick() => {
                remaining = duration.saturating_sub(d.player.position().unwrap_or_default());
                if !pub_started && remaining <= offset {
                    pub_started = true;
                    pub_task = Some(spawn_go_public(d, bid));
                }
                if let Err(e) = d.counter.show(remaining).await
                    && !counter_warned
                {
                    counter_warned = true;
                    warn(format!("Could not update the counter: {e}"));
                }
                progress(Stage::Countdown { remaining, public });
            }
        }
    }

    if !pub_started {
        warn("The countdown ended before the broadcast went public; going public now".into());
        pub_task = Some(spawn_go_public(d, bid));
    }
    let r = t0(d, c, levels).await;
    if let Some(t) = pub_task.take()
        && let Err(e) = joined(t.await)
    {
        warn(format!("Could not make the YouTube broadcast public: {e}"));
    }
    r?;
    progress(Stage::Live);
    Ok(CountdownOutcome::Live)
}

async fn video_failed(d: &Devices, warn: &impl Fn(String), e: String) -> Result<CountdownOutcome> {
    warn(format!("The countdown video failed: {e}"));
    if let Err(e) = d.counter.clear().await {
        warn(format!("Could not clear the counter: {e}"));
    }
    Err(DeviceError::Other(format!("countdown video failed: {e}")))
}

/// Make the broadcast public: one attempt plus `GO_PUBLIC_RETRIES` retries, 2 s apart.
/// A failed attempt counts as success if the broadcast is in fact already live.
pub async fn go_public(d: &Devices, bid: &str) -> Result<()> {
    let mut retries = 0;
    loop {
        match d.yt.transition(bid, Phase::Live).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if matches!(d.yt.life_cycle(bid).await.as_deref(), Ok("live")) {
                    return Ok(());
                }
                if retries == GO_PUBLIC_RETRIES {
                    return Err(e);
                }
                retries += 1;
                sleep(GO_PUBLIC_RETRY_GAP).await;
            }
        }
    }
}

/// T-0: stream to live action, mics up, hall to `t0_input`, counter off.
/// Mics come up only if the stream cut succeeded. Returns the first error.
pub async fn t0(d: &Devices, c: &Config, levels: &LiveLevels) -> Result<()> {
    let sa = &c.stream_atem;
    let stream_cut = async {
        let pv = d.stream.preview().await.ok();
        if !pv.is_some_and(|p| sa.live_inputs.contains(&p)) {
            d.stream.set_preview(sa.default_live_input).await?;
        }
        d.stream.auto().await
    }
    .await;
    let mut first_err = stream_cut.as_ref().err().cloned();
    if stream_cut.is_ok()
        && let Err(e) = fade(&*d.stream, &sa.mic_sources, Target::Live(levels), sa.fade).await
    {
        first_err.get_or_insert(e);
    }
    if let Err(e) = cut(&*d.screens, c.screens_atem.t0_input).await {
        first_err.get_or_insert(e);
    }
    if let Err(e) = d.counter.clear().await {
        first_err.get_or_insert(e);
    }
    first_err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FLOOR_DB;
    use crate::config::AudioSourceId;
    use crate::devices::{Health, Phase, PlayerEvent};
    use crate::fakes::{Call, CallLog, fake_devices};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tokio::task::JoinHandle;
    use tokio::time::{Instant, sleep};

    const MIC: AudioSourceId = AudioSourceId { input: 1001, source: -65280 };
    const MS: fn(u64) -> Duration = Duration::from_millis;

    type Rec<T> = Arc<Mutex<Vec<T>>>;

    fn sink<T: Send + 'static>() -> (Rec<T>, impl Fn(T) + Send + Sync + Clone + 'static) {
        let r: Rec<T> = Arc::default();
        let r2 = r.clone();
        (r, move |v| r2.lock().unwrap().push(v))
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

    fn live_transitions(log: &CallLog) -> usize {
        log.calls().iter().filter(|c| **c == Call::YtTransition(Phase::Live)).count()
    }

    fn yt_calls(calls: &[Call]) -> Vec<Call> {
        calls.iter().filter(|c| matches!(c, Call::YtTransition(_) | Call::YtFix(_))).cloned().collect()
    }

    struct Run {
        task: JoinHandle<Result<CountdownOutcome>>,
        stages: Rec<Stage>,
        warns: Rec<String>,
        cancel: CancellationToken,
    }

    fn spawn_countdown(d: &Devices, c: &Config, ready: Readiness, levels: LiveLevels) -> Run {
        let (stages, p) = sink();
        let (warns, w) = sink();
        let cancel = CancellationToken::new();
        let (d, c, tok) = (d.clone(), c.clone(), cancel.clone());
        let task = tokio::spawn(async move { countdown(&d, &c, "b", &ready, &levels, tok, p, w).await });
        Run { task, stages, warns, cancel }
    }

    #[tokio::test(start_paused = true)]
    async fn stage0_selects_first_upcoming_and_changes_nothing() {
        let (d, log, h) = fake_devices();
        let b = |id: &str| BroadcastSummary {
            id: id.into(),
            title: id.into(),
            scheduled_start: None,
            life_cycle: "ready".into(),
            bound_stream_id: None,
        };
        h.yt.set_upcoming(vec![b("first"), b("second")]);
        let r = stage0(&d).await;
        assert!(r.signed_in);
        assert_eq!(r.broadcasts.len(), 2);
        assert_eq!(r.selected.as_deref(), Some("first"));
        assert!(r.check.is_some());
        assert!(r.device_errors.is_empty(), "{:?}", r.device_errors);
        h.encoder.fail_next("status");
        let r = stage0(&d).await;
        assert_eq!(r.device_errors.len(), 1);
        assert!(log.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_runs_in_order_and_goes_testing() {
        let (d, log, h) = fake_devices();
        let mut c = Config::default();
        c.stream_atem.opening_still = 4;
        h.stream.set_audio_level_state(MIC, -3.0);
        let mut levels = LiveLevels::default();
        let (progress, p) = sink();
        let (warns, w) = sink();
        let r = prepare(&d, &c, "b", &mut levels, p, w).await;
        assert_eq!(r, Readiness::Ready);
        assert_eq!(levels.0[&MIC], -3.0);
        let calls = log.calls();
        assert_in_order(
            &calls,
            &[
                Call::StreamSetMediaStill(1, 4),
                Call::StreamSetPreview(3010),
                Call::StreamAuto,
                Call::StreamSetAudioLevel(MIC, FLOOR_DB),
                Call::EncoderStart,
                Call::YtTransition(Phase::Testing),
            ],
        );
        // The whole fade sits between the cut to the still and the encoder start.
        let auto = pos(&calls, &Call::StreamAuto);
        let enc = pos(&calls, &Call::EncoderStart);
        let fades: Vec<usize> = (0..calls.len())
            .filter(|&i| matches!(calls[i], Call::StreamSetAudioLevel(..)))
            .collect();
        assert_eq!(fades.len(), 20);
        assert!(fades.iter().all(|&i| i > auto && i < enc));
        assert_eq!(calls[*fades.last().unwrap()], Call::StreamSetAudioLevel(MIC, FLOOR_DB));
        assert_eq!(calls.len(), 3 + 20 + 2);
        assert!(warns.lock().unwrap().is_empty());
        assert_eq!(progress.lock().unwrap().last(), Some(&Readiness::Ready));
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_skips_testing_if_already_testing() {
        for lc in ["testing", "live"] {
            let (d, log, h) = fake_devices();
            h.yt.set_life_cycle(lc);
            let mut levels = LiveLevels::default();
            let r = prepare(&d, &Config::default(), "b", &mut levels, |_| {}, |_| {}).await;
            assert_eq!(r, Readiness::Ready);
            assert!(yt_calls(&log.calls()).is_empty(), "{lc}");
            assert!(log.calls().contains(&Call::EncoderStart));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_warns_when_stream_never_healthy() {
        let (d, log, h) = fake_devices();
        h.yt.set_health(Health::NotReceiving);
        let mut levels = LiveLevels::default();
        let (progress, p) = sink();
        let (warns, w) = sink();
        let start = Instant::now();
        let r = prepare(&d, &Config::default(), "b", &mut levels, p, w).await;
        let took = start.elapsed();
        assert!(matches!(r, Readiness::Warning(_)), "{r:?}");
        assert!(took >= Duration::from_secs(60) && took < Duration::from_secs(62), "{took:?}");
        assert!(yt_calls(&log.calls()).is_empty());
        assert!(log.calls().contains(&Call::EncoderStart));
        assert!(progress.lock().unwrap().iter().any(|r| matches!(r, Readiness::Waiting(_))));
        assert!(!warns.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_becomes_ready_when_health_turns_good() {
        let (d, log, h) = fake_devices();
        h.yt.set_health(Health::NotReceiving);
        let h2 = h.clone();
        tokio::spawn(async move {
            sleep(Duration::from_secs(5)).await;
            h2.yt.set_health(Health::Good);
        });
        let mut levels = LiveLevels::default();
        let start = Instant::now();
        let r = prepare(&d, &Config::default(), "b", &mut levels, |_| {}, |_| {}).await;
        assert_eq!(r, Readiness::Ready);
        assert!(start.elapsed() < Duration::from_secs(10));
        assert_eq!(yt_calls(&log.calls()), vec![Call::YtTransition(Phase::Testing)]);
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_goes_public_at_offset() {
        let (d, log, h) = fake_devices();
        let c = Config::default();
        h.player.set_duration(Duration::from_secs(60));
        h.player.set_position(Some(Duration::ZERO));
        let run = spawn_countdown(&d, &c, Readiness::Ready, LiveLevels::default());
        sleep(MS(250)).await;
        assert_eq!(
            log.calls()[..4],
            [
                Call::PlayerLoad(PathBuf::from("countdown.mp4"), 0.0),
                Call::ScreensSetPreview(8),
                Call::ScreensAuto,
                Call::PlayerPlay,
            ]
        );
        assert!(log.calls().contains(&Call::CounterShow(Duration::from_secs(60))));

        h.player.set_position(Some(MS(49_900)));
        sleep(MS(200)).await;
        assert_eq!(live_transitions(&log), 0);
        assert!(log.calls().contains(&Call::CounterShow(MS(10_100))));

        h.player.set_position(Some(MS(50_000)));
        sleep(MS(200)).await;
        assert_eq!(live_transitions(&log), 1);
        h.player.set_position(Some(MS(55_000)));
        sleep(Duration::from_secs(1)).await;
        assert_eq!(live_transitions(&log), 1);
        assert!(run.stages.lock().unwrap().iter().any(|s| matches!(s, Stage::Countdown { public: true, .. })));

        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live);
        assert_eq!(run.stages.lock().unwrap().last(), Some(&Stage::Live));
        assert!(log.calls().ends_with(&[Call::ScreensSetPreview(6), Call::ScreensAuto, Call::CounterClear]));
        assert!(run.warns.lock().unwrap().is_empty(), "{:?}", run.warns.lock().unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn short_countdown_goes_public_immediately() {
        let (d, log, h) = fake_devices();
        let c = Config::default();
        h.player.set_duration(Duration::from_secs(5));
        h.player.set_position(Some(Duration::ZERO));
        let run = spawn_countdown(&d, &c, Readiness::Ready, LiveLevels::default());
        sleep(MS(50)).await;
        let calls = log.calls();
        assert_eq!(live_transitions(&log), 1);
        assert!(pos(&calls, &Call::YtTransition(Phase::Live)) > pos(&calls, &Call::PlayerPlay));
        assert_eq!(run.warns.lock().unwrap().len(), 1);
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live);
        assert_eq!(live_transitions(&log), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_when_not_ready_warns_but_runs() {
        let (d, log, h) = fake_devices();
        h.player.set_position(Some(Duration::ZERO));
        let ready = Readiness::Warning("stream not healthy".into());
        let run = spawn_countdown(&d, &Config::default(), ready, LiveLevels::default());
        sleep(MS(250)).await;
        assert!(log.calls().contains(&Call::PlayerPlay));
        assert!(!run.warns.lock().unwrap().is_empty());
        run.cancel.cancel();
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn go_public_failure_retries_and_flags() {
        let (d, log, h) = fake_devices();
        h.yt.fail_times("transition", 4);
        let start = Instant::now();
        assert!(go_public(&d, "b").await.is_err());
        assert_eq!(live_transitions(&log), 4);
        assert_eq!(start.elapsed(), Duration::from_secs(6));

        // Inside countdown: retries run in the background; the counter keeps ticking.
        h.yt.fail_times("transition", 4);
        h.player.set_duration(Duration::from_secs(5));
        h.player.set_position(Some(Duration::ZERO));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(Duration::from_secs(3)).await;
        let shows = log.calls().iter().filter(|c| matches!(c, Call::CounterShow(_))).count();
        assert!(shows >= 29, "counter stalled: {shows} updates in 3 s");
        sleep(Duration::from_secs(4)).await;
        assert_eq!(live_transitions(&log), 8);
        assert!(run.warns.lock().unwrap().len() >= 2); // short countdown + go public failed
        assert!(matches!(
            run.stages.lock().unwrap().last(),
            Some(Stage::Countdown { public: false, .. })
        ));
        assert!(!run.stages.lock().unwrap().iter().any(|s| matches!(s, Stage::Countdown { public: true, .. })));
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live);
        assert_eq!(live_transitions(&log), 8);
    }

    #[tokio::test(start_paused = true)]
    async fn t0_fixes_preview_when_not_live_input() {
        let (d, log, h) = fake_devices();
        h.stream.set_preview_state(5);
        t0(&d, &Config::default(), &LiveLevels::default()).await.unwrap();
        assert_eq!(log.calls()[..2], [Call::StreamSetPreview(1), Call::StreamAuto]);
        assert_eq!(d.stream.program().await.unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn t0_keeps_preview_when_live_input() {
        let (d, log, h) = fake_devices();
        h.stream.set_preview_state(2);
        t0(&d, &Config::default(), &LiveLevels::default()).await.unwrap();
        assert!(!log.calls().iter().any(|c| matches!(c, Call::StreamSetPreview(_))));
        assert_eq!(log.calls()[0], Call::StreamAuto);
        assert_eq!(d.stream.program().await.unwrap(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn t0_switches_screens_and_fades_mics_up() {
        let (d, log, h) = fake_devices();
        h.stream.set_preview_state(3);
        h.stream.set_audio_level_state(MIC, FLOOR_DB);
        let levels = LiveLevels([(MIC, -3.0)].into_iter().collect());
        t0(&d, &Config::default(), &levels).await.unwrap();
        let calls = log.calls();
        let fades = calls.iter().filter(|c| matches!(c, Call::StreamSetAudioLevel(..))).count();
        assert_eq!(fades, 20);
        assert_eq!(calls[0], Call::StreamAuto);
        assert_eq!(calls[20], Call::StreamSetAudioLevel(MIC, -3.0));
        assert_eq!(calls[21..], [Call::ScreensSetPreview(6), Call::ScreensAuto, Call::CounterClear]);
        assert_eq!(d.screens.program().await.unwrap(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_countdown_returns_hall_and_leaves_youtube() {
        let (d, log, h) = fake_devices();
        h.player.set_position(Some(Duration::ZERO));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(MS(300)).await;
        run.cancel.cancel();
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Stopped);
        let calls = log.calls();
        let play = pos(&calls, &Call::PlayerPlay);
        let tail: Vec<Call> =
            calls[play + 1..].iter().filter(|c| !matches!(c, Call::CounterShow(_))).cloned().collect();
        assert_eq!(tail, [Call::PlayerStop, Call::CounterClear, Call::ScreensSetPreview(6), Call::ScreensAuto]);
        assert!(yt_calls(&calls).is_empty());
        assert!(!calls.contains(&Call::StreamAuto));
    }

    #[tokio::test(start_paused = true)]
    async fn stop_countdown_abandons_pending_go_public_retries() {
        let (d, log, h) = fake_devices();
        h.yt.fail_times("transition", 4);
        h.player.set_duration(Duration::from_secs(5));
        h.player.set_position(Some(Duration::ZERO));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(MS(500)).await;
        assert_eq!(live_transitions(&log), 1);
        run.cancel.cancel();
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Stopped);
        sleep(Duration::from_secs(10)).await;
        assert_eq!(live_transitions(&log), 1);
    }
}
