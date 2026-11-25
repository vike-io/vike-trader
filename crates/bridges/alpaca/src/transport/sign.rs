//! OAuth2 client-credentials token source — the one bridge that mints a short-lived Bearer.
//!
//! Alpaca Broker API creds are OAuth2 client-credentials, exchanged at `{authx}/v1/oauth2/token`
//! for a 900 s Bearer (verified live 2026-07-14). This caches the token and refreshes it
//! proactively (within [`REFRESH_MARGIN_SECS`] of expiry) and on demand ([`TokenSource::force_refresh`],
//! the stream's 401; `transport/mod.rs`'s 401 ladder re-mints through [`TokenSource::relogin`]). The cache is the
//! shared `TokenCell`, so two calls that see the token die together mint ONCE. The exchange is abstracted
//! behind a closure so tests inject a fake exchanger + clock and never touch the network. The
//! secret/id/token never reach Debug/logs.

use vike_bridge_core::transport::auth::{TokenCell, token_refresh_due};

const REFRESH_MARGIN_SECS: u64 = 60;

/// **The bound on one token mint** (the agent's whole-call timeout): a mint is a small form POST answered in well under
/// a second, and every mint sits in an order's submit chain, before its POST (counted against the queue's wait) or
/// after a 401 on it, so a mint at the shared 30 s would carry a submit past the core's last-resort reject. Pinned to
/// `vike_venues::venue_timings::ALPACA`'s `submit` chain beside `crate::execution`'s submit.
pub(crate) const MINT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// This module's whole-second clock reading as the epoch-ms [`token_refresh_due`] compares.
fn ms(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX).saturating_mul(1000)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The mint ANSWERED and the answer was a refusal — a non-2xx, or a body carrying no
    /// `access_token`. This one IS about the credentials.
    Exchange(String),
    /// The mint was never REACHED: connect refused, timed out, TLS failed, or the body read died.
    /// It says NOTHING about the credentials, so it must not wear an auth status code — see
    /// [`crate::transport::ALPACA_HTTP`]'s `status: 0` transport convention.
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

#[derive(Clone)]
pub(crate) struct CachedToken {
    access_token: String,
    expires_at_unix: u64,
}

/// The `Authorization` value of the token a cell holds (`None` before the first mint: an empty Bearer, which the
/// venue refuses like any dead token).
pub(crate) fn bearer_header(token: &Option<CachedToken>) -> String {
    format!("Bearer {}", token.as_ref().map_or("", |t| t.access_token.as_str()))
}

/// `(access_token, expires_in_secs)` on success.
type Exchanger = Box<dyn Fn(&str) -> Result<(String, u64), AuthError> + Send + Sync>;
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

pub struct TokenSource {
    client_id: String,
    client_secret: String,
    /// The cached token (`None` before the first mint) and the generation each mint bumps.
    cell: TokenCell<Option<CachedToken>>,
    exchanger: Exchanger,
    clock: Clock,
}

impl std::fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenSource(cached={})", self.cell.current().0.is_some())
    }
}

impl TokenSource {
    /// Production constructor: installs the real ureq exchanger against `{authx}/v1/oauth2/token`.
    pub fn new(client_id: String, client_secret: String, authx: String) -> Self {
        let url = format!("{authx}/v1/oauth2/token");
        let exchanger: Exchanger = Box::new(move |body: &str| {
            let agent = vike_transport::http::blocking_agent_with_timeout(MINT_TIMEOUT);
            let mut response = agent
                .post(&url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .send(body.as_bytes())
                .map_err(|e| AuthError::Transport(e.to_string()))?;
            let status = response.status().as_u16();
            let text = response
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
            cell: TokenCell::new(None),
            exchanger,
            // `now_secs` saturates to 0 before the epoch, so the cast to the token clock's u64 is exact.
            clock: Box::new(|| vike_model::time::clock::now_secs() as u64),
        }
    }

    /// Test-only: inject a deterministic clock + fake exchanger.
    #[doc(hidden)]
    pub fn with_clock_and_exchanger(clock: Clock, exchanger: Exchanger) -> Self {
        Self {
            client_id: "test-id".into(),
            client_secret: "test-secret".into(),
            cell: TokenCell::new(None),
            exchanger,
            clock,
        }
    }

    /// A valid Bearer, refreshing proactively within the margin. Callers coalesce on the cell: two that find
    /// the token due mint once.
    pub fn bearer(&self) -> Result<String, AuthError> {
        let (cached, generation) = self.cell.current();
        let now = (self.clock)();
        if let Some(c) = cached
            && !token_refresh_due(Some(ms(c.expires_at_unix)), ms(now), ms(REFRESH_MARGIN_SECS))
        {
            return Ok(c.access_token);
        }
        self.mint(generation)
    }

    /// Re-exchange now (the stream's on-401 path), unless another caller already did since this one read the
    /// cell. Replaces the cached token.
    pub fn force_refresh(&self) -> Result<String, AuthError> {
        self.mint(self.cell.current().1)
    }

    /// The cell `transport/mod.rs`'s 401 ladder runs on.
    pub(crate) fn cell(&self) -> &TokenCell<Option<CachedToken>> {
        &self.cell
    }

    /// Mint again unless another caller already did since generation `seen`, and answer the generation in force
    /// (`TokenCell::relogin`): the 401 ladder's re-login.
    pub(crate) fn relogin(&self, seen: u64) -> Result<u64, AuthError> {
        let now = (self.clock)();
        self.cell.relogin(seen, || self.exchange(now).map(Some))
    }

    /// [`Self::relogin`] from `seen`, answering the access token then in force.
    fn mint(&self, seen: u64) -> Result<String, AuthError> {
        self.relogin(seen)?;
        Ok(self.cell.current().0.map(|c| c.access_token).unwrap_or_default())
    }

    fn exchange(&self, now: u64) -> Result<CachedToken, AuthError> {
        // client_id/client_secret are opaque credential strings and may contain
        // `&`/`=`/`+`/`%`/whitespace, which would corrupt the form body if
        // interpolated raw — percent-encode via the shared urlencode helper.
        let body = format!(
            "grant_type=client_credentials&{}",
            vike_bridge_core::urlencode(&[
                ("client_id", self.client_id.clone()),
                ("client_secret", self.client_secret.clone()),
            ])
        );
        let (access_token, expires_in) = (self.exchanger)(&body)?;
        Ok(CachedToken { access_token, expires_at_unix: now + expires_in })
    }
}
