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
    let auth = Auth::new(
        "cid",
        "sec",
        Url::parse(&format!("{}/token", s.uri())).unwrap(),
        store.clone(),
    );
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
