//! The engine: the one task that takes every button press, runs the sequencers,
//! enforces which presses are allowed when, and publishes a `Snapshot`.
//!
//! Busy rules: only one of Prepare / Countdown / End Service / a roll-in runs at a
//! time (the "job"). The job runs as its own task; the engine loop never waits on
//! it, so Stop Countdown, Stop Video, Unmute, Go Public, Go Live, End Broadcast and
//! Recover are always accepted (the last four also run as their own tasks).
//! Every ignored command is logged at `info`.
use crate::audio::LiveLevels;
use crate::config::Config;
use crate::devices::{self, BroadcastCheck, BroadcastSummary, Devices, Fix, Phase};
use crate::end::{EndOutcome, end_service};
use crate::golive::{self, CountdownOutcome, Readiness, Stage, Stage0Report};
use crate::recovery::{self, Assessment, Offer};
use crate::rollin::{self, Cue, CueList, CueState, Destination, RollinOutcome};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// How long an ignored cue key flashes.
pub const FLASH_FOR: Duration = Duration::from_secs(1);
/// Warning shown when a sequence runs with no broadcast selected.
pub const NO_BROADCAST: &str = "No broadcast selected";

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Prepare,
    Countdown,
    StopCountdown,
    GoPublic,
    GoLive,
    EndService,
    EndBroadcast,
    PlayCue(usize),
    StopVideo,
    UnmuteMics,
    Recover,
    SelectBroadcast(String),
    FixBroadcast(Fix),
    Refresh,
    DeviceHealth(DeviceKind, DeviceHealth),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceKind {
    StreamAtem,
    ScreensAtem,
    VideoHub,
    WebPresenter,
    YouTube,
    DeckLink,
    Ndi,
    StreamDeck,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceHealth {
    Ok,
    Warning(String),
    Down(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub stage: Stage,
    pub broadcast: Option<BroadcastSummary>,
    pub check: Option<BroadcastCheck>,
    pub cues: Vec<(Cue, CueState)>,
    pub playing: Option<usize>,
    pub mics_ducked: bool,
    pub offers: Vec<Offer>,
    pub device_status: BTreeMap<DeviceKind, DeviceHealth>,
    pub last_warning: Option<String>,
    pub flash: Option<usize>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            stage: Stage::Idle,
            broadcast: None,
            check: None,
            cues: Vec::new(),
            playing: None,
            mics_ducked: false,
            offers: Vec::new(),
            device_status: BTreeMap::new(),
            last_warning: None,
            flash: None,
        }
    }
}

pub struct EngineHandle {
    pub tx: mpsc::Sender<Command>,
    pub rx: watch::Receiver<Snapshot>,
}

/// The snapshot, shared with the sequence tasks so they can publish progress.
type Shared = Arc<watch::Sender<Snapshot>>;

/// The sequence that makes the engine busy.
enum Job {
    Prepare,
    Countdown { cancel: CancellationToken },
    End { skip: Arc<Notify> },
    Video { index: usize, stop: CancellationToken, unmute: Arc<Notify> },
}

impl Job {
    fn name(&self) -> &'static str {
        match self {
            Job::Prepare => "Prepare",
            Job::Countdown { .. } => "the countdown",
            Job::End { .. } => "End Service",
            Job::Video { .. } => "a video",
        }
    }
}

struct Running {
    job: Job,
    handle: JoinHandle<Done>,
}

/// What a finished job hands back to the engine loop.
enum Done {
    Prepared(Readiness),
    Countdown { result: devices::Result<CountdownOutcome>, public: bool, before: Stage },
    Ended(EndOutcome),
    Video { index: usize, outcome: RollinOutcome },
}

struct Refreshed {
    report: Stage0Report,
    selected: Option<String>,
    check: Option<BroadcastCheck>,
    assessment: Assessment,
}

/// Results of side tasks that change engine state, applied by the loop.
enum Aux {
    Checked { id: String, check: Result<BroadcastCheck, String> },
    Refreshed(Box<Refreshed>),
    Assessed(Assessment),
    ClearFlash(u64),
}

enum Wake {
    Cmd(Command),
    Done(Result<Done, JoinError>),
    Aux(Aux),
}

pub struct Engine {
    d: Devices,
    config: Arc<RwLock<Config>>,
    snap: Shared,
    /// The mics' live levels. Each sequence works on a copy and writes it back,
    /// so the lock is never held across a device call.
    levels: Arc<Mutex<LiveLevels>>,
    cues: CueList,
    broadcasts: Vec<BroadcastSummary>,
    running: Option<Running>,
    /// End Service was pressed during a video: run it once the video has returned.
    end_after_video: bool,
    flash_gen: u64,
    aux: mpsc::UnboundedSender<Aux>,
}

