//! IBKR error taxonomy. The code sets follow a reference IB adapter (source-verified
//! 2026-07-14) so a given IB `errorCode` drives BOTH the connection-flag toggle and order
//! rejection consistently. `is_advisory` is the Data-vs-Notice band (2100–2169): those are
//! log-only and MUST never synthesize a terminal (consumed by the event mapper, Task 7).
//!
//! The three refusal sets (`ORDER_REJECTION`, `CANCEL_REFUSAL`, `AMEND_REFUSAL`) are IB's published error table
//! (<https://www.interactivebrokers.com/docs/tws-api/doc/error/codes>), one set per write a code answers, so a code IB
//! documents as refusing a CANCEL can never end the order it names. Each code has a sample in this repository or is
//! on the tests' `WITHOUT_SAMPLE` list (which only shrinks: a code joins it only in the change that adds the code).

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
            IbkrError::Unavailable => {
                write!(f, "IBKR transport unavailable (the account does not run)")
            }
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
    /// IB refused a CANCEL of the order the code names (`CANCEL_REFUSAL`): the order is not ended by it.
    CancelRefusal,
    /// IB refused an AMEND of the order the code names (`AMEND_REFUSAL`): the order keeps its terms.
    AmendRefusal,
    Suppress,
    Other,
}

const CONNECTIVITY_LOST: &[i32] = &[326, 1100, 1300, 2103, 2110];
const CONNECTIVITY_RESTORED: &[i32] = &[1101, 1102, 2104];

/// Codes that refuse a SUBMIT: they end an order IB never acknowledged, and only that
/// (`crates/bridges/vike-ibkr/src/execution/event_mapper.rs`'s `refused`).
///
/// **200 was in `SUPPRESS` and is the one code that provably belonged here first.** "No security
/// definition has been found for the request" means the CONTRACT did not resolve — so IB created no
/// order, and silencing it left the intent SUBMITTED forever with nothing at the venue that could
/// ever answer it. A synthesized reject cannot contradict live exposure there, because a contract that
/// did not resolve cannot have produced a resting order. Any other code may come after IB accepted the
/// order and tell us something *about* it, which is why an ACKNOWLEDGED order is never ended by a code
/// alone (below).
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
///   `crates/vike-core/src/runtime/config.rs`'s `submit_ack_timeout`, which every shipped start arms
///   (including `crates/vike-run/src/bin/ibkr_mount.rs`, which did not until this change, and which
///   was deleted on 2026-09-28 — the IBKR engine now starts only under the daemon, which arms it).
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
/// `crates/bridges/vike-ibkr/src/execution/event_mapper.rs`'s `on_error` already says it relies on.
///
/// ## The list is IB's published refusals of a submit (S4b, 2026-11-23)
///
/// It used to grow only by a code a live gateway was OBSERVED sending, and `110` ("The price does not conform to the
/// minimum price variation for this contract", refused live: the crate's `CLAUDE.md`) showed the cost: a refusal IB
/// states in a number was only logged, and the order waited a minute for the watchdog, whose terminal does not carry
/// IB's words. It now holds every code IB's table documents as refusing a NEW order (refused for good or for now: a
/// halted symbol's `154` refuses this order too). What keeps a mis-read code out of the phantom-rejection direction
/// above is the mapper, not the list: `refused` ends only an order IB never acknowledged, and an ACKNOWLEDGED order is
/// ended by IB's own `Cancelled`/`Inactive` status alone, carrying the code's words.
///
/// Left out on purpose: `103` "Duplicate order ID" names an EARLIER order's id, so it must never end the order that id
/// resolves to (only the new write is refused); the cancel and amend refusals, sets of their own below; and every code
/// the table words as an order going on (`399`, `404` held for a short-sale locate, `481` size reduced, the advisory
/// band, the routing notices `10311` and `10329`).
///
/// **10052 "Invalid time in force:GTC" joined on 2026-10-25, observed:** a limit order sent with `outsideRth` +
/// `includeOvernight` + GTC (the v0.1.87 `extended_hours` switch) was answered by this one error 2-3 ms after the
/// `placeOrder` on the wire (tcpdump of the paper gateway's API socket, 5 of 5 orders, cold and warm price lane), with
/// no order status after it, so IB created no order. That is the safe direction argued above: a refusal at
/// validation cannot contradict live exposure. The cure for that order is the TIF
/// (`crates/bridges/vike-ibkr/src/transport/socket.rs`'s `build_order`); this entry is for the next refusal of the
/// kind, which must end the order at once and not after the watchdog's minute. 10329 ("This order will be directly
/// routed to OVERNIGHT") is a NOTICE that an order was routed and stays out.
const ORDER_REJECTION: &[i32] = &[
    106, 107, 109, 110, 111, 113, 116, 122, 124, 125, 126, 132, 133, 140, 141, 145, 146, 147, 148,
    152, 154, 157, 158, 159, 160, 163, 164, 168, 200, 201, 203, 303, 311, 321, 328, 334, 336, 337,
    338, 342, 351, 355, 361, 382, 383, 387, 388, 392, 395, 396, 401, 402, 403, 406, 411, 412, 426,
    434, 435, 10006, 10015, 10020, 10052, 10243, 10250, 10289, 10290, 10293, 10340,
];

