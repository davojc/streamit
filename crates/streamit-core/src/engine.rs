//! The engine: the one task that takes every button press, runs the sequencers,
//! enforces which presses are allowed when, and publishes a `Snapshot`.
//!
//! Job slot (R14/R15): one sequence runs at a time — Prepare, the countdown, End
//! Service, a video, Go Live (T-0 by hand) or Recover. It runs as its own task and the
//! engine loop never waits on it. A press that would start a second job is ignored (a
//! cue key, GO LIVE and RECOVER flash), with these exceptions:
//! - Go Live during the countdown stops the countdown, waits for its cancel path, then
//!   runs T-0 (R13). Go Live is refused once Live (unless offered), Ending or Ended (R22).
//!   Go Live is Start service now: T-0 with, unless already public, the go-public run
//!   alongside it in the same job (never before the cut).
//! - Recover and End Service during a video stop it and wait for its return path
//!   (Recover first, then End Service).
//! - End Service pressed again during End Service skips the closing hold.
//!
//! Always accepted, whatever runs: Stop Countdown and Stop Video and Unmute (they act on
//! the running job), Go Public and End Broadcast (each its own short task), Select
//! Broadcast, Fix, Refresh and device health. Every ignored command is logged at `info`.
use crate::audio::LiveLevels;
use crate::config::Config;
use crate::devices::{self, BroadcastCheck, BroadcastSummary, DeviceError, Devices, Fix, Phase};
use crate::end::{EndOutcome, end_service};
use crate::golive::{self, CountdownOutcome, CountdownReport, Readiness, Stage, Stage0Report, T0Error};
use crate::recovery::{self, Assessment, Offer};
use crate::rollin::{self, Cue, CueList, CueState, RollinOutcome, RollinReport};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// How long an ignored cue key flashes.
pub const FLASH_FOR: Duration = Duration::from_secs(1);
/// Longest a startup check or a Refresh may take (a down YouTube otherwise holds the
/// startup check, and every press queued behind it, for its HTTP timeouts).
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);
/// Warning shown when a sequence runs with no broadcast selected.
pub const NO_BROADCAST: &str = "No broadcast selected";
/// Most entries the activity list keeps.
pub const ACTIVITY_MAX: usize = 50;

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
    /// Replace the cue list (after an edit in the window). Ignored with a warning
    /// while a video plays, so the playing index keeps meaning the same cue.
    SetCues(Vec<Cue>),
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
    /// A non-cue key whose press was ignored; cleared after `FLASH_FOR`.
    pub flash_key: Option<FlashKey>,
    /// The sequence running now (Prepare, the countdown, End Service, a video, Go Live,
    /// Recover), if any.
    pub job: Option<&'static str>,
    /// The roll-in playing now, once it has started; updated every 100 ms.
    pub video: Option<VideoProgress>,
    /// The countdown video's length while the countdown runs (`stage` has what remains).
    pub countdown_total: Option<Duration>,
    /// When the service went live; cleared when it has ended.
    pub on_air_since: Option<SystemTime>,
    /// What has happened, in plain words, newest first; at most `ACTIVITY_MAX`.
    pub activity: Vec<Activity>,
    /// The engine drives pretend devices: nothing is live.
    pub simulation: bool,
}

/// How far the playing roll-in is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoProgress {
    /// The cue's index in the list.
    pub index: usize,
    pub position: Duration,
    pub duration: Duration,
}

/// One line of the activity list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub at: SystemTime,
    pub text: String,
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
            flash_key: None,
            job: None,
            video: None,
            countdown_total: None,
            on_air_since: None,
            activity: Vec::new(),
            simulation: false,
        }
    }
}

/// Keys other than cues that flash when their press is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashKey {
    GoLive,
    Recover,
}

pub struct EngineHandle {
    pub tx: mpsc::Sender<Command>,
    pub rx: watch::Receiver<Snapshot>,
}

/// The snapshot, shared with the sequence tasks so they can publish progress.
type Shared = Arc<watch::Sender<Snapshot>>;

/// The one sequence that may run at a time. Every press that starts a sequence
/// is ignored while another runs (`busy`), with the documented exceptions:
/// Go Live pre-empts a countdown, and End Service and Recover during a video
/// stop it and wait for its return path.
enum Job {
    Prepare,
    Countdown { cancel: CancellationToken },
    End { skip: Arc<Notify> },
    Video { index: usize, stop: CancellationToken, unmute: Arc<Notify> },
    /// Start service now by hand: T-0, going public alongside it unless already public.
    GoLive(GoLiveStart),
    Recover,
}

impl Job {
    fn name(&self) -> &'static str {
        match self {
            Job::Prepare => "Prepare",
            Job::Countdown { .. } => "the countdown",
            Job::End { .. } => "End Service",
            Job::Video { .. } => "a video",
            Job::GoLive(_) => "Go Live",
            Job::Recover => "Recover",
        }
    }
}

/// How a Start service now began; its job and its result both carry it.
#[derive(Debug, Clone, Copy)]
struct GoLiveStart {
    /// Set when it pre-empted a countdown: whether that countdown had made the
    /// broadcast public.
    preempt_public: Option<bool>,
    /// The broadcast was known to be public already, so no go-public ran.
    already_public: bool,
    /// No broadcast was selected, so no go-public ran.
    no_broadcast: bool,
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
    /// `public` is the go-public's result (`None` if none ran).
    WentLive { result: Result<(), T0Error>, public: Option<devices::Result<()>>, start: GoLiveStart },
    Recovered { ok: bool, assessment: Assessment },
}

struct Refreshed {
    /// Engine epoch and selection generation when the refresh started.
    epoch: u64,
    sel_gen: u64,
    report: Stage0Report,
    selected: Option<String>,
    check: Option<BroadcastCheck>,
    assessment: Assessment,
}

/// Results of side tasks, applied by the loop.
enum Aux {
    Checked { sel_gen: u64, check: Result<BroadcastCheck, String> },
    Refreshed(Box<Refreshed>),
    ClearFlash(u64),
    ClearKeyFlash(u64),
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
    /// Where the levels are saved whenever they change (I5), so a restart keeps them.
    levels_file: Option<Arc<PathBuf>>,
    cues: CueList,
    broadcasts: Vec<BroadcastSummary>,
    running: Option<Running>,
    /// Bumped when a job starts or finishes. A Refresh assessment started under
    /// an older epoch may not set the stage, and only adds offers.
    epoch: u64,
    /// Bumped on every Select Broadcast; older check and refresh results do not
    /// change the selection.
    sel_gen: u64,
    /// Go Live is waiting for the countdown it pre-empted to run its cancel path.
    golive_pending: bool,
    /// Recover / End Service are waiting for the running video's return path
    /// (Recover first, then End).
    recover_pending: bool,
    end_pending: bool,
    flash_gen: u64,
    key_flash_gen: u64,
    aux: mpsc::UnboundedSender<Aux>,
}

impl Engine {
    /// Start the engine. It first runs `stage0` and `assess` and publishes the
    /// result; commands sent meanwhile wait in the channel.
    pub fn spawn(d: Devices, c: Arc<RwLock<Config>>) -> EngineHandle {
        Self::spawn_with(d, c, LiveLevels::default(), None, false)
    }

