//! Logging decorators for the device traits. Every action call is logged at `info` with
//! its arguments and result (`warn` when it fails); read getters (program, route_of,
//! status, signed_in, upcoming, check, health, life_cycle, live_details, audio_level,
//! audio_input, chat_messages, position, stills, input_names) are
//! not logged, because the health poll calls them every 2 s. A downloaded still is logged by
//! its size, not its pixels.
//!
//! Every trait method must be forwarded here, defaults included: one left out silently
//! falls back to the trait's default (an empty list, or "cannot") on real hardware. The countdown counter's
//! `show`, called ten times a second, is logged at `debug` (a failure still at `warn`). Arguments are numbers,
//! file paths and broadcast ids only: no tokens or secrets pass through these traits.
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use streamit_core::config::AudioSourceId;
use streamit_core::devices::{
    AudioInput, BroadcastCheck, BroadcastSummary, Broadcasts, ChatPage, Counter, Encoder, EncoderStatus, Fix, Health,
    InputName, LabelKind, Lighting, LightingStatus, LiveDetails, Monitor, Phase, Player, PlayerEvent, Result, Router,
    Still, StreamSwitcher, Switcher,
};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

pub struct Logged<T: ?Sized> {
    device: &'static str,
    inner: Arc<T>,
}

impl<T: ?Sized> Logged<T> {
    pub fn wrap(device: &'static str, inner: Arc<T>) -> Arc<Logged<T>> {
        Arc::new(Logged { device, inner })
    }
}

/// `done` for a call made many times a second: success at `debug`, failure at `warn`.
fn done_quiet<R: Debug>(device: &str, call: String, r: Result<R>) -> Result<R> {
    match &r {
        Ok(v) => debug!(device, call = %call, result = ?v, "device call"),
        Err(e) => warn!(device, call = %call, error = %e, "device call failed"),
    }
    r
}

fn done<R: Debug>(device: &str, call: String, r: Result<R>) -> Result<R> {
    match &r {
        Ok(v) => info!(device, call = %call, result = ?v, "device call"),
        Err(e) => warn!(device, call = %call, error = %e, "device call failed"),
    }
    r
}

#[async_trait]
impl<T: Switcher + ?Sized> Switcher for Logged<T> {
    async fn program(&self) -> Result<u16> {
        self.inner.program().await
    }
    async fn preview(&self) -> Result<u16> {
        self.inner.preview().await
    }
    async fn set_preview(&self, input: u16) -> Result<()> {
        done(self.device, format!("set_preview({input})"), self.inner.set_preview(input).await)
    }
    async fn set_program(&self, input: u16) -> Result<()> {
        done(self.device, format!("set_program({input})"), self.inner.set_program(input).await)
    }
    async fn auto(&self) -> Result<()> {
        done(self.device, "auto()".into(), self.inner.auto().await)
    }
    async fn wait_transition_done(&self, timeout: Duration) -> Result<()> {
        done(self.device, format!("wait_transition_done({timeout:?})"), self.inner.wait_transition_done(timeout).await)
    }
    fn stills(&self) -> Vec<Option<String>> {
        self.inner.stills()
    }
    fn still_size(&self) -> Option<(u32, u32)> {
        self.inner.still_size()
    }
    async fn download_still(&self, slot: u16) -> Result<Still> {
        let r = self.inner.download_still(slot).await;
        let size = r.as_ref().map(|s| format!("{}x{}", s.width, s.height)).map_err(Clone::clone);
        done(self.device, format!("download_still({slot})"), size)?;
        r
    }
    async fn upload_still(&self, slot: u16, still: Still, name: &str) -> Result<()> {
        let call = format!("upload_still({slot}, {}x{}, {name:?})", still.width, still.height);
        done(self.device, call, self.inner.upload_still(slot, still, name).await)
    }
    async fn clear_still(&self, slot: u16) -> Result<()> {
        done(self.device, format!("clear_still({slot})"), self.inner.clear_still(slot).await)
    }
    fn input_names(&self) -> BTreeMap<u16, InputName> {
        self.inner.input_names()
    }
    async fn set_input_name(&self, input: u16, long: &str, short: &str) -> Result<()> {
        let call = format!("set_input_name({input}, {long:?}, {short:?})");
        done(self.device, call, self.inner.set_input_name(input, long, short).await)
    }
}