/// IB's published refusals of a CANCEL: `135` "Can't find order with ID", `161` "not in a cancellable state", `10147`
/// "Order to be canceled was not found" and `10148` "can not be cancelled, state:" say the order is gone (filled or
/// ended), `136` "This order cannot be cancelled" that it still works. None ends the order (its end is IB's status or
/// its fill): the cancel is answered with a non-terminal `OrderCancelRejected` in IB's words.
const CANCEL_REFUSAL: &[i32] = &[135, 136, 161, 10147, 10148];

/// IB's published refusals of an AMEND (a `placeOrder` again under the order's id): `104` "Can't modify a filled
/// order", `105` "does not match original order", `134` "Modify order failed", `329` "Cannot change to the new order
/// type", `2102` "still being processed" (inside the advisory band, so classified before it), `10241` and `10242`
/// "Modification is not supported via API". The amend in flight is refused and the order keeps its terms; with none in
/// flight the code ends nothing.
const AMEND_REFUSAL: &[i32] = &[104, 105, 134, 329, 2102, 10241, 10242];

/// Seen and deliberately silent. Empty since 200 moved to [`ORDER_REJECTION`] — see there.
const SUPPRESS: &[i32] = &[];

/// The Data-vs-Notice advisory band — log-only, never a terminal event (ibapi behaviour).
pub fn is_advisory(code: i32) -> bool {
    (2100..=2169).contains(&code)
}

pub fn is_connection_lost(code: i32) -> bool {
    CONNECTIVITY_LOST.contains(&code)
}

