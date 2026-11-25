//! Signed blocking REST transport for Bybit V5: the shared signed JSON seam
//! (`vike_bridge_core::transport::json`'s `RestTransportJson`, stubbed with canned envelopes in offline tests) and its
//! live `RestTransportJsonUreq`, built here ([`rest_transport`]) on bybit's reply rules ([`BYBIT_REPLY`]) and its log
//! target. GET appends the signed query to the URL; POST sends the EXACT signed JSON body bytes with Content-Type:
//! application/json — never re-serialized, or X-BAPI-SIGN won't match. Bybit returns HTTP 200 for both success and
//! business errors; the {retCode,retMsg,result} envelope is unwrapped by the CLIENT's [`unwrap_envelope`] (raising on
//! retCode != 0), not here. Non-2xx bodies with a retCode become typed errors.

use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::json::RestTransportJsonUreq;
use vike_bridge_core::transport::reply::{
    RestReplyCode, RestReplyGatewayFirst, RestReplyNote, RestReplySpec,
};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};

/// Bybit's reply rules (`vike_bridge_core::transport::reply`): an error body's `retCode` (a JSON integer) and
/// `retMsg`; a WRITE's gateway status is "outcome unknown" before the body is read, while a read keeps its status
/// (owner ruling B-3); a write's 5xx with no venue code of its own is unknown (owner ruling B-1), and so is a write's
/// 3xx (B-3: no agent follows a redirect).
pub const BYBIT_REPLY: RestReplySpec = RestReplySpec {
    code: Some(RestReplyCode { field: "retCode", msg: "retMsg", string_ok: false }),
    no_verdict_on_5xx: &[],
    gateway_first: RestReplyGatewayFirst::OnWrites,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: true,
    words: None,
    unknown_body_head: 0,
    plain_text_errors: false,
    verdict_on_5xx: &[],
};

/// The two lines this transport has always written, under this venue's own compile-time `target`, which `RUST_LOG`
/// directives filter on.
fn note(note: RestReplyNote<'_>) {
    match note {
        RestReplyNote::SendFailed(e) => {
            tracing::warn!(target: "vike_bybit::transport::client", error = %e, "REST request failed");
        }
        RestReplyNote::Refused { status, .. } => {
            tracing::warn!(target: "vike_bybit::transport::client", status, "REST request failed");
        }
    }
}

/// The live bybit transport: no rate gate (every client that rides one attaches it, `with_rate_gate`), the
/// re-query on its short-timeout agent and exempt from any gate. Its signer stamps what it was built with
/// (the local clock in the manual smokes); a production client is built with [`rest_transport_on`].
pub fn rest_transport() -> RestTransportJsonUreq {
    RestTransportJsonUreq::new("bybit", &BYBIT_REPLY, note)
}

/// **[`rest_transport`] on the venue's clock (owner ruling B-4)**: before its first signed call it reads bybit's time
/// (`GET /v5/market/time` on the call's own host, [`server_time`]) into `clock`, and every signer built on
/// `clock.ms()` then stamps the venue's time, so a box clock off by more than the 5 s `recvWindow` no longer draws
/// `10002`. ONE clock per start (the adapter's) serves the exec thread's clients, the reconcile client and the private
/// socket's login; a one-shot client (a leverage write, an operator's read) holds its own. A read that fails is
/// warned and the call signs on the offset held: the local clock until one answers, as before B-4.
pub fn rest_transport_on(clock: &VenueClock) -> RestTransportJsonUreq {
    rest_transport().with_venue_clock(clock, crate::execution::rest::PATH_TIME, server_time)
}

/// The venue's time in a `GET /v5/market/time` answer: the TOP-LEVEL `"time"` NUMBER, in ms. ⚠ `result.timeNano` is
/// a NANOSECOND string and `result.timeSecond` a SECONDS string, so the wrong one is off by a factor of a million or a
/// thousand while still looking like a plausible integer.
pub(crate) fn server_time(body: &serde_json::Value) -> Option<i64> {
    body.get("time").and_then(serde_json::Value::as_i64)
}

