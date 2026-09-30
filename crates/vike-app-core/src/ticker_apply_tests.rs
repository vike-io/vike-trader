use super::*;
use std::collections::BTreeMap;
use vike_deribit::options_feed::TickerRow;
use vike_options::{Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow, UnderlyingKind};

const NOW: i64 = 1_780_387_200_000; // 2026-06-02 08:00 UTC (chain.rs's NOW)

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

/// A full ticker row for `name`. `underlying_price` is deliberately DIFFERENT from the seeded
/// chain spot (100k) to prove the fold scales by the CHAIN spot, not this per-row value.
fn row(name: &str) -> TickerRow {
    TickerRow {
        instrument_name: name.into(),
        best_bid: Some(0.1015),
        best_ask: Some(0.106),
        mark_price: Some(0.1036),
        mark_iv: Some(66.33),
        open_interest: Some(1.0),
        volume: Some(2.0),
        underlying_price: Some(99_000.0),
    }
}

#[test]
fn folds_bidask_usd_scaled_iv_percent_and_reenriches_greeks() {
    let mut by = seeded_btc();
    let n = apply_ticker_to_chains(&mut by, &[row("BTC-27JUN26-100000-C")], NOW, 0.0);
    assert_eq!(n, 1);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    // coin → USD by the CHAIN spot (100k), NOT the ticker's own underlying_price (99k)
    assert_eq!(call.bid, Some(0.1015 * 100_000.0), "bid coin→USD by chain spot");
    assert_eq!(call.ask, Some(0.106 * 100_000.0), "ask coin→USD by chain spot");
    assert_eq!(call.mark, Some(0.1036 * 100_000.0), "mark coin→USD");
    assert_eq!(call.iv, Some(0.6633), "mark_iv PERCENT → decimal (÷100)");
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
        row("BTC-PERPETUAL"),        // not an option — parse_instrument_name rejects it
    ];
    assert_eq!(apply_ticker_to_chains(&mut by, &rows, NOW, 0.0), 0, "nothing matched");
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.bid, Some(9.9), "seeded quote untouched");
}

#[test]
fn absent_spot_yields_absent_bidask_not_zero() {
    let mut by = seeded_btc();
    by.get_mut("BTC").unwrap().chains.get_mut("2026-06-27").unwrap().underlying_price = None;
    assert_eq!(apply_ticker_to_chains(&mut by, &[row("BTC-27JUN26-100000-C")], NOW, 0.0), 1);
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
    let sparse = TickerRow {
        instrument_name: "BTC-27JUN26-100000-C".into(),
        best_bid: Some(0.2),
        best_ask: Some(0.21),
        mark_price: None,
        mark_iv: None,
        open_interest: None,
        volume: None,
        underlying_price: None,
    };
    assert_eq!(apply_ticker_to_chains(&mut by, &[sparse], NOW, 0.0), 1);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.bid, Some(0.2 * 100_000.0), "bid updated");
    assert_eq!(call.mark, Some(1.0), "mark preserved (ticker carried none)");
    assert_eq!(call.iv, Some(0.10), "iv preserved (ticker carried none)");
}

#[test]
fn sol_usdc_bidask_is_usd_quoted_not_spot_scaled() {
    // SOL rides the shared USDC book: its ticker bid/ask are ALREADY USD, so the fold must NOT
    // scale by spot (parse_instrument_name strips _USDC → base "SOL" → is_usd_quoted → scale 1.0).
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
    let rows = vec![TickerRow {
        instrument_name: "SOL_USDC-25SEP26-45-P".into(),
        best_bid: Some(0.24), // ALREADY USD — must pass through, NOT × 75.64
        best_ask: Some(0.26),
        mark_price: Some(0.25),
        mark_iv: Some(71.15),
        open_interest: Some(1.0),
        volume: Some(1.0),
        underlying_price: Some(75.64),
    }];
    assert_eq!(apply_ticker_to_chains(&mut by, &rows, NOW, 0.0), 1);
    let put = by["SOL"].chains["2026-09-25"].rows[0].put.as_ref().unwrap();
    assert_eq!(put.bid, Some(0.24), "USDC bid passed through unscaled (no × spot)");
    assert_eq!(put.ask, Some(0.26));
    assert_eq!(put.mark, Some(0.25));
    assert_eq!(put.iv, Some(0.7115));
    assert!(put.delta.is_some(), "greeks re-enriched from spot + IV");
}
