//! Player tests against real GStreamer with `SinkKind::Fake`. Built only with `--features gst`;
//! each test skips itself when GStreamer (or x264enc / mp4mux) is not available.
#![cfg(feature = "gst")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use streamit_core::config::PlayoutConfig;
use streamit_core::devices::{Player, PlayerEvent};
use streamit_playout::{GstPlayer, SinkKind};

/// Writes a 2 s, 30 fps, video-only H.264 MP4. `None` when the encoder is unavailable.
fn make_clip(name: &str) -> Option<PathBuf> {
    if gst::init().is_err() {
        eprintln!("skipping: GStreamer not available");
        return None;
    }
    if gst::ElementFactory::find("x264enc").is_none()
        || gst::ElementFactory::find("mp4mux").is_none()
    {
        eprintln!("skipping: x264enc or mp4mux not available");
        return None;
    }
    let path = std::env::temp_dir().join(name);
    let pipeline = gst::parse::launch(
        "videotestsrc num-buffers=60 ! video/x-raw,width=320,height=240,framerate=30/1 ! \
         x264enc ! mp4mux ! filesink name=out",
    )
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
    Some(path)
}

#[tokio::test(flavor = "multi_thread")]
async fn fake_player_loads_returns_duration_and_emits_ended() {
    let Some(clip) = make_clip("streamit-playout-2s.mp4") else {
        return;
    };
    let player = GstPlayer::new(&PlayoutConfig::default(), SinkKind::Fake).unwrap();

    let duration = player.load(&clip, -6.0).await.unwrap();
    assert!(
        duration > Duration::from_millis(1900) && duration < Duration::from_millis(2100),
        "{duration:?}"
    );

    let mut events = player.events();
    player.play().await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(10), events.recv()).await;
    assert_eq!(event.unwrap().unwrap(), PlayerEvent::Ended);
    player.stop().await.unwrap();
    let _ = std::fs::remove_file(clip);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_file_errors_on_load() {
    if gst::init().is_err() {
        eprintln!("skipping: GStreamer not available");
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
