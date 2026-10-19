//! `ErrorKind`'s central classification of `VenueApiError`s, and the WS-handshake transient set.

use super::*;

/// The ambiguous-timeout sentinel must classify to its OWN kind (never Network / Unknown), and
/// that kind is the must-re-query (never blind-retry, never reject) path.
#[test]
fn ambiguous_timeout_stays_its_own_kind() {
    let e = VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: "network error: timed out".into() };
    assert_eq!(e.kind(), ErrorKind::Timeout);
    assert!(ErrorKind::Timeout.must_requery(), "the ambiguous path MUST re-query");
    assert!(!ErrorKind::Timeout.is_retryable(), "it must NOT be blind-retried (double-submit)");
    // The other kinds are NOT the re-query path.
    for k in [
        ErrorKind::Network,
        ErrorKind::RateLimited,
        ErrorKind::ServerError,
        ErrorKind::Auth,
        ErrorKind::InvalidRequest,
        ErrorKind::NotFound,
        ErrorKind::Unknown,
    ] {
        assert!(!k.must_requery(), "{k} must not be treated as the ambiguous re-query path");
    }
}

/// `code == 0` (definite pre-send DNS/connect/TLS failure) is Network — distinct from the
/// ambiguous timeout, retryable, and NOT the re-query path (nothing landed → safe to reject).
#[test]
fn pre_send_network_failure_is_network() {
    let e = VenueApiError { code: 0, msg: "network error: dns".into() };
    assert_eq!(e.kind(), ErrorKind::Network);
    assert!(ErrorKind::Network.is_retryable());
    assert!(!ErrorKind::Network.must_requery());
}

/// The HTTP-status-as-code path (transport stores `i64::from(status)` when the error body has no
/// venue `{code}`) maps by HTTP semantics.
#[test]
fn http_statuses_map_to_expected_kinds() {
    let cases = [
        (429, ErrorKind::RateLimited),
        (418, ErrorKind::RateLimited), // Binance teapot = IP auto-ban
        (401, ErrorKind::Auth),
        (403, ErrorKind::Auth), // default; a venue override can remap to RateLimited
        (404, ErrorKind::NotFound),
        (400, ErrorKind::InvalidRequest),
        (422, ErrorKind::InvalidRequest),
        (500, ErrorKind::ServerError),
        (503, ErrorKind::ServerError),
    ];
    for (status, want) in cases {
        assert_eq!(ErrorKind::from_http_status(status), want, "HTTP {status}");
        // Same mapping via the merged {code,msg} classifier (status carried as the code).
        assert_eq!(
            VenueApiError { code: i64::from(status), msg: "http error".into() }.kind(),
            want,
            "code={status}"
        );
    }
}

/// Representative venue business codes (Binance negative; Bybit/OKX ≥ 10000) map to the seeded
/// kinds.
#[test]
fn venue_business_codes_map_to_expected_kinds() {
    let cases = [
        // rate limit
        (-1003, ErrorKind::RateLimited), // Binance too many requests
        (10006, ErrorKind::RateLimited), // Bybit too many visits
        (50011, ErrorKind::RateLimited), // OKX requests too frequent
        // auth
        (-2015, ErrorKind::Auth), // Binance invalid key/IP/perms
        (10003, ErrorKind::Auth), // Bybit invalid api key
        (50113, ErrorKind::Auth), // OKX invalid signature
        // not found
        (-2011, ErrorKind::NotFound), // Binance unknown order
        (51603, ErrorKind::NotFound), // OKX order does not exist
        // server
        (-1001, ErrorKind::ServerError), // Binance disconnected/internal
        (10016, ErrorKind::ServerError), // Bybit server error
        // bad request
        (-1013, ErrorKind::InvalidRequest), // Binance filter failure
        (-2010, ErrorKind::InvalidRequest), // Binance new-order rejected
        (10001, ErrorKind::InvalidRequest), // Bybit param error
    ];
    for (code, want) in cases {
        assert_eq!(VenueApiError { code, msg: String::new() }.kind(), want, "venue code {code}");
    }
}

