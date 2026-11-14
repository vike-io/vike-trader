//! OKX's official V5 error-code table, as a [`VenueTaxonomy`] (venue-error-taxonomy lane).
//!
//! Source: <https://www.okx.com/docs-v5/en/#error-code> (the V5 "Error Code" reference — the
//! 5xxxx public/general block, the 501xx authentication block, and the 51xxx trade block).
//!
//! Same shape as the binance/bybit siblings: an exact-code table consulted before the central
//! baseline ([`vike_bridge_core::transport::ErrorKind::classify`]), plus a message fallback.
//!
//! ## OKX's one venue-specific nuance
//! OKX runs SCHEDULED matching-engine maintenance around the funding settlements (00:00 / 08:00 /
//! 16:00 UTC+8), surfaced as `50001` (system maintenance), `50005` (endpoint offline) and `50013`
//! (system busy). Those are minutes-long windows, not millisecond blips, which is exactly what
//! [`ErrorKind::VenueMaintenance`] exists to distinguish — the central baseline lumps `50001` in
//! with ordinary `ServerError`s, so a caller on the baseline retries far too aggressively through a
//! funding window.
//!
//! ## `sCode` vs `code`
//! OKX batch endpoints report the PER-ORDER outcome in `sCode`/`sMsg` while the envelope `code` is
//! `0`. This table classifies whichever numeric the adapter hands it — a caller that reads `sCode`
//! must pass THAT, not the envelope code (a `0` envelope reaches the baseline's pre-send `Network`
//! sentinel, which would be plainly wrong for a per-order reject).
//!
//! The [`TAXONOMY`] const is opt-in data; the one live reader today is [`submit_outcome_unknown`],
//! which asks [`by_code`] whether a failed submit may still be live.

use vike_bridge_core::error_kind::VenueTaxonomy;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, ErrorKind, VenueApiError};
use vike_model::events::Event;

