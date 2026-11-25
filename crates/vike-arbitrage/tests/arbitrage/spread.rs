use vike_arbitrage::spread::{
    FEATURE_NAMES, ReversionConfig, SpreadSignals, log_prices, reversion_dataset,
    reversion_features, signals, single_series_signals, step_pnl,
};
use vike_indicators::series;

fn near(got: f64, want: f64) -> bool {
    (got - want).abs() <= 1e-9 * want.abs().max(1.0)
}

const NAN: f64 = f64::NAN;

fn bits(values: &[f64]) -> Vec<u64> {
    values.iter().map(|v| v.to_bits()).collect()
}

/// `first = [3, 5, 4]`, `second = [1, 2, 2]` at 1.5 give the spread `[1.5, 2, 1]`; over two bars its windows are
/// `[1.5, 2]` (mean 1.75, population deviation 0.25: `z = +1`) and `[2, 1]` (mean 1.5, deviation 0.5: `z = −1`).
#[test]
fn the_spread_and_its_signals_by_hand() {
    let s = signals(&[3.0, 5.0, 4.0], &[1.0, 2.0, 2.0], 1.5, 2).unwrap();
    assert_eq!(s.spread, vec![1.5, 2.0, 1.0]);
    assert!(s.zscore[0].is_nan());
    assert!(near(s.zscore[1], 1.0) && near(s.zscore[2], -1.0), "{:?}", s.zscore);
    assert!(s.deviation[0].is_nan());
    assert!(near(s.deviation[1], 0.25) && near(s.deviation[2], 0.5), "{:?}", s.deviation);
    assert_eq!(s.half_life.len(), 3);
}

/// `signals` adds the pairing and the refusals, not arithmetic: the numbers are the indicators' own, bit for bit.
#[test]
fn the_signals_are_the_indicators_own_numbers() {
    let first: Vec<f64> = (0..60).map(|t| 5.0 + 0.1 * (t % 7) as f64 + 0.01 * t as f64).collect();
    let second: Vec<f64> = (0..60).map(|t| 2.0 + 0.05 * (t % 5) as f64).collect();
    let s = signals(&first, &second, 0.8, 20).unwrap();
    let zscore = series::spread_zscore(&first, &second, 20.0, 0.8).unwrap();
    let half_life = series::half_life(&first, &second, 20.0, 0.8).unwrap();
    assert_eq!(bits(&s.zscore), bits(&zscore));
    assert_eq!(bits(&s.half_life), bits(&half_life));
}

/// A spread that is one series is the pair with a zero second leg.
#[test]
fn a_single_series_is_its_own_spread() {
    let spread = [1.5, 2.0, 1.0, 1.8, 1.2, 1.9];
    let one = single_series_signals(&spread, 3).unwrap();
    assert_eq!(one.spread, spread.to_vec());
    let pair = signals(&spread, &[0.0; 6], 0.0, 3).unwrap();
    assert_eq!(bits(&one.zscore), bits(&pair.zscore));
    assert_eq!(bits(&one.half_life), bits(&pair.half_life));
    assert_eq!(bits(&one.deviation), bits(&pair.deviation));
}

/// The registry's ranges are refusals, not panics or clamps.
#[test]
fn the_indicator_ranges_are_refusals() {
    let a = [1.0, 2.0, 3.0, 4.0];
    assert!(signals(&a, &a[..3], 1.0, 2).is_none(), "different lengths");
    assert!(signals(&a, &a, 1.0, 1).is_none(), "a window below 2");
    assert!(signals(&a, &a, 1.0, 201).is_none(), "a window above 200");
    assert!(signals(&a, &a, 9.0, 2).is_none(), "a hedge ratio above 5");
}

#[test]
fn the_log_of_a_price_is_nan_where_it_has_none() {
    let logs = log_prices(&[1.0, std::f64::consts::E, 0.0, -2.0, f64::NAN]);
    assert_eq!(logs[0], 0.0);
    assert!(near(logs[1], 1.0));
    assert!(logs[2..].iter().all(|v| v.is_nan()));
}

/// The four vectors are one value only when they agree in length: a signal one bar short would shift every row
/// of a model after it.
#[test]
fn signals_are_built_only_from_vectors_of_one_length() {
    let four = || vec![1.0, 2.0, 3.0, 4.0];
    let signals = SpreadSignals::new(four(), four(), four(), four()).unwrap();
    assert_eq!(signals.spread, four());
    assert_eq!(signals.deviation, four());
    assert!(SpreadSignals::new(four(), four(), vec![1.0], four()).is_none());
    assert!(SpreadSignals::new(four(), four(), four(), Vec::new()).is_none());
}

/// A spread of eight bars with chosen z-scores and half-lives (the fields are public so a test can set them). Its
/// deviation is the population standard deviation of the spread over three bars: `sqrt(2/3)` for the windows
/// `[2, 1, 0]`, `[1, 0, -1]` and `[-1, 0, 1]` (and the first), `sqrt(2/9)` for `[1, 2, 1]` and `[0, -1, 0]`.
fn eight_bars() -> SpreadSignals {
    let (wide, narrow) = ((2.0f64 / 3.0).sqrt(), (2.0f64 / 9.0).sqrt());
    SpreadSignals {
        spread: vec![0.0, 1.0, 2.0, 1.0, 0.0, -1.0, 0.0, 1.0],
        zscore: vec![NAN, NAN, 1.0, 0.5, -0.5, -1.0, -0.5, 0.5],
        half_life: vec![NAN, NAN, 4.0, NAN, 2.0, 5.0, 10.0, NAN],
        deviation: vec![NAN, NAN, wide, narrow, wide, wide, narrow, wide],
    }
}

