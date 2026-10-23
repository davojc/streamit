//! Startup assessment and recovery. `assess` only reads; it never changes a device.
//! `recover` is the one-press fix for a crash during a video.
use crate::audio::{LiveLevels, Target, down_threshold, fade};
use crate::config::{AudioSourceId, Config};
use crate::devices::{Devices, EncoderStatus, Result};
use crate::golive::{Readiness, Stage, cut};
use crate::rollin::{STEREO_SOURCE, TRANSITION_TIMEOUT};
use std::time::Duration;

/// Longest `assess` waits for the broadcast's life cycle; past it the life cycle is unknown.
pub const LIFE_CYCLE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Recover,
    GoPublic,
    GoLive,
    EndBroadcast,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub stage: Stage,
    pub offer: Vec<Offer>,
    /// A broadcast was given but its life cycle could not be read (failed or timed
    /// out), so the YouTube-based offers (Go Public, Go Live, End Broadcast) say nothing.
    pub broadcast_unknown: bool,
}

/// Read every device and say what the operator may press. Read-only: no action
/// call is ever made, and a failed read counts as "unknown", never as a finding.
pub async fn assess(d: &Devices, c: &Config, bid: Option<&str>, player_active: bool) -> Assessment {
    let sa = &c.stream_atem;
    let stream_prog = d.stream.program().await.ok();
    let screens_prog = d.screens.program().await.ok();
    let hub = &c.videohub;
    let route_on_decklink = if hub.input8_output != 0 && hub.decklink_input != 0 {
        d.hub.route_of(hub.input8_output).await.ok() == Some(hub.decklink_input)
    } else {
        false
    };
    let encoder = d.encoder.status().await.ok();
    let streaming = encoder == Some(EncoderStatus::Streaming);
    let on_live_input = stream_prog.is_some_and(|p| sa.live_inputs.contains(&p));

    let mut mic_muted = false;
    if streaming && on_live_input {
        let down = down_threshold(sa.duck_db);
        for &m in &sa.mic_sources {
            if d.stream.audio_level(m).await.is_ok_and(|db| db <= down) {
                mic_muted = true;
            }
        }
    }

    let recover_needed = stream_prog == Some(sa.decklink_input)
        || screens_prog == Some(c.screens_atem.decklink_input)
        || route_on_decklink
        || mic_muted;

    let life_cycle = match bid {
        Some(id) => tokio::time::timeout(LIFE_CYCLE_TIMEOUT, d.yt.life_cycle(id)).await.ok().and_then(|r| r.ok()),
        None => None,
    };
    let broadcast_unknown = bid.is_some() && life_cycle.is_none();
    let lc = life_cycle.as_deref();

    let mut stage = Stage::Idle;
    let mut offer = Vec::new();
    if recover_needed && !player_active {
        offer.push(Offer::Recover);
    }
    if lc == Some("live") && on_live_input {
        stage = Stage::Live;
    } else if lc == Some("live") && stream_prog == Some(d.stream.media_player_input(sa.media_player)) {
        stage = Stage::Live;
        offer.extend([Offer::GoLive, Offer::EndBroadcast]);
    } else if streaming && lc == Some("testing") && stream_prog.is_some() {
        stage = Stage::Prepared(Readiness::Ready);
        offer.extend([Offer::GoPublic, Offer::GoLive]);
    }
    Assessment { stage, offer, broadcast_unknown }
}

