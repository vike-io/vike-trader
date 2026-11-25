//! `classify_venue`'s layering over the venue-neutral layer, the unmapped-code rule and the order-write rule.

use super::*;
use crate::transport::E_TIMEOUT_AMBIGUOUS;
use vike_utility::errors::FailureOutcome;

fn err(code: i64, msg: &str) -> VenueApiError {
    VenueApiError { code, msg: msg.to_string() }
}

/// A test taxonomy: one exact code plus one message substring.
const TEST_TAX: VenueTaxonomy = VenueTaxonomy {
    venue: "test",
    by_code: |code| match code {
        777 => Some(ErrorKind::InsufficientFunds),
        -2011 => Some(ErrorKind::VenueMaintenance),
        -2010 => Some(ErrorKind::InvalidRequest),
        -1007 => Some(ErrorKind::Timeout),
        _ => None,
    },
    by_msg: |msg| {
        msg.to_ascii_lowercase().contains("insufficient").then_some(ErrorKind::InsufficientFunds)
    },
    leaves_write_open: |code| code == -1001,
};

/// THE inertness guarantee: with no taxonomy, `classify_venue` IS `err.kind()`.
#[test]
fn no_taxonomy_is_exactly_the_venue_neutral_layer() {
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
fn venue_code_table_wins_over_the_venue_neutral_layer() {
    let e = err(-2011, "unknown order");
    assert_eq!(e.kind(), ErrorKind::Unknown, "the neutral layer reads no venue's code");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::VenueMaintenance, "table wins");
}

#[test]
fn message_table_is_the_fallback_when_the_code_table_misses() {
    // 424 is a plain 4xx the neutral layer calls InvalidRequest; the msg table refines it.
    let e = err(424, "Account has insufficient balance for requested action");
    assert_eq!(e.kind(), ErrorKind::InvalidRequest, "neutral");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::InsufficientFunds);
}

#[test]
fn code_table_takes_precedence_over_an_unrelated_message() {
    let e = err(777, "totally unrelated text");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::InsufficientFunds);
}

/// A message refines a CODE row within its disposition (an umbrella code's real cause is only in its words), and
/// never flips it.
#[test]
fn a_message_refines_a_code_row_within_its_disposition_only() {
    let e = err(-2010, "Account has insufficient balance for requested action");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::InsufficientFunds, "refined");
    let bare = err(-2010, "some novel reject");
    assert_eq!(classify_venue(Some(&TEST_TAX), &bare), ErrorKind::InvalidRequest, "the row");
    const MAINTENANCE_WORDS: VenueTaxonomy =
        VenueTaxonomy { by_msg: |_| Some(ErrorKind::VenueMaintenance), ..TEST_TAX };
    assert_eq!(
        classify_venue(Some(&MAINTENANCE_WORDS), &bare),
        ErrorKind::InvalidRequest,
        "a terminal row is never made retryable by words"
    );
}

/// An unmapped VENUE code under a taxonomy is never final, its outcome unknown, whatever its words say. A
/// bridge's own code, an HTTP status and a code with rate-limit words keep their venue-neutral kinds.
#[test]
fn an_unmapped_venue_code_is_never_final() {
    let k = classify_venue(Some(&TEST_TAX), &err(424_242, "some novel venue failure"));
    assert_eq!(k, ErrorKind::UnmappedCode);
    assert_eq!(k.failure_outcome(), FailureOutcome::OutcomeUnknown);
    assert!(!k.is_terminal() && !k.is_retryable() && k.must_requery());
    let refusal_words = err(424_242, "Account has insufficient balance");
    assert_eq!(classify_venue(Some(&TEST_TAX), &refusal_words), ErrorKind::UnmappedCode, "no row");
    assert_eq!(TEST_TAX.lookup(&refusal_words), None);
    assert_eq!(classify_venue(Some(&TEST_TAX), &err(-9_001, "no row")), ErrorKind::Unknown);
    assert_eq!(classify_venue(Some(&TEST_TAX), &err(302, "moved")), ErrorKind::Unknown);
    assert_eq!(
        classify_venue(Some(&TEST_TAX), &err(424_243, "rate limit reached")),
        ErrorKind::RateLimited
    );
    assert_eq!(err(424_242, "x").kind(), ErrorKind::Unknown, "no table, no venue code");
}

/// The first sighting of each (venue, code) is ONE WARN naming the venue, the code and the words; the next is
/// silent. The codes are this test's own, so no other test can have seen them first. A code the write rule
/// leaves open (`leaves_write_open`, the server-trouble `-1001` here) is not reported: it has its reading.
#[test]
fn an_unmapped_code_is_reported_once_per_code() {
    let (_, events) = vike_log::capture::captured(|| {
        classify_venue(Some(&TEST_TAX), &err(31_337_001, "brand new words"));
        classify_venue(Some(&TEST_TAX), &err(31_337_001, "brand new words"));
        TEST_TAX.submit_outcome_unknown(31_337_002);
        TEST_TAX.submit_outcome_unknown(31_337_002);
        TEST_TAX.submit_outcome_unknown(-1001);
    });
    let warns: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::WARN).collect();
    assert_eq!(warns.len(), 2, "{events:?}");
    assert_eq!(warns[0].field("venue"), Some("test"));
    assert_eq!(warns[0].field("code"), Some("31337001"));
    assert_eq!(warns[0].field("words"), Some("brand new words"));
    assert_eq!(warns[1].field("code"), Some("31337002"));
}

/// The ambiguous timeout survives a taxonomy: `classify` handles the sentinel before any venue word.
#[test]
fn ambiguous_timeout_survives_a_taxonomy() {
    let e = err(E_TIMEOUT_AMBIGUOUS, "timed out");
    assert_eq!(classify_venue(Some(&TEST_TAX), &e), ErrorKind::Timeout);
}

