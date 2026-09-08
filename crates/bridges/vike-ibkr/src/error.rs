//! IBKR error taxonomy. The code sets mirror NautilusTrader's IB adapter (source-verified
//! 2026-07-14) so a given IB `errorCode` drives BOTH the connection-flag toggle and order
//! rejection consistently. `is_advisory` is the Data-vs-Notice band (2100–2169): those are
//! log-only and MUST never synthesize a terminal (consumed by the event mapper, Task 7).

use std::fmt;

/// Bridge-level error. Carries only non-secret diagnostic text (host/port/reason) — never a login.
#[derive(Clone, PartialEq, Eq)]
pub enum IbkrError {
    /// No transport (absent config, unreachable Gateway, or `ibkr-socket` not compiled) → paper.
    Unavailable,
    /// Socket connect/handshake failure.
    Connect(String),
    /// Protocol/decode failure after connect.
    Protocol(String),
    /// Requested symbol/interval/venue mapping has no support (e.g. `parse_simplified` couldn't
    /// parse the canonical symbol, or the interval has no bar-size mapping). Added for PR-3b
    /// (historical backfill); the unit `Unavailable` above stays the transport-level ("no
    /// connection at all") gate and is untouched.
    Unsupported(String),
    /// A specific request came back empty/failed AFTER a working connection (e.g. no head
    /// timestamp, a `historical_data` fetch error) — distinct from the connection-level
    /// `Unavailable` (no transport). Added for PR-3b (historical backfill).
    DataUnavailable(String),
}

impl fmt::Display for IbkrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IbkrError::Unavailable => write!(f, "IBKR transport unavailable (staying paper)"),
            IbkrError::Connect(m) => write!(f, "IBKR connect failed: {m}"),
            IbkrError::Protocol(m) => write!(f, "IBKR protocol error: {m}"),
            IbkrError::Unsupported(m) => write!(f, "IBKR unsupported: {m}"),
            IbkrError::DataUnavailable(m) => write!(f, "IBKR data unavailable: {m}"),
        }
    }
}

impl fmt::Debug for IbkrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl std::error::Error for IbkrError {}

/// Classification of an IB `errorCode`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CodeClass {
    Warning,
    ConnectivityLost,
    ConnectivityRestored,
    OrderRejection,
    Suppress,
    Other,
}

const CONNECTIVITY_LOST: &[i32] = &[326, 1100, 1300, 2103, 2110];
const CONNECTIVITY_RESTORED: &[i32] = &[1101, 1102, 2104];

/// Codes that terminalize an order.
///
/// **200 was in `SUPPRESS` and is the one code that provably belonged here.** "No security
/// definition has been found for the request" means the CONTRACT did not resolve — so IB created no
/// order, and silencing it left the intent SUBMITTED forever with nothing at the venue that could
/// ever answer it. It is also the single safest code to terminalize, and for a reason none of the
/// others share: a synthesized reject cannot contradict live exposure, because a contract that did
/// not resolve cannot have produced a resting order. Every other candidate code carries the risk
/// that IB accepted the order and is telling us something *about* it.
///
/// `SUPPRESS` is now empty, and stays as a class because the distinction it draws — "seen,
/// deliberately silent" — is the one an added code needs to be argued into.
///
/// ## Why this is not INVERTED (unknown ⇒ rejection unless allowlisted benign)
///
/// Inverting is the obvious reading of "a five-code allowlist means most rejections never
/// terminalize", and it is the wrong cure, because the two failure directions are not symmetric:
///
/// * A missed rejection leaves an order stuck SUBMITTED. Bad, RECOVERABLE, and it now has a net —
///   `crates/vike-core/src/runtime/mod.rs`'s `submit_ack_timeout`, which every shipped mount arms
///   (including `crates/vike-run/src/bin/ibkr_mount.rs`, which did not until this change).
/// * A phantom rejection tells the platform an order is DEAD while IB still holds it live. The
///   position is then invisible to risk, the FSM will not accept its later fill, and nothing
///   recovers it but a human. There is no watchdog for believing a lie.
///
/// And IB's numeric space is full of codes that arrive attached to an order without rejecting it —
/// order-status commentary, cancel-not-found, warnings about attributes IB then adjusted. An invert
/// rule terminalizes on every one of them. The `2100..=2169` advisory band is only the part of that
/// hazard that happens to be contiguous.
///
/// So the shape is: terminalize on codes we can ARGUE, and let the watchdog — which triggers on the
/// ABSENCE of an ack rather than on a guess about a code's meaning, and re-queries through the
/// confirm-grace ladder before rejecting anything — cover the rest. That is what
/// `crates/bridges/vike-ibkr/src/event_mapper.rs`'s `on_error` already says it relies on.
///
/// ## Why the list is not WIDENED by enumeration either
///
/// IB publishes dozens more order-rejecting codes (the 103–141 validation band, the 10xxx family).
/// Adding them from memory or from a scraped table is exactly the "GUESSED" practice this crate is
/// trying to retire elsewhere, and a mis-added code produces the phantom-rejection direction above.
/// A code joins this list when a live gateway has been OBSERVED sending it for a rejected order —
/// `crates/bridges/vike-ibkr/tests/ibkr_smoke.rs` and `ibkr_cpapi_smoke.rs` are the vehicles, and
/// item 1 of the socket smoke's module doc already asks the human running it to watch for exactly
/// this.
const ORDER_REJECTION: &[i32] = &[200, 201, 203, 321, 10289, 10293];

