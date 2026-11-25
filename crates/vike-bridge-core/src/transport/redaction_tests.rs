//! **No secret in an error's text (plan S10)**: what a request carried, long (an API key, a signature) or short (a
//! four-character passphrase), planted in its URL, its headers and the answer's body, reaches neither the `Display` nor
//! the `Debug` of the error the two shared signed transports return ([`UreqTransport`], [`RestTransportJsonUreq`]),
//! through [`redact_sent_values`] and [`send_error_text`]. The reply engine's spec shapes are `reply_tests.rs`'s and
//! `VenueApiHttpSpec::finish`'s are `http_error_tests.rs`'s.

use super::json::{RestTransportJson, RestTransportJsonUreq};
use super::reply::{RestReplyCode, RestReplyGatewayFirst, RestReplyNote, RestReplySpec};
use super::transport_error_kind_tests::serve;
use super::*;
use crate::signer::{PreparedRequest, SignerJson};

/// An API key, a signature (both long) and a passphrase short enough to be a word.
const KEY: &str = "k3y-7f3a9b2c4d";
const SIGNATURE: &str = "5d1e9a7c0b3f";
const PASS: &str = "Pw4z";
const PLANTED: [&str; 3] = [KEY, SIGNATURE, PASS];

/// `e`'s `Display` and `Debug` hold none of [`PLANTED`].
fn assert_holds_none(e: &VenueApiError, case: &str) {
    let (shown, debugged) = (e.to_string(), format!("{e:?}"));
    for value in PLANTED {
        // secret-absent: a value the request carried never reaches the error's text
        assert!(!shown.contains(value) && !debugged.contains(value), "{case}: {debugged}");
    }
}

/// The request's carried values as a proxy page, a JSON envelope and a plain line would echo them.
fn echoes(code_field: &str, msg_field: &str) -> Vec<String> {
    let echo = format!(
        "X-KEY: {KEY} X-SIGN: {SIGNATURE} X-PASSPHRASE: {PASS} signature={SIGNATURE}&p={PASS}"
    );
    vec![
        format!("<html><pre>{echo}</pre></html>"),
        serde_json::json!({ code_field: 10003, msg_field: format!("refused: {echo}") }).to_string(),
        serde_json::json!({ "error": format!("refused: {echo}") }).to_string(),
        echo,
    ]
}

fn reply(status: u16, body: &str) -> &'static str {
    Box::leak(
        format!(
            "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_boxed_str(),
    )
}

/// A short value is gone where it stands as a word and kept inside a longer one; a long one is gone wherever it is; a
/// short value that is a word of a longer one cannot cut the longer one open; an empty one redacts nothing.
#[test]
fn a_short_value_is_redacted_as_a_word_and_never_inside_one() {
    let text = "passphrase: pass, pass=pass&x, passed bypass (pass)";
    assert_eq!(
        redact_sent_values(text, &["pass"]),
        "passphrase: [redacted], [redacted]=[redacted]&x, passed bypass ([redacted])"
    );
    assert_eq!(
        redact_sent_values("5000 50000 recv_window[5000]", &["5000"]),
        "[redacted] 50000 recv_window[[redacted]]"
    );
    assert_eq!(redact_sent_values("xk3y-7f3a9b2c4dy", &[KEY]), "x[redacted]y");
    assert_eq!(redact_sent_values("key: ab-cdefghij", &["ab", "ab-cdefghij"]), "key: [redacted]");
    assert_eq!(redact_sent_values("é Pw4z é", &[PASS, ""]), "é [redacted] é");
    assert_eq!(redact_sent_values("no value", &[""]), "no value");
}

/// ureq's `BadUri` and `RequireHttpsOnly` print the whole URL; the venue error names only what is wrong with it.
#[test]
fn a_send_failure_never_quotes_the_url() {
    let url = format!("venue.example/api?apikey={KEY}&signature={SIGNATURE}&passphrase={PASS}");
    for failure in [
        ureq::Error::BadUri(format!("{url} is missing scheme")),
        ureq::Error::RequireHttpsOnly(url.clone()),
    ] {
        let e = classify_send_error(&failure);
        assert_eq!(e.code, 0, "a bad URL is provably never sent: {e}");
        assert_holds_none(&e, "constructed");
    }
    // The same through a real agent and the reply engine's own words: a base URL with no scheme.
    let e = UreqTransport::new("test").signed("", "/api", "GET", &[], &Planted).unwrap_err();
    assert_eq!(e, VenueApiError { code: 0, msg: "network error: bad uri (not shown)".into() });
    assert_holds_none(&e, "no scheme");
}

/// Signs nothing real: the planted key and passphrase as headers, the signature in the query (binance's shape).
struct Planted;
impl Signer for Planted {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest {
            query: format!("timestamp=1&signature={SIGNATURE}"),
            body: None,
            headers: vec![
                ("X-MBX-APIKEY".into(), KEY.into()),
                ("X-PASSPHRASE".into(), PASS.into()),
            ],
        }
    }
}

/// binance's transport: the signature rides the URL, the key and passphrase the headers, and every answer that echoes
/// them (a refusal's envelope, a gateway page, an unreadable 2xx, a 5xx on a write) leaves the error without them.
#[test]
fn the_ureq_transport_error_holds_nothing_the_request_carried() {
    for body in echoes("code", "msg") {
        for status in [200, 302, 400, 500, 502] {
            for method in ["GET", "POST"] {
                let answer = reply(status, &body);
                let Err(e) = UreqTransport::new("test").signed(
                    &serve(answer),
                    "/api",
                    method,
                    &[],
                    &Planted,
                ) else {
                    continue;
                };
                assert_holds_none(&e, &format!("{method} {status} {body}"));
            }
        }
    }
}

/// Signs nothing real: the planted values as the signed headers (bybit's and okx's shape).
struct PlantedJson;
impl SignerJson for PlantedJson {
    fn prepare(
        &self,
        _params: &[(&str, serde_json::Value)],
        _method: &str,
        _path: &str,
    ) -> PreparedRequest {
        PreparedRequest {
            headers: vec![
                ("X-KEY".into(), KEY.into()),
                ("X-SIGN".into(), SIGNATURE.into()),
                ("X-PASSPHRASE".into(), PASS.into()),
            ],
            ..PreparedRequest::default()
        }
    }
    fn prepare_body(&self, body: &str, method: &str, path: &str) -> PreparedRequest {
        PreparedRequest { body: Some(body.into()), ..self.prepare(&[], method, path) }
    }
}

const ENVELOPE: RestReplySpec = RestReplySpec {
    code: Some(RestReplyCode { field: "retCode", msg: "retMsg", string_ok: false }),
    no_verdict_on_5xx: &[],
    gateway_first: RestReplyGatewayFirst::Never,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: true,
    words: None,
    unknown_body_head: 0,
    plain_text_errors: false,
    verdict_on_5xx: &[],
};

fn quiet(_: RestReplyNote<'_>) {}

/// The JSON transport: every signed header the request carried is out of the error, whatever answered it.
#[test]
fn the_json_transport_error_holds_no_signed_header() {
    let transport = RestTransportJsonUreq::new("test", &ENVELOPE, quiet);
    for body in echoes("retCode", "retMsg") {
        for status in [200, 302, 400, 500, 502] {
            for method in ["GET", "POST"] {
                let answer = reply(status, &body);
                let Err(e) = transport.signed(&serve(answer), "/v5/x", method, &[], &PlantedJson)
                else {
                    continue;
                };
                assert_holds_none(&e, &format!("{method} {status} {body}"));
            }
        }
    }
}