impl Engine {
    /// Start the engine. It first runs `stage0` and `assess` and publishes the
    /// result; commands sent meanwhile wait in the channel.
    pub fn spawn(d: Devices, c: Arc<RwLock<Config>>) -> EngineHandle {
        let (tx, rx) = mpsc::channel(32);
        let (aux, aux_rx) = mpsc::unbounded_channel();
        let cues = CueList::from_cues(read(&c).cues);
        let first = Snapshot { cues: cue_rows(&cues), ..Snapshot::default() };
        let (stx, srx) = watch::channel(first);
        let engine = Engine {
            d,
            config: c,
            snap: Arc::new(stx),
            levels: Arc::default(),
            cues,
            broadcasts: Vec::new(),
            running: None,
            end_after_video: false,
            flash_gen: 0,
            aux,
        };
        tokio::spawn(engine.run(rx, aux_rx));
        EngineHandle { tx, rx: srx }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Command>, mut aux_rx: mpsc::UnboundedReceiver<Aux>) {
        let r = refresh(self.d.clone(), self.config(), None, false).await;
        self.apply_refresh(r);
        loop {
            let wake = tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(c) => Wake::Cmd(c),
                    None => break,
                },
                done = async {
                    match self.running.as_mut() {
                        Some(r) => (&mut r.handle).await,
                        None => std::future::pending().await,
                    }
                } => Wake::Done(done),
                Some(a) = aux_rx.recv() => Wake::Aux(a),
            };
            match wake {
                Wake::Cmd(c) => self.handle(c),
                Wake::Done(d) => self.finish(d),
                Wake::Aux(a) => self.apply_aux(a),
            }
        }
        info!("engine stopped: the command channel closed");
    }

    // ---- helpers ----

    fn config(&self) -> Config {
        read(&self.config)
    }

    fn set(&self, f: impl FnOnce(&mut Snapshot)) {
        self.snap.send_modify(f);
    }

    fn stage(&self) -> Stage {
        self.snap.borrow().stage.clone()
    }

    fn selected(&self) -> Option<String> {
        self.snap.borrow().broadcast.as_ref().map(|b| b.id.clone())
    }

    /// The selected broadcast id, or "" with a warning when none is selected
    /// (YouTube errors then become warnings and the local steps still run).
    fn bid(&self) -> String {
        let id = self.selected();
        id.unwrap_or_else(|| {
            set_warning(&self.snap, NO_BROADCAST.into());
            String::new()
        })
    }

    fn busy(&self) -> Option<String> {
        self.running.as_ref().map(|r| format!("{} is running", r.job.name()))
    }

    fn ignore(&self, cmd: &Command, why: &str) {
        info!(command = ?cmd, reason = why, "command ignored");
    }

    fn summary_for(&self, id: &str) -> BroadcastSummary {
        self.broadcasts.iter().find(|b| b.id == id).cloned().unwrap_or_else(|| BroadcastSummary {
            id: id.into(),
            title: id.into(),
            scheduled_start: None,
            life_cycle: String::new(),
            bound_stream_id: None,
        })
    }

    // ---- commands ----

    fn handle(&mut self, cmd: Command) {
        match &cmd {
            Command::Prepare => {
                if let Some(why) = self.busy() {
                    return self.ignore(&cmd, &why);
                }
                if matches!(self.stage(), Stage::Countdown { .. } | Stage::Live) {
                    return self.ignore(&cmd, "the service is counting down or live");
                }
                self.start_prepare();
            }
            Command::Countdown => {
                if let Some(why) = self.busy() {
                    return self.ignore(&cmd, &why);
                }
                let before = self.stage();
                if matches!(before, Stage::Live | Stage::Ending { .. }) {
                    return self.ignore(&cmd, "the service is live or ending");
                }
                self.start_countdown(before);
            }
            Command::StopCountdown => match &self.running {
                Some(Running { job: Job::Countdown { cancel }, .. }) => cancel.cancel(),
                _ => self.ignore(&cmd, "no countdown is running"),
            },
            Command::EndService => match &self.running {
                None => self.start_end(),
                Some(Running { job: Job::End { skip }, .. }) => {
                    info!("End Service pressed again: skipping the closing hold");
                    skip.notify_one();
                }
                Some(Running { job: Job::Video { stop, .. }, .. }) => {
                    info!("End Service during a video: stopping the video first");
                    stop.cancel();
                    self.end_after_video = true;
                }
                Some(r) => {
                    let why = format!("{} is running", r.job.name());
                    self.ignore(&cmd, &why);
                }
            },
            Command::PlayCue(i) => {
                let i = *i;
                if let Some(why) = self.busy() {
                    self.flash(i);
                    return self.ignore(&cmd, &why);
                }
                if i >= self.cues.len() {
                    return self.ignore(&cmd, "there is no such cue");
                }
                self.start_video(i);
            }
            Command::StopVideo => match &self.running {
                Some(Running { job: Job::Video { stop, .. }, .. }) => stop.cancel(),
                _ => self.ignore(&cmd, "no video is playing"),
            },
            Command::UnmuteMics => match &self.running {
                Some(Running { job: Job::Video { unmute, .. }, .. }) => {
                    unmute.notify_one();
                    self.set(|s| s.mics_ducked = false);
                }
                _ => self.ignore(&cmd, "no video is playing"),
            },
            Command::GoPublic => self.spawn_go_public(),
            Command::GoLive => self.spawn_go_live(),
            Command::EndBroadcast => self.spawn_end_broadcast(),
            Command::Recover => self.spawn_recover(),
            Command::SelectBroadcast(id) => self.spawn_check(id.clone(), None),
            Command::FixBroadcast(fix) => match self.selected() {
                Some(id) => self.spawn_check(id, Some(*fix)),
                None => {
                    set_warning(&self.snap, NO_BROADCAST.into());
                    self.ignore(&cmd, NO_BROADCAST);
                }
            },
            Command::Refresh => {
                let (d, c, sel, aux) = (self.d.clone(), self.config(), self.selected(), self.aux.clone());
                let player_active = matches!(self.running, Some(Running { job: Job::Video { .. }, .. }));
                tokio::spawn(async move {
                    let r = refresh(d, c, sel, player_active).await;
                    let _ = aux.send(Aux::Refreshed(Box::new(r)));
                });
            }
            Command::DeviceHealth(kind, health) => {
                let (kind, health) = (*kind, health.clone());
                self.set(|s| {
                    s.device_status.insert(kind, health);
                });
            }
        }
    }

    fn flash(&mut self, i: usize) {
        self.flash_gen += 1;
        let generation = self.flash_gen;
        self.set(|s| s.flash = Some(i));
        let aux = self.aux.clone();
        tokio::spawn(async move {
            tokio::time::sleep(FLASH_FOR).await;
            let _ = aux.send(Aux::ClearFlash(generation));
        });
    }

    // ---- busy jobs ----

    fn start_prepare(&mut self) {
        let (d, c, snap, levels, bid) = (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone(), self.bid());
        let handle = tokio::spawn(async move {
            let mut l = lock(&levels).clone();
            let (p, w) = (snap.clone(), snap);
            let r = golive::prepare(
                &d,
                &c,
                &bid,
                &mut l,
                move |r| p.send_modify(|s| s.stage = Stage::Prepared(r)),
                move |m| set_warning(&w, m),
            )
            .await;
            *lock(&levels) = l;
            Done::Prepared(r)
        });
        self.running = Some(Running { job: Job::Prepare, handle });
    }

    fn start_countdown(&mut self, before: Stage) {
        let ready = match &before {
            Stage::Prepared(r) => r.clone(),
            _ => Readiness::Warning("Prepare has not been run".into()),
        };
        let cancel = CancellationToken::new();
        let (d, c, snap, levels, bid, tok) =
            (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone(), self.bid(), cancel.clone());
        let handle = tokio::spawn(async move {
            let l = lock(&levels).clone();
            let public = Arc::new(AtomicBool::new(false));
            let (p, w, pf) = (snap.clone(), snap, public.clone());
            let result = golive::countdown(
                &d,
                &c,
                &bid,
                &ready,
                &l,
                tok,
                move |st| {
                    if matches!(st, Stage::Countdown { public: true, .. }) {
                        pf.store(true, Ordering::SeqCst);
                    }
                    p.send_modify(|s| s.stage = st);
                },
                move |m| set_warning(&w, m),
            )
            .await;
            Done::Countdown { result, public: public.load(Ordering::SeqCst), before }
        });
        self.running = Some(Running { job: Job::Countdown { cancel }, handle });
    }

    fn start_end(&mut self) {
        let skip = Arc::new(Notify::new());
        let (d, c, snap, bid, s2) = (self.d.clone(), self.config(), self.snap.clone(), self.bid(), skip.clone());
        let handle = tokio::spawn(async move {
            let out = end_service(&d, &c, &bid, s2, move |st| snap.send_modify(|s| s.stage = st)).await;
            Done::Ended(out)
        });
        self.running = Some(Running { job: Job::End { skip }, handle });
    }

    fn start_video(&mut self, index: usize) {
        let Some(cue) = self.cues.get(index).cloned() else { return };
        self.cues.state[index] = CueState::Playing;
        let ducks = cue.destination != Destination::HallOnly;
        let rows = cue_rows(&self.cues);
        self.set(|s| {
            s.cues = rows;
            s.playing = Some(index);
            s.mics_ducked = ducks;
        });
        let (stop, unmute) = (CancellationToken::new(), Arc::new(Notify::new()));
        let (d, c, levels, st, um) = (self.d.clone(), self.config(), self.levels.clone(), stop.clone(), unmute.clone());
        let handle = tokio::spawn(async move {
            let mut l = lock(&levels).clone();
            let outcome = rollin::play(&d, &c, &cue, &mut l, st, um).await;
            *lock(&levels) = l;
            Done::Video { index, outcome }
        });
        self.running = Some(Running { job: Job::Video { index, stop, unmute }, handle });
    }

    fn finish(&mut self, res: Result<Done, JoinError>) {
        let Some(Running { job, .. }) = self.running.take() else { return };
        match res {
            Ok(Done::Prepared(r)) => self.set(|s| s.stage = Stage::Prepared(r)),
            Ok(Done::Countdown { result, public, before }) => self.finish_countdown(result, public, before),
            Ok(Done::Ended(out)) => self.finish_end(out),
            Ok(Done::Video { index, outcome }) => self.finish_video(index, outcome),
            Err(e) => {
                warn!(job = job.name(), error = %e, "sequence task failed");
                let reason = format!("{} stopped unexpectedly: {e}", job.name());
                match job {
                    Job::Video { index, .. } => {
                        self.finish_video(index, RollinOutcome::Failed { recover_needed: true, reason })
                    }
                    _ => set_warning(&self.snap, reason),
                }
            }
        }
    }

    fn finish_countdown(&mut self, result: devices::Result<CountdownOutcome>, public: bool, before: Stage) {
        match result {
            Ok(CountdownOutcome::Live) => self.set(|s| {
                s.stage = Stage::Live;
                remove(&mut s.offers, Offer::GoLive);
            }),
            Ok(CountdownOutcome::Stopped) => self.set(|s| s.stage = before),
            Err(e) => {
                let msg = format!("The countdown failed ({e}); press Go Live manually");
                self.set(|s| {
                    s.stage = Stage::Prepared(Readiness::Warning(msg.clone()));
                    add(&mut s.offers, Offer::GoLive);
                    if !public {
                        add(&mut s.offers, Offer::GoPublic);
                    }
                    s.last_warning = Some(msg);
                });
            }
        }
    }

    fn finish_end(&mut self, out: EndOutcome) {
        self.set(|s| {
            s.stage = Stage::Ended;
            for o in [Offer::GoLive, Offer::GoPublic, Offer::EndBroadcast] {
                remove(&mut s.offers, o);
            }
            if !out.ended_broadcast {
                add(&mut s.offers, Offer::EndBroadcast);
            }
            if !out.warnings.is_empty() {
                s.last_warning = Some(out.warnings.join("; "));
            }
        });
    }

    fn finish_video(&mut self, index: usize, outcome: RollinOutcome) {
        let (state, warning, recover) = match outcome {
            RollinOutcome::Finished | RollinOutcome::Stopped => (CueState::Played, None, false),
            RollinOutcome::Refused(reason) => (CueState::Cued, Some(reason), false),
            RollinOutcome::Failed { recover_needed, reason } => {
                (CueState::Error(reason.clone()), Some(reason), recover_needed)
            }
        };
        if let Some(s) = self.cues.state.get_mut(index) {
            *s = state;
        }
        let rows = cue_rows(&self.cues);
        self.set(|s| {
            s.cues = rows;
            s.playing = None;
            s.mics_ducked = false;
            if let Some(w) = warning {
                s.last_warning = Some(w);
            }
            if recover {
                add(&mut s.offers, Offer::Recover);
            }
        });
        if std::mem::take(&mut self.end_after_video) {
            self.start_end();
        }
    }

    // ---- always-accepted side tasks ----

    fn spawn_go_public(&self) {
        let (d, snap, bid) = (self.d.clone(), self.snap.clone(), self.bid());
        tokio::spawn(async move {
            match golive::go_public(&d, &bid).await {
                Ok(()) => snap.send_modify(|s| remove(&mut s.offers, Offer::GoPublic)),
                Err(e) => set_warning(&snap, format!("Could not make the YouTube broadcast public: {e}")),
            }
        });
    }

    fn spawn_go_live(&self) {
        let (d, c, snap, levels) = (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone());
        tokio::spawn(async move {
            let l = lock(&levels).clone();
            match golive::t0(&d, &c, &l).await {
                Ok(()) => snap.send_modify(|s| {
                    s.stage = Stage::Live;
                    remove(&mut s.offers, Offer::GoLive);
                }),
                Err(e) => set_warning(&snap, format!("Go Live did not complete: {e}")),
            }
        });
    }

    fn spawn_end_broadcast(&self) {
        let (d, snap, bid) = (self.d.clone(), self.snap.clone(), self.bid());
        tokio::spawn(async move {
            match golive::transition_with_retry(&d, &bid, Phase::Complete).await {
                Ok(()) => snap.send_modify(|s| remove(&mut s.offers, Offer::EndBroadcast)),
                Err(e) => set_warning(&snap, format!("Could not end the YouTube broadcast: {e}")),
            }
        });
    }

    fn spawn_recover(&self) {
        let (d, c, snap, levels, aux) =
            (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone(), self.aux.clone());
        let bid = self.selected();
        let player_active = matches!(self.running, Some(Running { job: Job::Video { .. }, .. }));
        tokio::spawn(async move {
            let l = lock(&levels).clone();
            if let Err(e) = recovery::recover(&d, &c, &l).await {
                set_warning(&snap, format!("Recover could not finish: {e}"));
            }
            let a = recovery::assess(&d, &c, bid.as_deref(), player_active).await;
            let _ = aux.send(Aux::Assessed(a));
        });
    }

    /// Optionally apply `fix` to broadcast `id`, then re-check it.
    fn spawn_check(&self, id: String, fix: Option<Fix>) {
        let (d, snap, aux) = (self.d.clone(), self.snap.clone(), self.aux.clone());
        tokio::spawn(async move {
            if let Some(f) = fix
                && let Err(e) = d.yt.fix(&id, f).await
            {
                set_warning(&snap, format!("Could not fix the broadcast: {e}"));
            }
            let check = d.yt.check(&id).await.map_err(|e| e.to_string());
            let _ = aux.send(Aux::Checked { id, check });
        });
    }

    // ---- side-task results ----

    fn apply_aux(&mut self, a: Aux) {
        match a {
            Aux::Checked { id, check } => {
                let b = self.summary_for(&id);
                self.set(|s| {
                    s.broadcast = Some(b);
                    match check {
                        Ok(c) => s.check = Some(c),
                        Err(e) => {
                            s.check = None;
                            s.last_warning = Some(format!("Could not check the broadcast: {e}"));
                        }
                    }
                });
            }
            Aux::Refreshed(r) => self.apply_refresh(*r),
            Aux::Assessed(a) => self.apply_assessment(a),
            Aux::ClearFlash(generation) => {
                if generation == self.flash_gen {
                    self.set(|s| s.flash = None);
                }
            }
        }
    }

    fn apply_refresh(&mut self, r: Refreshed) {
        self.broadcasts = r.report.broadcasts;
        let broadcast = r.selected.as_deref().map(|id| self.summary_for(id));
        let errors = r.report.device_errors.join("; ");
        self.set(|s| {
            s.broadcast = broadcast;
            s.check = r.check;
            if !errors.is_empty() {
                s.last_warning = Some(errors);
            }
        });
        self.apply_assessment(r.assessment);
    }

    /// Offers always follow the assessment; the stage only when no job is running.
    fn apply_assessment(&self, a: Assessment) {
        let busy = self.running.is_some();
        self.set(|s| {
            s.offers = a.offer;
            if !busy {
                s.stage = a.stage;
            }
        });
    }
}

