//! Bybit's official V5 error-code table, as a [`VenueTaxonomy`] (venue-error-taxonomy lane).
//!
//! Source: <https://bybit-exchange.github.io/docs/v5/error> (the V5 "Error Codes" reference — the
//! 10xxx UTA/general block, the 110xxx derivatives-order block, and the 170xxx spot-trade block).
//!
//! Shape mirrors the binance sibling: an exact-code table consulted before the central baseline
//! ([`vike_bridge_core::transport::ErrorKind::classify`]), plus a message fallback. Bybit is much
//! LESS message-overloaded than Binance — its codes are specific — so [`by_msg`] is thin and exists
//! mainly for the Cloudflare-fronted HTML bodies that arrive with no venue code at all.
//!
//! ## Bybit's one venue-specific nuance
//! Bybit fronts REST with Cloudflare, which answers an IP block with **HTTP 403**. The central
//! baseline maps 403 → [`ErrorKind::Auth`] (its standard meaning), which for Bybit is misleading:
//! the key is fine, the IP is throttled. [`by_code`] remaps `403` → [`ErrorKind::RateLimited`]
//! here — exactly the override the central `kind_with` doc names as the motivating example.
//!
//! ENTIRELY OPT-IN: nothing in this crate consults [`TAXONOMY`] yet.

use vike_bridge_core::error_kind::VenueTaxonomy;
use vike_bridge_core::transport::ErrorKind;

/// Bybit's documented V5 error codes → [`ErrorKind`]. `None` = fall through to the baseline.
pub fn by_code(code: i64) -> Option<ErrorKind> {
    Some(match code {
        // ---- HTTP-status-as-code: the ONE Bybit-specific override -------------------------
        // Cloudflare answers an IP block with 403 (NOT an auth failure — see the module doc).
        403 => ErrorKind::RateLimited,
        // ---- 10xxx: general / auth / rate limit / system ----------------------------------
        10001 => ErrorKind::InvalidRequest, // request parameter error
        10002 => ErrorKind::InvalidRequest, // request timestamp outside recv_window
        10003 => ErrorKind::Auth,           // API key invalid / key-domain mismatch
        10004 => ErrorKind::Auth,           // error sign: bad signature algorithm
        10005 => ErrorKind::Auth,           // permission denied for this API key
        10006 => ErrorKind::RateLimited,    // too many visits: API rate limit exceeded
        10010 => ErrorKind::Auth,           // unmatched IP: not in the key's bound IP list
        10016 => ErrorKind::VenueMaintenance, // internal server error; service is restarting
        10017 => ErrorKind::InvalidRequest, // route not found
        10018 => ErrorKind::RateLimited,    // exceeded the IP rate limit
        10027 => ErrorKind::Auth,           // trading banned / restricted
        20003 => ErrorKind::RateLimited,    // too frequent requests under the same session
        // ---- 110xxx: derivatives order errors ---------------------------------------------
        110001 => ErrorKind::NotFound,          // order does not exist
        110003 => ErrorKind::InvalidRequest,    // order price exceeds the permitted range
        110004 => ErrorKind::InsufficientFunds, // wallet balance is insufficient
        110006 => ErrorKind::InsufficientFunds, // position value exceeds the available balance
        110007 => ErrorKind::InsufficientFunds, // available balance is insufficient
        110008 => ErrorKind::NotFound,          // the order has been completed or cancelled
        110009 => ErrorKind::InvalidRequest,    // too many stop orders on this contract
        110010 => ErrorKind::NotFound,          // the order has been cancelled
        110012 => ErrorKind::InsufficientFunds, // insufficient available balance
        110013 => ErrorKind::InvalidRequest,    // due to risk limit, cannot set leverage
        110017 => ErrorKind::InvalidRequest,    // reduce-only rule not satisfied
        110020 => ErrorKind::InvalidRequest,    // more than 500 active orders not allowed
        110021 => ErrorKind::InvalidRequest,    // open-interest position limit exceeded
        110022 => ErrorKind::InvalidRequest,    // qty exceeds the risk-limit maximum
        110023 => ErrorKind::InvalidRequest,    // reduce-only: can only reduce on this contract
        110025 => ErrorKind::InvalidRequest,    // position mode not modified
        110043 => ErrorKind::InvalidRequest,    // leverage not modified
        110045 => ErrorKind::InsufficientFunds, // wallet balance insufficient to add margin
        110051 => ErrorKind::InsufficientFunds, // available balance cannot cover the order cost
        110052 => ErrorKind::InsufficientFunds, // available balance insufficient for the fee
        110053 => ErrorKind::InsufficientFunds, // available balance insufficient (margin)
        110094 => ErrorKind::InvalidRequest,    // order notional below the lower limit
        110120 => ErrorKind::InvalidRequest,    // order price below the allowed minimum
        110121 => ErrorKind::InvalidRequest,    // order price above the allowed maximum
        // ---- 170xxx: spot trade errors -----------------------------------------------------
        170007 => ErrorKind::Timeout, // timeout waiting for the matching engine — status UNKNOWN
        170131 => ErrorKind::InsufficientFunds, // balance insufficient
        170133 => ErrorKind::InvalidRequest, // order price precision too high
        170134 => ErrorKind::InvalidRequest, // order qty precision too high
        170136 => ErrorKind::InvalidRequest, // order quantity below the minimum
        170137 => ErrorKind::InvalidRequest, // order volume too large
        170139 => ErrorKind::NotFound, // order has already been filled
        170140 => ErrorKind::InvalidRequest, // order value below the minimum notional
        170143 => ErrorKind::NotFound, // cannot be found on the order book
        170213 => ErrorKind::NotFound, // order does not exist
        _ => return None,
    })
}

