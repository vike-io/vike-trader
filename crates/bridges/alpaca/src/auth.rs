//! OAuth2 client-credentials token source — the one bridge that mints a short-lived Bearer.
//!
//! Alpaca Broker API creds are OAuth2 client-credentials, exchanged at `{authx}/v1/oauth2/token`
//! for a 900 s Bearer (verified live 2026-07-14). This caches the token and refreshes it
//! proactively (within [`REFRESH_MARGIN_SECS`] of expiry) and on demand ([`TokenSource::force_refresh`], called
//! by `rest.rs` on a 401). The exchange is abstracted behind a closure so tests inject a fake
//! exchanger + clock and never touch the network. The secret/id/token never reach Debug/logs.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const REFRESH_MARGIN_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The mint ANSWERED and the answer was a refusal — a non-2xx, or a body carrying no
    /// `access_token`. This one IS about the credentials.
    Exchange(String),
    /// The mint was never REACHED: connect refused, timed out, TLS failed, or the body read died.
    /// It says NOTHING about the credentials, so it must not wear an auth status code — see
    /// [`crate::rest::AlpacaApiError`]'s `status: 0` transport convention.
    Transport(String),
}
impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Exchange(m) => write!(f, "alpaca token exchange failed: {m}"),
            AuthError::Transport(m) => write!(f, "alpaca token mint unreachable: {m}"),
        }
    }
}
impl std::error::Error for AuthError {}

struct CachedToken {
    access_token: String,
    expires_at_unix: u64,
}

/// `(access_token, expires_in_secs)` on success.
type Exchanger = Box<dyn Fn(&str) -> Result<(String, u64), AuthError> + Send + Sync>;
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

pub struct TokenSource {
    client_id: String,
    client_secret: String,
    cache: Mutex<Option<CachedToken>>,
    exchanger: Exchanger,
    clock: Clock,
}

impl std::fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenSource(cached={})", self.cache.lock().map(|c| c.is_some()).unwrap_or(false))
    }
}

impl TokenSource {
    /// Production constructor: installs the real ureq exchanger against `{authx}/v1/oauth2/token`.
    pub fn new(client_id: String, client_secret: String, authx: String) -> Self {
        let url = format!("{authx}/v1/oauth2/token");
        let exchanger: Exchanger = Box::new(move |body: &str| {
            let agent = vike_bridge_core::http::blocking_agent();
            let mut resp = agent
                .post(&url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .send(body.as_bytes())
                .map_err(|e| AuthError::Transport(e.to_string()))?;
            let status = resp.status().as_u16();
            let text = resp
                .body_mut()
                .read_to_string()
                .map_err(|e| AuthError::Transport(e.to_string()))?;
            if !(200..300).contains(&status) {
                return Err(AuthError::Exchange(format!("HTTP {status}")));
            }
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| AuthError::Exchange(e.to_string()))?;
            let token = v
                .get("access_token")
                .and_then(|t| t.as_str())
                .ok_or_else(|| AuthError::Exchange("no access_token".into()))?;
            let expires = v.get("expires_in").and_then(|e| e.as_u64()).unwrap_or(900);
            Ok((token.to_string(), expires))
        });
        Self {
            client_id,
            client_secret,
            cache: Mutex::new(None),
            exchanger,
            clock: Box::new(system_now_unix),
        }
    }

    /// Test-only: inject a deterministic clock + fake exchanger.
    #[doc(hidden)]
    pub fn with_clock_and_exchanger(clock: Clock, exchanger: Exchanger) -> Self {
        Self {
            client_id: "test-id".into(),
            client_secret: "test-secret".into(),
            cache: Mutex::new(None),
            exchanger,
            clock,
        }
    }

    /// A valid Bearer, refreshing proactively within the margin. Callers coalesce on the lock.
    pub fn bearer(&self) -> Result<String, AuthError> {
        let mut guard = self.cache.lock().expect("token cache poisoned");
        let now = (self.clock)();
        if let Some(c) = guard.as_ref()
            && now + REFRESH_MARGIN_SECS < c.expires_at_unix
        {
            return Ok(c.access_token.clone());
        }
        let tok = self.exchange(now)?;
        let out = tok.access_token.clone();
        *guard = Some(tok);
        Ok(out)
    }

    /// Unconditional re-exchange (the on-401 path). Replaces the cached token.
    pub fn force_refresh(&self) -> Result<String, AuthError> {
        let mut guard = self.cache.lock().expect("token cache poisoned");
        let now = (self.clock)();
        let tok = self.exchange(now)?;
        let out = tok.access_token.clone();
        *guard = Some(tok);
        Ok(out)
    }

    fn exchange(&self, now: u64) -> Result<CachedToken, AuthError> {
        // client_id/client_secret are opaque credential strings and may contain
        // `&`/`=`/`+`/`%`/whitespace, which would corrupt the form body if
        // interpolated raw — percent-encode via the shared urlencode helper.
        let body = format!(
            "grant_type=client_credentials&{}",
            vike_bridge_core::signer::urlencode(&[
                ("client_id", self.client_id.clone()),
                ("client_secret", self.client_secret.clone()),
            ])
        );
        let (access_token, expires_in) = (self.exchanger)(&body)?;
        Ok(CachedToken { access_token, expires_at_unix: now + expires_in })
    }
}

fn system_now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
