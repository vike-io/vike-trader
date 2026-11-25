//! `RestTransportJsonUreq` end to end against a one-shot peer that records the request: the URL, the headers in order,
//! the exact body, the gate and the venue's note. The reply classification itself is `reply_tests.rs`'s.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::signer::PreparedRequest;

const SPEC: RestReplySpec = RestReplySpec {
    code: Some(super::super::reply::RestReplyCode {
        field: "retCode",
        msg: "retMsg",
        string_ok: false,
    }),
    no_verdict_on_5xx: &[],
    gateway_first: super::super::reply::RestReplyGatewayFirst::OnEveryCall,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: false,
    words: None,
    unknown_body_head: 0,
    plain_text_errors: false,
    verdict_on_5xx: &[],
};

static NOTES: AtomicUsize = AtomicUsize::new(0);

fn count(_: RestReplyNote<'_>) {
    NOTES.fetch_add(1, Ordering::SeqCst);
}

/// Signs nothing real: a fixed query and header, the body it is given (the pairs as compact JSON on a POST).
struct Stamp;
impl SignerJson for Stamp {
    fn prepare(&self, params: &[(&str, Value)], method: &str, _path: &str) -> PreparedRequest {
        let get = method.eq_ignore_ascii_case("GET");
        PreparedRequest {
            query: if get { "q=1".into() } else { String::new() },
            body: (!get).then(|| crate::signer::compact_json(params).into_bytes()),
            headers: vec![("x-sign".into(), "s".into())],
        }
    }
    fn prepare_body(&self, body: &str, _method: &str, _path: &str) -> PreparedRequest {
        PreparedRequest {
            query: String::new(),
            body: Some(body.into()),
            headers: vec![("x-sign".into(), "s".into())],
        }
    }
}

const OK: &str = "HTTP/1.1 200 OK\r\ncontent-length: 8\r\nconnection: close\r\n\r\n{\"ok\":1}";

/// A peer that records one request (head and `content-length` body, lowercased) and answers `200 {"ok":1}`.
fn recorder() -> (String, mpsc::Receiver<String>) {
    peer(vec![OK.to_string()])
}

/// A peer that serves `answers` in turn, one connection each, recording each request as [`recorder`] does.
fn peer(answers: Vec<String>) -> (String, mpsc::Receiver<String>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for answer in answers {
            let Ok((mut s, _)) = l.accept() else { return };
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut got = Vec::new();
            let mut buffer = [0_u8; 4096];
            while let Ok(n) = s.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&got).to_ascii_lowercase();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&got).to_ascii_lowercase());
            let _ = s.write_all(answer.as_bytes());
        }
    });
    (format!("http://{addr}"), rx)
}

fn ok_json(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
}

const AN_HOUR_MS: i64 = 3_600_000;

fn clocked(clock: &crate::signer::VenueClock) -> RestTransportJsonUreq {
    transport().with_venue_clock(clock, "/time", |b| b.get("time")?.as_i64())
}

/// A signer that stamps `x-bapi-timestamp` from the clock it is built on, as a venue signer does (bybit's, in its
/// bridge, puts the same header on the same clock).
struct ClockStamp<F>(F);
impl<F: Fn() -> i64 + Send> SignerJson for ClockStamp<F> {
    fn prepare(&self, _params: &[(&str, Value)], _method: &str, _path: &str) -> PreparedRequest {
        let headers = vec![("x-bapi-timestamp".into(), (self.0)().to_string())];
        PreparedRequest { headers, ..PreparedRequest::default() }
    }
    fn prepare_body(&self, body: &str, method: &str, path: &str) -> PreparedRequest {
        PreparedRequest { body: Some(body.into()), ..self.prepare(&[], method, path) }
    }
}

/// B-4: the first signed call reads the venue's clock once, keyless, and the signer built on that clock stamps the
/// venue's time; the next signed call reads nothing.
#[test]
fn a_signed_call_reads_the_venue_clock_once_and_signs_on_it() {
    let clock = crate::signer::VenueClock::default();
    let signer = ClockStamp(clock.ms());
    let ahead = format!(r#"{{"retCode":0,"time":{}}}"#, vike_model::now_ms() + AN_HOUR_MS);
    let (base, seen) = peer(vec![ok_json(&ahead), OK.to_string(), OK.to_string()]);
    let t = clocked(&clock);
    t.signed(&base, "/v5/x", "GET", &[], &signer).unwrap();
    let read = seen.recv().unwrap();
    assert!(
        read.starts_with("get /time http/1.1\r\n") && !read.contains("x-bapi-timestamp"),
        "{read}"
    );
    let call = seen.recv().unwrap();
    let stamp: i64 = call
        .lines()
        .find_map(|l| l.strip_prefix("x-bapi-timestamp: "))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("no stamp in {call}"));
    assert!((stamp - vike_model::now_ms() - AN_HOUR_MS).abs() < 60_000, "{stamp}");
    t.signed_json(&base, "/v5/y", "POST", "[]", &signer).unwrap();
    assert!(seen.recv().unwrap().starts_with("post /v5/y http/1.1\r\n"), "no second clock read");
}