/// An unknown code is Unknown by default, but a clear rate-limit phrase in the message is the
/// last-resort textual fallback (e.g. a CDN block that arrived with an odd code).
#[test]
fn unknown_code_uses_msg_text_as_last_resort() {
    assert_eq!(
        VenueApiError { code: 999_999, msg: "some novel error".into() }.kind(),
        ErrorKind::Unknown
    );
    assert_eq!(
        VenueApiError { code: 999_999, msg: "Too Many Requests from your IP".into() }.kind(),
        ErrorKind::RateLimited
    );
    assert_eq!(
        VenueApiError { code: 888_888, msg: "hit the venue RATE LIMIT".into() }.kind(),
        ErrorKind::RateLimited
    );
}

/// The override hook wins when it returns `Some`, and falls back to the central default on
/// `None`. Concrete example: a venue whose CDN uses HTTP 403 as an IP-block remaps 403 →
/// RateLimited, while every other error keeps the central classification.
#[test]
fn override_hook_wins_then_falls_back() {
    // A Bybit-shaped override: 403 (Cloudflare IP block) means rate-limited, not auth.
    let remap_403 = |e: &VenueApiError| (e.code == 403).then_some(ErrorKind::RateLimited);

    let forbidden = VenueApiError { code: 403, msg: "blocked".into() };
    assert_eq!(forbidden.kind(), ErrorKind::Auth, "central default for 403 is Auth");
    assert_eq!(forbidden.kind_with(remap_403), ErrorKind::RateLimited, "override wins");

    // A different error the override doesn't touch falls back to the central default.
    let unknown_order = VenueApiError { code: -2011, msg: "unknown order".into() };
    assert_eq!(unknown_order.kind_with(remap_403), ErrorKind::NotFound, "None → central default");
}

/// The retry-vs-abort helpers are a single principled source: transient kinds are retryable,
/// hard-failure kinds are not, and only the ambiguous timeout is the re-query path.
#[test]
fn retry_and_requery_semantics() {
    for k in [ErrorKind::RateLimited, ErrorKind::ServerError, ErrorKind::Network] {
        assert!(k.is_retryable(), "{k} should be retryable");
        assert!(!k.must_requery(), "{k} is not the re-query path");
    }
    for k in [ErrorKind::Auth, ErrorKind::InvalidRequest, ErrorKind::NotFound, ErrorKind::Unknown] {
        assert!(!k.is_retryable(), "{k} is a terminal failure");
        assert!(!k.must_requery(), "{k} is not the re-query path");
    }
    assert!(ErrorKind::Timeout.must_requery());
    assert!(!ErrorKind::Timeout.is_retryable());
}

/// Display strings are stable, snake_case log tokens (used as structured field values).
#[test]
fn display_tokens_are_stable() {
    assert_eq!(ErrorKind::RateLimited.to_string(), "rate_limited");
    assert_eq!(ErrorKind::Timeout.to_string(), "timeout");
    assert_eq!(ErrorKind::Network.to_string(), "network");
    assert_eq!(ErrorKind::Unknown.to_string(), "unknown");
}

/// The WS-handshake transient predicate the binance/bybit/okx `ws_auth` matchers delegate to. Each
/// decides reconnect-vs-fatal on a live private order stream, so the union must answer EXACTLY as
/// each venue's own 2-code list — no code gained, none lost.
#[test]
fn ws_ack_transient_reproduces_each_venues_original_list_exactly() {
    // Each venue's own list (binance/bybit/okx `ws_auth`).
    let binance_was = |c: i64| matches!(c, -1021 | -1003);
    let bybit_was = |c: i64| matches!(c, 10002 | 10006);
    let okx_was = |c: i64| matches!(c, 50011 | 50102);

    // Every code either venue's REST table knows, plus the absent-code sentinel and an
    // unseeded one — each classified by the shared predicate exactly as its venue used to.
    let binance_codes =
        [-1021, -1003, -1015, -1022, -1000, -1001, -1013, -1016, -1100, -2010, -2011, -2015];
    let bybit_codes = [10002, 10006, 10018, 10001, 10003, 10004, 10005, 10016, 110001];
    let okx_codes = [50011, 50102, 50061, 50001, 50111, 50113, 51000, 51603];

    for c in binance_codes {
        assert_eq!(ws_ack_is_transient(c), binance_was(c), "binance ws ack {c} changed");
    }
    for c in bybit_codes {
        assert_eq!(ws_ack_is_transient(c), bybit_was(c), "bybit ws ack {c} changed");
    }
    for c in okx_codes {
        assert_eq!(ws_ack_is_transient(c), okx_was(c), "okx ws ack {c} changed");
    }
    // An absent code (every matcher's `unwrap_or(0)` on a malformed frame) and an unseeded one
    // must surface, never reconnect-loop.
    assert!(!ws_ack_is_transient(0), "an absent code must not reconnect-loop");
    assert!(!ws_ack_is_transient(999_999), "an unseeded code must not reconnect-loop");
}

