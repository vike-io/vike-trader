//! Value tests for the indicators added after the first release, each against a SMALL series whose
//! expected numbers were derived by hand from the formula in the indicator's own doc comment (the
//! derivation sits beside each expectation). Public API only: `compute` and `Bar::ohlcv`.
//!
//! `on_bar == vectorize`, the warm-up claims and the parameter grid are held for every registry row by
//! `tests/parity`, `tests/lookback.rs` and `tests/params.rs`; this file is about the NUMBERS.

use vike_indicators::{compute, lookback};
use vike_marketdata::Bar;

const NAN: f64 = f64::NAN;

/// Bars from `(high, low, close)` rows; open = close, volume 100, one minute apart.
fn bars_hlc(rows: &[(f64, f64, f64)]) -> Vec<Bar> {
    rows.iter()
        .enumerate()
        .map(|(i, &(h, l, c))| Bar::ohlcv(i as i64 * 60_000, c, h, l, c, 100.0))
        .collect()
}

/// Bars whose open and close are the given pair and whose range hugs them by one.
fn bars_oc(rows: &[(f64, f64)]) -> Vec<Bar> {
    rows.iter()
        .enumerate()
        .map(|(i, &(o, c))| {
            Bar::ohlcv(i as i64 * 60_000, o, o.max(c) + 1.0, o.min(c) - 1.0, c, 100.0)
        })
        .collect()
}

/// Bars that only carry a close (a flat candle at it).
fn bars_close(closes: &[f64]) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(i, &c)| Bar::ohlcv(i as i64 * 60_000, c, c, c, c, 100.0))
        .collect()
}

fn leading_nans(v: &[f64]) -> usize {
    v.iter().take_while(|x| x.is_nan()).count()
}

/// Run registry indicator `name` and require its output names, one value per bar per line, exactly
/// `warm` leading NaNs on every line, and every value equal to `want` (a NaN in `want` means NaN).
fn check(name: &str, raw: &[f64], bars: &[Bar], names: &[&str], warm: usize, want: &[Vec<f64>]) {
    let lines = compute(name, raw, bars).unwrap_or_else(|| panic!("{name} is not a registry key"));
    let got_names: Vec<&str> = lines.iter().map(|(n, _)| *n).collect();
    assert_eq!(got_names, names, "{name}: output names, in order");
    assert_eq!(want.len(), names.len(), "{name}: test data has one expectation per line");
    for ((line, got), want) in lines.iter().zip(want) {
        assert_eq!(got.len(), bars.len(), "{name}.{line}: one value per bar");
        assert_eq!(want.len(), bars.len(), "{name}.{line}: test data has one value per bar");
        assert_eq!(leading_nans(got), warm, "{name}.{line}: NaN warm-up count");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            if w.is_nan() {
                assert!(g.is_nan(), "{name}.{line}[{i}]: expected NaN, got {g}");
            } else {
                assert!((g - w).abs() < 1e-9, "{name}.{line}[{i}]: got {g}, want {w}");
            }
        }
    }
}

/// Equal where both are NaN, and otherwise within a relative 1e-9 (floored at 1). `adx` (a fast core
/// that multiplies by `1 / n`) and `plus_di` / `minus_di` (a separate kernel that divides) are two
/// implementations of one definition, so they agree to rounding, no longer bit for bit.
fn same_bits(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| (x.is_nan() && y.is_nan()) || (x - y).abs() <= 1e-9 * y.abs().max(1.0))
}

// ============================ directional movement ==========================

/// (high, low, close) of seven bars, with the per-bar directional movement worked out below.
///
/// ```text
/// bar   h   l   c    +DM  -DM  TR
///  0   10   8   9      -    -   -
///  1   12   9  11      2    0   3    up = 12-10 = 2, down = 8-9 = -1; TR = max(3, |12-9|, |9-9|)
///  2   11   7   8      0    2   4    up = -1, down = 9-7 = 2;          TR = max(4, |11-11|, |7-11|)
///  3   13   8  12      2    0   5    up = 2, down = 7-8 = -1;          TR = max(5, |13-8|, |8-8|)
///  4   12  10  11      0    0   2    up = -1, down = -2: neither wins; TR = max(2, |12-12|, |10-12|)
///  5   14  11  13      2    0   3    up = 2, down = -1;                TR = max(3, |14-11|, |11-11|)
///  6   13   9  10      0    2   4    up = -1, down = 2;                TR = max(4, |13-13|, |9-13|)
/// ```
fn dm_bars() -> Vec<Bar> {
    bars_hlc(&[
        (10.0, 8.0, 9.0),
        (12.0, 9.0, 11.0),
        (11.0, 7.0, 8.0),
        (13.0, 8.0, 12.0),
        (12.0, 10.0, 11.0),
        (14.0, 11.0, 13.0),
        (13.0, 9.0, 10.0),
    ])
}

