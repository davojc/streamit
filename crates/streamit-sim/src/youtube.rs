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
