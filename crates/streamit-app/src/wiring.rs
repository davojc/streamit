//! Builds the real devices from the config, with stand-ins for any that cannot be built,
//! and polls each device's health every 2 s (R19).
//!
//! Startup never changes a device: the network clients only connect and listen (they
//! reconnect by themselves), and the playout objects only initialise.
use crate::logged::Logged;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use streamit_core::config::{AudioSourceId, Config, OscFraming};
use streamit_core::devices::{
    self, BroadcastCheck, BroadcastSummary, Broadcasts, Counter, DeviceError, Devices, Encoder, EncoderStatus, Fix,
    Health, Lighting, LightingStatus, Phase, Player, PlayerEvent, Router, StreamSwitcher, Switcher,
};
use streamit_core::engine::{AudioReading, Command, DeviceHealth, DeviceKind, Snapshot};
use streamit_core::golive::Stage;
use streamit_core::routing::{RoutingRead, Target, TargetRead};
use streamit_videohub::multiview::{MultiView, labels_to_set};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tracing::{Instrument, info, warn};

/// How often every device is polled.
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// How often YouTube's stream health is checked during a service (1 quota unit each).
pub const STREAM_HEALTH_EVERY: Duration = Duration::from_secs(12);
/// The live chat is read at most this often (5 quota units each, so about 1,800 an hour).
pub const CHAT_EVERY: Duration = Duration::from_secs(10);
/// After a chat read fails, the next try waits this long.
pub const CHAT_RETRY: Duration = Duration::from_secs(30);

/// Reads the selected broadcast's live chat while it has one (the health poll finds the
/// chat with the viewer count) and sends the new messages to the engine.
pub fn spawn_chat_poll(
    yt: Arc<dyn Broadcasts>,
    tx: mpsc::Sender<Command>,
    snap: watch::Receiver<Snapshot>,
) -> JoinHandle<()> {
    tokio::spawn(
        async move {
            let mut chat: Option<String> = None;
            let mut page: Option<String> = None;
            loop {
                let now = snap.borrow().live.as_ref().and_then(|l| l.chat_id.clone());
                if now != chat {
                    chat = now;
                    page = None;
                    if tx.send(Command::ChatRead { reset: true, messages: Vec::new(), error: None }).await.is_err() {
                        return;
                    }
                }
                let Some(id) = chat.clone() else {
                    tokio::time::sleep(POLL_EVERY).await;
                    continue;
                };
                let (read, wait) = match yt.chat_messages(&id, page.as_deref()).await {
                    Ok(p) => {
                        page = p.next_page.or(page);
                        let read = Command::ChatRead { reset: false, messages: p.messages, error: None };
                        (read, p.wait.max(CHAT_EVERY))
                    }
                    Err(e) => {
                        warn!(error = %e, "could not read the live chat");
                        let read = Command::ChatRead { reset: false, messages: Vec::new(), error: Some(e.to_string()) };
                        (read, CHAT_RETRY)
                    }
                };
                if tx.send(read).await.is_err() {
                    return;
                }
                tokio::time::sleep(wait).await;
            }
        }
        .in_current_span(),
    )
}
/// A poll with no answer in this time is a warning.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long startup waits for the network devices' first state before the engine's
/// first assessment, so a restart mid-service sees what is on air.
pub const STARTUP_WAIT: Duration = Duration::from_secs(5);
/// The devices whose first good read after startup triggers a Refresh.
pub const NETWORK: [DeviceKind; 5] = [
    DeviceKind::StreamAtem,
    DeviceKind::ScreensAtem,
    DeviceKind::VideoHub,
    DeviceKind::WebPresenter,
    DeviceKind::YouTube,
];
/// How often a failed player/counter build is retried.
#[cfg_attr(not(feature = "gst"), allow(dead_code))]
const PLAYOUT_RETRY: Duration = Duration::from_secs(10);

pub const ATEM_PORT: u16 = 9910;
pub const VIDEOHUB_PORT: u16 = 9990;
pub const WEBPRESENTER_PORT: u16 = 9977;

#[cfg(not(feature = "gst"))]
const NO_GST: &str = "built without GStreamer (rebuild with --features gst)";

/// One poll result per health: Ok is Ok, Disconnected is Down, anything else a Warning.
pub fn health_of<T>(r: &devices::Result<T>) -> DeviceHealth {
    match r {
        Ok(_) => DeviceHealth::Ok,
        Err(e @ DeviceError::Disconnected(_)) => DeviceHealth::Down(e.to_string()),
        Err(e) => DeviceHealth::Warning(e.to_string()),
    }
}

/// Remembers the last health per device and reports only changes.
#[derive(Default)]
pub struct HealthTracker {
    last: BTreeMap<DeviceKind, DeviceHealth>,
}

impl HealthTracker {
    /// The entries of `now` that differ from what was last seen (all of them the first time).
    pub fn changes(&mut self, now: &[(DeviceKind, DeviceHealth)]) -> Vec<(DeviceKind, DeviceHealth)> {
        let mut changed = Vec::new();
        for (k, h) in now {
            if self.last.get(k) != Some(h) {
                self.last.insert(*k, h.clone());
                changed.push((*k, h.clone()));
            }
        }
        changed
    }
}

// ---- stand-ins ----

/// Stands in for a device that could not be built: every call is `Disconnected(reason)`,
/// so it shows as Down and the sequences report it like an unplugged device.
pub struct Absent {
    reason: String,
    events: broadcast::Sender<PlayerEvent>,
}

impl Absent {
    pub fn new(reason: impl Into<String>) -> Arc<Absent> {
        let (events, _) = broadcast::channel(4);
        Arc::new(Absent { reason: reason.into(), events })
    }

    fn err<T>(&self) -> devices::Result<T> {
        Err(DeviceError::Disconnected(self.reason.clone()))
    }
}

#[async_trait]
impl Switcher for Absent {
    async fn program(&self) -> devices::Result<u16> {
        self.err()
    }
    async fn preview(&self) -> devices::Result<u16> {
        self.err()
    }
    async fn set_preview(&self, _: u16) -> devices::Result<()> {
        self.err()
    }
    async fn set_program(&self, _: u16) -> devices::Result<()> {
        self.err()
    }
    async fn auto(&self) -> devices::Result<()> {
        self.err()
    }
    async fn wait_transition_done(&self, _: Duration) -> devices::Result<()> {
        self.err()
    }
}