/// One-press fix for a crash during a video. Every step is attempted; failures
/// are collected and returned together. The route restore is always last, and is
/// skipped (a failure) while the Stream ATEM is still on the DeckLink input.
pub async fn recover(d: &Devices, c: &Config, levels: &LiveLevels) -> Result<()> {
    let sa = &c.stream_atem;
    let mut failures: Vec<String> = Vec::new();
    let mut fail = |what: &str, e: crate::devices::DeviceError| failures.push(format!("{what}: {e}"));

    // 1. Stream ATEM off the DeckLink. A real ATEM reports the old program until a mix
    // completes (I3), so a running transition lands first.
    if let Err(e) = d.stream.wait_transition_done(TRANSITION_TIMEOUT).await {
        fail("A stream transition did not finish", e);
    }
    match d.stream.program().await {
        Ok(p) if p == sa.decklink_input => {
            let r = match d.stream.preview().await {
                Ok(pv) if pv == sa.decklink_input => cut(&*d.stream, sa.default_live_input).await,
                Ok(_) => d.stream.auto().await,
                Err(e) => Err(e),
            };
            match r {
                Ok(()) => {
                    if let Err(e) = d.stream.wait_transition_done(TRANSITION_TIMEOUT).await {
                        fail("The stream transition off the video did not finish", e);
                    }
                }
                Err(e) => fail("Could not switch the stream off the video", e),
            }
        }
        Ok(_) => {}
        Err(e) => fail("Could not read the Stream ATEM program", e),
    }

    // 2. Hall screens off the DeckLink.
    let sc = &c.screens_atem;
    if let Err(e) = d.screens.wait_transition_done(TRANSITION_TIMEOUT).await {
        fail("A hall screens transition did not finish", e);
    }
    match d.screens.program().await {
        Ok(p) if p == sc.decklink_input => {
            let r = match d.screens.preview().await {
                Ok(pv) if pv == sc.decklink_input => cut(&*d.screens, sc.t0_input).await,
                Ok(_) => d.screens.auto().await,
                Err(e) => Err(e),
            };
            if let Err(e) = r {
                fail("Could not switch the hall screens off the video", e);
            }
        }
        Ok(_) => {}
        Err(e) => fail("Could not read the hall screens program", e),
    }

    // 3. Mics back to their live level (Target::Live falls back to 0 dB), unless the
    // stream is on the holding still, where the mics stay down (R24).
    match d.stream.program().await {
        Ok(p) if p == d.stream.media_player_input(sa.media_player) => {}
        _ => {
            if let Err(e) = fade(&*d.stream, &sa.mic_sources, Target::Live(levels), sa.fade).await {
                fail("Could not fade the mics up", e);
            }
        }
    }

    // 4. Input 8 audio off.
    let in8 = AudioSourceId { input: sa.decklink_input, source: STEREO_SOURCE };
    if let Err(e) = d.stream.set_audio_on(in8, false).await {
        fail("Could not turn off input 8 audio", e);
    }

    // 5. Route input 8 back to the camera, last, and only once it is off air.
    let hub = &c.videohub;
    if hub.input8_output != 0 {
        match d.stream.program().await {
            Ok(p) if p != sa.decklink_input => {
                if hub.fallback_normal_input == 0 {
                    failures.push("No normal input is configured for input 8, so its VideoHub route was not restored".into());
                } else if let Err(e) = d.hub.route(hub.input8_output, hub.fallback_normal_input).await {
                    fail("Could not restore the VideoHub route for input 8", e);
                }
            }
            Ok(_) => failures.push("Input 8 is still on air, so its VideoHub route was not restored".into()),
            Err(e) => fail("Could not read the Stream ATEM program, so the VideoHub route was not restored", e),
        }
    }

    if failures.is_empty() { Ok(()) } else { Err(crate::devices::DeviceError::Other(failures.join("; "))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FLOOR_DB;
    use crate::devices::DeviceError;
    use crate::fakes::{Call, fake_devices};

    const OUT: u16 = 12;
    const DK: u16 = 40;
    const CAM8: u16 = 5;
    const MIC: AudioSourceId = AudioSourceId { input: 1001, source: -65280 };

    fn cfg() -> Config {
        let mut c = Config::default();
        c.videohub.input8_output = OUT;
        c.videohub.decklink_input = DK;
        c.videohub.fallback_normal_input = CAM8;
        c
    }

    fn rig() -> (Devices, crate::fakes::CallLog, crate::fakes::FakeHandles) {
        let (d, log, h) = fake_devices();
        h.hub.set_route(OUT, CAM8);
        h.stream.set_program_state(1);
        h.stream.set_preview_state(2);
        h.screens.set_program_state(6);
        h.screens.set_preview_state(7);
        h.stream.set_audio_level_state(MIC, -3.0);
        (d, log, h)
    }

    #[tokio::test(start_paused = true)]
    async fn assess_makes_no_action_calls() {
        let (d, log, h) = rig();
        h.stream.set_program_state(8);
        h.screens.set_program_state(8);
        h.hub.set_route(OUT, DK);
        h.encoder.set_status(EncoderStatus::Streaming);
        h.yt.set_life_cycle("testing");
        let _ = assess(&d, &cfg(), Some("b"), false).await;
        h.yt.set_life_cycle("live");
        let _ = assess(&d, &cfg(), Some("b"), false).await;
        assert!(log.calls().is_empty(), "{:?}", log.calls());
    }

    #[tokio::test(start_paused = true)]
    async fn slow_life_cycle_is_unknown_after_timeout() {
        let (d, _l, h) = rig();
        h.yt.set_life_cycle("live");
        h.yt.set_delay("life_cycle", Duration::from_secs(60));
        let start = tokio::time::Instant::now();
        let a = assess(&d, &cfg(), Some("b"), false).await;
        assert!(start.elapsed() <= LIFE_CYCLE_TIMEOUT + Duration::from_millis(100), "{:?}", start.elapsed());
        assert_eq!(a, Assessment { stage: Stage::Idle, offer: vec![], broadcast_unknown: true });
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_start_is_idle_with_no_offers() {
        let (d, _l, _h) = rig();
        let a = assess(&d, &cfg(), None, false).await;
        assert_eq!(a, Assessment { stage: Stage::Idle, offer: vec![], broadcast_unknown: false });
    }

    #[tokio::test(start_paused = true)]
    async fn stuck_on_decklink_offers_recover() {
        let (d, _l, h) = rig();
        h.stream.set_program_state(8);
        let a = assess(&d, &cfg(), None, false).await;
        assert_eq!(a.offer, vec![Offer::Recover]);
        assert_eq!(a.stage, Stage::Idle);
        // not while a video is playing
        assert!(assess(&d, &cfg(), None, true).await.offer.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn screens_on_decklink_or_route_on_decklink_offers_recover() {
        let (d, _l, h) = rig();
        h.screens.set_program_state(8);
        assert_eq!(assess(&d, &cfg(), None, false).await.offer, vec![Offer::Recover]);
        h.screens.set_program_state(6);
        h.hub.set_route(OUT, DK);
        assert_eq!(assess(&d, &cfg(), None, false).await.offer, vec![Offer::Recover]);
    }

    #[tokio::test(start_paused = true)]
    async fn muted_mics_on_live_stream_offer_recover() {
        let (d, _l, h) = rig();
        h.encoder.set_status(EncoderStatus::Streaming);
        h.stream.set_audio_level_state(MIC, FLOOR_DB);
        assert_eq!(assess(&d, &cfg(), None, false).await.offer, vec![Offer::Recover]);
        h.encoder.set_status(EncoderStatus::Idle);
        assert!(assess(&d, &cfg(), None, false).await.offer.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn mics_down_check_uses_duck_threshold() {
        let (d, _l, h) = rig();
        let mut c = cfg();
        c.stream_atem.duck_db = -20.0;
        h.encoder.set_status(EncoderStatus::Streaming);
        h.stream.set_audio_level_state(MIC, -20.0); // left ducked
        assert_eq!(assess(&d, &c, None, false).await.offer, vec![Offer::Recover]);
        h.stream.set_audio_level_state(MIC, -10.0);
        assert!(assess(&d, &c, None, false).await.offer.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn testing_broadcast_offers_go_public_and_go_live() {
        let (d, _l, h) = rig();
        h.encoder.set_status(EncoderStatus::Streaming);
        h.yt.set_life_cycle("testing");
        let a = assess(&d, &cfg(), Some("b"), false).await;
        assert_eq!(a.stage, Stage::Prepared(Readiness::Ready));
        assert_eq!(a.offer, vec![Offer::GoPublic, Offer::GoLive]);
        // no bid: YouTube is not read
        let a = assess(&d, &cfg(), None, false).await;
        assert_eq!(a.stage, Stage::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn live_broadcast_on_live_input_is_live() {
        let (d, _l, h) = rig();
        h.encoder.set_status(EncoderStatus::Streaming);
        h.yt.set_life_cycle("live");
        let a = assess(&d, &cfg(), Some("b"), false).await;
        assert_eq!(a.stage, Stage::Live);
        assert!(a.offer.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn live_on_media_player_offers_go_live_and_end() {
        let (d, _l, h) = rig();
        h.yt.set_life_cycle("live");
        h.stream.set_program_state(d.stream.media_player_input(1));
        let a = assess(&d, &cfg(), Some("b"), false).await;
        assert_eq!(a.stage, Stage::Live);
        assert_eq!(a.offer, vec![Offer::GoLive, Offer::EndBroadcast]);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_reads_are_unknown_and_offer_nothing() {
        let (d, _l, h) = rig();
        h.stream.fail_next("program");
        h.screens.fail_next("program");
        h.hub.fail_next("route_of");
        h.encoder.fail_next("status");
        h.yt.fail_next("life_cycle");
        let a = assess(&d, &cfg(), Some("b"), false).await;
        assert_eq!(a, Assessment { stage: Stage::Idle, offer: vec![], broadcast_unknown: true });
    }

    #[tokio::test(start_paused = true)]
    async fn unreadable_program_while_streaming_is_idle() {
        let (d, _l, h) = rig();
        h.encoder.set_status(EncoderStatus::Streaming);
        h.yt.set_life_cycle("testing");
        h.stream.fail_next("program");
        let a = assess(&d, &cfg(), Some("b"), false).await;
        assert_eq!(a.stage, Stage::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn recover_restores_route_after_cutting_away() {
        let (d, log, h) = rig();
        h.stream.set_program_state(8);
        h.stream.set_preview_state(8);
        h.screens.set_program_state(8);
        h.screens.set_preview_state(8);
        h.hub.set_route(OUT, DK);
        let live = LiveLevels::default();
        recover(&d, &cfg(), &live).await.unwrap();
        let calls = log.calls();
        let pos = |c: &Call| calls.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c:?} missing in {calls:?}"));
        assert!(pos(&Call::StreamSetPreview(1)) < pos(&Call::StreamAuto));
        assert!(pos(&Call::ScreensSetPreview(6)) < pos(&Call::ScreensAuto));
        let route = pos(&Call::HubRoute { output: OUT, input: CAM8 });
        assert!(pos(&Call::StreamAuto) < route && pos(&Call::ScreensAuto) < route);
        assert!(pos(&Call::StreamSetAudioOn(AudioSourceId { input: 8, source: STEREO_SOURCE }, false)) < route);
        let last_fade = calls.iter().rposition(|c| matches!(c, Call::StreamSetAudioLevel(..))).expect("mics faded");
        assert!(last_fade < route, "mic fade must precede the route restore");
        assert_eq!(route, calls.len() - 1);
    }

    #[tokio::test(start_paused = true)]
    async fn recover_on_holding_still_leaves_mics_down() {
        let is_mic = |c: &Call| matches!(c, Call::StreamSetAudioLevel(m, _) if *m == MIC);
        // On the holding still already.
        let (d, log, h) = rig();
        h.stream.set_program_state(d.stream.media_player_input(1));
        h.stream.set_audio_level_state(MIC, FLOOR_DB);
        recover(&d, &cfg(), &LiveLevels::default()).await.unwrap();
        assert!(!log.calls().iter().any(is_mic), "{:?}", log.calls());
        assert_eq!(d.stream.audio_level(MIC).await.unwrap(), FLOOR_DB);

        // Stuck on a video played over the holding still: Recover returns to the still.
        let (d, log, h) = rig();
        h.stream.set_program_state(8);
        h.stream.set_preview_state(d.stream.media_player_input(1));
        h.stream.set_audio_level_state(MIC, FLOOR_DB);
        recover(&d, &cfg(), &LiveLevels::default()).await.unwrap();
        assert_eq!(d.stream.program().await.unwrap(), d.stream.media_player_input(1));
        assert!(!log.calls().iter().any(is_mic), "{:?}", log.calls());
    }

    #[tokio::test(start_paused = true)]
    async fn recover_waits_for_a_running_transition() {
        let (d, log, h) = rig();
        h.stream.set_auto_delay(std::time::Duration::from_millis(1000));
        h.screens.set_auto_delay(std::time::Duration::from_millis(1000));
        h.hub.set_route(OUT, DK);
        // Both switchers are mid-mix onto the DeckLink when Recover is pressed.
        d.stream.set_preview(8).await.unwrap();
        d.stream.auto().await.unwrap();
        d.screens.set_preview(8).await.unwrap();
        d.screens.auto().await.unwrap();
        recover(&d, &cfg(), &LiveLevels::default()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert_ne!(d.stream.program().await.unwrap(), 8, "{:?}", log.calls());
        assert_ne!(d.screens.program().await.unwrap(), 8, "{:?}", log.calls());
        assert_eq!(d.hub.route_of(OUT).await.unwrap(), CAM8);
    }

    #[tokio::test(start_paused = true)]
    async fn recover_with_preview_holding_cue_just_autos() {
        let (d, log, h) = rig();
        h.stream.set_program_state(8);
        h.stream.set_preview_state(3);
        recover(&d, &cfg(), &LiveLevels::default()).await.unwrap();
        assert!(!log.calls().iter().any(|c| matches!(c, Call::StreamSetPreview(_))));
        assert!(log.calls().contains(&Call::StreamAuto));
    }

    #[tokio::test(start_paused = true)]
    async fn recover_skips_route_when_stream_still_on_decklink() {
        let (d, log, h) = rig();
        h.stream.set_program_state(8);
        h.stream.fail_next("auto");
        h.hub.set_route(OUT, DK);
        let r = recover(&d, &cfg(), &LiveLevels::default()).await;
        assert!(matches!(r, Err(DeviceError::Other(_))));
        assert!(!log.calls().iter().any(|c| matches!(c, Call::HubRoute { .. })));
        // other steps were still attempted
        assert!(log.calls().iter().any(|c| matches!(c, Call::StreamSetAudioOn(_, false))));
    }
}