/// OKX's documented V5 error codes → [`ErrorKind`]. `None` = fall through to the baseline.
pub fn by_code(code: i64) -> Option<ErrorKind> {
    Some(match code {
        // ---- 5000x: system / general -------------------------------------------------------
        50000 => ErrorKind::InvalidRequest, // body cannot be empty
        50001 => ErrorKind::VenueMaintenance, // service temporarily unavailable: maintenance
        50002 => ErrorKind::InvalidRequest, // JSON syntax error
        // 50004 is AMBIGUOUS, not transient. OKX's official text is "Endpoint request timeout (does
        // not mean the request was successful or failed, please check the request result)" — the
        // venue is explicitly telling the caller it does not know the outcome. Classifying it
        // retryable would re-POST /api/v5/trade/order over an order the venue may already hold:
        // double-submit, double position. It belongs in the SAME bucket as the transport's
        // E_TIMEOUT_AMBIGUOUS sentinel — Timeout ⇒ must_requery.
        50004 => ErrorKind::Timeout, // endpoint request timeout — OUTCOME UNKNOWN, must re-query
        50005 => ErrorKind::VenueMaintenance, // API endpoint offline or unavailable
        50006 => ErrorKind::InvalidRequest, // invalid Content-Type
        50007 => ErrorKind::Auth,    // user account frozen
        50008 => ErrorKind::Auth,    // user does not exist
        50009 => ErrorKind::Auth,    // account is suspended
        50011 => ErrorKind::RateLimited, // rate limit reached
        50013 => ErrorKind::VenueMaintenance, // system busy — the funding-window signal
        50014 => ErrorKind::InvalidRequest, // required parameter cannot be blank
        50026 => ErrorKind::ServerError, // system error; try again later
        50061 => ErrorKind::RateLimited, // requests too frequent (sub-account limit)
        // ---- 501xx: authentication ---------------------------------------------------------
        50100 => ErrorKind::InvalidRequest, // API frozen: contact customer service
        50101 => ErrorKind::Auth,           // API key does not match the current environment
        50102 => ErrorKind::InvalidRequest, // timestamp differs from server time by >30s
        50103 => ErrorKind::Auth,           // request header OK-ACCESS-KEY cannot be empty
        50104 => ErrorKind::Auth,           // request header OK-ACCESS-PASSPHRASE cannot be empty
        50105 => ErrorKind::Auth,           // request header OK-ACCESS-PASSPHRASE incorrect
        50106 => ErrorKind::Auth,           // request header OK-ACCESS-SIGN cannot be empty
        50107 => ErrorKind::Auth,           // request header OK-ACCESS-TIMESTAMP cannot be empty
        50110 => ErrorKind::Auth,           // invalid IP: not in the API key's allowlist
        50111 => ErrorKind::Auth,           // invalid OK-ACCESS-KEY
        50112 => ErrorKind::Auth,           // invalid OK-ACCESS-TIMESTAMP
        50113 => ErrorKind::Auth,           // invalid signature
        50114 => ErrorKind::Auth,           // invalid authorization
        // ---- 51xxx: trade ------------------------------------------------------------------
        51000 => ErrorKind::InvalidRequest,    // parameter error
        51001 => ErrorKind::InvalidRequest,    // instrument ID does not exist
        51004 => ErrorKind::InvalidRequest,    // order amount exceeds the position limit
        51008 => ErrorKind::InsufficientFunds, // insufficient balance / margin to place the order
        51009 => ErrorKind::Auth,              // order placement blocked by the account
        51010 => ErrorKind::InvalidRequest,    // account mode does not support this operation
        51020 => ErrorKind::InvalidRequest,    // order amount below the minimum
        51089 => ErrorKind::InvalidRequest,    // missing size in a batch order leg
        51094 => ErrorKind::InvalidRequest,    // price outside the allowed band
        51119 => ErrorKind::InsufficientFunds, // order placement failed: insufficient balance
        51121 => ErrorKind::InvalidRequest,    // order quantity must be a multiple of the lot size
        51127 => ErrorKind::InsufficientFunds, // available balance is zero
        51131 => ErrorKind::InsufficientFunds, // insufficient balance
        51136 => ErrorKind::InsufficientFunds, // closing-order size exceeds the available balance
        51400 => ErrorKind::NotFound,          // cancellation failed: order does not exist
        51401 => ErrorKind::NotFound,          // cancellation failed: order already canceled
        51402 => ErrorKind::NotFound,          // cancellation failed: order already completed
        51403 => ErrorKind::InvalidRequest,    // cancellation failed: order type not supported
        51404 => ErrorKind::InvalidRequest,    // order in pending-cancel state
        51410 => ErrorKind::NotFound,          // cancellation failed: order already in cancelling
        51503 => ErrorKind::NotFound,          // amendment failed: order does not exist
        51600 => ErrorKind::ServerError,       // status not found
        51601 => ErrorKind::InvalidRequest,    // unsupported order status
        51603 => ErrorKind::NotFound,          // order does not exist
        _ => return None,
    })
}

/// Does this code say only that the SERVER had trouble, so that it says nothing about what became of
/// the request? [`by_code`]'s rows, from the V5 reference this module cites: `50001` "service temporarily
/// unavailable", `50013` "system busy", `50026` "system error; try again later". `50005` ("API endpoint
/// offline or unavailable") is not one: an endpoint that is offline took nothing.
pub fn server_had_trouble(code: i64) -> bool {
    matches!(code, 50001 | 50013 | 50026)
}

/// Does a failed ORDER SUBMIT with this `code` leave the order's fate UNKNOWN (it may be live)? The
/// transport's own sentinel ([`E_TIMEOUT_AMBIGUOUS`]), a venue code this table files under
/// [`ErrorKind::Timeout`] (`50004`: the venue itself says the request may have succeeded or failed;
/// it arrives as the body's code, so it is not the sentinel) OR [`server_had_trouble`]. `true` means
/// re-query (`resolve_ambiguous_submit`, `order-algo` for a stop), never a terminal `OrderRejected`. The
/// ONE rule behind every submit arm in `crates/bridges/okx/src/execution/rest/mod.rs`, `execution/rest/batch.rs` and
/// `execution/rest/stop_algo.rs`.
pub fn submit_outcome_unknown(code: i64) -> bool {
    code == E_TIMEOUT_AMBIGUOUS
        || server_had_trouble(code)
        || by_code(code).is_some_and(ErrorKind::must_requery)
}

