use super::*;
use std::collections::BTreeMap;
use vike_marketdata::{
    Expiry, OptionChain, OptionKind, OptionQuote, OptionTicker, StrikeRow, UnderlyingKind,
};

const NOW: i64 = 1_780_387_200_000; // 2026-06-02 08:00 UTC (chain.rs's NOW)

/// The option index every name these tests stream is looked up in (the markprice tests' set): the
/// ticker channel carries no strike, so the index is the ONLY source of one.
fn index() -> OptionIndex {
    use crate::chain::index::{test_delta_one, test_option};
    use OptionKind::{Call, Put};
    OptionIndex::from_instruments(&[
        test_option("BTC-27JUN26-100000-C", "BTC", "2026-06-27", 100_000.0, Call, "BTC"),
        test_option("BTC-27JUN26-100000-P", "BTC", "2026-06-27", 100_000.0, Put, "BTC"),
        test_option("BTC-27JUN26-999000-C", "BTC", "2026-06-27", 999_000.0, Call, "BTC"),
        test_option("BTC-25SEP26-100000-C", "BTC", "2026-09-25", 100_000.0, Call, "BTC"),
        test_option("ETH-27JUN26-3000-C", "ETH", "2026-06-27", 3_000.0, Call, "ETH"),
        test_option("SOL_USDC-25SEP26-45-P", "SOL", "2026-09-25", 45.0, Put, "USDC"),
        test_delta_one("BTC-PERPETUAL", "BTC"),
    ])
}

/// A one-strike BTC chain for 2026-06-27 (call+put), mimicking a REST-seeded grid the ticker then
/// updates. Spot 100k; each quote carries a stale bid/ask/mark/iv the stream overwrites.
fn seeded_btc() -> BTreeMap<String, UnderlyingChains> {
    let mk = |name: &str, kind: OptionKind| OptionQuote {
        bid: Some(9.9),
        ask: Some(9.9),
        mark: Some(1.0),
        iv: Some(0.10),
        instrument_name: Some(name.to_string()),
        ..OptionQuote::new(100_000.0, kind)
    };
    let chain = OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(100_000.0),
        expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
        asof_ms: NOW,
        source: "deribit".into(),
        rows: vec![StrikeRow {
            strike: 100_000.0,
            call: Some(mk("BTC-27JUN26-100000-C", OptionKind::Call)),
            put: Some(mk("BTC-27JUN26-100000-P", OptionKind::Put)),
        }],
    };
    let mut chains = BTreeMap::new();
    chains.insert("2026-06-27".to_string(), chain);
    let bundle = UnderlyingChains {
        default_expiry: "2026-06-27".into(),
        expiries: vec![Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() }],
        chains,
    };
    let mut by = BTreeMap::new();
    by.insert("BTC".to_string(), bundle);
    by
}

/// A full ticker row for `name`, as `vike_deribit::options_feed::parse_ticker` emits it: the IV is
/// ALREADY a decimal (`66.33 / 100.0`, the wire's percent divided at the deribit boundary).
/// `underlying_price` is deliberately DIFFERENT from the seeded chain spot (100k) to prove the fold
/// scales by the CHAIN spot, not this per-row value.
fn row(name: &str) -> OptionTicker {
    let mut t = OptionTicker::new(NOW, name);
    t.bid = Some(0.1015);
    t.ask = Some(0.106);
    t.mark = Some(0.1036);
    t.mark_iv = Some(66.33 / 100.0);
    t.open_interest = Some(1.0);
    t.volume = Some(2.0);
    t.underlying_price = Some(99_000.0);
    t
}

#[test]
fn folds_bidask_usd_scaled_iv_percent_and_reenriches_greeks() {
    let mut by = seeded_btc();
    let n = apply_ticker_to_chains(&mut by, &index(), &[row("BTC-27JUN26-100000-C")], NOW, 0.0);
    assert_eq!(n, 1);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    // coin → USD by the CHAIN spot (100k), NOT the ticker's own underlying_price (99k)
    assert_eq!(call.bid, Some(0.1015 * 100_000.0), "bid coin→USD by chain spot");
    assert_eq!(call.ask, Some(0.106 * 100_000.0), "ask coin→USD by chain spot");
    assert_eq!(call.mark, Some(0.1036 * 100_000.0), "mark coin→USD");
    assert_eq!(call.iv, Some(0.6633), "the row's decimal mark_iv stored verbatim (no second ÷100)");
    assert!(call.delta.is_some(), "greeks re-enriched from the fresh IV");
    // the put had no row this batch → untouched
    let put = by["BTC"].chains["2026-06-27"].rows[0].put.as_ref().unwrap();
    assert_eq!(put.iv, Some(0.10), "unrelated quote unchanged");
    assert_eq!(put.bid, Some(9.9));
}

