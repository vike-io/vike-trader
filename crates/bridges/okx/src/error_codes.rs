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
//! ENTIRELY OPT-IN: nothing in this crate consults [`TAXONOMY`] yet.

use vike_bridge_core::error_kind::VenueTaxonomy;
use vike_bridge_core::transport::ErrorKind;

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
        for code in 50000..52000i64 {
            let e = VenueApiError { code, msg: String::new() };
            let baseline = e.kind();
            if baseline == ErrorKind::Unknown {
                continue;
            }
            let Some(ours) = by_code(code) else { continue };
            assert_eq!(
                (ours.is_retryable(), ours.must_requery(), ours.is_terminal()),
                (baseline.is_retryable(), baseline.must_requery(), baseline.is_terminal()),
                "okx {code}: {ours} changes the disposition of baseline {baseline}"
            );
        }
    }

    /// REGRESSION (adversarial review, major/minor): the pin above tests only `by_code`, but
    /// `classify_venue` consults `by_msg` for EVERY error before falling through to the baseline —
    /// so the exact regression class the pin claims to prevent was reachable through a MESSAGE. This
    /// runs the FULL classifier over (code × representative message) pairs, including the HTTP-status
    /// codes a gateway body arrives as.
    #[test]
    fn full_classifier_never_changes_a_baseline_codes_disposition() {
        let msgs = [
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
            for msg in msgs {
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
}
