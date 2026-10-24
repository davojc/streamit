//! Hardware probe for a Blackmagic ATEM (UDP 9910), shared by the `atem-probe` example and
//! `streamit probe atem`.
//!
//!   <ip> state                        print program, preview, media players and Fairlight levels
//!   <ip> preview <input>              put an input on preview (M/E 1)
//!   <ip> program <input>              cut an input to program (M/E 1)
//!   <ip> auto                         run an auto transition and wait for it to finish
//!   <ip> still <player> <index>       load still <index> (0-based media pool slot) into
//!                                     media player <player> (1-based)
//!   <ip> level <input> <source> <db>  set a Fairlight fader, e.g. level 1001 -65280 -10
//!                                     (-100 or -inf is the floor)
use crate::{ATEM_PORT, AtemClient, Snapshot};
use anyhow::{anyhow, bail};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use streamit_core::config::AudioSourceId;
use streamit_core::devices::{StreamSwitcher, Switcher};

pub const USAGE: &str = "usage: atem <ip> state | preview <input> | program <input> | auto \
                         | still <player> <index> | level <input> <source> <db>";

fn arg<T: std::str::FromStr>(args: &[String], i: usize) -> anyhow::Result<T> {
    let a = args.get(i).ok_or_else(|| anyhow!(USAGE))?;
    a.parse().map_err(|_| anyhow!("bad argument {a:?}\n{USAGE}"))
}

/// Runs the probe; `args` are everything after the program (or `probe atem`) name.
pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let ip: IpAddr = arg(args, 0)?;
    let addr = SocketAddr::new(ip, ATEM_PORT);
    let cmd = args.get(1).map(String::as_str).ok_or_else(|| anyhow!(USAGE))?;
    if !matches!(cmd, "state" | "preview" | "program" | "auto" | "still" | "level") {
        bail!(USAGE);
    }

    let atem = AtemClient::connect(addr).await?;
    atem.wait_initialised(Duration::from_secs(5))
        .await
        .map_err(|e| anyhow!("{e} (is {addr} an ATEM, and is ATEM Software Control able to reach it?)"))?;

    match cmd {
        "state" => {}
        "preview" => {
            let input: u16 = arg(args, 2)?;
            atem.set_preview(input).await?;
            println!("preview <- {input} (acked)");
        }
        "program" => {
            let input: u16 = arg(args, 2)?;
            atem.set_program(input).await?;
            println!("program <- {input} (acked)");
        }
        "auto" => {
            atem.auto().await?;
            println!("auto (acked); waiting for the transition to finish");
            atem.wait_transition_done(Duration::from_secs(10)).await?;
            println!("transition done");
        }
        "still" => {
            let player: u8 = arg(args, 2)?;
            let index: u16 = arg(args, 3)?;
            atem.set_media_player_still(player, index).await?;
            println!("media player {player} (input {}) <- still {index} (acked)", atem.media_player_input(player));
        }
        "level" => {
            let src = AudioSourceId { input: arg(args, 2)?, source: arg(args, 3)? };
            let db: f32 = arg(args, 4)?;
            let before = atem.audio_level(src).await?;
            println!("fader {} / {} was {before} dB", src.input, src.source);
            atem.set_audio_level(src, db).await?;
            println!("fader {} / {} <- {db} dB (acked)", src.input, src.source);
        }
        _ => unreachable!(),
    }

    // Let the switcher report the result before printing the state.
    tokio::time::sleep(Duration::from_millis(500)).await;
    print_state(&atem.snapshot());
    Ok(())
}

fn print_state(s: &Snapshot) {
    let st = &s.state;
    println!("--- state ---");
    println!("connected: {}, initialised: {}", s.connected, st.initialised);
    if let Some(v) = st.protocol_version {
        println!("protocol version: {}.{}", v >> 16, v & 0xffff);
    }
    println!("M/E blocks: {:?}, media players: {:?}", st.mix_effects, st.media_player_count);
    println!(
        "program: {:?}  preview: {:?}  in transition: {:?}",
        st.program.get(&0),
        st.preview.get(&0),
        st.in_transition.get(&0)
    );
    for (p, mp) in &st.media_players {
        let what = match mp.source_type {
            1 => format!("still {}", mp.still_index),
            2 => format!("clip {}", mp.clip_index),
            t => format!("source type {t}"),
        };
        println!("media player {} (input {}): {what}", p + 1, 3000 + 10 * (u16::from(*p) + 1));
    }
    if st.fairlight.is_empty() {
        println!("fairlight: none reported (classic audio?)");
    }
    for ((input, source), f) in &st.fairlight {
        let mix = match f.mix_option {
            1 => "off",
            2 => "on",
            4 => "afv",
            _ => "?",
        };
        println!(
            "fairlight {input} / {source}: fader {} dB, mix {mix} (supported mask {:#x})",
            f.fader_gain as f32 / 100.0,
            f.supported_mix_options
        );
    }
}
