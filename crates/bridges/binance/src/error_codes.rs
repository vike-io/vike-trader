//! Binance's official error-code table, as a [`VenueTaxonomy`] (venue-error-taxonomy lane).
//!
//! Source: <https://developers.binance.com/docs/binance-spot-api-docs/errors> (the "Error codes for
//! Binance" reference; the USD-M futures list at
//! <https://developers.binance.com/docs/derivatives/usds-margined-futures/error-code> reuses the
//! same 10xx/11xx/2xxx numbering for every code named here).
//!
//! ## What this adds over the central baseline
//! [`vike_bridge_core::transport::ErrorKind::classify`] already seeds ~9 Binance codes. This table
//! is the FULL documented set, and it is consulted BEFORE the baseline — but it deliberately agrees
//! with the baseline on every code the baseline already knows, so adopting it can only ADD
//! resolution, never contradict what a caller sees today (pinned by
//! [`tests::table_never_contradicts_the_central_baseline`]).
//!
//! ## Binance's message-overloading problem
//! `-2010 NEW_ORDER_REJECTED` and `-2011 CANCEL_REJECTED` are umbrella codes: the real cause is only
//! in the MESSAGE. "Account has insufficient balance for requested action" and "Duplicate order
//! sent" both arrive as `-2010`. So [`by_msg`] carries the substring rules for the causes worth
//! distinguishing — chiefly insufficient balance, which is a sizing signal rather than a bug.
//!
//! ENTIRELY OPT-IN: nothing in this crate consults [`TAXONOMY`] yet. It is data plus a `const`,
//! wired by whoever flips [`vike_bridge_core::error_kind::ExecErrorPolicy::Classified`].

use vike_bridge_core::error_kind::VenueTaxonomy;
use vike_bridge_core::transport::ErrorKind;

/// Binance's documented error codes → [`ErrorKind`]. `None` = undocumented here; the caller falls
/// through to the central baseline (and ultimately to the terminal `Unknown` posture).
pub fn by_code(code: i64) -> Option<ErrorKind> {
    Some(match code {
        // ---- 10xx: general server / request ----------------------------------------------
        -1000 => ErrorKind::ServerError, // UNKNOWN: unknown error handling the request
        -1001 => ErrorKind::ServerError, // DISCONNECTED: internal failure; retry
        -1002 => ErrorKind::Auth,        // UNAUTHORIZED: not authorized for this request
        -1003 => ErrorKind::RateLimited, // TOO_MANY_REQUESTS: queue full / IP banned
        -1006 => ErrorKind::ServerError, // UNEXPECTED_RESP: bad message-bus response
        -1007 => ErrorKind::Timeout,     // TIMEOUT: backend never answered — status UNKNOWN
        -1008 => ErrorKind::RateLimited, // SERVER_BUSY: overloaded; back off
        -1013 => ErrorKind::InvalidRequest, // INVALID_MESSAGE: rejected pre-matching-engine
        -1014 => ErrorKind::InvalidRequest, // UNKNOWN_ORDER_COMPOSITION
        -1015 => ErrorKind::RateLimited, // TOO_MANY_ORDERS: new-order rate limit
        -1016 => ErrorKind::VenueMaintenance, // SERVICE_SHUTTING_DOWN
        -1020 => ErrorKind::InvalidRequest, // UNSUPPORTED_OPERATION
        -1021 => ErrorKind::InvalidRequest, // INVALID_TIMESTAMP: outside recvWindow
        -1022 => ErrorKind::Auth,        // INVALID_SIGNATURE
        // ---- 11xx: request parameters ----------------------------------------------------
        -1100 => ErrorKind::InvalidRequest, // ILLEGAL_CHARS
        -1101 => ErrorKind::InvalidRequest, // TOO_MANY_PARAMETERS
        -1102 => ErrorKind::InvalidRequest, // MANDATORY_PARAM_EMPTY_OR_MALFORMED
        -1103 => ErrorKind::InvalidRequest, // UNKNOWN_PARAM
        -1104 => ErrorKind::InvalidRequest, // UNREAD_PARAMETERS
        -1105 => ErrorKind::InvalidRequest, // PARAM_EMPTY
        -1106 => ErrorKind::InvalidRequest, // PARAM_NOT_REQUIRED
        -1108 => ErrorKind::InvalidRequest, // PARAM_OVERFLOW
        -1111 => ErrorKind::InvalidRequest, // BAD_PRECISION: too much precision for the step/tick
        -1115 => ErrorKind::InvalidRequest, // INVALID_TIF
        -1116 => ErrorKind::InvalidRequest, // INVALID_ORDER_TYPE
        -1117 => ErrorKind::InvalidRequest, // INVALID_SIDE
        -1121 => ErrorKind::InvalidRequest, // BAD_SYMBOL
        -1128 => ErrorKind::InvalidRequest, // OPTIONAL_PARAMS_BAD_COMBO
        -1135 => ErrorKind::InvalidRequest, // INVALID_JSON
        // ---- 2xxx: order / account -------------------------------------------------------
        // -2010 NEW_ORDER_REJECTED and -2011 CANCEL_REJECTED are UMBRELLA codes: the real cause is
        // only in the message. They are intentionally ABSENT from this exact-code table so
        // `by_msg` gets first refusal (e.g. insufficient balance); when no message rule matches,
        // the central baseline still classifies both (InvalidRequest / NotFound respectively).
        -2013 => ErrorKind::NotFound,       // NO_SUCH_ORDER
        -2014 => ErrorKind::Auth,           // BAD_API_KEY_FMT
        -2015 => ErrorKind::Auth,           // REJECTED_MBX_KEY: bad key / IP / permissions
        -2026 => ErrorKind::NotFound,       // ORDER_ARCHIVED (90+ days old)
        -2039 => ErrorKind::InvalidRequest, // CLIENT_ORDER_ID_INVALID
        _ => return None,
    })
}

