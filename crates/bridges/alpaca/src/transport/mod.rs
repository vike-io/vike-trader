//! Alpaca Bearer REST — wraps a shared [`TokenSource`], injecting `Authorization: Bearer` and
//! refreshing the token on a 401 (it is short-lived) through the shared ladder
//! (`vike_bridge_core::transport::auth`'s `TokenCell::reauth_once`): a read or a cancel is then retried ONCE, an
//! order write (POST, PATCH) never (owner ruling B-14), its 401 the answer; a re-mint that fails answers the
//! mint's own error. Shape copied from `crates/bridges/oanda/src/transport.rs`. ureq blocking (rustls), no OpenSSL.
//!
//! Its error is the ONE bearer-venue HTTP error,
//! `vike_bridge_core::transport::http_error::VenueApiHttpError` (`{status, message}`,
//! `alpaca HTTP {status}: {message}`), with this venue's facts in [`ALPACA_HTTP`]:
//! status `0` = provably never sent, `1` = the venue may have acted (an order POST or amend PATCH that failed
//! after the write, a reply whose body never came, any 5xx to a write), a gateway status unknown too.

pub(crate) mod sign;
pub(crate) mod urls;

use std::sync::Arc;

use vike_bridge_core::transport::auth::TokenReauthFailedLogin;
use vike_bridge_core::transport::http_error::{VenueApiHttpError, VenueApiHttpSpec};

use crate::transport::sign::{TokenSource, bearer_header};

/// The status that says the Bearer is dead.
const DEAD_TOKEN: u16 = 401;

/// Alpaca's HTTP error facts (the field docs are the rules). A 2xx with an empty body is `null` (a DELETE's
/// `204`); a never-sent failure carries the cause's words alone; nothing beyond the gateway set is unknown.
/// A 3xx on a write is unknown (owner ruling B-3): no agent here follows a redirect, so it is no verdict on
/// the order; so is any 5xx on a write (owner ruling B-1): nothing says Alpaca did not act before it failed;
/// and so is a write's 2xx whose body does not read (owner ruling B-2): Alpaca answered yes to something. A
/// read's unreadable 2xx keeps its status.
pub(crate) static ALPACA_HTTP: VenueApiHttpSpec = VenueApiHttpSpec {
    prefix: "alpaca HTTP",
    network_words: "",
    also_unknown: &[],
    empty_2xx_is_null: true,
    unreadable_2xx_on_write_unknown: true,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: true,
};

pub struct AlpacaTransport {
    agent: ureq::Agent,
    token: Arc<TokenSource>,
}

impl AlpacaTransport {
    pub fn new(token: Arc<TokenSource>) -> Self {
        Self { agent: vike_bridge_core::http::blocking_agent(), token }
    }

    /// A token error keeps its CLASS. A mint that ANSWERED and refused is an auth rejection (401);
    /// a mint that was never REACHED is a transport failure and wears the never-sent
    /// `status: 0`, like every other unreached socket in this file. Collapsing the two made a
    /// geo-blocked `authx` host render as `alpaca HTTP 401` — read as "bad keys", answered with a
    /// credential rotation, for a network condition.
    fn auth_err(e: crate::transport::sign::AuthError) -> VenueApiHttpError {
        let status = match e {
            crate::transport::sign::AuthError::Exchange(_) => 401,
            crate::transport::sign::AuthError::Transport(_) => 0,
        };
        ALPACA_HTTP.error(status, e.to_string())
    }

    /// One call through the 401 ladder: the token in force (refreshed first if it is due), `run` with its
    /// `Authorization` value, and on a 401 one re-mint whose failure answers the mint's own error. `retry`: then
    /// `run` ONCE more with the fresh token (`TokenCell::reauth_once`); without it (an order write, owner ruling
    /// B-14) the 401 is the answer and the next call carries the fresh token.
    fn ladder(
        &self,
        retry: bool,
        run: impl Fn(&str) -> Result<(u16, String), VenueApiHttpError>,
    ) -> Result<(u16, String), VenueApiHttpError> {
        self.token.bearer().map_err(Self::auth_err)?;
        let relogin = |seen| self.token.relogin(seen).map_err(Self::auth_err);
        if retry {
            return self.token.cell().reauth_once(
                DEAD_TOKEN,
                TokenReauthFailedLogin::AnswerLoginError,
                |token| run(&bearer_header(token)),
                relogin,
                |_| {},
            );
        }
        let (token, seen) = self.token.cell().current();
        let first = run(&bearer_header(&token))?;
        if first.0 == DEAD_TOKEN {
            relogin(seen)?;
        }
        Ok(first)
    }