/// Period 3. The three Wilder sums are seeded at bar 3 with the plain sum of bars 1..=3 and then
/// updated `x += new - x / 3`:
///
/// ```text
/// bar   TR sum            +DM sum                 -DM sum
///  3    3+4+5     = 12    2+0+2     = 4           0+2+0     = 2
///  4    12+2-12/3 = 10    4+0-4/3   = 8/3         2+0-2/3   = 4/3
///  5    10+3-10/3 = 29/3  8/3+2-8/9 = 34/9        4/3+0-4/9 = 8/9
///  6    29/3+4-29/9 = 94/9  34/9+0-34/27 = 68/27  8/9+2-8/27 = 70/27
/// ```
///
/// and `+DI = 100 * (+DM sum) / (TR sum)`, `-DI` likewise, `DX = 100 * |+DI - -DI| / (+DI + -DI)`.
#[test]
fn directional_movement_family_matches_the_hand_derivation() {
    let bars = dm_bars();
    let p = [3.0];

    check(
        "plus_dm",
        &p,
        &bars,
        &["plus_dm"],
        3,
        &[vec![NAN, NAN, NAN, 4.0, 8.0 / 3.0, 34.0 / 9.0, 68.0 / 27.0]],
    );
    check(
        "minus_dm",
        &p,
        &bars,
        &["minus_dm"],
        3,
        &[vec![NAN, NAN, NAN, 2.0, 4.0 / 3.0, 8.0 / 9.0, 70.0 / 27.0]],
    );
    // bar 3: 100*4/12; bar 4: 100*(8/3)/10; bar 5: 100*(34/9)/(29/3) = 3400/87; bar 6: 100*(68/27)/(94/9) = 6800/282
    check(
        "plus_di",
        &p,
        &bars,
        &["plus_di"],
        3,
        &[vec![NAN, NAN, NAN, 100.0 / 3.0, 80.0 / 3.0, 3400.0 / 87.0, 6800.0 / 282.0]],
    );
    // bar 3: 100*2/12; bar 4: 100*(4/3)/10; bar 5: 100*(8/9)/(29/3) = 800/87; bar 6: 100*(70/27)/(94/9) = 7000/282
    check(
        "minus_di",
        &p,
        &bars,
        &["minus_di"],
        3,
        &[vec![NAN, NAN, NAN, 50.0 / 3.0, 40.0 / 3.0, 800.0 / 87.0, 7000.0 / 282.0]],
    );
    // bar 3: |100/3 - 50/3| / 50 -> 100/3; bar 4: |80/3 - 40/3| / 40 -> 100/3;
    // bar 5: 100 * 2600/4200 = 1300/21; bar 6: 100 * |6800 - 7000| / (6800 + 7000) = 20000/13800
    check(
        "dx",
        &p,
        &bars,
        &["dx"],
        3,
        &[vec![NAN, NAN, NAN, 100.0 / 3.0, 100.0 / 3.0, 1300.0 / 21.0, 20000.0 / 13800.0]],
    );

    // The first value lands at index `period`, and a series of `period` bars or fewer has none.
    for name in ["dx", "plus_di", "minus_di", "plus_dm", "minus_dm"] {
        assert_eq!(lookback(name, &p), Some(3), "{name}: first value at index 3");
        let short = compute(name, &p, &bars[..3]).expect("a registry key");
        assert!(short[0].1.iter().all(|v| v.is_nan()), "{name}: 3 bars at period 3 is all warm-up");
    }
}

/// A smooth-ish synthetic series with real highs, lows and gaps.
fn wavy_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let t = i as f64;
            let c = 100.0 + (t * 0.37).sin() * 5.0 + t * 0.05;
            let o = c - 0.3 * (t * 0.7).cos();
            let h = c.max(o) + 1.0 + (t * 0.11).cos().abs();
            let l = c.min(o) - 1.0 - (t * 0.23).sin().abs();
            Bar::ohlcv(i as i64 * 60_000, o, h, l, c, 1000.0 + t)
        })
        .collect()
}

