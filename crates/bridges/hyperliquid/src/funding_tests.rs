use super::*;
use std::assert_matches;

/// A realistic `userFunding` array: a PAID (negative `usdc`, long `szi`) row and a RECEIVED
/// (positive `usdc`, short `szi`) row — count, signed `usdc`, `coin`, `szi`, rate, time, hash.
#[test]
fn parse_user_funding_signed_paid_and_received() {
    // Top-level `time` is a NUMBER; hash/coin/usdc/szi/fundingRate are STRINGS.
    let body = r#"[
            {"time":1681222254710,"hash":"0xabc","delta":{"type":"funding","coin":"BTC","usdc":"-1.25","szi":"0.5","fundingRate":"0.0000125"}},
            {"time":1681222254720,"hash":"0xdef","delta":{"type":"funding","coin":"ETH","usdc":"0.75","szi":"-2.0","fundingRate":"-0.0000088"}}
        ]"#;
    let r = parse_user_funding(body).unwrap();
    assert_eq!(r.len(), 2, "both funding rows parsed");

    let paid = &r[0];
    assert_eq!(paid.time_ms, 1681222254710);
    assert_eq!(paid.coin, "BTC", "coin carried verbatim");
    assert_eq!(paid.usdc, -1.25, "negative usdc = paid, signed as-is");
    assert_eq!(paid.szi, 0.5, "long position size");
    assert_eq!(paid.funding_rate, 0.0000125);
    assert_eq!(paid.hash, "0xabc");

    let received = &r[1];
    assert_eq!(received.coin, "ETH");
    assert_eq!(received.usdc, 0.75, "positive usdc = received");
    assert_eq!(received.szi, -2.0, "short position size (signed)");
    assert_eq!(received.funding_rate, -0.0000088, "negative funding rate survives");
    assert_eq!(received.hash, "0xdef");
}

/// Non-`funding` deltas (the envelope is shared across HL ledger endpoints) and rows missing a
/// `delta` are skipped; only the real funding rows survive, in order.
#[test]
fn parse_user_funding_skips_non_funding_and_deltaless_rows() {
    let body = r#"[
            {"time":1,"hash":"0x1","delta":{"type":"funding","coin":"BTC","usdc":"-1.0","szi":"0.5","fundingRate":"0.00001"}},
            {"time":2,"hash":"0x2","delta":{"type":"deposit","usdc":"100.0"}},
            {"time":3,"hash":"0x3"},
            {"time":4,"hash":"0x4","delta":{"type":"funding","coin":"ETH","usdc":"0.5","szi":"-1.0","fundingRate":"-0.00002"}}
        ]"#;
    let r = parse_user_funding(body).unwrap();
    assert_eq!(r.len(), 2, "deposit + delta-less rows skipped");
    assert_eq!(r.iter().map(|p| p.coin.as_str()).collect::<Vec<_>>(), ["BTC", "ETH"]);
}

/// An empty array is an empty (Ok) result, not an error.
#[test]
fn parse_user_funding_empty_array_is_ok_empty() {
    assert_eq!(parse_user_funding("[]").unwrap(), Vec::new());
}

/// Malformed / non-array bodies are errors, never panics (mirrors the recon_client robustness).
#[test]
fn parse_user_funding_malformed_is_error_not_panic() {
    assert!(parse_user_funding("not json").is_err());
    assert!(parse_user_funding("{}").is_err(), "an object (not the expected array) is an error");
    assert!(parse_user_funding("null").is_err());
    assert!(parse_user_funding("42").is_err());
}

// --- live-fold producer ---

fn payment(hash: &str, coin: &str, usdc: f64, time_ms: i64) -> FundingPayment {
    FundingPayment {
        time_ms,
        coin: coin.to_string(),
        usdc,
        szi: 1.0,
        funding_rate: 0.0001,
        hash: hash.to_string(),
    }
}

/// The platform convention (`FundingEvent.amount` received-positive) maps DIRECTLY off HL's
/// already-signed `usdc` — no flip either direction, unlike bybit's WS-vs-REST sign trap.
#[test]
fn funding_payment_to_event_carries_the_signed_amount_unflipped() {
    let paid = funding_payment_to_event(&payment("0x1", "BTC", -1.25, 1000), "hyperliquid");
    match paid {
        Event::Funding(f) => {
            assert_eq!(f.venue, "hyperliquid");
            assert_eq!(f.symbol, "BTC");
            assert_eq!(f.position_side, PositionSide::Both);
            assert_eq!(f.amount, -1.25, "negative = paid, carried as-is");
            assert_eq!(f.funding_rate, 0.0001);
            assert_eq!(f.ts, 1000);
            assert_eq!(f.mark_price, None);
        }
        other => panic!("expected Event::Funding, got {other:?}"),
    }
    let received = funding_payment_to_event(&payment("0x2", "ETH", 0.75, 2000), "hyperliquid");
    match received {
        Event::Funding(f) => assert_eq!(f.amount, 0.75, "positive = received, carried as-is"),
        other => panic!("expected Event::Funding, got {other:?}"),
    }
}

