//! OAuth for YouTube: installed-app loopback sign-in with PKCE, refresh-token storage and
//! an in-memory access-token cache.
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    Scope, TokenResponse, TokenUrl,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use streamit_core::devices::{DeviceError, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

pub const SCOPE: &str = "https://www.googleapis.com/auth/youtube";
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SERVICE: &str = "StreamIt";
const ENTRY: &str = "youtube";
const SIGN_IN_REQUIRED: &str = "sign-in required";

/// Where the refresh token lives. The real one is the Windows Credential Manager.
pub trait TokenStore: Send + Sync {
    fn get(&self) -> Option<String>;
    fn set(&self, token: &str) -> std::result::Result<(), String>;
    fn clear(&self);
}

/// Windows Credential Manager, service "StreamIt", entry "youtube".
pub struct KeyringStore;

impl KeyringStore {
    fn entry() -> std::result::Result<keyring::Entry, keyring::Error> {
        keyring::Entry::new(SERVICE, ENTRY)
    }
}

impl TokenStore for KeyringStore {
    fn get(&self) -> Option<String> {
        Self::entry().ok()?.get_password().ok()
    }
    fn set(&self, token: &str) -> std::result::Result<(), String> {
        Self::entry()
            .and_then(|e| e.set_password(token))
            .map_err(|e| e.to_string())
    }
    fn clear(&self) {
        if let Ok(e) = Self::entry() {
            let _ = e.delete_credential();
        }
    }
}

/// In-memory store, for tests.
#[derive(Default)]
pub struct MemoryStore(Mutex<Option<String>>);

impl MemoryStore {
    pub fn with_token(token: &str) -> Self {
        Self(Mutex::new(Some(token.to_string())))
    }
}

impl TokenStore for MemoryStore {
    fn get(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }
    fn set(&self, token: &str) -> std::result::Result<(), String> {
        *self.0.lock().unwrap() = Some(token.to_string());
        Ok(())
    }
    fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }
}

struct Inner {
    client_id: String,
    client_secret: String,
    token_url: Url,
    store: Arc<dyn TokenStore>,
    http: reqwest::Client,
    cache: tokio::sync::Mutex<Option<(String, Instant)>>,
}

/// Shared handle to the sign-in state. Cheap to clone.
#[derive(Clone)]
pub struct Auth(Arc<Inner>);

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http client")
}

impl Auth {
    /// Build with an injectable token endpoint and token store.
    pub fn new(
        client_id: &str,
        client_secret: &str,
        token_url: Url,
        store: Arc<dyn TokenStore>,
    ) -> Auth {
        Auth(Arc::new(Inner {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            token_url,
            store,
            http: http_client(),
            cache: tokio::sync::Mutex::new(None),
        }))
    }

    /// Use the stored refresh token (if any) against Google. Never blocks or prompts.
    pub fn load(client_id: &str, client_secret: &str) -> Result<Auth> {
        Ok(Auth::new(
            client_id,
            client_secret,
            Url::parse(TOKEN_URL).expect("token url"),
            Arc::new(KeyringStore),
        ))
    }

    /// Run the browser sign-in and store the refresh token in the Credential Manager.
    pub async fn sign_in(client_id: &str, client_secret: &str) -> Result<Auth> {
        let auth = Auth::load(client_id, client_secret)?;
        auth.run_sign_in(Url::parse(AUTH_URL).expect("auth url"), &|url| {
            if webbrowser::open(url.as_str()).is_err() {
                println!("Open this address in your browser to sign in:\n{url}");
            }
        })
        .await?;
        Ok(auth)
    }