    /// A status and its body, read, in the venue's words ([`Self::venue_words`]). A malformed 2xx body keeps
    /// its real 2xx status (not the never-sent `0`), so a caller telling a transport failure from a server
    /// response keeps the signal. `write`: the call may have changed the account ([`ALPACA_HTTP`]'s rules).
    fn finish(
        status: u16,
        text: &str,
        write: bool,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        ALPACA_HTTP.finish(status, text, write, Self::venue_words)
    }

    /// The venue's own words on a non-2xx: its `message`, then its `code` and every other field it sent (a
    /// 403's `buying_power`/`cost_basis` or `available`/`held_for_orders`), so a refusal says WHICH refusal it
    /// is: one 403 `code` covers buying power, held shares, wash trades and a forbidden request alike. A body
    /// that is no JSON object with a string `message` is carried trimmed, as it came.
    fn venue_words(text: &str) -> String {
        let Ok(serde_json::Value::Object(mut body)) = serde_json::from_str(text) else {
            return text.trim().to_string();
        };
        let Some(serde_json::Value::String(message)) = body.remove("message") else {
            return text.trim().to_string();
        };
        let code = body.remove("code").map(|c| format!("code {c}"));
        let rest = (!body.is_empty()).then(|| serde_json::Value::Object(body).to_string());
        let extra: Vec<String> = code.into_iter().chain(rest).collect();
        if extra.is_empty() { message } else { format!("{message} ({})", extra.join(", ")) }
    }

    // --- one-shot senders (given an explicit bearer) ---------------------------------------
    fn send_get(&self, url: &str, bearer: &str) -> Result<(u16, String), VenueApiHttpError> {
        let mut response = self
            .agent
            .get(url)
            .header("Authorization", bearer)
            .call()
            .map_err(|e| ALPACA_HTTP.network(e))?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().map_err(|e| ALPACA_HTTP.network(e))?;
        Ok((status, text))
    }
    fn send_body(
        &self,
        method: &str,
        url: &str,
        bearer: &str,
        body: &[u8],
    ) -> Result<(u16, String), VenueApiHttpError> {
        // ureq 3.x's typed `.delete()` builder carries no body (`RequestBuilder<WithoutBody>`,
        // unlike post/patch's `WithBody`), so DELETE (always an empty body for this client) runs
        // via `.call()` while POST/PATCH run via `.send(body)`.
        let mut response = match method {
            // An order POST or amend PATCH that failed after the write may have acted: `write_send_error` says so.
            "POST" => self
                .agent
                .post(url)
                .header("Authorization", bearer)
                .header("Content-Type", "application/json")
                .send(body)
                .map_err(|e| ALPACA_HTTP.write_send_error(&e))?,
            "PATCH" => self
                .agent
                .patch(url)
                .header("Authorization", bearer)
                .header("Content-Type", "application/json")
                .send(body)
                .map_err(|e| ALPACA_HTTP.write_send_error(&e))?,
            "DELETE" => self
                .agent
                .delete(url)
                .header("Authorization", bearer)
                .call()
                .map_err(|e| ALPACA_HTTP.network(e))?,
            _ => unreachable!("unsupported method {method}"),
        };
        let status = response.status().as_u16();
        // The headers arrived, so Alpaca received the request.
        let text =
            response.body_mut().read_to_string().map_err(|e| ALPACA_HTTP.write_read_error(e))?;
        Ok((status, text))
    }

    // --- verbs (refresh on a 401; retry once, but never an order write) ---------------------
    pub fn get(
        &self,
        base: &str,
        path: &str,
        query: &str,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let url = if query.is_empty() {
            format!("{base}{path}")
        } else {
            format!("{base}{path}?{query}")
        };
        let (status, text) = self.ladder(true, |bearer| self.send_get(&url, bearer))?;
        Self::finish(status, &text, false)
    }

