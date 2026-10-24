//! Go Live sequencer, stages 0-3: check, prepare, countdown, T-0.
//!
//! Device errors never abort `prepare`; they become warnings. In `countdown` only a
//! failure to load or play the countdown video (or the video itself failing) ends it
//! with `Err`; every other device error is a warning and the sequence carries on.
use crate::audio::{LiveLevels, Target, down_threshold, fade};
use crate::config::Config;
use crate::devices::{
    BroadcastCheck, BroadcastSummary, DeviceError, Devices, Health, Phase, PlayerEvent, Result, Switcher,
};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Instant, MissedTickBehavior, interval, sleep};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

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
/// Longest wait for an in-flight `go_public` once the countdown has ended or failed.
pub const GO_PUBLIC_WAIT: Duration = Duration::from_secs(10);

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stage0Report {
    pub signed_in: bool,
    /// The broadcast list was read. When false, `broadcasts` is empty and says nothing.
    pub listed: bool,
    pub broadcasts: Vec<BroadcastSummary>,
    pub selected: Option<String>,
    pub check: Option<BroadcastCheck>,
    pub device_errors: Vec<String>,
}

/// What `countdown` reports along the way, besides its stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CountdownReport {
    /// The countdown video is loaded and about to play; this is its length.
    Started(Duration),
    /// A plain-language step for the activity list.
    Note(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountdownOutcome {
    /// T-0 is done. `public` is whether the broadcast went public (go-public settled Ok);
    /// `mics_failed` is whether T-0 could not bring the stream audio up (I1: Recover is offered).
    Live {
        public: bool,
        mics_failed: bool,
    },
    Stopped,
}

/// The broadcast to select when none is: one already in testing or live and bound to
/// our stream (R23: StreamIt restarted mid-service), else the first that is not yet
/// active (an upcoming one), else none. Another active broadcast is stale or someone
/// else's, so it is never the default.
pub fn preferred_broadcast(list: &[BroadcastSummary], stream_id: Option<&str>) -> Option<String> {
    // testStarting/liveStarting are YouTube's in-flight forms of testing/live.
    let active =
        |b: &BroadcastSummary| matches!(b.life_cycle.as_str(), "testing" | "live" | "testStarting" | "liveStarting");
    let ours = |b: &&BroadcastSummary| active(b) && stream_id.is_some() && b.bound_stream_id.as_deref() == stream_id;
    list.iter().find(ours).or_else(|| list.iter().find(|b| !active(b))).map(|b| b.id.clone())
}

/// Stage 0: read-only checks. Changes nothing on any device.
pub async fn stage0(d: &Devices, c: &Config) -> Stage0Report {
    let mut report = stage0_youtube(d, c).await;
    report.device_errors.extend(stage0_devices(d).await);
    report
}

/// Stage 0's YouTube part: sign-in, the broadcast list, the default selection and its
/// check. Read-only. Kept apart so a slow YouTube can be bounded on its own.
pub async fn stage0_youtube(d: &Devices, c: &Config) -> Stage0Report {
    let mut device_errors = Vec::new();
    let signed_in = match d.yt.signed_in().await {
        Ok(v) => v,
        Err(e) => {
            device_errors.push(format!("YouTube: {e}"));
            false
        }
    };
    let (listed, broadcasts) = if signed_in {
        match d.yt.upcoming().await {
            Ok(v) => (true, v),
            Err(e) => {
                device_errors.push(format!("YouTube broadcasts: {e}"));
                (false, Vec::new())
            }
        }
    } else {
        (false, Vec::new())
    };
    let selected = preferred_broadcast(&broadcasts, c.youtube.stream_id.as_deref());
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
    Stage0Report { signed_in, listed, broadcasts, selected, check, device_errors }
}

/// Stage 0's equipment part: one read of each switcher and the encoder. Read-only.
pub async fn stage0_devices(d: &Devices) -> Vec<String> {
    let mut device_errors = Vec::new();
    if let Err(e) = d.stream.program().await {
        device_errors.push(format!("Stream ATEM: {e}"));
    }
    if let Err(e) = d.screens.program().await {
        device_errors.push(format!("Screens ATEM: {e}"));
    }
    if let Err(e) = d.encoder.status().await {
        device_errors.push(format!("Encoder: {e}"));
    }
    device_errors
}

fn problem(problems: &mut Vec<String>, warn: &impl Fn(String), msg: String) {
    warn(msg.clone());
    problems.push(msg);
}

/// Stage 1: holding still on air, mics down, encoder on, YouTube in testing.
/// Never aborts early: each failed step becomes part of a `Warning`. `note` gets a
/// plain-language line for each milestone reached.
pub async fn prepare(
    d: &Devices,
    c: &Config,
    bid: &str,
    levels: &mut LiveLevels,
    progress: impl Fn(Readiness),
    warn: impl Fn(String),
    note: impl Fn(String),
) -> Readiness {
    let sa = &c.stream_atem;
    let mut problems = Vec::new();
    let waiting = |s: &str| progress(Readiness::Waiting(s.to_string()));

    waiting("Capturing the stream audio levels");
    if let Err(e) = levels.recapture(&*d.stream, &sa.mic_sources, down_threshold(sa.duck_db)).await {
        problem(&mut problems, &warn, format!("Could not read the stream audio levels: {e}"));
    }

    waiting("Putting the opening still on air");
    let before = problems.len();
    if let Err(e) = d.stream.set_media_player_still(sa.media_player, sa.opening_still).await {
        problem(&mut problems, &warn, format!("Could not set the opening still: {e}"));
    }
    let still_input = d.stream.media_player_input(sa.media_player);
    if let Err(e) = cut(&*d.stream, still_input).await {
        problem(&mut problems, &warn, format!("Could not cut the stream to the opening still: {e}"));
    }

    waiting("Fading the stream audio down");
    if let Err(e) = fade(&*d.stream, &sa.mic_sources, Target::Floor, sa.fade).await {
        problem(&mut problems, &warn, format!("Could not fade the stream audio down: {e}"));
    }
    if problems.len() == before {
        note("Opening still on air · stream audio silent".into());
    }

    waiting("Starting the encoder");
    match d.encoder.start().await {
        Ok(()) => note("Web Presenter streaming".into()),
        Err(e) => problem(&mut problems, &warn, format!("Could not start the encoder: {e}")),
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
            problem(&mut problems, &warn, msg);
            break false;
        }
        progress(Readiness::Waiting(format!("Waiting for YouTube to receive the stream ({last})")));
        sleep(HEALTH_POLL).await;
    };

    if healthy {
        note("YouTube receiving · private".into());
        let lc = match d.yt.life_cycle(bid).await {
            Ok(l) => Some(l),
            Err(e) => {
                problem(&mut problems, &warn, format!("Could not read the broadcast state: {e}"));
                None
            }
        };
        // testStarting/liveStarting are YouTube's in-flight forms of testing/live.
        if !matches!(lc.as_deref(), Some("testing" | "live" | "testStarting" | "liveStarting")) {
            waiting("Starting the YouTube test");
            if let Err(e) = d.yt.transition(bid, Phase::Testing).await {
                problem(&mut problems, &warn, format!("Could not put the broadcast into testing: {e}"));
            }
        }
    }

    let r = if problems.is_empty() { Readiness::Ready } else { Readiness::Warning(problems.join("; ")) };
    progress(r.clone());
    r
}

