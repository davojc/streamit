//! `streamit probe <device> ...`: forwards to each crate's probe module.
use crate::discovery;
use anyhow::bail;
use std::process::ExitCode;
use std::time::Duration;

pub const USAGE: &str = "usage: streamit probe <device> ...
  discover [seconds]                         list Blackmagic devices on the network (mDNS) with their TXT records
  videohub <ip> dump | route <out> <in>
  webpresenter <ip> dump|status|start|stop
  atem <ip> state | preview <in> | program <in> | auto | still <player> <index> | level <in> <source> <db>
  youtube signin|upcoming|streams <client_id> <client_secret>
  youtube check <client_id> <client_secret> <broadcast_id> [stream_id]
  youtube health <client_id> <client_secret> <stream_id>
  player <file> <gain_db>                    (needs the gst build)
  counter <seconds>                          (needs the gst build)
  deck                                       (close Companion first)";

/// Runs one probe on its own multi-thread runtime (the deck needs one).
pub fn main(args: &[String]) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: &[String]) -> anyhow::Result<()> {
    let Some((device, rest)) = args.split_first() else { bail!(USAGE) };
    match device.as_str() {
        "discover" => discover(rest).await,
        "videohub" => streamit_videohub::probe::run(rest).await,
        "webpresenter" => streamit_webpresenter::probe::run(rest).await,
        "atem" => streamit_atem::probe::run(rest).await,
        "youtube" => streamit_youtube::probe::run(rest).await,
        "deck" => streamit_deck::probe::run(rest).await,
        "player" => playout("play", rest).await,
        "counter" => playout("counter", rest).await,
        _ => bail!(USAGE),
    }
}

#[cfg(feature = "gst")]
async fn playout(sub: &str, rest: &[String]) -> anyhow::Result<()> {
    let mut args = vec![sub.to_string()];
    args.extend_from_slice(rest);
    streamit_playout::probe::run(&args).await
}

#[cfg(not(feature = "gst"))]
async fn playout(sub: &str, _rest: &[String]) -> anyhow::Result<()> {
    let what = if sub == "play" { "player" } else { "counter" };
    bail!(
        "the {what} probe needs GStreamer: on the playout PC build with \
         `cargo build -p streamit-app --release --features gst` (see README)"
    )
}

async fn discover(rest: &[String]) -> anyhow::Result<()> {
    let secs = match rest.first() {
        Some(s) => s.parse::<u64>().map_err(|_| anyhow::anyhow!("bad seconds {s:?}"))?,
        None => 5,
    };
    println!("browsing {} for {secs} s ...", discovery::SERVICE_TYPE);
    tokio::task::spawn_blocking(move || {
        discovery::browse_with(Duration::from_secs(secs), |s| {
            let mut ips: Vec<_> = s.get_addresses_v4().into_iter().collect();
            ips.sort();
            println!("{}  host={}  port={}  ipv4={ips:?}", s.get_fullname(), s.get_hostname(), s.get_port());
            let txt = s.get_properties();
            for p in txt.iter() {
                println!("    {}={}", p.key(), p.val_str());
            }
            println!("    -> kind {:?}", discovery::kind_from_txt(txt));
        })
    })
    .await?;
    Ok(())
}
