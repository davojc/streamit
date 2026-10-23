//! Live check for the YouTube client. The logic lives in `streamit_youtube::probe` (also
//! reachable as `streamit probe youtube ...`). Read-only: it cannot change a broadcast.
//!
//!   youtube-probe signin|upcoming|streams <client_id> <client_secret>
//!   youtube-probe check <client_id> <client_secret> <broadcast_id> [stream_id]
//!   youtube-probe health <client_id> <client_secret> <stream_id>

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = streamit_youtube::probe::run(&args).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
