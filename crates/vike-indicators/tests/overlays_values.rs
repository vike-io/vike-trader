//! Value tests for the overlays added after the first release (`kama`, `vidya`, `frama`, `evwma`,
//! `median_ma`, `chandelier`, `hilo_activator`, `atr_trailing_stop`, `ttm_squeeze`, `starc`,
//! `acceleration_bands`, `linreg_channel`), through the public door only: `compute(name, params,
//! bars)`.
//!
//! Every expected number below was derived BY HAND from the formula in the type's documentation, on
//! a series small enough to do that (period 2 or 3, six or seven bars), and chosen so the working is
//! exact in binary floating point wherever the formula allows it: those assertions use a tolerance
//! of `0.0`, i.e. bit-for-bit equality of the value. A tolerance of `1e-9` is used ONLY where the
//! formula itself leaves the rationals (a square root, an `exp`, or a thirds fraction), and it is
//! there for that reason alone; it is nowhere near the size of an arithmetic mistake.
//!
//! The streaming path is not re-tested here: `tests/parity.rs` already holds `on_bar` to
//! `vectorize` bit for bit for every registry row.

use vike_indicators::{compute, get};
use vike_marketdata::Bar;

const N: f64 = f64::NAN;

/// Bars from `(high, low, close, volume)` rows; the open is the close.
fn hlcv(rows: &[(f64, f64, f64, f64)]) -> Vec<Bar> {
    rows.iter()
        .enumerate()
        .map(|(i, &(h, l, c, v))| Bar::ohlcv(i as i64 * 60_000, c, h, l, c, v))
        .collect()
}

/// Bars from `(high, low, close)` rows with volume 1.
fn hlc(rows: &[(f64, f64, f64)]) -> Vec<Bar> {
    let with_volume: Vec<(f64, f64, f64, f64)> =
        rows.iter().map(|&(h, l, c)| (h, l, c, 1.0)).collect();
    hlcv(&with_volume)
}

/// Bars whose high, low and close are all the given close (the ranges come from the closes alone).
fn closes(cs: &[f64]) -> Vec<Bar> {
    cs.iter().enumerate().map(|(i, &c)| Bar::ohlcv(i as i64 * 60_000, c, c, c, c, 1.0)).collect()
}

fn run(name: &str, params: &[f64], bars: &[Bar]) -> Vec<(&'static str, Vec<f64>)> {
    compute(name, params, bars).unwrap_or_else(|| panic!("{name} is not in the registry"))
}

fn line<'a>(out: &'a [(&'static str, Vec<f64>)], name: &str) -> &'a [f64] {
    let found = out.iter().find(|(n, _)| *n == name);
    &found.unwrap_or_else(|| panic!("no output line named {name}")).1
}

/// `NaN` must be `NaN`; every other position must be within `tol` of the expected value (`0.0` =
/// exactly equal).
fn assert_series(what: &str, got: &[f64], want: &[f64], tol: f64) {
    assert_eq!(got.len(), want.len(), "{what}: one value per bar");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{what}[{i}]: expected the warm-up NaN, got {g}");
        } else {
            assert!(!g.is_nan() && (g - w).abs() <= tol, "{what}[{i}]: got {g}, expected {w}");
        }
    }
}