    /// `spawn`, starting from `levels` (loaded from disk) and saving them to `levels_file`
    /// whenever a sequence changes them. `simulation` is published in the snapshot: it
    /// says `d` are pretend devices.
    pub fn spawn_with(
        d: Devices,
        c: Arc<RwLock<Config>>,
        levels: LiveLevels,
        levels_file: Option<PathBuf>,
        simulation: bool,
    ) -> EngineHandle {
        let (tx, rx) = mpsc::channel(32);
        let (aux, aux_rx) = mpsc::unbounded_channel();
        let cues = CueList::from_cues(read(&c).cues);
        let first = Snapshot { cues: cue_rows(&cues), simulation, ..Snapshot::default() };
        let (stx, srx) = watch::channel(first);
        let engine = Engine {
            d,
            config: c,
            snap: Arc::new(stx),
            levels: Arc::new(Mutex::new(levels)),
            levels_file: levels_file.map(Arc::new),
            cues,
            broadcasts: Vec::new(),
            running: None,
            epoch: 0,
            sel_gen: 0,
            golive_pending: false,
            recover_pending: false,
            end_pending: false,
            flash_gen: 0,
            key_flash_gen: 0,
            aux,
        };
        tokio::spawn(engine.run(rx, aux_rx));
        EngineHandle { tx, rx: srx }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Command>, mut aux_rx: mpsc::UnboundedReceiver<Aux>) {
        let r = refresh(self.d.clone(), self.config(), None, false, self.epoch, self.sel_gen);
        match tokio::time::timeout(REFRESH_TIMEOUT, r).await {
            Ok(r) => self.apply_refresh(r),
            Err(_) => set_warning(
                &self.snap,
                format!(
                    "The startup check did not finish within {} s; press Refresh to try again",
                    REFRESH_TIMEOUT.as_secs()
                ),
            ),
        }
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

    fn video_running(&self) -> bool {
        matches!(self.running, Some(Running { job: Job::Video { .. }, .. }))
    }

    /// The player is meant to be on the DeckLink: a video or the countdown is running, so
    /// an assessment must not read the DeckLink on air as a crash.
    fn player_active(&self) -> bool {
        matches!(self.running, Some(Running { job: Job::Video { .. } | Job::Countdown { .. }, .. }))
    }

    fn ignore(&self, cmd: &Command, why: &str) {
        info!(command = ?cmd, reason = why, "command ignored");
    }

    /// The summary for `id` from the last list read, else the one the snapshot already
    /// shows for it, else a bare one titled with the id.
    fn summary_for(&self, id: &str) -> BroadcastSummary {
        let shown = self.snap.borrow().broadcast.clone().filter(|b| b.id == id);
        self.broadcasts.iter().find(|b| b.id == id).cloned().or(shown).unwrap_or_else(|| BroadcastSummary {
            id: id.into(),
            title: id.into(),
            scheduled_start: None,
            life_cycle: String::new(),
            bound_stream_id: None,
        })
    }

    fn begin(&mut self, job: Job, handle: JoinHandle<Done>) {
        self.epoch += 1;
        let name = job.name();
        self.set(|s| s.job = Some(name));
        self.running = Some(Running { job, handle });
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
                Some(Running { job: Job::Countdown { cancel }, .. }) if !self.golive_pending => cancel.cancel(),
                Some(Running { job: Job::Countdown { .. }, .. }) => {
                    self.ignore(&cmd, "Go Live is already stopping the countdown")
                }
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
                    self.end_pending = true;
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
            // `mics_ducked` clears only when the roll-in reports a successful fade up.
            Command::UnmuteMics => match &self.running {
                Some(Running { job: Job::Video { unmute, .. }, .. }) => unmute.notify_one(),
                _ => self.ignore(&cmd, "no video is playing"),
            },
            Command::GoPublic => self.spawn_go_public(),
            Command::EndBroadcast => self.spawn_end_broadcast(),
            Command::GoLive => {
                // R22: the deck lets Go Live be pressed in Idle/Prepared/Countdown; the
                // engine decides. Live is refused unless Go Live is offered (live on the
                // holding still, as assessed after a restart).
                let stage = self.stage();
                if matches!(stage, Stage::Ending { .. } | Stage::Ended) {
                    self.flash_key(FlashKey::GoLive);
                    return self.ignore(&cmd, "the service has ended");
                }
                if stage == Stage::Live && !self.snap.borrow().offers.contains(&Offer::GoLive) {
                    self.flash_key(FlashKey::GoLive);
                    return self.ignore(&cmd, "the service is already live");
                }
                match &self.running {
                    None => self.start_golive(None),
                    Some(Running { job: Job::Countdown { cancel }, .. }) if !self.golive_pending => {
                        info!("Go Live during the countdown: stopping the countdown first");
                        cancel.cancel();
                        self.golive_pending = true;
                    }
                    Some(r) => {
                        let why = if self.golive_pending {
                            "Go Live is already in progress".to_string()
                        } else {
                            format!("{} is running", r.job.name())
                        };
                        self.flash_key(FlashKey::GoLive);
                        self.ignore(&cmd, &why);
                    }
                }
            }
            Command::Recover => match &self.running {
                None => self.start_recover(),
                Some(Running { job: Job::Video { stop, .. }, .. }) if !self.recover_pending => {
                    info!("Recover during a video: stopping the video first");
                    stop.cancel();
                    self.recover_pending = true;
                }
                Some(r) => {
                    let why = if self.recover_pending {
                        "Recover is already waiting for the video".to_string()
                    } else {
                        format!("{} is running", r.job.name())
                    };
                    self.flash_key(FlashKey::Recover);
                    self.ignore(&cmd, &why);
                }
            },
            Command::SelectBroadcast(id) => {
                self.sel_gen += 1;
                let b = self.summary_for(id);
                self.set(|s| {
                    s.broadcast = Some(b);
                    s.check = None;
                });
                self.spawn_check(id.clone(), None);
            }
            Command::FixBroadcast(fix) => match self.selected() {
                Some(id) => self.spawn_check(id, Some(*fix)),
                None => {
                    set_warning(&self.snap, NO_BROADCAST.into());
                    self.ignore(&cmd, NO_BROADCAST);
                }
            },
            Command::Refresh => {
                let (d, c, sel, aux, snap) =
                    (self.d.clone(), self.config(), self.selected(), self.aux.clone(), self.snap.clone());
                let (player_active, epoch, sel_gen) = (self.player_active(), self.epoch, self.sel_gen);
                tokio::spawn(async move {
                    let r = refresh(d, c, sel, player_active, epoch, sel_gen);
                    match tokio::time::timeout(REFRESH_TIMEOUT, r).await {
                        Ok(r) => {
                            let _ = aux.send(Aux::Refreshed(Box::new(r)));
                        }
                        Err(_) => set_warning(
                            &snap,
                            format!("Refresh did not finish within {} s", REFRESH_TIMEOUT.as_secs()),
                        ),
                    }
                });
            }
            Command::DeviceHealth(kind, health) => {
                let (kind, health) = (*kind, health.clone());
                self.set(|s| {
                    s.device_status.insert(kind, health);
                });
            }
            Command::SetCues(cues) => {
                if self.video_running() {
                    set_warning(&self.snap, "The cue list can't change while a video plays".into());
                    return self.ignore(&cmd, "a video is playing");
                }
                self.cues = rebuild_cues(&self.cues, cues.clone());
                let rows = cue_rows(&self.cues);
                info!(count = rows.len(), "cue list replaced");
                self.set(|s| {
                    s.cues = rows;
                    s.flash = None;
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

    fn flash_key(&mut self, key: FlashKey) {
        self.key_flash_gen += 1;
        let generation = self.key_flash_gen;
        self.set(|s| s.flash_key = Some(key));
        let aux = self.aux.clone();
        tokio::spawn(async move {
            tokio::time::sleep(FLASH_FOR).await;
            let _ = aux.send(Aux::ClearKeyFlash(generation));
        });
    }

    // ---- jobs ----

    fn start_prepare(&mut self) {
        let (d, c, snap, levels, bid) = (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone(), self.bid());
        let file = self.levels_file.clone();
        let handle = tokio::spawn(async move {
            let mut l = lock(&levels).clone();
            let (p, w, n) = (snap.clone(), snap.clone(), snap);
            let r = golive::prepare(
                &d,
                &c,
                &bid,
                &mut l,
                move |r| p.send_modify(|s| set_prepared(s, r)),
                move |m| set_warning(&w, m),
                move |m| n.send_modify(|s| log(s, m)),
            )
            .await;
            store_levels(&levels, l, file.as_deref());
            Done::Prepared(r)
        });
        self.begin(Job::Prepare, handle);
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
            let (p, w, r, pf) = (snap.clone(), snap.clone(), snap, public.clone());
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
                    p.send_modify(|s| set_stage(s, st));
                },
                move |m| set_warning(&w, m),
                move |report| {
                    r.send_modify(|s| match report {
                        CountdownReport::Started(total) => {
                            s.countdown_total = Some(total);
                            log(s, format!("Countdown started ({})", mss(total)));
                        }
                        CountdownReport::Note(m) => log(s, m),
                    })
                },
            )
            .await;
            Done::Countdown { result, public: public.load(Ordering::SeqCst), before }
        });
        self.begin(Job::Countdown { cancel }, handle);
    }

    fn start_end(&mut self) {
        let skip = Arc::new(Notify::new());
        let (d, c, snap, bid, s2) = (self.d.clone(), self.config(), self.snap.clone(), self.bid(), skip.clone());
        let handle = tokio::spawn(async move {
            let out = end_service(&d, &c, &bid, s2, move |st| snap.send_modify(|s| set_stage(s, st))).await;
            Done::Ended(out)
        });
        self.begin(Job::End { skip }, handle);
    }

    fn start_video(&mut self, index: usize) {
        let Some(cue) = self.cues.get(index).cloned() else { return };
        self.cues.state[index] = CueState::Playing;
        let rows = cue_rows(&self.cues);
        self.set(|s| {
            s.cues = rows;
            s.playing = Some(index);
        });
        let (stop, unmute) = (CancellationToken::new(), Arc::new(Notify::new()));
        let (d, c, snap, levels, st, um) =
            (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone(), stop.clone(), unmute.clone());
        let file = self.levels_file.clone();
        let handle = tokio::spawn(async move {
            let mut l = lock(&levels).clone();
            let name = cue.name.clone();
            let report = move |r: RollinReport| {
                snap.send_modify(|s| match r {
                    RollinReport::MicsDucked(down) => s.mics_ducked = down,
                    RollinReport::Position { position, duration } => {
                        if s.video.is_none() {
                            log(s, format!("Video {} · {name} · started", index + 1));
                        }
                        s.video = Some(VideoProgress { index, position, duration });
                    }
                })
            };
            let outcome = rollin::play_reporting(&d, &c, &cue, &mut l, st, um, report).await;
            store_levels(&levels, l, file.as_deref());
            Done::Video { index, outcome }
        });
        self.begin(Job::Video { index, stop, unmute }, handle);
    }

    /// Start service now (the GO LIVE key, or a pre-empted countdown): the T-0 cut and,
    /// in parallel, going public. The broadcast is known public when the pre-empted
    /// countdown made it so, when already Live (Go Live offered on the holding still:
    /// YouTube is live), or when the selected broadcast was listed as live (live never
    /// goes back to testing). With no broadcast selected only the cut runs, with a
    /// warning and Go Public offered. While its own go-public runs, Go Public is not
    /// offered, so "Retry going public" cannot start a second one alongside it. The job
    /// slot is held until the go-public settles (at most `GO_PUBLIC_WAIT`), so a second
    /// press can never start a second cut; LIVE is shown as soon as the cut is done.
    fn start_golive(&mut self, preempt_public: Option<bool>) {
        let start = self.golive_start(preempt_public);
        let bid = self.selected();
        let going_public = !start.already_public && !start.no_broadcast;
        if going_public {
            self.set(|s| remove(&mut s.offers, Offer::GoPublic));
        }
        let (d, c, levels, snap) = (self.d.clone(), self.config(), self.levels.clone(), self.snap.clone());
        let handle = tokio::spawn(async move {
            let l = lock(&levels).clone();
            let live = snap.clone();
            let out = golive::start_service_reporting(
                &d,
                &c,
                bid.as_deref().unwrap_or_default(),
                !going_public,
                &l,
                |m| snap.send_modify(|s| log(s, m)),
                |r| {
                    if r.as_ref().map_or_else(|e| e.on_air, |()| true) {
                        live.send_modify(|s| {
                            set_stage(s, Stage::Live);
                            remove(&mut s.offers, Offer::GoLive);
                        });
                    }
                },
            )
            .await;
            Done::WentLive { result: out.t0, public: out.public, start }
        });
        self.begin(Job::GoLive(start), handle);
    }

    fn golive_start(&self, preempt_public: Option<bool>) -> GoLiveStart {
        let s = self.snap.borrow();
        let listed_live = s.broadcast.as_ref().is_some_and(|b| b.life_cycle == "live");
        GoLiveStart {
            preempt_public,
            already_public: preempt_public == Some(true) || s.stage == Stage::Live || listed_live,
            no_broadcast: s.broadcast.is_none(),
        }
    }

    fn start_recover(&mut self) {
        let (d, c, snap, levels, bid) = (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone(), self.selected());
        let handle = tokio::spawn(async move {
            let l = lock(&levels).clone();
            let ok = match recovery::recover(&d, &c, &l).await {
                Ok(()) => true,
                Err(e) => {
                    set_warning(&snap, format!("Recover could not finish: {e}"));
                    false
                }
            };
            // Recover never runs alongside a video, so no player is active.
            let assessment = recovery::assess(&d, &c, bid.as_deref(), false).await;
            Done::Recovered { ok, assessment }
        });
        self.begin(Job::Recover, handle);
    }

    fn finish(&mut self, res: Result<Done, JoinError>) {
        let Some(Running { job, .. }) = self.running.take() else { return };
        self.epoch += 1;
        let countdown = matches!(job, Job::Countdown { .. });
        self.set(|s| {
            s.job = None;
            if countdown {
                s.countdown_total = None;
            }
        });
        match res {
            Ok(Done::Prepared(r)) => self.set(|s| set_prepared(s, r)),
            Ok(Done::Countdown { result, public, before }) => self.finish_countdown(result, public, before),
            Ok(Done::Ended(out)) => self.finish_end(out),
            Ok(Done::Video { index, outcome }) => self.finish_video(index, outcome),
            Ok(Done::WentLive { result, public, start }) => self.finish_golive(result, public, start),
            Ok(Done::Recovered { ok, assessment }) => {
                if ok {
                    self.set(|s| {
                        s.mics_ducked = false;
                        log(s, "Recover finished".into());
                    });
                }
                self.apply_assessment(assessment, self.epoch);
            }
            Err(e) => {
                warn!(job = job.name(), error = %e, "sequence task failed");
                let reason = format!("{} stopped unexpectedly: {e}", job.name());
                match job {
                    Job::Video { index, .. } => {
                        self.finish_video(index, RollinOutcome::Failed { recover_needed: true, reason })
                    }
                    // Handled like a countdown error: back to Prepared, Go Live / Go Public offered.
                    Job::Countdown { .. } => {
                        self.finish_countdown(Err(DeviceError::Other(reason)), false, Stage::Idle)
                    }
                    // Unknown whether the stream cut happened: treated as not on air.
                    Job::GoLive(start) => {
                        let e = T0Error { on_air: false, mics_failed: false, failures: vec![reason] };
                        self.finish_golive(Err(e), None, start)
                    }
                    _ => set_warning(&self.snap, reason),
                }
            }
        }
        self.after_job();
    }

    /// Run what was waiting for a video's return path: Recover, then End Service.
    fn after_job(&mut self) {
        if self.running.is_some() {
            return;
        }
        if std::mem::take(&mut self.recover_pending) {
            self.start_recover();
        } else if std::mem::take(&mut self.end_pending) {
            self.start_end();
        }
    }

    fn finish_countdown(&mut self, result: devices::Result<CountdownOutcome>, public: bool, before: Stage) {
        let golive = std::mem::take(&mut self.golive_pending);
        match result {
            // Reached T-0 by itself (even if Go Live was pressed meanwhile): no second t0.
            Ok(CountdownOutcome::Live { public, mics_failed }) => self.set(|s| {
                set_stage(s, Stage::Live);
                remove(&mut s.offers, Offer::GoLive);
                if mics_failed {
                    mics_left_down(s);
                }
                if public {
                    remove(&mut s.offers, Offer::GoPublic);
                } else {
                    add(&mut s.offers, Offer::GoPublic);
                }
            }),
            // Pre-empted by Go Live: the hall is back, so run t0 now. The stage is
            // not left on Countdown; the Go Live job's result sets the final one.
            Ok(CountdownOutcome::Stopped) if golive => {
                self.set(|s| set_stage(s, Stage::Prepared(Readiness::Waiting("Going live".into()))));
                self.start_golive(Some(public));
            }
            // R22: Go Live (and Go Public, if the countdown had not gone public) stay offered.
            Ok(CountdownOutcome::Stopped) => self.set(|s| {
                log(s, "Countdown stopped".into());
                set_stage(s, before);
                add(&mut s.offers, Offer::GoLive);
                if !public {
                    add(&mut s.offers, Offer::GoPublic);
                }
            }),
            Err(e) => {
                let msg = format!("The countdown failed ({e}); press Go Live manually");
                self.set(|s| {
                    set_stage(s, Stage::Prepared(Readiness::Warning(msg.clone())));
                    add(&mut s.offers, Offer::GoLive);
                    if !public {
                        add(&mut s.offers, Offer::GoPublic);
                    }
                    warn_in(s, msg);
                });
                if golive {
                    self.start_golive(Some(public));
                }
            }
        }
    }

    /// Live whenever the stream cut succeeded, even if later steps failed (then
    /// with a warning). Whenever the broadcast did not go public (the go-public failed,
    /// or there was no broadcast to make public), Go Public is offered: the window's
    /// "Retry going public".
    fn finish_golive(&mut self, result: Result<(), T0Error>, public: Option<devices::Result<()>>, start: GoLiveStart) {
        let GoLiveStart { preempt_public, already_public, no_broadcast } = start;
        let mics_failed = result.as_ref().err().is_some_and(|e| e.mics_failed);
        let (on_air, warning) = match &result {
            Ok(()) => (true, None),
            Err(e) if e.on_air => (true, Some(format!("Live, but some T-0 steps failed: {e}"))),
            Err(e) => (false, Some(format!("Go Live did not complete: {e}"))),
        };
        let public_warning = match &public {
            Some(Err(e)) => Some(format!("Could not make the YouTube broadcast public: {e}")),
            _ => None,
        };
        let now_public = already_public || matches!(public, Some(Ok(())));
        self.set(|s| {
            if on_air {
                set_stage(s, Stage::Live);
                remove(&mut s.offers, Offer::GoLive);
                if mics_failed {
                    mics_left_down(s);
                }
            } else if preempt_public.is_some() {
                set_stage(s, Stage::Prepared(Readiness::Warning(warning.clone().unwrap_or_default())));
                add(&mut s.offers, Offer::GoLive);
            }
            if on_air || preempt_public.is_some() || public.is_some() {
                if now_public {
                    remove(&mut s.offers, Offer::GoPublic);
                } else {
                    add(&mut s.offers, Offer::GoPublic);
                }
            }
            // Live with no broadcast selected: say why nothing went public.
            if on_air && no_broadcast {
                warn_in(s, NO_BROADCAST.into());
            }
            // The T-0 warning goes last: it is the one left showing.
            if let Some(w) = public_warning {
                warn_in(s, w);
            }
            if let Some(w) = warning {
                warn_in(s, w);
            }
        });
    }

    fn finish_end(&mut self, out: EndOutcome) {
        self.set(|s| {
            set_stage(s, Stage::Ended);
            for o in [Offer::GoLive, Offer::GoPublic, Offer::EndBroadcast] {
                remove(&mut s.offers, o);
            }
            if !out.ended_broadcast {
                add(&mut s.offers, Offer::EndBroadcast);
            }
            if !out.warnings.is_empty() {
                warn_in(s, out.warnings.join("; "));
            }
        });
    }

    /// `mics_ducked` is left as the roll-in last reported it.
    fn finish_video(&mut self, index: usize, outcome: RollinOutcome) {
        let name = self.cues.get(index).map(|c| c.name.clone()).unwrap_or_default();
        let done = match outcome {
            RollinOutcome::Finished => Some("ended"),
            RollinOutcome::Stopped => Some("stopped"),
            _ => None,
        };
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
            s.video = None;
            if let Some(done) = done {
                log(s, format!("Video {} · {name} · {done}", index + 1));
            }
            if let Some(w) = warning {
                warn_in(s, w);
            }
            if recover {
                add(&mut s.offers, Offer::Recover);
            }
        });
    }

    // ---- always-accepted side tasks ----

    fn spawn_go_public(&self) {
        let (d, snap, bid) = (self.d.clone(), self.snap.clone(), self.bid());
        tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            match golive::go_public(&d, &bid).await {
                Ok(()) => snap.send_modify(|s| {
                    remove(&mut s.offers, Offer::GoPublic);
                    log(s, golive::public_note(start.elapsed()));
                }),
                Err(e) => set_warning(&snap, format!("Could not make the YouTube broadcast public: {e}")),
            }
        });
    }

    fn spawn_end_broadcast(&self) {
        let (d, snap, bid) = (self.d.clone(), self.snap.clone(), self.bid());
        tokio::spawn(async move {
            match golive::transition_with_retry(&d, &bid, Phase::Complete).await {
                Ok(()) => snap.send_modify(|s| {
                    remove(&mut s.offers, Offer::EndBroadcast);
                    log(s, "YouTube broadcast ended".into());
                }),
                Err(e) => set_warning(&snap, format!("Could not end the YouTube broadcast: {e}")),
            }
        });
    }

    /// Optionally apply `fix` to broadcast `id`, then re-check it.
    fn spawn_check(&self, id: String, fix: Option<Fix>) {
        let (d, snap, aux, sel_gen) = (self.d.clone(), self.snap.clone(), self.aux.clone(), self.sel_gen);
        tokio::spawn(async move {
            if let Some(f) = fix
                && let Err(e) = d.yt.fix(&id, f).await
            {
                set_warning(&snap, format!("Could not fix the broadcast: {e}"));
            }
            let check = d.yt.check(&id).await.map_err(|e| e.to_string());
            let _ = aux.send(Aux::Checked { sel_gen, check });
        });
    }

    // ---- side-task results ----

    fn apply_aux(&mut self, a: Aux) {
        match a {
            Aux::Checked { sel_gen, check } => {
                if sel_gen != self.sel_gen {
                    info!("dropping a broadcast check for an earlier selection");
                    return;
                }
                self.set(|s| match check {
                    Ok(c) => s.check = Some(c),
                    Err(e) => {
                        s.check = None;
                        warn_in(s, format!("Could not check the broadcast: {e}"));
                    }
                });
            }
            Aux::Refreshed(r) => self.apply_refresh(*r),
            Aux::ClearFlash(generation) => {
                if generation == self.flash_gen {
                    self.set(|s| s.flash = None);
                }
            }
            Aux::ClearKeyFlash(generation) => {
                if generation == self.key_flash_gen {
                    self.set(|s| s.flash_key = None);
                }
            }
        }
    }

    /// R23: a refresh never replaces or clears the operator's selection. It only fills an
    /// empty one; a failed list read keeps the last list.
    fn apply_refresh(&mut self, r: Refreshed) {
        if r.report.listed {
            self.broadcasts = r.report.broadcasts;
        }
        let current_sel = r.sel_gen == self.sel_gen;
        let broadcast = r.selected.as_deref().map(|id| self.summary_for(id));
        let errors = r.report.device_errors.join("; ");
        self.set(|s| {
            if current_sel && let Some(b) = broadcast {
                let same = s.broadcast.as_ref().is_some_and(|x| x.id == b.id);
                // A failed check of the same broadcast keeps the last good one.
                if !same || r.check.is_some() {
                    s.check = r.check;
                }
                s.broadcast = Some(b);
            }
            if !errors.is_empty() {
                warn_in(s, errors);
            }
        });
        self.apply_assessment(r.assessment, r.epoch);
    }

    /// A current assessment sets the offers, and the stage only when no job runs and
    /// the stage is Idle (R20): an assessment can raise Idle to Prepared or Live, but
    /// never lowers Prepared/Countdown/Live/Ending/Ended, because a single failed read
    /// mid-service would otherwise show Idle. A stale one (the epoch has moved on)
    /// never sets the stage and only adds offers.
    fn apply_assessment(&self, a: Assessment, epoch: u64) {
        let current = epoch == self.epoch;
        let busy = self.running.is_some();
        if !current {
            info!("merging the offers of a stale assessment");
        }
        self.set(|s| {
            if current {
                s.offers = a.offer;
                if !busy && s.stage == Stage::Idle {
                    if a.stage == Stage::Live {
                        log(s, "Found the service already live".into());
                    }
                    set_stage(s, a.stage);
                } else if !busy && s.stage != a.stage {
                    info!(stage = ?s.stage, assessed = ?a.stage, "assessment does not change the stage");
                }
            } else {
                for o in a.offer {
                    add(&mut s.offers, o);
                }
            }
        });
    }
}