const CONFIG: ReversionConfig = ReversionConfig { momentum: 2, horizon: 2 };

/// By hand with `momentum = 2`: a bar needs its z-score and the ones one and two bars before it, so bars 2 and
/// 3 are out (a `NaN` is behind them) and bars 4 to 7 give a row `[z, z − z₋₁, z − z₋₂, |z|, 1 / half-life]`,
/// the speed `0` where there is no finite half-life (bar 7).
#[test]
fn feature_rows_by_hand() {
    let (bars, rows) = reversion_features(&eight_bars(), CONFIG, &[]).unwrap();
    assert_eq!(bars, vec![4, 5, 6, 7]);
    let want = [
        [-0.5, -1.0, -1.5, 0.5, 0.5],
        [-1.0, -0.5, -1.5, 1.0, 0.2],
        [-0.5, 0.5, 0.0, 0.5, 0.1],
        [0.5, 1.0, 1.5, 0.5, 0.0],
    ];
    assert_eq!(rows.len(), 20);
    for (row, want) in rows.as_chunks::<5>().0.iter().zip(want) {
        for (got, want) in row.iter().zip(want) {
            assert!(near(*got, want), "{row:?}");
        }
    }
    assert_eq!(FEATURE_NAMES.len(), 5);
}

/// A label reads `horizon = 2` bars ahead, so only bars 4 and 5 have one. Over three bars, bar 4's window
/// is `[2, 1, 0]` and bar 5's `[1, 0, −1]`, both with the population deviation `sqrt(2/3)`; the spread then
/// moves `0 − 0 = 0` and `1 − (−1) = 2`, so the labels are `0` and `2 / sqrt(2/3)`.
#[test]
fn labels_by_hand() {
    let dataset = reversion_dataset(&eight_bars(), CONFIG, &[]).unwrap();
    assert_eq!(dataset.bars, vec![4, 5]);
    assert_eq!(dataset.columns, 5);
    assert_eq!(dataset.features.len(), 10);
    assert_eq!(dataset.labels[0], 0.0);
    // The label is an `f32`: right to about seven digits.
    let want = 2.0 / (2.0f64 / 3.0).sqrt();
    assert!((f64::from(dataset.labels[1]) - want).abs() < 1e-6, "{:?}", dataset.labels);
}

/// An extra column rides along as another feature, and a row where it is missing is dropped.
#[test]
fn an_extra_column_is_a_feature_and_a_gap_drops_the_row() {
    let funding = [NAN, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, NAN];
    let (bars, rows) = reversion_features(&eight_bars(), CONFIG, &[&funding]).unwrap();
    assert_eq!(bars, vec![4, 5, 6], "bar 7's extra value is missing");
    assert_eq!(rows.len(), 18);
    assert_eq!(rows[5], 4.0);
    let dataset = reversion_dataset(&eight_bars(), CONFIG, &[&funding]).unwrap();
    assert_eq!(dataset.columns, 6);
    assert_eq!(dataset.bars, vec![4, 5]);
}

#[test]
fn no_dataset_for_a_bad_request() {
    let short = [1.0, 2.0];
    assert!(
        reversion_features(&eight_bars(), CONFIG, &[&short]).is_none(),
        "a column of another length"
    );
    let zero_momentum = ReversionConfig { momentum: 0, ..CONFIG };
    assert!(reversion_features(&eight_bars(), zero_momentum, &[]).is_none());
    let zero_horizon = ReversionConfig { horizon: 0, ..CONFIG };
    assert!(reversion_dataset(&eight_bars(), zero_horizon, &[]).is_none());
}

/// Horizon 2 on the spread `[0, 1, 3, 2, 4, 3, 5]`, gross (the costs are a backtest's). Bar 0, long: `3 − 0 = 3`.
/// Bar 1 starts inside that hold: skipped. Bar 2, short: `−(4 − 3) = −1`. Bar 4, long: `5 − 4 = 1`. Bar 5 starts
/// inside the hold and bar 6 has no bar two ahead.
#[test]
fn step_profits_by_hand() {
    let spread = [0.0, 1.0, 3.0, 2.0, 4.0, 3.0, 5.0];
    let decisions = [(0, 1.0), (1, -1.0), (2, -1.0), (4, 1.0), (5, 1.0), (6, 1.0)];
    let profits = step_pnl(&spread, &decisions, 2);
    assert_eq!(profits.len(), 3);
    for ((bar, got), (want_bar, want)) in profits.iter().zip([(0, 3.0), (2, -1.0), (4, 1.0)]) {
        assert_eq!(*bar, want_bar, "{profits:?}");
        assert!(near(*got, want), "{profits:?}");
    }
    assert!(step_pnl(&spread, &[], 2).is_empty());
}

/// A hold that ends before the next decision leaves the book flat, and the next decision is kept: horizon 2, the
/// same spread, bar 0 long ends at bar 2, nothing is decided there, bar 3 goes long again. Bar 0 earns
/// `3 − 0 = 3`, bar 3 `3 − 2 = 1`; what the exit at bar 2 and the re-entry cost is the backtest's to charge.
#[test]
fn a_decision_after_a_flat_gap_is_kept() {
    let spread = [0.0, 1.0, 3.0, 2.0, 4.0, 3.0, 5.0];
    let profits = step_pnl(&spread, &[(0, 1.0), (3, 1.0)], 2);
    assert_eq!(profits.len(), 2, "{profits:?}");
    assert_eq!((profits[0].0, profits[1].0), (0, 3));
    assert!(near(profits[0].1, 3.0), "{profits:?}");
    assert!(near(profits[1].1, 1.0), "{profits:?}");
}