/// The poll/spawn-glue seam: a canned `fetch` closure (NO network, NO transport fake needed —
/// [`HyperliquidTransport`] has no seam to fake) proves the watermark advances and new rows
/// decode to `Event::Funding`s.
#[test]
fn poller_emits_new_rows_and_advances_the_watermark() {
    let mut poller = HlFundingPoller::new(
        |start, end| {
            assert_eq!(start, 0, "first poll's window floors at 0, never negative");
            assert_eq!(end, 10_000);
            Ok(vec![payment("0xabc", "BTC", 1.5, 500)])
        },
        "hyperliquid",
        0, // unfloored: watermark(0) - OVERLAP_MS would go negative -> clamped to 0
    );
    let evs = poller.poll(10_000).unwrap();
    match evs.as_slice() {
        [Event::Funding(f)] => {
            assert_eq!(f.symbol, "BTC");
            assert_eq!(f.amount, 1.5);
        }
        other => panic!("expected one Event::Funding, got {other:?}"),
    }
}

/// The restart law: a poller floored at its spawn instant (as `spawn_funding_poll` does) must
/// NEVER emit a payment with `time_ms` BEFORE the floor — those are already embedded in the
/// authoritative venue balance — while a payment AT/after the floor is emitted exactly once.
/// Simulates the (re)start-with-empty-seen-set case the in-memory hash-dedup cannot cover.
#[test]
fn poller_never_emits_payments_before_the_spawn_floor() {
    const FLOOR: i64 = 5_000;
    let mut poller = HlFundingPoller::new(
        |start, _end| {
            assert_eq!(start, FLOOR, "the first window starts AT the floor, never before");
            // The venue answers with history straddling the floor anyway (defensive: the
            // filter, not the request window, is the guarantee).
            Ok(vec![
                payment("0xold", "BTC", 9.99, 1_000),
                payment("0xedge", "BTC", 0.25, 5_000),
                payment("0xnew", "ETH", 1.5, 9_000),
            ])
        },
        "hyperliquid",
        FLOOR,
    );
    let evs = poller.poll(10_000).unwrap();
    match evs.as_slice() {
        [Event::Funding(edge), Event::Funding(new)] => {
            assert_eq!(edge.ts, 5_000, "ts == floor is emitted (floor is inclusive)");
            assert_eq!(new.ts, 9_000);
            assert_eq!(new.amount, 1.5);
        }
        other => panic!("expected exactly the at/post-floor payments, got {other:?}"),
    }
    // The next cadence re-serves the same rows (overlap window): nothing re-emits.
    assert!(poller.poll(10_060).unwrap().is_empty(), "no duplicates on the second poll");
}

/// A row whose `hash` the poller already emitted (the overlap window re-fetching it) is NOT
/// re-emitted — the precise dedup key doing its job across two overlapping polls.
#[test]
fn poller_dedupes_repeated_hashes_across_polls() {
    let mut poller = HlFundingPoller::new(
        // Every call returns the SAME row (as the overlap window would re-fetch it).
        |_start, _end| Ok(vec![payment("0xdup", "BTC", 2.0, 500)]),
        "hyperliquid",
        0,
    );
    let first = poller.poll(1_000).unwrap();
    assert_eq!(first.len(), 1, "first sighting emits");
    let second = poller.poll(2_000).unwrap();
    assert!(second.is_empty(), "the same hash on the next poll must not re-emit");
}

/// A fetch failure surfaces as `Err` (never panics, so the caller can rate-limit a warn) and
/// does NOT advance the watermark — the next poll retries the same window rather than
/// silently skipping it.
#[test]
fn poller_fetch_failure_is_err_and_does_not_advance_watermark() {
    let mut first_call = true;
    let mut poller = HlFundingPoller::new(
        move |start, _end| {
            if first_call {
                first_call = false;
                return Err("transient failure".to_string());
            }
            // The retry's window start must be the SAME as if the failed poll never happened —
            // i.e. still floored from the original watermark, not advanced past it.
            assert_eq!(start, 0);
            Ok(vec![payment("0xretry", "BTC", 3.0, 500)])
        },
        "hyperliquid",
        0,
    );
    assert!(poller.poll(100_000).is_err(), "a fetch failure surfaces as Err, never panics");
    let retried = poller.poll(100_000).unwrap();
    assert_eq!(retried.len(), 1, "the retry succeeds against the SAME (unadvanced) window");
}

