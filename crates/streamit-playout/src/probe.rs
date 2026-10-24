//! Hardware check for the playout PC (needs `--features gst`), shared by the
//! `playout-probe` example and `streamit probe player|counter`.
//!
//!   play <file> <gain_db>   load (first frame holds), play, then stop to black
//!   counter <seconds>       count down on the NDI source, then clear
//!
//! Uses the default config: DeckLink device 0 in 1080p2997, NDI source "StreamIt Countdown".
use crate::{GstPlayer, NdiCounter, SinkKind};
use anyhow::{anyhow, bail};
use std::path::PathBuf;
use std::time::Duration;
use streamit_core::config::{NdiConfig, PlayoutConfig};
use streamit_core::devices::{Counter, Player, PlayerEvent};

pub const USAGE: &str = "usage:\n  play <file> <gain_db>\n  counter <seconds>";

/// Runs the probe; `args` are everything after the program name.
pub async fn run(args: &[String]) -> anyhow::Result<()> {
    crate::loader::init().map_err(|e| anyhow!(e))?;
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["play", file, gain] => {
            let gain = gain.parse::<f32>().map_err(|_| anyhow!("bad gain_db {gain:?}"))?;
            play(PathBuf::from(file), gain).await
        }
        ["counter", secs] => {
            let secs = secs.parse::<u64>().map_err(|_| anyhow!("bad seconds {secs:?}"))?;
            counter(secs).await
        }
        _ => bail!(USAGE),
    }
}

async fn play(file: PathBuf, gain_db: f32) -> anyhow::Result<()> {
    let player = GstPlayer::new(&PlayoutConfig::default(), SinkKind::DeckLink)?;
    let duration = player.load(&file, gain_db).await?;
    println!(
        "loaded {} ({:.2} s); the first frame should now hold on the DeckLink input",
        file.display(),
        duration.as_secs_f64()
    );
    tokio::time::sleep(Duration::from_secs(5)).await;

    let mut events = player.events();
    player.play().await?;
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
    player.stop().await?;
    println!("stopped; the DeckLink should now output black for 5 s");
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}

async fn counter(secs: u64) -> anyhow::Result<()> {
    let cfg = NdiConfig::default();
    let counter = NdiCounter::new(&cfg)?;
    println!("NDI source {:?} is up; counting down from {secs} s", cfg.source_name);
    for s in (0..=secs).rev() {
        let remaining = Duration::from_secs(s);
        counter.show(remaining).await?;
        println!("  {}", crate::counter_text(remaining));
        // Hold the first value long enough to check it in ProPresenter.
        let hold = if s == secs { 10 } else { 1 };
        tokio::time::sleep(Duration::from_secs(hold)).await;
    }
    counter.clear().await?;
    println!("cleared; the source should show plain black for 5 s");
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}