/// I1: T-0 went live but could not bring the mics up. Show them as down and offer
/// Recover, the one-press way back.
fn mics_left_down(s: &mut Snapshot) {
    s.mics_ducked = true;
    add(&mut s.offers, Offer::Recover);
}

/// Prepare never moves the stage off Live (Go Live may have landed meanwhile).
fn set_prepared(s: &mut Snapshot, r: Readiness) {
    if s.stage != Stage::Live {
        set_stage(s, Stage::Prepared(r));
    }
}

/// Move to `stage`, keeping `on_air_since` and noting the change in the activity list.
/// Countdown and Live are noted by their sequences, which know more (the video's
/// length, what went on air).
fn set_stage(s: &mut Snapshot, stage: Stage) {
    let text = match (&s.stage, &stage) {
        (old, new) if old == new => None,
        (_, Stage::Prepared(Readiness::Ready)) => Some("Ready for the countdown"),
        (_, Stage::Prepared(Readiness::Warning(_))) => Some("Prepared, with warnings"),
        (Stage::Ending { .. }, Stage::Ending { .. }) => None,
        (_, Stage::Ending { .. }) => Some("Ending the service"),
        (_, Stage::Ended) => Some("Service ended"),
        _ => None,
    };
    if let Some(t) = text {
        log(s, t.into());
    }
    match stage {
        Stage::Live if s.on_air_since.is_none() => s.on_air_since = Some(SystemTime::now()),
        Stage::Ended => s.on_air_since = None,
        _ => {}
    }
    s.stage = stage;
}

