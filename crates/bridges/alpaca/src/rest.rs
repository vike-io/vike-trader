//! Alpaca Bearer REST — wraps a shared [`TokenSource`], injecting `Authorization: Bearer` and
//! transparently refreshing-and-retrying ONCE on a 401 (the token is short-lived). Shape copied
//! from `crates/bridges/oanda/src/rest.rs`. ureq blocking (rustls), no OpenSSL.

use std::sync::Arc;

use crate::auth::TokenSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlpacaApiError {
    pub status: u16,
    pub message: String,
}
impl std::fmt::Display for AlpacaApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "alpaca HTTP {}: {}", self.status, self.message)
    }
}
impl std::error::Error for AlpacaApiError {}

pub struct AlpacaRest {
    agent: ureq::Agent,
    token: Arc<TokenSource>,
}

impl AlpacaRest {
    pub fn new(token: Arc<TokenSource>) -> Self {
        Self { agent: vike_bridge_core::http::blocking_agent(), token }
    }

    /// A token error keeps its CLASS. A mint that ANSWERED and refused is an auth rejection (401);
    /// a mint that was never REACHED is a transport failure and wears [`Self::net_err`]'s
    /// `status: 0`, like every other unreached socket in this file. Collapsing the two made a
    /// geo-blocked `authx` host render as `alpaca HTTP 401` — read as "bad keys", answered with a
    /// credential rotation, for a network condition.
    fn auth_err(e: crate::auth::AuthError) -> AlpacaApiError {
        let status = match e {
            crate::auth::AuthError::Exchange(_) => 401,
            crate::auth::AuthError::Transport(_) => 0,
        };
        AlpacaApiError { status, message: e.to_string() }
    }

    fn bearer(&self) -> Result<String, AlpacaApiError> {
        self.token.bearer().map(|t| format!("Bearer {t}")).map_err(Self::auth_err)
    }

    fn net_err(e: impl std::fmt::Display) -> AlpacaApiError {
        AlpacaApiError { status: 0, message: e.to_string() }
    }

    fn finish(status: u16, text: &str) -> Result<serde_json::Value, AlpacaApiError> {
        if (200..300).contains(&status) {
            if text.trim().is_empty() {
                return Ok(serde_json::Value::Null);
            }
            // Preserve the real 2xx status on a malformed body (not `net_err`'s status:0), so a
            // caller distinguishing transport failure (0) from a server response keeps the signal.
            return serde_json::from_str(text)
                .map_err(|e| AlpacaApiError { status, message: format!("bad json: {e}") });
        }
        let message = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or_else(|| text.trim().to_string());
        Err(AlpacaApiError { status, message })
    }

    // --- one-shot senders (given an explicit bearer) ---------------------------------------
    fn send_get(&self, url: &str, bearer: &str) -> Result<(u16, String), AlpacaApiError> {
        let mut resp =
            self.agent.get(url).header("Authorization", bearer).call().map_err(Self::net_err)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(Self::net_err)?;
        Ok((status, text))
    }
    fn send_body(
        &self,
        method: &str,
        url: &str,
        bearer: &str,
        body: &[u8],
    ) -> Result<(u16, String), AlpacaApiError> {
        // ureq 3.x's typed `.delete()` builder carries no body (`RequestBuilder<WithoutBody>`,
        // unlike post/patch's `WithBody`), so DELETE (always an empty body for this client) runs
        // via `.call()` while POST/PATCH run via `.send(body)`.
        let mut resp = match method {
            "POST" => self
                .agent
                .post(url)
                .header("Authorization", bearer)
                .header("Content-Type", "application/json")
                .send(body)
                .map_err(Self::net_err)?,
            "PATCH" => self
                .agent
                .patch(url)
                .header("Authorization", bearer)
                .header("Content-Type", "application/json")
                .send(body)
                .map_err(Self::net_err)?,
            "DELETE" => self
                .agent
                .delete(url)
                .header("Authorization", bearer)
                .call()
                .map_err(Self::net_err)?,
            _ => unreachable!("unsupported method {method}"),
        };
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(Self::net_err)?;
        Ok((status, text))
    }