/// The REST table is deliberately BROADER than the WS handshake set and the two must not be
/// merged: these three REST-only throttle codes are `RateLimited` (retryable at REST) yet not
/// handshake-transient. Folding `classify_venue_code`'s RateLimited bucket into
/// [`ws_ack_is_transient`] would flip all three from a fatal handshake rejection to a silent
/// reconnect loop on three LIVE order streams.
#[test]
fn rest_only_throttle_codes_are_not_ws_handshake_transient() {
    for code in [
        -1015, // binance: too many new orders
        10018, // bybit:   exceeded IP rate limit
        50061, // okx:     requests too frequent
    ] {
        assert_eq!(
            VenueApiError { code, msg: String::new() }.kind(),
            ErrorKind::RateLimited,
            "{code} is a REST rate-limit code"
        );
        assert!(
            !ws_ack_is_transient(code),
            "{code} must NOT be handshake-transient (that would be a live behavior change)"
        );
    }
}

// --- `classify_send_error`: only a failure PROVABLY before the write is definite -----------------

fn io(kind: std::io::ErrorKind) -> ureq::Error {
    ureq::Error::Io(std::io::Error::new(kind, "x"))
}

/// Every kind that can only happen before the request was written keeps `code: 0` (a submit may
/// reject it: nothing reached the venue).
#[test]
fn provably_pre_write_send_failures_stay_definite() {
    use std::io::ErrorKind as K;
    let mut definite = vec![
        ureq::Error::HostNotFound,
        ureq::Error::ConnectionFailed,
        ureq::Error::InvalidProxyUrl,
        ureq::Error::ConnectProxyFailed("407".into()),
        ureq::Error::BadUri("x".into()),
        ureq::Error::TlsRequired,
        ureq::Error::RequireHttpsOnly("http://x".into()),
        ureq::Error::Http(ureq::http::Request::builder().uri("not a uri").body(()).unwrap_err()),
    ];
    for kind in
        [K::ConnectionRefused, K::AddrNotAvailable, K::NetworkUnreachable, K::HostUnreachable]
    {
        definite.push(io(kind));
    }
    for e in definite {
        let c = classify_send_error(&e);
        assert_eq!(c.code, 0, "{e} never left this host");
        assert_eq!(c.kind(), ErrorKind::Network);
    }
}

/// Everything that can follow the write is AMBIGUOUS: a venue that got the POST may have the order
/// live, so the caller re-queries and never rejects (a stranded position, then a duplicate).
#[test]
fn post_write_or_unknown_send_failures_are_ambiguous() {
    use std::io::ErrorKind as K;
    let mut ambiguous = vec![
        ureq::Error::Timeout(ureq::Timeout::Global),
        ureq::Error::RedirectFailed,
        ureq::Error::LargeResponseHeader(9, 1),
        ureq::Error::TooManyRedirects,
        ureq::Error::BodyStalled,
        ureq::Error::BodyExceedsLimit(1),
    ];
    for kind in [
        K::UnexpectedEof,
        K::ConnectionReset,
        K::ConnectionAborted,
        K::BrokenPipe,
        K::TimedOut,
        K::Other,
        K::NotFound,
    ] {
        ambiguous.push(io(kind));
    }
    for e in ambiguous {
        let c = classify_send_error(&e);
        assert_eq!(c.code, E_TIMEOUT_AMBIGUOUS, "{e} may have followed the write");
        assert!(c.kind().must_requery(), "{e}");
    }
}