/// Add `text` to the top of the activity list, dropping the oldest past `ACTIVITY_MAX`.
fn log(s: &mut Snapshot, text: String) {
    s.activity.insert(0, Activity { at: SystemTime::now(), text });
    s.activity.truncate(ACTIVITY_MAX);
}

/// Show `msg` as the warning and record it in the activity list.
fn warn_in(s: &mut Snapshot, msg: String) {
    log(s, msg.clone());
    s.last_warning = Some(msg);
}

/// `M:SS`.
fn mss(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Stage 0 plus assessment. Keeps `current` selected whatever the list says (R23);
/// only an empty selection takes stage 0's choice.
async fn refresh(
    d: Devices,
    c: Config,
    current: Option<String>,
    player_active: bool,
    epoch: u64,
    sel_gen: u64,
) -> Refreshed {
    let mut report = golive::stage0(&d, &c).await;
    let selected = current.or_else(|| report.selected.clone());
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
    Refreshed { epoch, sel_gen, report, selected, check, assessment }
}

fn read(c: &RwLock<Config>) -> Config {
    c.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn lock(m: &Mutex<LiveLevels>) -> std::sync::MutexGuard<'_, LiveLevels> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write a sequence's levels back and, if they changed, save them (I5). A failed save
/// is logged; the levels in memory are still used.
fn store_levels(levels: &Mutex<LiveLevels>, l: LiveLevels, file: Option<&PathBuf>) {
    let changed = {
        let mut cur = lock(levels);
        let changed = *cur != l;
        *cur = l.clone();
        changed
    };
    if changed && let Some(path) = file {
        match l.save(path) {
            Ok(()) => info!(path = %path.display(), "mic levels saved"),
            Err(e) => warn!(error = %format!("{e:#}"), "could not save the mic levels"),
        }
    }
}

fn set_warning(snap: &Shared, msg: String) {
    info!(warning = %msg, "warning");
    snap.send_modify(|s| warn_in(s, msg));
}

fn cue_rows(l: &CueList) -> Vec<(Cue, CueState)> {
    l.cues()
        .iter()
        .enumerate()
        .map(|(i, c)| (c.clone(), l.state.get(i).cloned().unwrap_or(CueState::Cued)))
        .collect()
}

/// A new list of `cues`, all `Cued` except that a cue equal to one in `old` keeps
/// that one's state (each old cue lends its state once).
fn rebuild_cues(old: &CueList, cues: Vec<Cue>) -> CueList {
    let mut used = vec![false; old.len()];
    let mut list = CueList::from_cues(cues);
    for i in 0..list.len() {
        let found = (0..old.len()).find(|&j| !used[j] && old.get(j) == list.get(i));
        if let Some(j) = found {
            used[j] = true;
            list.state[i] = old.state.get(j).cloned().unwrap_or(CueState::Cued);
        }
    }
    list
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
        assert!(!s.offers.contains(&Offer::Recover), "{:?}", s.offers);
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

    // ---- R13 ----

    fn stream_autos(log: &CallLog) -> usize {
        count(log, |c| *c == Call::StreamAuto)
    }

    #[tokio::test(start_paused = true)]
    async fn golive_mid_countdown_preempts() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        let calls = log.calls();
        // The countdown's cancel path (video stopped, counter cleared) runs before t0.
        assert!(pos(&calls, &Call::PlayerStop) < pos(&calls, &Call::StreamAuto), "{calls:?}");
        assert_eq!(stream_autos(&log), 1, "{calls:?}");
        // The countdown is gone: its video ending changes nothing.
        h.player.emit(PlayerEvent::Ended);
        sleep(Duration::from_secs(15)).await;
        assert_eq!(stream_autos(&log), 1, "{:?}", log.calls());
        assert_eq!(snap(&e).stage, Stage::Live);
    }

    #[tokio::test(start_paused = true)]
    async fn double_golive_ignored() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::GoLive).await;
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        sleep(Duration::from_secs(5)).await;
        assert_eq!(stream_autos(&log), 1, "{:?}", log.calls());
    }

    #[tokio::test(start_paused = true)]
    async fn stop_countdown_after_golive_does_not_reopen_prepare() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        // Presses while Go Live is in flight.
        send(&e, Command::GoLive).await;
        send(&e, Command::StopCountdown).await;
        sleep(MS(50)).await; // the countdown has returned; t0 is still fading the mics up
        send(&e, Command::Prepare).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        // Presses once live.
        send(&e, Command::StopCountdown).await;
        send(&e, Command::Prepare).await;
        sleep(Duration::from_secs(5)).await;
        assert_eq!(snap(&e).stage, Stage::Live);
        assert!(!log.calls().contains(&Call::EncoderStart), "{:?}", log.calls());
        assert_eq!(count(&log, |c| matches!(c, Call::StreamSetMediaStill(..))), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn recover_during_video_waits_for_return_path() {
        let (d, log, _h) = rig();
        let c = cfg(vec![cue("a", Destination::HallAndOnline)]);
        c.write().unwrap().videohub.fallback_normal_input = CAM8;
        let mut e = Engine::spawn(d, c);
        send(&e, Command::PlayCue(0)).await;
        // Recover pressed in the VideoHub settle window.
        until_call(&log, Call::HubRoute { output: OUT, input: DK }).await;
        send(&e, Command::Recover).await;
        for _ in 0..1000 {
            if count(&log, |c| *c == Call::HubRoute { output: OUT, input: CAM8 }) == 2 {
                break;
            }
            sleep(MS(10)).await;
        }
        let calls = log.calls();
        let restores: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == Call::HubRoute { output: OUT, input: CAM8 })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(restores.len(), 2, "{calls:?}");
        // Nothing touched the route before the roll-in stopped its video.
        assert!(pos(&calls, &Call::PlayerStop) < restores[0], "{calls:?}");
        // The roll-in never went on air (it was stopped during the settle).
        assert!(!calls.contains(&Call::StreamSetPreview(8)), "{calls:?}");
        let s = until(&mut e, "cue played", |s| s.cues[0].1 == CueState::Played).await;
        assert_eq!(s.playing, None);
    }

    #[tokio::test(start_paused = true)]
    async fn double_recover_ignored() {
        let (d, log, _h) = rig();
        let e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Recover).await;
        send(&e, Command::Recover).await;
        sleep(Duration::from_secs(5)).await;
        let in8_off = Call::StreamSetAudioOn(AudioSourceId { input: 8, source: -65280 }, false);
        assert_eq!(count(&log, |c| *c == in8_off), 1, "{:?}", log.calls());
        // Once it has finished, Recover is accepted again.
        send(&e, Command::Recover).await;
        sleep(Duration::from_secs(5)).await;
        assert_eq!(count(&log, |c| *c == in8_off), 2, "{:?}", log.calls());
    }

    #[tokio::test(start_paused = true)]
    async fn stale_refresh_does_not_overwrite_live() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        // A slow Refresh whose assessment (broadcast state unreadable) says Idle.
        h.yt.set_delay("upcoming", Duration::from_secs(5));
        h.yt.fail_next("life_cycle");
        send(&e, Command::Refresh).await;
        sleep(MS(10)).await;
        h.player.emit(PlayerEvent::Ended);
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        sleep(Duration::from_secs(10)).await;
        assert_eq!(snap(&e).stage, Stage::Live);
    }

    #[tokio::test(start_paused = true)]
    async fn later_select_wins() {
        let (d, _log, h) = rig();
        h.yt.set_upcoming(vec![summary("b1"), summary("b2"), summary("b3")]);
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "b1", |s| s.broadcast.as_ref().is_some_and(|b| b.id == "b1")).await;
        h.yt.set_delay("check", Duration::from_secs(5));
        send(&e, Command::SelectBroadcast("b2".into())).await;
        sleep(MS(10)).await;
        h.yt.set_delay("check", Duration::ZERO);
        send(&e, Command::SelectBroadcast("b3".into())).await;
        until(&mut e, "b3", |s| s.broadcast.as_ref().is_some_and(|b| b.id == "b3")).await;
        sleep(Duration::from_secs(10)).await;
        let s = snap(&e);
        assert_eq!(s.broadcast.map(|b| b.id).as_deref(), Some("b3"));
        assert!(s.check.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn live_without_public_offers_go_public() {
        let (d, _log, h) = rig();
        h.yt.fail_times("transition", 4);
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.player.emit(PlayerEvent::Ended);
        let s = until(&mut e, "GoPublic offered", |s| s.offers.contains(&Offer::GoPublic)).await;
        assert_eq!(s.stage, Stage::Live);
        assert!(!s.offers.contains(&Offer::GoLive));
    }

    #[tokio::test(start_paused = true)]
    async fn live_and_public_offers_nothing() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.player.emit(PlayerEvent::Ended);
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        sleep(Duration::from_secs(15)).await;
        assert!(snap(&e).offers.is_empty(), "{:?}", snap(&e).offers);
    }

    #[tokio::test(start_paused = true)]
    async fn mics_stay_ducked_until_unmute_fade_succeeds() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallAndOnline)]));
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        sleep(MS(1500)).await;
        h.stream.fail_next("set_audio_level");
        send(&e, Command::UnmuteMics).await;
        sleep(MS(1500)).await;
        assert!(snap(&e).mics_ducked, "the fade up failed, so the mics are still down");
        // A second press works.
        send(&e, Command::UnmuteMics).await;
        until(&mut e, "unducked", |s| !s.mics_ducked).await;
    }

    // ---- R14 ----

    /// Go Live was ignored: its key flashes and t0 (which clears the counter) never ran.
    async fn assert_golive_ignored(e: &mut EngineHandle, log: &CallLog) {
        send(e, Command::GoLive).await;
        until(e, "Go Live flash", |s| s.flash_key == Some(FlashKey::GoLive)).await;
        sleep(Duration::from_secs(2)).await;
        assert!(!log.calls().contains(&Call::CounterClear), "{:?}", log.calls());
        assert_eq!(snap(e).flash_key, None, "the flash clears after 1 s");
        assert_ne!(snap(e).stage, Stage::Live);
    }

    #[tokio::test(start_paused = true)]
    async fn golive_ignored_during_prepare() {
        let (d, log, h) = rig();
        h.yt.set_health(crate::devices::Health::NotReceiving); // Prepare waits up to 60 s
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Prepare).await;
        until_call(&log, Call::EncoderStart).await;
        let autos = stream_autos(&log);
        assert_golive_ignored(&mut e, &log).await;
        assert_eq!(stream_autos(&log), autos);
        assert!(matches!(snap(&e).stage, Stage::Prepared(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn golive_ignored_during_end_service() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::EndService).await;
        until(&mut e, "Ending", |s| matches!(s.stage, Stage::Ending { .. })).await;
        assert_golive_ignored(&mut e, &log).await;
        until(&mut e, "Ended", |s| s.stage == Stage::Ended).await;
        assert!(!log.calls().contains(&Call::CounterClear));
    }

    #[tokio::test(start_paused = true)]
    async fn golive_ignored_during_video() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallAndOnline)]));
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        assert_golive_ignored(&mut e, &log).await;
        assert_eq!(snap(&e).playing, Some(0));
        assert!(!log.calls().contains(&Call::PlayerStop));
    }

    #[tokio::test(start_paused = true)]
    async fn recover_ignored_during_countdown() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        send(&e, Command::Recover).await;
        sleep(Duration::from_secs(3)).await;
        let in8_off = Call::StreamSetAudioOn(AudioSourceId { input: 8, source: -65280 }, false);
        assert!(!log.calls().contains(&in8_off), "{:?}", log.calls());
        assert!(matches!(snap(&e).stage, Stage::Countdown { .. }));
        assert!(!log.calls().contains(&Call::PlayerStop), "the countdown keeps running");
    }

    #[tokio::test(start_paused = true)]
    async fn preempt_goes_public_or_offers_retry() {
        // v1.1: the pre-empt goes public alongside the cut.
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { public: false, .. })).await;
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert!(!s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
        assert!(!s.offers.contains(&Offer::GoLive), "{:?}", s.offers);
        assert_eq!(count(&log, |c| *c == Call::YtTransition(Phase::Live)), 1, "{:?}", log.calls());

        // If that go-public fails, Go Public stays offered.
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { public: false, .. })).await;
        h.yt.fail_times("transition", 4);
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert!(s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
        assert!(!s.offers.contains(&Offer::GoLive), "{:?}", s.offers);
    }

    #[tokio::test(start_paused = true)]
    async fn preempt_t0_failure_goes_prepared_with_go_live_offer() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.stream.fail_next("auto"); // t0's stream cut fails
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Prepared(Warning)", |s| matches!(s.stage, Stage::Prepared(Readiness::Warning(_)))).await;
        assert!(s.offers.contains(&Offer::GoLive), "{:?}", s.offers);
        // v1.1: going public ran alongside the failed cut, and worked.
        assert!(!s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
        sleep(Duration::from_secs(5)).await;
        assert!(matches!(snap(&e).stage, Stage::Prepared(Readiness::Warning(_))), "{:?}", snap(&e).stage);
        // Go Live may be pressed again.
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
    }

    // ---- R15 ----

    const IN8_OFF: Call = Call::StreamSetAudioOn(AudioSourceId { input: 8, source: -65280 }, false);

    #[tokio::test(start_paused = true)]
    async fn play_cue_ignored_while_go_live_runs() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallAndOnline)]));
        send(&e, Command::GoLive).await; // t0's mic fade takes 1 s
        sleep(MS(100)).await;
        send(&e, Command::PlayCue(0)).await;
        until(&mut e, "cue flash", |s| s.flash == Some(0)).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        sleep(MS(500)).await;
        assert_eq!(count(&log, |c| matches!(c, Call::PlayerLoad(..))), 0, "{:?}", log.calls());
        assert_eq!(snap(&e).cues[0].1, CueState::Cued);
    }

    #[tokio::test(start_paused = true)]
    async fn go_live_and_recover_exclusive() {
        // Go Live first: Recover is ignored and flashes.
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::GoLive).await;
        sleep(MS(100)).await;
        send(&e, Command::Recover).await;
        until(&mut e, "Recover flash", |s| s.flash_key == Some(FlashKey::Recover)).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        sleep(Duration::from_secs(3)).await;
        assert!(!log.calls().contains(&IN8_OFF), "{:?}", log.calls());

        // Recover first: Go Live is ignored and flashes.
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Recover).await;
        sleep(MS(100)).await;
        send(&e, Command::GoLive).await;
        until(&mut e, "Go Live flash", |s| s.flash_key == Some(FlashKey::GoLive)).await;
        sleep(Duration::from_secs(3)).await;
        assert!(log.calls().contains(&IN8_OFF), "{:?}", log.calls());
        assert!(!log.calls().contains(&Call::CounterClear), "{:?}", log.calls());
        assert_ne!(snap(&e).stage, Stage::Live);
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_ignored_while_recover_runs() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Recover).await;
        sleep(MS(100)).await;
        send(&e, Command::Prepare).await;
        send(&e, Command::Countdown).await;
        sleep(Duration::from_secs(3)).await;
        assert!(log.calls().contains(&IN8_OFF));
        assert!(!log.calls().contains(&Call::EncoderStart), "{:?}", log.calls());
        assert_eq!(count(&log, |c| matches!(c, Call::PlayerLoad(..))), 0);
        // Recover has finished: Prepare is accepted again.
        send(&e, Command::Prepare).await;
        until(&mut e, "Prepared", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn go_live_ignored_after_ended() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::EndService).await;
        until(&mut e, "Ended", |s| s.stage == Stage::Ended).await;
        let before = log.calls().len();
        send(&e, Command::GoLive).await;
        until(&mut e, "Go Live flash", |s| s.flash_key == Some(FlashKey::GoLive)).await;
        sleep(Duration::from_secs(3)).await;
        assert_eq!(log.calls().len(), before, "{:?}", log.calls());
        assert_eq!(snap(&e).stage, Stage::Ended);
    }

    #[tokio::test(start_paused = true)]
    async fn t0_partial_failure_after_cut_stays_live_with_warning() {
        // Go Live from idle.
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        h.counter.fail_next("clear");
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        assert!(s.last_warning.as_deref().is_some_and(|w| w.contains("counter")), "{:?}", s.last_warning);
        send(&e, Command::Prepare).await;
        sleep(Duration::from_secs(2)).await;
        assert_eq!(snap(&e).stage, Stage::Live);
        assert!(!log.calls().contains(&Call::EncoderStart));

        // Go Live pre-empting a countdown.
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.screens.fail_times("set_preview", 1); // cancel path's hall return fails
        h.counter.fail_times("clear", 2); // cancel path and t0 both fail to clear
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        assert!(s.last_warning.is_some());
        sleep(Duration::from_secs(2)).await;
        assert_eq!(snap(&e).stage, Stage::Live);

        // The countdown's own T-0.
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.counter.fail_next("clear");
        h.player.emit(PlayerEvent::Ended);
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        sleep(Duration::from_secs(15)).await;
        let s = snap(&e);
        assert_eq!(s.stage, Stage::Live);
        assert!(!s.offers.contains(&Offer::GoLive), "{:?}", s.offers);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_fade_up_keeps_mics_ducked() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallAndOnline)]));
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        sleep(MS(1500)).await;
        h.stream.fail_next("audio_level"); // the return-path fade up cannot start
        h.player.emit(PlayerEvent::Ended);
        let s = until(&mut e, "cue error", |s| matches!(s.cues[0].1, CueState::Error(_))).await;
        assert!(s.mics_ducked);
        assert!(s.offers.contains(&Offer::Recover));
    }

    #[tokio::test(start_paused = true)]
    async fn set_cues_replaces_list_when_no_video_plays() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallOnly)]));
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        h.player.emit(PlayerEvent::Ended);
        until(&mut e, "played", |s| s.cues[0].1 == CueState::Played).await;
        let new = vec![cue("b", Destination::OnlineOnly), cue("a", Destination::HallOnly)];
        send(&e, Command::SetCues(new.clone())).await;
        let s = until(&mut e, "new cues", |s| s.cues.len() == 2).await;
        assert_eq!(s.cues[0], (new[0].clone(), CueState::Cued));
        // An unchanged cue keeps its state when the list is rebuilt.
        assert_eq!(s.cues[1], (new[1].clone(), CueState::Played));
        assert_eq!(s.last_warning, None);
        // The new list is what PlayCue now plays.
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerLoad(PathBuf::from("b.mp4"), 0.0)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn set_cues_ignored_while_video_plays() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallOnly)]));
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        send(&e, Command::SetCues(vec![])).await;
        let s = until(&mut e, "warning", |s| s.last_warning.is_some()).await;
        assert!(s.last_warning.as_deref().unwrap().contains("video"), "{:?}", s.last_warning);
        assert_eq!(s.cues.len(), 1);
        assert_eq!(s.playing, Some(0));
    }

    #[tokio::test(start_paused = true)]
    async fn snapshot_shows_the_running_job() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallOnly)]));
        assert_eq!(snap(&e).job, None);
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::PlayerPlay).await;
        assert_eq!(snap(&e).job, Some("a video"));
        h.player.emit(PlayerEvent::Ended);
        until(&mut e, "no job", |s| s.job.is_none() && s.playing.is_none()).await;
    }

    // ---- R22 ----

    #[tokio::test(start_paused = true)]
    async fn go_live_ignored_and_flashed_when_live() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        let before = log.calls().len();
        send(&e, Command::GoLive).await;
        until(&mut e, "Go Live flash", |s| s.flash_key == Some(FlashKey::GoLive)).await;
        sleep(Duration::from_secs(3)).await;
        assert_eq!(log.calls().len(), before, "{:?}", log.calls());
        assert_eq!(stream_autos(&log), 1);
        assert_eq!(snap(&e).stage, Stage::Live);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_countdown_offers_go_live() {
        let (d, _log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { public: false, .. })).await;
        send(&e, Command::StopCountdown).await;
        let s = until(&mut e, "stopped", |s| s.job.is_none() && !matches!(s.stage, Stage::Countdown { .. })).await;
        assert!(s.offers.contains(&Offer::GoLive), "{:?}", s.offers);
        assert!(s.offers.contains(&Offer::GoPublic), "the countdown had not gone public: {:?}", s.offers);
    }

    // ---- I1 ----

    #[tokio::test(start_paused = true)]
    async fn t0_mic_fade_failure_offers_recover() {
        // Go Live by hand.
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "startup done", |s| s.broadcast.is_some()).await;
        h.stream.fail_next("set_audio_level");
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert!(s.mics_ducked, "the mics did not come up");
        assert!(s.offers.contains(&Offer::Recover), "{:?}", s.offers);

        // The countdown's own T-0.
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.stream.fail_next("set_audio_level");
        h.player.emit(PlayerEvent::Ended);
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert!(s.mics_ducked, "the mics did not come up");
        assert!(s.offers.contains(&Offer::Recover), "{:?}", s.offers);
    }

    // ---- I5 ----

    #[tokio::test(start_paused = true)]
    async fn engine_starts_with_loaded_levels() {
        let (d, log, h) = rig();
        h.stream.set_audio_level_state(MIC, crate::audio::FLOOR_DB); // restarted with the mics down
        let loaded = LiveLevels([(MIC, -7.0)].into_iter().collect());
        let mut e = Engine::spawn_with(d, cfg(vec![]), loaded, None, false);
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        let last_mic = log.calls().iter().rev().find_map(|c| match c {
            Call::StreamSetAudioLevel(m, db) if *m == MIC => Some(*db),
            _ => None,
        });
        assert_eq!(last_mic, Some(-7.0), "{:?}", log.calls());
    }

    #[tokio::test(start_paused = true)]
    async fn engine_saves_levels_when_they_change() {
        let dir = std::env::temp_dir().join("streamit-engine-levels");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("levels.toml");
        let (d, _log, _h) = rig(); // mics at -3 dB
        let mut e = Engine::spawn_with(d, cfg(vec![]), LiveLevels::default(), Some(path.clone()), false);
        send(&e, Command::Prepare).await;
        until(&mut e, "Prepared", |s| s.stage == Stage::Prepared(Readiness::Ready) && s.job.is_none()).await;
        assert_eq!(LiveLevels::load(&path).unwrap().0.get(&MIC), Some(&-3.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- minors ----

    #[tokio::test(start_paused = true)]
    async fn refresh_during_countdown_offers_no_recover() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        // The hall is on the DeckLink, playing the countdown: that is not a crash.
        h.encoder.fail_next("status");
        send(&e, Command::Refresh).await;
        until(&mut e, "the refresh's warning", |s| s.last_warning.is_some()).await;
        sleep(MS(500)).await;
        assert!(!snap(&e).offers.contains(&Offer::Recover), "{:?}", snap(&e).offers);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_youtube_does_not_hold_presses_past_the_refresh_timeout() {
        let (d, log, h) = rig();
        h.yt.set_delay("upcoming", Duration::from_secs(60));
        let start = tokio::time::Instant::now();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert!(start.elapsed() < Duration::from_secs(15), "Go Live waited {:?}", start.elapsed());
        assert!(s.activity.iter().any(|a| a.text.contains("did not finish")), "{:?}", texts(&s));
        assert!(log.calls().contains(&Call::StreamAuto));

        // A manual Refresh is bounded too, and says so.
        send(&e, Command::Refresh).await;
        sleep(Duration::from_secs(11)).await;
        let w = snap(&e).last_warning.unwrap_or_default();
        assert!(w.contains("Refresh did not finish"), "{w}");
    }

    // ---- R23 ----

    fn selected_id(s: &Snapshot) -> Option<String> {
        s.broadcast.as_ref().map(|b| b.id.clone())
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_keeps_selection_not_in_list() {
        let (d, _log, h) = rig();
        h.yt.set_upcoming(vec![summary("b1"), summary("b2")]);
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "b1", |s| selected_id(s).as_deref() == Some("b1")).await;
        send(&e, Command::SelectBroadcast("b2".into())).await;
        until(&mut e, "b2 checked", |s| selected_id(s).as_deref() == Some("b2") && s.check.is_some()).await;
        // b2 has gone from YouTube's list (it went live and then ended, say).
        h.yt.set_upcoming(vec![summary("b1")]);
        h.encoder.fail_next("status"); // so the refresh leaves a warning to wait for
        send(&e, Command::Refresh).await;
        until(&mut e, "the refresh's warning", |s| s.last_warning.is_some()).await;
        sleep(MS(500)).await;
        let s = snap(&e);
        assert_eq!(selected_id(&s).as_deref(), Some("b2"));
        assert_eq!(s.broadcast.unwrap().title, "b2");
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_failure_keeps_selection() {
        let (d, _log, h) = rig();
        h.yt.set_upcoming(vec![summary("b1"), summary("b2")]);
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "b1", |s| selected_id(s).as_deref() == Some("b1")).await;
        send(&e, Command::SelectBroadcast("b2".into())).await;
        until(&mut e, "b2 checked", |s| selected_id(s).as_deref() == Some("b2") && s.check.is_some()).await;
        h.yt.fail_next("upcoming");
        send(&e, Command::Refresh).await;
        until(&mut e, "the refresh's warning", |s| {
            s.last_warning.as_deref().is_some_and(|w| w.contains("broadcasts"))
        })
        .await;
        sleep(MS(500)).await;
        let s = snap(&e);
        assert_eq!(selected_id(&s).as_deref(), Some("b2"));
        assert_eq!(s.broadcast.unwrap().title, "b2");
        assert!(s.check.is_some(), "the check is kept too");
    }

    #[tokio::test(start_paused = true)]
    async fn startup_prefers_live_bound_broadcast() {
        let (d, _log, h) = rig();
        let bound = |id: &str, lc: &str, stream: Option<&str>| BroadcastSummary {
            life_cycle: lc.into(),
            bound_stream_id: stream.map(String::from),
            ..summary(id)
        };
        h.yt.set_upcoming(vec![
            summary("next-week"),
            bound("other-key", "live", Some("someone-else")),
            bound("ours", "testing", Some("s1")),
        ]);
        let c = cfg(vec![]);
        c.write().unwrap().youtube.stream_id = Some("s1".into());
        let mut e = Engine::spawn(d, c);
        let s = until(&mut e, "a selection", |s| s.broadcast.is_some()).await;
        assert_eq!(selected_id(&s).as_deref(), Some("ours"));
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_never_downgrades_prepared_or_live() {
        // Prepared: the encoder read fails, so a fresh assessment says Idle.
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Prepare).await;
        until(&mut e, "Prepared", |s| s.stage == Stage::Prepared(Readiness::Ready) && s.job.is_none()).await;
        h.encoder.fail_times("status", 20);
        send(&e, Command::Refresh).await;
        until(&mut e, "the refresh's warning", |s| {
            s.last_warning.as_deref().is_some_and(|w| w.contains("disconnected"))
        })
        .await;
        sleep(MS(500)).await;
        assert_eq!(snap(&e).stage, Stage::Prepared(Readiness::Ready));
        assert!(log.calls().contains(&Call::EncoderStart));

        // Live: the YouTube life-cycle read fails, so a fresh assessment says Idle.
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        h.yt.fail_times("life_cycle", 20);
        h.encoder.fail_times("status", 20);
        send(&e, Command::Refresh).await;
        until(&mut e, "the refresh's warning", |s| {
            s.last_warning.as_deref().is_some_and(|w| w.contains("disconnected"))
        })
        .await;
        sleep(MS(500)).await;
        assert_eq!(snap(&e).stage, Stage::Live);
    }

    // ---- v1.1: what the screens draw ----

    fn texts(s: &Snapshot) -> Vec<String> {
        s.activity.iter().map(|a| a.text.clone()).collect()
    }

    fn has(s: &Snapshot, text: &str) -> bool {
        s.activity.iter().any(|a| a.text == text)
    }

    #[tokio::test(start_paused = true)]
    async fn video_progress_is_published_while_playing() {
        let (d, log, h) = rig();
        h.player.set_duration(Duration::from_secs(60));
        h.player.set_position(Some(Duration::ZERO));
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallOnly), cue("Mission update", Destination::HallOnly)]));
        send(&e, Command::PlayCue(1)).await;
        until_call(&log, Call::PlayerPlay).await;
        sleep(Duration::from_secs(10)).await;
        h.player.set_position(Some(Duration::from_secs(10)));
        let s = until(&mut e, "10 s in", |s| s.video.as_ref().is_some_and(|v| v.position == Duration::from_secs(10))).await;
        let want = VideoProgress { index: 1, position: Duration::from_secs(10), duration: Duration::from_secs(60) };
        assert_eq!(s.video, Some(want));
        assert!(has(&s, "Video 2 · Mission update · started"), "{:?}", texts(&s));
        h.player.emit(PlayerEvent::Ended);
        let s = until(&mut e, "played", |s| s.cues[1].1 == CueState::Played).await;
        assert_eq!(s.video, None);
        assert_eq!(s.activity[0].text, "Video 2 · Mission update · ended", "{:?}", texts(&s));
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_total_is_the_video_length() {
        let (d, _log, h) = rig();
        h.player.set_duration(Duration::from_secs(300));
        h.player.set_position(Some(Duration::ZERO));
        let mut e = Engine::spawn(d, cfg(vec![]));
        assert_eq!(snap(&e).countdown_total, None);
        send(&e, Command::Countdown).await;
        let s = until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        assert_eq!(s.countdown_total, Some(Duration::from_secs(300)));
        assert!(has(&s, "Countdown started (5:00)"), "{:?}", texts(&s));
        send(&e, Command::StopCountdown).await;
        let s = until(&mut e, "stopped", |s| s.job.is_none() && !matches!(s.stage, Stage::Countdown { .. })).await;
        assert_eq!(s.countdown_total, None);
    }

    #[tokio::test(start_paused = true)]
    async fn on_air_since_set_at_live_and_cleared_at_ended() {
        let (d, _log, h) = rig();
        h.stream.set_preview_state(9); // not a live input: T-0 takes the default, Cam 1
        let mut e = Engine::spawn(d, cfg(vec![]));
        assert_eq!(snap(&e).on_air_since, None);
        let before = std::time::SystemTime::now();
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert!(s.on_air_since.is_some_and(|t| t >= before), "{:?}", s.on_air_since);
        let t0 = cfg(vec![]).read().unwrap().screens_atem.t0_input;
        let live = format!("Live · Cam 1 on air · mics up · hall → input {t0}");
        assert!(has(&s, &live), "{:?}", texts(&s));
        send(&e, Command::EndService).await;
        let s = until(&mut e, "Ended", |s| s.stage == Stage::Ended).await;
        assert_eq!(s.on_air_since, None);
    }

    #[tokio::test(start_paused = true)]
    async fn activity_records_stage_changes_and_warnings_newest_first_capped_at_50() {
        let (d, _log, h) = rig();
        h.yt.set_upcoming(vec![]); // no broadcast: every sequence warns
        let mut e = Engine::spawn(d, cfg(vec![cue("a", Destination::HallOnly)]));
        send(&e, Command::Prepare).await;
        let s = until(&mut e, "Prepared", |s| s.stage == Stage::Prepared(Readiness::Ready) && s.job.is_none()).await;
        let t = texts(&s);
        let at = |x: &str| t.iter().position(|y| y == x).unwrap_or_else(|| panic!("{x:?} not in {t:?}"));
        // Newest first: the warning came first, then the steps in order.
        assert!(at("Ready for the countdown") < at("YouTube receiving · private"), "{t:?}");
        assert!(at("YouTube receiving · private") < at("Web Presenter streaming"), "{t:?}");
        assert!(at("Web Presenter streaming") < at("Opening still on air · mics silent"), "{t:?}");
        assert!(at("Opening still on air · mics silent") < at(NO_BROADCAST), "{t:?}");
        assert!(s.activity.windows(2).all(|w| w[0].at >= w[1].at));

        // Every warning is recorded; the list keeps the newest 50.
        for _ in 0..60 {
            send(&e, Command::FixBroadcast(Fix::Bind)).await;
        }
        sleep(MS(100)).await;
        let s = snap(&e);
        assert_eq!(s.activity.len(), ACTIVITY_MAX);
        assert!(s.activity.iter().all(|a| a.text == NO_BROADCAST), "{:?}", texts(&s));
    }

    // ---- v1.1: Start service now ----

    fn live_transitions(log: &CallLog) -> usize {
        count(log, |c| *c == Call::YtTransition(Phase::Live))
    }

    /// Run Prepare to the end; YouTube is then in testing (not public).
    async fn prepared(e: &mut EngineHandle) {
        send(e, Command::Prepare).await;
        until(e, "Prepared", |s| s.stage == Stage::Prepared(Readiness::Ready) && s.job.is_none()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn start_service_now_from_prepared_goes_public_and_cuts() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        prepared(&mut e).await;
        let autos = stream_autos(&log);
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert_eq!(live_transitions(&log), 1, "{:?}", log.calls());
        assert_eq!(stream_autos(&log), autos + 1, "{:?}", log.calls());
        assert!(!s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
        assert!(s.activity.iter().any(|a| a.text.starts_with("YouTube public (took ")), "{:?}", texts(&s));
        assert!(s.activity.iter().any(|a| a.text.starts_with("Live · Cam ")), "{:?}", texts(&s));
    }

    #[tokio::test(start_paused = true)]
    async fn start_service_now_does_not_wait_for_youtube() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        prepared(&mut e).await;
        let autos = stream_autos(&log);
        h.yt.set_transition_hangs(true);
        let start = tokio::time::Instant::now();
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        assert!(start.elapsed() < Duration::from_secs(2), "LIVE waited for YouTube: {:?}", start.elapsed());
        assert_eq!(stream_autos(&log), autos + 1, "{:?}", log.calls());
        let s = until(&mut e, "Go Public offered", |s| s.offers.contains(&Offer::GoPublic) && s.job.is_none()).await;
        assert_eq!(s.stage, Stage::Live);
        assert!(s.last_warning.as_deref().is_some_and(|w| w.contains("public")), "{:?}", s.last_warning);
        assert!(start.elapsed() <= golive::GO_PUBLIC_WAIT + Duration::from_secs(1), "{:?}", start.elapsed());
        assert_eq!(live_transitions(&log), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn start_service_now_twice_one_cut() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        prepared(&mut e).await;
        let autos = stream_autos(&log);
        send(&e, Command::GoLive).await;
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        send(&e, Command::GoLive).await;
        sleep(Duration::from_secs(5)).await;
        assert_eq!(stream_autos(&log), autos + 1, "{:?}", log.calls());
        assert_eq!(live_transitions(&log), 1, "{:?}", log.calls());
        assert_eq!(snap(&e).stage, Stage::Live);
    }

    #[tokio::test(start_paused = true)]
    async fn go_public_failure_offers_retry() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "startup done", |s| s.broadcast.is_some()).await;
        h.yt.fail_times("transition", 4);
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Go Public offered", |s| s.offers.contains(&Offer::GoPublic) && s.job.is_none()).await;
        assert_eq!(s.stage, Stage::Live);
        assert!(!s.offers.contains(&Offer::GoLive), "{:?}", s.offers);
        assert!(s.last_warning.as_deref().is_some_and(|w| w.contains("public")), "{:?}", s.last_warning);
        assert_eq!(live_transitions(&log), 4);
        // Retry going public: it works now, and the offer goes.
        send(&e, Command::GoPublic).await;
        let s = until(&mut e, "Go Public cleared", |s| !s.offers.contains(&Offer::GoPublic)).await;
        assert!(s.activity.iter().any(|a| a.text.starts_with("YouTube public (took ")), "{:?}", texts(&s));
        assert_eq!(s.stage, Stage::Live);
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_t0_retries_public_if_it_failed_at_t10() {
        let (d, log, h) = rig();
        h.player.set_duration(Duration::from_secs(60));
        h.player.set_position(Some(MS(50_000)));
        let mut e = Engine::spawn(d, cfg(vec![]));
        prepared(&mut e).await;
        h.yt.fail_times("transition", 4);
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        sleep(Duration::from_secs(7)).await;
        assert_eq!(live_transitions(&log), 4, "the go-public at T-10 has failed");
        h.player.emit(PlayerEvent::Ended);
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert_eq!(live_transitions(&log), 5, "{:?}", log.calls());
        assert!(!s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
    }

    #[tokio::test(start_paused = true)]
    async fn go_live_without_broadcast_warns() {
        let (d, log, h) = rig();
        h.yt.set_upcoming(vec![]);
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert_eq!(s.last_warning.as_deref(), Some(NO_BROADCAST));
        assert!(s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
        assert_eq!(live_transitions(&log), 0, "{:?}", log.calls());
    }

    #[tokio::test(start_paused = true)]
    async fn go_live_skips_public_when_broadcast_already_live() {
        let (d, log, h) = rig();
        h.yt.set_upcoming(vec![BroadcastSummary { life_cycle: "live".into(), ..summary("b1") }]);
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "b1", |s| s.broadcast.as_ref().is_some_and(|b| b.id == "b1")).await;
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert_eq!(live_transitions(&log), 0, "{:?}", log.calls());
        assert!(!s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
        assert_eq!(stream_autos(&log), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_public_not_offered_while_go_live_goes_public() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        send(&e, Command::StopCountdown).await;
        until(&mut e, "Go Public offered", |s| s.offers.contains(&Offer::GoPublic) && s.job.is_none()).await;
        h.yt.set_transition_hangs(true);
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        assert_eq!(s.job, Some("Go Live"), "the go-public is still running");
        assert!(!s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
        sleep(Duration::from_secs(5)).await;
        assert!(!snap(&e).offers.contains(&Offer::GoPublic), "{:?}", snap(&e).offers);
        // It timed out: offered again.
        until(&mut e, "Go Public offered", |s| s.offers.contains(&Offer::GoPublic) && s.job.is_none()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn simulation_flag_is_published() {
        let (d, _log, _h) = rig();
        let e = Engine::spawn_with(d, cfg(vec![]), LiveLevels::default(), None, true);
        assert!(snap(&e).simulation);
        let (d, _log, _h) = rig();
        let e = Engine::spawn(d, cfg(vec![]));
        assert!(!snap(&e).simulation);
    }
}