/// Preview then Auto. Auto is skipped when the preview could not be set, so a
/// failed preview never puts an unknown input on program.
pub(crate) async fn cut(sw: &dyn Switcher, input: u16) -> Result<()> {
    sw.set_preview(input).await?;
    sw.auto().await
}

/// An in-flight go-public; `Ok` holds how long it took. Dropping it aborts the task, so
/// a countdown that is itself aborted part-way (a simulated crash) leaves no go-public
/// retrying on its own. Every normal path settles or aborts it first.
struct GoPublicTask(JoinHandle<Result<Duration>>);

impl GoPublicTask {
    fn abort(&self) {
        self.0.abort();
    }
}

impl Drop for GoPublicTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn joined(r: std::result::Result<Result<Duration>, JoinError>) -> Result<Duration> {
    r.unwrap_or_else(|e| Err(DeviceError::Other(format!("go public task failed: {e}"))))
}

fn spawn_go_public(d: &Devices, bid: &str) -> GoPublicTask {
    let (d, bid) = (d.clone(), bid.to_string());
    GoPublicTask(tokio::spawn(
        async move {
            let start = Instant::now();
            go_public(&d, &bid).await.map(|()| start.elapsed())
        }
        .in_current_span(),
    ))
}

/// The activity line for a go-public that worked after `took`.
pub fn public_note(took: Duration) -> String {
    format!("YouTube public (took {:.1} s)", took.as_secs_f32())
}

