//! Recording fakes for every device trait. Actions are logged to a shared `CallLog`;
//! read-only getters are not logged.
use crate::config::AudioSourceId;
use crate::devices::*;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    StreamSetPreview(u16),
    StreamSetProgram(u16),
    StreamAuto,
    StreamSetMediaStill(u8, u16),
    StreamSetAudioLevel(AudioSourceId, f32),
    StreamSetAudioOn(AudioSourceId, bool),
    ScreensSetPreview(u16),
    ScreensSetProgram(u16),
    ScreensAuto,
    HubRoute { output: u16, input: u16 },
    EncoderStart,
    EncoderStop,
    YtFix(Fix),
    YtTransition(Phase),
    YtSendChat(String, String),
    PlayerLoad(PathBuf, f32),
    PlayerPlay,
    PlayerStop,
    CounterShow(Duration),
    CounterClear,
    LightsFireCue(u32, String),
}

#[derive(Clone, Default)]
pub struct CallLog(Arc<Mutex<Vec<Call>>>);

impl CallLog {
    pub fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
    fn push(&self, c: Call) {
        self.0.lock().unwrap().push(c);
    }
}

/// Scriptable counted failures keyed by trait method name.
#[derive(Clone, Default)]
struct Failures(Arc<Mutex<HashMap<String, u32>>>);

