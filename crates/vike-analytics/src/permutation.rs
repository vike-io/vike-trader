//! Bar-history permutation for Monte Carlo permutation tests (Masters, *Permutation and
//! Randomization Tests for Trading System Development*, 2020).
//!
//! A permutation test asks whether a strategy would have scored as well on a history with the SAME
//! price changes in a RANDOM order. Such a history keeps the drift, the volatility and the fat tails
//! of the real one and loses exactly the serial structure a strategy claims to exploit.
//! [`permute_bars`] builds one; the test that runs a backtest on many of them is the harness's
//! (`vike_backtest::permutation`).
//!
//! # The construction (Masters' bar permutation)
//!
//! In log prices every bar after the first is two pieces: its GAP (the open against the previous
//! close) and its SHAPE (the high, the low and the close against its own open). Two independent
//! permutations of bars `1..n` shuffle the gaps and the shapes, a shape's three numbers and its
//! volume moving together, and the bars are rebuilt from bar 0, which stays as it is. So:
//!
//! - the first bar and every timestamp stay where they were;
//! - the gaps and the shapes are the original's, only reordered, so the return distribution is;
//! - the last close is the original's up to rounding (a sum of logs does not depend on order);
//! - every rebuilt bar is a bar: `low <= open, close <= high`, because a shape moves whole.
//!
//! A bar's `funding` (the stored rate at its instant) and `symbol` stay at their timestamp: they are
//! facts of the clock, not of the price path. Its `bid`/`ask` move with its shape, scaled to its new
//! open, so the spread stays the same fraction of the price.
//!
//! # Several symbols
//!
//! ONE pair of permutations is drawn and applied to every series, so the cross-section survives (bar
//! `i` of every symbol comes from the same original bar). The series must therefore be aligned: the
//! same length and the same timestamp at every index. A misaligned set is refused, as is a price
//! that is not positive and finite (the construction is in logs).
//!
//! The logs are the `libm` crate's (`lib.rs`'s "Cross-platform determinism") and the shuffle is a
//! seeded `StdRng`, so one seed gives the same history run to run.

use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use vike_marketdata::Bar;

/// One permuted history of `series`, all of them shuffled by the same draw from `seed`.
///
/// Returns the series in the caller's order. A series of fewer than three bars has nothing to
/// reorder (bar 0 stays and one bar has one place) and comes back as it is.
///
/// Errors, in words: series of different lengths, a timestamp that differs between two series at
/// the same index, a price that is not positive and finite.
pub fn permute_bars(series: &[&[Bar]], seed: u64) -> Result<Vec<Vec<Bar>>, String> {
    let Some(first) = series.first() else { return Ok(Vec::new()) };
    let n = first.len();
    for (m, bars) in series.iter().enumerate() {
        if bars.len() != n {
            return Err(format!(
                "series {m} holds {} bars and series 0 holds {n}: a joint permutation needs \
                 aligned series (one length, one timestamp per index)",
                bars.len()
            ));
        }
        for (i, (bar, reference)) in bars.iter().zip(first.iter()).enumerate() {
            if bar.timestamp != reference.timestamp {
                return Err(format!(
                    "series {m} bar {i} is at {} and series 0 bar {i} at {}: a joint permutation \
                     needs aligned series (one length, one timestamp per index)",
                    bar.timestamp, reference.timestamp
                ));
            }
            for price in [bar.open, bar.high, bar.low, bar.close] {
                if !(price.is_finite() && price > 0.0) {
                    return Err(format!(
                        "series {m} bar {i} has a price of {price}: the permutation works in log \
                         prices, so every price must be positive and finite"
                    ));
                }
            }
        }
    }
    if n < 3 {
        return Ok(series.iter().map(|bars| bars.to_vec()).collect());
    }

    let mut rng = StdRng::seed_from_u64(seed);
    let mut gap_order: Vec<usize> = (1..n).collect();
    gap_order.shuffle(&mut rng);
    let mut shape_order: Vec<usize> = (1..n).collect();
    shape_order.shuffle(&mut rng);
    Ok(series.iter().map(|bars| rebuild(bars, &gap_order, &shape_order)).collect())
}