/// Stage 0 plus assessment. Keeps `current` selected if it is still upcoming.
async fn refresh(d: Devices, c: Config, current: Option<String>, player_active: bool) -> Refreshed {
    let mut report = golive::stage0(&d).await;
    let selected = current
        .filter(|id| report.broadcasts.iter().any(|b| &b.id == id))
        .or_else(|| report.selected.clone());
    let check = match &selected {
        Some(id) if selected != report.selected => match d.yt.check(id).await {
            Ok(c) => Some(c),
            Err(e) => {
                report.device_errors.push(format!("YouTube broadcast check: {e}"));
                None
            }
        },
        _ => report.check,
    };
    let assessment = recovery::assess(&d, &c, selected.as_deref(), player_active).await;
    Refreshed { report, selected, check, assessment }
}

fn read(c: &RwLock<Config>) -> Config {
    c.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn lock(m: &Mutex<LiveLevels>) -> std::sync::MutexGuard<'_, LiveLevels> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_warning(snap: &Shared, msg: String) {
    info!(warning = %msg, "warning");
    snap.send_modify(|s| s.last_warning = Some(msg));
}

fn cue_rows(l: &CueList) -> Vec<(Cue, CueState)> {
    l.cues()
        .iter()
        .enumerate()
        .map(|(i, c)| (c.clone(), l.state.get(i).cloned().unwrap_or(CueState::Cued)))
        .collect()
}