/// A host whose clock does not answer: the call still goes out, on the local clock.
#[test]
fn a_failed_clock_read_signs_on_the_local_clock() {
    let clock = crate::signer::VenueClock::default();
    let (base, seen) = peer(vec![ok_json(r#"{"retCode":0}"#), OK.to_string()]);
    clocked(&clock).signed_requery(&base, "/v5/x", "GET", &[], &Stamp).unwrap();
    assert!(seen.recv().unwrap().starts_with("get /time "));
    assert!(seen.recv().unwrap().starts_with("get /v5/x?q=1 "));
    assert_eq!(clock.ms_from(|| 0)(), 0, "the offset is the local clock's");
}

fn transport() -> RestTransportJsonUreq {
    RestTransportJsonUreq::new("test", &SPEC, count).with_headers(&[("x-extra", "1")])
}

fn at(request: &str, needle: &str) -> usize {
    request.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {request}"))
}

#[test]
fn a_get_carries_the_signed_query_and_every_header() {
    let (base, seen) = recorder();
    let got = transport().signed(&base, "/v5/x", "GET", &[], &Stamp).unwrap();
    assert_eq!(got["ok"], 1);
    let request = seen.recv().unwrap();
    assert!(request.starts_with("get /v5/x?q=1 http/1.1\r\n"), "{request}");
    assert!(at(&request, "x-extra: 1") < at(&request, "x-sign: s"), "{request}");
    assert!(!request.contains("content-type"), "{request}");
}

#[test]
fn a_post_carries_the_exact_signed_body_after_the_content_type() {
    let (base, seen) = recorder();
    transport()
        .signed(&base, "/v5/o", "POST", &[("qty", serde_json::json!("0.01"))], &Stamp)
        .unwrap();
    let request = seen.recv().unwrap();
    assert!(request.starts_with("post /v5/o http/1.1\r\n"), "{request}");
    let (extra, kind, sign) = (
        at(&request, "x-extra: 1"),
        at(&request, "content-type: application/json"),
        at(&request, "x-sign: s"),
    );
    assert!(extra < kind && kind < sign, "{request}");
    assert!(request.ends_with("\r\n\r\n{\"qty\":\"0.01\"}"), "{request}");

    let (base, seen) = recorder();
    transport().signed_json(&base, "/a", "POST", r#"[{"x":1}]"#, &Stamp).unwrap();
    let request = seen.recv().unwrap();
    assert!(
        request.starts_with("post /a http/1.1\r\n") && request.ends_with("[{\"x\":1}]"),
        "{request}"
    );
}

/// The network is the one the transport was put on, with the headers that select it on every request; a transport
/// that does not say, the seam's default included, is the main network.
#[test]
fn the_network_is_the_one_its_headers_select() {
    assert_eq!(transport().network(), VenueNetwork::Mainnet);
    let test = transport().with_network(VenueNetwork::Testnet, &[("x-net", "test")]);
    assert_eq!(test.network(), VenueNetwork::Testnet);
    assert_eq!(test.headers(), &[("x-extra", "1"), ("x-net", "test")][..]);
    let (base, seen) = recorder();
    test.public(&base, "/t", &[]).unwrap();
    assert!(seen.recv().unwrap().contains("x-net: test"));
}

#[test]
fn a_public_get_urlencodes_its_params_and_signs_nothing() {
    let (base, seen) = recorder();
    transport().public(&base, "/t", &[("inst", "A B".into())]).unwrap();
    let request = seen.recv().unwrap();
    assert!(request.starts_with("get /t?inst=a+b http/1.1\r\n"), "{request}");
    assert!(!request.contains("x-sign"), "{request}");
}

/// The venue's own function writes the line: a call that failed to send reaches it.
#[test]
fn a_failed_call_reaches_the_venues_note() {
    let before = NOTES.load(Ordering::SeqCst);
    let e = transport().signed("http://127.0.0.1:1", "/x", "POST", &[], &Stamp).unwrap_err();
    assert_eq!(e.code, 0, "{e}");
    assert!(NOTES.load(Ordering::SeqCst) > before);
}

// A closed localhost port: connection-refused is immediate, so the send fails fast; the assertion is on the GATE,
// consulted BEFORE the send.
const DEAD: &str = "http://127.0.0.1:1";

#[test]
fn every_call_but_the_requery_rides_the_gate() {
    let gated = || {
        let gate = crate::ratelimit::RateGate::new(1, Duration::from_secs(30));
        (gate.clone(), transport().with_rate_gate(gate))
    };
    let (gate, t) = gated();
    let _ = t.signed(DEAD, "/x", "GET", &[], &Stamp);
    assert!(!gate.try_proceed(), "signed took the gate's only slot");
    let (gate, t) = gated();
    let _ = t.signed_json(DEAD, "/x", "POST", "[]", &Stamp);
    assert!(!gate.try_proceed(), "signed_json took the gate's only slot");
    let (gate, t) = gated();
    let _ = t.public(DEAD, "/x", &[]);
    assert!(!gate.try_proceed(), "public took the gate's only slot");
    let (gate, t) = gated();
    let _ = t.signed_requery(DEAD, "/x", "GET", &[], &Stamp);
    assert!(gate.try_proceed(), "the re-query must not ride the gate");
    assert!(t.rate_gate().is_some());
    assert!(transport().rate_gate().is_none());
}

/// A seam that implements only `signed` refuses the other calls in words and sends nothing.
#[test]
fn the_seams_defaults_refuse_in_words() {
    struct SignedOnly;
    impl RestTransportJson for SignedOnly {
        fn signed(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[(&str, Value)],
            _: &dyn SignerJson,
        ) -> Result<Value, VenueApiError> {
            Ok(Value::Null)
        }
    }
    let e = SignedOnly.signed_json("h", "/a", "POST", "[]", &Stamp).unwrap_err();
    assert_eq!(
        (e.code, e.msg.as_str()),
        (0, "POST /a: this transport sends no pre-serialized body")
    );
    let e = SignedOnly.public("h", "/t", &[]).unwrap_err();
    assert_eq!(e.code, 0, "{e}");
    assert!(SignedOnly.signed_requery("h", "/a", "GET", &[], &Stamp).unwrap().is_null());
    assert_eq!(SignedOnly.network(), VenueNetwork::Mainnet);
}