fn names(out: &[(&'static str, Vec<f64>)]) -> Vec<&'static str> {
    out.iter().map(|(n, _)| *n).collect()
}

/// Index of the first non-NaN value; every value from there on must be a number too.
fn first_number(what: &str, line: &[f64]) -> usize {
    let first = line.iter().position(|v| !v.is_nan()).unwrap_or(line.len());
    assert!(line[first..].iter().all(|v| !v.is_nan()), "{what}: a NaN after the warm-up");
    first
}

// ============================== kama =======================================

/// period 2, fast 1 (`2/(1+1) = 1`), slow 3 (`2/(3+1) = 1/2`), so `sc = (ER/2 + 1/2)^2`.
///
/// closes 10, 12, 10, 10, 14, 15.5, 15; the seed is the close at index `period - 1 = 1` (12).
/// - idx 2: window 10,12,10: path 4, net 0, ER 0, sc 1/4: 12 + (10 - 12)/4 = 11.5
/// - idx 3: window 12,10,10: path 2, net -2, ER 1, sc 1: 11.5 + (10 - 11.5) = 10
/// - idx 4: window 10,10,14: path 4, net 4 (path = net, ER 1), sc 1: 10 + 4 = 14
/// - idx 5: window 10,14,15.5: path 5.5, net 5.5, ER 1, sc 1: 14 + 1.5 = 15.5
/// - idx 6: window 14,15.5,15: path 2, net 1, ER 1/2, sc (1/4 + 1/2)^2 = 9/16:
///   15.5 + (15 - 15.5) * 9/16 = 15.21875
#[test]
fn kama_hand_derived_series() {
    let bars = closes(&[10.0, 12.0, 10.0, 10.0, 14.0, 15.5, 15.0]);
    let out = run("kama", &[2.0, 1.0, 3.0], &bars);
    assert_eq!(names(&out), ["kama"]);
    assert_series("kama", line(&out, "kama"), &[N, N, 11.5, 10.0, 14.0, 15.5, 15.21875], 0.0);
}

#[test]
fn kama_flat_series_stays_flat() {
    // A flat window has path 0 and net 0 (ER is taken as 1), and every close equals the seed.
    let out = run("kama", &[3.0], &closes(&[7.0; 8]));
    assert_series("kama", line(&out, "kama"), &[N, N, N, 7.0, 7.0, 7.0, 7.0, 7.0], 0.0);
}

// ============================== vidya ======================================

/// period 3 (`alpha = 2/(3+1) = 1/2`), cmo_period 2; closes 10, 12, 10, 16, 17, 14.
/// The seed is the close before the first output (idx 1: 12); `vi` = |up - down| / (up + down)
/// over the last 2 changes, `VIDYA = alpha*vi*close + (1 - alpha*vi)*prev`.
/// - idx 2: changes +2, -2: up 2, down 2, vi 0: VIDYA = 12
/// - idx 3: changes -2, +6: up 6, down 2, vi 1/2, k 1/4: 16/4 + 12*3/4 = 13
/// - idx 4: changes +6, +1: up 7, down 0, vi 1, k 1/2: 17/2 + 13/2 = 15
/// - idx 5: changes +1, -3: up 1, down 3, vi 1/2, k 1/4: 14/4 + 15*3/4 = 14.75
#[test]
fn vidya_hand_derived_series() {
    let bars = closes(&[10.0, 12.0, 10.0, 16.0, 17.0, 14.0]);
    let out = run("vidya", &[3.0, 2.0], &bars);
    assert_eq!(names(&out), ["vidya"]);
    assert_series("vidya", line(&out, "vidya"), &[N, N, 12.0, 13.0, 15.0, 14.75], 0.0);
}

#[test]
fn vidya_flat_series_stays_flat() {
    // No movement at all: vi is 0 by definition, so the value stays at its seed.
    let out = run("vidya", &[5.0, 2.0], &closes(&[5.0, 5.0, 5.0, 5.0]));
    assert_series("vidya", line(&out, "vidya"), &[N, N, 5.0, 5.0], 0.0);
}

// ============================== frama ======================================

/// period 4 (halves of 2), bars with high = low = close. Closes 10, 12, 10, 12, 10, 12.
/// Seed: the close at idx 3 (12). From idx 4 every window alternates 12,10,12,10 or 10,12,10,12:
/// each half spans 2 (N1 = N2 = 2/2 = 1) and the whole window spans 2 (N3 = 2/4 = 1/2), so
/// D = (ln 2 - ln 1/2)/ln 2 = 2 and alpha = exp(-4.6) =: a (inside [0.01, 1]).
/// - idx 4: a*10 + (1 - a)*12 = 12 - 2a
/// - idx 5: a*12 + (1 - a)*(12 - 2a) = 12 - 2a + 2a^2
#[test]
fn frama_choppy_window_uses_the_slowest_alpha() {
    let a = (-4.6f64).exp();
    let bars = closes(&[10.0, 12.0, 10.0, 12.0, 10.0, 12.0]);
    let out = run("frama", &[4.0], &bars);
    assert_eq!(names(&out), ["frama"]);
    let want = [N, N, N, 12.0, 12.0 - 2.0 * a, 12.0 - 2.0 * a + 2.0 * a * a];
    // 1e-9: `a` is an exp(), the rest is exact.
    assert_series("frama", line(&out, "frama"), &want, 1e-9);
}

/// Closes 10, 11, 11, 12, 13. Seed at idx 3: 12. Window at idx 4 is 11,11,12,13: the older half
/// spans 0 (N1 = 0), the newer 1 (N2 = 1/2), the whole 2 (N3 = 2/4 = 1/2): D = (ln 1/2 - ln 1/2)/ln 2
/// = 0, so exp(-4.6*(0 - 1)) = exp(4.6) ~ 99.5 is clamped to 1 and FRAMA = the close, 13.
#[test]
fn frama_trending_window_clamps_alpha_to_one() {
    let out = run("frama", &[4.0], &closes(&[10.0, 11.0, 11.0, 12.0, 13.0]));
    assert_series("frama", line(&out, "frama"), &[N, N, N, 12.0, 13.0], 0.0);
}

/// A window with no range at all (N3 = 0) takes alpha = 1; the value is the (constant) close.
#[test]
fn frama_flat_series_is_the_price() {
    let out = run("frama", &[4.0], &closes(&[5.0; 5]));
    assert_series("frama", line(&out, "frama"), &[N, N, N, 5.0, 5.0], 0.0);
}

// ============================== evwma ======================================

/// period 3; closes 10, 20, 30, 40, 50 with volumes 1, 1, 1, 3, 1. Seed: the close at idx 2 (30).
/// - idx 3: V = 1 + 1 + 3 = 5, v = 3: ((5 - 3)*30 + 3*40)/5 = 36
/// - idx 4: V = 1 + 3 + 1 = 5, v = 1: ((5 - 1)*36 + 1*50)/5 = 194/5 = 38.8
#[test]
fn evwma_hand_derived_series() {
    let bars = hlcv(&[
        (10.0, 10.0, 10.0, 1.0),
        (20.0, 20.0, 20.0, 1.0),
        (30.0, 30.0, 30.0, 1.0),
        (40.0, 40.0, 40.0, 3.0),
        (50.0, 50.0, 50.0, 1.0),
    ]);
    let out = run("evwma", &[3.0], &bars);
    assert_eq!(names(&out), ["evwma"]);
    // 1e-9: 194/5 is not a binary fraction.
    assert_series("evwma", line(&out, "evwma"), &[N, N, 30.0, 36.0, 38.8], 1e-9);
}

/// No volume traded in the window (V = 0): the previous value is carried, not divided by zero.
#[test]
fn evwma_without_volume_carries_the_seed() {
    let bars = hlcv(&[
        (10.0, 10.0, 10.0, 0.0),
        (20.0, 20.0, 20.0, 0.0),
        (30.0, 30.0, 30.0, 0.0),
        (40.0, 40.0, 40.0, 0.0),
    ]);
    let out = run("evwma", &[3.0], &bars);
    assert_series("evwma", line(&out, "evwma"), &[N, N, 30.0, 30.0], 0.0);
}

// ============================== median_ma ==================================

/// period 3 over 5, 1, 3, 9, 2, 4: medians of (5,1,3) (1,3,9) (3,9,2) (9,2,4) = 3, 3, 3, 4.
#[test]
fn median_ma_odd_period() {
    let out = run("median_ma", &[3.0], &closes(&[5.0, 1.0, 3.0, 9.0, 2.0, 4.0]));
    assert_eq!(names(&out), ["median_ma"]);
    assert_series("median_ma", line(&out, "median_ma"), &[N, N, 3.0, 3.0, 3.0, 4.0], 0.0);
}

/// period 4 over 5, 1, 3, 9, 2: sorted (1,3,5,9) -> (3 + 5)/2 = 4; sorted (1,2,3,9) -> (2 + 3)/2 = 2.5.
#[test]
fn median_ma_even_period_averages_the_middle_pair() {
    let out = run("median_ma", &[4.0], &closes(&[5.0, 1.0, 3.0, 9.0, 2.0]));
    assert_series("median_ma", line(&out, "median_ma"), &[N, N, N, 4.0, 2.5], 0.0);
}

// ============================== chandelier =================================

/// Bars (high, low, close): (11,9,10) (12,10,11) (13,10,12) (12,9,10). period 2, mult 1.
/// True ranges: 2, max(2,|12-10|,|10-10|) = 2, max(3,|13-11|,|10-11|) = 3, max(3,|12-12|,|9-12|) = 3.
/// Wilder ATR(2) seeded with the mean of the first two: 2, then (2*1 + 3)/2 = 2.5, (2.5*1 + 3)/2 = 2.75.
/// Highest high of 2: 12, 13, 13. Lowest low of 2: 9, 10, 9.
/// long_stop  = HH - ATR: 12 - 2, 13 - 2.5, 13 - 2.75 = 10, 10.5, 10.25
/// short_stop = LL + ATR:  9 + 2, 10 + 2.5,  9 + 2.75 = 11, 12.5, 11.75
#[test]
fn chandelier_hand_derived_series() {
    let bars = hlc(&[(11.0, 9.0, 10.0), (12.0, 10.0, 11.0), (13.0, 10.0, 12.0), (12.0, 9.0, 10.0)]);
    let out = run("chandelier", &[2.0, 1.0], &bars);
    assert_eq!(names(&out), ["long_stop", "short_stop"]);
    assert_series("long_stop", line(&out, "long_stop"), &[N, 10.0, 10.5, 10.25], 0.0);
    assert_series("short_stop", line(&out, "short_stop"), &[N, 11.0, 12.5, 11.75], 0.0);
}

// ============================== hilo_activator =============================

/// period 2. Bars (high, low, close): (10,8,9) (12,9,11) (13,10,12.5) (12,7,7.5) (10,8,11) (15,11,14).
/// hi = SMA(high, 2): -, 11, 12.5, 12.5, 11, 12.5.   lo = SMA(low, 2): -, 8.5, 9.5, 8.5, 7.5, 9.5.
/// - idx 1: seed. close 11 >= (11 + 8.5)/2 = 9.75: up, line = lo = 8.5
/// - idx 2: close 12.5 > previous hi 11: up, line = lo = 9.5
/// - idx 3: close 7.5 < previous lo 9.5: down, line = hi = 12.5
/// - idx 4: close 11 is neither above previous hi 12.5 nor below previous lo 8.5: stays down, line = hi = 11
/// - idx 5: close 14 > previous hi 11: up, line = lo = 9.5
#[test]
fn hilo_activator_hand_derived_series() {
    let bars = hlc(&[
        (10.0, 8.0, 9.0),
        (12.0, 9.0, 11.0),
        (13.0, 10.0, 12.5),
        (12.0, 7.0, 7.5),
        (10.0, 8.0, 11.0),
        (15.0, 11.0, 14.0),
    ]);
    let out = run("hilo_activator", &[2.0], &bars);
    assert_eq!(names(&out), ["hilo"]);
    assert_series("hilo", line(&out, "hilo"), &[N, 8.5, 9.5, 12.5, 11.0, 9.5], 0.0);
}

// ============================== atr_trailing_stop ==========================

/// period 2, mult 1. Bars (high, low, close): (11,9,10) (12,10,11) (13,10,12) (12,9,10) (9,6,7) (8,6,6).
/// True ranges 2, 2, 3, 3, max(3,|9-10|,|6-10|) = 4, max(2,|8-7|,|6-7|) = 2, so Wilder ATR(2) is
/// -, 2, 2.5, 2.75, 3.375, 2.6875.
/// - idx 1: first ATR, stop = close - loss = 11 - 2 = 9
/// - idx 2: close 12 and previous close 11 both above 9: max(9, 12 - 2.5) = 9.5
/// - idx 3: close 10 and previous close 12 both above 9.5: max(9.5, 10 - 2.75) = 9.5
/// - idx 4: close 7 below 9.5 but the previous close 10 was not: flips, 7 + 3.375 = 10.375
/// - idx 5: close 6 and previous close 7 both below 10.375: min(10.375, 6 + 2.6875) = 8.6875
#[test]
fn atr_trailing_stop_hand_derived_series() {
    let bars = hlc(&[
        (11.0, 9.0, 10.0),
        (12.0, 10.0, 11.0),
        (13.0, 10.0, 12.0),
        (12.0, 9.0, 10.0),
        (9.0, 6.0, 7.0),
        (8.0, 6.0, 6.0),
    ]);
    let out = run("atr_trailing_stop", &[2.0, 1.0], &bars);
    assert_eq!(names(&out), ["stop"]);
    assert_series("stop", line(&out, "stop"), &[N, 9.0, 9.5, 9.5, 10.375, 8.6875], 0.0);
}

// ============================== ttm_squeeze ================================

/// Closes 10, 11, 12, 11, 12, 13 with high = close + 1 and low = close - 1; every true range is 2.
/// length 3. mid = SMA(close, 3): -, -, 11, 34/3, 35/3, 12.
/// Highest high / lowest low of 3 bars: 13/9, 13/10, 13/10, 14/10 -> their midpoint 11, 11.5, 11.5, 12.
/// delta = close - ((midpoint) + mid)/2: idx 2: 12 - 11 = 1; idx 3: 11 - (23/2 + 34/3)/2 = -5/12;
/// idx 4: 12 - (23/2 + 35/3)/2 = 5/12; idx 5: 13 - 12 = 1.
/// momentum = value of the least-squares line at the last of 3 points; for three points that is
/// mean + (y2 - y0)/2: idx 4 over (1, -5/12, 5/12): 1/3 + (5/12 - 1)/2 = 1/24;
/// idx 5 over (-5/12, 5/12, 1): 1/3 + (1 + 5/12)/2 = 25/24.
#[test]
fn ttm_squeeze_momentum_hand_derived() {
    let rows: Vec<(f64, f64, f64)> =
        [10.0, 11.0, 12.0, 11.0, 12.0, 13.0].iter().map(|&c| (c + 1.0, c - 1.0, c)).collect();
    let out = run("ttm_squeeze", &[3.0, 2.0, 1.5], &hlc(&rows));
    assert_eq!(names(&out), ["momentum", "squeeze"]);
    // 1e-9: thirds and twenty-fourths are not binary fractions.
    let want = [N, N, N, N, 1.0 / 24.0, 25.0 / 24.0];
    assert_series("momentum", line(&out, "momentum"), &want, 1e-9);
}

/// Same bars. The Keltner half-width is kc_mult * 2 = 3 around mid; the Bollinger half-width is
/// bb_mult * (population std of the 3 closes). The std is sqrt(2/3) = 0.8165 for (10,11,12) and
/// (11,12,13) and sqrt(2/9) = 0.4714 for (11,12,11) and (12,11,12).
/// - bb_mult 2: widths 1.633 / 0.943, all inside 3: the squeeze is on (1) at every defined bar.
/// - bb_mult 4: widths 3.266 / 1.886: the two wide windows (idx 2 and 5) break out of 3 (0), the
///   two narrow ones (idx 3 and 4) stay inside (1).
///
/// The margins are 0.27 or more, so the 0/1 flags do not depend on the last bits.
#[test]
fn ttm_squeeze_flag_hand_derived() {
    let rows: Vec<(f64, f64, f64)> =
        [10.0, 11.0, 12.0, 11.0, 12.0, 13.0].iter().map(|&c| (c + 1.0, c - 1.0, c)).collect();
    let bars = hlc(&rows);
    let narrow = run("ttm_squeeze", &[3.0, 2.0, 1.5], &bars);
    assert_series("squeeze bb 2", line(&narrow, "squeeze"), &[N, N, 1.0, 1.0, 1.0, 1.0], 0.0);
    let wide = run("ttm_squeeze", &[3.0, 4.0, 1.5], &bars);
    assert_series("squeeze bb 4", line(&wide, "squeeze"), &[N, N, 0.0, 1.0, 1.0, 0.0], 0.0);
}

// ============================== starc ======================================

/// Same bars as the chandelier test. sma_period 2, atr_period 2, mult 1.
/// mid = SMA(close, 2): 10.5, 11.5, 11. ATR(2): 2, 2.5, 2.75.
/// upper = mid + ATR: 12.5, 14, 13.75.   lower = mid - ATR: 8.5, 9, 8.25.
#[test]
fn starc_hand_derived_series() {
    let bars = hlc(&[(11.0, 9.0, 10.0), (12.0, 10.0, 11.0), (13.0, 10.0, 12.0), (12.0, 9.0, 10.0)]);
    let out = run("starc", &[2.0, 2.0, 1.0], &bars);
    assert_eq!(names(&out), ["upper", "mid", "lower"]);
    assert_series("upper", line(&out, "upper"), &[N, 12.5, 14.0, 13.75], 0.0);
    assert_series("mid", line(&out, "mid"), &[N, 10.5, 11.5, 11.0], 0.0);
    assert_series("lower", line(&out, "lower"), &[N, 8.5, 9.0, 8.25], 0.0);
}

/// All three lines wait for BOTH the average and the ATR: sma_period 2, atr_period 3 -> index 2.
#[test]
fn starc_lines_start_together_at_the_slower_window() {
    let bars = hlc(&[(11.0, 9.0, 10.0), (12.0, 10.0, 11.0), (13.0, 10.0, 12.0), (12.0, 9.0, 10.0)]);
    let out = run("starc", &[2.0, 3.0, 1.0], &bars);
    for name in ["upper", "mid", "lower"] {
        assert_eq!(first_number(name, line(&out, name)), 2, "{name}");
    }
}

// ============================== acceleration_bands =========================

/// period 2, factor 4. Bars (high, low, close): (12,4,8) (10,6,8) (20,12,16).
/// w = (high - low)/(high + low) = 8/16, 4/16, 8/32 = 1/2, 1/4, 1/4, so factor*w = 2, 1, 1.
/// high*(1 + factor*w) = 36, 20, 40.   low*(1 - factor*w) = -4, 0, 0.
/// upper = SMA(.., 2): 28, 30.   lower: -2, 0.   mid = SMA(close, 2): 8, 12.
#[test]
fn acceleration_bands_hand_derived_series() {
    let bars = hlc(&[(12.0, 4.0, 8.0), (10.0, 6.0, 8.0), (20.0, 12.0, 16.0)]);
    let out = run("acceleration_bands", &[2.0, 4.0], &bars);
    assert_eq!(names(&out), ["upper", "mid", "lower"]);
    assert_series("upper", line(&out, "upper"), &[N, 28.0, 30.0], 0.0);
    assert_series("mid", line(&out, "mid"), &[N, 8.0, 12.0], 0.0);
    assert_series("lower", line(&out, "lower"), &[N, -2.0, 0.0], 0.0);
}

/// high + low = 0 has no width ratio: it counts as 0 rather than producing a NaN or an infinity.
#[test]
fn acceleration_bands_zero_price_has_no_width() {
    let bars = hlc(&[(0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)]);
    let out = run("acceleration_bands", &[2.0, 4.0], &bars);
    assert_series("upper", line(&out, "upper"), &[N, 0.0, 0.0], 0.0);
    assert_series("lower", line(&out, "lower"), &[N, 0.0, 0.0], 0.0);
}

// ============================== linreg_channel =============================

/// period 3, mult 2, closes 1, 3, 2, 5.
/// - window (1,3,2): slope (2 - 1)/2 = 1/2, intercept 3/2, fitted 1.5, 2, 2.5; residuals
///   -0.5, 1, -0.5 (mean 0, variance 1.5/3 = 1/2). mid = 2.5, s = sqrt(1/2).
/// - window (3,2,5): slope (5 - 3)/2 = 1, intercept 7/3, fitted 7/3, 10/3, 13/3; residuals
///   2/3, -4/3, 2/3 (variance (4 + 16 + 4)/9/3 = 8/9). mid = 13/3, s = sqrt(8/9).
///
/// upper = mid + 2s, lower = mid - 2s.
#[test]
fn linreg_channel_hand_derived_series() {
    let out = run("linreg_channel", &[3.0, 2.0], &closes(&[1.0, 3.0, 2.0, 5.0]));
    assert_eq!(names(&out), ["upper", "mid", "lower"]);
    let (s1, s2) = (0.5f64.sqrt(), (8.0f64 / 9.0).sqrt());
    let mid = [N, N, 2.5, 13.0 / 3.0];
    let upper = [N, N, 2.5 + 2.0 * s1, 13.0 / 3.0 + 2.0 * s2];
    let lower = [N, N, 2.5 - 2.0 * s1, 13.0 / 3.0 - 2.0 * s2];
    // 1e-9: a square root and a third.
    assert_series("mid", line(&out, "mid"), &mid, 1e-9);
    assert_series("upper", line(&out, "upper"), &upper, 1e-9);
    assert_series("lower", line(&out, "lower"), &lower, 1e-9);
}

/// A window of identical closes is its own regression line with no residual: the bands collapse
/// onto it exactly.
#[test]
fn linreg_channel_flat_window_collapses_the_bands() {
    let out = run("linreg_channel", &[3.0, 2.0], &closes(&[7.0, 7.0, 7.0, 7.0]));
    for name in ["upper", "mid", "lower"] {
        assert_series(name, line(&out, name), &[N, N, 7.0, 7.0], 0.0);
    }
}

// ============================== the whole set ==============================

/// A deterministic positive series with real ranges and volume, long enough for the deepest default.
fn long_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let x = i as f64;
            let close = 100.0 + 10.0 * (x * 0.2).sin() + x * 0.05;
            let spread = 1.0 + 0.3 * (x * 1.1).sin().abs();
            Bar::ohlcv(
                i as i64 * 60_000,
                close - 0.2,
                close + spread,
                close - spread,
                close,
                100.0 + (i % 7) as f64,
            )
        })
        .collect()
}

