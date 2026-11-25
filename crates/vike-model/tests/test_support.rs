//! Self-tests for `vike_model::test_support` — the helpers several crates' tests used to carry as
//! identical private copies. Each test pins, over planted input, the property the callers rely on,
//! so a helper that stopped doing its job fails HERE instead of turning every caller's assertion
//! vacuous.
//!
//! Integration tests compile this crate WITHOUT `cfg(test)`, so the module is visible here only
//! through the `test-support` feature, which this crate's self-referential dev-dependency turns on.

use std::time::Duration;

use vike_marketdata::Bar;
use vike_model::test_support::bars::assert_bars_bit_eq;
use vike_model::test_support::corpus::{Fnv, decade, lcg};
use vike_model::test_support::etxtbsy::{
    ATTEMPTS, ETXTBSY, FIRST_BACKOFF, retry_budget, spawn_error_is_etxtbsy,
};
use vike_model::test_support::maker::as_test_params;
use vike_model::test_support::text::fn_body;
use vike_model::{HorizonMode, KappaMode, VarianceMode};

// ---- text::fn_body ----

#[test]
fn fn_body_returns_the_lines_between_a_definition_and_its_column_zero_brace() {
    let src = "\
// a comment quoting `fn generic(` must not anchor the scan
fn generic<R>(r: R) -> u8 {
    let x = { 1 };
    x
}

pub fn plain() {
    other();
}
";
    assert_eq!(
        fn_body(src, "generic"),
        "    let x = { 1 };\n    x\n",
        "a GENERIC fn is found, its definition and closing lines are excluded, and the comment \
         that quotes it is not an anchor"
    );
    assert_eq!(
        fn_body(src, "plain"),
        "    other();\n",
        "a `pub fn`, and nothing from the fn above"
    );
    assert_eq!(
        fn_body(src, "gen"),
        "",
        "a name that is only a PREFIX of a fn's name anchors nothing"
    );
    assert_eq!(
        fn_body(src, "absent"),
        "",
        "an absent name harvests nothing — callers assert a floor"
    );
}

// ---- maker::as_test_params ----

#[test]
fn as_test_params_is_the_deterministic_config_with_gamma_wired_through() {
    let p = as_test_params(0.7);
    assert_eq!(p.gamma.to_bits(), 0.7f64.to_bits(), "gamma is the argument, untouched");
    // The three choices that make every quoted price exact: no sigma warm-up, no resolution
    // blackout, no live kappa fit.
    assert_eq!(p.variance_mode, VarianceMode::PureBernoulli);
    assert_eq!(p.horizon_mode, HorizonMode::ConstantTau);
    assert_eq!(p.resolution_ts, None);
    assert_eq!(p.kappa_mode, KappaMode::Fixed);
    assert_eq!(p.q_scale.to_bits(), 1.0f64.to_bits(), "the position IS q_norm");
}

// ---- etxtbsy ----

#[test]
fn the_retry_budget_is_every_sleep_between_attempts_and_no_more() {
    // ATTEMPTS attempts leave ATTEMPTS - 1 gaps, each double the last: a geometric sum.
    let closed_form = FIRST_BACKOFF * ((1u32 << (ATTEMPTS - 1)) - 1);
    assert_eq!(retry_budget(), closed_form);
    assert_eq!(
        retry_budget(),
        Duration::from_millis(635),
        "8 attempts from 5 ms: 5 + 10 + … + 320"
    );
}

#[test]
fn only_the_etxtbsy_errno_is_the_race() {
    let busy = std::io::Error::from_raw_os_error(ETXTBSY);
    let missing = std::io::Error::from_raw_os_error(2); // ENOENT: a real failure, never retried
    assert!(!spawn_error_is_etxtbsy(&missing), "a missing binary is not the race");
    // Windows has no exec-while-open-for-write rule, so nothing is ever the race there.
    assert_eq!(spawn_error_is_etxtbsy(&busy), cfg!(unix));
}

// ---- bars::assert_bars_bit_eq ----

fn bar(close: f64) -> Bar {
    Bar {
        timestamp: 1,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close,
        volume: 3.0,
        funding: Some(0.0001),
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn equal_bits_pass_even_where_float_equality_would_not() {
    assert_bars_bit_eq(&[bar(f64::NAN)], &[bar(f64::NAN)]);
}

#[test]
#[should_panic(expected = "close[0]")]
fn a_signed_zero_is_a_difference() {
    assert_bars_bit_eq(&[bar(0.0)], &[bar(-0.0)]);
}

#[test]
#[should_panic(expected = "close[1]")]
fn a_one_ulp_difference_is_refused_and_named_by_index() {
    let x = 0.3f64;
    assert_bars_bit_eq(&[bar(1.0), bar(x)], &[bar(1.0), bar(f64::from_bits(x.to_bits() + 1))]);
}

#[test]
#[should_panic(expected = "bar count")]
fn a_missing_bar_is_refused() {
    assert_bars_bit_eq(&[bar(1.0), bar(2.0)], &[bar(1.0)]);
}

#[test]
fn bid_ask_and_symbol_are_not_compared() {
    // The comparison covers ts, OHLCV and funding — exactly what the five copies it replaced
    // compared, so widening it would change what five crates' assertions mean.
    let read_back = Bar { symbol: Some("BTCUSDT.binance".into()), bid: Some(1.0), ..bar(1.0) };
    assert_bars_bit_eq(&[bar(1.0)], &[read_back]);
}

// ---- corpus ----

#[test]
fn fnv_is_fnv1a_over_the_le_bits_with_nan_canonicalised() {
    assert_eq!(Fnv::new().hex(), "cbf29ce484222325", "the offset basis");
    let mut h = Fnv::new();
    h.push(1.0);
    assert_eq!(h.hex(), "aab1693229ba1db8");
    h.push(-0.0);
    assert_eq!(h.hex(), "2f12dcea1c5dde38", "-0.0 is its own bit pattern");
    let (mut a, mut b) = (Fnv::new(), Fnv::new());
    a.push(f64::NAN);
    b.push(f64::from_bits(0xfff8_0000_0000_0001));
    assert_eq!(a.hex(), b.hex(), "a NaN's sign and payload must not reach the hash");
}

#[test]
fn lcg_draws_the_pinned_sequence_and_decade_builds_ten_to_the_k() {
    let mut seed = 0u64;
    assert_eq!(lcg(&mut seed), 0.07820865487829387);
    assert_eq!(lcg(&mut seed), 0.10169876029679303);
    assert_eq!(seed, 1_876_011_003_808_476_466);
    assert_eq!((decade(0), decade(3), decade(-1)), (1.0, 1000.0, 0.1));
}
