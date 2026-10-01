use super::*;
use crate::transport::E_TIMEOUT_AMBIGUOUS;

fn err(code: i64, msg: &str) -> VenueApiError {
    VenueApiError { code, msg: msg.to_string() }
}

/// A test taxonomy: one exact code plus one message substring.
const TEST_TAX: VenueTaxonomy = VenueTaxonomy {
    venue: "test",
    by_code: |code| match code {
        777 => Some(ErrorKind::InsufficientFunds),
        // Deliberately CONTRADICTS the central baseline (which maps -2011 → NotFound) so the
        // precedence assertion below is unambiguous.
        -2011 => Some(ErrorKind::VenueMaintenance),
        _ => None,
    },
    by_msg: |msg| {
        msg.to_ascii_lowercase().contains("insufficient").then_some(ErrorKind::InsufficientFunds)
    },
};

/// THE inertness guarantee: with no taxonomy, `classify_venue` IS `err.kind()`. This is what
/// makes the whole layer additive.
#[test]
fn no_taxonomy_is_exactly_the_central_baseline() {
    for e in [
        err(E_TIMEOUT_AMBIGUOUS, "timed out"),
        err(0, "dns"),
        err(429, "http error"),
        err(-2011, "unknown order"),
        err(50011, ""),
        err(999_999, "novel"),
    ] {
        assert_eq!(classify_venue(None, &e), e.kind(), "code {} drifted", e.code);
    }
}

#[test]
fn venue_code_table_wins_over_the_central_baseline() {
    let e = err(-2011, "unknown order");
    assert_eq!(e.kind(), ErrorKind::NotFound, "baseline");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::VenueMaintenance, "table wins");
}

#[test]
fn msg_table_is_the_fallback_when_the_code_table_misses() {
    // 424 is a plain 4xx the baseline calls InvalidRequest; the msg table reclassifies it.
    let e = err(424, "Account has insufficient balance for requested action");
    assert_eq!(e.kind(), ErrorKind::InvalidRequest, "baseline");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::InsufficientFunds);
}

#[test]
fn code_table_takes_precedence_over_msg_table() {
    // 777 is in the code table; the msg would ALSO match a different rule if consulted.
    let e = err(777, "totally unrelated text");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::InsufficientFunds);
}

/// An unknown code under a taxonomy that has no rule for it still lands on the safe posture.
#[test]
fn unknown_falls_through_to_the_terminal_default() {
    let e = err(424_242, "some novel venue failure");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::Unknown);
    assert!(ErrorKind::Unknown.is_terminal(), "unknown must be the default-Fatal posture");
}

/// The ambiguous timeout must survive a venue taxonomy: even a table that (wrongly) claims the
/// sentinel cannot turn it into a retry or a reject, because `classify` handles the sentinel
/// before any venue code — and `submit_disposition` checks `must_requery` first.
#[test]
fn ambiguous_timeout_survives_a_taxonomy() {
    let e = err(E_TIMEOUT_AMBIGUOUS, "timed out");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::Timeout);
    assert_eq!(submit_disposition(ErrorKind::Timeout), SubmitDisposition::Requery);
}

#[test]
fn dispositions_match_the_kind_semantics() {
    for k in [
        ErrorKind::RateLimited,
        ErrorKind::ServerError,
        ErrorKind::Network,
        ErrorKind::VenueMaintenance,
    ] {
        assert_eq!(submit_disposition(k), SubmitDisposition::Retry(k), "{k} should retry");
        assert!(k.backoff_scale() > 0, "{k} needs a backoff");
    }
    for k in [
        ErrorKind::Auth,
        ErrorKind::InvalidRequest,
        ErrorKind::NotFound,
        ErrorKind::InsufficientFunds,
        ErrorKind::Unknown,
    ] {
        assert_eq!(submit_disposition(k), SubmitDisposition::Reject(k), "{k} should reject");
        assert!(k.is_terminal(), "{k} is terminal");
        assert_eq!(k.backoff_scale(), 0, "{k} must not carry a backoff");
    }
    assert_eq!(submit_disposition(ErrorKind::Timeout), SubmitDisposition::Requery);
    assert!(!ErrorKind::Timeout.is_terminal(), "the ambiguous path is never terminal");
}

/// Maintenance waits far longer than an ordinary blip — that distinction is the whole reason it
/// is its own kind rather than another `ServerError`.
#[test]
fn maintenance_backs_off_much_longer_than_a_server_blip() {
    assert!(
        ErrorKind::VenueMaintenance.backoff_scale() > ErrorKind::ServerError.backoff_scale() * 4
    );
}

#[test]
fn reject_reason_names_the_kind_and_keeps_the_venue_text() {
    assert_eq!(
        reject_reason(ErrorKind::InsufficientFunds, "Wallet balance is insufficient"),
        "venue error [insufficient_funds]: Wallet balance is insufficient"
    );
    assert_eq!(reject_reason(ErrorKind::Auth, ""), "venue error [auth]");
}

