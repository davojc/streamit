use serde_json::json;
use std::sync::Arc;
use streamit_youtube::{Auth, MemoryStore, TokenStore};
use url::Url;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn loopback_sign_in_exchanges_code_with_pkce_and_stores_refresh_token() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("code=thecode"))
        .and(body_string_contains("code_verifier="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token":"at","refresh_token":"rt","expires_in":3600,"token_type":"Bearer"})))
        .expect(1)
        .mount(&s)
        .await;
    let store = Arc::new(MemoryStore::default());
    let auth = Auth::new("cid", "sec", Url::parse(&format!("{}/token", s.uri())).unwrap(), store.clone());
    let seen = Arc::new(std::sync::Mutex::new(None::<Url>));
    let seen2 = seen.clone();
    let open = move |url: &Url| {
        *seen2.lock().unwrap() = Some(url.clone());
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        let redirect = q["redirect_uri"].clone();
        let state = q["state"].clone();
        tokio::spawn(async move {
            // An unrelated hit first (wrong state) must be ignored, then the real redirect.
            let _ = reqwest::get(format!("{redirect}/?code=evil&state=nope")).await;
            let _ = reqwest::get(format!("{redirect}/?code=thecode&state={state}")).await;
        });
    };
    let auth_url = Url::parse("https://accounts.example/auth").unwrap();
    auth.run_sign_in(auth_url, &open).await.unwrap();
    assert_eq!(store.get().as_deref(), Some("rt"));

    let url = seen.lock().unwrap().clone().unwrap();
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["scope"], "https://www.googleapis.com/auth/youtube");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["access_type"], "offline");
    assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert_eq!(auth.signed_in().await, Ok(true));
}

fn new_auth(s: &MockServer, store: Arc<MemoryStore>) -> Auth {
    Auth::new("cid", "sec", Url::parse(&format!("{}/token", s.uri())).unwrap(), store)
}

async fn token_with_refresh(s: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token":"at","refresh_token":"rt","expires_in":3600,"token_type":"Bearer"})))
        .mount(s)
        .await;
}

#[tokio::test]
async fn silent_preconnect_does_not_block_redirect() {
    let s = MockServer::start().await;
    token_with_refresh(&s).await;
    let store = Arc::new(MemoryStore::default());
    let auth = new_auth(&s, store.clone());
    let silent = Arc::new(std::sync::Mutex::new(None));
    let silent2 = silent.clone();
    let open = move |url: &Url| {
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        let redirect = q["redirect_uri"].clone();
        let state = q["state"].clone();
        let addr = redirect.trim_start_matches("http://").to_string();
        let silent = silent2.clone();
        tokio::spawn(async move {
            // A connection that never sends anything, held open.
            let sock = tokio::net::TcpStream::connect(&addr).await.unwrap();
            *silent.lock().unwrap() = Some(sock);
            let _ = reqwest::get(format!("{redirect}/?code=c&state={state}")).await;
        });
    };
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        auth.run_sign_in(Url::parse("https://accounts.example/auth").unwrap(), &open),
    )
    .await;
    assert!(r.expect("sign-in blocked by a silent socket").is_ok());
    assert_eq!(store.get().as_deref(), Some("rt"));
}

#[tokio::test]
async fn error_with_wrong_state_is_ignored() {
    let s = MockServer::start().await;
    token_with_refresh(&s).await;
    let store = Arc::new(MemoryStore::default());
    let auth = new_auth(&s, store.clone());
    let open = move |url: &Url| {
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        let redirect = q["redirect_uri"].clone();
        let state = q["state"].clone();
        tokio::spawn(async move {
            let _ = reqwest::get(format!("{redirect}/?error=access_denied&state=forged")).await;
            let _ = reqwest::get(format!("{redirect}/?error=access_denied")).await;
            let _ = reqwest::get(format!("{redirect}/?code=c&state={state}")).await;
        });
    };
    auth.run_sign_in(Url::parse("https://accounts.example/auth").unwrap(), &open).await.unwrap();
    assert_eq!(store.get().as_deref(), Some("rt"));
}