/// Output names and arity, the warm-up of each line at the DEFAULT parameters (the first index at
/// which it is a number, derived from each formula: `period - 1`, `period`, `2 * length - 2`, ...),
/// and no NaN after it.
#[test]
fn defaults_output_names_and_warmup() {
    let bars = long_bars(300);
    // (name, [(line, first index)])
    let cases: &[(&str, &[(&str, usize)])] = &[
        ("kama", &[("kama", 10)]),                                // period
        ("vidya", &[("vidya", 9)]),                               // cmo_period
        ("frama", &[("frama", 15)]),                              // period - 1
        ("evwma", &[("evwma", 19)]),                              // period - 1
        ("median_ma", &[("median_ma", 19)]),                      // period - 1
        ("chandelier", &[("long_stop", 21), ("short_stop", 21)]), // period - 1
        ("hilo_activator", &[("hilo", 2)]),                       // period - 1
        ("atr_trailing_stop", &[("stop", 13)]),                   // period - 1
        ("ttm_squeeze", &[("momentum", 38), ("squeeze", 19)]),    // 2*length - 2, length - 1
        ("starc", &[("upper", 14), ("mid", 14), ("lower", 14)]),  // max(6, 15) - 1
        ("acceleration_bands", &[("upper", 19), ("mid", 19), ("lower", 19)]), // period - 1
        ("linreg_channel", &[("upper", 99), ("mid", 99), ("lower", 99)]), // period - 1
    ];
    for &(name, expected) in cases {
        let out = run(name, &[], &bars);
        let got: Vec<&str> = names(&out);
        let want: Vec<&str> = expected.iter().map(|&(n, _)| n).collect();
        assert_eq!(got, want, "{name}: output lines");
        for &(line_name, first) in expected {
            let values = line(&out, line_name);
            assert_eq!(values.len(), bars.len(), "{name}.{line_name}: one value per bar");
            assert_eq!(
                first_number(&format!("{name}.{line_name}"), values),
                first,
                "{name}.{line_name}: first value"
            );
        }
    }
}

/// The six recurrences carry every bar since the start and say so; the six window kernels do not.
#[test]
fn only_the_infinite_memory_overlays_are_path_dependent() {
    for name in ["kama", "vidya", "frama", "evwma", "hilo_activator", "atr_trailing_stop"] {
        let meta = get(name).unwrap_or_else(|| panic!("{name} is not in the registry"));
        assert!(meta.warmup_path_dependent(), "{name} must be flagged path dependent");
        assert!(meta.lookback_exact(), "{name} declares an exact warm-up");
    }
    for name in
        ["median_ma", "chandelier", "ttm_squeeze", "starc", "acceleration_bands", "linreg_channel"]
    {
        let meta = get(name).unwrap_or_else(|| panic!("{name} is not in the registry"));
        assert!(!meta.warmup_path_dependent(), "{name} is a bounded window kernel");
    }
}
