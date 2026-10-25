//! Draws a demo snapshot on the first Stream Deck XL and prints every command a key
//! press would send. Close Companion first: it holds the deck open. Shared by the
//! `deck-probe` example and `streamit probe deck`. Needs a multi-thread tokio runtime.
use crate::DeckSurface;
use std::time::Duration;
use streamit_core::engine::{Command, EngineHandle, Snapshot};
use streamit_core::golive::Stage;
use streamit_core::recovery::Offer;
use streamit_core::rollin::{Cue, CueState, Destination};
use tokio::sync::{mpsc, watch};

fn cue(name: &str) -> Cue {
    Cue {
        path: format!("{name}.mp4").into(),
        name: name.into(),
        destination: Destination::HallAndOnline,
        gain_db: 0.0,
        return_input: None,
        scene: None,
    }
}

/// The snapshot the probe draws.
pub fn demo() -> Snapshot {
    let mut s = Snapshot {
        stage: Stage::Live,
        on_air_since: Some(std::time::SystemTime::now() - Duration::from_secs(42 * 60)),
        ..Snapshot::default()
    };
    s.offers = vec![Offer::GoPublic];
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

/// Runs until interrupted (Ctrl+C). Takes no arguments.
pub async fn run(_args: &[String]) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::channel::<Command>(16);
    let (_snap_tx, snap_rx) = watch::channel(demo());
    DeckSurface::run(
        EngineHandle { tx, rx: snap_rx },
        std::sync::Arc::new(tokio::sync::watch::channel(crate::layout::Page::default()).0),
    );
    println!("Drawing a demo snapshot. Press keys; Ctrl+C to quit.");
    while let Some(cmd) = rx.recv().await {
        println!("{cmd:?}");
    }
    Ok(())
}
