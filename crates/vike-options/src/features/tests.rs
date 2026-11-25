use super::*;
use crate::pricing::greeks::{black_scholes_price, years_to_expiry};
use crate::{MS_PER_YEAR, expiry_ms};

#[test]
fn moneyness_and_log_moneyness_by_hand() {
    assert_eq!(moneyness(110.0, 100.0).to_bits(), 1.1f64.to_bits());
    assert_eq!(log_moneyness(100.0, 100.0), 0.0, "at the money");
    assert!((log_moneyness(200.0, 100.0) - std::f64::consts::LN_2).abs() < 1e-16);
    assert!((log_moneyness(50.0, 100.0) + std::f64::consts::LN_2).abs() < 1e-16);
}

#[test]
fn years_to_expiry_ms_is_the_one_division_by_the_365_day_year() {
    let day = 86_400_000;
    assert_eq!(years_to_expiry_ms(365 * day, 0), 1.0);
    assert_eq!(years_to_expiry_ms(73 * day, 0).to_bits(), 0.2f64.to_bits());
    assert_eq!(years_to_expiry_ms(0, day), 0.0, "a past expiry clamps at zero");
    assert_eq!(MS_PER_YEAR, 31_536_000_000.0);
    // The ISO form divides through the same function: the same bits.
    let now = 1_780_387_200_000; // 2026-06-02 08:00 UTC
    let iso = years_to_expiry("2026-06-27", now);
    assert_eq!(iso.to_bits(), years_to_expiry_ms(expiry_ms("2026-06-27"), now).to_bits());
    assert_eq!(iso.to_bits(), (25.0f64 / 365.0).to_bits(), "25 days");
}

#[test]
fn quote_iv_is_the_quote_s_own_else_its_mark_s() {
    let (spot, strike, years) = (100.0, 105.0, 0.25);
    let mark = black_scholes_price(spot, strike, years, 0.5, OptionKind::Call, 0.0);
    assert_eq!(quote_iv(Some(0.6), mark, spot, strike, years, OptionKind::Call, 0.0), Some(0.6));
    for own in [None, Some(f64::NAN), Some(0.0), Some(-0.1)] {
        let implied = quote_iv(own, mark, spot, strike, years, OptionKind::Call, 0.0)
            .expect("inverted from the mark");
        assert!((implied - 0.5).abs() < 1e-5, "{own:?}: {implied}");
    }
    assert_eq!(quote_iv(None, None, spot, strike, years, OptionKind::Call, 0.0), None);
}

#[test]
fn delta_at_quote_is_the_delta_at_the_quote_s_iv() {
    let (spot, strike, years) = (100.0, 105.0, 0.25);
    let mark = black_scholes_price(spot, strike, years, 0.5, OptionKind::Call, 0.0);
    for own in [Some(0.6), None] {
        let iv = quote_iv(own, mark, spot, strike, years, OptionKind::Call, 0.0).unwrap();
        let delta = black_scholes_greeks(spot, strike, years, iv, OptionKind::Call, 0.0).unwrap().0;
        let got = delta_at_quote(own, mark, spot, strike, years, OptionKind::Call, 0.0);
        assert_eq!(got.map(f64::to_bits), Some(delta.to_bits()), "{own:?}");
    }
    assert_eq!(delta_at_quote(None, None, spot, strike, years, OptionKind::Call, 0.0), None);
    assert_eq!(delta_at_quote(Some(0.6), mark, spot, strike, 0.0, OptionKind::Call, 0.0), None);
}

#[test]
fn atm_iv_by_expiry_reads_each_expiry_nearest_the_forward() {
    let day = 86_400_000;
    let (near, far) = (30 * day, 90 * day);
    let quotes = [
        (near, 95.0, 0.62),
        (far, 90.0, 0.70),
        (near, 100.0, 0.60),
        (far, 110.0, 0.50),
        (near, 100.0, 0.64),
        (near, 105.0, 0.58),
    ];
    let atm = atm_iv_by_expiry(&quotes, 100.0, 0, 0.0);
    assert_eq!(atm.len(), 2);
    // The near expiry: strike 100 is the forward; its call and put average, (0.60 + 0.64) / 2.
    assert_eq!(atm[&near].to_bits(), (0.5 * (0.60 + 0.64f64)).to_bits());
    // The far expiry: 90 and 110 tie at the forward; the first strike seen wins.
    assert_eq!(atm[&far], 0.70);
    // A rate moves the forward: F = 100·e^{0.6·90/365} ≈ 115.94, nearest 110.
    assert_eq!(atm_iv_by_expiry(&quotes, 100.0, 0, 0.6)[&far], 0.50);
    assert!(atm_iv_by_expiry(&quotes, 0.0, 0, 0.0).is_empty(), "no price, no read");
}
