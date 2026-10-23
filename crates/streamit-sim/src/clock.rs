//! The simulation speed and the clock that scales pretend delays by it.
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    X1,
    X4,
    X10,
}

impl Speed {
    pub fn factor(self) -> u32 {
        match self {
            Speed::X1 => 1,
            Speed::X4 => 4,
            Speed::X10 => 10,
        }
    }

    fn from_factor(f: u32) -> Self {
        match f {
            4 => Speed::X4,
            10 => Speed::X10,
            _ => Speed::X1,
        }
    }
}

/// Shared by every pretend device. A speed change applies to later sleeps and
/// scalings; a sleep already in progress keeps its duration.
#[derive(Debug, Clone)]
pub struct SimClock {
    factor: Arc<AtomicU32>,
}

impl Default for SimClock {
    fn default() -> Self {
        Self::new(Speed::X1)
    }
}

impl SimClock {
    pub fn new(speed: Speed) -> Self {
        Self { factor: Arc::new(AtomicU32::new(speed.factor())) }
    }

    pub fn speed(&self) -> Speed {
        Speed::from_factor(self.factor.load(Ordering::Relaxed))
    }

    pub fn set_speed(&self, speed: Speed) {
        self.factor.store(speed.factor(), Ordering::Relaxed);
    }

    /// `d` divided by the current speed factor.
    pub fn scale(&self, d: Duration) -> Duration {
        d / self.factor.load(Ordering::Relaxed).max(1)
    }

    pub async fn sleep(&self, d: Duration) {
        tokio::time::sleep(self.scale(d)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_divides_by_factor() {
        let c = SimClock::default();
        assert_eq!(c.scale(Duration::from_secs(1)), Duration::from_secs(1));
        c.set_speed(Speed::X4);
        assert_eq!(c.scale(Duration::from_secs(1)), Duration::from_millis(250));
        c.set_speed(Speed::X10);
        assert_eq!(c.scale(Duration::from_secs(1)), Duration::from_millis(100));
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_uses_speed_at_call_time() {
        let c = SimClock::new(Speed::X10);
        let t = tokio::time::Instant::now();
        c.sleep(Duration::from_secs(1)).await;
        assert_eq!(t.elapsed(), Duration::from_millis(100));
    }
}