/// Message-substring rules for Binance's umbrella codes (chiefly `-2010`/`-2011`, whose text is the
/// only place the real cause appears). Case-insensitive. `None` = no rule matched.
pub fn by_msg(msg: &str) -> Option<ErrorKind> {
    let m = msg.to_ascii_lowercase();
    // Insufficient balance is the one that most changes a caller's response (resize, don't abort).
    if m.contains("insufficient balance") || m.contains("insufficient margin") {
        return Some(ErrorKind::InsufficientFunds);
    }
    // "Market is closed." is deliberately NOT VenueMaintenance. Binance returns it as
    // `-2010 NEW_ORDER_REJECTED` when a symbol is in BREAK/HALT — a rejection of THIS order that
    // stands until the venue reopens the symbol, i.e. terminal, and the baseline for -2010 is
    // terminal too. Classifying it retryable flipped a baseline code's disposition and spun a halted
    // symbol's order in a backoff loop instead of rejecting it once.
    if m.contains("market is closed") {
        return Some(ErrorKind::InvalidRequest);
    }
    if m.contains("system maintenance") {
        return Some(ErrorKind::VenueMaintenance);
    }
    if m.contains("unknown order") || m.contains("order does not exist") {
        return Some(ErrorKind::NotFound);
    }
    if m.contains("duplicate order")
        || m.contains("too much precision")
        || m.contains("maximum defined limit")
        || m.contains("is zero or less")
    {
        return Some(ErrorKind::InvalidRequest);
    }
    if m.contains("too many request") || m.contains("rate limit") {
        return Some(ErrorKind::RateLimited);
    }
    None
}