#[async_trait]
impl<T: StreamSwitcher + ?Sized> StreamSwitcher for Logged<T> {
    async fn set_media_player_still(&self, player: u8, still: u16) -> Result<()> {
        done(
            self.device,
            format!("set_media_player_still({player}, {still})"),
            self.inner.set_media_player_still(player, still).await,
        )
    }
    fn media_player_input(&self, player: u8) -> u16 {
        self.inner.media_player_input(player)
    }
    async fn audio_level(&self, src: AudioSourceId) -> Result<f32> {
        self.inner.audio_level(src).await
    }
    fn audio_sources(&self) -> Vec<AudioSourceId> {
        self.inner.audio_sources()
    }
    async fn audio_input(&self, src: AudioSourceId) -> Result<AudioInput> {
        self.inner.audio_input(src).await
    }
    async fn set_audio_level(&self, src: AudioSourceId, db: f32) -> Result<()> {
        done(
            self.device,
            format!("set_audio_level({}/{}, {db} dB)", src.input, src.source),
            self.inner.set_audio_level(src, db).await,
        )
    }
    async fn set_audio_on(&self, src: AudioSourceId, on: bool) -> Result<()> {
        done(
            self.device,
            format!("set_audio_on({}/{}, {on})", src.input, src.source),
            self.inner.set_audio_on(src, on).await,
        )
    }
}

#[async_trait]
impl<T: Router + ?Sized> Router for Logged<T> {
    async fn route_of(&self, output: u16) -> Result<u16> {
        self.inner.route_of(output).await
    }
    async fn route(&self, output: u16, input: u16) -> Result<()> {
        done(self.device, format!("route({output}, {input})"), self.inner.route(output, input).await)
    }
    async fn input_labels(&self) -> Result<Vec<String>> {
        self.inner.input_labels().await
    }
    async fn output_labels(&self) -> Result<Vec<String>> {
        self.inner.output_labels().await
    }
    async fn routing_table(&self) -> Result<Vec<u16>> {
        self.inner.routing_table().await
    }
    async fn set_labels(&self, kind: LabelKind, labels: &[(u16, String)]) -> Result<()> {
        done(
            self.device,
            format!("set_labels({kind:?}, {} labels)", labels.len()),
            self.inner.set_labels(kind, labels).await,
        )
    }
    async fn route_many(&self, routes: &[(u16, u16)]) -> Result<()> {
        done(self.device, format!("route_many({routes:?})"), self.inner.route_many(routes).await)
    }
    async fn set_label(&self, kind: LabelKind, n: u16, text: &str) -> Result<()> {
        done(self.device, format!("set_label({kind:?}, {n}, {text:?})"), self.inner.set_label(kind, n, text).await)
    }
}

#[async_trait]
impl<T: Encoder + ?Sized> Encoder for Logged<T> {
    async fn start(&self) -> Result<()> {
        done(self.device, "start()".into(), self.inner.start().await)
    }
    async fn stop(&self) -> Result<()> {
        done(self.device, "stop()".into(), self.inner.stop().await)
    }
    async fn stream_settings(&self) -> Result<Vec<(String, String)>> {
        self.inner.stream_settings().await
    }
    async fn set_stream_settings(&self, settings: &[(String, String)]) -> Result<()> {
        let names: Vec<&str> = settings.iter().map(|(k, _)| k.as_str()).collect();
        done(self.device, format!("set_stream_settings({names:?})"), self.inner.set_stream_settings(settings).await)
    }
    async fn status(&self) -> Result<EncoderStatus> {
        self.inner.status().await
    }
}

#[async_trait]
impl<T: Broadcasts + ?Sized> Broadcasts for Logged<T> {
    async fn signed_in(&self) -> Result<bool> {
        self.inner.signed_in().await
    }
    async fn upcoming(&self) -> Result<Vec<BroadcastSummary>> {
        self.inner.upcoming().await
    }
    async fn check(&self, id: &str) -> Result<BroadcastCheck> {
        self.inner.check(id).await
    }
    async fn fix(&self, id: &str, fix: Fix) -> Result<()> {
        done(self.device, format!("fix({id}, {fix:?})"), self.inner.fix(id, fix).await)
    }
    async fn health(&self) -> Result<Health> {
        self.inner.health().await
    }
    async fn life_cycle(&self, id: &str) -> Result<String> {
        self.inner.life_cycle(id).await
    }
    async fn live_details(&self, id: &str) -> Result<LiveDetails> {
        self.inner.live_details(id).await
    }
    async fn chat_messages(&self, chat_id: &str, page: Option<&str>) -> Result<ChatPage> {
        self.inner.chat_messages(chat_id, page).await
    }
    async fn send_chat(&self, chat_id: &str, text: &str) -> Result<()> {
        // The message itself is the viewers' to read, not the log's: only its length.
        done(
            self.device,
            format!("send_chat({chat_id}, {} chars)", text.chars().count()),
            self.inner.send_chat(chat_id, text).await,
        )
    }
    async fn transition(&self, id: &str, to: Phase) -> Result<()> {
        done(self.device, format!("transition({id}, {to:?})"), self.inner.transition(id, to).await)
    }
}

