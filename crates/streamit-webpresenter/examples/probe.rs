//! Hardware probe for a Blackmagic Web Presenter 4K. The logic lives in
//! `streamit_webpresenter::probe` (also reachable as `streamit probe webpresenter ...`).
//!
//!   webpresenter-probe <ip> dump|status|start|stop

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = streamit_webpresenter::probe::run(&args).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