/// Gateway-style 5xx say nothing about the order behind them; the venue's own 500 / 4xx stay as is.
#[test]
fn gateway_statuses_are_ambiguous_and_others_are_not() {
    for s in [502, 503, 504, 524] {
        assert!(status_is_gateway_ambiguous(s), "{s}");
        assert_eq!(gateway_ambiguous(s).code, E_TIMEOUT_AMBIGUOUS);
    }
    for s in [200, 400, 401, 403, 429, 500, 501, 521] {
        assert!(!status_is_gateway_ambiguous(s), "{s}");
    }
}

struct NoSigner;
impl Signer for NoSigner {
    fn prepare(
        &self,
        _p: &[(&str, String)],
        _m: &str,
        _path: &str,
    ) -> crate::signer::PreparedRequest {
        crate::signer::PreparedRequest::default()
    }
}

/// One-shot server: read the request head, then answer `reply` (empty = hang up, no response).
fn serve(reply: &'static str) -> String {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(reply.as_bytes());
        }
    });
    format!("http://{addr}")
}

/// The wiring end to end through the real agent: a peer that takes the POST and hangs up, and a
/// gateway 503, are ambiguous; a refused connect and an ordinary 500 are not.
#[test]
fn ureq_transport_maps_a_hangup_and_a_503_to_ambiguous() {
    let t = UreqTransport::new("test");
    let hangup = t.signed(&serve(""), "/x", "POST", &[], &NoSigner).unwrap_err();
    assert_eq!(hangup.code, E_TIMEOUT_AMBIGUOUS, "{hangup}");
    let gw = t
        .signed(
            &serve("HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"),
            "/x",
            "POST",
            &[],
            &NoSigner,
        )
        .unwrap_err();
    assert_eq!(gw.code, E_TIMEOUT_AMBIGUOUS, "{gw}");
    let venue = t
        .signed(
            &serve("HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"),
            "/x",
            "POST",
            &[],
            &NoSigner,
        )
        .unwrap_err();
    assert_eq!(venue.code, E_TIMEOUT_AMBIGUOUS, "a bare 500 on a write is unknown too: {venue}");
    let refusal = t
        .signed(
            &serve("HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"),
            "/x",
            "POST",
            &[],
            &NoSigner,
        )
        .unwrap_err();
    assert_eq!(refusal.code, 400, "a 4xx is the venue's own verdict: {refusal}");
    let refused = t.signed("http://127.0.0.1:1", "/x", "POST", &[], &NoSigner).unwrap_err();
    assert_eq!(refused.code, 0, "{refused}");
}