#[async_trait]
impl StreamSwitcher for Absent {
    async fn set_media_player_still(&self, _: u8, _: u16) -> devices::Result<()> {
        self.err()
    }
    fn media_player_input(&self, player: u8) -> u16 {
        3000 + 10 * u16::from(player)
    }
    async fn audio_level(&self, _: AudioSourceId) -> devices::Result<f32> {
        self.err()
    }
    async fn set_audio_level(&self, _: AudioSourceId, _: f32) -> devices::Result<()> {
        self.err()
    }
    async fn set_audio_on(&self, _: AudioSourceId, _: bool) -> devices::Result<()> {
        self.err()
    }
}

#[async_trait]
impl Router for Absent {
    async fn route_of(&self, _: u16) -> devices::Result<u16> {
        self.err()
    }
    async fn route(&self, _: u16, _: u16) -> devices::Result<()> {
        self.err()
    }
}

#[async_trait]
impl Encoder for Absent {
    async fn start(&self) -> devices::Result<()> {
        self.err()
    }
    async fn stop(&self) -> devices::Result<()> {
        self.err()
    }
    async fn status(&self) -> devices::Result<EncoderStatus> {
        self.err()
    }
}

#[async_trait]
impl Broadcasts for Absent {
    async fn signed_in(&self) -> devices::Result<bool> {
        self.err()
    }
    async fn upcoming(&self) -> devices::Result<Vec<BroadcastSummary>> {
        self.err()
    }
    async fn check(&self, _: &str) -> devices::Result<BroadcastCheck> {
        self.err()
    }
    async fn fix(&self, _: &str, _: Fix) -> devices::Result<()> {
        self.err()
    }
    async fn health(&self) -> devices::Result<Health> {
        self.err()
    }
    async fn life_cycle(&self, _: &str) -> devices::Result<String> {
        self.err()
    }
    async fn transition(&self, _: &str, _: Phase) -> devices::Result<()> {
        self.err()
    }
}

#[async_trait]
impl Player for Absent {
    async fn load(&self, _: &Path, _: f32) -> devices::Result<Duration> {
        self.err()
    }
    async fn play(&self) -> devices::Result<()> {
        self.err()
    }
    async fn stop(&self) -> devices::Result<()> {
        self.err()
    }
    fn position(&self) -> Option<Duration> {
        None
    }
    fn events(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }
}

#[async_trait]
impl Lighting for Absent {
    async fn fire_cue(&self, _: u32, _: &str) -> devices::Result<()> {
        self.err()
    }
    async fn status(&self) -> devices::Result<LightingStatus> {
        self.err()
    }
}

#[async_trait]
impl Counter for Absent {
    async fn show(&self, _: Duration) -> devices::Result<()> {
        self.err()
    }
    async fn clear(&self) -> devices::Result<()> {
        self.err()
    }
}

// ---- player / counter built later ----

/// A player or counter that may not exist yet: calls go to it once it is installed and
/// fail as `Disconnected` (with the last build error) until then.
pub struct Late<T: ?Sized> {
    inner: RwLock<Option<Arc<T>>>,
    error: Mutex<String>,
    stand_in: Arc<Absent>,
}

impl<T: ?Sized> Late<T> {
    pub fn new(why: impl Into<String>) -> Arc<Late<T>> {
        let why = why.into();
        Arc::new(Late { inner: RwLock::new(None), stand_in: Absent::new(why.clone()), error: Mutex::new(why) })
    }

    pub fn get(&self) -> Option<Arc<T>> {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn install(&self, v: Arc<T>) {
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = Some(v);
    }

    pub fn fail(&self, why: impl Into<String>) {
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = why.into();
    }

    pub fn health(&self) -> DeviceHealth {
        match self.get() {
            Some(_) => DeviceHealth::Ok,
            None => DeviceHealth::Down(self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()),
        }
    }

    fn missing<R>(&self) -> devices::Result<R> {
        Err(DeviceError::Disconnected(self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()))
    }
}

#[async_trait]
impl Player for Late<dyn Player> {
    async fn load(&self, path: &Path, gain_db: f32) -> devices::Result<Duration> {
        match self.get() {
            Some(p) => p.load(path, gain_db).await,
            None => self.missing(),
        }
    }
    async fn play(&self) -> devices::Result<()> {
        match self.get() {
            Some(p) => p.play().await,
            None => self.missing(),
        }
    }
    async fn stop(&self) -> devices::Result<()> {
        match self.get() {
            Some(p) => p.stop().await,
            None => self.missing(),
        }
    }
    fn position(&self) -> Option<Duration> {
        self.get().and_then(|p| p.position())
    }
    fn events(&self) -> broadcast::Receiver<PlayerEvent> {
        match self.get() {
            Some(p) => p.events(),
            None => Player::events(&*self.stand_in),
        }
    }
}

#[async_trait]
impl Counter for Late<dyn Counter> {
    async fn show(&self, remaining: Duration) -> devices::Result<()> {
        match self.get() {
            Some(c) => c.show(remaining).await,
            None => self.missing(),
        }
    }
    async fn clear(&self) -> devices::Result<()> {
        match self.get() {
            Some(c) => c.clear().await,
            None => self.missing(),
        }
    }
}

/// Builds the device off the async threads (GStreamer init and the NDI sink block), and
/// keeps retrying every `PLAYOUT_RETRY` while it fails. Returns after the first attempt,
/// with the retry task if one is needed (the session stops it when it ends).
#[cfg_attr(not(feature = "gst"), allow(dead_code))]
async fn keep_building<T, F>(slot: Arc<Late<T>>, what: &'static str, build: F) -> Option<AbortHandle>
where
    T: ?Sized + Send + Sync + 'static,
    F: Fn() -> devices::Result<Arc<T>> + Send + Sync + 'static,
{
    let build = Arc::new(build);
    let attempt = {
        let (slot, build) = (slot.clone(), build.clone());
        move || {
            let (slot, build) = (slot.clone(), build.clone());
            async move {
                match tokio::task::spawn_blocking(move || build()).await {
                    Ok(Ok(v)) => {
                        info!("{what} ready");
                        slot.install(v);
                        true
                    }
                    Ok(Err(e)) => {
                        warn!("{what} could not start: {e}");
                        slot.fail(e.to_string());
                        false
                    }
                    Err(e) => {
                        warn!("{what} start panicked: {e}");
                        slot.fail(format!("{what} start failed"));
                        false
                    }
                }
            }
        }
    };
    if attempt().await {
        return None;
    }
    let retry = tokio::spawn(
        async move {
            loop {
                tokio::time::sleep(PLAYOUT_RETRY).await;
                if attempt().await {
                    return;
                }
            }
        }
        .in_current_span(),
    );
    Some(retry.abort_handle())
}

// ---- building ----

/// `address` as a socket address on `port`, or why it cannot be used.
pub fn socket_addr(address: &Option<String>, port: u16) -> Result<SocketAddr, String> {
    let a = address.as_deref().map(str::trim).unwrap_or("");
    if a.is_empty() {
        return Err("no address set (assign one in Settings)".into());
    }
    if let Ok(sa) = a.parse::<SocketAddr>() {
        return Ok(sa);
    }
    a.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, port)).map_err(|_| format!("bad address {a:?}"))
}

