use serde_json::json;
use std::sync::Arc;
use streamit_core::devices::{Broadcasts, DeviceError, Fix, Health, Phase};
use streamit_youtube::{Auth, MemoryStore, YouTubeClient};
use url::Url;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn token_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token":"at","expires_in":3600,"token_type":"Bearer"}),
        ))
        .mount(server)
        .await;
}

fn client(server: &MockServer, stream: Option<&str>) -> YouTubeClient {
    let store = Arc::new(MemoryStore::with_token("refresh"));
    let auth = Auth::new(
        "cid",
        "sec",
        Url::parse(&format!("{}/token", server.uri())).unwrap(),
        store,
    );
    YouTubeClient::new(
        Url::parse(&format!("{}/youtube/v3", server.uri())).unwrap(),
        auth,
        stream.map(String::from),
    )
}

fn broadcast(id: &str, start: Option<&str>, bound: Option<&str>) -> serde_json::Value {
    let mut b = json!({"id": id, "snippet": {"title": format!("T {id}")},
        "status": {"lifeCycleStatus": "ready"}, "contentDetails": {}});
    if let Some(s) = start {
        b["snippet"]["scheduledStartTime"] = json!(s);
    }
    if let Some(s) = bound {
        b["contentDetails"]["boundStreamId"] = json!(s);
    }
    b
}

#[tokio::test]
async fn upcoming_sorted_by_scheduled_start() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/liveBroadcasts"))
        .and(query_param("broadcastStatus", "upcoming"))
        .and(query_param("maxResults", "25"))
        .and(query_param("part", "id,snippet,contentDetails,status"))
        .and(header("authorization", "Bearer at"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[
            broadcast("b", Some("2026-11-02T09:00:00Z"), None),
            broadcast("a", Some("2026-11-01T09:00:00Z"), Some("s1")),
            broadcast("n", None, None)]})))
        .mount(&s)
        .await;
    let list = client(&s, Some("s1")).upcoming().await.unwrap();
    let ids: Vec<_> = list.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "n"]);
    assert_eq!(list[0].bound_stream_id.as_deref(), Some("s1"));
    assert_eq!(list[0].life_cycle, "ready");
    assert_eq!(list[0].title, "T a");
}

#[tokio::test]
async fn check_reads_flags_and_binding() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/liveBroadcasts"))
        .and(query_param("id", "x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{
            "id":"x","contentDetails":{"boundStreamId":"s1","enableAutoStart":true,
            "enableAutoStop":false,"monitorStream":{"enableMonitorStream":true}}}]})))
        .mount(&s)
        .await;
    let c = client(&s, Some("s1")).check("x").await.unwrap();
    assert!(c.bound_to_ours && c.auto_start && !c.auto_stop && c.monitor_stream);
    let c = client(&s, Some("other")).check("x").await.unwrap();
    assert!(!c.bound_to_ours);
}

#[tokio::test]
async fn check_missing_fields_default_and_unknown_id_is_not_found() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    Mock::given(method("GET"))
        .and(query_param("id", "x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"x"}]})))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(query_param("id", "gone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&s)
        .await;
    let c = client(&s, None).check("x").await.unwrap();
    assert!(!c.bound_to_ours && !c.auto_start && !c.auto_stop && !c.monitor_stream);
    assert_eq!(
        client(&s, None).check("gone").await,
        Err(DeviceError::Refused("broadcast not found".into()))
    );
}

#[tokio::test]
async fn fix_auto_start_sends_full_content_details() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    let cd = json!({"boundStreamId":"s1","enableAutoStart":true,"enableAutoStop":true,
        "enableDvr":true,"latencyPreference":"low","monitorStream":{"enableMonitorStream":true,
        "broadcastStreamDelayMs":0}});
    Mock::given(method("GET"))
        .and(path("/youtube/v3/liveBroadcasts"))
        .and(query_param("id", "x"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"x","contentDetails":cd}]})),
        )
        .mount(&s)
        .await;
    let mut want = cd.clone();
    want["enableAutoStart"] = json!(false);
    Mock::given(method("PUT"))
        .and(path("/youtube/v3/liveBroadcasts"))
        .and(query_param("part", "contentDetails"))
        .and(body_partial_json(json!({"id":"x","contentDetails":want})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"x"})))
        .expect(1)
        .mount(&s)
        .await;
    client(&s, Some("s1")).fix("x", Fix::AutoStartOff).await.unwrap();
}