/// A 5xx (Binance: "every 5XX: execution status UNKNOWN and could have been a success") is "outcome
/// unknown" only for a MUTATING call, and the venue's own non-zero code in the body outranks it; a
/// read keeps the status-as-code (nothing to be unsure of), and a 4xx stays the venue's verdict.
#[test]
fn a_5xx_is_ambiguous_for_a_write_without_a_venue_code_only() {
    let t = UreqTransport::new("test");
    let reply = |status: &str, body: &str| -> &'static str {
        Box::leak(
            format!(
                "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        )
    };
    let coded = |status: &str| reply(status, "{\"code\":-1007,\"msg\":\"timeout\"}");
    for status in [500, 501, 502, 503, 504, 505, 520, 521, 522, 523, 524, 599] {
        let html = reply(&status.to_string(), "<html>");
        for method in ["POST", "PUT", "DELETE"] {
            let e = t.signed(&serve(html), "/x", method, &[], &NoSigner).unwrap_err();
            assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS, "{status} {method}: {e}");
            let e = t.signed(&serve(coded(&status.to_string())), "/x", method, &[], &NoSigner);
            assert_eq!(e.unwrap_err().code, -1007, "{status} {method} keeps the venue's own code");
        }
        let e = t.signed(&serve(html), "/x", "GET", &[], &NoSigner).unwrap_err();
        assert_eq!(e.code, i64::from(status), "a GET has no outcome to be unsure of: {e}");
        let e = t.signed_requery(&serve(html), "/x", "GET", &[], &NoSigner).unwrap_err();
        assert_eq!(e.code, i64::from(status), "{e}");
        let e = t.public(&serve(html), "/x", &[]).unwrap_err();
        assert_eq!(e.code, i64::from(status), "{e}");
    }
    // Binance's own `-1000` UNKNOWN / `-1001` DISCONNECTED on a 5xx are the same "status unknown" the
    // general 5xx rule documents, so on a write they are no venue code; `-1006`/`-1007` and any other
    // code keep theirs (`submit_outcome_unknown` decides those), and so does a 4xx or a read.
    for code in [-1000, -1001] {
        let body = format!("{{\"code\":{code},\"msg\":\"m\"}}");
        for status in ["500", "502", "503"] {
            for method in ["POST", "PUT", "DELETE"] {
                let e = t.signed(&serve(reply(status, &body)), "/x", method, &[], &NoSigner);
                assert_eq!(e.unwrap_err().code, E_TIMEOUT_AMBIGUOUS, "{code} on {status} {method}");
            }
            let e = t.signed(&serve(reply(status, &body)), "/x", "GET", &[], &NoSigner);
            assert_eq!(e.unwrap_err().code, code, "{code} on a {status} read");
        }
        for method in ["POST", "PUT", "DELETE"] {
            let e = t.signed(&serve(reply("400", &body)), "/x", method, &[], &NoSigner);
            assert_eq!(e.unwrap_err().code, code, "{code} on a 400 {method}");
        }
    }
    for code in [-1002, -1006, -1007, -1016, -2010] {
        let body = format!("{{\"code\":{code},\"msg\":\"m\"}}");
        let e = t.signed(&serve(reply("500", &body)), "/x", "POST", &[], &NoSigner);
        assert_eq!(e.unwrap_err().code, code, "{code} on a 500 write keeps its code");
    }
    for status in [400, 401, 403, 404, 418, 429] {
        let e = t
            .signed(&serve(reply(&status.to_string(), "<html>")), "/x", "POST", &[], &NoSigner)
            .unwrap_err();
        assert_eq!(e.code, i64::from(status), "a 4xx is the venue's own verdict: {e}");
    }
}

/// A 2xx the venue sent whose body is not JSON: on a write the venue RECEIVED the request and
/// answered, so what became of it is unknown (`code: 0` would license a terminal reject of an order
/// that may be live); a read has no outcome, so it keeps `code: 0`.
#[test]
fn an_unreadable_2xx_reply_is_ambiguous_for_a_write_only() {
    let t = UreqTransport::new("test");
    let html = "HTTP/1.1 200 OK\r\ncontent-length: 6\r\nconnection: close\r\n\r\n<html>";
    let empty = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    for method in ["POST", "PUT", "DELETE"] {
        for reply in [html, empty] {
            let e = t.signed(&serve(reply), "/x", method, &[], &NoSigner).unwrap_err();
            assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS, "{method}: {e}");
            assert!(e.msg.contains("bad json"), "{e}");
        }
    }
    let e = t.signed(&serve(html), "/x", "GET", &[], &NoSigner).unwrap_err();
    assert_eq!(e.code, 0, "{e}");
    let e = t.public(&serve(html), "/x", &[]).unwrap_err();
    assert_eq!(e.code, 0, "{e}");
}

/// A signed query is never replayed to a `Location`: the 302 is the answer.
#[test]
fn a_redirect_is_not_followed() {
    let e = UreqTransport::new("test")
        .signed(
            &serve("HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:1/y\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"),
            "/x",
            "POST",
            &[],
            &NoSigner,
        )
        .unwrap_err();
    assert_eq!(e.code, 302, "{e}");
}

