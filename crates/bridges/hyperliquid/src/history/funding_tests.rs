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
            assert_eq!(f.timestamp, 1000);
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
    let mut poller = HyperliquidFundingPoller::new(
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
    let mut poller = HyperliquidFundingPoller::new(
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
            assert_eq!(edge.timestamp, 5_000, "ts == floor is emitted (floor is inclusive)");
            assert_eq!(new.timestamp, 9_000);
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
    let mut poller = HyperliquidFundingPoller::new(
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

/// The venue's own words: a MAINNET account's `userFunding` page, read keyless from `/info` on
/// 2026-11-08 over three hours (500 rows, ~177 coins an hour), its BTC, ETH and SOL rows kept verbatim.
/// Every funding row the venue lists states the ZERO hash, so the hash keys nothing.
const MAINNET: &str = include_str!("../../tests/fixtures/hl_user_funding_mainnet.json");

fn recorded() -> Vec<FundingPayment> {
    let page: Value = serde_json::from_str(MAINNET).expect("the fixture parses");
    parse_user_funding(&page["mainnet_2026_11_08"].to_string()).expect("the recorded page decodes")
}

fn amounts(events: &[Event]) -> Vec<(i64, String, f64)> {
    events
        .iter()
        .map(|e| match e {
            Event::Funding(f) => (f.timestamp, f.symbol.to_string(), f.amount),
            other => panic!("not a funding event: {other:?}"),
        })
        .collect()
}

/// **Each recorded payment emits ONCE.** Nine payments (three coins, three funding hours) under ONE
/// hash: a hash seen-set emitted the first and dropped the other eight as its duplicates. The second
/// poll re-reads the same rows (the overlap window) and emits nothing.
#[test]
fn every_recorded_payment_under_the_zero_hash_emits_once() {
    let rows = recorded();
    assert_eq!(rows.len(), 9);
    assert!(rows.iter().all(|p| p.hash == format!("0x{}", "0".repeat(64))), "the recorded hash");
    let served = rows.clone();
    let mut poller = HyperliquidFundingPoller::new(|_, _| Ok(served.clone()), "hyperliquid", 0);
    let first = amounts(&poller.poll(1_794_152_320_000).unwrap());
    let want: Vec<(i64, String, f64)> =
        rows.iter().map(|p| (p.time_ms, p.coin.clone(), p.usdc)).collect();
    assert_eq!(first, want, "every payment, in the venue's order");
    assert!(poller.poll(1_794_152_380_000).unwrap().is_empty(), "no duplicates on the next poll");
}

/// A FULL answer (the venue's 500-row cap) moves the next window's start only to its newest row less
/// the overlap, so the rows the cap cut off are read next; a short answer moves it to `now`.
#[test]
fn a_full_page_is_read_on_from_its_newest_row() {
    const T: i64 = 1_794_142_800_025;
    const CAP: usize = 500; // the venue's measured page (`USER_FUNDING_PAGE_CAP`)
    let full: Vec<FundingPayment> = (0..CAP)
        .map(|i| FundingPayment {
            time_ms: T,
            coin: format!("C{i}"),
            usdc: 0.01,
            szi: 1.0,
            funding_rate: 0.0000125,
            hash: format!("0x{}", "0".repeat(64)),
        })
        .collect();
    let starts = std::cell::RefCell::new(Vec::new());
    let mut poller = HyperliquidFundingPoller::new(
        |start, _| {
            starts.borrow_mut().push(start);
            Ok(if starts.borrow().len() == 1 { full.clone() } else { Vec::new() })
        },
        "hyperliquid",
        T - 2 * OVERLAP_MS,
    );
    assert_eq!(poller.poll(T + 10_800_000).unwrap().len(), CAP);
    poller.poll(T + 10_860_000).unwrap();
    poller.poll(T + 10_920_000).unwrap();
    assert_eq!(
        *starts.borrow(),
        vec![T - 2 * OVERLAP_MS, T - OVERLAP_MS, T + 10_860_000 - OVERLAP_MS]
    );
}

/// **The engine books each payment once and the wallet does not count it again.** An engine on `BTC`
/// folds the recorded page: the three BTC payments (the other coins are not its symbol) move the funding
/// total, own PnL (the drawdown latch's PnL term) and equity by their sum, once; the next poll books
/// nothing. A reconcile pass against the wallet that holds them finds no drift (an engine that never
/// heard of them holds a drift of exactly them), and adopting the wallet moves neither equity nor own PnL.
#[test]
fn the_engine_books_the_recorded_payments_once_and_the_wallet_agrees() {
    use vike_execution::recon::{
        ReconcileBalanceCheck, ReconcileBalanceTolerance, ReconcileDivergence, diff_balance,
    };
    use vike_execution::testing::RecordingClient;
    use vike_execution::{
        Account, BalanceMode, Engine, EventBusOutbox, EventHandler, PriceBoardConfig, RiskGate,
    };
    use vike_model::events::AccountState;

    let engine = || {
        Engine::new(
            Account::new(1.0, "hyperliquid", None, BalanceMode::Delta),
            RiskGate::new(vike_model::RiskLimits::new()),
            RecordingClient::default(),
            "hyperliquid",
            "BTC",
        )
    };
    let adopt = |e: &mut Engine<RecordingClient>, cash: f64| {
        let label = e.quote_asset.clone();
        e.account.apply_account_state(
            &AccountState {
                venue: "hyperliquid".into(),
                balances: vec![(label.clone(), cash)],
                timestamp: 0,
                route_key: None,
            },
            &label,
        );
    };
    let rows = recorded();
    let paid: f64 = rows.iter().filter(|p| p.coin == "BTC").map(|p| p.usdc).sum();
    assert!((paid - 0.152_778).abs() < 1e-12, "{paid}");
    let (before, wallet) = (1_000.0, 1_000.0 + paid);

    let served = rows.clone();
    let mut poller = HyperliquidFundingPoller::new(|_, _| Ok(served.clone()), "hyperliquid", 0);
    let config = PriceBoardConfig::default();
    let mut e = engine();
    adopt(&mut e, before);
    let (equity0, own0) = (e.resolved_equity(0.0, &config), e.resolved_own_pnl(&config));
    let mut ob = EventBusOutbox::default();
    for event in poller.poll(1_794_152_320_000).unwrap() {
        e.on_event(&event, &mut ob);
    }
    assert!((e.account.funding_paid - paid).abs() < 1e-12, "{}", e.account.funding_paid);
    assert!((e.resolved_own_pnl(&config) - (own0 + paid)).abs() < 1e-9, "own PnL moved once");
    let equity1 = e.resolved_equity(0.0, &config);
    assert!((equity1 - (equity0 + paid)).abs() < 1e-9, "equity moved once");
    let own1 = e.resolved_own_pnl(&config);
    for event in poller.poll(1_794_152_380_000).unwrap() {
        e.on_event(&event, &mut ob);
    }
    assert!((e.account.funding_paid - paid).abs() < 1e-12, "no second booking");

    let tolerance = ReconcileBalanceTolerance { abs_floor: 1e-9, rel_frac: 0.0 };
    match diff_balance(&e.local_view().as_view(), Some(wallet), &e.quote_asset, tolerance, 0) {
        ReconcileBalanceCheck::Anchored { drift: None, .. } => {}
        other => panic!("the books must agree with the wallet: {other:?}"),
    }
    let mut blind = engine();
    adopt(&mut blind, before);
    match diff_balance(
        &blind.local_view().as_view(),
        Some(wallet),
        &blind.quote_asset,
        tolerance,
        0,
    ) {
        ReconcileBalanceCheck::Anchored {
            drift: Some(ReconcileDivergence::BalanceDrift { local, venue_balance, .. }),
            ..
        } => assert!((venue_balance - local - paid).abs() < 1e-9, "the wallet is ahead by them"),
        other => panic!("an engine that never booked the funding holds a drift: {other:?}"),
    }
    adopt(&mut e, wallet);
    assert!((e.resolved_equity(0.0, &config) - equity1).abs() < 1e-9, "the wallet holds it once");
    assert_eq!(
        e.resolved_own_pnl(&config).to_bits(),
        own1.to_bits(),
        "an adoption writes no own PnL"
    );
}

/// A fetch failure surfaces as `Err` (never panics, so the caller can rate-limit a warn) and
/// does NOT advance the watermark — the next poll retries the same window rather than
/// silently skipping it.
#[test]
fn poller_fetch_failure_is_err_and_does_not_advance_watermark() {
    let mut first_call = true;
    let mut poller = HyperliquidFundingPoller::new(
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
fn parse_hyperliquid_decodes_timestamp_and_string_rate() {
    let body = r#"[
            {"coin":"BTC","fundingRate":"0.0000125","premium":"0.0001","time":1700000000000},
            {"coin":"BTC","fundingRate":"-0.0000088","premium":"-0.0002","time":1700003600000}
        ]"#;
    let pts = parse_funding_history(body).unwrap();
    assert_eq!(pts.len(), 2);
    assert_eq!(pts[0].timestamp_ms, 1_700_000_000_000);
    assert!((pts[0].rate - 0.000_012_5).abs() < 1e-15);
    assert_eq!(pts[1].timestamp_ms, 1_700_003_600_000);
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
/// [`FundingRateSourceHyperliquid::fetch`] sent a spot pair's unified `symbol` (`HYPE/USDC`) straight through
/// as the wire `coin`, which this one-symbol seam can only express as `coin == symbol` — a request
/// no `identity_coin_for` check ever stood in front of.
#[test]
fn the_market_funding_source_refuses_a_spot_pair_before_the_venue_is_asked() {
    use vike_data::source::FundingRateSource;

    let seed = vike_venues::venue_urls::VenueUrls::seed(crate::transport::consts::VENUE);
    let row = FundingRateSourceHyperliquid::from_urls(&seed).expect("the seed's live data row");
    assert_eq!(
        row.rest, "https://api.hyperliquid.xyz",
        "mainnet, byte for byte, whatever the tier"
    );
    let refused = row
        .fetch("HYPE/USDC", 0, 1)
        .expect_err("a non-identity symbol is refused by the one-symbol seam");
    assert_matches!(
        refused,
        vike_data::source::SourceError::Refused(_),
        "it must be a REFUSAL, not a fetch failure: {refused}"
    );
    assert!(refused.to_string().contains("vike-catalog"), "names the way forward: {refused}");
}