/// Seen and deliberately silent. Empty since 200 moved to [`ORDER_REJECTION`] — see there.
const SUPPRESS: &[i32] = &[];

/// The Data-vs-Notice advisory band — log-only, never a terminal event (ibapi behaviour).
pub fn is_advisory(code: i32) -> bool {
    (2100..=2169).contains(&code)
}

pub fn is_connection_lost(code: i32) -> bool {
    CONNECTIVITY_LOST.contains(&code)
}

/// Classify with connectivity/rejection/suppress taking precedence over the broad advisory band.
pub fn classify_code(code: i32) -> CodeClass {
    if CONNECTIVITY_LOST.contains(&code) {
        CodeClass::ConnectivityLost
    } else if CONNECTIVITY_RESTORED.contains(&code) {
        CodeClass::ConnectivityRestored
    } else if ORDER_REJECTION.contains(&code) {
        CodeClass::OrderRejection
    } else if SUPPRESS.contains(&code) {
        CodeClass::Suppress
    } else if is_advisory(code) {
        CodeClass::Warning
    } else {
        CodeClass::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connectivity_codes_classify() {
        for c in [326, 1100, 1300, 2103, 2110] {
            assert_eq!(classify_code(c), CodeClass::ConnectivityLost, "code {c}");
            assert!(is_connection_lost(c));
        }
        for c in [1101, 1102, 2104] {
            assert_eq!(classify_code(c), CodeClass::ConnectivityRestored, "code {c}");
        }
    }

    #[test]
    fn order_rejection_codes_classify() {
        for c in [200, 201, 203, 321, 10289, 10293] {
            assert_eq!(classify_code(c), CodeClass::OrderRejection, "code {c}");
        }
    }

    /// Code 200 ("no security definition has been found") used to be SUPPRESSED, so a symbol that
    /// parsed but does not exist at IB was placed, answered, and got no venue terminal at all —
    /// the order sat SUBMITTED forever. `classify_code` has exactly ONE consumer,
    /// `crates/bridges/vike-ibkr/src/event_mapper.rs`'s `on_error`, which is fed only by the
    /// ORDER-update lane, so this reclassification cannot touch a market-data 200.
    #[test]
    fn code_200_terminalizes_instead_of_being_silenced() {
        assert_eq!(classify_code(200), CodeClass::OrderRejection);
        assert_ne!(classify_code(200), CodeClass::Suppress);
    }

    /// The invert rule this list deliberately does NOT implement: an unknown code stays `Other` and
    /// emits nothing, leaving the (now-armed) submit-ack watchdog to terminalize on the ABSENCE of
    /// an ack rather than on a guess about what a number means.
    #[test]
    fn an_unknown_code_is_not_a_rejection() {
        for c in [399, 404, 10147, 999999] {
            assert_eq!(classify_code(c), CodeClass::Other, "code {c}");
        }
    }

    #[test]
    fn advisory_band_is_warning_and_never_connection_lost() {
        // 2104/2103/2110 are carved out by the connectivity sets; the rest of 2100..=2169 warn.
        assert_eq!(classify_code(2137), CodeClass::Warning);
        assert!(is_advisory(2137));
        assert!(!is_connection_lost(2137));
    }

    #[test]
    fn suppress_is_empty_and_unknown_codes_are_other() {
        // `SUPPRESS` holds nothing since 200 moved out; the class stays so an added code has to be
        // argued into "seen, deliberately silent" rather than defaulting there.
        assert!(SUPPRESS.is_empty());
        assert_eq!(classify_code(999999), CodeClass::Other);
    }

    #[test]
    fn error_debug_leaks_nothing() {
        // Sanity: variants carry only non-secret diagnostic text.
        let e = IbkrError::Connect("connection refused (127.0.0.1:7497)".into());
        assert!(format!("{e:?}").contains("connection refused"));
    }
}
