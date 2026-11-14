//! Thin blocking Bearer-token REST client for OANDA v20 (rustls via ureq — no OpenSSL).
//!
//! OANDA does not sign requests (unlike the crypto HMAC venues), so this is a small standalone
//! client rather than an impl of the Binance-shaped [`RestTransport`](vike_bridge_core::transport::RestTransport). All
//! requests carry `Accept-Datetime-Format: UNIX` so timestamps arrive as epoch-seconds strings
//! (no RFC3339 date parsing needed). Non-2xx bodies carry OANDA's `errorMessage` /
//! `orderRejectTransaction`, so 4xx responses are returned parsed (not errored) for POSTs.
//!
//! **Its error is the ONE HTTP API error of the bearer-token venues**, `VenueApiHttpError` `{status, message}`
//! (`crates/vike-bridge-core/src/transport/http_error.rs` is its contract), over OANDA's facts, [`OANDA_HTTP`]: the
//! `OANDA error {status}: {message}` words, a status `0` for a request that provably never left this host, and
//! `VenueApiHttpError::STATUS_OUTCOME_UNKNOWN` (`1`) for one OANDA may have acted on. `outcome_unknown` is that `1`, a
//! gateway status (a proxy in front of OANDA says nothing about what OANDA did) or Cloudflare's `520`: a submit that
//! sees it re-queries by order client id and NEVER emits a terminal reject.

use vike_bridge_core::transport::http_error::{VenueApiHttpError, VenueApiHttpSpec, field_or_text};

/// OANDA's facts for the one HTTP API error (`VenueApiHttpSpec`'s doc is each field's).
///
/// * `unreadable_2xx_on_write_unknown`: an ORDER reply (a submit, a replace) whose 2xx body does not parse says OANDA
///   ANSWERED yes to something, so it is an UNKNOWN outcome (resolve by client id), never a terminal reject; a read's
///   keeps its 2xx status.
/// * `server_error_on_write_unknown`: owner ruling B-1 (2026-11-11), any 5xx to an order (a submit, a replace) is
///   "outcome unknown", resolved by client id like the gateway set and [`CLOUDFLARE_UNKNOWN_ERROR`]: nothing says OANDA
///   did not act before it failed. A read keeps its status.
/// * `redirect_on_write_unknown`: owner ruling B-3 (2026-11-11), a 3xx to an order is "outcome unknown". The agent
///   follows no redirect (`vike_bridge_core::http::blocking_agent`), so the 3xx is the answer and says nothing about
///   the order: a submit resolves it by client id and a replace reads the order back, where a 3xx used to be a terminal
///   reject over an order OANDA may hold. A read keeps its 3xx status.
pub(crate) static OANDA_HTTP: VenueApiHttpSpec = VenueApiHttpSpec {
    prefix: "OANDA error",
    network_words: "network error: ",
    also_unknown: &[CLOUDFLARE_UNKNOWN_ERROR],
    empty_2xx_is_null: false,
    unreadable_2xx_on_write_unknown: true,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: true,
};

/// Cloudflare's "the origin answered something unexpected": OANDA's hosts sit behind Cloudflare and were measured answering
/// it on 2026-10-18 (the practice stream hosts' trap in `crates/bridges/oanda/CLAUDE.md`), and as a terminal reject it could
/// stand over an order OANDA took.
const CLOUDFLARE_UNKNOWN_ERROR: u16 = 520;

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
pub struct OandaTransport {
    agent: ureq::Agent,
    /// The agent of the by-client-id LOOKUP reads ([`LOOKUP_TIMEOUT`] instead of the 30 s global one),
    /// so a lookup ladder is bounded by seconds and never holds the exec thread behind it for minutes.
    lookup_agent: ureq::Agent,
    token: String,
}

