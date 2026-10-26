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
use crate::config::{AudioSourceId, Config};
use crate::devices::{
    self, AudioInput, BroadcastCheck, BroadcastSummary, ChatMessage, DeviceError, Devices, Fix, Health, LabelKind,
    LightingStatus, LiveDetails, Phase,
};
use crate::end::{EndOutcome, end_service};
use crate::golive::{self, CountdownOutcome, CountdownReport, Readiness, Stage, Stage0Report, T0Error};
use crate::recovery::{self, Assessment, Offer};
use crate::rollin::{self, Cue, CueList, CueState, RollinOutcome, RollinReport};
use crate::routing::{Preset, RoutingRead, RoutingState, Target, TargetState, locked};
use crate::scenes::{self, MAX_SCENES, Scene};
use crate::steps::{self, Step, StepGroup, StepId, StepMark};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, info, warn};

/// How long an ignored cue key flashes.
pub const FLASH_FOR: Duration = Duration::from_secs(1);
/// Longest the YouTube part of a startup check or a Refresh may take (a down YouTube
/// otherwise holds the startup check, and every press queued behind it, for its HTTP
/// timeouts). The equipment part is never bounded by it: its assessment always applies.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);
/// Warning shown when a sequence runs with no broadcast selected.
pub const NO_BROADCAST: &str = "No broadcast selected";
/// Most entries the activity list keeps.
pub const ACTIVITY_MAX: usize = 50;
/// Most live chat messages kept.
pub const CHAT_MAX: usize = 300;
/// The longest chat reply YouTube takes.
pub const CHAT_REPLY_MAX: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Prepare,
    Countdown,
    StopCountdown,
    /// Back to Idle from Prepared or the countdown: the countdown stops as with Stop,
    /// and the encoder stops. YouTube is left as it is. Refused once the broadcast is
    /// going public: then the service goes on, and End Service ends it.
    Abandon,
    GoPublic,
    GoLive,
    EndService,
    EndBroadcast,
    PlayCue(usize),
    /// Test play a video: in the hall and on Stream ATEM input 8, not on the stream.
    /// Refused while a sequence runs and from the countdown until the service ends.
    TestVideo(Cue),
    /// Stops the playing video, or the test play.
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
    /// Show `input` on destination `dest` of `target`: route the VideoHub output feeding
    /// it. Allowed during a service too, except Stream ATEM input 8 while a sequence runs.
    /// The window and the deck ask first when it changes an input on program.
    Route {
        target: Target,
        dest: usize,
        input: u16,
    },
    /// Show `input` on VideoHub `output` (both 1-based), from the VideoHub page. Refused
    /// when the output feeds a locked destination (the Media input during a sequence).
    RouteOutput {
        output: u16,
        input: u16,
    },
    /// Route `target` to the preset in `slot` (1..=8).
    RecallPreset(Target, u8),
    /// Replace a target's presets (after a save or a clear in the window).
    SetPresets(Target, Vec<Preset>),
    /// Rename input or output `n` (1-based) on the VideoHub.
    SetLabel {
        kind: LabelKind,
        n: u16,
        text: String,
    },
    /// What the VideoHub and the ATEMs report for routing, from the health poll.
    RoutingRead(Box<RoutingRead>),
    /// Fire cue `cue` in cue list `list` on the lighting console.
    FireLightingCue {
        list: u32,
        cue: String,
    },
    /// What the lighting console says now, from the health poll (`None`: not answering).
    LightingRead(Option<LightingStatus>),
    /// YouTube's health for the stream key, from the poll while a service is under way
    /// (`None`: not being checked).
    StreamHealthRead(Option<Health>),
    /// The shown audio inputs as the Stream ATEM reports them, from the health poll.
    AudioRead(Vec<AudioReading>),
    /// The viewer count and chat of the selected broadcast, from the poll while a service
    /// is under way (`None`: not being checked).
    LiveDetailsRead(Option<LiveDetails>),
    /// New live chat messages from the chat poll (`reset`: a different chat, so the old
    /// messages go), or why it could not read.
    ChatRead {
        reset: bool,
        messages: Vec<ChatMessage>,
        error: Option<String>,
    },
    /// Post a reply to the live chat as the channel.
    SendChat(String),
    /// Go to the scene with this id: its hall screens input and its lighting cue.
    GoScene(u32),
    /// Replace the scenes (after an edit on the Scenes tab).
    SetScenes(Vec<Scene>),
}

/// One shown audio input: its name and what the switcher says (`None`: no answer).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioReading {
    pub name: String,
    pub source: AudioSourceId,
    pub input: Option<AudioInput>,
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
    /// The lighting console (ETC Eos), when one is set up.
    Lighting,
    /// The MultiView 4, when one is set up.
    MultiView,
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
    /// The test play running now.
    pub test: Option<TestPlay>,
    /// The countdown video's length while the countdown runs (`stage` has what remains).
    pub countdown_total: Option<Duration>,
    /// When the service went live; cleared when it has ended.
    pub on_air_since: Option<SystemTime>,
    /// What has happened, in plain words, newest first; at most `ACTIVITY_MAX`.
    pub activity: Vec<Activity>,
    /// The engine drives pretend devices: nothing is live.
    pub simulation: bool,
    pub routing: RoutingState,
    /// The lighting console's show and active cue, while it answers.
    pub lighting: Option<LightingStatus>,
    /// YouTube's health for the stream, checked from Prepare until the service ends.
    pub stream_health: Option<Health>,
    /// Live, and YouTube has not been receiving the stream for `STREAM_ALARM_AFTER`.
    pub stream_alarm: bool,
    /// The audio inputs Settings picks to show, as last read.
    pub audio: Vec<AudioReading>,
    /// The selected broadcast's viewer count and chat, while a service is under way.
    pub live: Option<LiveDetails>,
    /// The live chat, oldest first, at most `CHAT_MAX` messages.
    pub chat: Vec<ChatMessage>,
    /// Why the chat could not be read last time, until a read works.
    pub chat_error: Option<String>,
    pub scenes: Vec<Scene>,
    /// The scene gone to last (its id).
    pub active_scene: Option<u32>,
    /// How far the steps of Prepare, the countdown and End Service have got this service.
    /// Prepare starts them all again; the countdown and End Service their own.
    pub steps: Vec<Step>,
    /// Going public has started this service, by the countdown or by hand. The end of the
    /// service clears it.
    pub public_started: bool,
}

impl Snapshot {
    /// The broadcast has gone public (or is going public) before the service is live:
    /// from then on the service only goes on, to Start service now and End Service.
    pub fn public_before_live(&self) -> bool {
        matches!(self.stage, Stage::Prepared(_) | Stage::Countdown { .. }) && self.public_started
    }

    /// Abandon can be pressed: prepared or counting down, still private.
    pub fn can_abandon(&self) -> bool {
        matches!(self.stage, Stage::Prepared(_) | Stage::Countdown { .. }) && !self.public_before_live()
    }
}

/// How far the playing roll-in is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoProgress {
    /// The cue's index in the list.
    pub index: usize,
    pub position: Duration,
    pub duration: Duration,
}

/// A test play: which video, and how far it is (both zero until it starts playing).
#[derive(Debug, Clone, PartialEq)]
pub struct TestPlay {
    pub cue: Cue,
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
            test: None,
            countdown_total: None,
            on_air_since: None,
            activity: Vec::new(),
            simulation: false,
            routing: RoutingState::default(),
            lighting: None,
            stream_health: None,
            stream_alarm: false,
            audio: Vec::new(),
            live: None,
            chat: Vec::new(),
            chat_error: None,
            scenes: Vec::new(),
            active_scene: None,
            steps: Vec::new(),
            public_started: false,
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
    Countdown {
        cancel: CancellationToken,
    },
    End {
        skip: Arc<Notify>,
    },
    Video {
        index: usize,
        stop: CancellationToken,
        unmute: Arc<Notify>,
    },
    Test {
        stop: CancellationToken,
    },
    /// Start service now by hand: T-0, going public alongside it unless already public.
    GoLive(GoLiveStart),
    Recover,
    Abandon,
}

