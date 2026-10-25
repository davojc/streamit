//! Counts the YouTube Data API quota units StreamIt uses, per quota day.
//!
//! Costs from Google's quota calculator (developers.google.com/youtube/v3/determine_quota_cost):
//! every request costs 1, invalid ones included, except liveChatMessages.list (5) and
//! liveChatMessages.insert (20). A project gets 10,000 units a day by default, and the
//! day resets at midnight Pacific time. The count is kept in a file so a restart keeps it;
//! it only knows what this computer's StreamIt used, not other apps on the same project.
use chrono::{DateTime, Datelike, Duration as Days, NaiveDate, TimeZone, Utc};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tracing::warn;

/// A project's default daily quota.
pub const DAILY_LIMIT: u32 = 10_000;

/// The units one request costs.
pub fn cost(method: &Method, path: &str) -> u32 {
    match (path, method) {
        ("liveChat/messages", &Method::GET) => 5,
        ("liveChat/messages", _) => 20,
        _ => 1,
    }
}

/// What has been used on one quota day.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Used {
    /// The quota day, Pacific time, as YYYY-MM-DD.
    pub day: String,
    pub units: u32,
    pub calls: u32,
}

#[derive(Default)]
pub struct Meter {
    used: Mutex<Used>,
    path: Mutex<Option<PathBuf>>,
}

/// The one meter every YouTube client adds to.
pub fn meter() -> &'static Meter {
    static METER: OnceLock<Meter> = OnceLock::new();
    METER.get_or_init(Meter::default)
}

impl Meter {
    /// Keep the count in `path` from now on, starting from what it holds for today.
    pub fn keep_in(&self, path: &Path) {
        let saved: Used = std::fs::read_to_string(path).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
        *self.path.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.to_path_buf());
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        if saved.day == quota_day(Utc::now()).to_string() && saved.units >= used.units {
            *used = saved;
        }
    }

    /// One request sent.
    pub fn add(&self, units: u32) {
        self.add_at(units, Utc::now());
    }

    fn add_at(&self, units: u32, now: DateTime<Utc>) {
        let copy = {
            let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
            roll(&mut used, now);
            used.units = used.units.saturating_add(units);
            used.calls = used.calls.saturating_add(1);
            used.clone()
        };
        let path = self.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(p) = path {
            let text = toml::to_string(&copy).unwrap_or_default();
            if let Err(e) = std::fs::write(&p, text) {
                warn!(path = %p.display(), "could not save the YouTube quota count: {e}");
            }
        }
    }

    /// Today's count (zero once the quota day has changed).
    pub fn today(&self) -> Used {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        roll(&mut used, Utc::now());
        used.clone()
    }
}

fn roll(used: &mut Used, now: DateTime<Utc>) {
    let day = quota_day(now).to_string();
    if used.day != day {
        *used = Used { day, units: 0, calls: 0 };
    }
}

/// The US daylight-saving rule: from 2:00 local on the second Sunday in March until 2:00
/// local on the first Sunday in November.
fn pacific_offset_hours(now: DateTime<Utc>) -> i64 {
    let year = now.year();
    let nth_sunday = |month: u32, n: u32| {
        let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid date");
        let to_sunday = (7 - first.weekday().num_days_from_sunday()) % 7;
        first + Days::days(i64::from(to_sunday + 7 * (n - 1)))
    };
    // 2:00 PST is 10:00 UTC; 2:00 PDT is 9:00 UTC.
    let starts = Utc.from_utc_datetime(&nth_sunday(3, 2).and_hms_opt(10, 0, 0).expect("valid time"));
    let ends = Utc.from_utc_datetime(&nth_sunday(11, 1).and_hms_opt(9, 0, 0).expect("valid time"));
    if now >= starts && now < ends { -7 } else { -8 }
}

/// The quota day `now` falls on: the date in Pacific time.
pub fn quota_day(now: DateTime<Utc>) -> NaiveDate {
    (now + Days::hours(pacific_offset_hours(now))).date_naive()
}

/// How long until the quota resets (the next midnight Pacific).
pub fn until_reset(now: DateTime<Utc>) -> std::time::Duration {
    let offset = pacific_offset_hours(now);
    let midnight = (quota_day(now) + Days::days(1)).and_hms_opt(0, 0, 0).expect("valid time");
    let reset = Utc.from_utc_datetime(&midnight) - Days::hours(offset);
    (reset - now).to_std().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn chat_costs_more_than_the_rest() {
        assert_eq!(cost(&Method::GET, "liveChat/messages"), 5);
        assert_eq!(cost(&Method::POST, "liveChat/messages"), 20);
        assert_eq!(cost(&Method::POST, "liveBroadcasts/transition"), 1);
        assert_eq!(cost(&Method::GET, "liveStreams"), 1);
        assert_eq!(cost(&Method::PUT, "liveBroadcasts"), 1);
    }

    #[test]
    fn the_quota_day_is_pacific() {
        // Summer (PDT, UTC-7): 06:59 UTC is still the day before.
        assert_eq!(quota_day(at("2026-07-10T06:59:00Z")).to_string(), "2026-07-09");
        assert_eq!(quota_day(at("2026-07-10T07:00:00Z")).to_string(), "2026-07-10");
        // Winter (PST, UTC-8).
        assert_eq!(quota_day(at("2026-01-10T07:59:00Z")).to_string(), "2026-01-09");
        assert_eq!(quota_day(at("2026-01-10T08:00:00Z")).to_string(), "2026-01-10");
        // 2026: DST from 8 March to 1 November.
        assert_eq!(pacific_offset_hours(at("2026-03-08T09:59:00Z")), -8);
        assert_eq!(pacific_offset_hours(at("2026-03-08T10:00:00Z")), -7);
        assert_eq!(pacific_offset_hours(at("2026-11-01T08:59:00Z")), -7);
        assert_eq!(pacific_offset_hours(at("2026-11-01T09:00:00Z")), -8);
    }

    #[test]
    fn the_reset_is_the_next_midnight_pacific() {
        assert_eq!(until_reset(at("2026-07-10T06:00:00Z")), std::time::Duration::from_secs(3600));
        assert_eq!(until_reset(at("2026-01-10T20:00:00Z")), std::time::Duration::from_secs(12 * 3600));
    }

    #[test]
    fn the_count_starts_again_each_day_and_is_kept_in_a_file() {
        let dir = std::env::temp_dir().join(format!("streamit-quota-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quota.toml");
        let m = Meter::default();
        m.keep_in(&path);
        let day1 = at("2026-07-10T12:00:00Z");
        m.add_at(1, day1);
        m.add_at(5, day1);
        let used = m.used.lock().unwrap().clone();
        assert_eq!((used.day.as_str(), used.units, used.calls), ("2026-07-10", 6, 2));
        let saved: Used = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved, used);
        m.add_at(20, at("2026-07-11T08:00:00Z"));
        let used = m.used.lock().unwrap().clone();
        assert_eq!((used.day.as_str(), used.units, used.calls), ("2026-07-11", 20, 1));
        std::fs::remove_dir_all(&dir).ok();
    }
}
