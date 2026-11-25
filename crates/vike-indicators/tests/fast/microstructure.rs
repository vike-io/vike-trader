//! The market-microstructure measures (`src/indicators/microstructure.rs`): Kyle's lambda, Amihud's
//! illiquidity and Roll's spread, each against small series worked by hand from its published
//! formula, and against the definition folded from scratch for every window (two passes, each
//! series shifted by its first value) at the three price scales, a `NaN` close travelling through
//! included. The cores carry running sums, so the last bits differ from the fold: the tolerance is
//! 1e-9 of the largest value of the series (Roll's spread is compared squared, `-4 cov`, because the
//! square root magnifies a rounding of a covariance near zero). Each row also streams what it
//! computes in batch, survives `reset`, and its `series::*` call gives the same bits.

use vike_indicators::indicators::{AmihudIlliquidity, KyleLambda, RollSpread};
use vike_indicators::series;
use vike_marketdata::Bar;

use crate::fast::{SCALES, assert_close, closes, same};
use crate::options::{assert_lines_same, checked_stream, registry};

const NAN: f64 = f64::NAN;

fn bars(close: &[f64], volume: &[f64]) -> Vec<Bar> {
    close
        .iter()
        .zip(volume)
        .enumerate()
        .map(|(i, (&c, &v))| Bar::ohlcv(i as i64 * 60_000, c, c, c, c, v))
        .collect()
}

/// A walk with an unchanged close every 11th bar (a bar that carries no direction).
fn flat_walk(level: f64, step: f64, n: usize) -> Vec<f64> {
    let mut c = closes(level, step, n);
    for i in (11..n).step_by(11) {
        c[i] = c[i - 1];
    }
    c
}

/// A volume that varies, and is zero every 9th bar (a bar that traded nothing).
fn volumes(n: usize) -> Vec<f64> {
    (0..n).map(|i| if i % 9 == 4 { 0.0 } else { 1.0 + ((i * 37) % 11) as f64 * 0.5 }).collect()
}

/// `got` against `want`: `NaN` exactly where the reference has one, and every other value within
/// `tolerance` of the largest magnitude the reference reaches.
fn close_to_scale(name: &str, got: &[f64], want: &[f64], tolerance: f64) {
    assert_eq!(got.len(), want.len(), "{name}: length");
    let scale = want.iter().filter(|w| w.is_finite()).fold(0.0_f64, |a, w| a.max(w.abs()));
    assert!(scale > 0.0, "{name}: the reference never left zero");
    for (i, (&g, &w)) in got.iter().zip(want).enumerate() {
        if w.is_nan() {
            assert!(g.is_nan(), "{name}[{i}]: want NaN, got {g}");
            continue;
        }
        let err = (g - w).abs() / scale;
        assert!(err <= tolerance, "{name}[{i}]: got {g}, want {w}, error {err:e} of {scale:e}");
    }
}

/// Close-to-close changes, `NaN` on the first bar.
fn changes(c: &[f64]) -> Vec<f64> {
    (0..c.len()).map(|i| if i == 0 { NAN } else { c[i] - c[i - 1] }).collect()
}

/// `sum (x - mean x)(y - mean y)`, in two passes over values shifted by each side's first.
fn co_deviation(x: &[f64], y: &[f64]) -> f64 {
    let (x0, y0, n) = (x[0], y[0], x.len() as f64);
    let mx = x.iter().map(|a| a - x0).sum::<f64>() / n;
    let my = y.iter().map(|b| b - y0).sum::<f64>() / n;
    x.iter().zip(y).map(|(a, b)| (a - x0 - mx) * (b - y0 - my)).sum()
}

/// Kyle's lambda by its definition: the slope of the change on the signed volume (`+volume` up,
/// `-volume` down, `0` unchanged) over the last `n` changes, `NaN` for a constant regressor.
fn kyle_ref(c: &[f64], v: &[f64], n: usize) -> Vec<f64> {
    let dp = changes(c);
    let signed: Vec<f64> = (0..c.len())
        .map(|i| match i {
            0 => 0.0,
            _ if c[i] > c[i - 1] => v[i],
            _ if c[i] < c[i - 1] => -v[i],
            _ => 0.0,
        })
        .collect();
    (0..c.len())
        .map(|i| {
            if i < n {
                return NAN;
            }
            let (x, y) = (&signed[i + 1 - n..=i], &dp[i + 1 - n..=i]);
            if x.iter().all(|a| *a == x[0]) { NAN } else { co_deviation(x, y) / co_deviation(x, x) }
        })
        .collect()
}