    fn body_verb(
        &self,
        method: &str,
        base: &str,
        path: &str,
        body: &[u8],
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        let url = format!("{base}{path}");
        // Owner ruling B-14: an ORDER write (POST, PATCH) is never sent twice, since nothing measured says a 401
        // came before the venue acted. The 401 is its answer; the next call carries the fresh token. A cancel is
        // re-sent: a second DELETE cannot place anything.
        let retry = method == "DELETE";
        let (status, text) =
            self.ladder(retry, |bearer| self.send_body(method, &url, bearer, body))?;
        Self::finish(status, &text, true)
    }

    pub fn post_json(
        &self,
        base: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        self.body_verb("POST", base, path, body.to_string().as_bytes())
    }
    pub fn patch_json(
        &self,
        base: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, VenueApiHttpError> {
        self.body_verb("PATCH", base, path, body.to_string().as_bytes())
    }
    pub fn delete(&self, base: &str, path: &str) -> Result<serde_json::Value, VenueApiHttpError> {
        self.body_verb("DELETE", base, path, b"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::sign::AuthError;

    /// `classify_send_error` decides: a failure provably before the write is `status 0`, a timeout may have followed it,
    /// and a gateway status says nothing about what Alpaca did, nor does any 5xx to a write (owner ruling B-1). The
    /// venue's own refusal stays what it was, and a read's 5xx keeps its status.
    #[test]
    fn only_a_failure_after_the_write_or_a_5xx_to_a_write_is_an_unknown_outcome() {
        let before = ALPACA_HTTP.write_send_error(&ureq::Error::HostNotFound);
        assert_eq!(before.status, 0);
        assert!(!before.outcome_unknown());
        let after = ALPACA_HTTP.write_send_error(&ureq::Error::Timeout(ureq::Timeout::Global));
        assert!(after.outcome_unknown());
        assert!(after.message.starts_with("network error: "), "{after}");
        assert!(AlpacaTransport::finish(504, "<html>", true).unwrap_err().outcome_unknown());
        let refused = AlpacaTransport::finish(422, r#"{"message":"bad"}"#, true).unwrap_err();
        assert!(!refused.outcome_unknown());
        let bare_500 = AlpacaTransport::finish(500, "boom", true).unwrap_err();
        assert_eq!(bare_500.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN, "{bare_500}");
        assert_eq!(bare_500.message, "http 500, outcome unknown: boom");
        assert!(bare_500.outcome_unknown());
        let read_500 = AlpacaTransport::finish(500, "boom", false).unwrap_err();
        assert_eq!(read_500.status, 500, "a read has no outcome to be unsure of");
        assert!(!read_500.outcome_unknown());
    }

    #[test]
    fn finish_parses_2xx_and_errors() {
        for write in [false, true] {
            assert_eq!(AlpacaTransport::finish(200, r#"{"id":"x"}"#, write).unwrap()["id"], "x");
            assert!(AlpacaTransport::finish(200, "", write).unwrap().is_null()); // empty 2xx body → Null
            let e = AlpacaTransport::finish(422, r#"{"code":40010001,"message":"bad ssn"}"#, write)
                .unwrap_err();
            assert_eq!(e.status, 422);
            assert_eq!(e.message, "bad ssn (code 40010001)");
            let e2 =
                AlpacaTransport::finish(401, r#"{"message":"unauthorized."}"#, write).unwrap_err();
            assert_eq!(e2.message, "unauthorized.");
            // A malformed 2xx body is never the never-sent `0`: a read keeps the real status, and a write's is
            // "outcome unknown" (owner ruling B-2), since Alpaca answered yes to something and an order may rest.
            let e3 = AlpacaTransport::finish(200, "not json", write).unwrap_err();
            let want = if write { VenueApiHttpError::STATUS_OUTCOME_UNKNOWN } else { 200 };
            assert_eq!(e3.status, want, "write {write}: {e3}");
            assert_eq!(e3.outcome_unknown(), write, "write {write}: {e3}");
            assert!(e3.message.starts_with("bad json"));
        }
    }

    /// **A venue refusal carries the venue's whole body, not its `message` alone**, and the `Display` is what the
    /// exec loop's `order submit failed` WARN and the `OrderRejected` reason both print. Measured on the CI box
    /// 2026-11-10: every order on the alpaca demo account came back `alpaca HTTP 403: request is forbidden`, the
    /// `code` dropped, and the 403s that name buying power or held shares send their numbers beside it.
    #[test]
    fn a_403_carries_the_venues_code_and_every_field_it_sent() {
        let e = AlpacaTransport::finish(
            403,
            r#"{"code":40310000,"message":"request is forbidden"}"#,
            true,
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "alpaca HTTP 403: request is forbidden (code 40310000)");
        let e = AlpacaTransport::finish(
            403,
            r#"{"buying_power":"0","code":40310000,"cost_basis":"192.88","message":"insufficient buying power"}"#,
            true,
        )
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            r#"alpaca HTTP 403: insufficient buying power (code 40310000, {"buying_power":"0","cost_basis":"192.88"})"#
        );
        // No `message`: the body as it came.
        assert_eq!(
            AlpacaTransport::finish(403, " forbidden \n", true).unwrap_err().message,
            "forbidden"
        );
        assert_eq!(
            AlpacaTransport::finish(403, r#"{"code":40310000}"#, true).unwrap_err().message,
            r#"{"code":40310000}"#
        );
        assert!(!e.outcome_unknown(), "a venue refusal is a refusal");
    }

    /// **A 3xx on a write is "outcome unknown"** (owner ruling B-3): this agent follows no redirect, so the
    /// 3xx is the whole answer and says nothing of the order, which stays `Submitted` rather than rejected.
    /// A read keeps its status, and the `Location` is never dialled.
    #[test]
    fn a_redirect_on_a_write_is_an_unknown_outcome_and_a_read_keeps_its_status() {
        let (origin, target_hits) = vike_log::loopback::redirecting_origin();
        let token = TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(|_| Ok(("t".to_string(), 900))),
        );
        let rest = AlpacaTransport::new(std::sync::Arc::new(token));
        let write = rest.post_json(&origin, "/v1/orders", &serde_json::json!({})).unwrap_err();
        assert!(write.outcome_unknown(), "{write}");
        assert!(write.message.starts_with("http 302, outcome unknown"), "{write}");
        let read = rest.get(&origin, "/v1/assets/AAPL", "").unwrap_err();
        assert_eq!(read.status, 302, "{read}");
        assert!(!read.outcome_unknown(), "{read}");
        assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "a redirect followed");
    }

    /// A loopback venue that reads EVERY request whole (head and declared body, so no unread byte resets the
    /// reply), then answers it `401` (`answer_401`) or closes the socket with no answer; and the request lines it saw.
    fn venue(answer_401: bool) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&seen);
        let serve = move || {
            for mut conn in listener.incoming().flatten() {
                let (mut buffer, mut chunk) = (Vec::new(), [0_u8; 4096]);
                let head_end = loop {
                    let n = conn.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break None;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    if let Some(p) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(p + 4);
                    }
                };
                let Some(head_end) = head_end else { continue };
                let head = String::from_utf8_lossy(&buffer[..head_end]).to_ascii_lowercase();
                let want = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:")?.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while buffer.len() < head_end + want {
                    let n = conn.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                log.lock().unwrap().push(head.lines().next().unwrap_or("").to_string());
                if !answer_401 {
                    continue;
                }
                let body = r#"{"message":"unauthorized."}"#;
                let _ = write!(
                    conn,
                    "HTTP/1.1 401 X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        };
        std::thread::Builder::new().name("alpaca-401".into()).spawn(serve).expect("spawn");
        (base, seen)
    }

    /// **An order write is never sent twice after a 401** (owner ruling B-14: a re-send could place a
    /// second order). The token is refreshed for the next call and the 401 is the answer; a cancel and a
    /// read still retry once with the fresh token.
    #[test]
    fn a_401_re_sends_a_read_or_a_cancel_and_never_an_order_write() {
        let mints = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&mints);
        let token = TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(move |_| {
                let n = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok((format!("t{n}"), 900))
            }),
        );
        let rest = AlpacaTransport::new(std::sync::Arc::new(token));
        let (base, seen) = venue(true);
        let count =
            |method: &str| seen.lock().unwrap().iter().filter(|l| l.starts_with(method)).count();
        let order =
            rest.post_json(&base, "/v1/orders", &serde_json::json!({"qty": "1"})).unwrap_err();
        assert_eq!((order.status, order.message.as_str()), (401, "unauthorized."));
        assert_eq!(count("post"), 1, "the order was sent twice: {:?}", seen.lock().unwrap());
        assert_eq!(mints.load(std::sync::atomic::Ordering::SeqCst), 2, "the token was refreshed");
        assert!(rest.patch_json(&base, "/v1/orders/x", &serde_json::json!({})).is_err());
        assert_eq!(count("patch"), 1);
        assert!(rest.delete(&base, "/v1/orders/x").is_err());
        assert_eq!(count("delete"), 2, "a cancel is retried once");
        assert!(rest.get(&base, "/v1/account", "").is_err());
        assert_eq!(count("get"), 2, "a read is retried once");
    }

    /// **An amend (PATCH) whose reply never came may have changed the order** (owner ruling T-D3): "outcome
    /// unknown", as the order POST says and as oanda's and ig's amends do, never the never-sent `0`.
    #[test]
    fn an_amend_whose_reply_was_lost_is_an_unknown_outcome_like_the_order_post() {
        let token = TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(|_| Ok(("t".to_string(), 900))),
        );
        let rest = AlpacaTransport::new(std::sync::Arc::new(token));
        let (base, seen) = venue(false);
        let order = rest.post_json(&base, "/v1/orders", &serde_json::json!({})).unwrap_err();
        assert!(order.outcome_unknown(), "{order}");
        let amend = rest.patch_json(&base, "/v1/orders/x", &serde_json::json!({})).unwrap_err();
        assert!(amend.outcome_unknown(), "{amend}");
        assert!(amend.message.starts_with("network error: "), "{amend}");
        let lines = seen.lock().unwrap().clone();
        assert_eq!(lines.len(), 2, "each was sent once: {lines:?}");
    }

    /// A 401 whose re-mint fails answers the MINT's error (its class: a mint never reached is the never-sent
    /// `0`), not the 401, and sends nothing again, whatever the verb.
    #[test]
    fn a_401_whose_re_mint_fails_answers_the_mints_own_error() {
        let mints = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&mints);
        let token = TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(move |_| match counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => Ok(("t0".to_string(), 900)),
                _ => Err(AuthError::Transport("io: Connection refused".into())),
            }),
        );
        let rest = AlpacaTransport::new(std::sync::Arc::new(token));
        let (base, seen) = venue(true);
        let read = rest.get(&base, "/v1/account", "").unwrap_err();
        assert_eq!(read.status, 0, "{read}");
        let order = rest.post_json(&base, "/v1/orders", &serde_json::json!({})).unwrap_err();
        assert_eq!(order.status, 0, "{order}");
        assert_eq!(seen.lock().unwrap().len(), 2, "nothing is sent again: {:?}", seen.lock());
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
        let rest = AlpacaTransport::new(std::sync::Arc::new(token));
        // `get` asks for a bearer FIRST, so this fails before any socket is opened.
        let e = rest.get("https://broker.invalid", "/v1/accounts", "").unwrap_err();
        assert_eq!(e.status, 0, "a transport failure keeps the never-sent 0, not 401: {e}");
        assert!(e.to_string().contains("Connection refused"), "the cause survives: {e}");
    }

    /// …and the OTHER half: a mint that ANSWERED with a refusal is still an auth rejection.
    #[test]
    fn a_refused_token_exchange_is_still_reported_as_an_auth_rejection() {
        let token = TokenSource::with_clock_and_exchanger(
            Box::new(|| 0),
            Box::new(|_| Err(AuthError::Exchange("HTTP 401".into()))),
        );
        let rest = AlpacaTransport::new(std::sync::Arc::new(token));
        let e = rest.get("https://broker.invalid", "/v1/accounts", "").unwrap_err();
        assert_eq!(e.status, 401, "the mint answered and refused: {e}");
    }
}
