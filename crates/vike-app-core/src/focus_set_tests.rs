use super::*;
use std::collections::BTreeMap;
use vike_options::{Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow, UnderlyingKind};

const NOW: i64 = 1_780_387_200_000;

fn opt(name: &str, k: f64, call: bool) -> OptionQuote {
    OptionQuote {
        instrument_name: Some(name.to_string()),
        ..OptionQuote::new(k, if call { OptionKind::Call } else { OptionKind::Put })
    }
}

fn srow(prefix: &str, k: f64) -> StrikeRow {
    StrikeRow {
        strike: k,
        call: Some(opt(&format!("{prefix}-{}-C", k as i64), k, true)),
        put: Some(opt(&format!("{prefix}-{}-P", k as i64), k, false)),
    }
}

fn chain(date: &str, spot: f64, strikes: &[f64], prefix: &str) -> OptionChain {
    OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(spot),
        expiry: Expiry { date: date.into(), dte: 25, label: date.into() },
        asof_ms: NOW,
        source: "deribit".into(),
        rows: strikes.iter().map(|&k| srow(prefix, k)).collect(),
    }
}

/// A BTC bundle: a 6-strike FRONT expiry (spot 100_500, between 100k and 101k) + a LATER expiry
/// the front-only focus must ignore.
fn by_btc() -> BTreeMap<String, UnderlyingChains> {
    let mut chains = BTreeMap::new();
    chains.insert(
        "2026-06-27".to_string(),
        chain(
            "2026-06-27",
            100_500.0,
            &[98_000.0, 99_000.0, 100_000.0, 101_000.0, 102_000.0, 103_000.0],
            "BTC-27JUN26",
        ),
    );
    chains.insert(
        "2026-07-25".to_string(),
        chain("2026-07-25", 100_500.0, &[100_000.0], "BTC-25JUL26"),
    );
    let bundle = UnderlyingChains {
        default_expiry: "2026-06-27".into(), // nearest DTE
        expiries: vec![
            Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
            Expiry { date: "2026-07-25".into(), dte: 53, label: "25 Jul".into() },
        ],
        chains,
    };
    let mut by = BTreeMap::new();
    by.insert("BTC".to_string(), bundle);
    by
}

#[test]
fn atm_window_collects_both_sides_at_each_strike() {
    // ±1 around spot 100_500 (split at 101_000): strikes [100_000, 101_000], call+put each.
    let ids = front_expiry_focus_instruments(&by_btc(), &["BTC"], 1);
    assert_eq!(
        ids,
        vec![
            "BTC-27JUN26-100000-C",
            "BTC-27JUN26-100000-P",
            "BTC-27JUN26-101000-C",
            "BTC-27JUN26-101000-P",
        ]
    );
}

#[test]
fn only_front_expiry_never_the_later_one() {
    // a window wider than the chain returns ALL front strikes and NONE of the later expiry.
    let ids = front_expiry_focus_instruments(&by_btc(), &["BTC"], 30);
    assert_eq!(ids.len(), 12, "6 front strikes × call+put");
    assert!(ids.iter().all(|i| i.starts_with("BTC-27JUN26-")), "only the front expiry: {ids:?}");
    assert!(!ids.iter().any(|i| i.contains("25JUL26")), "the later expiry is excluded");
}

#[test]
fn skips_underlying_without_spot_or_without_fetch() {
    let mut by = by_btc();
    by.get_mut("BTC").unwrap().chains.get_mut("2026-06-27").unwrap().underlying_price = None;
    assert!(front_expiry_focus_instruments(&by, &["BTC"], 10).is_empty(), "no spot → skipped");
    // an underlying that never fetched is simply skipped (empty, no panic).
    assert!(front_expiry_focus_instruments(&by_btc(), &["DOGE"], 10).is_empty());
}
