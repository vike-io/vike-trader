use super::*;
use std::collections::BTreeMap;
use vike_options::{Expiry, OptionChain, UnderlyingKind};

/// Minimal chain for an (underlying, expiry) — only the keys/fields the selection logic reads.
fn chain(underlying: &str, date: &str) -> OptionChain {
    OptionChain {
        underlying: underlying.to_string(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(100.0),
        expiry: Expiry { date: date.to_string(), dte: 1, label: date.to_string() },
        asof_ms: 0,
        source: "deribit".into(),
        rows: Vec::new(),
    }
}

fn bundle(underlying: &str, expiries: &[&str]) -> UnderlyingChains {
    let mut chains = BTreeMap::new();
    let mut exps = Vec::new();
    for (i, d) in expiries.iter().enumerate() {
        chains.insert(d.to_string(), chain(underlying, d));
        exps.push(Expiry { date: d.to_string(), dte: i as i64, label: d.to_string() });
    }
    UnderlyingChains {
        default_expiry: expiries.first().map(|s| s.to_string()).unwrap_or_default(),
        expiries: exps,
        chains,
    }
}

fn books() -> BTreeMap<String, UnderlyingChains> {
    let mut m = BTreeMap::new();
    m.insert("BTC".into(), bundle("BTC", &["2026-07-17", "2026-07-24"]));
    m.insert("ETH".into(), bundle("ETH", &["2026-07-18", "2026-07-25"]));
    m.insert("SOL".into(), bundle("SOL", &["2026-07-19"]));
    m
}

#[test]
fn defaults_to_first_underlying_when_unset() {
    let by = books();
    let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
    let mut expiry = None;
    let sel = resolve_options_selection(&by, &order, &None, &mut expiry).unwrap();
    assert_eq!(sel.default_expiry, "2026-07-17", "BTC bundle");
    assert_eq!(expiry, None, "no expiry pick, nothing to reset");
}

#[test]
fn honors_a_valid_underlying_selection() {
    // Pre-selecting ETH (the VIKE_SHOT path) renders ETH's book.
    let by = books();
    let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
    let mut expiry = None;
    let sel = resolve_options_selection(&by, &order, &Some("ETH".into()), &mut expiry).unwrap();
    assert_eq!(sel.default_expiry, "2026-07-18", "ETH bundle");
    assert_eq!(sel.default_expiry, "2026-07-18");
}

#[test]
fn unknown_underlying_falls_back_to_first() {
    let by = books();
    let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
    let mut expiry = None;
    // "DOGE" never fetched → fall back to the first fetched underlying.
    let sel = resolve_options_selection(&by, &order, &Some("DOGE".into()), &mut expiry).unwrap();
    assert_eq!(sel.default_expiry, "2026-07-17", "BTC bundle");
}

#[test]
fn resets_expiry_when_invalid_for_new_underlying() {
    // Switching BTC→ETH while a BTC-only expiry is selected clears it so ETH's default shows.
    let by = books();
    let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
    let mut expiry = Some("2026-07-17".to_string()); // a BTC expiry, absent from ETH
    let sel = resolve_options_selection(&by, &order, &Some("ETH".into()), &mut expiry).unwrap();
    assert_eq!(sel.default_expiry, "2026-07-18", "ETH bundle");
    assert_eq!(expiry, None, "the BTC expiry must reset so ETH's default takes over");
}

#[test]
fn keeps_expiry_that_is_valid_for_selected_underlying() {
    let by = books();
    let order = vec!["BTC".to_string(), "ETH".into(), "SOL".into()];
    let mut expiry = Some("2026-07-24".to_string()); // a real BTC expiry
    resolve_options_selection(&by, &order, &Some("BTC".into()), &mut expiry).unwrap();
    assert_eq!(expiry, Some("2026-07-24".to_string()), "a valid pick is preserved");
}

#[test]
fn none_when_nothing_fetched() {
    let by: BTreeMap<String, UnderlyingChains> = BTreeMap::new();
    let order: Vec<String> = Vec::new();
    let mut expiry = None;
    assert!(resolve_options_selection(&by, &order, &Some("ETH".into()), &mut expiry).is_none());
}

#[test]
fn canonical_order_ranks_btc_eth_sol() {
    let mut order = vec!["SOL".to_string(), "BTC".into(), "ETH".into()];
    order.sort_by_key(|u| underlying_rank(u));
    assert_eq!(order, vec!["BTC", "ETH", "SOL"]);
}
