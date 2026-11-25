//! Signed blocking REST transport for OKX V5: the shared signed JSON seam
//! (`vike_bridge_core::transport::json`'s `RestTransportJson`) and its live `RestTransportJsonUreq`, built here
//! ([`rest_transport`]) on okx's reply rules ([`OKX_REPLY`]), its log target and its headers: every request (signed
//! AND public) carries its Accept pair ([`REQUEST_HEADERS`]; the agent sends this client's own User-Agent), and
//! `x-simulated-trading: 1` when simulated (demo). GET appends the signed query VERBATIM from the signer (sign-then-send); POST sends the EXACT
//! signed body bytes. OKX returns HTTP 200 for business errors — the {code,msg,data} envelope is unwrapped by the
//! client ([`unwrap_okx`]), not here.
//!
//! Which network a transport talks to is the shared seam's answer (`RestTransportJson::network`): okx's one host
//! serves both and the simulated-trading header is the whole tier switch, so [`rest_transport`] puts the transport on
//! the test network and sends that header in one call. The account-mode gate reads it.

use std::time::Duration;

use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::json::RestTransportJsonUreq;
use vike_bridge_core::transport::reply::{
    RestReplyCode, RestReplyGatewayFirst, RestReplyNote, RestReplySpec,
};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_model::VenueNetwork;

/// okx's network switch on its one host: the DEMO (simulated-trading) network.
const SIMULATED_TRADING: (&str, &str) = ("x-simulated-trading", "1");

/// The headers every request carries, in the order they always went out: the Accept pair. The agent names this
/// client (`vike_transport::http::USER_AGENT`; measured 2026-11-16 from a datacenter box: OKX's Cloudflare front
/// answers it normally).
const REQUEST_HEADERS: [(&str, &str); 2] =
    [("Accept", "application/json, text/plain, */*"), ("Accept-Language", "en-US,en;q=0.9")];

/// okx's reply rules (`vike_bridge_core::transport::reply`): an error body's `code` (OKX sends it as a JSON
/// STRING; a present one that does not read, or reads as the HTTP status, is the status: **never `code: 0`**, the
/// workspace's definite PRE-SEND sentinel, because response headers arrived) and `msg`; a WRITE's gateway status
/// is "outcome unknown" before the body is read, while a read keeps its status (owner ruling B-3); a write's 5xx
/// with no venue code of its own is unknown (owner ruling B-1), and so is a write's 3xx (B-3: no agent follows a
/// redirect). The status-as-code range (100..600) is disjoint from OKX business codes (>= 10000), so the fallback
/// stays unambiguous downstream.
pub const OKX_REPLY: RestReplySpec = RestReplySpec {
    code: Some(RestReplyCode { field: "code", msg: "msg", string_ok: true }),
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
            tracing::warn!(target: "vike_okx::transport::client", error = %e, "REST request failed");
        }
        RestReplyNote::Refused { status, .. } => {
            tracing::warn!(target: "vike_okx::transport::client", status, "REST request failed");
        }
    }
}

/// The live okx transport on the network `simulated` names (`crate::execution::rest::simulated`, from the account's tier):
/// [`REQUEST_HEADERS`] on every request, and the simulated-trading header on the demo network. No rate gate (every
/// client that rides one attaches it, `with_rate_gate`); the re-query on its short-timeout agent, exempt from it.
pub fn rest_transport(simulated: bool) -> RestTransportJsonUreq {
    let transport =
        RestTransportJsonUreq::new("okx", &OKX_REPLY, note).with_headers(&REQUEST_HEADERS);
    if simulated {
        transport.with_network(VenueNetwork::Testnet, &[SIMULATED_TRADING])
    } else {
        transport
    }
}

/// **[`rest_transport`] on the venue's clock (owner ruling B-4)**: before its first signed call it reads okx's time
/// (`GET /api/v5/public/time` on the call's own host, with the simulated-trading header on demo, [`server_time`])
/// into `clock`, and every signer built on `clock.ms()` then stamps the venue's time, so a box clock off by more than
/// okx's 30 s window no longer draws `50102`. ONE clock per start (the adapter's) serves the exec thread's clients,
/// the reconcile client and both private sockets' logins; a one-shot client holds its own. A read that fails is warned
/// and the call signs on the offset held: the local clock until one answers, as before B-4.
pub fn rest_transport_on(simulated: bool, clock: &VenueClock) -> RestTransportJsonUreq {
    rest_transport(simulated).with_venue_clock(
        clock,
        crate::execution::rest::PATH_TIME,
        server_time,
    )
}