/// Client-side envelope unwrap: retCode != 0 raises; success returns `result`. A body with NO `retCode`
/// is no answer at all: read as success it accepted a submit with no order id and let a read see nothing,
/// so it raises the ambiguous sentinel (a write may have landed; a read says nothing).
pub fn unwrap_envelope(response: serde_json::Value) -> Result<serde_json::Value, VenueApiError> {
    let Some(code) = response.get("retCode").and_then(|c| c.as_i64()) else {
        return Err(VenueApiError {
            code: E_TIMEOUT_AMBIGUOUS,
            msg: "the venue answered 2xx with no retCode".to_string(),
        });
    };
    if code != 0 {
        return Err(VenueApiError {
            code,
            msg: response.get("retMsg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        });
    }
    Ok(response.get("result").cloned().unwrap_or(serde_json::json!({})))
}

/// A request as a failed one is named: `bybit GET /v5/position/list (category linear, symbol BTCUSDT)`, the method,
/// the path and each parameter asked as its key and value (no parameter, no parentheses).
pub fn request_words(method: &str, path: &str, params: &[(&str, serde_json::Value)]) -> String {
    let asked: Vec<String> = params
        .iter()
        .map(|(key, value)| match value.as_str() {
            Some(text) => format!("{key} {text}"),
            None => format!("{key} {value}"),
        })
        .collect();
    if asked.is_empty() {
        format!("bybit {method} {path}")
    } else {
        format!("bybit {method} {path} ({})", asked.join(", "))
    }
}

/// **A bybit REST call that failed, said the ONE way every read and write of this crate says it**: the request
/// ([`request_words`]), then the venue's code and its message, or `(empty message)` when it sent none:
/// `bybit GET /v5/position/list (category linear, symbol BTCUSDT): venue error 10001 (empty message)`. The error was
/// the venue's message ALONE, and the demo host answers with an EMPTY `retMsg` (the CI box's v0.1.102 start warned
/// `fee-rate fetch failed … error=` with nothing after it), so such a failure said nothing. A code under 600 is the
/// HTTP status (the transport logs a non-2xx itself). The venue's error is kept whole for a caller that judges it.
#[derive(Debug)]
pub struct FailedRequest {
    /// [`request_words`].
    pub request: String,
    pub error: VenueApiError,
}

impl FailedRequest {
    pub fn new(
        method: &str,
        path: &str,
        params: &[(&str, serde_json::Value)],
        error: VenueApiError,
    ) -> Self {
        FailedRequest { request: request_words(method, path, params), error }
    }

    /// The error for a shared seam that says a [`VenueApiError`]'s `msg` alone (a cancel's and an amend's refusal, a
    /// confirm's failed re-query): the code kept, so it is judged as before, and the message these words.
    pub fn into_venue_error(self) -> VenueApiError {
        VenueApiError { code: self.error.code, msg: self.to_string() }
    }
}

impl std::fmt::Display for FailedRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let FailedRequest { request, error } = self;
        if error.msg.trim().is_empty() {
            write!(f, "{request}: venue error {} (empty message)", error.code)
        } else {
            write!(f, "{request}: venue error {}, message {:?}", error.code, error.msg)
        }
    }
}

#[cfg(test)]
mod rate_gate_wiring {
    use super::*;
    use crate::signer::BybitV5Signer;
    use std::time::Duration;
    use vike_bridge_core::ratelimit::RateGate;
    use vike_bridge_core::transport::json::RestTransportJson;
    use vike_secrets::credentials::Credentials;

    // A closed localhost port: connection-refused is immediate (no DNS/network), so the send fails
    // fast — the assertion is purely on the GATE, which is consulted BEFORE the send.
    const DEAD: &str = "http://127.0.0.1:1";

    fn signer() -> BybitV5Signer {
        let creds = Credentials {
            api_key: "k".to_string().into(),
            api_secret: "s".to_string().into(),
            passphrase: None,
        };
        BybitV5Signer::new(&creds, || 0)
    }

    #[test]
    fn signed_consumes_the_gate() {
        let gate = RateGate::new(1, Duration::from_secs(30));
        let t = rest_transport().with_rate_gate(gate.clone());
        let _ = t.signed(DEAD, "/x", "GET", &[], &signer());
        assert!(!gate.try_proceed(), "the signed call took the gate's only slot");
    }

    #[test]
    fn t1_requery_is_exempt_from_the_gate() {
        let gate = RateGate::new(1, Duration::from_secs(30));
        let t = rest_transport().with_rate_gate(gate.clone());
        let _ = t.signed_requery(DEAD, "/x", "GET", &[], &signer());
        assert!(gate.try_proceed(), "the T1 re-query must not ride the gate");
    }

    #[test]
    fn no_gate_configured_is_a_passthrough() {
        let t = rest_transport();
        let _ = t.signed(DEAD, "/x", "GET", &[], &signer());
    }

    /// What the live transport answers a `write` (a POST) or a read (a GET) that came back `status` with `body`.
    fn answer(write: bool, status: u16, body: &'static str) -> VenueApiError {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let reply = format!(
            "HTTP/1.1 {status} X
content-length: {}
connection: close

{body}",
            body.len()
        );
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request);
            socket.write_all(reply.as_bytes()).expect("reply");
        });
        let method = if write { "POST" } else { "GET" };
        let e = rest_transport().signed(&base, "/x", method, &[], &signer()).expect_err("no 2xx");
        peer.join().expect("peer");
        e
    }

    /// Owner ruling B-3: a READ keeps its gateway status (a retryable server error, not the re-query sentinel), a
    /// write's gateway answer is still "outcome unknown", and a write's 3xx is unknown too (no agent follows a
    /// redirect, so it is no verdict on the order) while a read keeps it as the status.
    #[test]
    fn a_read_keeps_its_gateway_status_and_a_writes_redirect_is_unknown() {
        assert_eq!(answer(false, 502, "").code, 502);
        assert_eq!(answer(true, 502, "").code, E_TIMEOUT_AMBIGUOUS);
        assert_eq!(answer(true, 302, "").code, E_TIMEOUT_AMBIGUOUS);
        assert_eq!(answer(false, 302, "").code, 302);
    }

    /// bybit sends no header of its own beyond the signer's: the transport adds none.
    #[test]
    fn the_live_transport_sends_no_venue_header() {
        assert!(rest_transport().headers().is_empty());
    }
}