    // --- verbs (refresh-and-retry once on 401) ---------------------------------------------
    pub fn get(
        &self,
        base: &str,
        path: &str,
        query: &str,
    ) -> Result<serde_json::Value, AlpacaApiError> {
        let url = if query.is_empty() {
            format!("{base}{path}")
        } else {
            format!("{base}{path}?{query}")
        };
        let (mut status, mut text) = self.send_get(&url, &self.bearer()?)?;
        if status == 401 {
            let fresh = self.token.force_refresh().map_err(Self::auth_err)?;
            (status, text) = self.send_get(&url, &format!("Bearer {fresh}"))?;
        }
        Self::finish(status, &text)
    }

    fn body_verb(
        &self,
        method: &str,
        base: &str,
        path: &str,
        body: &[u8],
    ) -> Result<serde_json::Value, AlpacaApiError> {
        let url = format!("{base}{path}");
        let (mut status, mut text) = self.send_body(method, &url, &self.bearer()?, body)?;
        if status == 401 {
            let fresh = self.token.force_refresh().map_err(Self::auth_err)?;
            (status, text) = self.send_body(method, &url, &format!("Bearer {fresh}"), body)?;
        }
        Self::finish(status, &text)
    }

    pub fn post_json(
        &self,
        base: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, AlpacaApiError> {
        self.body_verb("POST", base, path, body.to_string().as_bytes())
    }
    pub fn patch_json(
        &self,
        base: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, AlpacaApiError> {
        self.body_verb("PATCH", base, path, body.to_string().as_bytes())
    }
    pub fn delete(&self, base: &str, path: &str) -> Result<serde_json::Value, AlpacaApiError> {
        self.body_verb("DELETE", base, path, b"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthError;

    #[test]
    fn finish_parses_2xx_and_errors() {
        assert_eq!(AlpacaRest::finish(200, r#"{"id":"x"}"#).unwrap()["id"], "x");
        assert!(AlpacaRest::finish(200, "").unwrap().is_null()); // empty 2xx body → Null
        let e = AlpacaRest::finish(422, r#"{"code":40010001,"message":"bad ssn"}"#).unwrap_err();
        assert_eq!(e.status, 422);
        assert_eq!(e.message, "bad ssn");
        let e2 = AlpacaRest::finish(401, r#"{"message":"unauthorized."}"#).unwrap_err();
        assert_eq!(e2.message, "unauthorized.");
        // A malformed 2xx body preserves the real status (not net_err's 0), so callers can still
        // tell a server response from a transport failure.
        let e3 = AlpacaRest::finish(200, "not json").unwrap_err();
        assert_eq!(e3.status, 200);
        assert!(e3.message.starts_with("bad json"));
    }

    /// A token mint that was never REACHED is a transport failure, and must not wear `HTTP 401`.
    /// `bearer()` mapped EVERY `TokenSource` error to 401, so an unreachable `authx` host rendered
    /// as `alpaca HTTP 401: …: io: Connection refused` — which an operator reads as "bad keys" and
    /// answers with a credential rotation, for what is a network condition. Measured on the
    /// Windows dev box 2026-08-22, where alpaca's sandbox is geo-blocked and every startup
    /// preflight reported it that way. The `status: 0` convention `finish` already documents above
    /// is the one this restores.
    #[test]
    fn an_unreachable_token_mint_is_not_reported_as_an_auth_rejection() {
        let token = TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(|_| Err(AuthError::Transport("io: Connection refused".into()))),
        );
        let rest = AlpacaRest::new(std::sync::Arc::new(token));
        // `get` asks for a bearer FIRST, so this fails before any socket is opened.
        let e = rest.get("https://broker.invalid", "/v1/accounts", "").unwrap_err();
        assert_eq!(e.status, 0, "a transport failure keeps net_err's 0, not 401: {e}");
        assert!(e.to_string().contains("Connection refused"), "the cause survives: {e}");
    }

    /// …and the OTHER half: a mint that ANSWERED with a refusal is still an auth rejection.
    #[test]
    fn a_refused_token_exchange_is_still_reported_as_an_auth_rejection() {
        let token = TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(|_| Err(AuthError::Exchange("HTTP 401".into()))),
        );
        let rest = AlpacaRest::new(std::sync::Arc::new(token));
        let e = rest.get("https://broker.invalid", "/v1/accounts", "").unwrap_err();
        assert_eq!(e.status, 401, "the mint answered and refused: {e}");
    }
}
