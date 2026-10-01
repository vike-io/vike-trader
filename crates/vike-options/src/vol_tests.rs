use super::*;
use crate::model::{Expiry, OptionKind, OptionQuote, StrikeRow, UnderlyingKind};

fn assert_bits(what: &str, got: Option<f64>, expect: f64) {
    let got = got.unwrap_or_else(|| panic!("{what}: expected Some({expect}), got None"));
    assert_eq!(got.to_bits(), expect.to_bits(), "{what}: got {got:e}, expected {expect:e}");
}

#[test]
fn ewma_matches_hand_unrolled_recursion() {
    // Prices 100 → 101 → 99 → 102 at λ = 0.94, unrolled by hand.
    // ⚠ The unroll spells `libm::log`, NOT `f64::ln`, and that is load-bearing rather than
    // stylistic: `assert_bits` below compares EXACT bits, so an `f64::ln` unroll would be
    // asserting that this box's platform CRT agrees with the portable implementation the fold
    // now uses. That is a test of the CRT (see the module doc's ADR-0032 section).
    let mut e = EwmaVol::riskmetrics();
    e.update(100.0);
    assert!(e.variance().is_none(), "no return yet");
    assert!(e.value().is_none());

    e.update(101.0);
    let r1 = libm::log(101.0f64 / 100.0);
    let mut v = r1 * r1; // seed
    assert_bits("seed variance", e.variance(), v);

    // NB: the recursion's weight is written `(1 - λ)` exactly as RiskMetrics states it —
    // in f64 `1.0 - 0.94` is NOT the literal `0.06` (2 ulp apart), so the unroll must
    // spell it the same way to land on the same bits.
    e.update(99.0);
    let r2 = libm::log(99.0f64 / 101.0);
    v = 0.94 * v + (1.0 - 0.94) * (r2 * r2);
    assert_bits("second fold", e.variance(), v);

    e.update(102.0);
    let r3 = libm::log(102.0f64 / 99.0);
    v = 0.94 * v + (1.0 - 0.94) * (r3 * r3);
    assert_bits("third fold", e.variance(), v);
    assert_bits("vol = sqrt(var)", e.value(), v.sqrt());
    assert_bits("annualized", e.annualized(DAYS_PER_YEAR), v.sqrt() * 365.0f64.sqrt());
}

#[test]
fn ewma_ignores_bad_ticks() {
    let mut e = EwmaVol::riskmetrics();
    for p in [100.0, f64::NAN, 0.0, -5.0, f64::INFINITY] {
        e.update(p);
    }
    assert!(e.variance().is_none(), "bad ticks must not create a return");
    e.update(101.0);
    let r1 = libm::log(101.0f64 / 100.0); // return vs 100, not vs any rejected tick
    assert_bits("return spans the bad ticks", e.variance(), r1 * r1);
}

#[test]
#[should_panic(expected = "lambda must be in (0, 1)")]
fn ewma_rejects_bad_lambda() {
    let _ = EwmaVol::new(1.0);
}

#[test]
fn rolling_matches_hand_computed_window() {
    // window = 3 over prices 100, 101, 99, 102, 103 → returns r1..r4; the live window
    // after the last update is [r2, r3, r4].
    let mut w = RollingVol::new(3);
    for p in [100.0, 101.0, 99.0, 102.0] {
        w.update(p);
    }
    // Only 3 returns exist after 4 prices — exactly full: [r1, r2, r3].
    let r1 = libm::log(101.0f64 / 100.0);
    let r2 = libm::log(99.0f64 / 101.0);
    let r3 = libm::log(102.0f64 / 99.0);
    let mean = (r1 + r2 + r3) / 3.0;
    let ss = (r1 - mean) * (r1 - mean) + (r2 - mean) * (r2 - mean) + (r3 - mean) * (r3 - mean);
    assert_bits("full window", w.value(), (ss / 2.0).sqrt());

    w.update(103.0); // r4 evicts r1
    let r4 = libm::log(103.0f64 / 102.0);
    let mean = (r2 + r3 + r4) / 3.0;
    let ss = (r2 - mean) * (r2 - mean) + (r3 - mean) * (r3 - mean) + (r4 - mean) * (r4 - mean);
    assert_bits("rolled window", w.value(), (ss / 2.0).sqrt());
}

#[test]
fn rolling_none_until_full() {
    let mut w = RollingVol::new(3);
    w.update(100.0);
    assert!(w.value().is_none());
    w.update(101.0);
    assert!(w.value().is_none(), "1 return < window");
    w.update(102.0);
    assert!(w.value().is_none(), "2 returns < window");
    w.update(103.0);
    assert!(w.value().is_some(), "3 returns == window");
}

#[test]
#[should_panic(expected = "window must be >= 2")]
fn rolling_rejects_window_of_one() {
    let _ = RollingVol::new(1);
}

#[test]
fn annualization_round_trip() {
    for basis in [365.0, 252.0, 365.0 * 24.0] {
        let v = 0.0123;
        let back = deannualize(annualize(v, basis), basis);
        let rel = ((back - v) / v).abs();
        assert!(rel < 1e-15, "basis {basis}: {back} vs {v} (rel {rel:e})");
    }
    // Known value on the crate basis: 1%/day → 19.105%/year (√365 ≈ 19.105).
    let ann = annualize(0.01, DAYS_PER_YEAR);
    assert!((ann - 0.191049731745428).abs() < 1e-15, "got {ann}");
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
