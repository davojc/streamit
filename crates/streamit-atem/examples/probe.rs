//! Hardware probe for a Blackmagic ATEM (UDP 9910).
//!
//! Run it as `cargo run -p streamit-atem --example atem-probe -- <ip> <command>`:
//!
//!   state                        print program, preview, media players and Fairlight levels
//!   preview <input>              put an input on preview (M/E 1)
//!   program <input>              cut an input to program (M/E 1)
//!   auto                         run an auto transition and wait for it to finish
//!   still <player> <index>       load still <index> (0-based media pool slot) into
//!                                media player <player> (1-based)
//!   level <input> <source> <db>  set a Fairlight fader, e.g. level 1001 -65280 -10
//!                                (-100 or -inf is the floor)
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use streamit_atem::{ATEM_PORT, AtemClient, Snapshot};
use streamit_core::config::AudioSourceId;
use streamit_core::devices::{StreamSwitcher, Switcher};

const USAGE: &str = "usage: atem-probe <ip> state | preview <input> | program <input> | auto \
                     | still <player> <index> | level <input> <source> <db>";

#[tokio::main]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn arg<T: std::str::FromStr>(args: &[String], i: usize) -> Result<T, String> {
    args.get(i)
        .ok_or(USAGE)?
        .parse()
        .map_err(|_| format!("bad argument {:?}\n{USAGE}", args[i]))
}

async fn real_main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ip: IpAddr = arg(&args, 0)?;
    let addr = SocketAddr::new(ip, ATEM_PORT);
    let cmd = args.get(1).map(String::as_str).ok_or(USAGE)?;
    if !matches!(
        cmd,
        "state" | "preview" | "program" | "auto" | "still" | "level"
    ) {
        return Err(USAGE.into());
    }

    let atem = AtemClient::connect(addr).await.map_err(|e| e.to_string())?;
    atem.wait_initialised(Duration::from_secs(5))
        .await
        .map_err(|e| {
            format!("{e} (is {addr} an ATEM, and is ATEM Software Control able to reach it?)")
        })?;

    match cmd {
        "state" => {}
        "preview" => {
            let input: u16 = arg(&args, 2)?;
            atem.set_preview(input).await.map_err(|e| e.to_string())?;
            println!("preview <- {input} (acked)");
        }
        "program" => {
            let input: u16 = arg(&args, 2)?;
            atem.set_program(input).await.map_err(|e| e.to_string())?;
            println!("program <- {input} (acked)");
        }
        "auto" => {
            atem.auto().await.map_err(|e| e.to_string())?;
            println!("auto (acked); waiting for the transition to finish");
            atem.wait_transition_done(Duration::from_secs(10))
                .await
                .map_err(|e| e.to_string())?;
            println!("transition done");
        }
        "still" => {
            let player: u8 = arg(&args, 2)?;
            let index: u16 = arg(&args, 3)?;
            atem.set_media_player_still(player, index)
                .await
                .map_err(|e| e.to_string())?;
            println!(
                "media player {player} (input {}) <- still {index} (acked)",
                atem.media_player_input(player)
            );
        }
        "level" => {
            let src = AudioSourceId {
                input: arg(&args, 2)?,
                source: arg(&args, 3)?,
            };
            let db: f32 = arg(&args, 4)?;
            let before = atem.audio_level(src).await.map_err(|e| e.to_string())?;
            println!("fader {} / {} was {before} dB", src.input, src.source);
            atem.set_audio_level(src, db)
                .await
                .map_err(|e| e.to_string())?;
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
    println!(
        "connected: {}, initialised: {}",
        s.connected, st.initialised
    );
    if let Some(v) = st.protocol_version {
        println!("protocol version: {}.{}", v >> 16, v & 0xffff);
    }
    println!(
        "M/E blocks: {:?}, media players: {:?}",
        st.mix_effects, st.media_player_count
    );
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
        println!(
            "media player {} (input {}): {what}",
            p + 1,
            3000 + 10 * (u16::from(*p) + 1)
        );
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
