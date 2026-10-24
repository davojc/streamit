//! YouTube Data API v3 client implementing `Broadcasts`, plus OAuth sign-in.
mod auth;
pub mod probe;

pub use auth::{Auth, KeyringStore, MemoryStore, TokenStore};

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;
use streamit_core::devices::{BroadcastCheck, BroadcastSummary, Broadcasts, DeviceError, Fix, Health, Phase, Result};
use url::Url;

pub const API_BASE: &str = "https://www.googleapis.com/youtube/v3";

/// One of the channel's live streams (a stream key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSummary {
    pub id: String,
    /// The name YouTube Studio shows for the key.
    pub title: String,
    /// The last four characters of the stream key ("…wxyz"), for telling keys apart.
    /// The key itself lets anyone stream to the channel, so it is never kept or shown.
    pub key_name: String,
}

/// "…" plus the last four characters of `key`, or "" when there is no key.
fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
    format!("…{tail}")
}

pub struct YouTubeClient {
    base: Url,
    auth: Auth,
    stream_id: Option<String>,
    http: reqwest::Client,
}

impl YouTubeClient {
    pub fn new(base: Url, auth: Auth, stream_id: Option<String>) -> Self {
        let mut base = base;
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().expect("http client");
        Self { base, auth, stream_id, http }
    }

    /// One API call. A 401 refreshes the access token and retries once.
    async fn call(&self, method: Method, path: &str, query: &[(&str, &str)], body: Option<&Value>) -> Result<Value> {
        for attempt in 0..2 {
            let token = self.auth.access_token().await?;
            let mut url = self.base.join(path).map_err(|e| DeviceError::Other(e.to_string()))?;
            url.query_pairs_mut().extend_pairs(query);
            let mut req = self.http.request(method.clone(), url).bearer_auth(token);
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await.map_err(auth::net_err)?;
            let status = resp.status();
            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.auth.invalidate().await;
                continue;
            }
            let text = resp.text().await.map_err(auth::net_err)?;
            let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if status.is_success() {
                if json.is_null() && !text.trim().is_empty() {
                    return Err(DeviceError::Other("YouTube returned unreadable data".into()));
                }
                return Ok(json);
            }
            return Err(map_error(status, &json));
        }
        Err(DeviceError::Refused("sign-in required".into()))
    }

    /// The channel's live streams (stream keys), for the Settings stream picker.
    /// Not part of `Broadcasts`: only the window uses it.
    pub async fn list_streams(&self) -> Result<Vec<StreamSummary>> {
        let v = self
            .call(
                Method::GET,
                "liveStreams",
                &[("part", "id,snippet,cdn"), ("mine", "true"), ("maxResults", "50")],
                None,
            )
            .await?;
        Ok(v["items"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|s| {
                Some(StreamSummary {
                    id: s["id"].as_str()?.to_string(),
                    title: s["snippet"]["title"].as_str().unwrap_or("").to_string(),
                    key_name: mask_key(s["cdn"]["ingestionInfo"]["streamName"].as_str().unwrap_or("")),
                })
            })
            .collect())
    }

    /// The title of the channel the sign-in belongs to (a brand account's channel when that
    /// was picked at sign-in), or `None` when the account has no channel.
    pub async fn channel_title(&self) -> Result<Option<String>> {
        let v = self.call(Method::GET, "channels", &[("part", "snippet"), ("mine", "true")], None).await?;
        Ok(v["items"][0]["snippet"]["title"].as_str().map(String::from))
    }

    /// One `liveBroadcasts.list` page filtered by `broadcastStatus` (`upcoming` or `active`).
    async fn list_broadcasts(&self, status: &str) -> Result<Vec<BroadcastSummary>> {
        let v = self
            .call(
                Method::GET,
                "liveBroadcasts",
                &[("part", "id,snippet,contentDetails,status"), ("broadcastStatus", status), ("maxResults", "25")],
                None,
            )
            .await?;
        Ok(v["items"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|b| {
                Some(BroadcastSummary {
                    id: b["id"].as_str()?.to_string(),
                    title: b["snippet"]["title"].as_str().unwrap_or("").to_string(),
                    scheduled_start: b["snippet"]["scheduledStartTime"].as_str().map(String::from),
                    life_cycle: b["status"]["lifeCycleStatus"].as_str().unwrap_or("").to_string(),
                    bound_stream_id: b["contentDetails"]["boundStreamId"].as_str().map(String::from),
                })
            })
            .collect())
    }

    async fn broadcast(&self, id: &str, part: &str) -> Result<Value> {
        let v = self.call(Method::GET, "liveBroadcasts", &[("part", part), ("id", id)], None).await?;
        v["items"].get(0).cloned().ok_or_else(|| DeviceError::Refused("broadcast not found".into()))
    }
}

fn map_error(status: StatusCode, body: &Value) -> DeviceError {
    let reason = body["error"]["errors"][0]["reason"].as_str().or(body["error"]["message"].as_str()).map(String::from);
    match status.as_u16() {
        404 => DeviceError::Refused("broadcast not found".into()),
        401 | 403 => DeviceError::Refused(reason.unwrap_or_else(|| format!("HTTP {status}"))),
        500..=599 => DeviceError::Other(format!("YouTube returned {status}")),
        _ => match reason {
            Some(r) => DeviceError::Refused(r),
            None => DeviceError::Other(format!("YouTube returned {status}")),
        },
    }
}