/// `adx` already carries `+DI` and `-DI`; the new rows must be the same numbers (to rounding: see `same_bits`), and
/// `dx` must be the ratio of those two lines. This is a real cross-check: it is two kernels written
/// separately that must agree.
#[test]
fn directional_indicators_equal_the_adx_lines() {
    let bars = wavy_bars(160);
    for period in [3.0, 14.0, 50.0] {
        let adx = compute("adx", &[period], &bars).expect("adx");
        let names: Vec<&str> = adx.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["adx", "plus_di", "minus_di"]);
        let (adx_plus, adx_minus) = (&adx[1].1, &adx[2].1);

        let plus = compute("plus_di", &[period], &bars).expect("plus_di");
        let minus = compute("minus_di", &[period], &bars).expect("minus_di");
        assert!(same_bits(&plus[0].1, adx_plus), "plus_di differs from adx's plus_di at {period}");
        assert!(
            same_bits(&minus[0].1, adx_minus),
            "minus_di differs from adx's minus_di at {period}"
        );

        let dx = compute("dx", &[period], &bars).expect("dx");
        for i in 0..bars.len() {
            let (p, m) = (adx_plus[i], adx_minus[i]);
            let got = dx[0].1[i];
            if p.is_nan() {
                assert!(got.is_nan(), "dx[{i}] must be warm-up where adx's DI lines are");
            } else {
                let want = if p + m > 0.0 { 100.0 * (p - m).abs() / (p + m) } else { 0.0 };
                assert!((got - want).abs() < 1e-9, "dx[{i}] at {period}: got {got}, want {want}");
            }
        }
    }
}

// ============================ random walk index ============================

/// Period 2 (so `sqrt(2)`), four bars:
///
/// ```text
/// bar   h   l   c    TR
///  0   10   8   9     -
///  1   11   9  10     2    max(2, |11-9|, |9-9|)
///  2   15   9  14     6    max(6, |15-10|, |9-10|)
///  3   14  10  12     4    max(4, |14-14|, |10-14|)
/// ```
///
/// Bar 2: ATR = (2+6)/2 = 4, high = (h2 - l0)/(4*sqrt2) = 7/(4*sqrt2), low = (h0 - l2)/(4*sqrt2) = 1/(4*sqrt2).
/// Bar 3: ATR = (6+4)/2 = 5, high = (h3 - l1)/(5*sqrt2) = 5/(5*sqrt2), low = (h1 - l3)/(5*sqrt2) = 1/(5*sqrt2).
#[test]
fn rwi_matches_the_hand_derivation() {
    let bars =
        bars_hlc(&[(10.0, 8.0, 9.0), (11.0, 9.0, 10.0), (15.0, 9.0, 14.0), (14.0, 10.0, 12.0)]);
    let r2 = 2.0_f64.sqrt();
    check(
        "rwi",
        &[2.0],
        &bars,
        &["rwi_high", "rwi_low"],
        2,
        &[
            vec![NAN, NAN, 7.0 / (4.0 * r2), 5.0 / (5.0 * r2)],
            vec![NAN, NAN, 1.0 / (4.0 * r2), 1.0 / (5.0 * r2)],
        ],
    );
    assert_eq!(lookback("rwi", &[2.0]), Some(2));
}

// ================================= qstick ==================================

/// `close - open` per bar is 1, -1, 3, -1, 4, -1; the period-3 simple average of that is
/// (1-1+3)/3 = 1, (-1+3-1)/3 = 1/3, (3-1+4)/3 = 2, (-1+4-1)/3 = 2/3.
#[test]
fn qstick_is_the_average_of_close_minus_open() {
    let bars = bars_oc(&[
        (10.0, 11.0),
        (11.0, 10.0),
        (10.0, 13.0),
        (13.0, 12.0),
        (12.0, 16.0),
        (16.0, 15.0),
    ]);
    check(
        "qstick",
        &[3.0],
        &bars,
        &["qstick"],
        2,
        &[vec![NAN, NAN, 1.0, 1.0 / 3.0, 2.0, 2.0 / 3.0]],
    );
    assert_eq!(lookback("qstick", &[3.0]), Some(2));
}

// ========================== chaikin volatility =============================