/// What the health poll looks at.
pub struct Probes {
    devices: Devices,
    player: Arc<Late<dyn Player>>,
    counter: Arc<Late<dyn Counter>>,
    /// A lighting console is set up, so its health is polled.
    lighting: bool,
    /// The NDI counter is on, so its health is polled.
    ndi: bool,
    /// A MultiView 4 is set up: its client (or why there is none).
    multiview: Option<Result<Arc<MultiView>, String>>,
}

/// The MultiView 4's Videohub Ethernet protocol port.
pub const MULTIVIEW_PORT: u16 = 9990;

/// The MultiView is up once its client is connected and has its labels.
fn multiview_health(mv: &Result<Arc<MultiView>, String>) -> DeviceHealth {
    match mv {
        Ok(mv) => match mv.ensure_usable() {
            Ok(()) => DeviceHealth::Ok,
            Err(e) => DeviceHealth::Down(e.to_string()),
        },
        Err(e) => DeviceHealth::Down(e.clone()),
    }
}

pub struct Wired {
    pub devices: Devices,
    /// The first poll, so the window and deck show health straight away.
    pub health: Vec<(DeviceKind, DeviceHealth)>,
    pub probes: Probes,
    /// Background tasks besides the clients' own (the playout build retries). The
    /// network clients stop their reconnect loops when their last handle is dropped.
    pub tasks: Vec<AbortHandle>,
}

/// Wraps every device so its action calls are logged; reads are not (see logged.rs).
pub fn logged(d: Devices) -> Devices {
    Devices {
        stream: Logged::wrap("Stream ATEM", d.stream),
        screens: Logged::wrap("Screens ATEM", d.screens),
        hub: Logged::wrap("VideoHub", d.hub),
        encoder: Logged::wrap("Web Presenter", d.encoder),
        yt: Logged::wrap("YouTube", d.yt),
        player: Logged::wrap("DeckLink", d.player),
        counter: Logged::wrap("NDI", d.counter),
        lights: Logged::wrap("Lighting", d.lights),
    }
}

/// Builds every device from `c`. Never fails: a device that cannot be built is a Down
/// stand-in, and the network clients keep reconnecting by themselves.
pub async fn build(c: &Config) -> Wired {
    let mut stand_ins: Vec<DeviceKind> = Vec::new();
    let mut stand_in = |kind: DeviceKind, what: &str, why: String| {
        stand_ins.push(kind);
        absent(what, why)
    };
    let stream: Arc<dyn StreamSwitcher> = match socket_addr(&c.stream_atem.address, ATEM_PORT) {
        Ok(a) => match streamit_atem::AtemClient::connect(a).await {
            Ok(x) => x,
            Err(e) => stand_in(DeviceKind::StreamAtem, "Stream ATEM", e.to_string()),
        },
        Err(e) => stand_in(DeviceKind::StreamAtem, "Stream ATEM", e),
    };
    let screens: Arc<dyn Switcher> = match socket_addr(&c.screens_atem.address, ATEM_PORT) {
        Ok(a) => match streamit_atem::AtemClient::connect(a).await {
            Ok(x) => x,
            Err(e) => stand_in(DeviceKind::ScreensAtem, "Screens ATEM", e.to_string()),
        },
        Err(e) => stand_in(DeviceKind::ScreensAtem, "Screens ATEM", e),
    };
    let hub: Arc<dyn Router> = match socket_addr(&c.videohub.address, VIDEOHUB_PORT) {
        Ok(a) => match streamit_videohub::VideoHub::connect(a).await {
            Ok(x) => x,
            Err(e) => stand_in(DeviceKind::VideoHub, "VideoHub", e.to_string()),
        },
        Err(e) => stand_in(DeviceKind::VideoHub, "VideoHub", e),
    };
    let encoder: Arc<dyn Encoder> = match socket_addr(&c.web_presenter.address, WEBPRESENTER_PORT) {
        Ok(a) => match streamit_webpresenter::WebPresenter::connect(a).await {
            Ok(x) => x,
            Err(e) => stand_in(DeviceKind::WebPresenter, "Web Presenter", e.to_string()),
        },
        Err(e) => stand_in(DeviceKind::WebPresenter, "Web Presenter", e),
    };
    let yt: Arc<dyn Broadcasts> = match youtube(c) {
        Ok(y) => y,
        Err(why) => stand_in(DeviceKind::YouTube, "YouTube", why),
    };
    // The lighting console is optional: without an address there is nothing to poll.
    let lights: Arc<dyn Lighting> = match (c.lighting.is_set(), socket_addr(&c.lighting.address, streamit_eos::PORT)) {
        (true, Ok(a)) => {
            let framing = match c.lighting.framing {
                OscFraming::Slip => streamit_eos::Framing::Slip,
                OscFraming::Length => streamit_eos::Framing::Length,
            };
            streamit_eos::EosClient::connect(a, framing)
        }
        (true, Err(e)) => absent("Lighting console", e),
        (false, _) if c.lighting.off => Absent::new("lighting control is turned off (Settings › Equipment)"),
        (false, _) => Absent::new("no lighting console set up (Settings › Equipment)"),
    };
    let (player, counter, tasks) = playout(c).await;
    let devices = logged(Devices {
        stream,
        screens,
        hub,
        encoder,
        yt,
        player: player.clone() as Arc<dyn Player>,
        counter: counter.clone() as Arc<dyn Counter>,
        lights,
    });
    let multiview = c
        .multiview_device
        .is_set()
        .then(|| socket_addr(&c.multiview_device.address, MULTIVIEW_PORT).map(MultiView::connect));
    let probes =
        Probes { devices: devices.clone(), player, counter, lighting: c.lighting.is_set(), ndi: !c.ndi.off, multiview };
    // Stand-ins never come up, so only wait for the devices that were really built.
    let real: Vec<DeviceKind> = NETWORK.iter().copied().filter(|k| !stand_ins.contains(k)).collect();
    probes.wait_ready(&real, c.videohub.input8_output, STARTUP_WAIT).await;
    let health = probes.poll(c.videohub.input8_output).await;
    for (k, h) in &health {
        log_health(*k, h);
    }
    Wired { devices, health, probes, tasks }
}

