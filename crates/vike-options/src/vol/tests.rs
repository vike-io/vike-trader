use super::*;
use vike_marketdata::{Expiry, OptionKind, OptionQuote, StrikeRow, UnderlyingKind};

fn assert_bits(what: &str, got: Option<f64>, expect: f64) {
    let got = got.unwrap_or_else(|| panic!("{what}: expected Some({expect}), got None"));
    assert_eq!(got.to_bits(), expect.to_bits(), "{what}: got {got:e}, expected {expect:e}");
}

#[test]
fn spread_is_a_pure_difference() {
    assert_eq!(iv_rv_spread(0.62, 0.50).to_bits(), (0.62f64 - 0.50).to_bits());
    assert!(iv_rv_spread(0.40, 0.55) < 0.0);
}

// ---- atm_iv ----

fn quote(strike: f64, kind: OptionKind, iv: Option<f64>) -> OptionQuote {
    OptionQuote { iv, ..OptionQuote::new(strike, kind) }
}

fn chain(spot: Option<f64>, rows: Vec<StrikeRow>) -> OptionChain {
    OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: spot,
        expiry: Expiry { date: "2026-08-28".into(), dte: 41, label: "28 Aug".into() },
        asof_ms: 0,
        source: "test".into(),
        rows,
    }
}

fn row(strike: f64, call_iv: Option<f64>, put_iv: Option<f64>) -> StrikeRow {
    StrikeRow {
        strike,
        call: Some(quote(strike, OptionKind::Call, call_iv)),
        put: Some(quote(strike, OptionKind::Put, put_iv)),
    }
}

#[test]
fn atm_iv_picks_nearest_to_forward_and_averages() {
    let c = chain(
        Some(101.0),
        vec![
            row(90.0, Some(0.70), Some(0.72)),
            row(100.0, Some(0.60), Some(0.62)),
            row(110.0, Some(0.55), Some(0.57)),
        ],
    );
    // r = 0 → forward = spot = 101 → nearest strike 100 → mean(0.60, 0.62).
    assert_bits("mean of both sides", atm_iv(&c, 0.5, 0.0), 0.5 * (0.60 + 0.62));
    // Nonzero r pushes the forward up: F = 101·e^{0.18·0.5} ≈ 110.51 → nearest strike 110.
    assert_bits("forward-shifted pick", atm_iv(&c, 0.5, 0.18), 0.5 * (0.55 + 0.57));
}

#[test]
fn atm_iv_one_sided_and_skips_ivless_rows() {
    let c = chain(
        Some(100.0),
        vec![row(100.0, None, None), row(105.0, Some(0.58), None), row(110.0, None, Some(0.54))],
    );
    // Strike 100 has no IV at all → skipped; nearest IV-bearing is 105 (call only).
    assert_bits("one-sided call", atm_iv(&c, 0.25, 0.0), 0.58);
}

#[test]
fn atm_iv_edge_cases_are_none() {
    let no_spot = chain(None, vec![row(100.0, Some(0.6), Some(0.6))]);
    assert!(atm_iv(&no_spot, 0.5, 0.0).is_none());
    let no_iv = chain(Some(100.0), vec![row(100.0, None, None)]);
    assert!(atm_iv(&no_iv, 0.5, 0.0).is_none());
    let empty = chain(Some(100.0), vec![]);
    assert!(atm_iv(&empty, 0.5, 0.0).is_none());
    let bad_spot = chain(Some(0.0), vec![row(100.0, Some(0.6), None)]);
    assert!(atm_iv(&bad_spot, 0.5, 0.0).is_none());
    let neg_t = chain(Some(100.0), vec![row(100.0, Some(0.6), None)]);
    assert!(atm_iv(&neg_t, -0.1, 0.0).is_none());
}

#[test]
fn atm_iv_tie_resolves_to_lower_strike() {
    // Spot 105 sits exactly between 100 and 110 → the LOWER strike wins (first in
    // ascending rows).
    let c = chain(Some(105.0), vec![row(100.0, Some(0.60), None), row(110.0, Some(0.50), None)]);
    assert_bits("tie → lower", atm_iv(&c, 0.5, 0.0), 0.60);
}
