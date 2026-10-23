//! Hardware probe for a Blackmagic VideoHub. The logic lives in `streamit_videohub::probe`
//! (also reachable as `streamit probe videohub ...`).
//!
//!   videohub-probe <ip> dump                 print the raw dump, then the routes (1-based)
//!   videohub-probe <ip> route <out1> <in1>   route an output to an input (1-based)

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = streamit_videohub::probe::run(&args).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
