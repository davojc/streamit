//! Pretend YouTube: one seeded broadcast, a stream health that turns Good five seconds
//! after the encoder streams, and transitions that take 2.5 s.
use crate::{Fault, SimHandle, SimWorld, YtPending, phase_name};
use async_trait::async_trait;
use std::time::Duration;
use streamit_core::devices::*;
use streamit_core::engine::DeviceKind;
use tokio::time::Instant;

/// Every pretend transition takes this long at 1x.
const TRANSITION: Duration = Duration::from_millis(2500);
/// The pretend broadcast's live chat.
pub const SIM_CHAT: &str = "sim-chat";
/// A new pretend viewer message arrives this often (1x) once the stream is healthy.
const CHAT_GAP: Duration = Duration::from_secs(15);
/// What the pretend viewers say, in order.
const CHAT_SCRIPT: [(&str, &str); 10] = [
    ("Margaret H", "Good morning from Leeds!"),
    ("The Okafor family", "Morning all, watching from home with the kids"),
    ("Dave P", "Sound is lovely today"),
    ("Ruth", "Praying for the Kenya team"),
    ("Sam T", "Is the video on? I can only hear music"),
    ("Joan W", "Thank you for streaming, can't make it in this week"),
    ("Margaret H", "Lovely hymn"),
    ("Peter", "Hello from Auckland, it's late here!"),
    ("Dave P", "Amen"),
    ("Ruth", "Thank you everyone, see you next week"),
];
/// With the slow fault on, every call takes this long at 1x.
const SLOW: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub struct SimBroadcasts {
    sim: SimHandle,
}

impl SimBroadcasts {
    pub fn new(sim: SimHandle) -> Self {
        Self { sim }
    }

    /// Start of every call: the slow fault's delay, then the unplugged check.
    async fn gate(&self) -> Result<()> {
        let slow = self.sim.lock().has_fault(Fault::YouTubeSlow);
        if slow {
            self.sim.clock.sleep(SLOW).await;
        }
        self.sim.lock().check_plugged(DeviceKind::YouTube)
    }

    fn with<T>(&self, f: impl FnOnce(&mut SimWorld) -> Result<T>) -> Result<T> {
        f(&mut self.sim.lock())
    }
}

/// Now as RFC 3339 (UTC), the way YouTube stamps chat messages.
fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

fn invalid(from: &str, to: &str, why: &str) -> DeviceError {
    DeviceError::Refused(format!("invalid transition {from} -> {to}: {why} (simulation)"))
}

fn unknown(id: &str) -> DeviceError {
    DeviceError::Other(format!("no broadcast {id} (simulation)"))
}

#[async_trait]
impl Broadcasts for SimBroadcasts {
    async fn signed_in(&self) -> Result<bool> {
        self.gate().await?;
        Ok(true)
    }

    async fn upcoming(&self) -> Result<Vec<BroadcastSummary>> {
        self.gate().await?;
        self.with(|w| Ok(w.broadcasts.iter().filter(|b| b.life_cycle != "complete").cloned().collect()))
    }

    async fn check(&self, id: &str) -> Result<BroadcastCheck> {
        self.gate().await?;
        self.with(|w| if w.broadcasts.iter().any(|b| b.id == id) { Ok(w.yt_check) } else { Err(unknown(id)) })
    }

    async fn fix(&self, id: &str, fix: Fix) -> Result<()> {
        self.gate().await?;
        self.with(|w| {
            let b = w.broadcasts.iter_mut().find(|b| b.id == id).ok_or_else(|| unknown(id))?;
            match fix {
                Fix::Bind => {
                    b.bound_stream_id = Some(crate::SEED_STREAM.into());
                    w.yt_check.bound_to_ours = true;
                }
                Fix::AutoStartOff => w.yt_check.auto_start = false,
                Fix::AutoStopOn => w.yt_check.auto_stop = true,
                Fix::MonitorOn => w.yt_check.monitor_stream = true,
            }
            Ok(())
        })
    }

    async fn health(&self) -> Result<Health> {
        self.gate().await?;
        self.with(|w| Ok(w.health()))
    }

