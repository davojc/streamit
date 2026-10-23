//! Live check for the YouTube client, shared by the `youtube-probe` example and
//! `streamit probe youtube`. Read-only on purpose: there is no transition subcommand, so
//! this cannot change a live broadcast.
//!
//!   signin <client_id> <client_secret>
//!   upcoming <client_id> <client_secret>
//!   streams <client_id> <client_secret>
//!   check <client_id> <client_secret> <broadcast_id> [stream_id]
//!   health <client_id> <client_secret> <stream_id>
use crate::{API_BASE, Auth, YouTubeClient};
use anyhow::{anyhow, bail};
use streamit_core::devices::Broadcasts;
use url::Url;

pub const USAGE: &str = "usage:\n  youtube signin <client_id> <client_secret>\n  \
     youtube upcoming <client_id> <client_secret>\n  \
     youtube streams <client_id> <client_secret>\n  \
     youtube check <client_id> <client_secret> <broadcast_id> [stream_id]\n  \
     youtube health <client_id> <client_secret> <stream_id>";

/// Runs the probe; `args` are everything after the program (or `probe youtube`) name.
pub async fn run(args: &[String]) -> anyhow::Result<()> {
    if args.len() < 3 {
        bail!(USAGE);
    }
    let (cmd, id, secret) = (args[0].as_str(), &args[1], &args[2]);
    let rest = &args[3..];
    let client = |stream: Option<String>| -> anyhow::Result<YouTubeClient> {
        let auth = Auth::load(id, secret)?;
        Ok(YouTubeClient::new(Url::parse(API_BASE)?, auth, stream))
    };
    match cmd {
        "signin" => {
            Auth::sign_in(id, secret).await?;
            println!("signed in; refresh token saved to Credential Manager (StreamIt / youtube)");
        }
        "upcoming" => {
            let c = client(None)?;
            println!("signed in: {}", c.signed_in().await?);
            for b in c.upcoming().await? {
                println!(
                    "{}  {}  [{}]  start={}  stream={}",
                    b.id,
                    b.title,
                    b.life_cycle,
                    b.scheduled_start.as_deref().unwrap_or("-"),
                    b.bound_stream_id.as_deref().unwrap_or("-")
                );
            }
        }
        "streams" => {
            for s in client(None)?.list_streams().await? {
                println!("{}  {}  key {}", s.id, s.title, s.key_name);
            }
        }
        "check" => {
            let bid = rest.first().ok_or_else(|| anyhow!(USAGE))?;
            let c = client(rest.get(1).cloned())?;
            println!("{:?}", c.check(bid).await?);
            println!("life cycle: {}", c.life_cycle(bid).await?);
        }
        "health" => {
            let sid = rest.first().ok_or_else(|| anyhow!(USAGE))?;
            println!("{:?}", client(Some(sid.clone()))?.health().await?);
        }
        _ => bail!(USAGE),
    }
    Ok(())
}
