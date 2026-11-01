//! Thin blocking Bearer-token REST client for OANDA v20 (rustls via ureq — no OpenSSL).
//!
//! OANDA does not sign requests (unlike the crypto HMAC venues), so this is a small standalone
//! client rather than an impl of the Binance-shaped [`RestTransport`](vike_bridge_core::transport::RestTransport). All
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

/// The `status` of an error that happened AFTER the request may have reached OANDA (a timeout or a
/// reset once the body was written, a response whose body never arrived): the order MAY be live.
/// Not an HTTP status (those start at 100), so it cannot collide; `0` stays "provably never sent".
pub(crate) const STATUS_OUTCOME_UNKNOWN: u16 = 1;

impl OandaApiError {
    /// Did the venue possibly act on the request? A submit that sees this re-queries by client order
    /// id and NEVER emits a terminal reject. The gateway statuses are the same set as
    /// `vike_bridge_core::transport::status_is_gateway_ambiguous` (a proxy in front of OANDA says
    /// nothing about what OANDA did).
    pub fn outcome_unknown(&self) -> bool {
        self.status == STATUS_OUTCOME_UNKNOWN
            || vike_bridge_core::transport::status_is_gateway_ambiguous(self.status)
    }
}

/// Is this a 404 body that says THE ORDER does not exist (and not the account, or a routing miss)?
/// `errorCode` is optional in v20 error bodies, so a missing one falls back to the message; a
/// present one must be `ORDER_DOES_NOT_EXIST`.
pub(crate) fn is_order_does_not_exist(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return false };
    let msg = v.get("errorMessage").and_then(|m| m.as_str()).unwrap_or_default().to_lowercase();
    match v.get("errorCode").and_then(|c| c.as_str()) {
        Some(code) => code == "ORDER_DOES_NOT_EXIST",
        None => msg.contains("order") && msg.contains("does not exist"),
    }
}

/// Blocking OANDA v20 client. Holds the Bearer token; the base host is passed per call so one
/// client can talk to REST while a future streaming client uses the stream host.
pub struct OandaRest {
    agent: ureq::Agent,
    /// The agent of the by-client-id LOOKUP reads ([`LOOKUP_TIMEOUT`] instead of the 30 s global one),
    /// so a lookup ladder is bounded by seconds and never holds the exec thread behind it for minutes.
    lookup_agent: ureq::Agent,
    token: String,
}