fn flag(v: &Value) -> bool {
    v.as_bool().unwrap_or(false)
}

#[async_trait]
impl Broadcasts for YouTubeClient {
    async fn signed_in(&self) -> Result<bool> {
        self.auth.signed_in().await
    }

    /// R23: the channel's upcoming and active (live) broadcasts, merged and deduped by
    /// id, so the broadcast that is on air stays in the list. Fails if either list fails.
    async fn upcoming(&self) -> Result<Vec<BroadcastSummary>> {
        let mut list = self.list_broadcasts("active").await?;
        for b in self.list_broadcasts("upcoming").await? {
            if !list.iter().any(|x| x.id == b.id) {
                list.push(b);
            }
        }
        // RFC 3339 UTC strings sort chronologically; unscheduled broadcasts go last.
        list.sort_by(|a, b| {
            a.scheduled_start
                .is_none()
                .cmp(&b.scheduled_start.is_none())
                .then_with(|| a.scheduled_start.cmp(&b.scheduled_start))
        });
        Ok(list)
    }

    async fn check(&self, id: &str) -> Result<BroadcastCheck> {
        let b = self.broadcast(id, "id,contentDetails").await?;
        let cd = &b["contentDetails"];
        let bound = cd["boundStreamId"].as_str();
        Ok(BroadcastCheck {
            bound_to_ours: bound.is_some() && bound == self.stream_id.as_deref(),
            auto_start: flag(&cd["enableAutoStart"]),
            auto_stop: flag(&cd["enableAutoStop"]),
            monitor_stream: flag(&cd["monitorStream"]["enableMonitorStream"]),
        })
    }

    async fn fix(&self, id: &str, fix: Fix) -> Result<()> {
        if fix == Fix::Bind {
            let stream = self.stream_id.as_deref().ok_or_else(|| DeviceError::Refused("no stream selected".into()))?;
            self.call(
                Method::POST,
                "liveBroadcasts/bind",
                &[("id", id), ("streamId", stream), ("part", "id,contentDetails")],
                None,
            )
            .await?;
            return Ok(());
        }
        // The API resets contentDetails fields that are left out, so send all of them back.
        let b = self.broadcast(id, "id,contentDetails").await?;
        let mut cd = match b["contentDetails"].clone() {
            Value::Object(m) => Value::Object(m),
            _ => json!({}),
        };
        match fix {
            Fix::AutoStartOff => cd["enableAutoStart"] = json!(false),
            Fix::AutoStopOn => cd["enableAutoStop"] = json!(true),
            Fix::MonitorOn => {
                if !cd["monitorStream"].is_object() {
                    cd["monitorStream"] = json!({});
                }
                cd["monitorStream"]["enableMonitorStream"] = json!(true);
            }
            Fix::Bind => unreachable!(),
        }
        self.call(
            Method::PUT,
            "liveBroadcasts",
            &[("part", "contentDetails")],
            Some(&json!({"id": id, "contentDetails": cd})),
        )
        .await?;
        Ok(())
    }

    async fn health(&self) -> Result<Health> {
        let stream = self.stream_id.as_deref().ok_or_else(|| DeviceError::Refused("no stream selected".into()))?;
        let v = self.call(Method::GET, "liveStreams", &[("part", "status"), ("id", stream)], None).await?;
        let Some(item) = v["items"].get(0) else {
            return Err(DeviceError::Refused("stream not found".into()));
        };
        let st = &item["status"];
        let stream_status = st["streamStatus"].as_str().unwrap_or("");
        let health = st["healthStatus"]["status"].as_str().unwrap_or("");
        Ok(match stream_status {
            "active" if health == "good" || health == "ok" => Health::Good,
            // Active (the key is in use) but YouTube has no data: nothing is arriving.
            "active" if health == "noData" => Health::NotReceiving,
            "active" => Health::Bad(if health.is_empty() { "no health reported".into() } else { health.into() }),
            "inactive" | "ready" => Health::NotReceiving,
            "" => Health::Bad("unknown".into()),
            other => Health::Bad(other.into()),
        })
    }

    async fn life_cycle(&self, id: &str) -> Result<String> {
        let b = self.broadcast(id, "status").await?;
        Ok(b["status"]["lifeCycleStatus"].as_str().unwrap_or("").to_string())
    }

    async fn transition(&self, id: &str, to: Phase) -> Result<()> {
        let status = match to {
            Phase::Testing => "testing",
            Phase::Live => "live",
            Phase::Complete => "complete",
        };
        let r = self
            .call(
                Method::POST,
                "liveBroadcasts/transition",
                &[("broadcastStatus", status), ("id", id), ("part", "status")],
                None,
            )
            .await;
        match r {
            // Already in that state: nothing to do.
            Err(DeviceError::Refused(m)) if m == "redundantTransition" => Ok(()),
            other => other.map(|_| ()),
        }
    }
}