/// Amihud's illiquidity by its definition: the mean of `|return| / (close * volume)` over the bars
/// of the last `n` that traded, `NaN` when none did.
fn amihud_ref(c: &[f64], v: &[f64], n: usize) -> Vec<f64> {
    (0..c.len())
        .map(|i| {
            if i < n {
                return NAN;
            }
            let (mut total, mut count) = (0.0_f64, 0usize);
            for j in i + 1 - n..=i {
                if v[j] != 0.0 {
                    total += ((c[j] - c[j - 1]) / c[j - 1]).abs() / (c[j] * v[j]);
                    count += 1;
                }
            }
            if count == 0 { NAN } else { total / count as f64 }
        })
        .collect()
}

/// Roll's spread by its definition, squared (`-4 cov`, or `0`): the sample serial covariance of the
/// changes over the last `n` pairs.
fn roll_squared_ref(c: &[f64], n: usize) -> Vec<f64> {
    let d = changes(c);
    (0..c.len())
        .map(|i| {
            if i < n + 1 {
                return NAN;
            }
            let cov = co_deviation(&d[i + 1 - n..=i], &d[i - n..i]) / (n as f64 - 1.0);
            if cov.is_nan() { NAN } else { 4.0 * (-cov).max(0.0) }
        })
        .collect()
}

// ---------------------------------------------------------------- worked by hand

#[test]
fn kyle_lambda_by_hand() {
    // Changes +1, -1, 0, +3; signed volumes +10, -20, 0 (unchanged), +5.
    let c = [100.0, 101.0, 100.0, 100.0, 103.0];
    let v = [1.0, 10.0, 20.0, 30.0, 5.0];
    // Two points: the slope of the line through them. (-1 - 1) / (-20 - 10), (0 + 1) / (0 + 20),
    // (3 - 0) / (5 - 0).
    let two = series::kyle_lambda(&c, &v, 2.0).unwrap();
    assert_close(
        "kyle_lambda(2)",
        &two,
        &[NAN, NAN, 1.0 / 15.0, 1.0 / 20.0, 3.0 / 5.0],
        1e-15,
        1e-300,
    );
    // Three: x = 10, -20, 0 and y = 1, -1, 0 give Sxy = 30, Sxx = 500 - 3 (10/3)^2 = 1400/3, so 9/140;
    // x = -20, 0, 5 and y = -1, 0, 3 give Sxy = 35 + 10 = 45, Sxx = 425 - 75 = 350, so 9/70.
    let three = series::kyle_lambda(&c, &v, 3.0).unwrap();
    assert_close(
        "kyle_lambda(3)",
        &three,
        &[NAN, NAN, NAN, 9.0 / 140.0, 9.0 / 70.0],
        1e-15,
        1e-300,
    );
    // A close that rises every bar on the same volume has one regressor value: no slope.
    let up = series::kyle_lambda(&[1.0, 2.0, 3.0, 4.0], &[5.0; 4], 2.0).unwrap();
    assert!(up.iter().all(|x| x.is_nan()), "a constant signed volume has no slope: {up:?}");
}

#[test]
fn amihud_illiquidity_by_hand() {
    // Returns 0.1, -0.1 (no volume: left out), 0, (108.9 - 99) / 99; traded values 220, -, 495, 108.9.
    let c = [100.0, 110.0, 99.0, 99.0, 108.9];
    let v = [1.0, 2.0, 0.0, 5.0, 1.0];
    let last = ((108.9 - 99.0) / 99.0) / 108.9;
    let one = series::amihud_illiquidity(&c, &v, 1.0).unwrap();
    assert_close("amihud(1)", &one, &[NAN, 0.1 / 220.0, NAN, 0.0, last], 1e-15, 1e-300);
    let two = series::amihud_illiquidity(&c, &v, 2.0).unwrap();
    assert_close("amihud(2)", &two, &[NAN, NAN, 0.1 / 220.0, 0.0, last / 2.0], 1e-15, 1e-300);
}