    /// The loopback flow: listen on 127.0.0.1:<ephemeral>, send the user to `auth_url`
    /// (through `open`), take the code from the redirect, exchange it with PKCE.
    pub async fn run_sign_in(&self, auth_url: Url, open: &dyn Fn(&Url)) -> Result<()> {
        let inner = &self.0;
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(other)?;
        let port = listener.local_addr().map_err(other)?.port();
        let redirect = format!("http://127.0.0.1:{port}");

        let client = BasicClient::new(ClientId::new(inner.client_id.clone()))
            .set_client_secret(ClientSecret::new(inner.client_secret.clone()))
            .set_auth_uri(AuthUrl::from_url(auth_url))
            .set_token_uri(TokenUrl::from_url(inner.token_url.clone()))
            .set_redirect_uri(RedirectUrl::new(redirect).map_err(other)?);
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf) = client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new(SCOPE.into()))
            .add_extra_param("access_type", "offline")
            .add_extra_param("prompt", "consent")
            .set_pkce_challenge(challenge)
            .url();
        open(&url);

        let code = tokio::time::timeout(Duration::from_secs(300), wait_for_code(&listener, csrf.secret()))
            .await
            .map_err(|_| DeviceError::Timeout("sign-in was not completed in 5 minutes".into()))??;

        let token = client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(verifier)
            .request_async(&inner.http)
            .await
            .map_err(|e| DeviceError::Refused(format!("sign-in failed: {e}")))?;
        let refresh = token
            .refresh_token()
            .ok_or_else(|| DeviceError::Refused("Google returned no refresh token".into()))?;
        inner.store.set(refresh.secret()).map_err(|e| DeviceError::Other(format!("saving sign-in: {e}")))?;
        let ttl = token.expires_in().unwrap_or(Duration::from_secs(3600));
        *inner.cache.lock().await = Some((token.access_token().secret().clone(), expiry(ttl)));
        Ok(())
    }

    /// A valid access token, refreshing when the cached one is missing or about to expire.
    /// No stored token, or a refresh rejected with `invalid_grant`, is
    /// `Refused("sign-in required")`.
    pub async fn access_token(&self) -> Result<String> {
        let inner = &self.0;
        let mut cache = inner.cache.lock().await;
        if let Some((tok, exp)) = cache.as_ref()
            && Instant::now() < *exp
        {
            return Ok(tok.clone());
        }
        *cache = None;
        let refresh = inner
            .store
            .get()
            .ok_or_else(|| DeviceError::Refused(SIGN_IN_REQUIRED.into()))?;
        let resp = inner
            .http
            .post(inner.token_url.clone())
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
                ("client_id", inner.client_id.as_str()),
                ("client_secret", inner.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(net_err)?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        if status.is_success() {
            let tok = body["access_token"]
                .as_str()
                .ok_or_else(|| DeviceError::Other("token endpoint returned no access_token".into()))?
                .to_string();
            let ttl = Duration::from_secs(body["expires_in"].as_u64().unwrap_or(3600));
            *cache = Some((tok.clone(), expiry(ttl)));
            return Ok(tok);
        }
        if body["error"] == "invalid_grant" {
            inner.store.clear();
            return Err(DeviceError::Refused(SIGN_IN_REQUIRED.into()));
        }
        if status.is_server_error() {
            return Err(DeviceError::Other(format!("token endpoint returned {status}")));
        }
        let why = body["error_description"]
            .as_str()
            .or(body["error"].as_str())
            .unwrap_or("token refresh refused");
        Err(DeviceError::Refused(why.to_string()))
    }

    /// Forget the cached access token (after a 401).
    pub async fn invalidate(&self) {
        *self.0.cache.lock().await = None;
    }

    /// True when a refresh yields an access token; false when sign-in is needed.
    pub async fn signed_in(&self) -> Result<bool> {
        match self.access_token().await {
            Ok(_) => Ok(true),
            Err(DeviceError::Refused(m)) if m == SIGN_IN_REQUIRED => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Expire a minute early so a request never carries a token about to die.
fn expiry(ttl: Duration) -> Instant {
    Instant::now() + ttl.saturating_sub(Duration::from_secs(60))
}

fn other(e: impl std::fmt::Display) -> DeviceError {
    DeviceError::Other(e.to_string())
}

pub(crate) fn net_err(e: reqwest::Error) -> DeviceError {
    if e.is_timeout() {
        DeviceError::Timeout(e.to_string())
    } else {
        DeviceError::Other(e.to_string())
    }
}

/// Accept connections until one carries the redirect with a matching `state`.
async fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String> {
    loop {
        let (mut sock, _) = listener.accept().await.map_err(other)?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let text = String::from_utf8_lossy(&buf[..n]);
        let target = text.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            let _ = respond(&mut sock, "Not found").await;
            continue;
        };
        let param = |k: &str| url.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
        if let Some(err) = param("error") {
            let _ = respond(&mut sock, "Sign-in was cancelled. You can close this tab.").await;
            return Err(DeviceError::Refused(format!("sign-in refused: {err}")));
        }
        match (param("code"), param("state")) {
            (Some(code), Some(s)) if s == state => {
                let _ = respond(&mut sock, "Signed in to StreamIt. You can close this tab.").await;
                return Ok(code);
            }
            _ => {
                let _ = respond(&mut sock, "Not found").await;
            }
        }
    }
}

async fn respond(sock: &mut tokio::net::TcpStream, msg: &str) -> std::io::Result<()> {
    let body = format!("<html><body><p>{msg}</p></body></html>");
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body.as_bytes()).await?;
    sock.shutdown().await
}