/// Binance's taxonomy, ready to thread into
/// [`vike_bridge_core::error_kind::classify_venue`].
pub const TAXONOMY: VenueTaxonomy = VenueTaxonomy { venue: "binance", by_code, by_msg };

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::error_kind::classify_venue;
    use vike_bridge_core::transport::VenueApiError;

    fn kind(code: i64, msg: &str) -> ErrorKind {
        classify_venue(Some(&TAXONOMY), &VenueApiError { code, msg: msg.to_string() })
    }

    /// Documented codes land in the right bucket, spanning every category the lane asked for.
    #[test]
    fn documented_codes_classify() {
        let cases = [
            (-1002, ErrorKind::Auth),
            (-1022, ErrorKind::Auth),
            (-2014, ErrorKind::Auth),
            (-2015, ErrorKind::Auth),
            (-1003, ErrorKind::RateLimited),
            (-1008, ErrorKind::RateLimited),
            (-1015, ErrorKind::RateLimited),
            (-1016, ErrorKind::VenueMaintenance),
            (-1000, ErrorKind::ServerError),
            (-1001, ErrorKind::ServerError),
            (-1006, ErrorKind::ServerError),
            (-1007, ErrorKind::Timeout),
            (-2013, ErrorKind::NotFound),
            (-2026, ErrorKind::NotFound),
            (-1013, ErrorKind::InvalidRequest),
            (-1021, ErrorKind::InvalidRequest),
            (-1100, ErrorKind::InvalidRequest),
            (-1111, ErrorKind::InvalidRequest),
            (-1121, ErrorKind::InvalidRequest),
            (-2039, ErrorKind::InvalidRequest),
        ];
        for (code, want) in cases {
            assert_eq!(kind(code, ""), want, "binance {code}");
        }
    }

    /// The umbrella codes resolve through the MESSAGE — the whole reason `by_msg` exists.
    #[test]
    fn umbrella_codes_resolve_via_the_message() {
        assert_eq!(
            kind(-2010, "Account has insufficient balance for requested action"),
            ErrorKind::InsufficientFunds
        );
        assert_eq!(kind(-2010, "Duplicate order sent."), ErrorKind::InvalidRequest);
        assert_eq!(kind(-2010, "Market is closed."), ErrorKind::InvalidRequest);
        // No message rule → the central baseline still classifies the bare umbrella codes.
        assert_eq!(kind(-2010, "some novel reject"), ErrorKind::InvalidRequest);
        assert_eq!(kind(-2011, "some novel reject"), ErrorKind::NotFound);
    }

    /// An undocumented code is terminal, never retried — the safe posture.
    #[test]
    fn unknown_code_is_terminal() {
        assert_eq!(kind(-9999, "brand new failure"), ErrorKind::Unknown);
        assert!(ErrorKind::Unknown.is_terminal());
    }

    /// ADOPTION SAFETY PIN: for every code the central baseline already classifies, this table must
    /// produce the same DISPOSITION (retry / re-query / terminal). That is what makes flipping a
    /// venue onto the taxonomy a pure refinement: it may sharpen a bucket, and it may resolve codes
    /// the baseline left `Unknown`, but it can never flip a code a caller already acts on from
    /// retryable to terminal (an order silently rejected) or the reverse (a retry loop on a
    /// hopeless request).
    ///
    /// One code IS deliberately re-bucketed within its disposition: `-1016 SERVICE_SHUTTING_DOWN`
    /// moves `ServerError` → `VenueMaintenance`. Both are retryable, so the disposition is
    /// unchanged; the refinement only lengthens the advised backoff
    /// ([`ErrorKind::backoff_scale`]), which is the right response to a shutdown window.
    #[test]
    fn table_never_changes_a_baseline_codes_disposition() {
        for code in -2100..=-1000i64 {
            let e = VenueApiError { code, msg: String::new() };
            let baseline = e.kind();
            if baseline == ErrorKind::Unknown {
                continue; // the table is free to ADD resolution here
            }
            let Some(ours) = by_code(code) else { continue };
            assert_eq!(
                (ours.is_retryable(), ours.must_requery(), ours.is_terminal()),
                (baseline.is_retryable(), baseline.must_requery(), baseline.is_terminal()),
                "binance {code}: {ours} changes the disposition of baseline {baseline}"
            );
        }
    }

    /// REGRESSION (adversarial review, major): the pin above calls only `by_code`, while
    /// `classify_venue` consults `by_msg` for EVERY error before falling through to the baseline —
    /// so the very regression the pin claims to prevent was reachable through a MESSAGE, and
    /// `-2010 NEW_ORDER_REJECTED / "Market is closed."` was a live example (terminal → retryable).
    /// This runs the FULL classifier over (code × representative message) pairs, umbrella codes
    /// included.
    #[test]
    fn full_classifier_never_changes_a_baseline_codes_disposition() {
        let msgs = [
            "",
            "Market is closed.",
            "system maintenance in progress",
            "Account has insufficient balance for requested action",
            "Duplicate order sent.",
            "Unknown order sent.",
            "Too many requests.",
            "insufficient upstream capacity",
        ];
        for code in (-2100..=-1000i64).chain(100..600) {
            let baseline = VenueApiError { code, msg: String::new() }.kind();
            if baseline == ErrorKind::Unknown {
                continue;
            }
            for msg in msgs {
                let ours = kind(code, msg);
                assert!(
                    vike_bridge_core::error_kind::same_disposition(ours, baseline),
                    "binance {code} + {msg:?}: {ours} changes the disposition of baseline {baseline}"
                );
            }
        }
    }

    /// REGRESSION: a HALTED symbol's reject is TERMINAL. Retrying it spins until the session
    /// reopens and (before the seam's retry budget) emitted no terminal event at all.
    #[test]
    fn market_is_closed_stays_terminal() {
        let k = kind(-2010, "Market is closed.");
        assert!(k.is_terminal(), "a halted symbol rejects the order, it does not defer it");
        assert!(!k.is_retryable());
        assert_eq!(by_msg("Market is closed."), Some(ErrorKind::InvalidRequest));
    }

    /// Sanity on breadth: the lane asked for a real table, not a token one.
    #[test]
    fn table_covers_the_documented_set() {
        let n = (-2100..=-1000i64).filter(|c| by_code(*c).is_some()).count();
        assert!(n >= 30, "expected the full documented Binance set, got {n} codes");
    }
}