fn absent(what: &str, why: String) -> Arc<Absent> {
    warn!("{what}: {why}; using a stand-in");
    Absent::new(why)
}

fn youtube(c: &Config) -> Result<Arc<dyn Broadcasts>, String> {
    let y = &c.youtube;
    if y.client_id.trim().is_empty() || y.client_secret.trim().is_empty() {
        return Err("YouTube client id and secret not set (Settings)".into());
    }
    let auth = streamit_youtube::Auth::load(&y.client_id, &y.client_secret).map_err(|e| e.to_string())?;
    let base = url::Url::parse(streamit_youtube::API_BASE).expect("api base");
    Ok(Arc::new(streamit_youtube::YouTubeClient::new(base, auth, y.stream_id.clone())))
}

/// The counter when the NDI counter is turned off: the countdown's updates go nowhere.
struct NoCounter;

#[async_trait]
impl Counter for NoCounter {
    async fn show(&self, _remaining: Duration) -> devices::Result<()> {
        Ok(())
    }
    async fn clear(&self) -> devices::Result<()> {
        Ok(())
    }
}

/// The counter for a turned-off NDI counter: installed at once, and quiet.
fn counter_off() -> Arc<Late<dyn Counter>> {
    let counter: Arc<Late<dyn Counter>> = Late::new("the NDI counter is turned off (Settings › Playout & NDI counter)");
    counter.install(Arc::new(NoCounter));
    counter
}

#[cfg(feature = "gst")]
async fn playout(c: &Config) -> (Arc<Late<dyn Player>>, Arc<Late<dyn Counter>>, Vec<AbortHandle>) {
    // A PC without GStreamer: down for this run, not retried (installing it needs a restart).
    if let Err(e) = streamit_playout::available() {
        warn!("DeckLink player and NDI counter: {e}");
        // The whole reason (where StreamIt looked, or which build is installed) shows on both.
        let counter = if c.ndi.off { counter_off() } else { Late::new(e.clone()) };
        return (Late::new(e), counter, Vec::new());
    }
    let player: Arc<Late<dyn Player>> = Late::new("DeckLink player starting");
    let pc = c.playout.clone();
    let p = keep_building(player.clone(), "DeckLink player", move || {
        streamit_playout::GstPlayer::new(&pc, streamit_playout::SinkKind::DeckLink).map(|p| p as Arc<dyn Player>)
    });
    if c.ndi.off {
        info!("NDI counter: turned off");
        return (player, counter_off(), p.await.into_iter().collect());
    }
    let counter: Arc<Late<dyn Counter>> = Late::new("NDI counter starting");
    let nc = c.ndi.clone();
    let n = keep_building(counter.clone(), "NDI counter", move || {
        streamit_playout::NdiCounter::new(&nc).map(|n| n as Arc<dyn Counter>)
    });
    let (p, n) = tokio::join!(p, n);
    (player, counter, p.into_iter().chain(n).collect())
}

#[cfg(not(feature = "gst"))]
async fn playout(c: &Config) -> (Arc<Late<dyn Player>>, Arc<Late<dyn Counter>>, Vec<AbortHandle>) {
    warn!("DeckLink player and NDI counter: {NO_GST}");
    let counter = if c.ndi.off { counter_off() } else { Late::new(NO_GST) };
    (Late::new(NO_GST), counter, Vec::new())
}

// ---- health ----

async fn probe<T>(f: impl Future<Output = devices::Result<T>>) -> DeviceHealth {
    match tokio::time::timeout(PROBE_TIMEOUT, f).await {
        Ok(r) => health_of(&r),
        Err(_) => DeviceHealth::Warning(format!("no reply in {} s", PROBE_TIMEOUT.as_secs())),
    }
}

impl Probes {
    /// Names each MultiView view after the VideoHub input on it (its VideoHub label, else
    /// "Input n") when the MultiView's label differs. A label the MultiView refused is not
    /// sent again until a different one is wanted.
    pub async fn name_views(
        &self,
        hub_labels: &[String],
        dests: &[Option<u16>],
        tried: &mut std::collections::HashMap<u16, String>,
    ) {
        let Some(Ok(mv)) = &self.multiview else { return };
        let Ok(current) = mv.labels() else { return };
        let label = |n: u16| {
            let own = usize::from(n).checked_sub(1).and_then(|i| hub_labels.get(i)).map(|l| l.trim().to_string());
            own.filter(|l| !l.is_empty()).unwrap_or_else(|| format!("Input {n}"))
        };
        for (view, text) in labels_to_set(dests, &current, label) {
            if tried.get(&view) == Some(&text) {
                continue;
            }
            match mv.set_label(view, &text).await {
                Ok(()) => {
                    info!(view, label = %text, "MultiView view named after its input");
                    tried.remove(&view);
                }
                Err(e) => {
                    warn!(view, label = %text, "could not name the MultiView view: {e}");
                    tried.insert(view, text);
                }
            }
        }
    }

    /// Probes for devices that are all there from the start (the pretend ones): the
    /// player and counter are installed at once.
    pub fn ready(devices: Devices) -> Probes {
        let (player, counter) = (Late::new(""), Late::new(""));
        player.install(devices.player.clone());
        counter.install(devices.counter.clone());
        // The pretend console is always there. Simulation never touches the network, so
        // a real MultiView is not checked.
        Probes { devices, player, counter, lighting: true, ndi: true, multiview: None }
    }

    #[cfg(test)]
    fn new(devices: Devices, player: Arc<Late<dyn Player>>, counter: Arc<Late<dyn Counter>>) -> Probes {
        Probes { devices, player, counter, lighting: false, ndi: true, multiview: None }
    }