/// Stages 2-3: countdown video to the hall, public at the offset, T-0 on `Ended`.
/// `progress` gets the stage every `COUNTER_TICK`; `report` gets the video's length
/// once it plays, and a plain-language line for each milestone.
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
    report: impl Fn(CountdownReport),
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
    if let Err(e) = d.player.play().await {
        return video_failed(d, c, &progress, &warn, &report, None, duration, e.to_string()).await;
    }
    report(CountdownReport::Started(duration));

    let mut public = false;
    let mut pub_started = false;
    let mut pub_task: Option<GoPublicTask> = None;
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
                Ok(PlayerEvent::Error(e)) => {
                    return video_failed(d, c, &progress, &warn, &report, pub_task.take(), remaining, e).await;
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => {
                    let e = "the player went away".to_string();
                    return video_failed(d, c, &progress, &warn, &report, pub_task.take(), remaining, e).await;
                }
            },
            res = async {
                match pub_task.as_mut() {
                    Some(t) => (&mut t.0).await,
                    None => std::future::pending().await,
                }
            } => {
                pub_task = None;
                match joined(res) {
                    Ok(took) => {
                        public = true;
                        report(CountdownReport::Note(public_note(took)));
                    }
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

    // Start service now at T-0. Public already: only the cut. Go-public still in flight:
    // the cut, with that attempt settled after it (never a second one). Never started,
    // or failed: a fresh go-public alongside the cut.
    if !pub_started {
        warn("The countdown ended before the broadcast went public; going public now".into());
    }
    let in_flight = pub_task.take();
    let already_public = public || in_flight.is_some();
    let note = |m: String| report(CountdownReport::Note(m));
    let out = start_service_reporting(d, c, bid, already_public, levels, note, |r| {
        let on_air = match r {
            Ok(()) => true,
            Err(e) => {
                if e.on_air {
                    warn(format!("Live, but some T-0 steps failed: {e}"));
                }
                e.on_air
            }
        };
        if on_air {
            // LIVE is reported as soon as the cut is done; YouTube never holds it back.
            progress(Stage::Live);
        }
    })
    .await;
    let public = match out.public {
        Some(Ok(())) => true,
        Some(Err(e)) => {
            warn(format!("Could not make the YouTube broadcast public: {e}"));
            false
        }
        None => settle_go_public(in_flight, &warn, &report).await.unwrap_or(public),
    };
    let mics_failed = out.t0.as_ref().err().is_some_and(|e| e.mics_failed);
    if let Err(e) = out.t0
        && !e.on_air
    {
        return Err(DeviceError::Other(e.to_string()));
    }
    Ok(CountdownOutcome::Live { public, mics_failed })
}

/// Waits at most `GO_PUBLIC_WAIT` for an in-flight `go_public`; on timeout it is
/// aborted, never left detached. `Some(public)` if there was a task, else `None`.
async fn settle_go_public(
    task: Option<GoPublicTask>,
    warn: &impl Fn(String),
    report: &impl Fn(CountdownReport),
) -> Option<bool> {
    let mut t = task?;
    match tokio::time::timeout(GO_PUBLIC_WAIT, &mut t.0).await {
        Ok(res) => match joined(res) {
            Ok(took) => {
                report(CountdownReport::Note(public_note(took)));
                Some(true)
            }
            Err(e) => {
                warn(format!("Could not make the YouTube broadcast public: {e}"));
                Some(false)
            }
        },
        Err(_) => {
            t.abort();
            warn(format!(
                "Making the YouTube broadcast public did not finish within {} s; check YouTube Studio",
                GO_PUBLIC_WAIT.as_secs()
            ));
            Some(false)
        }
    }
}

/// The countdown video failed (or would not play): return the hall exactly as Stop
/// Countdown does, settle any in-flight go-public, and hand back to the operator.
#[allow(clippy::too_many_arguments)]
async fn video_failed(
    d: &Devices,
    c: &Config,
    progress: &impl Fn(Stage),
    warn: &impl Fn(String),
    report: &impl Fn(CountdownReport),
    pub_task: Option<GoPublicTask>,
    remaining: Duration,
    err: String,
) -> Result<CountdownOutcome> {
    warn(format!(
        "The countdown video failed ({err}). The hall screens are back on their normal input; press Go Live manually."
    ));
    if let Err(e) = cut(&*d.screens, c.screens_atem.t0_input).await {
        warn(format!("Could not return the hall screens: {e}"));
    }
    if let Err(e) = d.counter.clear().await {
        warn(format!("Could not clear the counter: {e}"));
    }
    if let Err(e) = d.player.stop().await {
        warn(format!("Could not stop the countdown video: {e}"));
    }
    if let Some(public) = settle_go_public(pub_task, warn, report).await {
        progress(Stage::Countdown { remaining, public });
    }
    Err(DeviceError::Other(format!("countdown video failed: {err}")))
}

/// Make the broadcast public: one attempt plus `GO_PUBLIC_RETRIES` retries, 2 s apart.
/// A failed attempt counts as success if the broadcast is in fact already live.
pub async fn go_public(d: &Devices, bid: &str) -> Result<()> {
    transition_with_retry(d, bid, Phase::Live).await
}

/// Transition the broadcast to `to`: one attempt plus `GO_PUBLIC_RETRIES` retries, 2 s apart.
/// A failed attempt counts as success if the broadcast is in fact already in the target state.
pub(crate) async fn transition_with_retry(d: &Devices, bid: &str, to: Phase) -> Result<()> {
    let target = match to {
        Phase::Testing => "testing",
        Phase::Live => "live",
        Phase::Complete => "complete",
    };
    let mut retries = 0;
    loop {
        match d.yt.transition(bid, to).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if matches!(d.yt.life_cycle(bid).await.as_deref(), Ok(lc) if lc == target) {
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

/// What Start service now did: T-0's result, and the go-public's (`None` when the
/// broadcast was already public, so none ran).
#[derive(Debug)]
pub struct StartOutcome {
    pub t0: std::result::Result<(), T0Error>,
    pub public: Option<Result<()>>,
}

/// Start service now: T-0 and, unless the broadcast is already public, `go_public`
/// alongside it. The cut never waits for YouTube; the go-public gets at most
/// `GO_PUBLIC_WAIT` (then it is dropped, never left detached, and counts as failed).
pub async fn start_service(
    d: &Devices,
    c: &Config,
    bid: &str,
    already_public: bool,
    levels: &LiveLevels,
) -> StartOutcome {
    start_service_reporting(d, c, bid, already_public, levels, |_| {}, |_| {}).await
}

/// `start_service`, also giving `note` T-0's line and "YouTube public (took X s)",
/// and calling `cut_done` with T-0's result as soon as T-0 is done, before any
/// go-public still running has settled (so LIVE is never held back by YouTube).
pub async fn start_service_reporting(
    d: &Devices,
    c: &Config,
    bid: &str,
    already_public: bool,
    levels: &LiveLevels,
    note: impl Fn(String),
    cut_done: impl FnOnce(&std::result::Result<(), T0Error>),
) -> StartOutcome {
    let cut = async {
        let r = t0_reporting(d, c, levels, &note).await;
        cut_done(&r);
        r
    };
    let public = async {
        if already_public {
            return None;
        }
        let start = Instant::now();
        let r = match tokio::time::timeout(GO_PUBLIC_WAIT, go_public(d, bid)).await {
            Ok(r) => r,
            Err(_) => Err(DeviceError::Other(format!(
                "it did not finish within {} s; check YouTube Studio",
                GO_PUBLIC_WAIT.as_secs()
            ))),
        };
        if r.is_ok() {
            note(public_note(start.elapsed()));
        }
        Some(r)
    };
    let (t0, public) = tokio::join!(cut, public);
    StartOutcome { t0, public }
}

/// Some T-0 steps failed. `on_air` says whether the stream cut to live action
/// itself succeeded: if it did, the service is live and only later steps failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct T0Error {
    pub on_air: bool,
    /// The stream cut succeeded but the mic fade-up failed: the mics may still be down.
    pub mics_failed: bool,
    /// One message per failed step, in order.
    pub failures: Vec<String>,
}

impl std::fmt::Display for T0Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.failures.join("; "))
    }
}

