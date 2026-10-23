//! Live check for the YouTube client. Read-only on purpose: there is no transition
//! subcommand, so this cannot change a live broadcast.
//!
//!   youtube-probe signin <client_id> <client_secret>
//!   youtube-probe upcoming <client_id> <client_secret>
//!   youtube-probe check <client_id> <client_secret> <broadcast_id> [stream_id]
//!   youtube-probe health <client_id> <client_secret> <stream_id>
use streamit_core::devices::Broadcasts;
use streamit_youtube::{API_BASE, Auth, YouTubeClient};
use url::Url;

fn usage() -> ! {
    eprintln!(
        "usage:\n  youtube-probe signin <client_id> <client_secret>\n  \
         youtube-probe upcoming <client_id> <client_secret>\n  \
         youtube-probe check <client_id> <client_secret> <broadcast_id> [stream_id]\n  \
         youtube-probe health <client_id> <client_secret> <stream_id>"
    );
    std::process::exit(2);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        usage();
    }
    let (cmd, id, secret) = (args[0].as_str(), &args[1], &args[2]);
    let rest = &args[3..];
    let client = |stream: Option<String>| -> YouTubeClient {
        let auth = Auth::load(id, secret).expect("auth");
        YouTubeClient::new(Url::parse(API_BASE).unwrap(), auth, stream)
    };
    let result: Result<(), String> = async {
        match cmd {
            "signin" => {
                Auth::sign_in(id, secret).await.map_err(|e| e.to_string())?;
                println!("signed in; refresh token saved to Credential Manager (StreamIt / youtube)");
            }
            "upcoming" => {
                let c = client(None);
                println!("signed in: {}", c.signed_in().await.map_err(|e| e.to_string())?);
                for b in c.upcoming().await.map_err(|e| e.to_string())? {
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
            "check" => {
                let bid = rest.first().unwrap_or_else(|| usage());
                let c = client(rest.get(1).cloned());
                println!("{:?}", c.check(bid).await.map_err(|e| e.to_string())?);
                println!("life cycle: {}", c.life_cycle(bid).await.map_err(|e| e.to_string())?);
            }
            "health" => {
                let sid = rest.first().unwrap_or_else(|| usage());
                let c = client(Some(sid.clone()));
                println!("{:?}", c.health().await.map_err(|e| e.to_string())?);
            }
            _ => usage(),
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