    /// One read for one device: program() for the ATEMs, route_of(input-8 output, or 1)
    /// for the VideoHub, status() for the Web Presenter, signed_in() for YouTube.
    async fn check(&self, kind: DeviceKind, hub_output: u16) -> DeviceHealth {
        let d = &self.devices;
        let out = if hub_output == 0 { 1 } else { hub_output };
        match kind {
            DeviceKind::StreamAtem => probe(d.stream.program()).await,
            DeviceKind::ScreensAtem => probe(d.screens.program()).await,
            DeviceKind::VideoHub => probe(d.hub.route_of(out)).await,
            DeviceKind::WebPresenter => probe(d.encoder.status()).await,
            DeviceKind::YouTube => match tokio::time::timeout(PROBE_TIMEOUT, d.yt.signed_in()).await {
                Ok(Ok(false)) => DeviceHealth::Warning("not signed in to YouTube".into()),
                Ok(r) => health_of(&r),
                Err(_) => DeviceHealth::Warning("no reply from YouTube".into()),
            },
            DeviceKind::DeckLink => self.player.health(),
            DeviceKind::Ndi => self.counter.health(),
            DeviceKind::StreamDeck => DeviceHealth::Ok, // reported by the deck itself
            DeviceKind::Lighting => probe(d.lights.status()).await,
            DeviceKind::MultiView => match &self.multiview {
                Some(mv) => multiview_health(mv),
                None => DeviceHealth::Down("no MultiView set up".into()),
            },
        }
    }

