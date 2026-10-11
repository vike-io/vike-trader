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