fn add(offers: &mut Vec<Offer>, o: Offer) {
    if !offers.contains(&o) {
        offers.push(o);
    }
}

fn remove(offers: &mut Vec<Offer>, o: Offer) {
    offers.retain(|x| *x != o);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AudioSourceId;
    use crate::devices::{PlayerEvent, Phase};
    use crate::fakes::{Call, CallLog, FakeHandles, fake_devices};
    use crate::golive::Readiness;
    use crate::rollin::Destination;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tokio::time::{sleep, timeout};

    const OUT: u16 = 20;
    const DK: u16 = 30;
    const CAM8: u16 = 5;
    const MIC: AudioSourceId = AudioSourceId { input: 1001, source: -65280 };
    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn cue(name: &str, destination: Destination) -> Cue {
        Cue { path: PathBuf::from(format!("{name}.mp4")), name: name.into(), destination, gain_db: 0.0, return_input: None }
    }

    /// Config with the VideoHub set up for input 8 and the given cues.
    fn cfg(cues: Vec<Cue>) -> Arc<RwLock<Config>> {
        let mut c = Config::default();
        c.videohub.input8_output = OUT;
        c.videohub.decklink_input = DK;
        c.cues = cues;
        Arc::new(RwLock::new(c))
    }

    fn summary(id: &str) -> BroadcastSummary {
        BroadcastSummary {
            id: id.into(),
            title: id.into(),
            scheduled_start: None,
            life_cycle: "ready".into(),
            bound_stream_id: None,
        }
    }

    /// Fakes as they stand before a service: one upcoming broadcast, Cam 8 on input 8, mics at -3 dB.
    fn rig() -> (Devices, CallLog, FakeHandles) {
        let (d, log, h) = fake_devices();
        h.yt.set_upcoming(vec![summary("b1")]);
        h.hub.set_route(OUT, CAM8);
        h.stream.set_audio_level_state(MIC, -3.0);
        (d, log, h)
    }

    async fn send(e: &EngineHandle, c: Command) {
        e.tx.send(c).await.unwrap();
    }

    async fn until(e: &mut EngineHandle, what: &str, pred: impl FnMut(&Snapshot) -> bool) -> Snapshot {
        let got = match timeout(Duration::from_secs(120), e.rx.wait_for(pred)).await {
            Ok(Ok(s)) => Some(s.clone()),
            _ => None,
        };
        got.unwrap_or_else(|| panic!("snapshot never showed {what}: {:?}", *e.rx.borrow()))
    }

    async fn until_call(log: &CallLog, call: Call) {
        for _ in 0..2000 {
            if log.calls().contains(&call) {
                return;
            }
            sleep(MS(10)).await;
        }
        panic!("{call:?} never happened: {:?}", log.calls());
    }

    fn count(log: &CallLog, pred: impl Fn(&Call) -> bool) -> usize {
        log.calls().iter().filter(|c| pred(c)).count()
    }

    fn pos(calls: &[Call], c: &Call) -> usize {
        calls.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c:?} not in {calls:?}"))
    }

    fn snap(e: &EngineHandle) -> Snapshot {
        e.rx.borrow().clone()
    }

    #[tokio::test(start_paused = true)]
    async fn double_countdown_ignored() {
        let (d, log, _h) = rig();
        let e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until_call(&log, Call::PlayerPlay).await;
        send(&e, Command::Countdown).await;
        sleep(MS(500)).await;
        assert_eq!(count(&log, |c| matches!(c, Call::PlayerLoad(..))), 1, "{:?}", log.calls());
        assert!(matches!(snap(&e).stage, Stage::Countdown { .. }), "{:?}", snap(&e).stage);
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_while_live_ignored() {
        let (d, log, h) = rig();
        // YouTube live and the stream on a live input: startup assesses Live.
        h.yt.set_life_cycle("live");
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        send(&e, Command::Prepare).await;
        sleep(MS(500)).await;
        assert!(log.calls().is_empty(), "{:?}", log.calls());
        assert_eq!(snap(&e).stage, Stage::Live);
    }

    #[tokio::test(start_paused = true)]
    async fn play_while_playing_flashes_and_is_ignored() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallAndOnline), cue("b", Destination::HallOnly)]));
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        send(&e, Command::PlayCue(1)).await;
        let s = until(&mut e, "flash", |s| s.flash == Some(1)).await;
        assert_eq!(s.playing, Some(0));
        assert_eq!(s.cues[0].1, CueState::Playing);
        assert_eq!(s.cues[1].1, CueState::Cued);
        assert_eq!(count(&log, |c| matches!(c, Call::PlayerLoad(..))), 1);
        sleep(MS(500)).await;
        assert_eq!(snap(&e).flash, Some(1));
        sleep(MS(600)).await;
        assert_eq!(snap(&e).flash, None);
        assert_eq!(count(&log, |c| matches!(c, Call::PlayerLoad(..))), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn end_during_video_stops_video_first() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallAndOnline)]));
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        send(&e, Command::EndService).await;
        let s = until(&mut e, "Ended", |s| s.stage == Stage::Ended).await;
        let calls = log.calls();
        let stop = pos(&calls, &Call::PlayerStop);
        let restore = pos(&calls, &Call::HubRoute { output: OUT, input: CAM8 });
        let closing = pos(&calls, &Call::StreamSetMediaStill(1, 1));
        assert!(stop < restore && restore < closing, "{calls:?}");
        assert_eq!(calls.last(), Some(&Call::EncoderStop));
        assert_eq!(s.cues[0].1, CueState::Played);
        assert_eq!(s.playing, None);
    }

    #[tokio::test(start_paused = true)]
    async fn unmute_accepted_while_busy() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallAndOnline)]));
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        assert!(until(&mut e, "ducked", |s| s.mics_ducked).await.mics_ducked);
        sleep(MS(1500)).await; // the fade down is done
        send(&e, Command::UnmuteMics).await;
        until(&mut e, "unducked", |s| !s.mics_ducked).await;
        sleep(MS(1500)).await;
        let calls = log.calls();
        let last_mic = calls.iter().rev().find_map(|c| match c {
            Call::StreamSetAudioLevel(m, db) if *m == MIC => Some(*db),
            _ => None,
        });
        assert_eq!(last_mic, Some(-3.0), "{calls:?}");
        assert!(!calls.contains(&Call::PlayerStop), "video kept playing");
        assert_eq!(snap(&e).playing, Some(0));
    }

    #[tokio::test(start_paused = true)]
    async fn snapshot_published_after_each_stage_change() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        let seen: Arc<Mutex<Vec<Stage>>> = Arc::default();
        let (seen2, mut rx) = (seen.clone(), e.rx.clone());
        tokio::spawn(async move {
            loop {
                seen2.lock().unwrap().push(rx.borrow_and_update().stage.clone());
                if rx.changed().await.is_err() {
                    break;
                }
            }
        });
        send(&e, Command::Prepare).await;
        until(&mut e, "Prepared(Ready)", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.player.emit(PlayerEvent::Ended);
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        send(&e, Command::EndService).await;
        until(&mut e, "Ended", |s| s.stage == Stage::Ended).await;
        sleep(MS(10)).await;

        let seen = seen.lock().unwrap();
        let want: [fn(&Stage) -> bool; 6] = [
            |s| *s == Stage::Idle,
            |s| *s == Stage::Prepared(Readiness::Ready),
            |s| matches!(s, Stage::Countdown { .. }),
            |s| *s == Stage::Live,
            |s| matches!(s, Stage::Ending { .. }),
            |s| *s == Stage::Ended,
        ];
        let mut it = seen.iter();
        for (i, w) in want.iter().enumerate() {
            assert!(it.any(w), "stage #{i} missing or out of order in {seen:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_failure_offers_go_live() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.player.emit(PlayerEvent::Error("decode error".into()));
        let s = until(&mut e, "Prepared", |s| matches!(s.stage, Stage::Prepared(_))).await;
        assert!(s.offers.contains(&Offer::GoLive), "{:?}", s.offers);
        assert!(s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
        assert!(s.last_warning.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn stop_countdown_returns_to_previous_stage() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Prepare).await;
        until(&mut e, "Prepared(Ready)", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        send(&e, Command::StopCountdown).await;
        let s = until(&mut e, "Prepared again", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
        assert!(s.offers.is_empty(), "{:?}", s.offers);
        assert!(log.calls().contains(&Call::PlayerStop));
        // Not busy any more: a new countdown starts.
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        assert_eq!(count(&log, |c| matches!(c, Call::PlayerLoad(..))), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn refused_cue_stays_cued_with_warning() {
        let (d, _log, _h) = rig();
        let c = cfg(vec![cue("a", Destination::HallAndOnline)]);
        c.write().unwrap().videohub.input8_output = 0;
        let mut e = Engine::spawn(d, c);
        send(&e, Command::PlayCue(0)).await;
        let s = until(&mut e, "warning", |s| s.last_warning.is_some()).await;
        assert_eq!(s.last_warning.as_deref(), Some("VideoHub not configured"));
        let s = until(&mut e, "not playing", |s| s.playing.is_none()).await;
        assert_eq!(s.cues[0].1, CueState::Cued);
        assert!(!s.offers.contains(&Offer::Recover));
    }

    #[tokio::test(start_paused = true)]
    async fn failed_cue_shows_error_and_offers_recover() {
        let (d, _log, h) = rig();
        h.player.fail_next("play");
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallAndOnline)]));
        send(&e, Command::PlayCue(0)).await;
        let s = until(&mut e, "cue error", |s| matches!(s.cues[0].1, CueState::Error(_))).await;
        assert!(s.offers.contains(&Offer::Recover), "{:?}", s.offers);
        assert_eq!(s.playing, None);
        assert!(!s.mics_ducked);
    }

    #[tokio::test(start_paused = true)]
    async fn finished_cue_is_played() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallOnly)]));
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        h.player.emit(PlayerEvent::Ended);
        let s = until(&mut e, "played", |s| s.cues[0].1 == CueState::Played).await;
        assert_eq!(s.playing, None);
        assert_eq!(s.last_warning, None);
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_without_broadcast_warns_and_runs_local_steps() {
        let (d, log, h) = rig();
        h.yt.set_upcoming(vec![]);
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Prepare).await;
        let s = until(&mut e, "Prepared", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
        assert_eq!(s.last_warning.as_deref(), Some(NO_BROADCAST));
        assert!(log.calls().contains(&Call::EncoderStart));
    }

    #[tokio::test(start_paused = true)]
    async fn second_end_press_skips_hold() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::EndService).await;
        until(&mut e, "Ending", |s| matches!(s.stage, Stage::Ending { .. })).await;
        let start = tokio::time::Instant::now();
        send(&e, Command::EndService).await;
        until(&mut e, "Ended", |s| s.stage == Stage::Ended).await;
        assert!(start.elapsed() < MS(100), "{:?}", start.elapsed());
        assert_eq!(count(&log, |c| *c == Call::YtTransition(Phase::Complete)), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn always_accepted_commands_run_while_busy() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        send(&e, Command::GoPublic).await;
        until_call(&log, Call::YtTransition(Phase::Live)).await;
        send(&e, Command::EndBroadcast).await;
        until_call(&log, Call::YtTransition(Phase::Complete)).await;
        assert!(matches!(snap(&e).stage, Stage::Countdown { .. }) || snap(&e).stage == Stage::Ended);
    }

    #[tokio::test(start_paused = true)]
    async fn go_live_runs_t0_and_reports_live() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        assert!(log.calls().contains(&Call::StreamAuto));
        assert!(log.calls().contains(&Call::CounterClear));
    }

    #[tokio::test(start_paused = true)]
    async fn device_health_updates_snapshot() {
        let (d, _log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::DeviceHealth(DeviceKind::VideoHub, DeviceHealth::Down("no reply".into()))).await;
        let s = until(&mut e, "status", |s| !s.device_status.is_empty()).await;
        assert_eq!(s.device_status[&DeviceKind::VideoHub], DeviceHealth::Down("no reply".into()));
    }

    #[tokio::test(start_paused = true)]
    async fn select_broadcast_rechecks() {
        let (d, _log, h) = rig();
        h.yt.set_upcoming(vec![summary("b1"), summary("b2")]);
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "b1", |s| s.broadcast.as_ref().is_some_and(|b| b.id == "b1")).await;
        h.yt.set_check(BroadcastCheck { bound_to_ours: false, auto_start: false, auto_stop: true, monitor_stream: true });
        send(&e, Command::SelectBroadcast("b2".into())).await;
        let s = until(&mut e, "b2", |s| s.broadcast.as_ref().is_some_and(|b| b.id == "b2")).await;
        assert_eq!(s.check.map(|c| c.bound_to_ours), Some(false));
    }
}