/// High-low ranges 4, 2, 3, 6, 3; `ema_period` 2 (alpha 2/3, seeded at bar 1 with the mean 3):
/// EMA = [-, 3, 3, 5, 11/3] (bar 2: 2/3*3 + 1/3*3 = 3; bar 3: 2/3*6 + 1/3*3 = 5;
/// bar 4: 2/3*3 + 1/3*5 = 11/3). `roc_period` 2 is the percent change against two bars earlier:
/// bar 3: (5-3)/3*100 = 200/3, bar 4: (11/3-3)/3*100 = 200/9. Bar 2 would compare with the EMA at
/// bar 0, which does not exist, so the first value is at (2-1)+2 = 3.
#[test]
fn chaikin_volatility_is_the_roc_of_the_ema_of_the_range() {
    let ranges = [4.0, 2.0, 3.0, 6.0, 3.0];
    let bars: Vec<Bar> = ranges
        .iter()
        .enumerate()
        .map(|(i, &r)| Bar::ohlcv(i as i64 * 60_000, 10.0, 10.0 + r, 10.0, 10.0, 1.0))
        .collect();
    check(
        "chaikin_volatility",
        &[2.0, 2.0],
        &bars,
        &["chaikin_volatility"],
        3,
        &[vec![NAN, NAN, NAN, 200.0 / 3.0, 200.0 / 9.0]],
    );
    assert_eq!(lookback("chaikin_volatility", &[2.0, 2.0]), Some(3));
}

// ============================== williams a/d ===============================

/// Per bar, with the previous close `pc`: close > pc adds `c - min(l, pc)`; close < pc adds
/// `c - max(h, pc)`; equal adds 0; the first bar is 0.
///
/// ```text
/// bar   h   l   c   pc   step                     running
///  0   10   8   9    -   -                          0
///  1   11   9  10    9   10 - min(9, 9)  =  1       1
///  2   12   8   9   10    9 - max(12,10) = -3      -2
///  3   10   8   9    9   equal           =  0      -2
///  4   13   7  12    9   12 - min(7, 9)  =  5       3
///  5   12  10  11   12   11 - max(12,12) = -1       2
/// ```
#[test]
fn williams_ad_matches_the_hand_derivation() {
    let bars = bars_hlc(&[
        (10.0, 8.0, 9.0),
        (11.0, 9.0, 10.0),
        (12.0, 8.0, 9.0),
        (10.0, 8.0, 9.0),
        (13.0, 7.0, 12.0),
        (12.0, 10.0, 11.0),
    ]);
    check("williams_ad", &[], &bars, &["williams_ad"], 0, &[vec![0.0, 1.0, -2.0, -2.0, 3.0, 2.0]]);
    assert_eq!(lookback("williams_ad", &[]), Some(0));
    // A single bar is the start of the sum.
    check("williams_ad", &[], &bars[..1], &["williams_ad"], 0, &[vec![0.0]]);
}

// ============================== anchored vwap ==============================

fn vwap_bars(volumes: [f64; 6]) -> Vec<Bar> {
    // (high, low, close) with typical price (h+l+c)/3 = 10, 11, 12, 13, 14, 15; timestamps 0, 1000, ...
    let hlc = [
        (12.0, 8.0, 10.0),
        (13.0, 9.0, 11.0),
        (14.0, 10.0, 12.0),
        (15.0, 11.0, 13.0),
        (16.0, 12.0, 14.0),
        (17.0, 13.0, 15.0),
    ];
    hlc.iter()
        .zip(volumes)
        .enumerate()
        .map(|(i, (&(h, l, c), v))| Bar::ohlcv(i as i64 * 1000, c, h, l, c, v))
        .collect()
}

/// Volumes 100, 200, 100, 300, 0, 100; anchor 2000 ms. Accumulation starts at bar 2 (ts 2000):
/// bar 2: 12*100/100 = 12; bar 3: (1200 + 13*300)/400 = 5100/400 = 12.75;
/// bar 4 (volume 0): unchanged 12.75; bar 5: (5100 + 15*100)/500 = 6600/500 = 13.2.
#[test]
fn anchored_vwap_starts_at_the_first_bar_at_or_after_the_anchor() {
    let bars = vwap_bars([100.0, 200.0, 100.0, 300.0, 0.0, 100.0]);
    check(
        "anchored_vwap",
        &[2000.0],
        &bars,
        &["anchored_vwap"],
        2,
        &[vec![NAN, NAN, 12.0, 12.75, 12.75, 13.2]],
    );
    // An anchor between two bars starts at the next one (ts 3000): 13, 13 (volume 0), (3900+1500)/400 = 13.5.
    check(
        "anchored_vwap",
        &[2500.0],
        &bars,
        &["anchored_vwap"],
        3,
        &[vec![NAN, NAN, NAN, 13.0, 13.0, 13.5]],
    );
    // An anchor after the last bar never starts.
    check("anchored_vwap", &[1.0e12], &bars, &["anchored_vwap"], 6, &[vec![NAN; 6]]);
}