    /// Waits (at most `limit` in total, all at once) until each of `kinds` answers its
    /// read with anything but Down: the device has connected and sent its state.
    pub async fn wait_ready(&self, kinds: &[DeviceKind], hub_output: u16, limit: Duration) {
        let one = |k: DeviceKind| async move {
            if !kinds.contains(&k) {
                return;
            }
            while matches!(self.check(k, hub_output).await, DeviceHealth::Down(_)) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        let all = async {
            tokio::join!(
                one(DeviceKind::StreamAtem),
                one(DeviceKind::ScreensAtem),
                one(DeviceKind::VideoHub),
                one(DeviceKind::WebPresenter),
                one(DeviceKind::YouTube),
            )
        };
        if tokio::time::timeout(limit, all).await.is_err() {
            info!("startup: not every device answered within {} s; continuing", limit.as_secs());
        }
    }

    /// For each routing target: the router output feeding each destination (from `c`), the
    /// input on each (`None` when not fed or unreadable) and, for the ATEMs, their program
    /// and preview. Also the VideoHub's input labels.
    pub async fn read_routing(&self, c: &Config) -> (Vec<String>, Vec<String>, [TargetRead; 3], Vec<u16>) {
        let d = &self.devices;
        let mut reads: [TargetRead; 3] = Default::default();
        for t in Target::ALL {
            let outputs = c.routing_outputs(t);
            let mut dests = Vec::with_capacity(outputs.len());
            for &out in &outputs {
                dests.push(if out == 0 { None } else { d.hub.route_of(out).await.ok().filter(|&i| i != 0) });
            }
            let read = match t {
                Target::MultiView => TargetRead::default(),
                Target::StreamAtem => TargetRead {
                    program: d.stream.program().await.ok(),
                    preview: d.stream.preview().await.ok(),
                    stills: d.stream.stills(),
                    names: d.stream.input_names(),
                    audio_sources: d.stream.audio_sources(),
                    ..TargetRead::default()
                },
                Target::ScreensAtem => TargetRead {
                    program: d.screens.program().await.ok(),
                    preview: d.screens.preview().await.ok(),
                    stills: d.screens.stills(),
                    names: d.screens.input_names(),
                    ..TargetRead::default()
                },
            };
            reads[t.index()] = TargetRead { outputs, dests, ..read };
        }
        let inputs = d.hub.input_labels().await.unwrap_or_default();
        let routes = d.hub.routing_table().await.unwrap_or_default();
        (inputs, d.hub.output_labels().await.unwrap_or_default(), reads, routes)
    }

    /// One read per device, all at once.
    pub async fn poll(&self, hub_output: u16) -> Vec<(DeviceKind, DeviceHealth)> {
        let (stream, screens, hub, encoder, yt) = tokio::join!(
            self.check(DeviceKind::StreamAtem, hub_output),
            self.check(DeviceKind::ScreensAtem, hub_output),
            self.check(DeviceKind::VideoHub, hub_output),
            self.check(DeviceKind::WebPresenter, hub_output),
            self.check(DeviceKind::YouTube, hub_output),
        );
        let mut out = vec![
            (DeviceKind::StreamAtem, stream),
            (DeviceKind::ScreensAtem, screens),
            (DeviceKind::VideoHub, hub),
            (DeviceKind::WebPresenter, encoder),
            (DeviceKind::YouTube, yt),
            (DeviceKind::DeckLink, self.player.health()),
        ];
        if self.ndi {
            out.push((DeviceKind::Ndi, self.counter.health()));
        }
        if self.lighting {
            out.push((DeviceKind::Lighting, self.check(DeviceKind::Lighting, hub_output).await));
        }
        if self.multiview.is_some() {
            out.push((DeviceKind::MultiView, self.check(DeviceKind::MultiView, hub_output).await));
        }
        out
    }

    /// What the lighting console says now (show, active cue), or `None` when it is not set
    /// up or not answering.
    pub async fn read_lighting(&self) -> Option<LightingStatus> {
        if !self.lighting {
            return None;
        }
        self.devices.lights.status().await.ok()
    }

    /// The audio inputs Settings picks to show, as the Stream ATEM reports them.
    pub async fn read_audio(&self, c: &Config) -> Vec<AudioReading> {
        let mut out = Vec::with_capacity(c.stream_atem.shown_audio.len());
        for a in &c.stream_atem.shown_audio {
            let input = self.devices.stream.audio_input(a.source).await.ok();
            out.push(AudioReading { name: a.name.clone(), source: a.source, input });
        }
        out
    }
}

fn log_health(kind: DeviceKind, h: &DeviceHealth) {
    match h {
        DeviceHealth::Ok => info!(device = ?kind, "device ok"),
        DeviceHealth::Warning(w) => warn!(device = ?kind, warning = %w, "device warning"),
        DeviceHealth::Down(e) => warn!(device = ?kind, error = %e, "device down"),
    }
}

/// Polls every `POLL_EVERY` and sends `Command::DeviceHealth` for each change. `seed` is
/// the health already sent. The first time a network device turns Ok after startup it
/// also sends one `Command::Refresh` (per poll), so the engine reassesses with that
/// device's state (a restart mid-service may come up before every device has answered),
/// but only while `snap` shows stage Idle and no job (R20). Stops when the engine is gone.
pub fn spawn_health_poll(
    probes: Probes,
    config: Arc<RwLock<Config>>,
    tx: mpsc::Sender<Command>,
    snap: watch::Receiver<Snapshot>,
    seed: &[(DeviceKind, DeviceHealth)],
) -> JoinHandle<()> {
    let mut tracker = HealthTracker::default();
    tracker.changes(seed);
    let mut ever_ok: BTreeSet<DeviceKind> =
        seed.iter().filter(|(_, h)| *h == DeviceHealth::Ok).map(|(k, _)| *k).collect();
    // Runs in the caller's span, so a simulation's polls log to the simulation log.
    tokio::spawn(
        async move {
            let mut last_routing = None;
            // MultiView view labels sent and refused, so a refusal is not resent every poll.
            let mut views_tried = std::collections::HashMap::new();
            let mut last_lighting = None;
            let mut last_audio = None;
            let mut stream_checked: Option<tokio::time::Instant> = None;
            let mut stream_sent = false;
            loop {
                // YouTube's stream health, from Prepare until the service ends.
                let active = {
                    let s = snap.borrow();
                    matches!(s.stage, Stage::Prepared(_) | Stage::Countdown { .. } | Stage::Live | Stage::Ending { .. })
                };
                if active {
                    if stream_checked.is_none_or(|t| t.elapsed() >= STREAM_HEALTH_EVERY) {
                        stream_checked = Some(tokio::time::Instant::now());
                        let h = match probes.devices.yt.health().await {
                            Ok(h) => h,
                            Err(e) => Health::Bad(format!("could not check: {e}")),
                        };
                        if tx.send(Command::StreamHealthRead(Some(h))).await.is_err() {
                            return;
                        }
                        // The viewer count and the chat, on the same beat (1 quota unit).
                        let id = snap.borrow().broadcast.as_ref().map(|b| b.id.clone());
                        // A failed read keeps the last details, so the chat is not dropped.
                        let live = match id {
                            Some(id) => probes.devices.yt.live_details(&id).await.ok().map(Some),
                            None => Some(None),
                        };
                        if let Some(live) = live
                            && tx.send(Command::LiveDetailsRead(live)).await.is_err()
                        {
                            return;
                        }
                        stream_sent = true;
                    }
                } else if stream_sent {
                    stream_checked = None;
                    stream_sent = false;
                    if tx.send(Command::StreamHealthRead(None)).await.is_err()
                        || tx.send(Command::LiveDetailsRead(None)).await.is_err()
                    {
                        return;
                    }
                }
                let lighting = probes.read_lighting().await;
                if last_lighting.as_ref() != Some(&lighting) {
                    if tx.send(Command::LightingRead(lighting.clone())).await.is_err() {
                        return;
                    }
                    last_lighting = Some(lighting);
                }
                // What the VideoHub feeds and the ATEMs show, sent when it changes.
                let c = config.read().unwrap_or_else(|e| e.into_inner()).clone();
                let audio = probes.read_audio(&c).await;
                if last_audio.as_ref() != Some(&audio) {
                    if tx.send(Command::AudioRead(audio.clone())).await.is_err() {
                        return;
                    }
                    last_audio = Some(audio);
                }
                let routing = probes.read_routing(&c).await;
                probes.name_views(&routing.0, &routing.2[Target::MultiView.index()].dests, &mut views_tried).await;
                if last_routing.as_ref() != Some(&routing) {
                    let (labels, output_labels, targets, routes) = routing.clone();
                    let read = RoutingRead { labels, output_labels, targets, media: c.media_dest(), routes };
                    if tx.send(Command::RoutingRead(Box::new(read))).await.is_err() {
                        return;
                    }
                    last_routing = Some(routing);
                }
                tokio::time::sleep(POLL_EVERY).await;
                let out = config.read().unwrap_or_else(|e| e.into_inner()).videohub.input8_output;
                let mut came_up = Vec::new();
                for (kind, h) in tracker.changes(&probes.poll(out).await) {
                    log_health(kind, &h);
                    if h == DeviceHealth::Ok && NETWORK.contains(&kind) && ever_ok.insert(kind) {
                        came_up.push(kind);
                    }
                    if tx.send(Command::DeviceHealth(kind, h)).await.is_err() {
                        return;
                    }
                }
                if !came_up.is_empty() {
                    // R20: only while nothing has started; mid-service the engine's stage
                    // stands, and the device stays marked so it never triggers later.
                    let idle = {
                        let s = snap.borrow();
                        s.stage == Stage::Idle && s.job.is_none()
                    };
                    if idle {
                        info!(devices = ?came_up, "first contact after startup: reassessing");
                        if tx.send(Command::Refresh).await.is_err() {
                            return;
                        }
                    } else {
                        info!(devices = ?came_up, "first contact during a service: not reassessing");
                    }
                }
            }
        }
        .in_current_span(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamit_core::fakes::fake_devices;

    #[test]
    fn health_maps_ok_disconnected_and_others() {
        assert_eq!(health_of(&Ok::<_, DeviceError>(3u16)), DeviceHealth::Ok);
        assert_eq!(
            health_of::<()>(&Err(DeviceError::Disconnected("gone".into()))),
            DeviceHealth::Down("disconnected: gone".into())
        );
        for e in
            [DeviceError::Refused("no".into()), DeviceError::Timeout("slow".into()), DeviceError::Other("odd".into())]
        {
            let text = e.to_string();
            assert_eq!(health_of::<()>(&Err(e)), DeviceHealth::Warning(text));
        }
    }

    #[test]
    fn tracker_reports_only_changes() {
        let mut t = HealthTracker::default();
        let a = vec![(DeviceKind::VideoHub, DeviceHealth::Ok), (DeviceKind::YouTube, DeviceHealth::Ok)];
        assert_eq!(t.changes(&a), a);
        assert_eq!(t.changes(&a), vec![]);
        let b = vec![(DeviceKind::VideoHub, DeviceHealth::Down("x".into())), (DeviceKind::YouTube, DeviceHealth::Ok)];
        assert_eq!(t.changes(&b), vec![(DeviceKind::VideoHub, DeviceHealth::Down("x".into()))]);
        let c = vec![(DeviceKind::VideoHub, DeviceHealth::Down("y".into())), (DeviceKind::YouTube, DeviceHealth::Ok)];
        assert_eq!(t.changes(&c), vec![(DeviceKind::VideoHub, DeviceHealth::Down("y".into()))]);
    }

    fn kinds_down(h: &[(DeviceKind, DeviceHealth)], kinds: &[DeviceKind]) {
        for k in kinds {
            let got = h.iter().find(|(x, _)| x == k).map(|(_, h)| h.clone());
            assert!(matches!(got, Some(DeviceHealth::Down(_))), "{k:?}: {got:?} in {h:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_with_unreachable_devices_still_returns_all_kinds_down() {
        let mut c = Config::default();
        c.stream_atem.address = Some("127.0.0.1:9".into());
        c.screens_atem.address = Some("127.0.0.1:9".into());
        c.videohub.address = Some("127.0.0.1:9".into());
        c.web_presenter.address = Some("127.0.0.1:9".into());
        c.youtube.client_id = String::new();
        let w = build(&c).await;
        assert_eq!(w.health.len(), 7, "{:?}", w.health);
        kinds_down(
            &w.health,
            &[
                DeviceKind::StreamAtem,
                DeviceKind::ScreensAtem,
                DeviceKind::VideoHub,
                DeviceKind::WebPresenter,
                DeviceKind::YouTube,
            ],
        );
        #[cfg(not(feature = "gst"))]
        kinds_down(&w.health, &[DeviceKind::DeckLink, DeviceKind::Ndi]);
        // The stand-ins answer like unplugged devices.
        assert!(matches!(w.devices.yt.signed_in().await, Err(DeviceError::Disconnected(_))));
        assert_eq!(w.devices.stream.media_player_input(1), 3010);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_turned_off_ndi_counter_is_not_polled_and_takes_the_countdown_quietly() {
        let mut c = Config::default();
        c.ndi.off = true;
        let w = build(&c).await;
        assert!(!w.health.iter().any(|(k, _)| *k == DeviceKind::Ndi), "{:?}", w.health);
        assert!(w.devices.counter.show(Duration::from_secs(30)).await.is_ok());
        assert!(w.devices.counter.clear().await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_or_bad_addresses_are_down_stand_ins() {
        let mut c = Config::default();
        c.stream_atem.address = None;
        c.screens_atem.address = Some("not an ip".into());
        c.videohub.address = Some("  ".into());
        c.web_presenter.address = Some("127.0.0.1:9".into());
        let w = build(&c).await;
        kinds_down(&w.health, &[DeviceKind::StreamAtem, DeviceKind::ScreensAtem, DeviceKind::VideoHub]);
        assert!(matches!(
            w.health.iter().find(|(k, _)| *k == DeviceKind::StreamAtem),
            Some((_, DeviceHealth::Down(m))) if m.contains("no address")
        ));
    }

    #[tokio::test]
    async fn poll_reads_each_device() {
        let (d, _log, h) = fake_devices();
        let player: Arc<Late<dyn Player>> = Late::new("not yet");
        let counter: Arc<Late<dyn Counter>> = Late::new("not yet");
        player.install(d.player.clone());
        let p = Probes::new(d, player, counter);
        let got: BTreeMap<_, _> = p.poll(0).await.into_iter().collect();
        for k in [
            DeviceKind::StreamAtem,
            DeviceKind::ScreensAtem,
            DeviceKind::VideoHub,
            DeviceKind::WebPresenter,
            DeviceKind::YouTube,
            DeviceKind::DeckLink,
        ] {
            assert_eq!(got[&k], DeviceHealth::Ok, "{k:?}");
        }
        assert_eq!(got[&DeviceKind::Ndi], DeviceHealth::Down("not yet".into()));

        h.yt.set_signed_in(false);
        h.hub.fail_next("route_of");
        let got: BTreeMap<_, _> = p.poll(0).await.into_iter().collect();
        assert!(matches!(got[&DeviceKind::YouTube], DeviceHealth::Warning(_)));
        assert!(matches!(got[&DeviceKind::VideoHub], DeviceHealth::Down(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn chat_poll_reads_the_chat_it_is_given_and_starts_afresh_on_another() {
        let (d, _log, h) = fake_devices();
        let msg = |id: &str| devices::ChatMessage {
            id: id.into(),
            author: "Ruth".into(),
            role: devices::ChatRole::Viewer,
            text: "Morning".into(),
            published: String::new(),
        };
        h.yt.push_chat(msg("a"));
        let (snap_tx, snap_rx) = watch::channel(Snapshot::default());
        let (tx, mut rx) = mpsc::channel(8);
        let poll = spawn_chat_poll(d.yt.clone(), tx, snap_rx);
        let live = |id: &str| Some(devices::LiveDetails { viewers: None, chat_id: Some(id.into()) });
        snap_tx.send_modify(|s| s.live = live("chat-1"));
        let mut next = async || rx.recv().await.unwrap();
        assert!(matches!(next().await, Command::ChatRead { reset: true, .. }));
        match next().await {
            Command::ChatRead { reset: false, messages, error: None } => assert_eq!(messages, vec![msg("a")]),
            other => panic!("{other:?}"),
        }
        h.yt.push_chat(msg("b"));
        match next().await {
            Command::ChatRead { messages, .. } => assert_eq!(messages, vec![msg("b")], "only the new ones"),
            other => panic!("{other:?}"),
        }
        snap_tx.send_modify(|s| s.live = live("chat-2"));
        h.yt.fail_next("chat_messages");
        assert!(matches!(next().await, Command::ChatRead { reset: true, .. }));
        assert!(matches!(next().await, Command::ChatRead { error: Some(_), .. }));
        poll.abort();
    }

    #[tokio::test]
    async fn late_player_forwards_once_installed() {
        let (d, log, _h) = fake_devices();
        let late: Arc<Late<dyn Player>> = Late::new("no player");
        assert_eq!(late.play().await, Err(DeviceError::Disconnected("no player".into())));
        late.fail("gst init failed");
        assert_eq!(late.health(), DeviceHealth::Down("gst init failed".into()));
        late.install(d.player.clone());
        late.play().await.unwrap();
        assert!(log.calls().contains(&streamit_core::fakes::Call::PlayerPlay));
        assert_eq!(late.health(), DeviceHealth::Ok);
    }

    #[tokio::test(start_paused = true)]
    async fn device_ready_after_startup_triggers_one_refresh() {
        let (d, _log, h) = fake_devices();
        h.stream.fail_times("program", 2); // the startup poll and the first poll see it Down
        let late = || -> Arc<Late<dyn Player>> { Late::new("x") };
        let counter: Arc<Late<dyn Counter>> = Late::new("x");
        let p = Probes::new(d, late(), counter);
        let seed = p.poll(0).await;
        assert!(matches!(seed[0], (DeviceKind::StreamAtem, DeviceHealth::Down(_))));
        let (tx, mut rx) = mpsc::channel(64);
        let cfg = Arc::new(RwLock::new(Config::default()));
        let (_snap_tx, snap_rx) = watch::channel(Snapshot::default());
        let poll = spawn_health_poll(p, cfg, tx, snap_rx, &seed);
        tokio::time::sleep(Duration::from_secs(13)).await; // six more polls
        // A later drop and recovery is not the first time: no second Refresh.
        h.stream.fail_next("program");
        tokio::time::sleep(Duration::from_secs(9)).await;
        poll.abort();
        let mut cmds = Vec::new();
        while let Ok(c) = rx.try_recv() {
            cmds.push(c);
        }
        let refreshes = cmds.iter().filter(|c| **c == Command::Refresh).count();
        assert_eq!(refreshes, 1, "{cmds:?}");
        let up = cmds.iter().position(|c| *c == Command::DeviceHealth(DeviceKind::StreamAtem, DeviceHealth::Ok));
        let refresh = cmds.iter().position(|c| *c == Command::Refresh);
        assert!(up.is_some() && up < refresh, "health first, then Refresh: {cmds:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn startup_waits_for_devices_to_answer() {
        let (d, _log, h) = fake_devices();
        h.hub.fail_times("route_of", 10); // ~1 s of Disconnected
        let p = Probes::new(d, Late::new("x"), Late::new("x"));
        let start = tokio::time::Instant::now();
        p.wait_ready(&NETWORK, 0, STARTUP_WAIT).await;
        assert!(start.elapsed() >= Duration::from_millis(900), "{:?}", start.elapsed());
        assert!(start.elapsed() < STARTUP_WAIT);
        assert_eq!(p.check(DeviceKind::VideoHub, 0).await, DeviceHealth::Ok);
        // A device that never answers costs at most the limit.
        h.stream.fail_times("program", 1000);
        let start = tokio::time::Instant::now();
        p.wait_ready(&NETWORK, 0, STARTUP_WAIT).await;
        assert!(start.elapsed() <= STARTUP_WAIT + Duration::from_millis(200));
    }

    #[tokio::test(start_paused = true)]
    async fn first_contact_during_prepared_sends_no_refresh() {
        use streamit_core::golive::Readiness;
        let (d, _log, h) = fake_devices();
        h.stream.fail_times("program", 2);
        let p = Probes::new(d, Late::new("x"), Late::new("x"));
        let seed = p.poll(0).await;
        let (tx, mut rx) = mpsc::channel(64);
        let cfg = Arc::new(RwLock::new(Config::default()));
        let prepared = Snapshot { stage: Stage::Prepared(Readiness::Ready), ..Snapshot::default() };
        let (snap_tx, snap_rx) = watch::channel(prepared);
        let poll = spawn_health_poll(p, cfg, tx, snap_rx, &seed);
        tokio::time::sleep(Duration::from_secs(7)).await; // the Stream ATEM comes up
        // Back to Idle and a later drop/recover: still the device's first contact was
        // used up, so no Refresh now either.
        snap_tx.send_replace(Snapshot::default());
        h.stream.fail_next("program");
        tokio::time::sleep(Duration::from_secs(7)).await;
        poll.abort();
        let mut cmds = Vec::new();
        while let Ok(c) = rx.try_recv() {
            cmds.push(c);
        }
        assert!(cmds.contains(&Command::DeviceHealth(DeviceKind::StreamAtem, DeviceHealth::Ok)), "{cmds:?}");
        assert!(!cmds.contains(&Command::Refresh), "{cmds:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn first_contact_while_a_job_runs_sends_no_refresh() {
        let (d, _log, h) = fake_devices();
        h.hub.fail_times("route_of", 2);
        let p = Probes::new(d, Late::new("x"), Late::new("x"));
        let seed = p.poll(0).await;
        let (tx, mut rx) = mpsc::channel(64);
        let cfg = Arc::new(RwLock::new(Config::default()));
        let (_snap_tx, snap_rx) = watch::channel(Snapshot { job: Some("Prepare"), ..Snapshot::default() });
        let poll = spawn_health_poll(p, cfg, tx, snap_rx, &seed);
        tokio::time::sleep(Duration::from_secs(7)).await;
        poll.abort();
        let mut cmds = Vec::new();
        while let Ok(c) = rx.try_recv() {
            cmds.push(c);
        }
        assert!(cmds.contains(&Command::DeviceHealth(DeviceKind::VideoHub, DeviceHealth::Ok)), "{cmds:?}");
        assert!(!cmds.contains(&Command::Refresh), "{cmds:?}");
    }

    #[tokio::test]
    async fn routing_read_skips_unset_and_unrouted_destinations() {
        let (d, _log, h) = fake_devices();
        h.hub.set_route(37, 6);
        h.hub.set_route(39, 12);
        h.hub.set_route(11, 13);
        h.stream.set_program_state(3);
        let mut c = Config::default();
        c.multiview.outputs = vec![37, 0, 39, 40];
        c.videohub.input8_output = 11;
        let p = Probes::new(d, Late::new("x"), Late::new("x"));
        let (labels, output_labels, reads, _routes) = p.read_routing(&c).await;
        assert!(labels.is_empty() && output_labels.is_empty());
        let mv = &reads[Target::MultiView.index()];
        assert_eq!(mv.dests, vec![Some(6), None, Some(12), None], "output 40 reads 0: nothing routed");
        assert_eq!(mv.program, None);
        let stream = &reads[Target::StreamAtem.index()];
        assert_eq!(stream.outputs, vec![0, 0, 0, 0, 0, 0, 0, 11], "input 8 is the video output");
        assert_eq!(stream.dests[7], Some(13));
        assert_eq!(stream.program, Some(3));
    }
}
