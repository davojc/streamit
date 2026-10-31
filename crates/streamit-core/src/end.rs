//! End Service: the closing countdown (when on), closing still on air, stream audio faded,
//! hold, YouTube complete, encoder stop.
//!
//! Device errors never abort the sequence; they become warnings. The encoder is
//! stopped even when YouTube fails or hangs.
use crate::audio::{Target, fade, switch_off};
use crate::config::Config;
use crate::devices::{Devices, Phase, PlayerEvent};
use crate::golive::{
    GO_PUBLIC_WAIT, Stage, countdown_audio, cut, keep_countdown_off_stream, stream_to_countdown, transition_with_retry,
};
use crate::steps::{StepId, StepMark};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::sleep;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndOutcome {
    pub ended_broadcast: bool,
    pub stopped_encoder: bool,
    pub warnings: Vec<String>,
}

/// Closing still, mics to the floor, hold (skippable), broadcast complete, encoder stop.
/// `step` gets how far each step has got.
pub async fn end_service(
    d: &Devices,
    c: &Config,
    bid: &str,
    skip_hold: Arc<Notify>,
    progress: impl Fn(Stage),
    step: impl Fn(StepId, StepMark),
) -> EndOutcome {
    let sa = &c.stream_atem;
    let mut warnings = Vec::new();

    // Input 8's route before the closing countdown took it, while the stream shows it.
    let stream_route = if c.playout.closing_plays() {
        closing_countdown(d, c, &skip_hold, &progress, &step, &mut warnings).await
    } else {
        None
    };

    step(StepId::ClosingStill, StepMark::Now(None));
    let mut still = d.stream.set_media_player_still(sa.media_player, sa.closing_still).await;
    if let Err(e) = &still {
        warnings.push(format!("Could not set the closing still: {e}"));
    }
    let still_input = d.stream.media_player_input(sa.media_player);
    let r = cut(&*d.stream, still_input).await;
    if let Err(e) = &r {
        warnings.push(format!("Could not cut the stream to the closing still: {e}"));
    }
    if still.is_ok() {
        still = r;
    }
    // Off the closing countdown: input 8's sound off and its route back.
    if let Some(prev) = stream_route {
        if let Err(e) = switch_off(&*d.stream, countdown_audio(c), sa.fade).await {
            warnings.push(format!("Could not turn off input 8 audio: {e}"));
        }
        crate::rollin::restore_route(d, c, prev, &mut warnings).await;
    }
    step(StepId::ClosingStill, StepMark::of(&still));
    step(StepId::EndAudioDown, StepMark::Now(None));
    let r = fade(&*d.stream, &sa.mic_sources, Target::Floor, sa.fade).await;
    step(StepId::EndAudioDown, StepMark::of(&r));
    if let Err(e) = r {
        warnings.push(format!("Could not fade the stream audio down: {e}"));
    }

    // Hold: one report per second, counting down; a (stored) skip permit ends it early.
    let skip = skip_hold.notified();
    tokio::pin!(skip);
    let mut remaining = c.timings.closing_hold;
    let second = Duration::from_secs(1);
    step(StepId::ClosingHold, StepMark::Now(None));
    while !remaining.is_zero() {
        progress(Stage::Ending { remaining });
        tokio::select! {
            biased;
            _ = &mut skip => break,
            _ = sleep(remaining.min(second)) => {}
        }
        remaining = remaining.saturating_sub(second);
    }

    step(StepId::ClosingHold, StepMark::done());

    step(StepId::YouTubeEnded, StepMark::Now(None));
    let ended_broadcast =
        match tokio::time::timeout(GO_PUBLIC_WAIT, transition_with_retry(d, bid, Phase::Complete)).await {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                warnings.push(format!("Could not end the YouTube broadcast: {e}"));
                false
            }
            Err(_) => {
                warnings.push(format!(
                    "Ending the YouTube broadcast did not finish within {} s; check YouTube Studio",
                    GO_PUBLIC_WAIT.as_secs()
                ));
                false
            }
        };

    if ended_broadcast {
        step(StepId::YouTubeEnded, StepMark::done());
    } else if let Some(w) = warnings.last() {
        step(StepId::YouTubeEnded, StepMark::Failed(w.clone()));
    }

    step(StepId::EncoderStopped, StepMark::Now(None));
    let r = d.encoder.stop().await;
    step(StepId::EncoderStopped, StepMark::of(&r));
    let stopped_encoder = match r {
        Ok(()) => true,
        Err(e) => {
            warnings.push(format!("Could not stop the encoder: {e}"));
            false
        }
    };
    progress(Stage::Ended);
    EndOutcome { ended_broadcast, stopped_encoder, warnings }
}