/// The default anchor (0) is the first bar. With no volume yet there is nothing to weight, so the
/// value stays NaN until volume appears: volumes 0, 200, 100, 300, 0, 100 give bar 0 NaN,
/// bar 1: 11*200/200 = 11, bar 2: (2200 + 12*100)/300 = 34/3, bar 3: (3400 + 13*300)/600 = 73/6,
/// bar 4 unchanged, bar 5: (7300 + 15*100)/700 = 88/7.
#[test]
fn anchored_vwap_default_anchor_is_the_first_bar_and_waits_for_volume() {
    let bars = vwap_bars([0.0, 200.0, 100.0, 300.0, 0.0, 100.0]);
    check(
        "anchored_vwap",
        &[],
        &bars,
        &["anchored_vwap"],
        1,
        &[vec![NAN, 11.0, 34.0 / 3.0, 73.0 / 6.0, 73.0 / 6.0, 88.0 / 7.0]],
    );
}

// ============================== heikin-ashi ================================

/// Bars (o, h, l, c) and their candles:
///
/// ```text
/// bar  o   h   l   c    ha_close = (o+h+l+c)/4   ha_open                       ha_high            ha_low
///  0  10  14   9  12    45/4 = 11.25            (o+c)/2 = 11                   max(14,11,11.25)=14  min(9,11,11.25)=9
///  1  12  16  11  15    54/4 = 13.5             (11+11.25)/2 = 11.125          16                   min(11,11.125,13.5)=11
///  2  15  15   7   8    45/4 = 11.25            (11.125+13.5)/2 = 12.3125      15                   7
///  3   8   9   6   7    30/4 = 7.5              (12.3125+11.25)/2 = 11.78125   max(9,11.78125,7.5)=11.78125  6
/// ```
#[test]
fn heikin_ashi_matches_the_hand_derivation() {
    let rows = [
        (10.0, 14.0, 9.0, 12.0),
        (12.0, 16.0, 11.0, 15.0),
        (15.0, 15.0, 7.0, 8.0),
        (8.0, 9.0, 6.0, 7.0),
    ];
    let bars: Vec<Bar> = rows
        .iter()
        .enumerate()
        .map(|(i, &(o, h, l, c))| Bar::ohlcv(i as i64 * 60_000, o, h, l, c, 1.0))
        .collect();
    check(
        "heikin_ashi",
        &[],
        &bars,
        &["ha_open", "ha_high", "ha_low", "ha_close"],
        0,
        &[
            vec![11.0, 11.125, 12.3125, 11.78125],
            vec![14.0, 16.0, 15.0, 11.78125],
            vec![9.0, 11.0, 7.0, 6.0],
            vec![11.25, 13.5, 11.25, 7.5],
        ],
    );
    assert_eq!(lookback("heikin_ashi", &[]), Some(0));
}

// ============================== statistics =================================

/// Period 3 windows of the close against the index 0, 1, 2 (so Σdx² = 2, dx = -1, 0, 1):
///
/// ```text
/// window    mean y  dy                    Σdx·dy  Σdy²    r² = (Σdx·dy)² / (Σdx²·Σdy²)
/// [1,2,4]   7/3     -4/3, -1/3, 5/3       3       14/3    9 / (2*14/3) = 27/28
/// [2,4,3]   3       -1, 1, 0              1       2       1 / (2*2) = 1/4
/// [4,3,3]   10/3    2/3, -1/3, -1/3       -1      2/3     1 / (2*2/3) = 3/4
/// [3,3,3]   -       constant              -       -       undefined (NaN)
/// ```
#[test]
fn r_squared_matches_the_hand_derivation() {
    let bars = bars_close(&[1.0, 2.0, 4.0, 3.0, 3.0, 3.0]);
    check(
        "r_squared",
        &[3.0],
        &bars,
        &["r_squared"],
        2,
        &[vec![NAN, NAN, 27.0 / 28.0, 0.25, 0.75, NAN]],
    );
    assert_eq!(lookback("r_squared", &[3.0]), Some(2));
    // A perfect straight line is exactly 1: dy = -2, 0, 2, so (4/2) * (4/8) = 1.
    check(
        "r_squared",
        &[3.0],
        &bars_close(&[5.0, 7.0, 9.0]),
        &["r_squared"],
        2,
        &[vec![NAN, NAN, 1.0]],
    );
}