impl Failures {
    fn add(&self, m: &str, n: u32) {
        *self.0.lock().unwrap().entry(m.to_string()).or_insert(0) += n;
    }
    fn check(&self, m: &str) -> Result<()> {
        let mut f = self.0.lock().unwrap();
        match f.get_mut(m) {
            Some(n) if *n > 0 => {
                *n -= 1;
                if *n == 0 {
                    f.remove(m);
                }
                Err(DeviceError::Disconnected(format!("scripted failure in {m}")))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Stream,
    Screens,
}

struct SwitcherState {
    program: u16,
    preview: u16,
    levels: HashMap<AudioSourceId, f32>,
    inputs: HashMap<AudioSourceId, AudioInput>,
    /// How long an `auto` takes to land (zero: at once).
    auto_delay: Duration,
    /// A running mix: when it lands, and the (program, preview) it lands on.
    pending: Option<(tokio::time::Instant, u16, u16)>,
}

impl SwitcherState {
    /// Land a mix whose time has come. Like a real ATEM, the old program is reported until then.
    fn settle(&mut self) {
        if let Some((at, program, preview)) = self.pending
            && tokio::time::Instant::now() >= at
        {
            self.program = program;
            self.preview = preview;
            self.pending = None;
        }
    }
}

#[derive(Clone)]
pub struct FakeSwitcher {
    kind: Kind,
    log: CallLog,
    fails: Failures,
    st: Arc<Mutex<SwitcherState>>,
}

impl FakeSwitcher {
    fn new(kind: Kind, log: CallLog) -> Self {
        Self {
            kind,
            log,
            fails: Failures::default(),
            st: Arc::new(Mutex::new(SwitcherState {
                program: 1,
                preview: 2,
                levels: HashMap::new(),
                inputs: HashMap::new(),
                auto_delay: Duration::ZERO,
                pending: None,
            })),
        }
    }
    /// The next call to `method` fails once.
    pub fn fail_next(&self, method: &str) {
        self.fails.add(method, 1);
    }
    /// The next `n` calls to `method` fail.
    pub fn fail_times(&self, method: &str, n: u32) {
        self.fails.add(method, n);
    }
    /// Script the program input without logging a call.
    pub fn set_program_state(&self, input: u16) {
        self.st.lock().unwrap().program = input;
    }
    /// Script the preview input without logging a call.
    pub fn set_preview_state(&self, input: u16) {
        self.st.lock().unwrap().preview = input;
    }
    /// Every later `auto` takes `d` (on the tokio clock) to land: `program` reports the old
    /// input until then, and `wait_transition_done` waits for it.
    pub fn set_auto_delay(&self, d: Duration) {
        self.st.lock().unwrap().auto_delay = d;
    }
    /// Script an audio level without logging a call.
    pub fn set_audio_level_state(&self, src: AudioSourceId, db: f32) {
        self.st.lock().unwrap().levels.insert(src, db);
    }
    /// Script what `audio_input` reports for `src`.
    pub fn set_audio_input_state(&self, src: AudioSourceId, input: AudioInput) {
        self.st.lock().unwrap().inputs.insert(src, input);
    }
    fn rec(&self, stream: Call, screens: Call) {
        self.log.push(if self.kind == Kind::Stream { stream } else { screens });
    }
}

#[async_trait]
impl Switcher for FakeSwitcher {
    async fn program(&self) -> Result<u16> {
        self.fails.check("program")?;
        let mut s = self.st.lock().unwrap();
        s.settle();
        Ok(s.program)
    }
    async fn preview(&self) -> Result<u16> {
        self.fails.check("preview")?;
        let mut s = self.st.lock().unwrap();
        s.settle();
        Ok(s.preview)
    }
    async fn set_preview(&self, input: u16) -> Result<()> {
        self.rec(Call::StreamSetPreview(input), Call::ScreensSetPreview(input));
        self.fails.check("set_preview")?;
        self.st.lock().unwrap().preview = input;
        Ok(())
    }
    async fn set_program(&self, input: u16) -> Result<()> {
        self.rec(Call::StreamSetProgram(input), Call::ScreensSetProgram(input));
        self.fails.check("set_program")?;
        let mut s = self.st.lock().unwrap();
        s.pending = None;
        s.program = input;
        Ok(())
    }
    async fn auto(&self) -> Result<()> {
        self.rec(Call::StreamAuto, Call::ScreensAuto);
        self.fails.check("auto")?;
        let mut s = self.st.lock().unwrap();
        // A mix already running lands first.
        if let Some((_, program, preview)) = s.pending.take() {
            s.program = program;
            s.preview = preview;
        }
        if s.auto_delay.is_zero() {
            let s = &mut *s;
            std::mem::swap(&mut s.program, &mut s.preview);
        } else {
            s.pending = Some((tokio::time::Instant::now() + s.auto_delay, s.preview, s.program));
        }
        Ok(())
    }
    async fn wait_transition_done(&self, timeout: Duration) -> Result<()> {
        self.fails.check("wait_transition_done")?;
        let at = self.st.lock().unwrap().pending.map(|(at, ..)| at);
        let Some(at) = at else { return Ok(()) };
        let limit = tokio::time::Instant::now() + timeout;
        if at > limit {
            tokio::time::sleep_until(limit).await;
            return Err(DeviceError::Timeout("transition still running".into()));
        }
        tokio::time::sleep_until(at).await;
        self.st.lock().unwrap().settle();
        Ok(())
    }
}

#[async_trait]
impl StreamSwitcher for FakeSwitcher {
    async fn set_media_player_still(&self, player: u8, still: u16) -> Result<()> {
        self.log.push(Call::StreamSetMediaStill(player, still));
        self.fails.check("set_media_player_still")
    }
    fn media_player_input(&self, player: u8) -> u16 {
        3000 + 10 * player as u16
    }
    async fn audio_level(&self, src: AudioSourceId) -> Result<f32> {
        self.fails.check("audio_level")?;
        Ok(*self.st.lock().unwrap().levels.get(&src).unwrap_or(&0.0))
    }
    async fn set_audio_level(&self, src: AudioSourceId, db: f32) -> Result<()> {
        self.log.push(Call::StreamSetAudioLevel(src, db));
        self.fails.check("set_audio_level")?;
        self.st.lock().unwrap().levels.insert(src, db);
        Ok(())
    }
    async fn set_audio_on(&self, src: AudioSourceId, on: bool) -> Result<()> {
        self.log.push(Call::StreamSetAudioOn(src, on));
        self.fails.check("set_audio_on")
    }
    async fn audio_input(&self, src: AudioSourceId) -> Result<AudioInput> {
        self.fails.check("audio_input")?;
        self.st
            .lock()
            .unwrap()
            .inputs
            .get(&src)
            .copied()
            .ok_or_else(|| DeviceError::Refused(format!("no audio input {}/{}", src.input, src.source)))
    }
}

#[derive(Clone)]
pub struct FakeRouter {
    log: CallLog,
    fails: Failures,
    routes: Arc<Mutex<HashMap<u16, u16>>>,
}

impl FakeRouter {
    /// The next call to `method` fails once.
    pub fn fail_next(&self, method: &str) {
        self.fails.add(method, 1);
    }
    /// The next `n` calls to `method` fail.
    pub fn fail_times(&self, method: &str, n: u32) {
        self.fails.add(method, n);
    }
    pub fn set_route(&self, output: u16, input: u16) {
        self.routes.lock().unwrap().insert(output, input);
    }
}

#[async_trait]
impl Router for FakeRouter {
    async fn route_of(&self, output: u16) -> Result<u16> {
        self.fails.check("route_of")?;
        Ok(*self.routes.lock().unwrap().get(&output).unwrap_or(&0))
    }
    async fn route(&self, output: u16, input: u16) -> Result<()> {
        self.log.push(Call::HubRoute { output, input });
        self.fails.check("route")?;
        self.routes.lock().unwrap().insert(output, input);
        Ok(())
    }
}

#[derive(Clone)]
pub struct FakeEncoder {
    log: CallLog,
    fails: Failures,
    status: Arc<Mutex<EncoderStatus>>,
}

impl FakeEncoder {
    /// The next call to `method` fails once.
    pub fn fail_next(&self, method: &str) {
        self.fails.add(method, 1);
    }
    /// The next `n` calls to `method` fail.
    pub fn fail_times(&self, method: &str, n: u32) {
        self.fails.add(method, n);
    }
    pub fn set_status(&self, s: EncoderStatus) {
        *self.status.lock().unwrap() = s;
    }
}

#[async_trait]
impl Encoder for FakeEncoder {
    async fn start(&self) -> Result<()> {
        self.log.push(Call::EncoderStart);
        self.fails.check("start")?;
        *self.status.lock().unwrap() = EncoderStatus::Streaming;
        Ok(())
    }
    async fn stop(&self) -> Result<()> {
        self.log.push(Call::EncoderStop);
        self.fails.check("stop")?;
        *self.status.lock().unwrap() = EncoderStatus::Idle;
        Ok(())
    }
    async fn status(&self) -> Result<EncoderStatus> {
        self.fails.check("status")?;
        Ok(self.status.lock().unwrap().clone())
    }
}

struct YtState {
    signed_in: bool,
    upcoming: Vec<BroadcastSummary>,
    check: BroadcastCheck,
    health: Health,
    life_cycle: String,
    live: LiveDetails,
    chat: Vec<ChatMessage>,
}

#[derive(Clone)]
pub struct FakeBroadcasts {
    log: CallLog,
    fails: Failures,
    st: Arc<Mutex<YtState>>,
    hang_transition: Arc<std::sync::atomic::AtomicBool>,
    delays: Arc<Mutex<HashMap<String, Duration>>>,
}

impl FakeBroadcasts {
    /// Every later call to `method` (`upcoming`, `check` or `life_cycle`) first
    /// waits `d` (on the tokio clock). `Duration::ZERO` removes the delay.
    pub fn set_delay(&self, method: &str, d: Duration) {
        self.delays.lock().unwrap().insert(method.to_string(), d);
    }
    async fn delay(&self, method: &str) {
        let d = self.delays.lock().unwrap().get(method).copied().unwrap_or_default();
        if !d.is_zero() {
            tokio::time::sleep(d).await;
        }
    }
    /// The next call to `method` fails once.
    pub fn fail_next(&self, method: &str) {
        self.fails.add(method, 1);
    }
    /// The next `n` calls to `method` fail.
    pub fn fail_times(&self, method: &str, n: u32) {
        self.fails.add(method, n);
    }
    pub fn set_signed_in(&self, v: bool) {
        self.st.lock().unwrap().signed_in = v;
    }
    pub fn set_upcoming(&self, v: Vec<BroadcastSummary>) {
        self.st.lock().unwrap().upcoming = v;
    }
    pub fn set_check(&self, c: BroadcastCheck) {
        self.st.lock().unwrap().check = c;
    }
    pub fn set_health(&self, h: Health) {
        self.st.lock().unwrap().health = h;
    }
    /// While set, `transition` logs its call and then never completes.
    pub fn set_transition_hangs(&self, v: bool) {
        self.hang_transition.store(v, std::sync::atomic::Ordering::SeqCst);
    }
    /// Adds a chat message; `chat_messages` pages through them by index.
    pub fn push_chat(&self, m: ChatMessage) {
        self.st.lock().unwrap().chat.push(m);
    }
    pub fn set_live_details(&self, d: LiveDetails) {
        self.st.lock().unwrap().live = d;
    }
    pub fn set_life_cycle(&self, l: &str) {
        self.st.lock().unwrap().life_cycle = l.to_string();
    }
}

#[async_trait]
impl Broadcasts for FakeBroadcasts {
    async fn signed_in(&self) -> Result<bool> {
        self.fails.check("signed_in")?;
        Ok(self.st.lock().unwrap().signed_in)
    }
    async fn upcoming(&self) -> Result<Vec<BroadcastSummary>> {
        self.delay("upcoming").await;
        self.fails.check("upcoming")?;
        Ok(self.st.lock().unwrap().upcoming.clone())
    }
    async fn check(&self, _id: &str) -> Result<BroadcastCheck> {
        self.delay("check").await;
        self.fails.check("check")?;
        Ok(self.st.lock().unwrap().check)
    }
    async fn fix(&self, _id: &str, fix: Fix) -> Result<()> {
        self.log.push(Call::YtFix(fix));
        self.fails.check("fix")
    }
    async fn health(&self) -> Result<Health> {
        self.fails.check("health")?;
        Ok(self.st.lock().unwrap().health.clone())
    }
    async fn life_cycle(&self, _id: &str) -> Result<String> {
        self.delay("life_cycle").await;
        self.fails.check("life_cycle")?;
        Ok(self.st.lock().unwrap().life_cycle.clone())
    }
    async fn live_details(&self, _id: &str) -> Result<LiveDetails> {
        self.fails.check("live_details")?;
        Ok(self.st.lock().unwrap().live.clone())
    }
    async fn chat_messages(&self, _chat_id: &str, page: Option<&str>) -> Result<ChatPage> {
        self.fails.check("chat_messages")?;
        let chat = &self.st.lock().unwrap().chat;
        let from = page.and_then(|p| p.parse::<usize>().ok()).unwrap_or(0).min(chat.len());
        Ok(ChatPage {
            messages: chat[from..].to_vec(),
            next_page: Some(chat.len().to_string()),
            wait: Duration::from_secs(5),
        })
    }
    async fn send_chat(&self, chat_id: &str, text: &str) -> Result<()> {
        self.log.push(Call::YtSendChat(chat_id.into(), text.into()));
        self.fails.check("send_chat")
    }
    async fn transition(&self, _id: &str, to: Phase) -> Result<()> {
        self.log.push(Call::YtTransition(to));
        if self.hang_transition.load(std::sync::atomic::Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.fails.check("transition")?;
        self.st.lock().unwrap().life_cycle = match to {
            Phase::Testing => "testing",
            Phase::Live => "live",
            Phase::Complete => "complete",
        }
        .to_string();
        Ok(())
    }
}

#[derive(Clone)]
pub struct FakePlayer {
    log: CallLog,
    fails: Failures,
    duration: Arc<Mutex<Duration>>,
    position: Arc<Mutex<Option<Duration>>>,
    tx: tokio::sync::broadcast::Sender<PlayerEvent>,
}

impl FakePlayer {
    /// The next call to `method` fails once.
    pub fn fail_next(&self, method: &str) {
        self.fails.add(method, 1);
    }
    /// The next `n` calls to `method` fail.
    pub fn fail_times(&self, method: &str, n: u32) {
        self.fails.add(method, n);
    }
    pub fn set_duration(&self, d: Duration) {
        *self.duration.lock().unwrap() = d;
    }
    pub fn set_position(&self, p: Option<Duration>) {
        *self.position.lock().unwrap() = p;
    }
    pub fn emit(&self, e: PlayerEvent) {
        let _ = self.tx.send(e);
    }
}

#[async_trait]
impl Player for FakePlayer {
    async fn load(&self, path: &Path, gain_db: f32) -> Result<Duration> {
        self.log.push(Call::PlayerLoad(path.to_path_buf(), gain_db));
        self.fails.check("load")?;
        Ok(*self.duration.lock().unwrap())
    }
    async fn play(&self) -> Result<()> {
        self.log.push(Call::PlayerPlay);
        self.fails.check("play")
    }
    async fn stop(&self) -> Result<()> {
        self.log.push(Call::PlayerStop);
        self.fails.check("stop")
    }
    fn position(&self) -> Option<Duration> {
        *self.position.lock().unwrap()
    }
    fn events(&self) -> tokio::sync::broadcast::Receiver<PlayerEvent> {
        self.tx.subscribe()
    }
}

#[derive(Clone)]
pub struct FakeCounter {
    log: CallLog,
    fails: Failures,
}

impl FakeCounter {
    /// The next call to `method` fails once.
    pub fn fail_next(&self, method: &str) {
        self.fails.add(method, 1);
    }
    /// The next `n` calls to `method` fail.
    pub fn fail_times(&self, method: &str, n: u32) {
        self.fails.add(method, n);
    }
}

#[async_trait]
impl Counter for FakeCounter {
    async fn show(&self, remaining: Duration) -> Result<()> {
        self.log.push(Call::CounterShow(remaining));
        self.fails.check("show")
    }
    async fn clear(&self) -> Result<()> {
        self.log.push(Call::CounterClear);
        self.fails.check("clear")
    }
}

/// A lighting console that records the cues fired and reports the last one active.
#[derive(Clone)]
pub struct FakeLighting {
    log: CallLog,
    fails: Failures,
    status: Arc<Mutex<LightingStatus>>,
}

impl FakeLighting {
    /// The next call to `method` fails once.
    pub fn fail_next(&self, method: &str) {
        self.fails.add(method, 1);
    }
}

#[async_trait]
impl Lighting for FakeLighting {
    async fn fire_cue(&self, list: u32, cue: &str) -> Result<()> {
        self.log.push(Call::LightsFireCue(list, cue.to_string()));
        self.fails.check("fire_cue")?;
        self.status.lock().unwrap().active_cue = Some(format!("{list}/{cue}"));
        Ok(())
    }
    async fn status(&self) -> Result<LightingStatus> {
        self.fails.check("status")?;
        Ok(self.status.lock().unwrap().clone())
    }
}

/// Typed handles to each fake, for scripting state in tests.
#[derive(Clone)]
pub struct FakeHandles {
    pub stream: FakeSwitcher,
    pub screens: FakeSwitcher,
    pub hub: FakeRouter,
    pub encoder: FakeEncoder,
    pub yt: FakeBroadcasts,
    pub player: FakePlayer,
    pub counter: FakeCounter,
    pub lights: FakeLighting,
}

pub fn fake_devices() -> (Devices, CallLog, FakeHandles) {
    let log = CallLog::default();
    let stream = FakeSwitcher::new(Kind::Stream, log.clone());
    let screens = FakeSwitcher::new(Kind::Screens, log.clone());
    let hub = FakeRouter { log: log.clone(), fails: Failures::default(), routes: Default::default() };
    let encoder =
        FakeEncoder { log: log.clone(), fails: Failures::default(), status: Arc::new(Mutex::new(EncoderStatus::Idle)) };
    let yt = FakeBroadcasts {
        log: log.clone(),
        fails: Failures::default(),
        st: Arc::new(Mutex::new(YtState {
            signed_in: true,
            upcoming: vec![],
            check: BroadcastCheck { bound_to_ours: true, auto_start: false, auto_stop: true, monitor_stream: true },
            health: Health::Good,
            life_cycle: "ready".into(),
            live: LiveDetails::default(),
            chat: Vec::new(),
        })),
        hang_transition: Default::default(),
        delays: Default::default(),
    };
    let (tx, _) = tokio::sync::broadcast::channel(16);
    let player = FakePlayer {
        log: log.clone(),
        fails: Failures::default(),
        duration: Arc::new(Mutex::new(Duration::from_secs(60))),
        position: Arc::new(Mutex::new(None)),
        tx,
    };
    let counter = FakeCounter { log: log.clone(), fails: Failures::default() };
    let lights = FakeLighting { log: log.clone(), fails: Failures::default(), status: Arc::default() };
    let devices = Devices {
        stream: Arc::new(stream.clone()),
        screens: Arc::new(screens.clone()),
        hub: Arc::new(hub.clone()),
        encoder: Arc::new(encoder.clone()),
        yt: Arc::new(yt.clone()),
        player: Arc::new(player.clone()),
        counter: Arc::new(counter.clone()),
        lights: Arc::new(lights.clone()),
    };
    (devices, log, FakeHandles { stream, screens, hub, encoder, yt, player, counter, lights })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_switcher_auto_swaps_and_logs() {
        let (d, log, _) = fake_devices();
        d.stream.set_preview(2).await.unwrap();
        d.stream.auto().await.unwrap();
        assert_eq!(d.stream.program().await.unwrap(), 2);
        assert_eq!(log.calls(), vec![Call::StreamSetPreview(2), Call::StreamAuto]);
    }

    #[tokio::test]
    async fn fail_next_returns_error_once() {
        let (d, log, h) = fake_devices();
        h.hub.fail_next("route");
        let e = d.hub.route(5, 8).await.unwrap_err();
        assert!(matches!(e, DeviceError::Disconnected(_)));
        d.hub.route(5, 8).await.unwrap();
        assert_eq!(h.hub.route_of(5).await.unwrap(), 8);
        assert_eq!(log.calls().len(), 2);
    }

    #[tokio::test]
    async fn fail_times_fails_n_calls_then_succeeds() {
        let (d, log, h) = fake_devices();
        h.yt.fail_times("transition", 2);
        assert!(d.yt.transition("b", Phase::Live).await.is_err());
        assert!(d.yt.transition("b", Phase::Live).await.is_err());
        assert_eq!(d.yt.life_cycle("b").await.unwrap(), "ready");
        d.yt.transition("b", Phase::Live).await.unwrap();
        assert_eq!(log.calls().len(), 3);
    }

    #[tokio::test]
    async fn transition_sets_life_cycle() {
        let (d, _log, _h) = fake_devices();
        d.yt.transition("b", Phase::Testing).await.unwrap();
        assert_eq!(d.yt.life_cycle("b").await.unwrap(), "testing");
        d.yt.transition("b", Phase::Live).await.unwrap();
        assert_eq!(d.yt.life_cycle("b").await.unwrap(), "live");
        d.yt.transition("b", Phase::Complete).await.unwrap();
        assert_eq!(d.yt.life_cycle("b").await.unwrap(), "complete");
    }

    #[tokio::test]
    async fn encoder_start_stop_set_status() {
        let (d, _log, _h) = fake_devices();
        d.encoder.start().await.unwrap();
        assert_eq!(d.encoder.status().await.unwrap(), EncoderStatus::Streaming);
        d.encoder.stop().await.unwrap();
        assert_eq!(d.encoder.status().await.unwrap(), EncoderStatus::Idle);
    }
}