/// Message fallback. Bybit codes are specific, so this covers only the code-less bodies (a
/// Cloudflare HTML page, a gateway error) that reach the classifier as raw text.
pub fn by_msg(msg: &str) -> Option<ErrorKind> {
    let m = msg.to_ascii_lowercase();
    // ANCHORED to balance/margin wording: a bare `contains("insufficient")` also matches bodies with
    // nothing to do with the account ("insufficient permissions" on an auth reject, "insufficient
    // upstream capacity" from the Cloudflare edge), and InsufficientFunds is TERMINAL — misfiring
    // there turns a transient outage into rejected orders. (The `classify_venue` disposition guard
    // now blocks that flip structurally too; this keeps the table honest on its own.)
    if m.contains("insufficient")
        && ["balance", "margin", "funds", "equity"].iter().any(|w| m.contains(w))
    {
        return Some(ErrorKind::InsufficientFunds);
    }
    if m.contains("restarting") || m.contains("system maintenance") || m.contains("upgrading") {
        return Some(ErrorKind::VenueMaintenance);
    }
    if m.contains("too many visits") || m.contains("rate limit") || m.contains("too frequent") {
        return Some(ErrorKind::RateLimited);
    }
    None
}

/// Bybit's taxonomy, ready to thread into [`vike_bridge_core::error_kind::classify_venue`].
pub const TAXONOMY: VenueTaxonomy = VenueTaxonomy { venue: "bybit", by_code, by_msg };

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
            (10003, ErrorKind::Auth),
            (10004, ErrorKind::Auth),
            (10005, ErrorKind::Auth),
            (10010, ErrorKind::Auth),
            (10006, ErrorKind::RateLimited),
            (10018, ErrorKind::RateLimited),
            (20003, ErrorKind::RateLimited),
            (10016, ErrorKind::VenueMaintenance),
            (10001, ErrorKind::InvalidRequest),
            (10002, ErrorKind::InvalidRequest),
            (110001, ErrorKind::NotFound),
            (110008, ErrorKind::NotFound),
            (110010, ErrorKind::NotFound),
            (170143, ErrorKind::NotFound),
            (110004, ErrorKind::InsufficientFunds),
            (110007, ErrorKind::InsufficientFunds),
            (110052, ErrorKind::InsufficientFunds),
            (170131, ErrorKind::InsufficientFunds),
            (110094, ErrorKind::InvalidRequest),
            (110120, ErrorKind::InvalidRequest),
            (170133, ErrorKind::InvalidRequest),
            (170007, ErrorKind::Timeout),
        ];
        for (code, want) in cases {
            assert_eq!(kind(code, ""), want, "bybit {code}");
        }
    }

    /// The Bybit-specific nuance: Cloudflare's 403 is an IP THROTTLE, not an auth failure. This is
    /// the one place the taxonomy deliberately departs from the central HTTP mapping.
    #[test]
    fn cloudflare_403_is_a_throttle_not_an_auth_failure() {
        let e = VenueApiError { code: 403, msg: "Forbidden".into() };
        assert_eq!(e.kind(), ErrorKind::Auth, "central baseline treats 403 as auth");
        assert_eq!(kind(403, "Forbidden"), ErrorKind::RateLimited, "bybit override");
        assert!(ErrorKind::RateLimited.is_retryable(), "so the caller backs off instead of dying");
    }

    #[test]
    fn unknown_code_is_terminal() {
        assert_eq!(kind(999_999, "brand new failure"), ErrorKind::Unknown);
        assert!(ErrorKind::Unknown.is_terminal());
    }

    #[test]
    fn message_fallback_catches_codeless_bodies() {
        assert_eq!(kind(502, "upstream restarting"), ErrorKind::VenueMaintenance);
        assert_eq!(kind(418, "<html>rate limit</html>"), ErrorKind::RateLimited);
    }

    /// ADOPTION SAFETY PIN — see the binance twin. Every code the baseline already classifies keeps
    /// its DISPOSITION (retry / re-query / terminal) under this table. The deliberate exceptions are
    /// documented and both stay within their disposition: `10016` ServerError → VenueMaintenance
    /// (both retryable) and the `403` Cloudflare override, which is asserted separately above and so
    /// is excluded here.
    #[test]
    fn table_never_changes_a_baseline_codes_disposition() {
        for code in (10000..20100i64).chain(110000..110200).chain(170000..170300) {
            let e = VenueApiError { code, msg: String::new() };
            let baseline = e.kind();
            if baseline == ErrorKind::Unknown {
                continue;
            }
            let Some(ours) = by_code(code) else { continue };
            assert_eq!(
                (ours.is_retryable(), ours.must_requery(), ours.is_terminal()),
                (baseline.is_retryable(), baseline.must_requery(), baseline.is_terminal()),
                "bybit {code}: {ours} changes the disposition of baseline {baseline}"
            );
        }
    }

    /// REGRESSION (adversarial review): the pin above tests only `by_code`, but `classify_venue`
    /// consults `by_msg` for EVERY error before the baseline — so a disposition flip was reachable
    /// through a MESSAGE. This runs the FULL classifier over (code × representative message) pairs.
    /// `403` is excluded: it is the ONE documented deliberate flip (Cloudflare throttle), asserted
    /// on its own above.
    #[test]
    fn full_classifier_never_changes_a_baseline_codes_disposition() {
        let msgs = [
            "",
            "insufficient upstream capacity",
            "insufficient permissions",
            "wallet balance is insufficient",
            "upstream restarting",
            "system maintenance",
            "upgrading",
            "too many visits",
            "rate limit",
        ];
        let codes = (10000..20100i64).chain(110000..110200).chain(170000..170300).chain(100..600);
        for code in codes {
            if code == 403 {
                continue;
            }
            let baseline = VenueApiError { code, msg: String::new() }.kind();
            if baseline == ErrorKind::Unknown {
                continue;
            }
            for msg in msgs {
                let ours = kind(code, msg);
                assert!(
                    vike_bridge_core::error_kind::same_disposition(ours, baseline),
                    "bybit {code} + {msg:?}: {ours} changes the disposition of baseline {baseline}"
                );
            }
        }
    }

    /// The anchored `insufficient` rule: only account-balance wording reaches the TERMINAL bucket.
    #[test]
    fn insufficient_is_anchored_to_balance_wording() {
        assert_eq!(by_msg("insufficient upstream capacity"), None);
        assert_eq!(by_msg("insufficient permissions"), None);
        assert_eq!(by_msg("Wallet balance is insufficient"), Some(ErrorKind::InsufficientFunds));
        assert!(kind(503, "insufficient upstream capacity").is_retryable());
    }

    #[test]
    fn table_covers_the_documented_set() {
        let n = (10000..20100i64)
            .chain(110000..110200)
            .chain(170000..170300)
            .filter(|c| by_code(*c).is_some())
            .count();
        assert!(n >= 30, "expected the full documented Bybit set, got {n} codes");
    }
}