/// `bars` rebuilt with bar `i`'s gap taken from bar `gap_order[i - 1]` and its shape from bar
/// `shape_order[i - 1]`.
fn rebuild(bars: &[Bar], gap_order: &[usize], shape_order: &[usize]) -> Vec<Bar> {
    let mut out = Vec::with_capacity(bars.len());
    out.push(bars[0].clone());
    let mut log_close = libm::log(bars[0].close);
    for (i, (&gap_from, &shape_from)) in gap_order.iter().zip(shape_order).enumerate() {
        let place = &bars[i + 1];
        let shape = &bars[shape_from];
        let log_open =
            log_close + (libm::log(bars[gap_from].open) - libm::log(bars[gap_from - 1].close));
        let log_shape_open = libm::log(shape.open);
        let open = libm::exp(log_open);
        log_close = log_open + (libm::log(shape.close) - log_shape_open);
        out.push(Bar {
            timestamp: place.timestamp,
            open,
            high: libm::exp(log_open + (libm::log(shape.high) - log_shape_open)),
            low: libm::exp(log_open + (libm::log(shape.low) - log_shape_open)),
            close: libm::exp(log_close),
            volume: shape.volume,
            funding: place.funding,
            bid: shape.bid.map(|bid| bid * open / shape.open),
            ask: shape.ask.map(|ask| ask * open / shape.open),
            symbol: place.symbol,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngExt;

    /// `n` hourly bars of a seeded random walk whose open, high, low and close all differ, each
    /// bar carrying its index as its funding rate and its index plus one as its volume.
    fn walk(n: usize, seed: u64) -> Vec<Bar> {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut close = 100.0_f64;
        (0..n)
            .map(|i| {
                let open = close * (1.0 + rng.random_range(-0.002..0.002));
                close = open * (1.0 + rng.random_range(-0.01..0.01));
                let high = open.max(close) * (1.0 + rng.random_range(0.0..0.005));
                let low = open.min(close) * (1.0 - rng.random_range(0.0..0.005));
                let mut bar =
                    Bar::ohlcv(i as i64 * 3_600_000, open, high, low, close, i as f64 + 1.0);
                bar.funding = Some(i as f64);
                bar
            })
            .collect()
    }

    /// Every bar's gap and shape after the first, in logs, as `[gap, high, low, close]` rows.
    fn pieces(bars: &[Bar]) -> Vec<[f64; 4]> {
        bars.windows(2)
            .map(|w| {
                let open = libm::log(w[1].open);
                [
                    open - libm::log(w[0].close),
                    libm::log(w[1].high) - open,
                    libm::log(w[1].low) - open,
                    libm::log(w[1].close) - open,
                ]
            })
            .collect()
    }

    /// One column of [`pieces`], sorted: the multiset the permutation must keep.
    fn sorted_column(rows: &[[f64; 4]], column: usize) -> Vec<f64> {
        let mut values: Vec<f64> = rows.iter().map(|row| row[column]).collect();
        values.sort_by(f64::total_cmp);
        values
    }

    fn close_to(a: f64, b: f64, relative: f64) -> bool {
        (a - b).abs() <= relative * a.abs().max(b.abs())
    }

    #[test]
    fn the_first_bar_the_clock_and_the_last_close_stay() {
        let bars = walk(200, 1);
        let out = permute_bars(&[&bars], 7).unwrap().pop().unwrap();
        assert_eq!(out.len(), bars.len());
        assert_eq!(out[0], bars[0], "bar 0 is kept as it is");
        for (permuted, original) in out.iter().zip(&bars) {
            assert_eq!(permuted.timestamp, original.timestamp);
            assert_eq!(permuted.funding, original.funding, "funding stays at its instant");
        }
        let (last, original_last) = (out[199].close, bars[199].close);
        assert!(close_to(last, original_last, 1e-12), "{last} vs {original_last}");
        assert_ne!(out, bars, "two hundred bars must come back reordered");
    }

    #[test]
    fn the_gaps_and_the_shapes_are_the_originals_reordered() {
        let bars = walk(200, 2);
        let out = permute_bars(&[&bars], 11).unwrap().pop().unwrap();
        let (before, after) = (pieces(&bars), pieces(&out));
        for column in 0..4 {
            let (want, got) = (sorted_column(&before, column), sorted_column(&after, column));
            for (w, g) in want.iter().zip(&got) {
                // The rebuild goes through exp and back through log: equal up to rounding.
                assert!((w - g).abs() <= 1e-12, "column {column}: {w} vs {g}");
            }
        }
        // A shape moves WHOLE, volume with it: each output bar's volume names the original bar its
        // shape came from, and that bar's shape is the one the output bar carries.
        for (i, row) in after.iter().enumerate() {
            let from = out[i + 1].volume as usize - 1;
            for (got, want) in row[1..].iter().zip(&before[from - 1][1..]) {
                assert!((got - want).abs() <= 1e-12, "bar {}", i + 1);
            }
        }
    }

    #[test]
    fn every_rebuilt_bar_is_a_bar() {
        let bars = walk(300, 3);
        for seed in 0..5 {
            for bar in permute_bars(&[&bars], seed).unwrap().pop().unwrap() {
                assert!(bar.low <= bar.open.min(bar.close), "{bar:?}");
                assert!(bar.high >= bar.open.max(bar.close), "{bar:?}");
            }
        }
    }

    #[test]
    fn one_seed_gives_one_history_and_another_seed_another() {
        let bars = walk(100, 4);
        let a = permute_bars(&[&bars], 21).unwrap();
        assert_eq!(a, permute_bars(&[&bars], 21).unwrap());
        assert_ne!(a, permute_bars(&[&bars], 22).unwrap());
    }

    #[test]
    fn several_symbols_are_permuted_by_one_draw() {
        // B is A at twice the price, so it has A's gaps and shapes; one draw keeps it so.
        let a = walk(150, 5);
        let b: Vec<Bar> = a
            .iter()
            .map(|bar| {
                Bar::ohlcv(
                    bar.timestamp,
                    bar.open * 2.0,
                    bar.high * 2.0,
                    bar.low * 2.0,
                    bar.close * 2.0,
                    1.0,
                )
            })
            .collect();
        let out = permute_bars(&[&a, &b], 9).unwrap();
        for (x, y) in out[0].iter().zip(&out[1]) {
            for (p, q) in [(x.open, y.open), (x.high, y.high), (x.low, y.low), (x.close, y.close)] {
                assert!(close_to(2.0 * p, q, 1e-12), "{p} vs {q}");
            }
        }
    }

    #[test]
    fn a_misaligned_set_or_a_price_that_has_no_log_is_refused() {
        let a = walk(50, 6);
        let shorter = &a[..40];
        let why = permute_bars(&[&a, shorter], 1).unwrap_err();
        assert!(why.contains("aligned"), "{why}");

        let mut shifted = a.clone();
        shifted[10].timestamp += 1;
        let why = permute_bars(&[&a, &shifted], 1).unwrap_err();
        assert!(why.contains("bar 10"), "{why}");

        let mut broken = a.clone();
        broken[5].low = 0.0;
        let why = permute_bars(&[&broken], 1).unwrap_err();
        assert!(why.contains("positive and finite"), "{why}");
    }

    #[test]
    fn nothing_to_reorder_comes_back_as_it_is() {
        assert!(permute_bars(&[], 1).unwrap().is_empty());
        let two = walk(2, 7);
        assert_eq!(permute_bars(&[&two], 1).unwrap(), vec![two]);
    }
}