/// The venue's time in a `GET /api/v5/public/time` answer. ⚠ `data[0].ts` is epoch ms as a STRING inside an ARRAY
/// (`as_i64()` reads `None`), and an empty `data` array must not panic.
pub(crate) fn server_time(body: &serde_json::Value) -> Option<i64> {
    body.get("data")?.get(0)?.get("ts")?.as_str()?.parse().ok()
}

/// [`rest_transport`] whose ordinary agent (the one `signed` and `public` ride) gives up after `timeout` instead of
/// the venue default of 30 s: for a read that blocks a thread nothing else can run on (a foreign instrument's row,
/// on the exec thread or the user-data pump).
pub fn rest_transport_with_timeout(simulated: bool, timeout: Duration) -> RestTransportJsonUreq {
    rest_transport(simulated).with_timeout(timeout)
}

/// OKX two-level envelope unwrap (the client's `unwrap`): top-level code != "0" raises
/// (preferring a per-order `data[0].sCode` when present); success + `data[0].sCode != "0"`
/// ALSO raises (partial-batch per-order error). Returns `data` (a list).
///
/// A body with no usable `code`, or a success with no `data` list, is no answer at all: read as success
/// it accepted a submit with no order id and let the orders read see no open orders, so it raises the
/// ambiguous sentinel (a write may have landed; a read says nothing). A real empty `data: []` is a list.
pub fn unwrap_okx(response: serde_json::Value) -> Result<serde_json::Value, VenueApiError> {
    let code_str = match response.get("code") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => return Err(not_an_envelope("no code")),
    };
    let data = response.get("data").cloned().unwrap_or(serde_json::Value::Null);
    if code_str == "0" && !data.is_array() {
        return Err(not_an_envelope("no data list"));
    }
    let scode =
        data.as_array().and_then(|d| d.first()).and_then(|d0| d0.get("sCode")).map(|s| match s {
            serde_json::Value::String(x) => x.clone(),
            other => other.to_string(),
        });
    if code_str != "0" {
        if let Some(sc) = &scode
            && sc != "0"
        {
            return Err(per_order_error(&data, sc));
        }
        return Err(VenueApiError {
            code: code_str.parse::<i64>().unwrap_or(E_TIMEOUT_AMBIGUOUS),
            msg: response.get("msg").and_then(|m| m.as_str()).unwrap_or("").to_string(),
        });
    }
    if let Some(sc) = &scode
        && sc != "0"
    {
        return Err(per_order_error(&data, sc));
    }
    Ok(data)
}

fn not_an_envelope(what: &str) -> VenueApiError {
    VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: format!("the venue answered 2xx with {what}") }
}

