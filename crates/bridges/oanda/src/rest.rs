//! Thin blocking Bearer-token REST client for OANDA v20 (rustls via ureq — no OpenSSL).
//!
//! OANDA does not sign requests (unlike the crypto HMAC venues), so this is a small standalone
//! client rather than an impl of the Binance-shaped [`RestTransport`](vike_bridge_core::RestTransport). All
//! requests carry `Accept-Datetime-Format: UNIX` so timestamps arrive as epoch-seconds strings
//! (no RFC3339 date parsing needed). Non-2xx bodies carry OANDA's `errorMessage` /
//! `orderRejectTransaction`, so 4xx responses are returned parsed (not errored) for POSTs.

/// An OANDA REST error, normalized to (status, message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OandaApiError {
    /// HTTP status (0 for a network/transport failure).
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for OandaApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OANDA error {}: {}", self.status, self.message)
    }
}

impl std::error::Error for OandaApiError {}

/// Blocking OANDA v20 client. Holds the Bearer token; the base host is passed per call so one
/// client can talk to REST while a future streaming client uses the stream host.
pub struct OandaRest {
    agent: ureq::Agent,
    token: String,
}

impl OandaRest {
    pub fn new(token: String) -> Self {
        Self { agent: vike_bridge_core::http::blocking_agent(), token }
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// Turn an HTTP (status, body-text) into either the parsed JSON (2xx) or a typed error
    /// (4xx/5xx — OANDA's `errorMessage` when present, else the raw body).
    fn finish(status: u16, text: &str) -> Result<serde_json::Value, OandaApiError> {
        if (200..300).contains(&status) {
            return serde_json::from_str(text)
                .map_err(|e| OandaApiError { status, message: format!("bad json: {e}") });
        }
        let message = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|b| b.get("errorMessage").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or_else(|| text.to_string());
        Err(OandaApiError { status, message })
    }

    fn net_err(e: impl std::fmt::Display) -> OandaApiError {
        OandaApiError { status: 0, message: format!("network error: {e}") }
    }

    /// GET a v3 endpoint. `query` is a pre-formatted `"k=v&k=v"` string (may be empty).
    pub fn get(
        &self,
        base: &str,
        path: &str,
        query: &str,
    ) -> Result<serde_json::Value, OandaApiError> {
        let url = if query.is_empty() {
            format!("{base}{path}")
        } else {
            format!("{base}{path}?{query}")
        };
        let mut resp = self
            .agent
            .get(&url)
            .header("Authorization", &self.bearer())
            .header("Accept-Datetime-Format", "UNIX")
            .call()
            .map_err(Self::net_err)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(Self::net_err)?;
        Self::finish(status, &text)
    }

    /// POST a JSON body (order placement). Returns the parsed response even on 4xx so the caller
    /// can read `orderRejectTransaction`.
    pub fn post_json(
        &self,
        base: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, OandaApiError> {
        let url = format!("{base}{path}");
        let payload = serde_json::to_string(body).map_err(Self::net_err)?;
        let mut resp = self
            .agent
            .post(&url)
            .header("Authorization", &self.bearer())
            .header("Accept-Datetime-Format", "UNIX")
            .header("Content-Type", "application/json")
            .send(payload.as_bytes())
            .map_err(Self::net_err)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(Self::net_err)?;
        Self::finish(status, &text)
    }

    /// PUT with no body (order cancel: `/orders/{id}/cancel`).
    pub fn put_empty(&self, base: &str, path: &str) -> Result<serde_json::Value, OandaApiError> {
        let url = format!("{base}{path}");
        let mut resp = self
            .agent
            .put(&url)
            .header("Authorization", &self.bearer())
            .header("Accept-Datetime-Format", "UNIX")
            .send_empty()
            .map_err(Self::net_err)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(Self::net_err)?;
        Self::finish(status, &text)
    }
}
