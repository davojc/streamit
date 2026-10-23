//! Hardware check for the playout PC (needs `--features gst`). The logic lives in
//! `streamit_playout::probe` (also reachable as `streamit probe player|counter ...`).
//!
//!   playout-probe play <file> <gain_db>   load (first frame holds), play, then stop to black
//!   playout-probe counter <seconds>       count down on the NDI source, then clear

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = streamit_playout::probe::run(&args).await {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
