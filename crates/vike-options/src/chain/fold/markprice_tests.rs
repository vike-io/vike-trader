use super::*;
use std::collections::BTreeMap;
use vike_marketdata::{
    Expiry, OptionChain, OptionKind, OptionQuote, OptionTicker, StrikeRow, UnderlyingKind,
};

const NOW: i64 = 1_780_387_200_000; // 2026-06-02 08:00 UTC (chain.rs's NOW)

/// A `markprice.options` row as `vike_deribit::options_feed::parse_markprice_options` emits it:
/// the venue name as `symbol`, `mark` in COIN units (USD on the USDC book), `mark_iv` the wire's
/// decimal `iv`, nothing else set.
fn mp(name: &str, mark: f64, iv: f64) -> OptionTicker {
    let mut t = OptionTicker::new(NOW, name);
    t.mark = Some(mark);
    t.mark_iv = Some(iv);
    t
}

/// The option index every name these tests stream is looked up in: the coin-settled BTC/ETH
/// options (strikes on and off the grid, a fetched and an unfetched expiry), the USDC-settled SOL
/// put, and the BTC perpetual (a future: indexed, but no option address).
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

/// A one-strike BTC chain for 2026-06-27 (call+put), mimicking a REST-seeded grid the WS stream
/// then updates. Spot 100k; each quote carries a stale mark/iv the stream overwrites.
fn seeded_btc() -> BTreeMap<String, UnderlyingChains> {
    let mk = |name: &str, kind: OptionKind| OptionQuote {
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

#[test]
fn folds_mark_usd_scaled_iv_verbatim_and_reenriches_greeks() {
    let mut by = seeded_btc();
    // mark 0.05 COIN units → USD = 0.05 * 100_000; iv 0.62 DECIMAL, stored verbatim
    let rows = vec![mp("BTC-27JUN26-100000-C", 0.05, 0.62)];
    let n = apply_markprice_to_chains(&mut by, &index(), &rows, NOW, 0.0);
    assert_eq!(n, 1);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.mark, Some(0.05 * 100_000.0), "coin→USD scaled by spot");
    assert_eq!(call.iv, Some(0.62), "decimal IV stored verbatim (no ÷100)");
    assert!(call.delta.is_some(), "greeks re-enriched from the fresh IV");
    // the put had no row this batch → untouched
    let put = by["BTC"].chains["2026-06-27"].rows[0].put.as_ref().unwrap();
    assert_eq!(put.iv, Some(0.10), "unrelated quote unchanged");
}

#[test]
fn skips_unknown_instrument_underlying_and_strike() {
    let mut by = seeded_btc();
    let rows = vec![
        // wrong underlying (never fetched)
        mp("ETH-27JUN26-3000-C", 0.1, 0.5),
        // right underlying+expiry, strike outside the grid
        mp("BTC-27JUN26-999000-C", 0.1, 0.5),
        // wrong expiry (not in chains)
        mp("BTC-25SEP26-100000-C", 0.1, 0.5),
        // a non-option instrument — indexed as a future, so it has no option address
        mp("BTC-PERPETUAL", 0.1, 0.5),
    ];
    assert_eq!(apply_markprice_to_chains(&mut by, &index(), &rows, NOW, 0.0), 0, "nothing matched");
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.mark, Some(1.0), "seeded quote untouched");
    assert_eq!(call.iv, Some(0.10));
}

#[test]
fn a_name_absent_from_the_index_is_skipped_not_parsed() {
    // The name is a well-formed option ON the grid, but the index does not hold it: the fold must
    // skip it (no name-grammar fallback), leave the grid untouched, and not panic. An empty index
    // (before the first fetch) skips every row the same way.
    let mut by = seeded_btc();
    let rows = vec![mp("BTC-27JUN26-100000-C", 0.05, 0.62)];
    assert_eq!(apply_markprice_to_chains(&mut by, &OptionIndex::default(), &rows, NOW, 0.0), 0);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.mark, Some(1.0), "seeded quote untouched");
    assert_eq!(call.iv, Some(0.10));
}

#[test]
fn absent_spot_yields_absent_mark_not_zero() {
    let mut by = seeded_btc();
    by.get_mut("BTC").unwrap().chains.get_mut("2026-06-27").unwrap().underlying_price = None;
    let rows = vec![mp("BTC-27JUN26-100000-C", 0.05, 0.62)];
    assert_eq!(apply_markprice_to_chains(&mut by, &index(), &rows, NOW, 0.0), 1);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.mark, None, "no spot → absent mark, never a fabricated 0.0");
    assert_eq!(call.iv, Some(0.62), "IV still updates (spot-independent)");
}

#[test]
fn sol_usdc_mark_is_usd_quoted_not_spot_scaled() {
    // SOL rides the shared USDC book: its markprice `mark_price` is ALREADY USD, so the fold must
    // NOT scale it by spot (the bug adding SOL naively would introduce — a $0.25 premium would
    // become ~$18). The index says the option settles in USDC (its payload's settlement currency) →
    // scale 1.0. Verified live: streamed 0.2477 ≈ REST 0.2463 at SOL spot 75.64.
    let seeded = OptionQuote {
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
    // mark 0.2477 is ALREADY USD — must pass through, NOT × 75.64
    let rows = vec![mp("SOL_USDC-25SEP26-45-P", 0.2477, 0.7115)];
    assert_eq!(apply_markprice_to_chains(&mut by, &index(), &rows, NOW, 0.0), 1);
    let put = by["SOL"].chains["2026-09-25"].rows[0].put.as_ref().unwrap();
    assert_eq!(put.mark, Some(0.2477), "USDC premium passed through unscaled (no × spot)");
    assert_eq!(put.iv, Some(0.7115));
    assert!(put.delta.is_some(), "greeks re-enriched from spot + IV");
}

#[test]
fn a_row_without_mark_or_iv_is_skipped_not_zeroed() {
    // The decoder never emits one, but the fold must not turn an absent mark/IV into a value.
    let mut by = seeded_btc();
    let mut no_iv = OptionTicker::new(NOW, "BTC-27JUN26-100000-C");
    no_iv.mark = Some(0.05);
    let mut no_mark = OptionTicker::new(NOW, "BTC-27JUN26-100000-P");
    no_mark.mark_iv = Some(0.62);
    assert_eq!(apply_markprice_to_chains(&mut by, &index(), &[no_iv, no_mark], NOW, 0.0), 0);
    let row = &by["BTC"].chains["2026-06-27"].rows[0];
    assert_eq!(row.call.as_ref().unwrap().mark, Some(1.0), "seeded quote untouched");
    assert_eq!(row.put.as_ref().unwrap().iv, Some(0.10), "seeded quote untouched");
}
