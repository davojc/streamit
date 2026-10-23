//! Hardware check for the playout PC (needs `--features gst`).
//!
//!   playout-probe play <file> <gain_db>   load (first frame holds), play, then stop to black
//!   playout-probe counter <seconds>       count down on the NDI source, then clear
//!
//! Uses the default config: DeckLink device 0 in 1080p2997, NDI source "StreamIt Countdown".

use std::path::PathBuf;
use std::time::Duration;

use streamit_core::config::{NdiConfig, PlayoutConfig};
use streamit_core::devices::{Counter, Player, PlayerEvent};
use streamit_playout::{GstPlayer, NdiCounter, SinkKind};

const USAGE: &str =
    "usage:\n  playout-probe play <file> <gain_db>\n  playout-probe counter <seconds>";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["play", file, gain] => match gain.parse::<f32>() {
            Ok(gain) => play(PathBuf::from(file), gain).await,
            Err(_) => Err(format!("bad gain_db {gain:?}")),
        },
        ["counter", secs] => match secs.parse::<u64>() {
            Ok(secs) => counter(secs).await,
            Err(_) => Err(format!("bad seconds {secs:?}")),
        },
        _ => Err(USAGE.to_string()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn play(file: PathBuf, gain_db: f32) -> Result<(), String> {
    let player =
        GstPlayer::new(&PlayoutConfig::default(), SinkKind::DeckLink).map_err(|e| e.to_string())?;
    let duration = player
        .load(&file, gain_db)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "loaded {} ({:.2} s); the first frame should now hold on the DeckLink input",
        file.display(),
        duration.as_secs_f64()
    );
    tokio::time::sleep(Duration::from_secs(5)).await;

    let mut events = player.events();
    player.play().await.map_err(|e| e.to_string())?;
    println!("playing at gain {gain_db} dB");
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let outcome = loop {
        tokio::select! {
            ev = events.recv() => break ev,
            _ = tick.tick() => {
                if let Some(pos) = player.position() {
                    println!("  {:.1} s", pos.as_secs_f64());
                }
            }
        }
    };
    match outcome {
        Ok(PlayerEvent::Ended) => println!("ended"),
        Ok(PlayerEvent::Error(e)) => println!("error: {e}"),
        Err(e) => println!("event channel: {e}"),
    }
    player.stop().await.map_err(|e| e.to_string())?;
    println!("stopped; the DeckLink should now output black for 5 s");
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}

async fn counter(secs: u64) -> Result<(), String> {
    let cfg = NdiConfig::default();
    let counter = NdiCounter::new(&cfg).map_err(|e| e.to_string())?;
    println!(
        "NDI source {:?} is up; counting down from {secs} s",
        cfg.source_name
    );
    for s in (0..=secs).rev() {
        let remaining = Duration::from_secs(s);
        counter.show(remaining).await.map_err(|e| e.to_string())?;
        println!("  {}", streamit_playout::counter_text(remaining));
        // Hold the first value long enough to check it in ProPresenter.
        let hold = if s == secs { 10 } else { 1 };
        tokio::time::sleep(Duration::from_secs(hold)).await;
    }
    counter.clear().await.map_err(|e| e.to_string())?;
    println!("cleared; the source should show plain black for 5 s");
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}