/// Does a CANCEL answered `code` say the order is gone? The filled / already-canceled / does-not-exist
/// family, the REGULAR book's "unknown" answer. A resting STOP (`order-algo`) is not in that book, so
/// this answer is about the wrong endpoint and proves nothing about a stop: `cancel_order` retries it on
/// the algo endpoint (`crates/bridges/okx/src/execution/rest/stop_algo.rs`'s `cancel_algo`) before it reads the
/// order as gone, and the cancel batch does the same per row. `algo_unknown` and the regular lookup's
/// "absent" read this set too.
///
/// **The set is exactly the words `cancel_order` swallowed before stops were cancelled on
/// `cancel-algos`, so every OTHER regular answer (`51603`, `51413`, `51403`, ...) stays the `Err` it
/// always was.** Measured 2026-10-23 on the demo account (`tests/okx_smokes/okx_algo_stop_smoke.rs`'s
/// `MEASURED regular cancel-order` line): `cancel-order` of a STOP's order client id answers `51400`, and the
/// stop survives it. To add a code, add exactly the one a printed line shows, with a row in
/// `tests/offline/algo_stop.rs`'s pin tests.
pub fn cancel_says_gone(code: i64) -> bool {
    matches!(code, 51400..=51402)
}

/// The non-terminal advisory for a failed amend. A status-unknown code ([`submit_outcome_unknown`])
/// is worded as the timeout sentinel is (`modify outcome unknown`: the amend may have been applied)
/// instead of as a refusal; a definite one keeps the venue's words. Stamped with `stated`, the refusal
/// reply row's own `ts`, else when the error was received (`vike_bridge_core::rest::amend_answer_time`):
/// a [`VenueApiError`] keeps no time, so the caller reads it off the reply before unwrapping it.
pub fn modify_failed(order_client_id: &str, exc: &VenueApiError, stated: Option<i64>) -> Event {
    let ts = vike_bridge_core::rest::amend_answer_time(stated);
    vike_bridge_core::rest::modify_failed_with(order_client_id, exc, submit_outcome_unknown, ts)
}

/// Message fallback for OKX bodies that arrive without a usable numeric (gateway HTML, an
/// `sCode`-less envelope).
pub fn by_msg(msg: &str) -> Option<ErrorKind> {
    let m = msg.to_ascii_lowercase();
    // ANCHORED to balance/margin wording: a bare `contains("insufficient")` also matches bodies that
    // have nothing to do with the account ("insufficient permissions" on an auth reject, "insufficient
    // upstream capacity" from a gateway), and InsufficientFunds is TERMINAL — misfiring there turns a
    // transient outage into rejected orders. (The `classify_venue` disposition guard now blocks that
    // flip structurally too; this keeps the table honest on its own.)
    if m.contains("insufficient")
        && ["balance", "margin", "funds", "equity"].iter().any(|w| m.contains(w))
    {
        return Some(ErrorKind::InsufficientFunds);
    }
    if m.contains("maintenance") || m.contains("system busy") || m.contains("upgrade") {
        return Some(ErrorKind::VenueMaintenance);
    }
    if m.contains("too frequent") || m.contains("rate limit") {
        return Some(ErrorKind::RateLimited);
    }
    if m.contains("does not exist") || m.contains("already canceled") {
        return Some(ErrorKind::NotFound);
    }
    None
}

