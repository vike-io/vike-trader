use super::*;
use vike_fills::market_impact::window_stats;
use vike_marketdata::Bar;

fn bar(ts: i64, close: f64, volume: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: close,
        high: close,
        low: close,
        close,
        volume,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

// The model's own tests (`ImpactModelAlmgrenChriss`, the lane split of its terms, the calibration that
// inverts it) moved with it to vike-analytics on 2026-11-17 and on to
// `crates/vike-fills/src/market_impact_tests.rs` on 2026-11-18.

// --- the tick-lane window -------------------------------------------------------------------

/// The two windows are ONE measurement: an `ImpactModelTickWindow` fed the same (price, size) pairs a bar
/// series carries measures bit-identically. This is what stops the tick lane from growing a
/// second, subtly different definition of `sigma`.
#[test]
fn the_tick_window_measures_exactly_what_the_bar_window_measures() {
    let bars: Vec<Bar> = vec![
        bar(0, 100.0, 12.0),
        bar(1, 101.5, 9.0),
        bar(2, 100.75, 31.0),
        bar(3, 103.0, 4.0),
        bar(4, 102.25, 17.0),
    ];
    let mut w = ImpactModelTickWindow::with_capacity(bars.len());
    for b in &bars {
        w.push(b.close, b.volume);
    }
    let from_bars = window_stats(&bars, bars.len()).expect("measurable");
    let from_ticks = w.stats().expect("measurable");
    assert_eq!(from_ticks.sigma.to_bits(), from_bars.sigma.to_bits());
    assert_eq!(from_ticks.average_volume.to_bits(), from_bars.average_volume.to_bits());
}

/// The window is BOUNDED: it holds `cap` prints and evicts the oldest, so a long replay costs
/// a constant. Proven by measurement, not by reading `len` — the retained window must be the
/// one the last `cap` prints describe.
#[test]
fn the_tick_window_is_bounded_and_keeps_the_newest_prints() {
    let mut w = ImpactModelTickWindow::with_capacity(3);
    for (px, sz) in [(10.0, 1.0), (11.0, 1.0), (12.0, 5.0), (13.0, 9.0), (14.0, 10.0)] {
        w.push(px, sz);
    }
    assert_eq!(w.len(), 3, "the window grew past its capacity");
    let tail: Vec<Bar> = vec![bar(0, 12.0, 5.0), bar(1, 13.0, 9.0), bar(2, 14.0, 10.0)];
    let want = window_stats(&tail, 3).expect("measurable");
    let got = w.stats().expect("measurable");
    assert_eq!(got.average_volume.to_bits(), want.average_volume.to_bits(), "not the newest three");
    assert_eq!(got.sigma.to_bits(), want.sigma.to_bits());
}

/// Warmup and degenerate tapes measure NOTHING rather than guessing — the tick twin of
/// `vike-analytics`' `window_stats_honors_the_window_and_needs_three_bars`.
#[test]
fn the_tick_window_refuses_to_measure_a_window_it_cannot_support() {
    let mut w = ImpactModelTickWindow::with_capacity(8);
    assert!(w.is_empty() && w.stats().is_none(), "an empty window measures nothing");
    w.push(10.0, 1.0);
    w.push(11.0, 1.0);
    assert!(w.stats().is_none(), "two prints are one return, not enough");
    w.push(12.0, 1.0);
    assert!(w.stats().is_some(), "three prints are measurable");
    // a zero-size tape cannot price participation at all
    let mut novol = ImpactModelTickWindow::with_capacity(8);
    for px in [10.0, 11.0, 12.0] {
        novol.push(px, 0.0);
    }
    assert!(novol.stats().is_none(), "zero traded size cannot price impact");
    // ...nor can a non-positive print
    let mut bad = ImpactModelTickWindow::with_capacity(8);
    for px in [10.0, 0.0, 12.0] {
        bad.push(px, 1.0);
    }
    assert!(bad.stats().is_none(), "a zero price has no meaningful return");
    // a zero-capacity window records nothing at all rather than growing without bound
    let mut nocap = ImpactModelTickWindow::with_capacity(0);
    for px in [10.0, 11.0, 12.0] {
        nocap.push(px, 1.0);
    }
    assert!(nocap.is_empty() && nocap.stats().is_none());
}