/// The closing countdown: to the hall screens and/or online (the stream audio faded down
/// first either way, then input 8 routed to the DeckLink with its sound, or input 8's
/// sound kept off when it is not going online), until it ends or End
/// Service is pressed again. The hall goes back to what it showed. Returns input 8's
/// route before, when the stream was put on it: the closing still's cut takes the stream
/// off it, then the caller puts the route back.
async fn closing_countdown(
    d: &Devices,
    c: &Config,
    skip: &Notify,
    progress: &impl Fn(Stage),
    step: &impl Fn(StepId, StepMark),
    warnings: &mut Vec<String>,
) -> Option<u16> {
    let p = &c.playout;
    let mut events = d.player.events();
    step(StepId::ClosingVideo, StepMark::Now(Some("Loading".into())));
    let duration = match d.player.load(&p.closing_file, p.closing_gain_db).await {
        Ok(t) => t,
        Err(e) => {
            step(StepId::ClosingVideo, StepMark::Failed(e.to_string()));
            warnings.push(format!("Could not play the closing countdown: {e}"));
            return None;
        }
    };
    let mut hall_before = None;
    if p.closing_to_hall {
        match d.screens.program().await {
            Ok(before) => hall_before = Some(before),
            Err(e) => warnings.push(format!("Could not read what the hall shows: {e}")),
        }
        if let Err(e) = cut(&*d.screens, c.screens_atem.decklink_input).await {
            warnings.push(format!("Could not switch the hall to the closing countdown: {e}"));
        }
    }
    // The stream audio goes down either way: in the hall only, the mics would carry the
    // video's sound from the hall speakers.
    let r = fade(&*d.stream, &c.stream_atem.mic_sources, Target::Floor, c.stream_atem.fade).await;
    if let Err(e) = r {
        warnings.push(format!("Could not fade the stream audio down: {e}"));
    }
    let mut stream_route = None;
    if p.closing_to_stream {
        match stream_to_countdown(d, c).await {
            Ok(prev) => stream_route = Some(prev),
            Err(e) => warnings.push(format!("The closing countdown does not go online: {e}")),
        }
    }
    if stream_route.is_none()
        && let Err(e) = keep_countdown_off_stream(d, c).await
    {
        warnings.push(format!("Could not turn off input 8 audio: {e}"));
    }
    let mut mark = StepMark::done();
    if let Err(e) = d.player.play().await {
        mark = StepMark::Failed(e.to_string());
        warnings.push(format!("Could not play the closing countdown: {e}"));
    } else {
        step(StepId::ClosingVideo, StepMark::Now(None));
        let skipped = skip.notified();
        tokio::pin!(skipped);
        let mut remaining = duration;
        loop {
            progress(Stage::Ending { remaining });
            tokio::select! {
                biased;
                _ = &mut skipped => {
                    if let Err(e) = d.player.stop().await {
                        warnings.push(format!("Could not stop the closing countdown: {e}"));
                    }
                    break;
                }
                ev = events.recv() => match ev {
                    Ok(PlayerEvent::Ended) => break,
                    Ok(PlayerEvent::Error(e)) => {
                        mark = StepMark::Failed(e.clone());
                        warnings.push(format!("The closing countdown stopped: {e}"));
                        break;
                    }
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => {
                        mark = StepMark::Failed("the player went away".into());
                        break;
                    }
                },
                _ = sleep(Duration::from_millis(250)) => {
                    remaining = duration.saturating_sub(d.player.position().unwrap_or_default());
                }
            }
        }
    }
    step(StepId::ClosingVideo, mark);
    if let Some(before) = hall_before
        && let Err(e) = cut(&*d.screens, before).await
    {
        warnings.push(format!("Could not return the hall screens: {e}"));
    }
    stream_route
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FLOOR_DB;
    use crate::config::AudioSourceId;
    use crate::devices::Switcher;
    use crate::fakes::{Call, fake_devices};
    use std::sync::Mutex;
    use tokio::time::Instant;

    const MIC: AudioSourceId = AudioSourceId { input: 1001, source: -65280 };

    fn stages() -> (Arc<Mutex<Vec<Stage>>>, impl Fn(Stage)) {
        let r: Arc<Mutex<Vec<Stage>>> = Arc::default();
        let r2 = r.clone();
        (r, move |s| r2.lock().unwrap().push(s))
    }

    fn pos(calls: &[Call], c: &Call) -> usize {
        calls.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c:?} not in {calls:?}"))
    }

    #[tokio::test(start_paused = true)]
    async fn end_runs_in_order_with_10s_hold() {
        let (d, log, h) = fake_devices();
        h.stream.set_audio_level_state(MIC, -3.0);
        let mut c = Config::default();
        c.stream_atem.closing_still = 2;
        let (seen, p) = stages();
        let start = Instant::now();
        let out = end_service(&d, &c, "b", Arc::default(), p, |_, _| {}).await;
        let took = start.elapsed();
        assert_eq!(out, EndOutcome { ended_broadcast: true, stopped_encoder: true, warnings: vec![] });
        // 1 s mic fade, then the 10 s hold, then YouTube.
        assert_eq!(took, Duration::from_secs(11));

        let calls = log.calls();
        assert_eq!(calls[..3], [Call::StreamSetMediaStill(1, 2), Call::StreamSetPreview(3010), Call::StreamAuto]);
        let last_fade = calls.iter().rposition(|x| matches!(x, Call::StreamSetAudioLevel(..))).unwrap();
        assert_eq!(calls[last_fade], Call::StreamSetAudioLevel(MIC, FLOOR_DB));
        let yt = pos(&calls, &Call::YtTransition(Phase::Complete));
        assert_eq!(yt, last_fade + 1);
        assert_eq!(calls[yt + 1..], [Call::EncoderStop]);

        let seen = seen.lock().unwrap();
        let want: Vec<Stage> =
            (1..=10).rev().map(|s| Stage::Ending { remaining: Duration::from_secs(s) }).chain([Stage::Ended]).collect();
        assert_eq!(*seen, want);
    }

    const OUT: u16 = 20;
    const DK: u16 = 30;
    const CAM8: u16 = 5;
    const IN8: AudioSourceId = AudioSourceId { input: 8, source: -65280 };

    /// The closing countdown on, to the hall and online, Cam 8 on input 8, the hall on 3.
    fn closing() -> (Devices, crate::fakes::CallLog, crate::fakes::FakeHandles, Config) {
        let (d, log, h) = fake_devices();
        let mut c = Config::default();
        c.playout.closing_on = true;
        c.videohub.input8_output = OUT;
        c.videohub.decklink_input = DK;
        c.stream_atem.mic_sources = vec![MIC];
        h.hub.set_route(OUT, CAM8);
        h.screens.set_program_state(3);
        h.stream.set_program_state(1);
        h.player.set_duration(Duration::from_secs(30));
        h.player.set_position(Some(Duration::ZERO));
        (d, log, h, c)
    }

    fn assert_in_order(calls: &[Call], want: &[Call]) {
        let mut it = calls.iter();
        for w in want {
            assert!(it.any(|c| c == w), "{w:?} missing or out of order in {calls:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_closing_countdown_plays_to_the_hall_and_online_before_the_still() {
        let (d, log, h, c) = closing();
        let hall = c.screens_atem.decklink_input;
        let (d2, c2) = (d.clone(), c.clone());
        let task = tokio::spawn(async move { end_service(&d2, &c2, "b", Arc::default(), |_| {}, |_, _| {}).await });
        sleep(Duration::from_secs(5)).await;
        assert!(!log.calls().iter().any(|x| matches!(x, Call::StreamSetMediaStill(..))), "no still yet");
        assert_eq!(h.screens.program().await.unwrap(), hall);
        assert_eq!(h.stream.program().await.unwrap(), 8);
        h.player.emit(PlayerEvent::Ended);
        let out = task.await.unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_in_order(
            &log.calls(),
            &[
                Call::PlayerLoad(c.playout.closing_file.clone(), 0.0),
                Call::ScreensSetPreview(hall),
                Call::ScreensAuto,
                Call::StreamSetAudioLevel(MIC, FLOOR_DB),
                Call::HubRoute { output: OUT, input: DK },
                Call::StreamSetAudioOn(IN8, true),
                Call::StreamSetPreview(8),
                Call::StreamAuto,
                Call::PlayerPlay,
                Call::ScreensSetPreview(3),
                Call::ScreensAuto,
                Call::StreamSetMediaStill(1, c.stream_atem.closing_still),
                Call::StreamAuto,
                Call::StreamSetAudioOn(IN8, false),
                Call::HubRoute { output: OUT, input: CAM8 },
                Call::YtTransition(Phase::Complete),
                Call::EncoderStop,
            ],
        );
        assert_eq!(h.screens.program().await.unwrap(), 3, "the hall back on what it showed");
    }

    #[tokio::test(start_paused = true)]
    async fn a_press_during_the_closing_countdown_skips_it_but_not_the_hold() {
        let (d, log, _h, mut c) = closing();
        c.playout.closing_to_stream = false;
        let skip = Arc::new(Notify::new());
        let (d2, c2, s2) = (d.clone(), c.clone(), skip.clone());
        let task = tokio::spawn(async move { end_service(&d2, &c2, "b", s2, |_| {}, |_, _| {}).await });
        sleep(Duration::from_secs(3)).await;
        skip.notify_one();
        sleep(Duration::from_secs(5)).await;
        let calls = log.calls();
        assert!(calls.contains(&Call::PlayerStop));
        assert!(!calls.iter().any(|x| matches!(x, Call::HubRoute { .. })), "hall only: no route");
        assert!(!calls.contains(&Call::YtTransition(Phase::Complete)), "the hold still runs");
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_hall_only_closing_countdown_keeps_its_sound_off_the_stream() {
        // The hall PA plays it: the stream mics would carry it, and input 8 must be off.
        let (d, log, h, mut c) = closing();
        c.playout.closing_to_stream = false;
        c.playout.closing_gain_db = -6.0;
        let (d2, c2) = (d.clone(), c.clone());
        let task = tokio::spawn(async move { end_service(&d2, &c2, "b", Arc::default(), |_| {}, |_, _| {}).await });
        sleep(Duration::from_secs(5)).await;
        h.player.emit(PlayerEvent::Ended);
        let out = task.await.unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let calls = log.calls();
        assert_in_order(
            &calls,
            &[
                Call::PlayerLoad(c.playout.closing_file.clone(), -6.0),
                Call::StreamSetAudioLevel(MIC, FLOOR_DB),
                Call::StreamSetAudioOn(IN8, false),
                Call::PlayerPlay,
            ],
        );
        assert!(!calls.contains(&Call::StreamSetAudioOn(IN8, true)));
    }

    #[tokio::test(start_paused = true)]
    async fn the_closing_countdown_turned_off_or_going_nowhere_is_not_played() {
        let (d, log, _h, mut c) = closing();
        c.playout.closing_on = false;
        end_service(&d, &c, "b", Arc::default(), |_| {}, |_, _| {}).await;
        c.playout.closing_on = true;
        c.playout.closing_to_hall = false;
        c.playout.closing_to_stream = false;
        end_service(&d, &c, "b", Arc::default(), |_| {}, |_, _| {}).await;
        assert!(!log.calls().iter().any(|x| matches!(x, Call::PlayerLoad(..))));
    }

    #[tokio::test(start_paused = true)]
    async fn yt_complete_not_before_hold_elapsed() {
        let (d, log, _h) = fake_devices();
        let c = Config::default();
        let (d2, c2) = (d.clone(), c.clone());
        let task = tokio::spawn(async move { end_service(&d2, &c2, "b", Arc::default(), |_| {}, |_, _| {}).await });
        tokio::time::sleep(Duration::from_millis(10_900)).await;
        assert!(!log.calls().contains(&Call::YtTransition(Phase::Complete)));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(log.calls().contains(&Call::YtTransition(Phase::Complete)));
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn second_press_skips_hold() {
        // A permit stored before the hold begins counts.
        let (d, log, _h) = fake_devices();
        let skip = Arc::new(Notify::new());
        skip.notify_one();
        let start = Instant::now();
        let out = end_service(&d, &Config::default(), "b", skip, |_| {}, |_, _| {}).await;
        assert_eq!(start.elapsed(), Duration::from_secs(1)); // only the fade
        assert!(out.ended_broadcast && out.stopped_encoder);
        assert!(log.calls().ends_with(&[Call::YtTransition(Phase::Complete), Call::EncoderStop]));

        // A press during the hold ends it at once.
        let (d, _log, _h) = fake_devices();
        let skip = Arc::new(Notify::new());
        let (d2, s2) = (d.clone(), skip.clone());
        let (seen, p) = stages();
        let task = tokio::spawn(async move { end_service(&d2, &Config::default(), "b", s2, p, |_, _| {}).await });
        let start = Instant::now();
        tokio::time::sleep(Duration::from_millis(3_500)).await;
        skip.notify_one();
        let out = task.await.unwrap();
        assert!(out.ended_broadcast && out.stopped_encoder);
        assert_eq!(start.elapsed(), Duration::from_millis(3_500));
        assert_eq!(seen.lock().unwrap().last(), Some(&Stage::Ended));
        assert!(seen.lock().unwrap().contains(&Stage::Ending { remaining: Duration::from_secs(8) }));
        assert!(!seen.lock().unwrap().contains(&Stage::Ending { remaining: Duration::from_secs(7) }));
    }

    #[tokio::test(start_paused = true)]
    async fn youtube_failure_still_stops_encoder() {
        let (d, log, h) = fake_devices();
        h.yt.fail_times("transition", 4);
        let out = end_service(&d, &Config::default(), "b", Arc::default(), |_| {}, |_, _| {}).await;
        assert!(!out.ended_broadcast);
        assert!(out.stopped_encoder);
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
        let calls = log.calls();
        let tries = calls.iter().filter(|c| **c == Call::YtTransition(Phase::Complete)).count();
        assert_eq!(tries, 4);
        assert_eq!(calls.last(), Some(&Call::EncoderStop));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_accepts_already_complete() {
        let (d, _log, h) = fake_devices();
        h.yt.fail_next("transition");
        h.yt.set_life_cycle("complete");
        let out = end_service(&d, &Config::default(), "b", Arc::default(), |_| {}, |_, _| {}).await;
        assert!(out.ended_broadcast, "{:?}", out.warnings);
    }

    #[tokio::test(start_paused = true)]
    async fn hanging_youtube_is_bounded_and_encoder_stops() {
        let (d, log, h) = fake_devices();
        h.yt.set_transition_hangs(true);
        let start = Instant::now();
        let out = end_service(&d, &Config::default(), "b", Arc::default(), |_| {}, |_, _| {}).await;
        assert_eq!(start.elapsed(), Duration::from_secs(11) + GO_PUBLIC_WAIT);
        assert!(!out.ended_broadcast && out.stopped_encoder);
        assert!(out.warnings.iter().any(|w| w.contains("YouTube")));
        assert_eq!(log.calls().last(), Some(&Call::EncoderStop));
    }

    #[tokio::test(start_paused = true)]
    async fn device_failures_become_warnings_and_sequence_continues() {
        let (d, log, h) = fake_devices();
        h.stream.fail_next("set_media_player_still");
        h.stream.fail_next("set_preview");
        h.encoder.fail_next("stop");
        let out = end_service(&d, &Config::default(), "b", Arc::default(), |_| {}, |_, _| {}).await;
        assert!(out.ended_broadcast);
        assert!(!out.stopped_encoder);
        assert_eq!(out.warnings.len(), 3, "{:?}", out.warnings);
        assert!(!log.calls().contains(&Call::StreamAuto)); // no Auto after a failed preview
    }
}