/// OKX's taxonomy, ready to thread into [`vike_bridge_core::error_kind::classify_venue`].
pub const TAXONOMY: VenueTaxonomy = VenueTaxonomy { venue: "okx", by_code, by_msg };

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::error_kind::classify_venue;
    use vike_bridge_core::transport::VenueApiError;

    fn kind(code: i64, msg: &str) -> ErrorKind {
        classify_venue(Some(&TAXONOMY), &VenueApiError { code, msg: msg.to_string() })
    }

    #[test]
    fn documented_codes_classify() {
        let cases = [
            (50111, ErrorKind::Auth),
            (50113, ErrorKind::Auth),
            (50105, ErrorKind::Auth),
            (50110, ErrorKind::Auth),
            (50007, ErrorKind::Auth),
            (50011, ErrorKind::RateLimited),
            (50061, ErrorKind::RateLimited),
            (50001, ErrorKind::VenueMaintenance),
            (50004, ErrorKind::Timeout),
            (50005, ErrorKind::VenueMaintenance),
            (50013, ErrorKind::VenueMaintenance),
            (50026, ErrorKind::ServerError),
            (50000, ErrorKind::InvalidRequest),
            (50102, ErrorKind::InvalidRequest),
            (51000, ErrorKind::InvalidRequest),
            (51001, ErrorKind::InvalidRequest),
            (51020, ErrorKind::InvalidRequest),
            (51008, ErrorKind::InsufficientFunds),
            (51119, ErrorKind::InsufficientFunds),
            (51131, ErrorKind::InsufficientFunds),
            (51400, ErrorKind::NotFound),
            (51401, ErrorKind::NotFound),
            (51503, ErrorKind::NotFound),
            (51603, ErrorKind::NotFound),
        ];
        for (code, want) in cases {
            assert_eq!(kind(code, ""), want, "okx {code}");
        }
    }

    /// The OKX nuance: the funding-window codes are a MAINTENANCE window, not an ordinary 5xx —
    /// still retryable, but on a far longer backoff.
    /// REGRESSION (adversarial review, critical): `50004` is "Endpoint request timeout (does not
    /// mean the request was successful or failed, please check the request result)" — an AMBIGUOUS
    /// outcome. It must land in the must-re-query bucket, NEVER a retry: re-issuing a POST
    /// /api/v5/trade/order the venue may already hold double-submits the order.
    #[test]
    fn okx_50004_is_ambiguous_and_must_requery_never_retry() {
        let k = kind(50004, "Endpoint request timeout");
        assert_eq!(
            k,
            ErrorKind::Timeout,
            "50004 is an ambiguous outcome, not a maintenance window"
        );
        assert!(k.must_requery(), "50004 must force a status re-query");
        assert!(!k.is_retryable(), "blind-retrying 50004 double-submits a live order");
        assert!(!k.is_terminal(), "50004 must not synthesize a reject either (phantom position)");
        assert_eq!(
            vike_bridge_core::error_kind::submit_disposition(k),
            vike_bridge_core::error_kind::SubmitDisposition::Requery
        );
    }

    #[test]
    fn submit_outcome_unknown_is_exactly_the_sentinel_50004_and_the_server_trouble() {
        for code in [E_TIMEOUT_AMBIGUOUS, 50004, 50001, 50013, 50026] {
            assert!(submit_outcome_unknown(code), "{code}");
        }
        // definite refusals, and an endpoint that is offline (it took nothing)
        for code in [0, 51000, 51008, 51603, 50005, 500] {
            assert!(!submit_outcome_unknown(code), "{code}");
        }
    }

    #[test]
    fn funding_window_codes_are_maintenance_not_a_server_blip() {
        for code in [50001, 50005, 50013] {
            let k = kind(code, "");
            assert_eq!(k, ErrorKind::VenueMaintenance, "okx {code}");
            assert!(k.is_retryable(), "a maintenance window is still retryable");
            assert!(
                k.backoff_scale() > ErrorKind::ServerError.backoff_scale(),
                "okx {code} must back off longer than an ordinary 5xx"
            );
        }
    }

    /// Today's set, code by code, and what the table files under `NotFound` beyond it.
    #[test]
    fn cancel_says_gone_is_exactly_todays_set() {
        let codes = 0..200_000i64;
        let gone: Vec<i64> = codes.clone().filter(|c| cancel_says_gone(*c)).collect();
        assert_eq!(gone, [51400, 51401, 51402]);
        let table_only: Vec<i64> = codes
            .filter(|c| by_code(*c) == Some(ErrorKind::NotFound) && !cancel_says_gone(*c))
            .collect();
        assert_eq!(table_only, [51410, 51503, 51603]);
    }

    #[test]
    fn unknown_code_is_terminal() {
        assert_eq!(kind(59_999, "brand new failure"), ErrorKind::Unknown);
        assert!(ErrorKind::Unknown.is_terminal());
    }

    #[test]
    fn message_fallback_catches_codeless_bodies() {
        assert_eq!(kind(502, "system busy, try later"), ErrorKind::VenueMaintenance);
        assert_eq!(kind(400, "Insufficient USDT balance"), ErrorKind::InsufficientFunds);
    }

    /// ADOPTION SAFETY PIN — see the binance twin. Deliberate in-disposition refinement: `50001`
    /// ServerError → VenueMaintenance (both retryable, longer backoff).
    #[test]
    fn table_never_changes_a_baseline_codes_disposition() {
        vike_bridge_core::error_table::assert_keeps_baseline_dispositions(
            "okx",
            50000..52000i64,
            by_code,
        );
    }

    /// REGRESSION (adversarial review, major/minor): the pin above tests only `by_code`, but
    /// `classify_venue` consults `by_msg` for EVERY error before falling through to the baseline —
    /// so the exact regression class the pin claims to prevent was reachable through a MESSAGE. This
    /// runs the FULL classifier over (code × representative message) pairs, including the HTTP-status
    /// codes a gateway body arrives as.
    #[test]
    fn full_classifier_never_changes_a_baseline_codes_disposition() {
        let messages = [
            "",
            "insufficient upstream capacity",
            "insufficient permissions for this endpoint",
            "Insufficient USDT balance",
            "system busy, try later",
            "scheduled upgrade in progress",
            "requests too frequent",
            "order does not exist",
            "service temporarily unavailable",
        ];
        let codes: Vec<i64> = (50000..52000).chain(100..600).collect();
        for code in codes {
            let baseline = VenueApiError { code, msg: String::new() }.kind();
            if baseline == ErrorKind::Unknown {
                continue; // free to ADD resolution where the baseline has no opinion
            }
            for msg in messages {
                let ours = kind(code, msg);
                assert!(
                    vike_bridge_core::error_kind::same_disposition(ours, baseline),
                    "okx {code} + {msg:?}: {ours} changes the disposition of baseline {baseline}"
                );
            }
        }
    }

    /// The anchored `insufficient` rule: only account-balance wording may reach the TERMINAL
    /// InsufficientFunds bucket. A 5xx outage body that merely contains the word must stay retryable.
    #[test]
    fn insufficient_is_anchored_to_balance_wording() {
        assert_eq!(by_msg("insufficient upstream capacity"), None);
        assert_eq!(by_msg("insufficient permissions"), None);
        assert_eq!(by_msg("Insufficient USDT balance"), Some(ErrorKind::InsufficientFunds));
        assert_eq!(by_msg("insufficient margin"), Some(ErrorKind::InsufficientFunds));
        assert!(kind(503, "insufficient upstream capacity").is_retryable());
    }

    #[test]
    fn table_covers_the_documented_set() {
        let n = (50000..52000i64).filter(|c| by_code(*c).is_some()).count();
        assert!(n >= 30, "expected the full documented OKX set, got {n} codes");
    }

    /// A failed modify: this venue's status-unknown code (50004) is worded as the timeout sentinel
    /// is, keeping the venue's message; a definite code keeps the venue's words verbatim.
    #[test]
    fn modify_failed_words_the_unknown_code_as_the_sentinel() {
        vike_bridge_core::error_table::assert_modify_failed_words_unknown_as_the_sentinel(
            |c, e| modify_failed(c, e, None),
            50004,
        );
    }
}