/// Classify with connectivity/refusal/suppress taking precedence over the broad advisory band.
pub fn classify_code(code: i32) -> CodeClass {
    if CONNECTIVITY_LOST.contains(&code) {
        CodeClass::ConnectivityLost
    } else if CONNECTIVITY_RESTORED.contains(&code) {
        CodeClass::ConnectivityRestored
    } else if ORDER_REJECTION.contains(&code) {
        CodeClass::OrderRejection
    } else if CANCEL_REFUSAL.contains(&code) {
        CodeClass::CancelRefusal
    } else if AMEND_REFUSAL.contains(&code) {
        CodeClass::AmendRefusal
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
        for c in [110, 154, 200, 201, 203, 321, 10052, 10289, 10293] {
            assert_eq!(classify_code(c), CodeClass::OrderRejection, "code {c}");
        }
    }

    /// A refusal of a cancel or of an amend is never a submit's refusal, so it can never end the order it names; `2102`
    /// ("still being processed") is classified before the advisory band it sits in; `103` names an EARLIER order's id and
    /// is in no set.
    #[test]
    fn each_refusal_is_classified_by_the_write_it_answers() {
        for c in [135, 136, 161, 10147, 10148] {
            assert_eq!(classify_code(c), CodeClass::CancelRefusal, "code {c}");
        }
        for c in [104, 105, 134, 329, 2102, 10241, 10242] {
            assert_eq!(classify_code(c), CodeClass::AmendRefusal, "code {c}");
        }
        assert_eq!(classify_code(103), CodeClass::Other);
        let sets = [ORDER_REJECTION, CANCEL_REFUSAL, AMEND_REFUSAL];
        for (i, set) in sets.iter().enumerate() {
            for code in *set {
                let elsewhere = sets.iter().enumerate().any(|(j, s)| j != i && s.contains(code));
                assert!(
                    !elsewhere && !CONNECTIVITY_LOST.contains(code),
                    "{code} is in one set only"
                );
            }
        }
    }

    /// The refusal codes proved by an answer of IB's that this repository keeps (the file names the code).
    const SAMPLED: &[(i32, &str)] = &[
        (110, "src/instrument/properties.rs"),
        (201, "src/transport/socket_tests.rs"),
        (321, "src/execution/order.rs"),
        (10052, "src/execution/event_mapper_tests.rs"),
    ];

    /// The refusal codes no captured answer proves: IB's published table is their only source. It only shrinks: a code
    /// joins it only in the change that adds the code (the S4b change added all but `200`, `203`, `10289`, `10293`,
    /// which were listed before it with no sample either), and leaves it when a capture lands in `SAMPLED`.
    const WITHOUT_SAMPLE: &[i32] = &[
        104, 105, 106, 107, 109, 111, 113, 116, 122, 124, 125, 126, 132, 133, 134, 135, 136, 140,
        141, 145, 146, 147, 148, 152, 154, 157, 158, 159, 160, 161, 163, 164, 168, 200, 203, 303,
        311, 328, 329, 334, 336, 337, 338, 342, 351, 355, 361, 382, 383, 387, 388, 392, 395, 396,
        401, 402, 403, 406, 411, 412, 426, 434, 435, 2102, 10006, 10015, 10020, 10147, 10148,
        10241, 10242, 10243, 10250, 10289, 10290, 10293, 10340,
    ];

    /// Every refusal code is sampled (its file names it) or on [`WITHOUT_SAMPLE`], never both, and the two lists name
    /// refusal codes only.
    #[test]
    fn every_refusal_code_has_a_sample_or_says_it_has_none() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let codes = [ORDER_REJECTION, CANCEL_REFUSAL, AMEND_REFUSAL].concat();
        for code in &codes {
            let sampled = SAMPLED.iter().any(|(c, _)| c == code);
            assert!(
                sampled != WITHOUT_SAMPLE.contains(code),
                "{code}: sampled or listed, exactly one"
            );
        }
        for (code, file) in SAMPLED {
            assert!(codes.contains(code), "{code} is a refusal code");
            let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
            assert!(text.contains(&code.to_string()), "{file} names {code}");
        }
        for code in WITHOUT_SAMPLE {
            assert!(codes.contains(code), "{code} is a refusal code");
        }
    }

    /// 10052 "Invalid time in force:GTC" is IB's answer, within 3 ms and with no order status after it, to a
    /// flagged (`outsideRth` + `includeOvernight`) GTC limit order (observed on the paper gateway, 5 of 5,
    /// 2026-10-25). It used to be `Other`, so the order sat SUBMITTED until the submit-ack watchdog.
    #[test]
    fn invalid_time_in_force_terminalizes() {
        assert_eq!(classify_code(10052), CodeClass::OrderRejection);
    }

    /// The codes that ride an order but do not refuse it stay out of the list: the whole advisory band, and 10329
    /// ("This order will be directly routed to OVERNIGHT", a notice that an order was ROUTED).
    #[test]
    fn notice_and_warning_codes_never_reject() {
        for c in [399, 2100, 2137, 2169, 10329] {
            assert_ne!(classify_code(c), CodeClass::OrderRejection, "code {c}");
        }
    }

    /// Code 200 ("no security definition has been found") used to be SUPPRESSED, so a symbol that
    /// parsed but does not exist at IB was placed, answered, and got no venue terminal at all —
    /// the order sat SUBMITTED forever. `classify_code` has exactly ONE consumer,
    /// `crates/bridges/vike-ibkr/src/execution/event_mapper.rs`'s `on_error`, which is fed only by the
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
        for c in [399, 404, 481, 999999] {
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