    /// Viewers grow while the stream is healthy; the chat is there from the start.
    async fn live_details(&self, id: &str) -> Result<LiveDetails> {
        self.gate().await?;
        let gap = self.sim.clock.scale(CHAT_GAP);
        self.with(|w| {
            if !w.broadcasts.iter().any(|b| b.id == id) {
                return Err(unknown(id));
            }
            let viewers = w.health_at.filter(|_| w.health() == Health::Good).map(|t| {
                let gaps = Instant::now().saturating_duration_since(t).as_secs_f32() / gap.as_secs_f32();
                (4.0 + gaps * 3.0).min(64.0) as u64
            });
            Ok(LiveDetails { viewers, chat_id: Some(SIM_CHAT.into()) })
        })
    }

    /// One scripted message every 15 s (scaled) once the stream is healthy, plus the
    /// channel's own replies. Pages are indexes into the chat.
    async fn chat_messages(&self, chat_id: &str, page: Option<&str>) -> Result<ChatPage> {
        self.gate().await?;
        let gap = self.sim.clock.scale(CHAT_GAP);
        self.with(|w| {
            if chat_id != SIM_CHAT {
                return Err(DeviceError::Refused("liveChatNotFound (simulation)".into()));
            }
            if let Some(t) = w.health_at.filter(|_| w.health() == Health::Good) {
                let due = (Instant::now().saturating_duration_since(t).as_secs_f32() / gap.as_secs_f32()) as usize + 1;
                while w.chat_script_sent < due.min(CHAT_SCRIPT.len()) {
                    let (author, text) = CHAT_SCRIPT[w.chat_script_sent];
                    w.chat_script_sent += 1;
                    w.chat.push(ChatMessage {
                        id: format!("sim-{}", w.chat.len()),
                        author: author.into(),
                        role: ChatRole::Viewer,
                        text: text.into(),
                        published: rfc3339_now(),
                    });
                }
            }
            let from = page.and_then(|p| p.parse::<usize>().ok()).unwrap_or(0).min(w.chat.len());
            Ok(ChatPage {
                messages: w.chat[from..].to_vec(),
                next_page: Some(w.chat.len().to_string()),
                wait: Duration::from_secs(5),
            })
        })
    }

    async fn send_chat(&self, chat_id: &str, text: &str) -> Result<()> {
        self.gate().await?;
        self.with(|w| {
            if chat_id != SIM_CHAT {
                return Err(DeviceError::Refused("liveChatNotFound (simulation)".into()));
            }
            w.chat.push(ChatMessage {
                id: format!("sim-{}", w.chat.len()),
                author: "Church (simulation)".into(),
                role: ChatRole::Owner,
                text: text.into(),
                published: rfc3339_now(),
            });
            Ok(())
        })
    }

    async fn life_cycle(&self, id: &str) -> Result<String> {
        self.gate().await?;
        self.with(|w| w.broadcasts.iter().find(|b| b.id == id).map(|b| b.life_cycle.clone()).ok_or_else(|| unknown(id)))
    }

    /// The change is booked at once with a deadline, so a caller that gives up waiting
    /// still sees it land, as with the real service.
    async fn transition(&self, id: &str, to: Phase) -> Result<()> {
        self.gate().await?;
        let plan = self.with(|w| {
            let lc = w.broadcasts.iter().find(|b| b.id == id).ok_or_else(|| unknown(id))?.life_cycle.clone();
            let target = phase_name(to);
            if let Some(p) = &w.yt_pending {
                // One change at a time: the same target is waited for, any other refused.
                return if p.id == id && p.to == to {
                    Ok(Some(p.done_at))
                } else {
                    Err(invalid(&lc, target, "another change is still in progress"))
                };
            }
            if lc == target {
                return Ok(None); // already there: the real client does nothing
            }
            let allowed = matches!(
                (lc.as_str(), to),
                ("ready" | "created", Phase::Testing) | ("testing", Phase::Live) | ("live", Phase::Complete)
            );
            if !allowed {
                return Err(invalid(&lc, target, "not allowed from this state"));
            }
            if to == Phase::Live && w.has_fault(Fault::YouTubeRefusesPublic) {
                return Err(DeviceError::Refused("YouTube refused to go public (simulation)".into()));
            }
            let requested = Instant::now();
            let done_at = requested + self.sim.clock.scale(TRANSITION);
            w.yt_pending = Some(YtPending { id: id.into(), to, requested, done_at });
            Ok(Some(done_at))
        })?;
        let Some(done_at) = plan else { return Ok(()) };
        tokio::time::sleep_until(done_at).await;
        drop(self.sim.lock()); // applies the transition
        Ok(())
    }
}