// --- market funding-rate parser (moved from vike-backfill's funding_rate_tests.rs, 0094) -------

/// The real `fundingHistory` shape: `time` is a NUMBER; `fundingRate`/`premium` are STRINGS, and
/// the timestamp is keyed on `time` (not `fundingTime`).
#[test]
fn parse_hyperliquid_decodes_ts_and_string_rate() {
    let body = r#"[
            {"coin":"BTC","fundingRate":"0.0000125","premium":"0.0001","time":1700000000000},
            {"coin":"BTC","fundingRate":"-0.0000088","premium":"-0.0002","time":1700003600000}
        ]"#;
    let pts = parse_funding_history(body).unwrap();
    assert_eq!(pts.len(), 2);
    assert_eq!(pts[0].ts_ms, 1_700_000_000_000);
    assert!((pts[0].rate - 0.000_012_5).abs() < 1e-15);
    assert_eq!(pts[1].ts_ms, 1_700_003_600_000);
    assert!((pts[1].rate + 0.000_008_8).abs() < 1e-15, "negative rate survives");
}

#[test]
fn parse_hyperliquid_empty_array_is_ok_empty() {
    assert_eq!(parse_funding_history("[]").unwrap(), Vec::new());
}

#[test]
fn parse_hyperliquid_malformed_or_non_array_is_error() {
    assert!(parse_funding_history("not json").is_err());
    assert!(parse_funding_history("{}").is_err());
    assert!(parse_funding_history("42").is_err());
}

/// Hyperliquid ships `premium` beside `fundingRate` in the SAME row, and both are decimal
/// strings. This is the regression the whole change exists to prevent: the field was on the
/// wire and discarded.
#[test]
fn parse_hyperliquid_keeps_the_premium_beside_the_rate() {
    let body = r#"[
            {"coin":"BTC","fundingRate":"0.0000125","premium":"0.0003354037","time":1700000000000},
            {"coin":"BTC","fundingRate":"-0.0000088","premium":"-0.0006236109","time":1700003600000}
        ]"#;
    let pts = parse_funding_history(body).unwrap();
    assert_eq!(pts.len(), 2);
    assert!((pts[0].premium.unwrap() - 0.000_335_403_7).abs() < 1e-15);
    assert!(
        (pts[1].premium.unwrap() + 0.000_623_610_9).abs() < 1e-15,
        "a negative premium survives — the perp traded BELOW its oracle"
    );
}

/// A present-but-unparseable premium degrades to `None` WITHOUT dropping the row — one step
/// softer than the rate's own tolerance, which skips the row entirely.
#[test]
fn an_unparseable_or_nonfinite_premium_degrades_to_none_and_keeps_the_row() {
    for bad in ["\"not-a-number\"", "\"NaN\"", "\"inf\"", "0.0003", "null"] {
        let body = format!(
            r#"[{{"coin":"BTC","fundingRate":"0.0000125","premium":{bad},"time":1700000000000}}]"#
        );
        let pts = parse_funding_history(&body).unwrap();
        assert_eq!(pts.len(), 1, "the rate row survives a bad premium ({bad})");
        assert_eq!(pts[0].premium, None, "premium {bad} must not become a number");
    }
}

// --- routing: the identity_coin_for gate (0094 follow-ups, D5) ---------------------------------

/// The MARKET funding-rate source refuses a SPOT pair before the venue is asked anything — the
/// funding twin of `history_tests.rs`'s
/// `the_registry_row_refuses_a_spot_pair_before_the_venue_is_asked`. Before this gate was applied,
/// [`HyperliquidFunding::fetch`] sent a spot pair's unified `symbol` (`HYPE/USDC`) straight through
/// as the wire `coin`, which this one-symbol seam can only express as `coin == symbol` — a request
/// no `identity_coin_for` check ever stood in front of.
#[test]
fn the_market_funding_source_refuses_a_spot_pair_before_the_venue_is_asked() {
    use vike_data::source::FundingRateSource;

    let refused = HyperliquidFunding
        .fetch("HYPE/USDC", 0, 1)
        .expect_err("a non-identity symbol is refused by the one-symbol seam");
    assert_matches!(
        refused,
        vike_data::source::SourceError::Refused(_),
        "it must be a REFUSAL, not a fetch failure: {refused}"
    );
    assert!(refused.to_string().contains("vike-catalog"), "names the way forward: {refused}");
}
