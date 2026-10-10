//! Tests for the catalog's tiered search ranking, its filters and `merge_ranked`.

use super::*;
use crate::{Instrument, SearchFilter, Tab};
use vike_model::AssetClass;

fn sample() -> Catalog {
    Catalog::from_instruments(vec![
        Instrument::test_row("binance", "ETHBTC", "ETH", "BTC", AssetClass::CryptoSpot),
        Instrument::test_row("binance", "BTCUSDC", "BTC", "USDC", AssetClass::CryptoSpot),
        Instrument::test_row("binance", "BTCUSDT", "BTC", "USDT", AssetClass::CryptoSpot),
        Instrument::test_row("alpaca", "BTCUSD", "BTC", "USD", AssetClass::CryptoSpot),
        Instrument::test_row("alpaca", "AAPL", "AAPL", "USD", AssetClass::Equity),
    ])
}

#[test]
fn exact_symbol_ranks_first_then_prefix_then_base() {
    let c = sample();
    let hits = c.search("BTCUSDT", &SearchFilter::default(), 10);
    assert_eq!(hits[0].raw_symbol, "BTCUSDT"); // exact beats prefix
}

#[test]
fn base_prefix_hits_match_when_symbol_does_not() {
    let c = sample();
    let syms: Vec<_> = c
        .search("BTC", &SearchFilter::default(), 10)
        .iter()
        .map(|x| x.raw_symbol.clone())
        .collect();
    // BTC* symbol-prefix hits, then ETHBTC via base? ETH base doesn't prefix BTC — ETHBTC is a
    // substring hit; ensure the three BTC-prefixed symbols come before ETHBTC.
    assert!(
        syms.iter().position(|s| s == "BTCUSDT").unwrap()
            < syms.iter().position(|s| s == "ETHBTC").unwrap()
    );
}

#[test]
fn preferred_quote_breaks_ties_usdt_first() {
    let c = sample();
    let hits = c.search("BTC", &SearchFilter::default(), 10);
    let usdt = hits.iter().position(|x| x.raw_symbol == "BTCUSDT").unwrap();
    let usdc = hits.iter().position(|x| x.raw_symbol == "BTCUSDC").unwrap();
    assert!(usdt < usdc);
}

#[test]
fn tab_filter_restricts_to_asset_class() {
    let c = sample();
    let hits = c.search("", &SearchFilter { tab: Some(Tab::Stocks), venue: None }, 10);
    assert!(hits.iter().all(|x| x.asset_class == AssetClass::Equity));
    assert_eq!(hits.len(), 1);
}

#[test]
fn venue_filter_restricts_to_venue() {
    let c = sample();
    let hits = c.search("BTC", &SearchFilter { tab: None, venue: Some("alpaca".into()) }, 10);
    assert!(hits.iter().all(|x| x.venue == "alpaca"));
}

#[test]
fn a_mixed_case_symbol_is_found_by_any_case() {
    let c = Catalog::from_instruments(vec![Instrument::test_row(
        "hyperliquid",
        "kPEPE",
        "kPEPE",
        "USDC",
        AssetClass::CryptoPerp,
    )]);
    for q in ["kpepe", "KPEPE", "kPepe", "kp"] {
        assert_eq!(c.search(q, &SearchFilter::default(), 5).len(), 1, "{q}");
    }
}

#[test]
fn empty_query_returns_filtered_head() {
    let c = sample();
    let hits = c.search("", &SearchFilter::default(), 2);
    assert_eq!(hits.len(), 2);
}

#[test]
fn merge_dedups_by_id_and_reranks() {
    let local =
        vec![Instrument::test_row("binance", "BTCUSDT", "BTC", "USDT", AssetClass::CryptoSpot)];
    let remote = vec![
        Instrument::test_row("ibkr", "BTC", "BTC", "USD", AssetClass::CryptoSpot),
        Instrument::test_row("binance", "BTCUSDT", "BTC", "USDT", AssetClass::CryptoSpot), // dup of local
    ];
    let out = merge_ranked(local, remote, "BTC", 10);
    let ids: Vec<_> = out.iter().map(|x| x.id()).collect();
    assert_eq!(ids.iter().filter(|id| *id == "BTCUSDT.BINANCE").count(), 1); // deduped
    assert!(ids.contains(&"BTC.IBKR".to_string()));
}

#[test]
fn scale_sanity_50k() {
    let mut items = Vec::with_capacity(50_000);
    for n in 0..50_000u32 {
        items.push(Instrument::test_row(
            "binance",
            &format!("SYM{n}USDT"),
            &format!("SYM{n}"),
            "USDT",
            AssetClass::CryptoSpot,
        ));
    }
    let c = Catalog::from_instruments(items);
    let hits = c.search("SYM123USDT", &SearchFilter::default(), 10);
    assert_eq!(hits[0].raw_symbol, "SYM123USDT");
}