#[test]
fn skips_unknown_instrument_underlying_and_strike() {
    let mut by = seeded_btc();
    let rows = vec![
        row("ETH-27JUN26-3000-C"),   // underlying never fetched
        row("BTC-27JUN26-999000-C"), // right underlying+expiry, strike outside the grid
        row("BTC-25SEP26-100000-C"), // wrong expiry (not in chains)
        row("BTC-PERPETUAL"),        // not an option — indexed as a future, no option address
    ];
    assert_eq!(apply_ticker_to_chains(&mut by, &index(), &rows, NOW, 0.0), 0, "nothing matched");
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.bid, Some(9.9), "seeded quote untouched");
}

#[test]
fn a_name_absent_from_the_index_is_skipped_not_parsed() {
    // A well-formed option name ON the grid that the index does not hold: skipped (no name-grammar
    // fallback), grid untouched, no panic.
    let mut by = seeded_btc();
    let empty = OptionIndex::default();
    assert_eq!(
        apply_ticker_to_chains(&mut by, &empty, &[row("BTC-27JUN26-100000-C")], NOW, 0.0),
        0
    );
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.bid, Some(9.9), "seeded quote untouched");
    assert_eq!(call.iv, Some(0.10));
}

#[test]
fn absent_spot_yields_absent_bidask_not_zero() {
    let mut by = seeded_btc();
    by.get_mut("BTC").unwrap().chains.get_mut("2026-06-27").unwrap().underlying_price = None;
    assert_eq!(
        apply_ticker_to_chains(&mut by, &index(), &[row("BTC-27JUN26-100000-C")], NOW, 0.0),
        1
    );
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.bid, None, "no spot → absent bid, never a fabricated 0.0");
    assert_eq!(call.ask, None);
    assert_eq!(call.mark, None);
    assert_eq!(call.iv, Some(0.6633), "IV still updates (spot-independent)");
}

#[test]
fn mark_and_iv_refined_only_when_carried() {
    // a sparse ticker (bid/ask only, no mark/iv) updates bid/ask but must NOT wipe a good mark/iv
    // (the markprice.options feed is their primary source).
    let mut by = seeded_btc();
    let mut sparse = OptionTicker::new(NOW, "BTC-27JUN26-100000-C");
    sparse.bid = Some(0.2);
    sparse.ask = Some(0.21);
    assert_eq!(apply_ticker_to_chains(&mut by, &index(), &[sparse], NOW, 0.0), 1);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.bid, Some(0.2 * 100_000.0), "bid updated");
    assert_eq!(call.mark, Some(1.0), "mark preserved (ticker carried none)");
    assert_eq!(call.iv, Some(0.10), "iv preserved (ticker carried none)");
}

#[test]
fn sol_usdc_bidask_is_usd_quoted_not_spot_scaled() {
    // SOL rides the shared USDC book: its ticker bid/ask are ALREADY USD, so the fold must NOT
    // scale by spot (the index says the option settles in USDC, its payload's settlement currency →
    // scale 1.0).
    let seeded = OptionQuote {
        bid: Some(9.9),
        ask: Some(9.9),
        mark: Some(1.0),
        iv: Some(0.10),
        instrument_name: Some("SOL_USDC-25SEP26-45-P".into()),
        ..OptionQuote::new(45.0, OptionKind::Put)
    };
    let chain = OptionChain {
        underlying: "SOL".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(75.64),
        expiry: Expiry { date: "2026-09-25".into(), dte: 90, label: "25 Sep".into() },
        asof_ms: NOW,
        source: "deribit".into(),
        rows: vec![StrikeRow { strike: 45.0, call: None, put: Some(seeded) }],
    };
    let mut chains = BTreeMap::new();
    chains.insert("2026-09-25".to_string(), chain);
    let mut by = BTreeMap::new();
    by.insert(
        "SOL".to_string(),
        UnderlyingChains {
            default_expiry: "2026-09-25".into(),
            expiries: vec![Expiry { date: "2026-09-25".into(), dte: 90, label: "25 Sep".into() }],
            chains,
        },
    );
    let mut sol = OptionTicker::new(NOW, "SOL_USDC-25SEP26-45-P");
    sol.bid = Some(0.24); // ALREADY USD — must pass through, NOT × 75.64
    sol.ask = Some(0.26);
    sol.mark = Some(0.25);
    sol.mark_iv = Some(71.15 / 100.0); // the wire's percent, divided by `parse_ticker`
    sol.open_interest = Some(1.0);
    sol.volume = Some(1.0);
    sol.underlying_price = Some(75.64);
    let rows = vec![sol];
    assert_eq!(apply_ticker_to_chains(&mut by, &index(), &rows, NOW, 0.0), 1);
    let put = by["SOL"].chains["2026-09-25"].rows[0].put.as_ref().unwrap();
    assert_eq!(put.bid, Some(0.24), "USDC bid passed through unscaled (no × spot)");
    assert_eq!(put.ask, Some(0.26));
    assert_eq!(put.mark, Some(0.25));
    assert_eq!(put.iv, Some(0.7115));
    assert!(put.delta.is_some(), "greeks re-enriched from spot + IV");
}