/// Period 5, lag 1 on 1, 3, 2, 5, 4, 7. The window's (value, next value) pairs are correlated, each
/// side about its own mean:
///
/// ```text
/// window [1,3,2,5,4]: x = 1,3,2,5 (mean 2.75)  y = 3,2,5,4 (mean 3.5)
///   dx = -1.75, .25, -.75, 2.25   dy = -.5, -1.5, 1.5, .5
///   Σdx·dy = .875 - .375 - 1.125 + 1.125 = .5   Σdx² = 8.75   Σdy² = 5
///   r = .5 / sqrt(8.75*5) = .5 / sqrt(43.75) = 1 / sqrt(175)
/// window [3,2,5,4,7]: x = 3,2,5,4 (mean 3.5)  y = 2,5,4,7 (mean 4.5)
///   dx = -.5, -1.5, 1.5, .5       dy = -2.5, .5, -.5, 2.5
///   Σdx·dy = 1.25 - .75 - .75 + 1.25 = 1   Σdx² = 5   Σdy² = 13   r = 1 / sqrt(65)
/// ```
///
/// Lag 2 on the first window pairs x = 1,3,2 with y = 2,5,4 (means 2 and 11/3): dx = -1, 1, 0,
/// dy = -5/3, 4/3, 1/3, Σdx·dy = 3, Σdx² = 2, Σdy² = 14/3, so r = 3 / sqrt(2*14/3) = 3 / sqrt(28/3).
/// On the second window y is x shifted up by exactly 2 (x = 3,2,5; y = 5,4,7), so r = 1.
#[test]
fn autocorr_matches_the_hand_derivation() {
    let bars = bars_close(&[1.0, 3.0, 2.0, 5.0, 4.0, 7.0]);
    check(
        "autocorr",
        &[5.0, 1.0],
        &bars,
        &["autocorr"],
        4,
        &[vec![NAN, NAN, NAN, NAN, 1.0 / 175.0_f64.sqrt(), 1.0 / 65.0_f64.sqrt()]],
    );
    check(
        "autocorr",
        &[5.0, 2.0],
        &bars,
        &["autocorr"],
        4,
        &[vec![NAN, NAN, NAN, NAN, 3.0 / (28.0_f64 / 3.0).sqrt(), 1.0]],
    );
}

/// A constant side has no correlation, and a window with fewer than two pairs has none either.
///
/// Closes 3,3,3,3,3,4,5 at period 5, lag 1: the windows ending at bars 4 and 5 have a constant x side
/// (3,3,3,3), so NaN. The window [3,3,3,4,5] pairs x = 3,3,3,4 (mean 3.25) with y = 3,3,4,5 (mean
/// 3.75): dx = -.25, -.25, -.25, .75, dy = -.75, -.75, .25, 1.25, Σdx·dy = .1875+.1875-.0625+.9375 =
/// 1.25, Σdx² = .75, Σdy² = 2.75, r = 1.25 / sqrt(.75*2.75) = 1.25 / sqrt(2.0625).
#[test]
fn autocorr_is_undefined_for_a_constant_side_or_too_short_a_window() {
    let bars = bars_close(&[3.0, 3.0, 3.0, 3.0, 3.0, 4.0, 5.0]);
    check(
        "autocorr",
        &[5.0, 1.0],
        &bars,
        &["autocorr"],
        6,
        &[vec![NAN, NAN, NAN, NAN, NAN, NAN, 1.25 / 2.0625_f64.sqrt()]],
    );
    // period 5 with lag 4 leaves one pair: undefined at every bar.
    let moving = bars_close(&[1.0, 3.0, 2.0, 5.0, 4.0, 7.0, 6.0, 9.0]);
    check("autocorr", &[5.0, 4.0], &moving, &["autocorr"], 8, &[vec![NAN; 8]]);
}