/// The global timeout of one lookup read (`get_order`, `get_lookup`). The steps of one resolution
/// share `crate::exec`'s `RESOLVE_BUDGET`; this bounds the ONE read in flight when it expires.
pub(crate) const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl OandaRest {
    pub fn new(token: String) -> Self {
        Self {
            agent: vike_bridge_core::http::blocking_agent(),
            lookup_agent: vike_bridge_core::http::blocking_agent_with_timeout(LOOKUP_TIMEOUT),
            token,
        }
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

    /// [`Self::finish`] for an ORDER reply: a 2xx whose body does not parse says OANDA ANSWERED yes
    /// to something, so it is an UNKNOWN outcome (resolve by client id), never a terminal reject.
    fn finish_order(status: u16, text: &str) -> Result<serde_json::Value, OandaApiError> {
        Self::finish(status, text).map_err(|e| {
            if (200..300).contains(&e.status) {
                OandaApiError { status: STATUS_OUTCOME_UNKNOWN, message: e.message }
            } else {
                e
            }
        })
    }

    fn net_err(e: impl std::fmt::Display) -> OandaApiError {
        OandaApiError { status: 0, message: format!("network error: {e}") }
    }

    /// A send failure of an ORDER request: ambiguous ([`STATUS_OUTCOME_UNKNOWN`]) unless it provably
    /// preceded the write (`vike_bridge_core::transport::classify_send_error` is the one rule).
    fn order_send_err(e: ureq::Error) -> OandaApiError {
        if vike_bridge_core::transport::send_error_is_definite(&e) {
            Self::net_err(e)
        } else {
            OandaApiError { status: STATUS_OUTCOME_UNKNOWN, message: format!("network error: {e}") }
        }
    }

    /// A body read failure on an ORDER reply: the headers arrived, so OANDA received the request.
    fn order_read_err(e: ureq::Error) -> OandaApiError {
        OandaApiError {
            status: STATUS_OUTCOME_UNKNOWN,
            message: format!("network error reading the reply: {e}"),
        }
    }

    /// GET a v3 endpoint. `query` is a pre-formatted `"k=v&k=v"` string (may be empty).
    pub fn get(
        &self,
        base: &str,
        path: &str,
        query: &str,
    ) -> Result<serde_json::Value, OandaApiError> {
        let (status, text) = self.get_raw(&self.agent, base, path, query)?;
        Self::finish(status, &text)
    }

    /// [`Self::get`] on the short-timeout lookup agent ([`LOOKUP_TIMEOUT`]): the transactions reads of
    /// a by-client-id resolution.
    pub fn get_lookup(
        &self,
        base: &str,
        path: &str,
        query: &str,
    ) -> Result<serde_json::Value, OandaApiError> {
        let (status, text) = self.get_raw(&self.lookup_agent, base, path, query)?;
        Self::finish(status, &text)
    }

    /// GET one order by specifier (`@{clientOrderID}` or a venue id): `Ok(None)` ONLY when OANDA
    /// answers 404 and says the ORDER does not exist ([`is_order_does_not_exist`]); any other
    /// non-2xx, and every transport failure, stays an `Err` (inconclusive, never "absent").
    pub fn get_order(
        &self,
        base: &str,
        path: &str,
    ) -> Result<Option<serde_json::Value>, OandaApiError> {
        let (status, text) = self.get_raw(&self.lookup_agent, base, path, "")?;
        if status == 404 && is_order_does_not_exist(&text) {
            return Ok(None);
        }
        Self::finish(status, &text).map(Some)
    }

    fn get_raw(
        &self,
        agent: &ureq::Agent,
        base: &str,
        path: &str,
        query: &str,
    ) -> Result<(u16, String), OandaApiError> {
        let url = if query.is_empty() {
            format!("{base}{path}")
        } else {
            format!("{base}{path}?{query}")
        };
        let mut resp = agent
            .get(&url)
            .header("Authorization", &self.bearer())
            .header("Accept-Datetime-Format", "UNIX")
            .call()
            .map_err(Self::net_err)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(Self::net_err)?;
        Ok((status, text))
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
            .map_err(Self::order_send_err)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(Self::order_read_err)?;
        Self::finish_order(status, &text)
    }

    /// PUT a JSON body (order REPLACE: `/orders/{specifier}` with the replacing order). A non-2xx
    /// answer is an `Err` carrying OANDA's `errorMessage` (`finish`), exactly as for a cancel.
    pub fn put_json(
        &self,
        base: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, OandaApiError> {
        let url = format!("{base}{path}");
        let payload = serde_json::to_string(body).map_err(Self::net_err)?;
        let mut resp = self
            .agent
            .put(&url)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2xx whose body does not parse says OANDA answered something: UNKNOWN, resolved by client
    /// id, never the terminal reject a plain `finish` error (status 200) would become.
    #[test]
    fn an_unparsable_2xx_order_reply_is_an_unknown_outcome() {
        let e = OandaRest::finish_order(201, "<html>").unwrap_err();
        assert_eq!(e.status, STATUS_OUTCOME_UNKNOWN);
        assert!(e.outcome_unknown());
        // OANDA's own refusal and a gateway page keep their statuses
        let refused = OandaRest::finish_order(400, r#"{"errorMessage":"bad"}"#).unwrap_err();
        assert!(!refused.outcome_unknown());
        assert!(OandaRest::finish_order(504, "<html>").unwrap_err().outcome_unknown());
        assert!(OandaRest::finish_order(201, "{}").is_ok());
    }

    /// The shared classifier decides: a failure PROVABLY before the write is a clean reject
    /// (`status 0`), a timeout may have followed the write.
    #[test]
    fn an_order_send_failure_before_the_write_is_definite_and_a_timeout_is_not() {
        let before = OandaRest::order_send_err(ureq::Error::HostNotFound);
        assert_eq!(before.status, 0);
        assert!(!before.outcome_unknown());
        let after = OandaRest::order_send_err(ureq::Error::Timeout(ureq::Timeout::Global));
        assert_eq!(after.status, STATUS_OUTCOME_UNKNOWN);
    }

    /// The lookup reads must not ride the 30 s global timeout.
    #[test]
    fn lookups_are_bounded_to_seconds() {
        assert!(LOOKUP_TIMEOUT <= std::time::Duration::from_secs(5));
    }
}