#[async_trait]
impl<T: Player + ?Sized> Player for Logged<T> {
    async fn load(&self, path: &Path, gain_db: f32) -> Result<Duration> {
        done(self.device, format!("load({}, {gain_db} dB)", path.display()), self.inner.load(path, gain_db).await)
    }
    async fn play(&self) -> Result<()> {
        done(self.device, "play()".into(), self.inner.play().await)
    }
    async fn stop(&self) -> Result<()> {
        done(self.device, "stop()".into(), self.inner.stop().await)
    }
    fn position(&self) -> Option<Duration> {
        self.inner.position()
    }
    fn events(&self) -> broadcast::Receiver<PlayerEvent> {
        self.inner.events()
    }
}

#[async_trait]
impl<T: Counter + ?Sized> Counter for Logged<T> {
    async fn show(&self, remaining: Duration) -> Result<()> {
        done_quiet(self.device, format!("show({}s)", remaining.as_secs()), self.inner.show(remaining).await)
    }
    async fn clear(&self) -> Result<()> {
        done(self.device, "clear()".into(), self.inner.clear().await)
    }
}

#[async_trait]
impl<T: Monitor + ?Sized> Monitor for Logged<T> {
    async fn set_solo(&self, input: Option<u16>) -> Result<()> {
        done(self.device, format!("set_solo({input:?})"), self.inner.set_solo(input).await)
    }
}

#[async_trait]
impl<T: Lighting + ?Sized> Lighting for Logged<T> {
    async fn fire_cue(&self, list: u32, cue: &str) -> Result<()> {
        done(self.device, format!("fire_cue({list}/{cue})"), self.inner.fire_cue(list, cue).await)
    }
    async fn status(&self) -> Result<LightingStatus> {
        self.inner.status().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use streamit_core::fakes::{Call, fake_devices};
    use tracing_subscriber::fmt::MakeWriter;

    /// The wrapper must pass on the media pool and the input names, not fall back to the
    /// trait's defaults (an empty pool: what a real ATEM showed).
    #[tokio::test]
    async fn a_wrapped_switcher_still_lists_its_stills_and_input_names() {
        let sim = streamit_sim::SimHandle::new(&streamit_core::config::Config::default(), streamit_sim::Speed::X10);
        let sw = Logged::wrap("Screens ATEM", Arc::new(sim.screens_switcher()));
        assert!(sw.stills().iter().any(Option::is_some));
        assert_eq!(sw.still_size(), Some((1920, 1080)));
        assert_eq!(sw.download_still(0).await.map(|s| s.width).ok(), Some(1920));
        assert!(!sw.input_names().is_empty());
        sw.set_input_name(2, "Pulpit Cam", "PULP").await.unwrap();
        assert_eq!(sw.input_names()[&2].long, "Pulpit Cam");
        sw.clear_still(0).await.unwrap();
        assert_eq!(sw.stills()[0], None);
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    #[tokio::test]
    async fn logs_actions_with_args_and_forwards_results() {
        let cap = Capture::default();
        let sub = tracing_subscriber::fmt().with_writer(cap.clone()).with_ansi(false).finish();
        let _g = tracing::subscriber::set_default(sub);
        let (d, log, h) = fake_devices();
        let hub = Logged::wrap("VideoHub", d.hub.clone());

        assert_eq!(hub.route(3, 5).await, Ok(()));
        assert!(log.calls().contains(&Call::HubRoute { output: 3, input: 5 }));
        assert_eq!(hub.route_of(3).await, Ok(5)); // forwarded, not logged
        h.hub.fail_next("route");
        assert!(hub.route(4, 6).await.is_err());

        let yt = Logged::wrap("YouTube", d.yt.clone());
        yt.transition("b1", Phase::Live).await.unwrap();

        let text = cap.text();
        let line =
            |needle: &str| text.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} in {text}"));
        assert!(line("route(3, 5)").contains("INFO"), "{text}");
        assert!(line("route(3, 5)").contains("VideoHub"), "{text}");
        assert!(line("route(4, 6)").contains("WARN"), "{text}");
        assert!(line("transition(b1, Live)").contains("INFO"), "{text}");
        assert!(!text.contains("route_of"), "reads are not logged: {text}");
    }

    #[tokio::test]
    async fn counter_ticks_log_at_debug_and_failures_at_warn() {
        let cap = Capture::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(cap.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let _g = tracing::subscriber::set_default(sub);
        let (d, _log, h) = fake_devices();
        let counter = Logged::wrap("NDI", d.counter.clone());
        counter.show(Duration::from_millis(41_900)).await.unwrap();
        h.counter.fail_next("show");
        assert!(counter.show(Duration::from_secs(40)).await.is_err());
        counter.clear().await.unwrap();

        let text = cap.text();
        let line =
            |needle: &str| text.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} in {text}"));
        assert!(line("show(41s)").contains("DEBUG"), "{text}");
        assert!(line("show(40s)").contains("WARN"), "{text}");
        assert!(line("clear()").contains("INFO"), "{text}");
    }
}