/// A failed DNS lookup arrives from ureq as `Io(Uncategorized)`; the agents' resolver turns it into
/// `HostNotFound`, so with the internet down a submit sees a DEFINITE pre-write failure.
/// (`.invalid` is reserved: it never resolves.)
#[test]
fn a_dns_failure_is_definite_through_the_venue_agent() {
    let e = UreqTransport::new("test")
        .signed("http://no-such-host.invalid", "/x", "POST", &[], &NoSigner)
        .unwrap_err();
    assert_eq!(e.code, 0, "{e}");
    let raw = crate::http::blocking_agent().post("http://no-such-host.invalid/x").send_empty();
    assert!(matches!(raw, Err(ureq::Error::HostNotFound)), "{raw:?}");
}

fn tls(e: rustls::Error) -> ureq::Error {
    ureq::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The lazy rustls handshake surfaces as `Io(InvalidData)` wrapping a `rustls::Error`: only the
/// certificate / negotiation errors (nothing in plaintext was sent) are definite.
#[test]
fn a_tls_handshake_io_error_is_definite_only_for_certificate_and_negotiation() {
    use rustls::{CertificateError, Error as R, PeerIncompatible};
    for e in [
        tls(R::InvalidCertificate(CertificateError::UnknownIssuer)),
        tls(R::InvalidCertificate(CertificateError::Expired)),
        tls(R::PeerIncompatible(PeerIncompatible::Tls12NotOffered)),
        tls(R::NoCertificatesPresented),
    ] {
        assert_eq!(classify_send_error(&e).code, 0, "{e}");
    }
    for e in [
        tls(R::General("x".into())),
        tls(R::DecryptError),
        tls(R::AlertReceived(rustls::AlertDescription::InternalError)),
        io(std::io::ErrorKind::InvalidData),
    ] {
        assert_eq!(classify_send_error(&e).code, E_TIMEOUT_AMBIGUOUS, "{e}");
    }
}

/// A real TLS server whose certificate no root vouches for: the client aborts inside the first
/// write, before any plaintext, and that is DEFINITE end to end.
#[test]
fn a_tls_server_presenting_an_untrusted_cert_is_definite() {
    use base64::Engine as _;
    use std::sync::Arc;
    // Self-signed ECDSA P-256, CN=localhost, 100 years; NOT trusted by any root. Test-only key.
    const CERT: &str = "MIIBlTCCATugAwIBAgIUbJ+DJfiAWEUMnq30IeY2xNbBJYMwCgYIKoZIzj0EAwIwFDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MTAxNjE1NTYyMVoYDzIxMjYwOTIyMTU1NjIxWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAQJUPI5fFCPRaCW9c8hnpxMTrSvkydxoHp+dM6MBYPpFRqo812weppUJSaFN5+gYM3MmOyRX6ArQtux5lCa89Z3o2kwZzAdBgNVHQ4EFgQU4kWm+49gJUkdX7cdymFrxX75sV0wHwYDVR0jBBgwFoAU4kWm+49gJUkdX7cdymFrxX75sV0wDwYDVR0TAQH/BAUwAwEB/zAUBgNVHREEDTALgglsb2NhbGhvc3QwCgYIKoZIzj0EAwIDSAAwRQIgGxlxqYDXHrQ7cHhIaeP3KnJnvhO0jScWTDbZJP4FxsACIQDSGqK8FjIn1iDKaDG/biTustxxw/iI4BXpRaF5/dWEFg==";
    const KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgIzuDyOl9WSMdBdWp+aqPOzTGaSyAa2BEJzm4LO2OFEihRANCAAQJUPI5fFCPRaCW9c8hnpxMTrSvkydxoHp+dM6MBYPpFRqo812weppUJSaFN5+gYM3MmOyRX6ArQtux5lCa89Z3";
    let der = |s: &str| base64::engine::general_purpose::STANDARD.decode(s).unwrap();
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(der(CERT))],
        rustls::pki_types::PrivatePkcs8KeyDer::from(der(KEY)).into(),
    )
    .unwrap();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut conn = rustls::ServerConnection::new(Arc::new(cfg)).unwrap();
            let _ = conn.complete_io(&mut s);
        }
    });
    let e = UreqTransport::new("test")
        .signed(&format!("https://127.0.0.1:{port}"), "/x", "POST", &[], &NoSigner)
        .unwrap_err();
    assert_eq!(e.code, 0, "{e}");
}