/// REGRESSION (adversarial review): the parity argument for this whole lane is that
/// `ErrorKind::classify` NEVER returns the two ADDITIVE variants — that is the sole reason
/// adding `VenueMaintenance` to `is_retryable` cannot change hyperliquid's LIVE transport retry
/// or its `must_requery` submit arms. That invariant was prose only. Pin it: sweep the sentinels,
/// the whole HTTP-status range, the seeded venue codes, and a spread of unknown codes with
/// messages that WOULD match the venue tables' substrings.
#[test]
fn central_classify_never_yields_the_additive_variants() {
    let msgs = [
        "",
        "Wallet balance is insufficient",
        "insufficient margin",
        "system maintenance",
        "service is restarting",
        "upgrade in progress",
    ];
    let codes = (0..700i64)
        .chain(-2100..=-1000)
        .chain(10000..10100)
        .chain(110000..110200)
        .chain(50000..51000)
        .chain([E_TIMEOUT_AMBIGUOUS, i64::MAX, -1, 999_999]);
    for code in codes {
        for msg in msgs {
            let k = ErrorKind::classify(code, msg);
            assert!(
                k != ErrorKind::InsufficientFunds && k != ErrorKind::VenueMaintenance,
                "classify({code}, {msg:?}) = {k}: the additive variants must stay unreachable \
                     from the central baseline — hyperliquid's live retry depends on it"
            );
        }
    }
}

/// REGRESSION (adversarial review): `by_msg` is an unanchored substring heuristic and must never
/// flip the DISPOSITION of a code the baseline already classifies — in either direction.
#[test]
fn by_msg_may_refine_but_never_flip_a_baseline_disposition() {
    // A hostile msg table that claims everything is a (retryable) maintenance window.
    const HOSTILE: VenueTaxonomy = VenueTaxonomy {
        venue: "hostile",
        by_code: |_| None,
        by_msg: |_| Some(ErrorKind::VenueMaintenance),
    };
    // 400 is terminal at the baseline; the msg table must NOT make it retryable.
    let e = err(400, "anything at all");
    assert_eq!(e.kind(), ErrorKind::InvalidRequest);
    assert_eq!(classify_venue(Some(&HOSTILE), &e), ErrorKind::InvalidRequest, "flip blocked");
    // …and the reverse direction: a terminal claim over a retryable baseline.
    const HOSTILE2: VenueTaxonomy = VenueTaxonomy {
        venue: "hostile2",
        by_code: |_| None,
        by_msg: |_| Some(ErrorKind::InsufficientFunds),
    };
    let e5 = err(503, "insufficient upstream capacity");
    assert_eq!(e5.kind(), ErrorKind::ServerError);
    assert_eq!(classify_venue(Some(&HOSTILE2), &e5), ErrorKind::ServerError, "flip blocked");
    assert!(classify_venue(Some(&HOSTILE2), &e5).is_retryable());
    // A same-disposition REFINEMENT still lands (this is what by_msg is FOR).
    let e4 = err(-2010, "Account has insufficient balance for requested action");
    assert_eq!(e4.kind(), ErrorKind::InvalidRequest, "baseline terminal");
    assert_eq!(classify_venue(Some(&HOSTILE2), &e4), ErrorKind::InsufficientFunds);
    // And where the baseline has NO opinion, by_msg wins outright.
    let eu = err(987_654, "anything");
    assert_eq!(eu.kind(), ErrorKind::Unknown);
    assert_eq!(classify_venue(Some(&HOSTILE), &eu), ErrorKind::VenueMaintenance);
}

#[test]
fn retry_budget_bounds_by_attempts_and_by_elapsed_time() {
    let b = SubmitRetryBudget { max_attempts: 3, max_elapsed_ms: 1_000 };
    assert!(!b.exhausted(SubmitAttempt::first()));
    assert!(!b.exhausted(SubmitAttempt { attempt: 2, elapsed_ms: 10 }));
    assert!(b.exhausted(SubmitAttempt { attempt: 3, elapsed_ms: 10 }), "attempt ceiling");
    assert!(b.exhausted(SubmitAttempt { attempt: 1, elapsed_ms: 1_000 }), "time ceiling");
    // A zero/one attempt budget means "never retry" — not "retry forever".
    assert!(
        SubmitRetryBudget { max_attempts: 0, max_elapsed_ms: 0 }.exhausted(SubmitAttempt::first())
    );
    // No time bound configured => attempts alone decide.
    let untimed = SubmitRetryBudget { max_attempts: 4, max_elapsed_ms: 0 };
    assert!(!untimed.exhausted(SubmitAttempt { attempt: 2, elapsed_ms: u64::MAX }));
    assert_eq!(SubmitAttempt::first().next(50), SubmitAttempt { attempt: 2, elapsed_ms: 50 });
}

#[test]
fn taxonomy_lookup_feeds_the_kind_with_hook() {
    let e = err(777, "");
    assert_eq!(e.kind_with(|x| TEST_TAX.lookup(x)), ErrorKind::InsufficientFunds);
    assert_eq!(TEST_TAX.venue, "test");
}