/// The order-write rule: the sentinel, a `Timeout` row, a server-trouble code and an unmapped venue code leave the
/// write unknown; every other row (a `VenueMaintenance` one included), a pre-send failure, an HTTP status and a
/// bridge's own code are the venue's answer.
#[test]
fn the_order_write_rule_is_the_sentinel_a_timeout_row_server_trouble_or_an_unmapped_code() {
    for code in [E_TIMEOUT_AMBIGUOUS, -1007, -1001, 424_244] {
        assert!(TEST_TAX.submit_outcome_unknown(code), "{code}");
    }
    for code in [0, 777, -2010, -2011, 503, -9_001] {
        assert!(!TEST_TAX.submit_outcome_unknown(code), "{code}");
    }
}

/// Every kind's outcome, and the trio read off it: retryable = not sent or not now, re-query = outcome unknown,
/// terminal = refused.
#[test]
fn every_kind_has_one_outcome_and_the_trio_reads_it() {
    use vike_utility::errors::FailureOutcome::{NotNow, NotSent, OutcomeUnknown, Refused};
    for (kind, outcome) in [
        (ErrorKind::Network, NotSent),
        (ErrorKind::Timeout, OutcomeUnknown),
        (ErrorKind::UnmappedCode, OutcomeUnknown),
        (ErrorKind::RateLimited, NotNow),
        (ErrorKind::ServerError, NotNow),
        (ErrorKind::VenueMaintenance, NotNow),
        (ErrorKind::Auth, Refused),
        (ErrorKind::InvalidRequest, Refused),
        (ErrorKind::NotFound, Refused),
        (ErrorKind::InsufficientFunds, Refused),
        (ErrorKind::Unknown, Refused),
    ] {
        assert_eq!(kind.failure_outcome(), outcome, "{kind}");
        assert_eq!(kind.is_retryable(), matches!(outcome, NotSent | NotNow), "{kind}");
        assert_eq!(kind.must_requery(), outcome == OutcomeUnknown, "{kind}");
        assert_eq!(kind.is_terminal(), outcome == Refused, "{kind}");
    }
}

/// `ErrorKind::classify` reads no venue's code and never returns a kind only a venue table can give, swept over
/// the sentinels, the HTTP-status range, the codes the old central seed held (Binance, Bybit, OKX; deribit's
/// `10004` was read as Bybit's auth code), and messages that WOULD match a venue table's substrings.
#[test]
fn the_venue_neutral_layer_reads_no_venue_code() {
    let messages = [
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
        for msg in messages {
            let k = ErrorKind::classify(code, msg);
            assert!(
                !matches!(
                    k,
                    ErrorKind::InsufficientFunds
                        | ErrorKind::VenueMaintenance
                        | ErrorKind::UnmappedCode
                ),
                "classify({code}, {msg:?}) = {k}: only a venue table gives that kind"
            );
        }
    }
    for code in [-1003, -2015, -2011, -1001, -2010, 10004, 10006, 10016, 50011, 50113, 51603] {
        assert_eq!(ErrorKind::classify(code, ""), ErrorKind::Unknown, "{code} is its venue's");
    }
}

/// `by_msg` is an unanchored substring heuristic and must never flip the DISPOSITION of what the code
/// already says — in either direction.
#[test]
fn by_message_may_refine_but_never_flip_a_disposition() {
    // A hostile msg table that claims everything is a (retryable) maintenance window.
    const HOSTILE: VenueTaxonomy = VenueTaxonomy {
        venue: "hostile",
        by_code: |_| None,
        by_msg: |_| Some(ErrorKind::VenueMaintenance),
        leaves_write_open: |_| false,
    };
    // 400 is terminal in the neutral layer; the msg table must NOT make it retryable.
    let e = err(400, "anything at all");
    assert_eq!(e.kind(), ErrorKind::InvalidRequest);
    assert_eq!(classify_venue(Some(&HOSTILE), &e), ErrorKind::InvalidRequest, "flip blocked");
    // …and the reverse direction: a terminal claim over a retryable status.
    const HOSTILE2: VenueTaxonomy =
        VenueTaxonomy { by_msg: |_| Some(ErrorKind::InsufficientFunds), ..HOSTILE };
    let e5 = err(503, "insufficient upstream capacity");
    assert_eq!(e5.kind(), ErrorKind::ServerError);
    assert_eq!(classify_venue(Some(&HOSTILE2), &e5), ErrorKind::ServerError, "flip blocked");
    assert!(classify_venue(Some(&HOSTILE2), &e5).is_retryable());
    // A venue code with no row says OUTCOME UNKNOWN: words make it neither retryable nor refused.
    let unmapped = err(987_654, "anything");
    assert_eq!(classify_venue(Some(&HOSTILE), &unmapped), ErrorKind::UnmappedCode);
    assert_eq!(classify_venue(Some(&HOSTILE2), &unmapped), ErrorKind::UnmappedCode);
    // And where nothing has an opinion (a non-error status), by_msg wins outright.
    let eu = err(302, "anything");
    assert_eq!(eu.kind(), ErrorKind::Unknown);
    assert_eq!(classify_venue(Some(&HOSTILE), &eu), ErrorKind::VenueMaintenance);
}

#[test]
fn taxonomy_lookup_feeds_the_kind_with_hook() {
    let e = err(777, "");
    assert_eq!(e.kind_with(|x| TEST_TAX.lookup(x)), ErrorKind::InsufficientFunds);
    assert_eq!(TEST_TAX.venue, "test");
}