fn per_order_error(data: &serde_json::Value, scode: &str) -> VenueApiError {
    VenueApiError {
        code: scode.parse::<i64>().unwrap_or(E_TIMEOUT_AMBIGUOUS),
        msg: data
            .as_array()
            .and_then(|d| d.first())
            .and_then(|d0| d0.get("sMsg"))
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

/// The non-2xx mapping okx always had, now [`OKX_REPLY`]'s, over a real reply: the venue's code (a string or a
/// number) wins; an absent, wrong-typed or unreadable one is the HTTP status, never the pre-send `0`.
#[cfg(test)]
mod http_error_mapping {
    use super::*;
    use std::io::{Read, Write};
    use vike_bridge_core::transport::json::RestTransportJson;

    /// `(code, msg)` the live transport answers a READ that came back `status` with `body`.
    fn mapped(status: u16, body: &str) -> (i64, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let reply = format!(
            "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request);
            socket.write_all(reply.as_bytes()).expect("reply");
        });
        let e = rest_transport(false).public(&base, "/x", &[]).expect_err("a refusal");
        peer.join().expect("peer");
        (e.code, e.msg)
    }

    #[test]
    fn venue_string_code_wins_over_the_status() {
        let got = mapped(429, r#"{"code": "50011", "msg": "Requests too frequent"}"#);
        assert_eq!(got, (50011, "Requests too frequent".into()));
    }

    #[test]
    fn venue_numeric_code_wins_over_the_status() {
        let got = mapped(400, r#"{"code": 51000, "msg": "Parameter error"}"#);
        assert_eq!(got, (51000, "Parameter error".into()));
    }

    /// A PRESENT but wrong-typed `code` falls back to the HTTP status like the absent case — never the
    /// pre-send `code: 0` sentinel (headers arrived, so "nothing left this host" is provably false; a `0`
    /// would classify a delivered request as safe-to-reject `Network`).
    #[test]
    fn wrong_typed_code_falls_back_to_the_status_not_the_pre_send_sentinel() {
        let got = mapped(400, r#"{"code": true, "msg": "upstream unhappy"}"#);
        assert_ne!(got.0, 0, "code 0 is the definite PRE-SEND sentinel — false post-headers");
        assert_eq!(got, (400, "upstream unhappy".into()));
    }

    /// Same, the non-numeric-string shape.
    #[test]
    fn non_numeric_string_code_falls_back_to_the_status() {
        let got = mapped(500, r#"{"code": "oops", "msg": "broken"}"#);
        assert_ne!(got.0, 0);
        assert_eq!(got, (500, "broken".into()));
    }

    /// Same, a numeric that does not fit an i64 (float): status, not 0.
    #[test]
    fn non_i64_number_code_falls_back_to_the_status() {
        assert_eq!(mapped(500, r#"{"code": 1.5, "msg": "x"}"#).0, 500);
    }

    #[test]
    fn absent_code_falls_back_to_the_status() {
        // No envelope: not okx's error, so its size alone (`vike_bridge_core::transport::body_summary`).
        let got = mapped(404, r#"{"note": "no okx envelope"}"#);
        assert_eq!(got, (404, "27-byte body not shown".into()));
    }

    #[test]
    fn unparseable_body_falls_back_to_the_status() {
        assert_eq!(mapped(500, "<html>bad gateway</html>"), (500, "http error".into()));
    }
}

#[cfg(test)]
mod rate_gate_wiring {
    use super::*;
    use crate::test_support::signer;
    use vike_bridge_core::ratelimit::RateGate;
    use vike_bridge_core::transport::json::RestTransportJson;

    // A closed localhost port: connection-refused is immediate (no DNS/network), so the send fails
    // fast — the assertion is purely on the GATE, consulted BEFORE the send.
    const DEAD: &str = "http://127.0.0.1:1";

    fn gated() -> (RateGate, RestTransportJsonUreq) {
        let gate = RateGate::new(1, Duration::from_secs(30));
        let t = rest_transport(false).with_rate_gate(gate.clone());
        (gate, t)
    }

    #[test]
    fn signed_consumes_the_gate() {
        let (gate, t) = gated();
        let _ = t.signed(DEAD, "/x", "GET", &[], &signer());
        assert!(!gate.try_proceed(), "signed took the gate's only slot");
    }

    #[test]
    fn signed_json_consumes_the_gate() {
        let (gate, t) = gated();
        let _ = t.signed_json(DEAD, "/x", "POST", "{}", &signer());
        assert!(!gate.try_proceed(), "signed_json took the gate's only slot");
    }

    #[test]
    fn public_consumes_the_gate() {
        let (gate, t) = gated();
        let _ = t.public(DEAD, "/x", &[]);
        assert!(!gate.try_proceed(), "public took the gate's only slot");
    }

    #[test]
    fn t1_requery_is_exempt_from_the_gate() {
        let (gate, t) = gated();
        let _ = t.signed_requery(DEAD, "/x", "GET", &[], &signer());
        assert!(gate.try_proceed(), "the T1 re-query must not ride the gate");
    }

    #[test]
    fn no_gate_configured_is_a_passthrough() {
        let t = rest_transport(false);
        let _ = t.public(DEAD, "/x", &[]);
    }

    /// What the live transport answers a `write` (a POST) or a read (a GET) that came back `status` with `body`.
    fn answer(write: bool, status: u16, body: &'static str) -> VenueApiError {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let reply = format!(
            "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request);
            socket.write_all(reply.as_bytes()).expect("reply");
        });
        let method = if write { "POST" } else { "GET" };
        let e =
            rest_transport(false).signed(&base, "/x", method, &[], &signer()).expect_err("no 2xx");
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

    /// The demo transport is on the test network and sends its header; [`REQUEST_HEADERS`] ride every request in
    /// their order, and none of them is a User-Agent (the agent's is this client's own).
    #[test]
    fn the_demo_network_is_the_header_it_sends() {
        assert_eq!(rest_transport(true).network(), VenueNetwork::Testnet);
        assert_eq!(rest_transport(false).network(), VenueNetwork::Mainnet);
        assert_eq!(rest_transport(false).headers(), &REQUEST_HEADERS[..]);
        assert!(REQUEST_HEADERS.iter().all(|(name, _)| !name.eq_ignore_ascii_case("user-agent")));
        let demo = rest_transport(true);
        assert_eq!(demo.headers()[..REQUEST_HEADERS.len()], REQUEST_HEADERS[..]);
        assert_eq!(demo.headers()[REQUEST_HEADERS.len()], SIMULATED_TRADING);
    }
}
