//! A window with no number in it has no highest high and no lowest low: `chandelier`,
//! `donchian_width` and `ttm_squeeze` answer `NaN` there, not an empty identity (`-inf`, `f64::MIN`)
//! that reads like a price. The line whose own column is intact keeps its number, and the value comes
//! back as soon as one real number is in the window again. Stream, `vectorize` and `series::*` agree.

use vike_indicators::indicators::{Chandelier, DonchianWidth, TtmSqueeze};
use vike_indicators::{Indicator, series};
use vike_marketdata::Bar;

const N: usize = 5;
/// Highs are `NaN` on bars 10 to 14: the window of bar 14 holds no high at all.
const GAP: std::ops::Range<usize> = 10..15;

fn columns() -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let close: Vec<f64> = (0..30).map(|i| 100.0 + (i as f64 * 0.7).sin() * 5.0).collect();
    let mut high: Vec<f64> = close.iter().map(|c| c + 1.0).collect();
    let low: Vec<f64> = close.iter().map(|c| c - 1.0).collect();
    for h in &mut high[GAP] {
        *h = f64::NAN;
    }
    (high, low, close)
}

fn bars(high: &[f64], low: &[f64], close: &[f64]) -> Vec<Bar> {
    (0..close.len())
        .map(|i| Bar::ohlcv(i as i64 * 60_000, close[i], high[i], low[i], close[i], 1.0))
        .collect()
}

fn same(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

#[test]
fn chandelier_has_no_long_stop_without_a_high_but_keeps_the_short_stop() {
    let (high, low, close) = columns();
    let [long, short] = series::chandelier(&high, &low, &close, N as f64, 3.0).unwrap();
    // Bar 14: the window (bars 10..=14) holds no high.
    assert!(long[14].is_nan(), "long stop of an empty-high window is {}", long[14]);
    assert!(short[14].is_finite(), "the lows are intact, so is the short stop");
    // Bar 15: one real high is back in the window.
    assert!(long[15].is_finite());
    // Earlier bars are untouched by the gap.
    assert!(long[9].is_finite());
    let b = bars(&high, &low, &close);
    let mut stream = Chandelier::new(N as f64, 3.0).unwrap();
    let batch = Chandelier::new(N as f64, 3.0).unwrap().vectorize(&b);
    for (i, bar) in b.iter().enumerate() {
        let [l, s] = stream.next(bar);
        assert!(same(l, long[i]) && same(s, short[i]), "stream vs series at {i}");
        assert!(same(batch[0][i], long[i]) && same(batch[1][i], short[i]), "vectorize at {i}");
    }
}

#[test]
fn donchian_width_is_nan_without_a_high_and_comes_back() {
    let (high, low, close) = columns();
    let w = series::donchian_width(&high, &low, N as f64).unwrap();
    assert!(w[14].is_nan(), "width of an empty-high window is {}", w[14]);
    assert!(w[15].is_finite() && w[9].is_finite());
    let b = bars(&high, &low, &close);
    let mut stream = DonchianWidth::new(N as f64).unwrap();
    for (i, bar) in b.iter().enumerate() {
        assert!(same(stream.next(bar), w[i]), "stream vs series at {i}");
    }
}

#[test]
fn ttm_squeeze_has_no_momentum_without_a_high_and_recovers() {
    let (high, low, close) = columns();
    let [mom, flag] = series::ttm_squeeze(&high, &low, &close, N as f64, 2.0, 1.5).unwrap();
    assert!(mom[14].is_nan(), "momentum of an empty-high window is {}", mom[14]);
    assert!(!mom[14].is_infinite());
    // The regression ring holds the NaN delta for `N` bars after the gap, then the value is back.
    assert!(mom[GAP.end + N].is_finite(), "momentum never recovers: {}", mom[GAP.end + N]);
    assert!(flag.iter().all(|f| !f.is_infinite()));
    let b = bars(&high, &low, &close);
    let mut stream = TtmSqueeze::new(N as f64, 2.0, 1.5).unwrap();
    for (i, bar) in b.iter().enumerate() {
        let [m, f] = stream.next(bar);
        assert!(same(m, mom[i]) && same(f, flag[i]), "stream vs series at {i}");
    }
}