#[test]
fn roll_spread_by_hand() {
    // Changes -1, +1, -1, +1 against the change before: four pairs, both means 0, sum of products
    // -4, sample covariance -4/3, spread 2 sqrt(4/3).
    let bounce = [10.0, 11.0, 10.0, 11.0, 10.0, 11.0];
    let got = series::roll_spread(&bounce, 4.0).unwrap();
    let want = [NAN, NAN, NAN, NAN, NAN, 2.0 * (4.0_f64 / 3.0).sqrt()];
    assert_close("roll_spread bounce", &got, &want, 1e-15, 1e-300);
    // Changes 1, 2, 3, 4: the pairs move together, a covariance Roll's model cannot produce, so 0.
    let trend = series::roll_spread(&[1.0, 2.0, 4.0, 7.0, 11.0], 3.0).unwrap();
    assert!(same(&trend, &[NAN, NAN, NAN, NAN, 0.0]), "a trend has no spread: {trend:?}");
}

// ---------------------------------------------------------------- the definition, every window

#[test]
fn each_matches_its_definition_folded_from_scratch() {
    let n = 3000;
    let v = volumes(n);
    for (level, step) in SCALES {
        for poisoned in [false, true] {
            let mut c = flat_walk(level, step, n);
            if poisoned {
                c[1200] = NAN;
            }
            let at = format!("level {level} nan {poisoned}");
            for window in [2usize, 3, 5, 20, 64, 200] {
                let w = window as f64;
                close_to_scale(
                    &format!("kyle_lambda({window}) {at}"),
                    &series::kyle_lambda(&c, &v, w).unwrap(),
                    &kyle_ref(&c, &v, window),
                    1e-9,
                );
                close_to_scale(
                    &format!("amihud_illiquidity({window}) {at}"),
                    &series::amihud_illiquidity(&c, &v, w).unwrap(),
                    &amihud_ref(&c, &v, window),
                    1e-9,
                );
                let squared: Vec<f64> =
                    series::roll_spread(&c, w).unwrap().iter().map(|s| s * s).collect();
                close_to_scale(
                    &format!("roll_spread({window}) {at}"),
                    &squared,
                    &roll_squared_ref(&c, window),
                    1e-9,
                );
            }
        }
    }
}

// ---------------------------------------------------------------- one arithmetic, four doors

#[test]
fn stream_batch_series_and_registry_agree_bit_for_bit() {
    let n = 1500;
    let v = volumes(n);
    let c = flat_walk(100.0, 2.0, n);
    let b = bars(&c, &v);
    for p in [2.0, 20.0, 400.0] {
        let kyle = checked_stream("kyle_lambda", Box::new(KyleLambda::new(p).unwrap()), &b);
        assert_lines_same("kyle_lambda: registry", &registry("kyle_lambda", &[p], &b), &kyle);
        assert!(same(&series::kyle_lambda(&c, &v, p).unwrap(), &kyle[0]), "kyle_lambda series");

        let amihud =
            checked_stream("amihud_illiquidity", Box::new(AmihudIlliquidity::new(p).unwrap()), &b);
        assert_lines_same(
            "amihud_illiquidity: registry",
            &registry("amihud_illiquidity", &[p], &b),
            &amihud,
        );
        assert!(
            same(&series::amihud_illiquidity(&c, &v, p).unwrap(), &amihud[0]),
            "amihud_illiquidity series"
        );

        let roll = checked_stream("roll_spread", Box::new(RollSpread::new(p).unwrap()), &b);
        assert_lines_same("roll_spread: registry", &registry("roll_spread", &[p], &b), &roll);
        assert!(same(&series::roll_spread(&c, p).unwrap(), &roll[0]), "roll_spread series");
    }
}

#[test]
fn a_parameter_outside_the_row_is_refused() {
    assert!(KyleLambda::new(1.0).is_err(), "a regression needs two points");
    assert!(RollSpread::new(1.0).is_err(), "a sample covariance needs two pairs");
    assert!(AmihudIlliquidity::new(0.0).is_err(), "an empty window");
    assert!(KyleLambda::new(f64::NAN).is_err(), "NaN");
    assert!(RollSpread::new(1001.0).is_err(), "above the row's 1000");
}