#[tokio::test]
async fn fix_monitor_on_and_auto_stop_on() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    Mock::given(method("GET"))
        .and(query_param("id", "x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"x",
            "contentDetails":{"enableAutoStop":false,"enableDvr":true}}]})))
        .mount(&s)
        .await;
    Mock::given(method("PUT"))
        .and(body_partial_json(
            json!({"contentDetails":{"enableAutoStop":true,"enableDvr":true}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&s)
        .await;
    Mock::given(method("PUT"))
        .and(body_partial_json(json!({"contentDetails":
            {"enableAutoStop":false,"monitorStream":{"enableMonitorStream":true}}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&s)
        .await;
    let c = client(&s, None);
    c.fix("x", Fix::AutoStopOn).await.unwrap();
    c.fix("x", Fix::MonitorOn).await.unwrap();
}

#[tokio::test]
async fn fix_bind_posts_bind() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    Mock::given(method("POST"))
        .and(path("/youtube/v3/liveBroadcasts/bind"))
        .and(query_param("id", "x"))
        .and(query_param("streamId", "s1"))
        .and(query_param("part", "id,contentDetails"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"x"})))
        .expect(1)
        .mount(&s)
        .await;
    client(&s, Some("s1")).fix("x", Fix::Bind).await.unwrap();
    assert_eq!(
        client(&s, None).fix("x", Fix::Bind).await,
        Err(DeviceError::Refused("no stream selected".into()))
    );
}

async fn health_for(status: serde_json::Value, stream: Option<&str>) -> Result<Health, DeviceError> {
    let s = MockServer::start().await;
    token_ok(&s).await;
    Mock::given(method("GET"))
        .and(path("/youtube/v3/liveStreams"))
        .and(query_param("part", "status"))
        .and(query_param("id", "s1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"items":[{"status":status}]})),
        )
        .mount(&s)
        .await;
    client(&s, stream).health().await
}

#[tokio::test]
async fn health_maps_status() {
    let some = Some("s1");
    assert_eq!(
        health_for(json!({"streamStatus":"active","healthStatus":{"status":"good"}}), some).await,
        Ok(Health::Good)
    );
    assert_eq!(
        health_for(json!({"streamStatus":"active","healthStatus":{"status":"ok"}}), some).await,
        Ok(Health::Good)
    );
    assert_eq!(
        health_for(json!({"streamStatus":"inactive"}), some).await,
        Ok(Health::NotReceiving)
    );
    assert_eq!(
        health_for(json!({"streamStatus":"ready"}), some).await,
        Ok(Health::NotReceiving)
    );
    assert!(matches!(
        health_for(json!({"streamStatus":"active","healthStatus":{"status":"bad"}}), some).await,
        Ok(Health::Bad(_))
    ));
    assert!(matches!(
        health_for(json!({"streamStatus":"active","healthStatus":{"status":"noData"}}), some).await,
        Ok(Health::Bad(_))
    ));
    assert!(matches!(health_for(json!({}), some).await, Ok(Health::Bad(_))));
    assert_eq!(
        health_for(json!({}), None).await,
        Err(DeviceError::Refused("no stream selected".into()))
    );
}

#[tokio::test]
async fn life_cycle_reads_status() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    Mock::given(method("GET"))
        .and(query_param("part", "status"))
        .and(query_param("id", "x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"items":[{"status":{"lifeCycleStatus":"testing"}}]}),
        ))
        .mount(&s)
        .await;
    assert_eq!(client(&s, None).life_cycle("x").await.unwrap(), "testing");
}

#[tokio::test]
async fn transition_posts_expected_query() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    for (phase, name) in [
        (Phase::Testing, "testing"),
        (Phase::Live, "live"),
        (Phase::Complete, "complete"),
    ] {
        Mock::given(method("POST"))
            .and(path("/youtube/v3/liveBroadcasts/transition"))
            .and(query_param("broadcastStatus", name))
            .and(query_param("id", "x"))
            .and(query_param("part", "status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id":"x"})))
            .expect(1)
            .mount(&s)
            .await;
        client(&s, None).transition("x", phase).await.unwrap();
    }
}

#[tokio::test]
async fn redundant_transition_is_ok() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    Mock::given(method("POST"))
        .and(path("/youtube/v3/liveBroadcasts/transition"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error":{"code":403,
            "message":"Redundant transition","errors":[{"reason":"redundantTransition"}]}})))
        .mount(&s)
        .await;
    client(&s, None).transition("x", Phase::Live).await.unwrap();
}

#[tokio::test]
async fn http_errors_map() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    Mock::given(method("GET"))
        .and(query_param("id", "forbidden"))
        .respond_with(ResponseTemplate::new(403).set_body_json(
            json!({"error":{"errors":[{"reason":"quotaExceeded"}]}}),
        ))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(query_param("id", "missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("<html>nope</html>"))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(query_param("id", "boom"))
        .respond_with(ResponseTemplate::new(503).set_body_string("oops"))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(query_param("id", "junk"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&s)
        .await;
    let c = client(&s, None);
    assert_eq!(c.check("forbidden").await, Err(DeviceError::Refused("quotaExceeded".into())));
    assert_eq!(c.check("missing").await, Err(DeviceError::Refused("broadcast not found".into())));
    assert!(matches!(c.check("boom").await, Err(DeviceError::Other(_))));
    assert!(matches!(c.check("junk").await, Err(DeviceError::Other(_))));
}

#[tokio::test]
async fn unauthorized_refreshes_token_once_and_retries() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token":"at","expires_in":3600,"token_type":"Bearer"}),
        ))
        .expect(2)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(query_param("id", "x"))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(query_param("id", "x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[{"id":"x"}]})))
        .mount(&s)
        .await;
    client(&s, None).check("x").await.unwrap();
}

#[tokio::test]
async fn expired_token_reports_signin_required() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error":"invalid_grant","error_description":"Token has been expired or revoked."})))
        .mount(&s)
        .await;
    let c = client(&s, Some("s1"));
    assert_eq!(c.signed_in().await, Ok(false));
    assert_eq!(c.upcoming().await, Err(DeviceError::Refused("sign-in required".into())));
    assert_eq!(c.health().await, Err(DeviceError::Refused("sign-in required".into())));
}

#[tokio::test]
async fn no_stored_token_is_signed_out_and_working_token_is_signed_in() {
    let s = MockServer::start().await;
    token_ok(&s).await;
    let auth = Auth::new(
        "c",
        "s",
        Url::parse(&format!("{}/token", s.uri())).unwrap(),
        Arc::new(MemoryStore::default()),
    );
    let c = YouTubeClient::new(Url::parse(&s.uri()).unwrap(), auth, None);
    assert_eq!(c.signed_in().await, Ok(false));
    assert_eq!(client(&s, None).signed_in().await, Ok(true));
}