/// The global timeout of one lookup read (`get_order`, `get_lookup`). The steps of one resolution
/// share `crate::execution`'s `RESOLVE_BUDGET`; this bounds the ONE read in flight when it expires.
pub(crate) const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl OandaTransport {
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
    fn finish(status: u16, text: &str) -> Result<serde_json::Value, VenueApiHttpError> {
        OANDA_HTTP.finish(status, text, false, |body| field_or_text(body, "errorMessage"))
    }

    /// [`Self::finish`] for an ORDER reply: a 2xx whose body does not parse says OANDA ANSWERED yes
    /// to something, so it is an UNKNOWN outcome (resolve by client id), never a terminal reject.
    fn finish_order(status: u16, text: &str) -> Result<serde_json::Value, VenueApiHttpError> {
        OANDA_HTTP.finish(status, text, true, |body| field_or_text(body, "errorMessage"))
    }

    fn net_err(e: impl std::fmt::Display) -> VenueApiHttpError {
        OANDA_HTTP.network(e)
    }

    /// A send failure of an ORDER request: ambiguous (`VenueApiHttpError::STATUS_OUTCOME_UNKNOWN`) unless it
    /// provably preceded the write (`vike_bridge_core::transport::classify_send_error` is the one rule).
    fn order_send_err(e: ureq::Error) -> VenueApiHttpError {
        OANDA_HTTP.write_send_error(&e)
    }

    /// A body read failure on an ORDER reply: the headers arrived, so OANDA received the request.
    fn order_read_err(e: ureq::Error) -> VenueApiHttpError {
        OANDA_HTTP.write_read_error(e)
    }

    /// GET a v3 endpoint. `query` is a pre-formatted `"k=v&k=v"` string (may be empty).
    pub fn get(
        &self,
        base: &str,
        path: &str,
        query: &str,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
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
    ) -> Result<serde_json::Value, VenueApiHttpError> {
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
    ) -> Result<Option<serde_json::Value>, VenueApiHttpError> {
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
    ) -> Result<(u16, String), VenueApiHttpError> {
        let url = if query.is_empty() {
            format!("{base}{path}")
        } else {
            format!("{base}{path}?{query}")
        };
        let mut response = agent
            .get(&url)
            .header("Authorization", &self.bearer())
            .header("Accept-Datetime-Format", "UNIX")
            .call()
            .map_err(Self::net_err)?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(Self::net_err)?;
        Ok((status, text))
    }

    /// POST a JSON body (order placement). Returns the parsed response even on 4xx so the caller
    /// can read `orderRejectTransaction`.
    pub fn post_json(
        &self,
        base: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let url = format!("{base}{path}");
        let payload = serde_json::to_string(body).map_err(Self::net_err)?;
        let mut response = self
            .agent
            .post(&url)
            .header("Authorization", &self.bearer())
            .header("Accept-Datetime-Format", "UNIX")
            .header("Content-Type", "application/json")
            .send(payload.as_bytes())
            .map_err(Self::order_send_err)?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(Self::order_read_err)?;
        Self::finish_order(status, &text)
    }

    /// PUT a JSON body (order REPLACE: `/orders/{specifier}` with the replacing order). A non-2xx
    /// answer is an `Err` carrying OANDA's `errorMessage` (`finish`), exactly as for a cancel. A failure after
    /// the write reads as an order POST's does ([`VenueApiHttpError::outcome_unknown`]): OANDA may have replaced the order.
    pub fn put_json(
        &self,
        base: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let url = format!("{base}{path}");
        let payload = serde_json::to_string(body).map_err(Self::net_err)?;
        let mut response = self
            .agent
            .put(&url)
            .header("Authorization", &self.bearer())
            .header("Accept-Datetime-Format", "UNIX")
            .header("Content-Type", "application/json")
            .send(payload.as_bytes())
            .map_err(Self::order_send_err)?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(Self::order_read_err)?;
        Self::finish_order(status, &text)
    }

    /// PUT with no body (order cancel: `/orders/{id}/cancel`).
    pub fn put_empty(
        &self,
        base: &str,
        path: &str,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let url = format!("{base}{path}");
        let mut response = self
            .agent
            .put(&url)
            .header("Authorization", &self.bearer())
            .header("Accept-Datetime-Format", "UNIX")
            .send_empty()
            .map_err(Self::net_err)?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(Self::order_read_err)?;
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
        let e = OandaTransport::finish_order(201, "<html>").unwrap_err();
        assert_eq!(e.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN);
        assert!(e.outcome_unknown());
        // OANDA's own refusal and a gateway page keep their statuses
        let refused = OandaTransport::finish_order(400, r#"{"errorMessage":"bad"}"#).unwrap_err();
        assert!(!refused.outcome_unknown());
        assert!(OandaTransport::finish_order(504, "<html>").unwrap_err().outcome_unknown());
        assert!(OandaTransport::finish_order(201, "{}").is_ok());
    }

    /// The shared classifier decides: a failure PROVABLY before the write is a clean reject
    /// (`status 0`), a timeout may have followed the write.
    #[test]
    fn an_order_send_failure_before_the_write_is_definite_and_a_timeout_is_not() {
        let before = OandaTransport::order_send_err(ureq::Error::HostNotFound);
        assert_eq!(before.status, 0);
        assert!(!before.outcome_unknown());
        let after = OandaTransport::order_send_err(ureq::Error::Timeout(ureq::Timeout::Global));
        assert_eq!(after.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN);
    }

    /// The words and the statuses are OANDA's own, byte for byte as the venue's own error type wrote them: the
    /// `OANDA error` prefix, the `network error: ` lead of a request that never left, OANDA's `errorMessage`,
    /// Cloudflare's `520` read as unknown, and a read's unparsable 2xx keeping its status.
    #[test]
    fn the_error_keeps_oandas_words_and_statuses() {
        let refused =
            OandaTransport::finish(404, r#"{"errorMessage":"Order does not exist"}"#).unwrap_err();
        assert_eq!(refused.to_string(), "OANDA error 404: Order does not exist");
        assert_eq!(OandaTransport::finish(500, "<html>").unwrap_err().message, "<html>");
        assert_eq!(
            OandaTransport::net_err("refused").to_string(),
            "OANDA error 0: network error: refused"
        );
        assert!(OandaTransport::finish(520, "").unwrap_err().outcome_unknown());
        let bare_500 = OandaTransport::finish_order(500, r#"{"errorMessage":"boom"}"#).unwrap_err();
        assert_eq!(bare_500.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN, "owner ruling B-1");
        assert_eq!(bare_500.message, "http 500, outcome unknown: boom");
        let read = OandaTransport::finish(200, "<html>").unwrap_err();
        assert_eq!(read.status, 200);
        assert!(read.message.starts_with("bad json: "), "{read}");
    }

    /// Owner ruling B-3: a 3xx to an ORDER is "outcome unknown" (the agent follows no redirect, so OANDA's act is
    /// unknown), resolved by client id rather than rejected; a read's 3xx keeps its status.
    #[test]
    fn a_redirect_on_an_order_write_is_an_unknown_outcome_and_on_a_read_is_not() {
        let order = OandaTransport::finish_order(302, "moved").unwrap_err();
        assert_eq!(order.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN);
        assert!(order.outcome_unknown());
        assert_eq!(order.message, "http 302, outcome unknown: moved");
        let read = OandaTransport::finish(302, "moved").unwrap_err();
        assert_eq!(read.status, 302);
        assert!(!read.outcome_unknown());
    }

    /// A replace whose reply was lost after the write read as `status 0`, "provably never sent", so its refusal said the
    /// order keeps its terms while the venue may have replaced it.
    #[test]
    fn a_replace_whose_reply_is_lost_is_an_unknown_outcome() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let base = format!("http://{}", listener.local_addr().expect("resolve assigned port"));
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("the PUT arrives");
            let mut chunk = [0_u8; 1024];
            let _ = std::io::Read::read(&mut sock, &mut chunk);
        });
        let e = OandaTransport::new("t".to_string())
            .put_json(&base, "/v3/x", &serde_json::json!({}))
            .expect_err("the socket closes with no answer");
        server.join().unwrap();
        assert!(e.outcome_unknown(), "{e}");
    }

    /// A cancel whose reply headers arrived and whose body broke off read as `status 0`, "provably never sent", while
    /// OANDA had received it (T-D5): the outcome is unknown, as alpaca's DELETE says.
    #[test]
    fn a_cancel_whose_reply_body_breaks_off_is_an_unknown_outcome() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let base = format!("http://{}", listener.local_addr().expect("resolve assigned port"));
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("the PUT arrives");
            let (mut head, mut chunk) = (Vec::new(), [0_u8; 1024]);
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = sock.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                head.extend_from_slice(&chunk[..n]);
            }
            // A body shorter than its declared length, then the close.
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n{\"orderCancel");
        });
        let e = OandaTransport::new("t".to_string())
            .put_empty(&base, "/v3/accounts/A/orders/1/cancel")
            .expect_err("the body breaks off");
        server.join().unwrap();
        assert!(e.outcome_unknown(), "{e}");
        assert!(e.message.starts_with("network error reading the reply"), "{e}");
    }

    /// The lookup reads must not ride the 30 s global timeout.
    #[test]
    fn lookups_are_bounded_to_seconds() {
        assert!(LOOKUP_TIMEOUT <= std::time::Duration::from_secs(5));
    }
}
