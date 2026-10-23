//! Hardware probe for a Blackmagic ATEM (UDP 9910). The logic lives in
//! `streamit_atem::probe` (also reachable as `streamit probe atem ...`).
//!
//! Run it as `cargo run -p streamit-atem --example atem-probe -- <ip> <command>`:
//!
//!   state | preview <input> | program <input> | auto | still <player> <index>
//!   | level <input> <source> <db>

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = streamit_atem::probe::run(&args).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