/// Seventeen closes, 16 log returns, block sizes 8 and 16. The closes are powers of two so every
/// return is `+ln2`, `-ln2` and R/S does not depend on the `ln2` scale: the returns are the signs
/// `+ + + + - - - - - - - - + + + +` (exponents 0 1 2 3 4 3 2 1 0 -1 -2 -3 -4 -3 -2 -1 0).
///
/// ```text
/// size 8:  block A = + + + + - - - -  mean 0, std 1, running sum 1 2 3 4 3 2 1 0   range 4-0 = 4   R/S 4
///          block B = - - - - + + + +  mean 0, std 1, running sum -1 -2 -3 -4 -3 -2 -1 0  range 0-(-4) = 4  R/S 4
///          mean R/S = 4
/// size 16: one block, mean 0, std 1, running sum 1 2 3 4 3 2 1 0 -1 -2 -3 -4 -3 -2 -1 0
///          range 4-(-4) = 8   R/S 8
/// slope of ln(mean R/S) on ln(size): (ln 8 - ln 4) / (ln 16 - ln 8) = ln2 / ln2 = 1
/// ```
///
/// The second series repeats block A twice, so the size-16 range is 4, not 8: both R/S are 4 and the
/// slope is 0.
#[test]
fn hurst_matches_the_hand_derivation() {
    let persistent = [
        1.0, 2.0, 4.0, 8.0, 16.0, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.125, 0.0625, 0.125, 0.25, 0.5,
        1.0,
    ];
    let mut want = vec![NAN; 16];
    want.push(1.0);
    check("hurst", &[17.0], &bars_close(&persistent), &["hurst"], 16, &[want]);
    assert_eq!(lookback("hurst", &[17.0]), Some(16));

    let flat_slope =
        [1.0, 2.0, 4.0, 8.0, 16.0, 8.0, 4.0, 2.0, 1.0, 2.0, 4.0, 8.0, 16.0, 8.0, 4.0, 2.0, 1.0];
    let mut want = vec![NAN; 16];
    want.push(0.0);
    check("hurst", &[17.0], &bars_close(&flat_slope), &["hurst"], 16, &[want]);
}

/// A series that never moves has blocks with no deviation (skipped), so no block size produces a
/// value; a non-positive close has no log return, so every window holding it is NaN.
#[test]
fn hurst_is_undefined_without_variation_or_with_a_non_positive_close() {
    check("hurst", &[17.0], &bars_close(&[5.0; 20]), &["hurst"], 20, &[vec![NAN; 20]]);

    let mut closes = [
        1.0, 2.0, 4.0, 8.0, 16.0, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.125, 0.0625, 0.125, 0.25, 0.5,
        1.0,
    ];
    closes[8] = 0.0;
    check("hurst", &[17.0], &bars_close(&closes), &["hurst"], 17, &[vec![NAN; 17]]);
}

// ============================== every new row ==============================

/// Every new row answers an empty series with empty lines (one per output) and a one-bar series with
/// one value per line.
#[test]
fn every_new_row_handles_empty_and_single_bar_input() {
    let rows: [(&str, usize); 14] = [
        ("dx", 1),
        ("plus_di", 1),
        ("minus_di", 1),
        ("plus_dm", 1),
        ("minus_dm", 1),
        ("rwi", 2),
        ("qstick", 1),
        ("chaikin_volatility", 1),
        ("williams_ad", 1),
        ("anchored_vwap", 1),
        ("heikin_ashi", 4),
        ("r_squared", 1),
        ("autocorr", 1),
        ("hurst", 1),
    ];
    let one = [Bar::ohlcv(0, 10.0, 12.0, 9.0, 11.0, 5.0)];
    for (name, arity) in rows {
        let empty =
            compute(name, &[], &[]).unwrap_or_else(|| panic!("{name} is not a registry key"));
        assert_eq!(empty.len(), arity, "{name}: output lines");
        assert!(empty.iter().all(|(_, v)| v.is_empty()), "{name}: no bars, no values");
        let single = compute(name, &[], &one).expect("a registry key");
        assert_eq!(single.len(), arity, "{name}: output lines on one bar");
        assert!(single.iter().all(|(_, v)| v.len() == 1), "{name}: one bar, one value per line");
    }
}