impl Job {
    fn name(&self) -> &'static str {
        match self {
            Job::Prepare => "Prepare",
            Job::Countdown { .. } => "the countdown",
            Job::End { .. } => "End Service",
            Job::Video { .. } => "a video",
            Job::Test { .. } => "a test play",
            Job::GoLive(_) => "Go Live",
            Job::Recover => "Recover",
            Job::Abandon => "Abandon",
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
    Countdown {
        result: devices::Result<CountdownOutcome>,
        public: bool,
        before: Stage,
    },
    Ended(EndOutcome),
    Video {
        index: usize,
        outcome: RollinOutcome,
    },
    Tested {
        name: String,
        outcome: RollinOutcome,
    },
    /// `public` is the go-public's result (`None` if none ran).
    WentLive {
        result: Result<(), T0Error>,
        public: Option<devices::Result<()>>,
        start: GoLiveStart,
    },
    Recovered {
        ok: bool,
        assessment: Assessment,
    },
    /// The encoder's stop.
    Abandoned(devices::Result<()>),
}

struct Refreshed {
    /// Engine epoch and selection generation when the refresh started.
    epoch: u64,
    sel_gen: u64,
    report: Stage0Report,
    selected: Option<String>,
    check: Option<BroadcastCheck>,
    assessment: Assessment,
    /// YouTube did not answer within `REFRESH_TIMEOUT`: the report and selection are empty.
    youtube_timed_out: bool,
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

/// How long YouTube may not receive the stream, while live, before StreamIt raises the
/// alarm (a short gap is normal when the encoder reconnects).
pub const STREAM_ALARM_AFTER: Duration = Duration::from_secs(20);

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
    /// Abandon is waiting for the countdown it stopped to run its cancel path.
    abandon_pending: bool,
    /// Recover / End Service are waiting for the running video's return path
    /// (Recover first, then End).
    recover_pending: bool,
    end_pending: bool,
    flash_gen: u64,
    key_flash_gen: u64,
    aux: mpsc::UnboundedSender<Aux>,
    /// The devices are pretend ones: a sequence still running when the engine stops is
    /// aborted (simulated crash) rather than left to finish.
    simulation: bool,
    /// Since when YouTube has not been receiving the stream, while live.
    not_receiving_since: Option<tokio::time::Instant>,
    /// The playing video went to its own scene; when it ends, go back to the one before.
    video_scene: Option<VideoScene>,
}

/// A video's scene, and the scene that was on before it.
#[derive(Debug, Clone, Copy)]
struct VideoScene {
    scene: u32,
    before: Option<u32>,
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
        let first = Snapshot {
            cues: cue_rows(&cues),
            simulation,
            routing: routing_from(&read(&c)),
            scenes: read(&c).scenes.into_iter().take(MAX_SCENES).collect(),
            ..Snapshot::default()
        };
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
            abandon_pending: false,
            recover_pending: false,
            end_pending: false,
            flash_gen: 0,
            key_flash_gen: 0,
            aux,
            simulation,
            not_receiving_since: None,
            video_scene: None,
        };
        tokio::spawn(engine.run(rx, aux_rx).in_current_span());
        EngineHandle { tx, rx: srx }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Command>, mut aux_rx: mpsc::UnboundedReceiver<Aux>) {
        let r = refresh(self.d.clone(), self.config(), None, false, self.epoch, self.sel_gen).await;
        self.apply_refresh(r, true);
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
        if let Some(r) = self.running.take() {
            if self.simulation {
                // "Crash & restart app": the sequence stops part-way, as it would if the
                // app crashed. Its own sub-tasks (the countdown's go-public) stop with it.
                warn!(job = r.job.name(), "simulated engine stopping with a sequence running: aborting it");
                r.handle.abort();
            } else {
                // v1: a sequence on the real equipment is never cut off part-way; it runs
                // to the end on its own.
                warn!(job = r.job.name(), "engine stopping with a sequence running: letting it finish");
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
        matches!(self.running, Some(Running { job: Job::Video { .. } | Job::Countdown { .. } | Job::Test { .. }, .. }))
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
            Command::StopCountdown if self.snap.borrow().public_before_live() => {
                self.ignore(&cmd, "the broadcast is going public: the service goes on")
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
            Command::TestVideo(cue) => {
                if let Some(why) = self.busy() {
                    set_warning(&self.snap, format!("Test play waits: {why}"));
                    return self.ignore(&cmd, &why);
                }
                if matches!(self.stage(), Stage::Countdown { .. } | Stage::Live | Stage::Ending { .. }) {
                    set_warning(&self.snap, "Test play is off from the countdown until the service ends".into());
                    return self.ignore(&cmd, "the service is under way");
                }
                self.start_test(cue.clone());
            }
            Command::StopVideo => match &self.running {
                Some(Running { job: Job::Video { stop, .. } | Job::Test { stop }, .. }) => stop.cancel(),
                _ => self.ignore(&cmd, "no video is playing"),
            },
            // `mics_ducked` clears only when the roll-in reports a successful fade up.
            Command::UnmuteMics => match &self.running {
                Some(Running { job: Job::Video { unmute, .. }, .. }) => unmute.notify_one(),
                _ => self.ignore(&cmd, "no video is playing"),
            },
            Command::Abandon => {
                let (public, can) = {
                    let s = self.snap.borrow();
                    (s.public_before_live(), s.can_abandon())
                };
                if public {
                    return self.ignore(&cmd, "the broadcast is going public: the service goes on");
                }
                if !can {
                    return self.ignore(&cmd, "nothing is prepared or counting down");
                }
                match &self.running {
                    None => self.start_abandon(),
                    Some(Running { job: Job::Countdown { cancel }, .. })
                        if !self.golive_pending && !self.abandon_pending =>
                    {
                        info!("Abandon during the countdown: stopping the countdown first");
                        cancel.cancel();
                        self.abandon_pending = true;
                    }
                    Some(r) => {
                        let why = format!("{} is running", r.job.name());
                        self.ignore(&cmd, &why);
                    }
                }
            }
            Command::GoPublic => {
                self.set(|s| s.public_started = true);
                self.spawn_go_public();
            }
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
                    Some(Running { job: Job::Countdown { cancel }, .. })
                        if !self.golive_pending && !self.abandon_pending =>
                    {
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
                let (d, c, sel, aux) = (self.d.clone(), self.config(), self.selected(), self.aux.clone());
                let (player_active, epoch, sel_gen) = (self.player_active(), self.epoch, self.sel_gen);
                tokio::spawn(
                    async move {
                        let r = refresh(d, c, sel, player_active, epoch, sel_gen).await;
                        let _ = aux.send(Aux::Refreshed(Box::new(r)));
                    }
                    .in_current_span(),
                );
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
            Command::Route { target, dest, input } => {
                let (target, dest, input) = (*target, *dest, *input);
                if dest >= target.size() || input == 0 {
                    return self.ignore(&cmd, "no such destination or input");
                }
                if let Some(why) = self.locked_why(target, dest) {
                    set_warning(&self.snap, why.clone());
                    return self.ignore(&cmd, &why);
                }
                let what = format!("{} {}: input {input}", target.name(), target.dest_name(dest));
                self.route_dests(target, vec![(dest, input)], what);
            }
            Command::RouteOutput { output, input } => {
                let (output, input) = (*output, *input);
                if output == 0 || input == 0 {
                    return self.ignore(&cmd, "no such output or input");
                }
                let fed = self.snap.borrow().routing.fed_by(output);
                if let Some(why) = fed.iter().find_map(|&(t, d)| self.locked_why(t, d)) {
                    set_warning(&self.snap, why.clone());
                    return self.ignore(&cmd, &why);
                }
                info!(output, input, "routing a VideoHub output");
                let (hub, snap) = (self.d.hub.clone(), self.snap.clone());
                tokio::spawn(
                    async move {
                        match hub.route(output, input).await {
                            Ok(()) => snap.send_modify(|s| {
                                s.routing.set_route(output, input);
                                for (t, d) in fed {
                                    let state = s.routing.target_mut(t);
                                    if state.dests.len() < t.size() {
                                        state.dests.resize(t.size(), None);
                                    }
                                    state.dests[d] = Some(input);
                                }
                            }),
                            Err(e) => set_warning(
                                &snap,
                                format!("VideoHub output {output}: could not route input {input}: {e}"),
                            ),
                        }
                    }
                    .in_current_span(),
                );
            }
            Command::RecallPreset(target, slot) => {
                let target = *target;
                let (p, mut routes) = {
                    let s = self.snap.borrow();
                    let t = s.routing.target(target);
                    let Some(p) = t.preset(*slot).cloned() else {
                        drop(s);
                        return self.ignore(&cmd, "that preset slot is empty");
                    };
                    let routes = t.changes(&p);
                    (p, routes)
                };
                if let Some(why) = routes.iter().find_map(|&(d, _)| self.locked_why(target, d)) {
                    set_warning(&self.snap, format!("{why} The rest of the preset is routed."));
                    routes.retain(|&(d, _)| self.locked_why(target, d).is_none());
                }
                if routes.is_empty() {
                    return self.ignore(&cmd, "nothing to change for that preset");
                }
                self.route_dests(target, routes, format!("{} preset {}, {}", target.name(), p.slot, p.name));
            }
            Command::SetPresets(target, presets) => {
                let (target, presets) = (*target, presets.clone());
                info!(target = target.name(), count = presets.len(), "routing presets replaced");
                self.set(|s| s.routing.target_mut(target).presets = presets);
            }
            Command::SetLabel { kind, n, text } => {
                let (kind, n, text) = (*kind, *n, text.clone());
                if n == 0 {
                    return self.ignore(&cmd, "inputs and outputs are numbered from 1");
                }
                info!(?kind, n, text = %text, "renaming on the VideoHub");
                let (hub, snap) = (self.d.hub.clone(), self.snap.clone());
                tokio::spawn(
                    async move {
                        match hub.set_label(kind, n, &text).await {
                            Ok(()) => snap.send_modify(|s| s.routing.set_label(kind, n, &text)),
                            Err(e) => {
                                let what = format!("{kind:?} {n}").to_lowercase();
                                set_warning(&snap, format!("VideoHub: could not rename {what}: {e}"));
                            }
                        }
                    }
                    .in_current_span(),
                );
            }
            Command::FireLightingCue { list, cue } => {
                let (list, cue) = (*list, cue.trim().to_string());
                if list == 0 || cue.is_empty() {
                    return self.ignore(&cmd, "a cue needs a list (from 1) and a number");
                }
                if self.config().lighting.off {
                    return self.ignore(&cmd, "lighting control is turned off");
                }
                info!(list, cue = %cue, "firing a lighting cue");
                let (lights, snap) = (self.d.lights.clone(), self.snap.clone());
                tokio::spawn(
                    async move {
                        match lights.fire_cue(list, &cue).await {
                            Ok(()) => snap.send_modify(|s| log(s, format!("Lighting · cue {list}/{cue} fired"))),
                            Err(e) => set_warning(&snap, format!("Lighting: could not fire cue {list}/{cue}: {e}")),
                        }
                    }
                    .in_current_span(),
                );
            }
            Command::AudioRead(audio) => {
                let audio = audio.clone();
                self.set(|s| s.audio = audio);
            }
            Command::ChatRead { reset, messages, error } => {
                let (reset, messages, error) = (*reset, messages.clone(), error.clone());
                self.set(|s| {
                    if reset {
                        s.chat.clear();
                    }
                    if error.is_none() {
                        for m in messages {
                            if !s.chat.iter().any(|x| x.id == m.id) {
                                s.chat.push(m);
                            }
                        }
                        let over = s.chat.len().saturating_sub(CHAT_MAX);
                        s.chat.drain(..over);
                    }
                    s.chat_error = error;
                });
            }
            Command::SendChat(text) => {
                let text: String = text.trim().chars().take(CHAT_REPLY_MAX).collect();
                let chat = self.snap.borrow().live.as_ref().and_then(|l| l.chat_id.clone());
                let Some(chat) = chat.filter(|_| !text.is_empty()) else {
                    return self.ignore(&cmd, "no live chat to reply to");
                };
                let (yt, snap) = (self.d.yt.clone(), self.snap.clone());
                tokio::spawn(
                    async move {
                        if let Err(e) = yt.send_chat(&chat, &text).await {
                            set_warning(&snap, format!("Live chat: could not send the reply: {e}"));
                        }
                    }
                    .in_current_span(),
                );
            }
            Command::GoScene(id) => {
                let id = *id;
                if scenes::find(&self.snap.borrow().scenes, id).is_none() {
                    return self.ignore(&cmd, "there is no such scene");
                }
                let hall = self.hall_busy();
                self.go_scene(id, hall);
            }
            Command::SetScenes(list) => {
                let list: Vec<Scene> = list.iter().take(MAX_SCENES).cloned().collect();
                info!(count = list.len(), "scenes replaced");
                self.set(|s| {
                    if s.active_scene.is_some_and(|id| scenes::find(&list, id).is_none()) {
                        s.active_scene = None;
                    }
                    s.scenes = list;
                });
            }
            Command::LiveDetailsRead(live) => {
                let live = live.clone();
                self.set(|s| s.live = live);
            }
            Command::StreamHealthRead(health) => {
                let health = health.clone();
                let live = self.stage() == Stage::Live;
                let receiving = !matches!(health, Some(Health::NotReceiving));
                let was_alarm = self.snap.borrow().stream_alarm;
                let now = tokio::time::Instant::now();
                if !live || receiving {
                    self.not_receiving_since = None;
                } else {
                    self.not_receiving_since.get_or_insert(now);
                }
                let alarm = self.not_receiving_since.is_some_and(|t| now.duration_since(t) >= STREAM_ALARM_AFTER);
                if alarm && !was_alarm {
                    warn!("YouTube is not receiving the stream");
                    set_warning(
                        &self.snap,
                        "YouTube is not receiving the stream: check the Web Presenter and the internet".into(),
                    );
                    self.set(|s| log(s, "YouTube stopped receiving the stream".into()));
                } else if was_alarm && !alarm && live && receiving {
                    info!("YouTube is receiving the stream again");
                    self.set(|s| log(s, "YouTube receiving the stream again".into()));
                }
                self.set(|s| {
                    s.stream_health = health;
                    s.stream_alarm = alarm;
                });
            }
            Command::LightingRead(status) => {
                let status = status.clone();
                self.set(|s| s.lighting = status);
            }
            Command::RoutingRead(read) => {
                let RoutingRead { labels, output_labels, targets, media, routes } = (**read).clone();
                self.set(|s| {
                    s.routing.media = media;
                    if !routes.is_empty() {
                        s.routing.routes = routes;
                    }
                    for (state, read) in s.routing.targets.iter_mut().zip(targets) {
                        state.outputs = read.outputs;
                        state.dests = read.dests;
                        state.program = read.program;
                        state.preview = read.preview;
                        state.stills = read.stills;
                    }
                    if !labels.is_empty() {
                        s.routing.labels = labels;
                    }
                    if !output_labels.is_empty() {
                        s.routing.output_labels = output_labels;
                    }
                });
            }
        }
    }

    /// Why `dest` of `target` cannot be routed now, if it cannot.
    fn locked_why(&self, target: Target, dest: usize) -> Option<String> {
        let job = self.running.as_ref().map(|r| r.job.name());
        let media = self.config().media_dest();
        locked(target, dest, job.is_some(), media).then(|| {
            format!(
                "Stream ATEM input {} carries StreamIt's videos: it can't be routed while {} is running.",
                dest + 1,
                job.unwrap_or("a sequence")
            )
        })
    }

    /// Routes each `(dest, input)` of `target` through the VideoHub output feeding it, one
    /// after another, in the background: routing never waits for, or holds up, a sequence.
    /// Each destination shows its new input once the VideoHub accepts it; a refusal is a
    /// warning.
    fn route_dests(&self, target: Target, routes: Vec<(usize, u16)>, what: String) {
        let outputs = self.config().routing_outputs(target);
        if let Some(&(d, _)) = routes.iter().find(|(d, _)| outputs[*d] == 0) {
            let msg = format!("{} {}: set which router output feeds it (Settings)", target.name(), target.dest_name(d));
            set_warning(&self.snap, msg);
            return;
        }
        info!(what = %what, "routing");
        let (hub, snap) = (self.d.hub.clone(), self.snap.clone());
        tokio::spawn(
            async move {
                for (dest, input) in routes {
                    match hub.route(outputs[dest], input).await {
                        Ok(()) => snap.send_modify(|s| {
                            s.routing.set_route(outputs[dest], input);
                            let t = s.routing.target_mut(target);
                            if t.dests.len() < target.size() {
                                t.dests.resize(target.size(), None);
                            }
                            t.dests[dest] = Some(input);
                        }),
                        Err(e) => {
                            let msg = format!(
                                "{} {}: could not route input {input}: {e}",
                                target.name(),
                                target.dest_name(dest)
                            );
                            set_warning(&snap, msg);
                            return;
                        }
                    }
                }
            }
            .in_current_span(),
        );
    }

    fn flash(&mut self, i: usize) {
        self.flash_gen += 1;
        let generation = self.flash_gen;
        self.set(|s| s.flash = Some(i));
        let aux = self.aux.clone();
        tokio::spawn(
            async move {
                tokio::time::sleep(FLASH_FOR).await;
                let _ = aux.send(Aux::ClearFlash(generation));
            }
            .in_current_span(),
        );
    }

    fn flash_key(&mut self, key: FlashKey) {
        self.key_flash_gen += 1;
        let generation = self.key_flash_gen;
        self.set(|s| s.flash_key = Some(key));
        let aux = self.aux.clone();
        tokio::spawn(
            async move {
                tokio::time::sleep(FLASH_FOR).await;
                let _ = aux.send(Aux::ClearKeyFlash(generation));
            }
            .in_current_span(),
        );
    }

    // ---- jobs ----

    fn start_prepare(&mut self) {
        let (d, c, snap, levels, bid) =
            (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone(), self.bid());
        let file = self.levels_file.clone();
        // A new service: every step starts again.
        self.set(|s| {
            s.steps.clear();
            steps::reset(&mut s.steps, StepGroup::Prepare);
        });
        let handle = tokio::spawn(
            async move {
                let mut l = lock(&levels).clone();
                let (p, w, n, st) = (snap.clone(), snap.clone(), snap.clone(), snap);
                let r = golive::prepare(
                    &d,
                    &c,
                    &bid,
                    &mut l,
                    move |r| p.send_modify(|s| set_prepared(s, r)),
                    move |m| set_warning(&w, m),
                    move |m| n.send_modify(|s| log(s, m)),
                    move |id, m| set_step(&st, id, m),
                )
                .await;
                store_levels(&levels, l, file.as_deref());
                Done::Prepared(r)
            }
            .in_current_span(),
        );
        self.begin(Job::Prepare, handle);
    }

    fn start_countdown(&mut self, before: Stage) {
        let ready = match &before {
            Stage::Prepared(r) => r.clone(),
            _ => Readiness::Warning("Prepare has not been run".into()),
        };
        // Offers a Stop (or a failed countdown) left say nothing about this countdown.
        self.set(|s| {
            remove(&mut s.offers, Offer::GoLive);
            remove(&mut s.offers, Offer::GoPublic);
            steps::reset(&mut s.steps, StepGroup::Countdown);
        });
        let cancel = CancellationToken::new();
        let (d, c, snap, levels, bid, tok) =
            (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone(), self.bid(), cancel.clone());
        let handle = tokio::spawn(
            async move {
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
                            CountdownReport::Step(id, m) => steps::mark(&mut s.steps, id, m),
                        })
                    },
                )
                .await;
                Done::Countdown { result, public: public.load(Ordering::SeqCst), before }
            }
            .in_current_span(),
        );
        self.begin(Job::Countdown { cancel }, handle);
    }

    fn start_abandon(&mut self) {
        let d = self.d.clone();
        let handle = tokio::spawn(async move { Done::Abandoned(d.encoder.stop().await) }.in_current_span());
        self.begin(Job::Abandon, handle);
    }

    /// Back to Idle with nothing started; YouTube is left as it is (still private, so
    /// Prepare can use the same broadcast again).
    fn finish_abandon(&mut self, stopped: devices::Result<()>) {
        self.set(|s| {
            set_stage(s, Stage::Idle);
            s.steps.clear();
            remove(&mut s.offers, Offer::GoLive);
            remove(&mut s.offers, Offer::GoPublic);
            log(s, "Service abandoned".into());
            if let Err(e) = &stopped {
                warn_in(s, format!("Could not stop the encoder: {e}"));
            }
        });
    }

    fn start_end(&mut self) {
        let skip = Arc::new(Notify::new());
        let (d, c, snap, bid, s2) = (self.d.clone(), self.config(), self.snap.clone(), self.bid(), skip.clone());
        self.set(|s| steps::reset(&mut s.steps, StepGroup::End));
        let handle = tokio::spawn(
            async move {
                let st = snap.clone();
                let out = end_service(
                    &d,
                    &c,
                    &bid,
                    s2,
                    move |stage| snap.send_modify(|s| set_stage(s, stage)),
                    move |id, m| set_step(&st, id, m),
                )
                .await;
                Done::Ended(out)
            }
            .in_current_span(),
        );
        self.begin(Job::End { skip }, handle);
    }

    fn start_video(&mut self, index: usize) {
        let Some(cue) = self.cues.get(index).cloned() else { return };
        let (video_scene, in_hall) = (cue.scene, cue.destination.screens_picture());
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
        let handle = tokio::spawn(
            async move {
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
            }
            .in_current_span(),
        );
        self.begin(Job::Video { index, stop, unmute }, handle);
        // The video's scene: in the hall the screens stay on the video, so only its
        // lighting cue fires.
        if let Some(id) = video_scene.filter(|&id| scenes::find(&self.snap.borrow().scenes, id).is_some()) {
            let before = self.snap.borrow().active_scene;
            self.video_scene = Some(VideoScene { scene: id, before });
            self.go_scene(id, in_hall);
        }
    }

    fn start_test(&mut self, cue: Cue) {
        self.set(|s| {
            s.test = Some(TestPlay { cue: cue.clone(), position: Duration::ZERO, duration: Duration::ZERO });
            log(s, format!("Test play · {} · started", cue.name));
        });
        let stop = CancellationToken::new();
        let (d, c, snap, st) = (self.d.clone(), self.config(), self.snap.clone(), stop.clone());
        let handle = tokio::spawn(
            async move {
                let report = |position, duration| {
                    snap.send_modify(|s| {
                        if let Some(t) = &mut s.test {
                            t.position = position;
                            t.duration = duration;
                        }
                    })
                };
                let outcome = rollin::test_play(&d, &c, &cue, st, report).await;
                Done::Tested { name: cue.name, outcome }
            }
            .in_current_span(),
        );
        self.begin(Job::Test { stop }, handle);
    }

    fn finish_test(&mut self, name: &str, outcome: RollinOutcome) {
        self.set(|s| {
            s.test = None;
            match outcome {
                RollinOutcome::Finished => log(s, format!("Test play · {name} · ended")),
                RollinOutcome::Stopped => log(s, format!("Test play · {name} · stopped")),
                RollinOutcome::Refused(why) => warn_in(s, format!("Test play of {name} refused: {why}")),
                RollinOutcome::Failed { recover_needed, reason } => {
                    warn_in(s, format!("Test play of {name}: {reason}"));
                    if recover_needed {
                        add(&mut s.offers, Offer::Recover);
                    }
                }
            }
        });
    }

    /// A countdown, a video or a test play is on the hall screens, so a scene must not
    /// cut them away.
    fn hall_busy(&self) -> bool {
        match &self.running {
            Some(Running { job: Job::Countdown { .. } | Job::Test { .. }, .. }) => true,
            Some(Running { job: Job::Video { index, .. }, .. }) => {
                self.cues.get(*index).is_some_and(|c| c.destination.screens_picture())
            }
            _ => false,
        }
    }

    /// Puts the scene's input on the hall screens (unless `keep_hall`) and fires its
    /// lighting cue. Each part that fails warns; the other still runs.
    fn go_scene(&mut self, id: u32, keep_hall: bool) {
        self.fire_scene(id, keep_hall, " · the hall stays on the video");
    }

    /// Live: the Go Live scene is the scene on now, and its lighting cue fires. T-0 has
    /// already put its input on the hall screens.
    fn t0_scene(&mut self) {
        if let Some(id) = self.config().screens_atem.t0_scene
            && scenes::find(&self.snap.borrow().scenes, id).is_some()
        {
            self.fire_scene(id, true, " · at Go Live");
        }
    }

    /// `go_scene`, saying `kept` after the scene's name when its hall input is left alone.
    fn fire_scene(&mut self, id: u32, keep_hall: bool, kept: &str) {
        let Some(scene) = scenes::find(&self.snap.borrow().scenes, id).cloned() else { return };
        let screens = scene.screens.filter(|_| !keep_hall);
        // Lighting control turned off: the scene changes only the hall screens.
        let lights_on = !self.config().lighting.off;
        let cue = scene.cue.clone().filter(|c| lights_on && !c.cue.trim().is_empty());
        let mut line = format!("Scene · {}", scene.name);
        if keep_hall && scene.screens.is_some() {
            line.push_str(kept);
        }
        info!(scene = %scene.name, ?screens, cue = ?scene.cue, "going to a scene");
        self.set(|s| {
            s.active_scene = Some(id);
            log(s, line);
        });
        let (d, snap) = (self.d.clone(), self.snap.clone());
        tokio::spawn(
            async move {
                if let Some(input) = screens
                    && let Err(e) = d.screens.set_program(input).await
                {
                    set_warning(
                        &snap,
                        format!("Scene {}: could not put input {input} on the hall screens: {e}", scene.name),
                    );
                }
                if let Some(c) = &cue
                    && let Err(e) = d.lights.fire_cue(c.list, &c.cue).await
                {
                    set_warning(
                        &snap,
                        format!("Scene {}: could not fire lighting cue {}/{}: {e}", scene.name, c.list, c.cue),
                    );
                }
            }
            .in_current_span(),
        );
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
        let handle = tokio::spawn(
            async move {
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
            }
            .in_current_span(),
        );
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
        let (d, c, snap, levels, bid) =
            (self.d.clone(), self.config(), self.snap.clone(), self.levels.clone(), self.selected());
        let handle = tokio::spawn(
            async move {
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
            }
            .in_current_span(),
        );
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
            Ok(Done::Tested { name, outcome }) => self.finish_test(&name, outcome),
            Ok(Done::WentLive { result, public, start }) => self.finish_golive(result, public, start),
            Ok(Done::Abandoned(r)) => self.finish_abandon(r),
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
                    Job::Test { .. } => {
                        let name = self.snap.borrow().test.as_ref().map(|t| t.cue.name.clone()).unwrap_or_default();
                        self.finish_test(&name, RollinOutcome::Failed { recover_needed: true, reason })
                    }
                    // Handled like a countdown error: back to Prepared, Go Live / Go Public offered.
                    Job::Countdown { .. } => self.finish_countdown(Err(DeviceError::Other(reason)), false, Stage::Idle),
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
        let abandon = std::mem::take(&mut self.abandon_pending);
        match result {
            // Reached T-0 by itself (even if Go Live was pressed meanwhile): no second t0.
            Ok(CountdownOutcome::Live { public, mics_failed }) => {
                self.set(|s| {
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
                });
                self.t0_scene();
            }
            // Pre-empted by Go Live: the hall is back, so run t0 now. The stage is
            // not left on Countdown; the Go Live job's result sets the final one.
            Ok(CountdownOutcome::Stopped) if golive => {
                self.set(|s| set_stage(s, Stage::Prepared(Readiness::Waiting("Going live".into()))));
                self.start_golive(Some(public));
            }
            Ok(CountdownOutcome::Stopped) if abandon => {
                self.set(|s| log(s, "Countdown stopped".into()));
                self.start_abandon();
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
        if on_air {
            self.t0_scene();
        }
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
        // Back to the scene before the video, unless someone picked another meanwhile.
        if let Some(v) = self.video_scene.take()
            && self.snap.borrow().active_scene == Some(v.scene)
            && let Some(before) = v.before
        {
            self.go_scene(before, false);
        }
    }

    // ---- always-accepted side tasks ----

    fn spawn_go_public(&self) {
        let (d, snap, bid) = (self.d.clone(), self.snap.clone(), self.bid());
        tokio::spawn(
            async move {
                let start = tokio::time::Instant::now();
                match golive::go_public(&d, &bid).await {
                    Ok(()) => snap.send_modify(|s| {
                        remove(&mut s.offers, Offer::GoPublic);
                        log(s, golive::public_note(start.elapsed()));
                    }),
                    Err(e) => snap.send_modify(|s| {
                        s.public_started = false;
                        warn_in(s, format!("Could not make the YouTube broadcast public: {e}"));
                    }),
                }
            }
            .in_current_span(),
        );
    }

    fn spawn_end_broadcast(&self) {
        let (d, snap, bid) = (self.d.clone(), self.snap.clone(), self.bid());
        tokio::spawn(
            async move {
                match golive::transition_with_retry(&d, &bid, Phase::Complete).await {
                    Ok(()) => snap.send_modify(|s| {
                        remove(&mut s.offers, Offer::EndBroadcast);
                        log(s, "YouTube broadcast ended".into());
                    }),
                    Err(e) => set_warning(&snap, format!("Could not end the YouTube broadcast: {e}")),
                }
            }
            .in_current_span(),
        );
    }

    /// Optionally apply `fix` to broadcast `id`, then re-check it.
    fn spawn_check(&self, id: String, fix: Option<Fix>) {
        let (d, snap, aux, sel_gen) = (self.d.clone(), self.snap.clone(), self.aux.clone(), self.sel_gen);
        tokio::spawn(
            async move {
                if let Some(f) = fix
                    && let Err(e) = d.yt.fix(&id, f).await
                {
                    set_warning(&snap, format!("Could not fix the broadcast: {e}"));
                }
                let check = d.yt.check(&id).await.map_err(|e| e.to_string());
                let _ = aux.send(Aux::Checked { sel_gen, check });
            }
            .in_current_span(),
        );
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
            Aux::Refreshed(r) => self.apply_refresh(*r, false),
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
    /// empty one; a failed list read keeps the last list. `startup` words the warning
    /// when YouTube timed out.
    fn apply_refresh(&mut self, r: Refreshed, startup: bool) {
        if r.report.listed {
            self.broadcasts = r.report.broadcasts;
        }
        let current_sel = r.sel_gen == self.sel_gen;
        let broadcast = r.selected.as_deref().map(|id| self.summary_for(id));
        let mut errors = r.report.device_errors;
        if r.youtube_timed_out {
            let secs = REFRESH_TIMEOUT.as_secs();
            errors.insert(
                0,
                if startup {
                    format!(
                        "The startup check did not finish reading YouTube within {secs} s; the equipment was still \
                         checked. Press Refresh to try again"
                    )
                } else {
                    format!("Refresh did not finish reading YouTube within {secs} s; the equipment was still checked")
                },
            );
        }
        let errors = errors.join("; ");
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
    /// never sets the stage and only adds offers. When the broadcast's life cycle could
    /// not be read, the YouTube offers it would have decided are kept as they were;
    /// Recover, which comes from the equipment alone, still follows the assessment.
    fn apply_assessment(&self, a: Assessment, epoch: u64) {
        let current = epoch == self.epoch;
        let busy = self.running.is_some();
        if !current {
            info!("merging the offers of a stale assessment");
        }
        self.set(|s| {
            if current {
                if a.broadcast_unknown {
                    info!("broadcast state unknown: keeping the YouTube offers");
                    let kept: Vec<Offer> = s.offers.iter().copied().filter(|o| *o != Offer::Recover).collect();
                    s.offers = a.offer;
                    for o in kept {
                        add(&mut s.offers, o);
                    }
                } else {
                    s.offers = a.offer;
                }
                // I6: mics left down (after a restart, or a failed fade up) show as down.
                // A running video reports its own fades.
                if !busy {
                    s.mics_ducked = a.mics_down;
                }
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

/// I1: T-0 went live but could not bring the stream audio up. Show them as down and offer
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
        Stage::Ended => {
            s.on_air_since = None;
            s.public_started = false;
        }
        _ => {}
    }
    s.stage = stage;
}

/// Add `text` to the top of the activity list, dropping the oldest past `ACTIVITY_MAX`.
fn log(s: &mut Snapshot, text: String) {
    s.activity.insert(0, Activity { at: SystemTime::now(), text });
    s.activity.truncate(ACTIVITY_MAX);
}

fn set_step(snap: &Shared, id: StepId, m: StepMark) {
    snap.send_modify(|s| {
        if id == StepId::YouTubePublic {
            s.public_started = matches!(m, StepMark::Now(_) | StepMark::Done(_));
        }
        steps::mark(&mut s.steps, id, m)
    });
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
/// only an empty selection takes stage 0's choice. Only the YouTube part is bounded by
/// `REFRESH_TIMEOUT`; on timeout the rest carries on with the operator's selection (if
/// any), so the equipment assessment is always made (a crash left on the DeckLink still
/// offers Recover) and the selected broadcast's state is still asked for.
async fn refresh(
    d: Devices,
    c: Config,
    current: Option<String>,
    player_active: bool,
    epoch: u64,
    sel_gen: u64,
) -> Refreshed {
    let kept = current.clone();
    let youtube = async {
        let mut report = golive::stage0_youtube(&d, &c).await;
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
        (report, selected, check)
    };
    let (mut report, selected, check, youtube_timed_out) = match tokio::time::timeout(REFRESH_TIMEOUT, youtube).await {
        Ok((report, selected, check)) => (report, selected, check, false),
        Err(_) => (Stage0Report::default(), kept, None, true),
    };
    report.device_errors.extend(golive::stage0_devices(&d).await);
    let assessment = recovery::assess(&d, &c, selected.as_deref(), player_active).await;
    Refreshed { epoch, sel_gen, report, selected, check, assessment, youtube_timed_out }
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
            Ok(()) => info!(path = %path.display(), "stream audio levels saved"),
            Err(e) => warn!(error = %format!("{e:#}"), "could not save the stream audio levels"),
        }
    }
}

fn set_warning(snap: &Shared, msg: String) {
    info!(warning = %msg, "warning");
    snap.send_modify(|s| warn_in(s, msg));
}

/// The routing the window and the deck start from: the outputs and presets in `c`,
/// nothing known yet about what is on each destination.
fn routing_from(c: &Config) -> RoutingState {
    RoutingState {
        labels: Vec::new(),
        output_labels: Vec::new(),
        media: c.media_dest(),
        routes: Vec::new(),
        targets: Target::ALL.map(|t| TargetState {
            outputs: c.routing_outputs(t),
            dests: vec![None; t.size()],
            presets: c.routing(t).presets.clone(),
            program: None,
            preview: None,
            stills: Vec::new(),
        }),
    }
}

fn cue_rows(l: &CueList) -> Vec<(Cue, CueState)> {
    l.cues().iter().enumerate().map(|(i, c)| (c.clone(), l.state.get(i).cloned().unwrap_or(CueState::Cued))).collect()
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
    use crate::devices::{Phase, PlayerEvent, Router, Switcher};
    use crate::fakes::{Call, CallLog, FakeHandles, fake_devices};
    use crate::golive::Readiness;
    use crate::rollin::Destination;
    use crate::routing::TargetRead;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tokio::time::{sleep, timeout};

    const OUT: u16 = 20;
    const DK: u16 = 30;
    const CAM8: u16 = 5;
    const MIC: AudioSourceId = AudioSourceId { input: 1001, source: -65280 };
    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn cue(name: &str, destination: Destination) -> Cue {
        Cue {
            path: PathBuf::from(format!("{name}.mp4")),
            name: name.into(),
            destination,
            gain_db: 0.0,
            return_input: None,
            scene: None,
        }
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

    fn marks(s: &Snapshot, g: StepGroup) -> Vec<StepMark> {
        steps::of_group(&s.steps, g).into_iter().map(|x| x.mark).collect()
    }

    fn all_done(s: &Snapshot, g: StepGroup) -> bool {
        marks(s, g).iter().all(|m| matches!(m, StepMark::Done(_)))
    }

    #[tokio::test(start_paused = true)]
    async fn steps_go_done_through_a_whole_service() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        assert!(snap(&e).steps.is_empty());
        send(&e, Command::Prepare).await;
        let s = until(&mut e, "Prepared(Ready)", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
        assert!(all_done(&s, StepGroup::Prepare), "{:?}", marks(&s, StepGroup::Prepare));
        assert!(marks(&s, StepGroup::Countdown).iter().all(|m| *m == StepMark::Todo));
        send(&e, Command::Countdown).await;
        let s = until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        assert!(
            matches!(marks(&s, StepGroup::Countdown)[2], StepMark::Now(_)),
            "{:?}",
            marks(&s, StepGroup::Countdown)
        );
        h.player.emit(PlayerEvent::Ended);
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        // Hall only: the stream step is never reached (the screens leave it out).
        let m = marks(&s, StepGroup::Countdown);
        assert!(
            m.iter().enumerate().all(|(i, m)| matches!((i, m), (1, StepMark::Todo) | (_, StepMark::Done(_)))),
            "{m:?}"
        );
        send(&e, Command::EndService).await;
        let s = until(&mut e, "Ending", |s| matches!(s.stage, Stage::Ending { .. })).await;
        assert!(matches!(marks(&s, StepGroup::End)[2], StepMark::Now(_)), "{:?}", marks(&s, StepGroup::End));
        let s = until(&mut e, "Ended", |s| s.stage == Stage::Ended && s.job.is_none()).await;
        assert!(all_done(&s, StepGroup::End), "{:?}", marks(&s, StepGroup::End));
        assert!(all_done(&s, StepGroup::Prepare), "the earlier stages keep their marks");
        // The next service starts every step again.
        send(&e, Command::Prepare).await;
        let s = until(&mut e, "Preparing", |s| s.job.is_some()).await;
        assert!(marks(&s, StepGroup::End).iter().all(|m| *m == StepMark::Todo));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_step_says_why() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        h.encoder.fail_next("start");
        send(&e, Command::Prepare).await;
        let s = until(&mut e, "Prepared(Warning)", |s| matches!(s.stage, Stage::Prepared(Readiness::Warning(_)))).await;
        let m = &marks(&s, StepGroup::Prepare)[3];
        assert!(matches!(m, StepMark::Failed(_)), "{m:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn test_play_runs_reports_and_stops() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::TestVideo(cue("Hymn", Destination::OnlineOnly))).await;
        let s = until(&mut e, "test playing", |s| s.test.as_ref().is_some_and(|t| t.duration > Duration::ZERO)).await;
        assert_eq!(s.job, Some("a test play"));
        assert_eq!(s.playing, None, "a test play is not a video in the service");
        // Another video waits for it.
        send(&e, Command::PlayCue(0)).await;
        send(&e, Command::StopVideo).await;
        let s = until(&mut e, "test stopped", |s| s.test.is_none() && s.job.is_none()).await;
        assert!(s.activity.iter().any(|a| a.text == "Test play · Hymn · stopped"), "{:?}", s.activity);
        assert!(!log.calls().iter().any(|c| matches!(c, Call::StreamSetPreview(_) | Call::StreamAuto)));
        assert_eq!(h.screens.program().await.unwrap(), 1);
        assert_eq!(h.hub.route_of(OUT).await.unwrap(), CAM8);
    }

    #[tokio::test(start_paused = true)]
    async fn test_play_refused_once_the_service_is_under_way() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.player.emit(PlayerEvent::Ended);
        until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        let loads = log.calls().iter().filter(|c| matches!(c, Call::PlayerLoad(..))).count();
        send(&e, Command::TestVideo(cue("Hymn", Destination::HallOnly))).await;
        let s = until(&mut e, "warned", |s| s.last_warning.as_deref().is_some_and(|w| w.contains("Test play"))).await;
        assert!(s.test.is_none());
        assert_eq!(log.calls().iter().filter(|c| matches!(c, Call::PlayerLoad(..))).count(), loads);
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
    async fn abandon_from_prepared_stops_the_encoder_and_goes_back_to_idle() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Prepare).await;
        until(&mut e, "Prepared(Ready)", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
        assert!(snap(&e).can_abandon());
        send(&e, Command::Abandon).await;
        let s = until(&mut e, "Idle", |s| s.stage == Stage::Idle && s.job.is_none()).await;
        assert!(s.steps.is_empty(), "{:?}", s.steps);
        assert!(log.calls().ends_with(&[Call::EncoderStop]), "{:?}", log.calls());
        // YouTube is left as it is.
        assert_eq!(count(&log, |c| *c == Call::YtTransition(Phase::Complete)), 0);
        // A new service can be prepared.
        send(&e, Command::Prepare).await;
        until(&mut e, "Prepared again", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn abandon_during_the_countdown_stops_it_then_goes_back_to_idle() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Prepare).await;
        until(&mut e, "Prepared(Ready)", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        send(&e, Command::Abandon).await;
        let s = until(&mut e, "Idle", |s| s.stage == Stage::Idle && s.job.is_none()).await;
        assert!(!s.offers.contains(&Offer::GoLive) && !s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
        let calls = log.calls();
        let stop = calls.iter().position(|c| *c == Call::PlayerStop).expect("the countdown video stopped");
        let enc = calls.iter().position(|c| *c == Call::EncoderStop).expect("the encoder stopped");
        assert!(stop < enc, "{calls:?}");
        assert_eq!(count(&log, |c| matches!(c, Call::YtTransition(_))), 1, "only Prepare's testing: {calls:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn once_going_public_the_countdown_cannot_be_abandoned_or_stopped() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Prepare).await;
        until(&mut e, "Prepared(Ready)", |s| s.stage == Stage::Prepared(Readiness::Ready)).await;
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        send(&e, Command::GoPublic).await;
        until_call(&log, Call::YtTransition(Phase::Live)).await;
        let s = snap(&e);
        assert!(s.public_before_live() && !s.can_abandon());
        send(&e, Command::Abandon).await;
        send(&e, Command::StopCountdown).await;
        sleep(Duration::from_secs(2)).await;
        assert!(matches!(snap(&e).stage, Stage::Countdown { .. }), "{:?}", snap(&e).stage);
        assert!(!log.calls().contains(&Call::EncoderStop));
        assert!(!log.calls().contains(&Call::PlayerStop));
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
        h.yt.set_check(BroadcastCheck {
            bound_to_ours: false,
            auto_start: false,
            auto_stop: true,
            monitor_stream: true,
        });
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
        sleep(MS(50)).await; // the countdown has returned; t0 is still fading the stream audio up
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
        assert!(snap(&e).mics_ducked, "the fade up failed, so the stream audio is still down");
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

    /// A countdown started again after a Stop drops the Go Live / Go Public offers the
    /// stop left: nothing has failed in the new countdown.
    #[tokio::test(start_paused = true)]
    async fn restarted_countdown_clears_the_stop_offers() {
        let (d, _log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        send(&e, Command::StopCountdown).await;
        until(&mut e, "offers after the stop", |s| s.job.is_none() && s.offers.contains(&Offer::GoLive)).await;
        send(&e, Command::Countdown).await;
        let s = until(&mut e, "Countdown again", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        assert!(!s.offers.contains(&Offer::GoLive), "{:?}", s.offers);
        assert!(!s.offers.contains(&Offer::GoPublic), "{:?}", s.offers);
    }

    /// I6: after a restart into Live with the mics left at the floor, the snapshot shows
    /// them down (and Recover offered) from the assessment.
    #[tokio::test(start_paused = true)]
    async fn restart_into_live_with_mics_down_shows_them_down() {
        let (d, _log, h) = rig();
        h.encoder.set_status(crate::devices::EncoderStatus::Streaming);
        h.yt.set_life_cycle("live");
        h.stream.set_audio_level_state(MIC, crate::audio::FLOOR_DB);
        let mut e = Engine::spawn(d, cfg(vec![]));
        let s = until(&mut e, "Live, Recover", |s| s.stage == Stage::Live && s.offers.contains(&Offer::Recover)).await;
        assert!(s.mics_ducked, "the stream audio is down: {s:?}");
        // Mics up again (put right by hand) and a Refresh: no longer shown down.
        h.stream.set_audio_level_state(MIC, -3.0);
        send(&e, Command::Refresh).await;
        let s = until(&mut e, "stream audio up", |s| !s.mics_ducked).await;
        assert!(!s.offers.contains(&Offer::Recover), "{:?}", s.offers);
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
        assert!(s.mics_ducked, "the stream audio did not come up");
        assert!(s.offers.contains(&Offer::Recover), "{:?}", s.offers);

        // The countdown's own T-0.
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        h.stream.fail_next("set_audio_level");
        h.player.emit(PlayerEvent::Ended);
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert!(s.mics_ducked, "the stream audio did not come up");
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

    /// The timeout bounds only YouTube: the device assessment always runs and applies,
    /// at startup and on Refresh.
    #[tokio::test(start_paused = true)]
    async fn slow_youtube_still_applies_device_assessment() {
        let (d, _log, h) = rig();
        let c = cfg(vec![]);
        let dk = c.read().unwrap().stream_atem.decklink_input;
        h.yt.set_delay("upcoming", Duration::from_secs(30));
        h.stream.set_program_state(dk); // left on the DeckLink by a crash mid-video
        let start = tokio::time::Instant::now();
        let mut e = Engine::spawn(d, c);
        let s = until(&mut e, "Recover offered", |s| s.offers.contains(&Offer::Recover)).await;
        assert!(start.elapsed() <= REFRESH_TIMEOUT + Duration::from_secs(1), "took {:?}", start.elapsed());
        assert_eq!(
            s.last_warning.as_deref(),
            Some(
                "The startup check did not finish reading YouTube within 10 s; the equipment was still checked. \
                 Press Refresh to try again"
            )
        );

        // A manual Refresh: the device reading still lands while YouTube is slow.
        h.stream.set_program_state(1);
        let start = tokio::time::Instant::now();
        send(&e, Command::Refresh).await;
        let s = until(&mut e, "Recover withdrawn", |s| !s.offers.contains(&Offer::Recover)).await;
        assert!(start.elapsed() <= REFRESH_TIMEOUT + Duration::from_secs(1), "took {:?}", start.elapsed());
        assert!(
            s.last_warning.as_deref().is_some_and(|w| w.contains("Refresh did not finish")),
            "{:?}",
            s.last_warning
        );
    }

    /// Live on the holding still after a restart (Go Live and End Broadcast offered). A
    /// Refresh while YouTube is slow keeps the selection and those offers.
    #[tokio::test(start_paused = true)]
    async fn refresh_with_slow_youtube_keeps_offers() {
        for slow_life_cycle in [false, true] {
            let (d, _log, h) = rig();
            let c = cfg(vec![]);
            let still = d.stream.media_player_input(c.read().unwrap().stream_atem.media_player);
            h.stream.set_program_state(still);
            h.encoder.set_status(crate::devices::EncoderStatus::Streaming);
            h.yt.set_life_cycle("live");
            let mut e = Engine::spawn(d, c);
            let want = [Offer::GoLive, Offer::EndBroadcast];
            let s = until(&mut e, "Live with offers", |s| {
                s.stage == Stage::Live && want.iter().all(|o| s.offers.contains(o))
            })
            .await;
            assert_eq!(selected_id(&s).as_deref(), Some("b1"));

            h.yt.set_delay("upcoming", Duration::from_secs(60));
            if slow_life_cycle {
                h.yt.set_delay("life_cycle", Duration::from_secs(60));
            }
            let start = tokio::time::Instant::now();
            send(&e, Command::Refresh).await;
            let s = until(&mut e, "the refresh's warning", |s| {
                s.last_warning.as_deref().is_some_and(|w| w.contains("Refresh did not finish"))
            })
            .await;
            sleep(MS(100)).await;
            let s2 = snap(&e);
            let bound = REFRESH_TIMEOUT + recovery::LIFE_CYCLE_TIMEOUT + Duration::from_secs(1);
            assert!(start.elapsed() <= bound, "slow life cycle {slow_life_cycle}: took {:?}", start.elapsed());
            assert_eq!(selected_id(&s).as_deref(), Some("b1"), "slow life cycle {slow_life_cycle}");
            assert!(s.check.is_some(), "slow life cycle {slow_life_cycle}: the last check is kept");
            for o in want {
                assert!(s2.offers.contains(&o), "slow life cycle {slow_life_cycle}: {o:?} dropped: {:?}", s2.offers);
            }
            assert_eq!(s2.stage, Stage::Live);
        }
    }

    /// Only a foreign broadcast is active: nothing is selected, and Go Live goes on air
    /// locally with the no-broadcast warning, never touching that broadcast.
    #[tokio::test(start_paused = true)]
    async fn stale_foreign_broadcast_not_selected_and_go_live_warns() {
        let (d, log, h) = rig();
        h.yt.set_upcoming(vec![BroadcastSummary {
            life_cycle: "live".into(),
            bound_stream_id: Some("someone-else".into()),
            ..summary("foreign")
        }]);
        let c = cfg(vec![]);
        c.write().unwrap().youtube.stream_id = Some("s1".into());
        let mut e = Engine::spawn(d, c);
        sleep(MS(500)).await;
        assert_eq!(selected_id(&snap(&e)), None);
        send(&e, Command::GoLive).await;
        let s = until(&mut e, "Live", |s| s.stage == Stage::Live && s.job.is_none()).await;
        assert_eq!(selected_id(&s), None);
        assert_eq!(s.last_warning.as_deref(), Some(NO_BROADCAST));
        assert_eq!(live_transitions(&log), 0, "{:?}", log.calls());
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
        until(&mut e, "the refresh's warning", |s| s.last_warning.as_deref().is_some_and(|w| w.contains("broadcasts")))
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

    /// In simulation, closing the command channel mid-video ("Crash & restart app")
    /// aborts the video: its return path never runs, and the engine's snapshot closes.
    #[tokio::test(start_paused = true)]
    async fn stopping_a_simulated_engine_aborts_its_running_sequence() {
        let (d, log, h) = rig();
        h.player.set_duration(Duration::from_secs(60));
        let c = cfg(vec![cue("a", Destination::HallOnly)]);
        let EngineHandle { tx, mut rx } = Engine::spawn_with(d, c, LiveLevels::default(), None, true);
        tx.send(Command::PlayCue(0)).await.unwrap();
        until_call(&log, Call::PlayerPlay).await;
        let before = log.calls().len();
        drop(tx);
        let closed = tokio::time::timeout(Duration::from_secs(5), async { while rx.changed().await.is_ok() {} }).await;
        assert!(closed.is_ok(), "the engine and its sequence let go of the snapshot");
        h.player.emit(PlayerEvent::Ended);
        sleep(Duration::from_secs(30)).await;
        assert_eq!(log.calls().len(), before, "no return path ran: {:?}", &log.calls()[before..]);
    }

    /// On the real equipment (v1) a sequence is never cut off part-way: when the engine's
    /// command channel closes mid-video, the video still runs its return path.
    #[tokio::test(start_paused = true)]
    async fn stopping_a_real_engine_lets_its_sequence_finish() {
        let (d, log, h) = rig();
        h.player.set_duration(Duration::from_secs(60));
        let EngineHandle { tx, rx } = Engine::spawn(d, cfg(vec![cue("a", Destination::HallOnly)]));
        tx.send(Command::PlayCue(0)).await.unwrap();
        until_call(&log, Call::PlayerPlay).await;
        let before = log.calls().len();
        drop(tx);
        sleep(Duration::from_secs(1)).await;
        h.player.emit(PlayerEvent::Ended);
        sleep(Duration::from_secs(30)).await;
        let after = log.calls()[before..].to_vec();
        // The hall went back (counter cleared, screens cut away from the DeckLink).
        assert!(after.ends_with(&[Call::CounterClear, Call::ScreensAuto]), "the return path ran: {after:?}");
        drop(rx);
    }

    #[tokio::test(start_paused = true)]
    async fn video_progress_is_published_while_playing() {
        let (d, log, h) = rig();
        h.player.set_duration(Duration::from_secs(60));
        h.player.set_position(Some(Duration::ZERO));
        let mut e =
            Engine::spawn(d, cfg(vec![cue("a", Destination::HallOnly), cue("Mission update", Destination::HallOnly)]));
        send(&e, Command::PlayCue(1)).await;
        until_call(&log, Call::PlayerPlay).await;
        sleep(Duration::from_secs(10)).await;
        h.player.set_position(Some(Duration::from_secs(10)));
        let s =
            until(&mut e, "10 s in", |s| s.video.as_ref().is_some_and(|v| v.position == Duration::from_secs(10))).await;
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
        let live = format!("Live · Cam 1 on air · stream audio up · hall → input {t0}");
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
        assert!(at("Web Presenter streaming") < at("Opening still on air · stream audio silent"), "{t:?}");
        assert!(at("Opening still on air · stream audio silent") < at(NO_BROADCAST), "{t:?}");
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
        // Live and bound to our stream (only then is an active broadcast the default).
        h.yt.set_upcoming(vec![BroadcastSummary {
            life_cycle: "live".into(),
            bound_stream_id: Some("s1".into()),
            ..summary("b1")
        }]);
        let c = cfg(vec![]);
        c.write().unwrap().youtube.stream_id = Some("s1".into());
        let mut e = Engine::spawn(d, c);
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

    // ---- routing

    /// The MultiView fed by VideoHub outputs 37..=40; the Stream ATEM's inputs 1-4 by 1-4.
    fn rt_cfg() -> Arc<RwLock<Config>> {
        let c = cfg(vec![]);
        {
            let mut w = c.write().unwrap();
            w.multiview.outputs = vec![37, 38, 39, 40];
            w.stream_routing.outputs = vec![1, 2, 3, 4];
        }
        c
    }

    fn preset(slot: u8, inputs: &[u16]) -> Preset {
        Preset { slot, name: format!("P{slot}"), inputs: inputs.to_vec() }
    }

    fn read(mv: [u16; 4], stream: [u16; 4], program: u16) -> Command {
        let mut targets: [TargetRead; 3] = Default::default();
        targets[0] =
            TargetRead { outputs: vec![37, 38, 39, 40], dests: mv.map(Some).to_vec(), ..TargetRead::default() };
        let mut dests: Vec<Option<u16>> = stream.map(Some).to_vec();
        dests.resize(8, None);
        targets[1] = TargetRead {
            outputs: vec![1, 2, 3, 4, 0, 0, 0, OUT],
            dests,
            program: Some(program),
            preview: None,
            stills: vec![],
        };
        Command::RoutingRead(Box::new(RoutingRead {
            labels: vec![],
            output_labels: vec![],
            targets,
            media: Some(7),
            routes: vec![],
        }))
    }

    fn hub_routes(log: &CallLog) -> Vec<Call> {
        log.calls().into_iter().filter(|c| matches!(c, Call::HubRoute { output, .. } if *output != OUT)).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn route_routes_the_output_feeding_the_destination() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, rt_cfg());
        send(&e, Command::Route { target: Target::MultiView, dest: 2, input: 12 }).await;
        until_call(&log, Call::HubRoute { output: 39, input: 12 }).await;
        let s = until(&mut e, "view 2 on input 12", |s| s.routing.target(Target::MultiView).dest(2) == Some(12)).await;
        assert_eq!(s.routing.target(Target::MultiView).dests, vec![None, None, Some(12), None]);
        send(&e, Command::Route { target: Target::StreamAtem, dest: 3, input: 25 }).await;
        until_call(&log, Call::HubRoute { output: 4, input: 25 }).await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_output_routes_from_the_videohub_page_unless_it_feeds_a_locked_input() {
        let (d, log, _h) = rig();
        let c = rt_cfg();
        c.write().unwrap().videohub.input8_output = OUT;
        let mut e = Engine::spawn(d, c);
        // An output that feeds nothing StreamIt knows.
        send(&e, Command::RouteOutput { output: 25, input: 7 }).await;
        until_call(&log, Call::HubRoute { output: 25, input: 7 }).await;
        let s = until(&mut e, "output 25 on input 7", |s| s.routing.route(25) == Some(7)).await;
        assert_eq!(s.routing.outputs_of(7), vec![25]);
        // An output that feeds a MultiView view: the view shows it too.
        send(&e, Command::RouteOutput { output: 38, input: 12 }).await;
        until(&mut e, "view 1 on input 12", |s| s.routing.target(Target::MultiView).dest(1) == Some(12)).await;
        // The Media input's output, while a sequence runs: refused.
        send(&e, Command::Countdown).await;
        until(&mut e, "Countdown", |s| matches!(s.stage, Stage::Countdown { .. })).await;
        send(&e, Command::RouteOutput { output: OUT, input: 3 }).await;
        until(&mut e, "locked", |s| s.last_warning.as_deref().is_some_and(|w| w.contains("carries StreamIt's videos")))
            .await;
        assert!(!log.calls().contains(&Call::HubRoute { output: OUT, input: 3 }));
    }

    #[tokio::test(start_paused = true)]
    async fn route_works_during_a_service() {
        let (d, log, h) = rig();
        h.yt.set_life_cycle("live");
        let mut e = Engine::spawn(d, rt_cfg());
        until(&mut e, "live", |s| s.stage == Stage::Live).await;
        send(&e, Command::Route { target: Target::StreamAtem, dest: 0, input: 3 }).await;
        until_call(&log, Call::HubRoute { output: 1, input: 3 }).await;
    }

    #[tokio::test(start_paused = true)]
    async fn stream_input_8_uses_the_video_output_and_locks_while_a_sequence_runs() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, rt_cfg());
        assert_eq!(snap(&e).routing.target(Target::StreamAtem).output(7), OUT, "the input-8 output");
        send(&e, Command::Countdown).await;
        until(&mut e, "the countdown", |s| s.job.is_some()).await;
        send(&e, Command::Route { target: Target::StreamAtem, dest: 7, input: 9 }).await;
        let s = until(&mut e, "a warning", |s| s.last_warning.as_deref().is_some_and(|w| w.contains("input 8"))).await;
        assert!(s.last_warning.unwrap().contains("the countdown"), "names what is running");
        assert!(!log.calls().contains(&Call::HubRoute { output: OUT, input: 9 }));
    }

    #[tokio::test(start_paused = true)]
    async fn a_destination_not_fed_by_the_router_warns_and_routes_nothing() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, rt_cfg());
        send(&e, Command::Route { target: Target::ScreensAtem, dest: 1, input: 3 }).await;
        let s =
            until(&mut e, "a warning", |s| s.last_warning.as_deref().is_some_and(|w| w.contains("Screens ATEM"))).await;
        assert!(s.last_warning.unwrap().contains("Input 2"));
        assert!(hub_routes(&log).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_route_warns_and_keeps_the_destination() {
        let (d, _log, h) = rig();
        let mut e = Engine::spawn(d, rt_cfg());
        send(&e, read([1, 2, 3, 4], [1, 2, 3, 4], 1)).await;
        until(&mut e, "the read", |s| s.routing.target(Target::MultiView).dest(0) == Some(1)).await;
        h.hub.fail_next("route");
        send(&e, Command::Route { target: Target::MultiView, dest: 0, input: 9 }).await;
        let s = until(&mut e, "a warning", |s| s.last_warning.as_deref().is_some_and(|w| w.contains("Top left"))).await;
        assert_eq!(s.routing.target(Target::MultiView).dest(0), Some(1));
    }

    #[tokio::test(start_paused = true)]
    async fn recall_routes_only_what_differs() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, rt_cfg());
        send(&e, Command::SetPresets(Target::MultiView, vec![preset(3, &[1, 2, 7, 8])])).await;
        send(&e, read([1, 2, 3, 4], [1, 2, 3, 4], 1)).await;
        until(&mut e, "the read", |s| s.routing.target(Target::MultiView).dest(0) == Some(1)).await;
        send(&e, Command::RecallPreset(Target::MultiView, 3)).await;
        let s = until(&mut e, "the preset", |s| {
            s.routing.target(Target::MultiView).dests == vec![Some(1), Some(2), Some(7), Some(8)]
        })
        .await;
        assert!(s.routing.target(Target::MultiView).is_current(&preset(3, &[1, 2, 7, 8])));
        assert_eq!(
            hub_routes(&log),
            vec![Call::HubRoute { output: 39, input: 7 }, Call::HubRoute { output: 40, input: 8 }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_atem_preset_skips_inputs_not_fed_by_the_router() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, rt_cfg());
        send(&e, Command::SetPresets(Target::StreamAtem, vec![preset(1, &[5, 2, 3, 4, 9, 0, 0, 0])])).await;
        send(&e, read([1, 2, 3, 4], [1, 2, 3, 4], 2)).await;
        until(&mut e, "the read", |s| s.routing.target(Target::StreamAtem).dest(0) == Some(1)).await;
        send(&e, Command::RecallPreset(Target::StreamAtem, 1)).await;
        until_call(&log, Call::HubRoute { output: 1, input: 5 }).await;
        send(&e, Command::Refresh).await;
        sleep(MS(500)).await;
        assert_eq!(hub_routes(&log), vec![Call::HubRoute { output: 1, input: 5 }], "input 5 is not fed");
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_preset_does_nothing() {
        let (d, log, _h) = rig();
        let e = Engine::spawn(d, rt_cfg());
        send(&e, Command::RecallPreset(Target::StreamAtem, 5)).await;
        send(&e, Command::Refresh).await;
        sleep(MS(500)).await;
        assert!(hub_routes(&log).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn presets_start_from_the_config_and_reads_keep_the_labels() {
        let (d, _log, _h) = rig();
        let c = rt_cfg();
        c.write().unwrap().screens_routing.presets = vec![preset(2, &[1, 1])];
        let mut e = Engine::spawn(d, c);
        assert_eq!(snap(&e).routing.target(Target::ScreensAtem).presets, vec![preset(2, &[1, 1])]);
        let mut first = read([1; 4], [1; 4], 1);
        if let Command::RoutingRead(read) = &mut first {
            read.labels = vec!["Cam 1".into()];
        }
        send(&e, first).await;
        until(&mut e, "labels", |s| s.routing.label(1) == "Cam 1").await;
        // A read without labels (the VideoHub did not resend them) keeps the ones it had.
        send(&e, read([2; 4], [2; 4], 3)).await;
        let s = until(&mut e, "the second read", |s| s.routing.target(Target::MultiView).dest(0) == Some(2)).await;
        assert_eq!(s.routing.label(1), "Cam 1");
        assert_eq!(s.routing.target(Target::StreamAtem).program, Some(3));
        assert!(s.routing.target(Target::StreamAtem).on_air(2));
    }

    #[tokio::test(start_paused = true)]
    async fn set_label_renames_on_the_videohub_and_shows_it() {
        let (d, _log, _h) = rig();
        let mut e = Engine::spawn(d, rt_cfg());
        // The fake router cannot rename: the refusal is a warning and nothing changes.
        send(&e, Command::SetLabel { kind: LabelKind::Output, n: 37, text: "MV top left".into() }).await;
        let s = until(&mut e, "a warning", |s| s.last_warning.as_deref().is_some_and(|w| w.contains("rename"))).await;
        assert!(s.last_warning.unwrap().contains("output 37"));
        assert_eq!(s.routing.own_output_label(37), None);
    }

    // ---- lighting

    #[tokio::test(start_paused = true)]
    async fn firing_a_lighting_cue_logs_it_and_a_refusal_warns() {
        let (d, log, h) = rig();
        let c = cfg(vec![]);
        let mut e = Engine::spawn(d, c.clone());
        send(&e, Command::FireLightingCue { list: 1, cue: " 5.5 ".into() }).await;
        until_call(&log, Call::LightsFireCue(1, "5.5".into())).await;
        until(&mut e, "the activity line", |s| s.activity.iter().any(|a| a.text == "Lighting · cue 1/5.5 fired")).await;
        h.lights.fail_next("fire_cue");
        send(&e, Command::FireLightingCue { list: 2, cue: "9".into() }).await;
        until(&mut e, "a warning", |s| s.last_warning.as_deref().is_some_and(|w| w.contains("cue 2/9"))).await;
        // Nothing to fire: ignored, the console is not called.
        send(&e, Command::FireLightingCue { list: 0, cue: "1".into() }).await;
        send(&e, Command::Refresh).await;
        sleep(MS(200)).await;
        assert!(!log.calls().contains(&Call::LightsFireCue(0, "1".into())));
        // Lighting control turned off: neither a cue by hand nor a scene's cue is sent.
        c.write().unwrap().lighting.off = true;
        let scenes = vec![scene(1, "Worship", Some(1), Some((1, "2")))];
        send(&e, Command::SetScenes(scenes)).await;
        send(&e, Command::FireLightingCue { list: 1, cue: "7".into() }).await;
        send(&e, Command::GoScene(1)).await;
        until_call(&log, Call::ScreensSetProgram(1)).await;
        sleep(MS(200)).await;
        assert!(!log.calls().contains(&Call::LightsFireCue(1, "7".into())));
        assert!(!log.calls().contains(&Call::LightsFireCue(1, "2".into())));
    }

    #[tokio::test(start_paused = true)]
    async fn the_lighting_status_follows_the_poll() {
        let (d, _log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        let st = LightingStatus { show: Some("Sunday".into()), active_cue: Some("1/5".into()) };
        send(&e, Command::LightingRead(Some(st.clone()))).await;
        until(&mut e, "the status", |s| s.lighting == Some(st.clone())).await;
        send(&e, Command::LightingRead(None)).await;
        until(&mut e, "no status", |s| s.lighting.is_none()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn audio_and_live_details_follow_the_poll() {
        let (d, _log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        let src = AudioSourceId { input: 1002, source: -65280 };
        let audio = vec![AudioReading {
            name: "Band".into(),
            source: src,
            input: Some(AudioInput { mix: devices::AudioMix::On, fader_db: -3.0 }),
        }];
        send(&e, Command::AudioRead(audio.clone())).await;
        until(&mut e, "the audio", |s| s.audio == audio).await;
        let live = LiveDetails { viewers: Some(12), chat_id: Some("chat".into()) };
        send(&e, Command::LiveDetailsRead(Some(live.clone()))).await;
        until(&mut e, "the details", |s| s.live == Some(live.clone())).await;
        send(&e, Command::LiveDetailsRead(None)).await;
        until(&mut e, "no details", |s| s.live.is_none()).await;
    }

    fn chat_msg(id: &str, text: &str) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            author: "Ruth".into(),
            role: devices::ChatRole::Viewer,
            text: text.into(),
            published: "2026-10-25T10:31:00Z".into(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn chat_reads_append_once_and_a_new_chat_starts_afresh() {
        let (d, _log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        let read = |reset, messages| Command::ChatRead { reset, messages, error: None };
        send(&e, read(false, vec![chat_msg("a", "Morning"), chat_msg("b", "Hello")])).await;
        send(&e, read(false, vec![chat_msg("b", "Hello"), chat_msg("c", "Lovely")])).await;
        let s = until(&mut e, "three messages", |s| s.chat.len() == 3).await;
        assert_eq!(s.chat.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        let error = Command::ChatRead { reset: false, messages: vec![], error: Some("quotaExceeded".into()) };
        send(&e, error).await;
        let s = until(&mut e, "the error", |s| s.chat_error.is_some()).await;
        assert_eq!(s.chat.len(), 3, "an error keeps the messages");
        send(&e, read(true, vec![])).await;
        until(&mut e, "a fresh chat", |s| s.chat.is_empty() && s.chat_error.is_none()).await;
        let many: Vec<ChatMessage> = (0..CHAT_MAX + 5).map(|i| chat_msg(&i.to_string(), "x")).collect();
        send(&e, read(false, many)).await;
        let s = until(&mut e, "the newest kept", |s| s.chat.len() == CHAT_MAX).await;
        assert_eq!(s.chat[0].id, "5");
    }

    #[tokio::test(start_paused = true)]
    async fn a_chat_reply_posts_to_the_chat_and_a_failure_warns() {
        let (d, log, h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        send(&e, Command::SendChat("ignored: no chat yet".into())).await;
        let live = LiveDetails { viewers: None, chat_id: Some("chat-1".into()) };
        send(&e, Command::LiveDetailsRead(Some(live))).await;
        until(&mut e, "the chat", |s| s.live.is_some()).await;
        send(&e, Command::SendChat("  Welcome everyone!  ".into())).await;
        until_call(&log, Call::YtSendChat("chat-1".into(), "Welcome everyone!".into())).await;
        assert!(!log.calls().iter().any(|c| matches!(c, Call::YtSendChat(_, t) if t.starts_with("ignored"))));
        h.yt.fail_next("send_chat");
        send(&e, Command::SendChat("Again".into())).await;
        until(&mut e, "a warning", |s| s.last_warning.as_deref().is_some_and(|w| w.contains("could not send"))).await;
    }

    // ---- scenes

    fn scene(id: u32, name: &str, screens: Option<u16>, cue: Option<(u32, &str)>) -> Scene {
        Scene {
            id,
            name: name.into(),
            key: String::new(),
            screens,
            cue: cue.map(|(list, c)| crate::scenes::LightCue { list, cue: c.into() }),
        }
    }

    fn with_scenes(c: Arc<RwLock<Config>>) -> Arc<RwLock<Config>> {
        c.write().unwrap().scenes = vec![
            scene(1, "Worship", Some(1), Some((1, "2"))),
            scene(2, "Preach", Some(3), Some((1, "4"))),
            scene(3, "Video", Some(5), Some((1, "9"))),
        ];
        c
    }

    #[tokio::test(start_paused = true)]
    async fn a_scene_cuts_the_hall_screens_and_fires_its_cue() {
        let (d, log, _h) = rig();
        let mut e = Engine::spawn(d, with_scenes(cfg(vec![])));
        assert_eq!(snap(&e).scenes.len(), 3, "the scenes come from the config");
        send(&e, Command::GoScene(2)).await;
        until_call(&log, Call::ScreensSetProgram(3)).await;
        until_call(&log, Call::LightsFireCue(1, "4".into())).await;
        let s = until(&mut e, "active", |s| s.active_scene == Some(2)).await;
        assert!(has(&s, "Scene · Preach"), "{:?}", texts(&s));
        send(&e, Command::GoScene(99)).await;
        send(&e, Command::SetScenes(vec![scene(1, "Worship", None, None)])).await;
        let s = until(&mut e, "the new list", |s| s.scenes.len() == 1).await;
        assert_eq!(s.active_scene, None, "the active scene was removed");
    }

    #[tokio::test(start_paused = true)]
    async fn go_live_takes_the_hall_to_the_go_live_scene_and_fires_its_cue() {
        let (d, log, _h) = rig();
        let c = with_scenes(cfg(vec![]));
        c.write().unwrap().screens_atem.t0_scene = Some(2);
        let mut e = Engine::spawn(d, c);
        send(&e, Command::GoLive).await;
        until(&mut e, "Live", |s| s.stage == Stage::Live).await;
        until_call(&log, Call::LightsFireCue(1, "4".into())).await;
        let s = until(&mut e, "active", |s| s.active_scene == Some(2)).await;
        assert!(log.calls().contains(&Call::ScreensSetPreview(3)), "{:?}", log.calls());
        assert!(!log.calls().contains(&Call::ScreensSetProgram(3)), "the hall is cut once, by T-0");
        assert!(has(&s, "Scene · Preach · at Go Live"), "{:?}", texts(&s));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hall_video_keeps_the_screens_but_its_scene_fires_and_goes_back_after() {
        let (d, log, h) = rig();
        let mut video = cue("Mission", Destination::HallAndOnline);
        video.scene = Some(3);
        let mut e = Engine::spawn(d, with_scenes(cfg(vec![video])));
        send(&e, Command::GoScene(1)).await;
        until(&mut e, "worship", |s| s.active_scene == Some(1)).await;
        until_call(&log, Call::LightsFireCue(1, "2".into())).await;
        send(&e, Command::PlayCue(0)).await;
        until_call(&log, Call::LightsFireCue(1, "9".into())).await;
        let s = until(&mut e, "the video's scene", |s| s.active_scene == Some(3)).await;
        assert!(has(&s, "Scene · Video · the hall stays on the video"), "{:?}", texts(&s));
        assert!(!log.calls().contains(&Call::ScreensSetProgram(5)), "the hall stays on the video");
        until_call(&log, Call::PlayerPlay).await;
        let before = log.calls().len();
        h.player.emit(PlayerEvent::Ended);
        until(&mut e, "played", |s| s.cues[0].1 == CueState::Played).await;
        let s = until(&mut e, "back to worship", |s| s.active_scene == Some(1)).await;
        assert_eq!(s.activity[0].text, "Scene · Worship", "{:?}", texts(&s));
        sleep(MS(200)).await;
        assert!(log.calls()[before..].contains(&Call::LightsFireCue(1, "2".into())), "worship's cue again");
    }

    #[tokio::test(start_paused = true)]
    async fn a_scene_picked_during_the_video_is_kept_after_it() {
        let (d, log, h) = rig();
        let mut video = cue("Notices", Destination::OnlineOnly);
        video.scene = Some(3);
        let mut e = Engine::spawn(d, with_scenes(cfg(vec![video])));
        send(&e, Command::GoScene(1)).await;
        until(&mut e, "worship", |s| s.active_scene == Some(1)).await;
        send(&e, Command::PlayCue(0)).await;
        // Online only: the video's scene may change the hall screens.
        until_call(&log, Call::ScreensSetProgram(5)).await;
        until_call(&log, Call::PlayerPlay).await;
        send(&e, Command::GoScene(2)).await;
        until(&mut e, "preach", |s| s.active_scene == Some(2)).await;
        until_call(&log, Call::ScreensSetProgram(3)).await;
        h.player.emit(PlayerEvent::Ended);
        until(&mut e, "played", |s| s.cues[0].1 == CueState::Played).await;
        sleep(MS(200)).await;
        assert_eq!(snap(&e).active_scene, Some(2), "the scene picked during the video stays");
    }

    // ---- stream health

    async fn health_read(e: &EngineHandle, h: Health) {
        send(e, Command::StreamHealthRead(Some(h))).await;
    }

    #[tokio::test(start_paused = true)]
    async fn not_receiving_while_live_alarms_after_20_s_and_clears() {
        let (d, _log, h) = rig();
        h.yt.set_life_cycle("live");
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "live", |s| s.stage == Stage::Live).await;
        health_read(&e, Health::NotReceiving).await;
        let s = until(&mut e, "the reading", |s| s.stream_health == Some(Health::NotReceiving)).await;
        assert!(!s.stream_alarm, "a short gap is not an alarm");
        sleep(Duration::from_secs(21)).await;
        health_read(&e, Health::NotReceiving).await;
        let s = until(&mut e, "the alarm", |s| s.stream_alarm).await;
        assert!(s.last_warning.unwrap().contains("not receiving the stream"));
        assert!(s.activity.iter().any(|a| a.text == "YouTube stopped receiving the stream"));
        health_read(&e, Health::Good).await;
        let s = until(&mut e, "the all-clear", |s| !s.stream_alarm).await;
        assert!(s.activity.iter().any(|a| a.text == "YouTube receiving the stream again"));
    }

    #[tokio::test(start_paused = true)]
    async fn poor_health_or_not_live_never_alarms() {
        let (d, _log, _h) = rig();
        let mut e = Engine::spawn(d, cfg(vec![]));
        // Idle: not receiving is expected.
        health_read(&e, Health::NotReceiving).await;
        sleep(Duration::from_secs(30)).await;
        health_read(&e, Health::NotReceiving).await;
        let s = until(&mut e, "the reading", |s| s.stream_health == Some(Health::NotReceiving)).await;
        assert!(!s.stream_alarm);
        send(&e, Command::StreamHealthRead(None)).await;
        until(&mut e, "not checked", |s| s.stream_health.is_none()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn receiving_but_poor_is_shown_but_not_an_alarm() {
        let (d, _log, h) = rig();
        h.yt.set_life_cycle("live");
        let mut e = Engine::spawn(d, cfg(vec![]));
        until(&mut e, "live", |s| s.stage == Stage::Live).await;
        health_read(&e, Health::Bad("bad".into())).await;
        sleep(Duration::from_secs(30)).await;
        health_read(&e, Health::Bad("bad".into())).await;
        let s = until(&mut e, "the reading", |s| s.stream_health == Some(Health::Bad("bad".into()))).await;
        assert!(!s.stream_alarm);
    }
}