impl std::error::Error for T0Error {}

/// T-0: stream to live action, then stream audio up and hall to `t0_input` together (the hall
/// does not wait out the 1 s mic fade, as for roll-ins), then counter off.
/// Mics come up only if the stream cut succeeded. Every step is attempted.
pub async fn t0(d: &Devices, c: &Config, levels: &LiveLevels) -> std::result::Result<(), T0Error> {
    t0_reporting(d, c, levels, |_| {}).await
}

/// `t0`, also giving `note` one plain line saying what went on air once the stream
/// cut has succeeded, e.g. "Live · Cam 1 on air · stream audio up · hall → input 6".
pub async fn t0_reporting(
    d: &Devices,
    c: &Config,
    levels: &LiveLevels,
    note: impl Fn(String),
) -> std::result::Result<(), T0Error> {
    let sa = &c.stream_atem;
    // Ok holds the live input now on program.
    let stream_cut = async {
        let input = match d.stream.preview().await.ok().filter(|p| sa.live_inputs.contains(p)) {
            Some(p) => p,
            None => {
                d.stream.set_preview(sa.default_live_input).await?;
                sa.default_live_input
            }
        };
        d.stream.auto().await.map(|()| input)
    }
    .await;
    let mut failures = Vec::new();
    let mics_up = async {
        match &stream_cut {
            Ok(_) => Some(fade(&*d.stream, &sa.mic_sources, Target::Live(levels), sa.fade).await),
            Err(_) => None,
        }
    };
    let (faded, hall) = tokio::join!(mics_up, cut(&*d.screens, c.screens_atem.t0_input));
    if let Err(e) = &stream_cut {
        failures.push(format!("Could not cut the stream to live action: {e}"));
    }
    let mics_failed = matches!(faded, Some(Err(_)));
    if let Ok(input) = &stream_cut {
        let mics = if mics_failed { "stream audio still down" } else { "stream audio up" };
        let hall = match &hall {
            Ok(()) => format!("hall → input {}", c.screens_atem.t0_input),
            Err(_) => "hall not switched".into(),
        };
        note(format!("Live · Cam {input} on air · {mics} · {hall}"));
    }
    if let Some(Err(e)) = faded {
        failures.push(format!("Could not fade the stream audio up: {e}"));
    }
    if let Err(e) = hall {
        failures.push(format!("Could not switch the hall screens: {e}"));
    }
    if let Err(e) = d.counter.clear().await {
        failures.push(format!("Could not clear the counter: {e}"));
    }
    if failures.is_empty() { Ok(()) } else { Err(T0Error { on_air: stream_cut.is_ok(), mics_failed, failures }) }
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

    /// T-0's tail: the stream cut, then the hall switch (alongside the mic fade), then
    /// the counter cleared last.
    fn assert_t0_tail(calls: &[Call]) {
        let auto = calls.iter().rposition(|c| *c == Call::StreamAuto).expect("stream cut");
        assert_in_order(&calls[auto..], &[Call::ScreensSetPreview(6), Call::ScreensAuto]);
        assert_eq!(calls.last(), Some(&Call::CounterClear), "{calls:?}");
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
        let task = tokio::spawn(async move { countdown(&d, &c, "b", &ready, &levels, tok, p, w, |_| {}).await });
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
        let r = stage0(&d, &Config::default()).await;
        assert!(r.signed_in);
        assert_eq!(r.broadcasts.len(), 2);
        assert_eq!(r.selected.as_deref(), Some("first"));
        assert!(r.check.is_some());
        assert!(r.device_errors.is_empty(), "{:?}", r.device_errors);
        h.encoder.fail_next("status");
        let r = stage0(&d, &Config::default()).await;
        assert_eq!(r.device_errors.len(), 1);
        assert!(log.calls().is_empty());
    }

    fn bcast(id: &str, life_cycle: &str, stream: Option<&str>) -> BroadcastSummary {
        BroadcastSummary {
            id: id.into(),
            title: id.into(),
            scheduled_start: None,
            life_cycle: life_cycle.into(),
            bound_stream_id: stream.map(String::from),
        }
    }

    #[test]
    fn default_skips_stale_active_broadcast() {
        let list = [bcast("foreign", "testing", Some("other")), bcast("ours", "ready", Some("s1"))];
        assert_eq!(preferred_broadcast(&list, Some("s1")).as_deref(), Some("ours"));
        // Active but unbound, or with no stream key known: still skipped.
        let list = [bcast("stale", "live", None), bcast("next", "created", None)];
        assert_eq!(preferred_broadcast(&list, None).as_deref(), Some("next"));
        let list = [bcast("starting", "liveStarting", Some("other")), bcast("next", "ready", None)];
        assert_eq!(preferred_broadcast(&list, Some("s1")).as_deref(), Some("next"));
    }

    #[test]
    fn default_prefers_live_bound_to_our_stream() {
        let list = [
            bcast("next-week", "ready", Some("s1")),
            bcast("foreign", "live", Some("other")),
            bcast("ours", "live", Some("s1")),
        ];
        assert_eq!(preferred_broadcast(&list, Some("s1")).as_deref(), Some("ours"));
        let list = [bcast("next-week", "ready", None), bcast("ours", "testing", Some("s1"))];
        assert_eq!(preferred_broadcast(&list, Some("s1")).as_deref(), Some("ours"));
    }

    #[test]
    fn default_none_when_only_foreign_active() {
        let list = [bcast("a", "testing", Some("other")), bcast("b", "live", None)];
        assert_eq!(preferred_broadcast(&list, Some("s1")), None);
        assert_eq!(preferred_broadcast(&list, None), None);
        assert_eq!(preferred_broadcast(&[], Some("s1")), None);
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
        let r = prepare(&d, &c, "b", &mut levels, p, w, |_| {}).await;
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
        let fades: Vec<usize> =
            (0..calls.len()).filter(|&i| matches!(calls[i], Call::StreamSetAudioLevel(..))).collect();
        assert_eq!(fades.len(), 20);
        assert!(fades.iter().all(|&i| i > auto && i < enc));
        assert_eq!(calls[*fades.last().unwrap()], Call::StreamSetAudioLevel(MIC, FLOOR_DB));
        assert_eq!(calls.len(), 3 + 20 + 2);
        assert!(warns.lock().unwrap().is_empty());
        assert_eq!(progress.lock().unwrap().last(), Some(&Readiness::Ready));
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_fades_to_floor_whatever_duck_db() {
        let (d, log, h) = fake_devices();
        let mut c = Config::default();
        c.stream_atem.duck_db = -20.0;
        h.stream.set_audio_level_state(MIC, -3.0);
        let mut levels = LiveLevels::default();
        prepare(&d, &c, "b", &mut levels, |_| {}, |_| {}, |_| {}).await;
        assert!(log.calls().contains(&Call::StreamSetAudioLevel(MIC, FLOOR_DB)), "{:?}", log.calls());
        assert_eq!(levels.0[&MIC], -3.0);
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_skips_testing_if_already_testing() {
        for lc in ["testing", "live"] {
            let (d, log, h) = fake_devices();
            h.yt.set_life_cycle(lc);
            let mut levels = LiveLevels::default();
            let r = prepare(&d, &Config::default(), "b", &mut levels, |_| {}, |_| {}, |_| {}).await;
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
        let r = prepare(&d, &Config::default(), "b", &mut levels, p, w, |_| {}).await;
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
        let r = prepare(&d, &Config::default(), "b", &mut levels, |_| {}, |_| {}, |_| {}).await;
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
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live { public: true, mics_failed: false });
        assert_eq!(run.stages.lock().unwrap().last(), Some(&Stage::Live));
        assert_t0_tail(&log.calls());
        assert!(run.warns.lock().unwrap().is_empty(), "{:?}", run.warns.lock().unwrap());
        // Already public at T-10: T-0 only cuts.
        assert_eq!(live_transitions(&log), 1);
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
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live { public: true, mics_failed: false });
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
        assert!(matches!(run.stages.lock().unwrap().last(), Some(Stage::Countdown { public: false, .. })));
        assert!(!run.stages.lock().unwrap().iter().any(|s| matches!(s, Stage::Countdown { public: true, .. })));
        // T-0 retries going public alongside the cut (v1.1); that fails too.
        h.yt.fail_times("transition", 4);
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live { public: false, mics_failed: false });
        assert_eq!(live_transitions(&log), 12);
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
        let fades: Vec<usize> =
            (0..calls.len()).filter(|&i| matches!(calls[i], Call::StreamSetAudioLevel(..))).collect();
        assert_eq!(fades.len(), 20);
        assert_eq!(calls[0], Call::StreamAuto);
        assert_eq!(calls[*fades.last().unwrap()], Call::StreamSetAudioLevel(MIC, -3.0));
        // The hall switches alongside the 1 s mic fade, not after it.
        let hall = pos(&calls, &Call::ScreensAuto);
        assert!(pos(&calls, &Call::ScreensSetPreview(6)) < hall);
        assert!(hall < fades[1], "the hall waited for the stream audio fade: {calls:?}");
        assert_eq!(calls.last(), Some(&Call::CounterClear));
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

    /// A countdown aborted part-way (a simulated crash) takes its go-public with it.
    #[tokio::test(start_paused = true)]
    async fn aborted_countdown_aborts_its_go_public() {
        let (d, log, h) = fake_devices();
        h.yt.fail_times("transition", 4);
        h.player.set_duration(Duration::from_secs(5));
        h.player.set_position(Some(Duration::ZERO));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(MS(500)).await;
        assert_eq!(live_transitions(&log), 1);
        run.task.abort();
        sleep(Duration::from_secs(10)).await;
        assert_eq!(live_transitions(&log), 1, "no go-public retry ran after the abort");
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

    fn after_play(calls: &[Call]) -> Vec<Call> {
        let play = pos(calls, &Call::PlayerPlay);
        calls[play + 1..].iter().filter(|c| !matches!(c, Call::CounterShow(_))).cloned().collect()
    }

    const RECOVER: [Call; 4] = [Call::ScreensSetPreview(6), Call::ScreensAuto, Call::CounterClear, Call::PlayerStop];

    #[tokio::test(start_paused = true)]
    async fn video_error_returns_hall_to_t0() {
        let (d, log, h) = fake_devices();
        h.player.set_position(Some(Duration::ZERO));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(MS(300)).await;
        h.player.emit(PlayerEvent::Error("decoder died".into()));
        assert!(run.task.await.unwrap().is_err());
        let calls = log.calls();
        assert_eq!(after_play(&calls), RECOVER);
        assert!(yt_calls(&calls).is_empty());
        assert!(!calls.contains(&Call::StreamAuto));
        let warns = run.warns.lock().unwrap();
        assert!(warns.iter().any(|w| w.contains("decoder died") && w.contains("Go Live")), "{warns:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn play_failure_returns_hall_to_t0() {
        let (d, log, h) = fake_devices();
        h.player.fail_next("play");
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        assert!(run.task.await.unwrap().is_err());
        let calls = log.calls();
        assert_eq!(
            calls[..4],
            [
                Call::PlayerLoad(PathBuf::from("countdown.mp4"), 0.0),
                Call::ScreensSetPreview(8),
                Call::ScreensAuto,
                Call::PlayerPlay,
            ]
        );
        assert_eq!(calls[4..], RECOVER);
        assert!(run.warns.lock().unwrap().iter().any(|w| w.contains("Go Live")));
    }

    #[tokio::test(start_paused = true)]
    async fn video_error_reports_pending_go_public() {
        // go_public is mid-retry when the video dies: it is awaited and its result reported.
        let (d, log, h) = fake_devices();
        h.yt.fail_times("transition", 2);
        h.player.set_duration(Duration::from_secs(5));
        h.player.set_position(Some(Duration::ZERO));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(MS(500)).await;
        assert_eq!(live_transitions(&log), 1);
        h.player.emit(PlayerEvent::Error("decoder died".into()));
        let start = Instant::now();
        assert!(run.task.await.unwrap().is_err());
        assert!(start.elapsed() >= Duration::from_secs(3), "returned before go public finished");
        assert_eq!(live_transitions(&log), 3);
        assert!(matches!(run.stages.lock().unwrap().last(), Some(Stage::Countdown { public: true, .. })));
        assert_eq!(
            after_play(&log.calls())
                .iter()
                .filter(|c| !matches!(c, Call::YtTransition(_)))
                .cloned()
                .collect::<Vec<_>>(),
            RECOVER
        );
        sleep(Duration::from_secs(10)).await;
        assert_eq!(live_transitions(&log), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn live_reported_immediately_even_if_go_public_hangs() {
        let (d, log, h) = fake_devices();
        h.yt.set_transition_hangs(true);
        h.player.set_position(Some(MS(50_000)));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(MS(300)).await;
        assert_eq!(live_transitions(&log), 1);
        h.player.emit(PlayerEvent::Ended);
        // t0 includes the 1 s mic fade; LIVE must follow it, not wait for YouTube.
        sleep(MS(1_500)).await;
        assert_eq!(run.stages.lock().unwrap().last(), Some(&Stage::Live));
        assert_t0_tail(&log.calls());
        // The hung go-public is given a bounded wait, then reported.
        let start = Instant::now();
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live { public: false, mics_failed: false });
        assert!(start.elapsed() <= GO_PUBLIC_WAIT);
        let warns = run.warns.lock().unwrap();
        assert!(warns.iter().any(|w| w.contains("public")), "{warns:?}");
        // The in-flight attempt was not duplicated at T-0.
        assert_eq!(live_transitions(&log), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn t0_reports_on_air_when_only_later_steps_fail() {
        let (d, _log, h) = fake_devices();
        h.counter.fail_next("clear");
        let e = t0(&d, &Config::default(), &LiveLevels::default()).await.unwrap_err();
        assert!(e.on_air, "{e:?}");
        assert_eq!(e.failures.len(), 1, "{e:?}");
        assert!(e.failures[0].contains("counter"), "{e:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn t0_stream_cut_failure_is_not_on_air() {
        let (d, log, h) = fake_devices();
        h.stream.fail_next("auto");
        let e = t0(&d, &Config::default(), &LiveLevels::default()).await.unwrap_err();
        assert!(!e.on_air, "{e:?}");
        // Mics stay down; the hall and the counter are still handled.
        assert!(!log.calls().iter().any(|c| matches!(c, Call::StreamSetAudioLevel(..))));
        assert!(log.calls().contains(&Call::CounterClear));
    }

    // ---- v1.1: Start service now ----

    #[tokio::test(start_paused = true)]
    async fn start_service_goes_public_alongside_the_cut() {
        let (d, log, _h) = fake_devices();
        let (notes, n) = sink();
        let out = start_service_reporting(&d, &Config::default(), "b", false, &LiveLevels::default(), n, |_| {}).await;
        assert_eq!(out.t0, Ok(()));
        assert!(matches!(out.public, Some(Ok(()))), "{:?}", out.public);
        let calls = log.calls();
        assert_eq!(live_transitions(&log), 1);
        // Going public started before T-0 had finished (the counter is cleared last).
        assert!(pos(&calls, &Call::YtTransition(Phase::Live)) < pos(&calls, &Call::CounterClear), "{calls:?}");
        assert_t0_tail(&calls);
        let notes = notes.lock().unwrap();
        assert!(notes.iter().any(|m| m.starts_with("YouTube public (took ")), "{notes:?}");
        assert!(notes.iter().any(|m| m.starts_with("Live · Cam ")), "{notes:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn start_service_when_already_public_only_cuts() {
        let (d, log, _h) = fake_devices();
        let out = start_service(&d, &Config::default(), "b", true, &LiveLevels::default()).await;
        assert_eq!(out.t0, Ok(()));
        assert!(out.public.is_none(), "{:?}", out.public);
        assert_eq!(live_transitions(&log), 0);
        assert_t0_tail(&log.calls());
    }

    #[tokio::test(start_paused = true)]
    async fn start_service_cut_never_waits_for_youtube() {
        let (d, log, h) = fake_devices();
        h.yt.set_transition_hangs(true);
        let start = Instant::now();
        let cut_at: Rec<Duration> = Arc::default();
        let c2 = cut_at.clone();
        let out = start_service_reporting(
            &d,
            &Config::default(),
            "b",
            false,
            &LiveLevels::default(),
            |_| {},
            |r| {
                assert_eq!(*r, Ok(()));
                c2.lock().unwrap().push(start.elapsed());
            },
        )
        .await;
        let cut = cut_at.lock().unwrap()[0];
        assert!(cut < Duration::from_secs(2), "T-0 waited for YouTube: {cut:?}");
        assert_eq!(out.t0, Ok(()));
        // The hung go-public is bounded by GO_PUBLIC_WAIT and reported as a failure.
        assert!(matches!(out.public, Some(Err(_))), "{:?}", out.public);
        assert!(
            start.elapsed() >= GO_PUBLIC_WAIT && start.elapsed() < GO_PUBLIC_WAIT + MS(500),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(live_transitions(&log), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_t0_retries_public_if_it_failed_at_t10() {
        let (d, log, h) = fake_devices();
        h.yt.fail_times("transition", 4);
        h.player.set_duration(Duration::from_secs(60));
        h.player.set_position(Some(MS(50_000)));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(Duration::from_secs(7)).await;
        assert_eq!(live_transitions(&log), 4, "the go-public at T-10 has failed");
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live { public: true, mics_failed: false });
        assert_eq!(live_transitions(&log), 5, "{:?}", log.calls());
        // The retry ran alongside T-0: it started before T-0 finished.
        let calls = log.calls();
        let retry = calls.iter().rposition(|c| *c == Call::YtTransition(Phase::Live)).unwrap();
        assert_t0_tail(&calls);
        assert!(retry < pos(&calls, &Call::CounterClear), "the retry waited for T-0: {calls:?}");
        assert_eq!(run.stages.lock().unwrap().last(), Some(&Stage::Live));
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_t0_does_not_duplicate_an_in_flight_go_public() {
        let (d, log, h) = fake_devices();
        h.yt.fail_times("transition", 2); // mid-retry at T-0
        h.player.set_duration(Duration::from_secs(60));
        h.player.set_position(Some(MS(50_000)));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(MS(500)).await;
        assert_eq!(live_transitions(&log), 1);
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live { public: true, mics_failed: false });
        // The in-flight attempt's own retries only: 3 transitions, no fresh go-public.
        assert_eq!(live_transitions(&log), 3, "{:?}", log.calls());
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_t0_partial_failure_is_live_with_warning() {
        let (d, _log, h) = fake_devices();
        h.player.set_position(Some(MS(50_000)));
        let run = spawn_countdown(&d, &Config::default(), Readiness::Ready, LiveLevels::default());
        sleep(MS(300)).await;
        h.counter.fail_next("clear");
        h.player.emit(PlayerEvent::Ended);
        assert_eq!(run.task.await.unwrap().unwrap(), CountdownOutcome::Live { public: true, mics_failed: false });
        assert_eq!(run.stages.lock().unwrap().last(), Some(&Stage::Live));
        let warns = run.warns.lock().unwrap();
        assert!(warns.iter().any(|w| w.contains("counter")), "{warns:?}");
    }
}
