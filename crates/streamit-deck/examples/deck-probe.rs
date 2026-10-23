//! Draws a demo snapshot on the first Stream Deck XL and prints every command a key
//! press would send. Close Companion first: it holds the deck open.
//!
//!     cargo run -p streamit-deck --example deck-probe
use std::time::Duration;
use streamit_core::engine::{Command, DeviceHealth, DeviceKind, EngineHandle, Snapshot};
use streamit_core::golive::Stage;
use streamit_core::recovery::Offer;
use streamit_core::rollin::{Cue, CueState, Destination};
use streamit_deck::DeckSurface;
use tokio::sync::{mpsc, watch};

fn cue(name: &str) -> Cue {
    Cue {
        path: format!("{name}.mp4").into(),
        name: name.into(),
        destination: Destination::HallAndOnline,
        gain_db: 0.0,
        return_input: None,
    }
}

fn demo() -> Snapshot {
    let mut s = Snapshot {
        stage: Stage::Countdown { remaining: Duration::from_secs(42), public: false },
        ..Snapshot::default()
    };
    s.device_status.insert(DeviceKind::StreamAtem, DeviceHealth::Ok);
    s.device_status.insert(DeviceKind::ScreensAtem, DeviceHealth::Ok);
    s.device_status.insert(DeviceKind::VideoHub, DeviceHealth::Warning("slow".into()));
    s.device_status.insert(DeviceKind::WebPresenter, DeviceHealth::Down("offline".into()));
    s.device_status.insert(DeviceKind::YouTube, DeviceHealth::Ok);
    s.offers = vec![Offer::GoPublic, Offer::GoLive];
    s.cues = vec![
        (cue("Welcome"), CueState::Cued),
        (cue("Notices and announcements for this week"), CueState::Playing),
        (cue("Offering"), CueState::Played),
        (cue("Broken file"), CueState::Error("missing".into())),
    ];
    s.playing = Some(1);
    s.mics_ducked = true;
    s.flash = Some(0);
    s
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let (tx, mut rx) = mpsc::channel::<Command>(16);
    let (_snap_tx, snap_rx) = watch::channel(demo());
    DeckSurface::run(EngineHandle { tx, rx: snap_rx });
    println!("Drawing a demo snapshot. Press keys; Ctrl+C to quit.");
    while let Some(cmd) = rx.recv().await {
        println!("{cmd:?}");
    }
}
