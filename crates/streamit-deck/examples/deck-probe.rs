//! Draws a demo snapshot on the first Stream Deck XL and prints every command a key
//! press would send. Close Companion first: it holds the deck open. The logic lives in
//! `streamit_deck::probe` (also reachable as `streamit probe deck`).
//!
//!     cargo run -p streamit-deck --example deck-probe

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = streamit_deck::probe::run(&args).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
