//! Player tests against real GStreamer with `SinkKind::Fake`. Built only with `--features gst`;
//! each test skips itself when GStreamer (or an encoder it needs) is not available.
#![cfg(feature = "gst")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use streamit_core::config::PlayoutConfig;
use streamit_core::devices::{Player, PlayerEvent};
use streamit_playout::{GstPlayer, SinkKind};

/// AAC encoders, in order of preference; a complete Windows install has at least
/// `avenc_aac` (gst-libav) or `mfaacenc` (Media Foundation).
const AAC_ENCODERS: &[&str] = &["avenc_aac", "mfaacenc", "fdkaacenc", "voaacenc", "faac"];

fn gst_ready() -> bool {
    if gst::init().is_err() {
        eprintln!("skipping: GStreamer not available");
        return false;
    }
    true
}

fn have(factory: &str) -> bool {
    gst::ElementFactory::find(factory).is_some()
}

/// Runs an encoding pipeline (with a `filesink name=out`) to EOS, writing `name` in temp.
fn encode(desc: &str, name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(name);
    let pipeline = gst::parse::launch(desc)
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    let out = pipeline.by_name("out").unwrap();
    out.set_property("location", path.to_str().unwrap());
    pipeline.set_state(gst::State::Playing).unwrap();
    let bus = pipeline.bus().unwrap();
    let msg = bus
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(30),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .expect("clip encoding finished");
    assert!(
        matches!(msg.view(), gst::MessageView::Eos(_)),
        "encoding failed: {msg:?}"
    );
    pipeline.set_state(gst::State::Null).unwrap();
    path
}

/// A 2 s, 30 fps, video-only H.264 MP4 (exercises the silence branch).
fn video_only_clip(name: &str) -> Option<PathBuf> {
    if !gst_ready() {
        return None;
    }
    if !have("x264enc") || !have("mp4mux") {
        eprintln!("skipping: x264enc or mp4mux not available");
        return None;
    }
    Some(encode(
        "videotestsrc num-buffers=60 ! video/x-raw,width=320,height=240,framerate=30/1 ! \
         x264enc tune=zerolatency ! mp4mux ! filesink name=out",
        name,
    ))
}

/// A 2 s H.264 + AAC MP4 (exercises `dec. ! audioconvert`, volume and 48 kHz stereo).
/// The tone is 44.1 kHz mono so the player really has to resample and upmix.
fn clip_with_audio(name: &str) -> Option<PathBuf> {
    if !gst_ready() {
        return None;
    }
    let Some(aac) = AAC_ENCODERS.iter().find(|f| have(f)) else {
        eprintln!("skipping: no AAC encoder available");
        return None;
    };
    if !have("x264enc") || !have("mp4mux") {
        eprintln!("skipping: x264enc or mp4mux not available");
        return None;
    }
    Some(encode(
        &format!(
            "videotestsrc num-buffers=60 ! video/x-raw,width=320,height=240,framerate=30/1 ! \
             x264enc tune=zerolatency ! queue ! mp4mux name=mux ! filesink name=out \
             audiotestsrc num-buffers=200 samplesperbuffer=441 ! \
             audio/x-raw,rate=44100,channels=1 ! audioconvert ! audioresample ! {aac} ! \
             queue ! mux."
        ),
        name,
    ))
}

async fn plays_to_end(clip: &Path) {
    let player = GstPlayer::new(&PlayoutConfig::default(), SinkKind::Fake).unwrap();

    let duration = player.load(clip, -6.0).await.unwrap();
    assert!(
        duration > Duration::from_millis(1900) && duration < Duration::from_millis(2100),
        "{duration:?}"
    );

    let mut events = player.events();
    player.play().await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(10), events.recv()).await;
    assert_eq!(event.unwrap().unwrap(), PlayerEvent::Ended);
    player.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_player_loads_returns_duration_and_emits_ended() {
    let Some(clip) = clip_with_audio("streamit-playout-2s-av.mp4") else {
        return;
    };
    plays_to_end(&clip).await;
    let _ = std::fs::remove_file(clip);
}

#[tokio::test(flavor = "multi_thread")]
async fn video_only_file_plays_with_silence_and_ends() {
    let Some(clip) = video_only_clip("streamit-playout-2s-video.mp4") else {
        return;
    };
    plays_to_end(&clip).await;
    let _ = std::fs::remove_file(clip);
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_before_end_sends_no_event() {
    let Some(clip) = video_only_clip("streamit-playout-2s-stop.mp4") else {
        return;
    };
    let player = GstPlayer::new(&PlayoutConfig::default(), SinkKind::Fake).unwrap();
    player.load(&clip, 0.0).await.unwrap();
    let mut events = player.events();
    player.play().await.unwrap();
    player.stop().await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(3), events.recv()).await;
    assert!(event.is_err(), "a stopped clip reports nothing: {event:?}");
    let _ = std::fs::remove_file(clip);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_file_errors_on_load() {
    if !gst_ready() {
        return;
    }
    let player = GstPlayer::new(&PlayoutConfig::default(), SinkKind::Fake).unwrap();
    let missing = Path::new("definitely-not-here-streamit.mp4");
    assert!(player.load(missing, 0.0).await.is_err());
    assert!(
        player.play().await.is_err(),
        "nothing is loaded after a failed load"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_or_corrupt_file_errors_on_load() {
    if !gst_ready() {
        return;
    }
    let player = GstPlayer::new(&PlayoutConfig::default(), SinkKind::Fake).unwrap();
    let dir = std::env::temp_dir();
    let empty = dir.join("streamit-playout-empty.mp4");
    let corrupt = dir.join("streamit-playout-corrupt.mp4");
    std::fs::write(&empty, b"").unwrap();
    std::fs::write(
        &corrupt,
        b"this is not a video file, just some text".repeat(100),
    )
    .unwrap();
    for file in [&empty, &corrupt] {
        let result = tokio::time::timeout(Duration::from_secs(30), player.load(file, 0.0)).await;
        let result = result.expect("load gives up rather than hanging");
        assert!(result.is_err(), "{} should not load", file.display());
        assert!(
            player.play().await.is_err(),
            "nothing is loaded after a failed load"
        );
    }
    let _ = std::fs::remove_file(empty);
    let _ = std::fs::remove_file(corrupt);
}
