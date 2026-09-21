//! The `[walkforward]` duration window form, end to end through `run_walkforward`.
//!
//! `harness::windows`'s own unit tests already gate the arithmetic — every suffix, the purge gap,
//! the embargo filter — against a `Vec<Bar>` with no store in sight. This file deliberately does
//! not restate those. What it adds is the part those cannot reach: the DRIVER, from OUTSIDE the
//! crate, over a real `HistStore`, so a window form that resolves correctly in isolation and is
//! never actually handed to the walk fails here while every unit test still passes.
//!
//! ⚠ Rides `hist-replay` alone, but `scripts/ci_feature_suite.sh`'s `backtest-hist-replay` arm runs
//! that build through CLIPPY ONLY — so what EXECUTES this file is that same arm's second lane,
//! `cargo test -p vike-backtest --features datafusion-store`, which implies `hist-replay`. Both
//! lanes see it; only one runs it.
//!
//! Over `vike_data::MemHistStore` rather than a temp-dir `DataFusionHist`, so nothing here needs a
//! Parquet fixture — the same seam
//! `crates/vike-backtest/tests/walkforward_optimize.rs` uses, and its bar half is real storage (it
//! honours the requested range and returns rows ts-ascending).
//!
//! ⚠ Its own top-level binary rather than a member of the `parity`/`laws`/`fills` groups, and the
//! `#![cfg]` below is why: `crates/vike-backtest/CLAUDE.md`'s *Test-binary consolidation* section
//! forbids a feature gate inside a group.
#![cfg(feature = "hist-replay")]

use std::sync::Arc;

use vike_backtest::harness::{BacktestProfile, run_walkforward};
use vike_data::{HistStore, MemHistStore};
use vike_model::Bar;

const VENUE: &str = "test";
const SYMBOL: &str = "TESTUSDT";
const INTERVAL: &str = "1h";
/// Bars in the fixture slice, and the number every window arithmetic below is stated against.
const BARS: usize = 1_200;
const STEP_MS: i64 = 3_600_000;

/// The profile every test here starts from: ONE hourly bar series, `buy_hold` at `size = 1.0`, and
/// a range wide enough to hold every bar — the fixture is anchored at a REAL civil date
/// (2026-01-01) so a calendar span has something to be anchored at, which puts its timestamps
/// around 1.77e12 ms.
const BASE: &str = r#"
name = "walkforward-windows"

[data]
venue = "test"
symbols = ["TESTUSDT"]
kind = "bar"
interval = "1h"
from = "0"
to = "9999999999999"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "TESTUSDT"
"#;

fn profile(extra: &str) -> BacktestProfile {
    BacktestProfile::from_toml_str(&format!("{BASE}{extra}")).expect("fixture profile parses")
}

/// A store holding [`BARS`] strictly-rising flat-OHLC bars for the fixture's one series, stepped
/// hourly from 2026-01-01T00:00:00Z.
///
/// Flat OHLC keeps each window's entry bar at exactly `cash`, which is the contract
/// `walk_forward_over_windows`'s stitch `debug_assert`s on every window it folds; strictly rising
/// closes are what make `buy_hold` return something other than zero, so "every window reports a
/// return" is a claim with content.
fn store() -> Arc<dyn HistStore + Send + Sync> {
    let t0 = vike_model::time::days_from_civil(2026, 1, 1) * 86_400_000;
    let store = MemHistStore::new();
    let bars: Vec<Bar> = (0..BARS)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1;
            Bar {
                ts: t0 + i as i64 * STEP_MS,
                open: close,
                high: close,
                low: close,
                close,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect();
    store.append_bars(VENUE, SYMBOL, INTERVAL, &bars, None).expect("the double stores bars");
    Arc::new(store)
}

/// The `(test_start, test_end)` index pair of each window, in order.
fn ranges(rep: &vike_backtest::walkforward::WalkForwardReport) -> Vec<(usize, usize)> {
    rep.windows.iter().map(|w| w.test_range).collect()
}

/// The duration form walks, and its window count and edges are the ones the durations name.
/// 1,200 hourly bars, train 400, test 100, step 100 ⇒ validation windows at [400,500), [500,600),
/// … up to [1100,1200), the last one that fits.
#[test]
fn a_duration_form_profile_walks_and_reports_its_windows() {
    let p = profile("[walkforward]\ntrain = \"400bars\"\ntest = \"100bars\"\nstep = \"100bars\"\n");
    let rep = run_walkforward(&p, store()).unwrap();
    assert_eq!(
        ranges(&rep),
        vec![
            (400, 500),
            (500, 600),
            (600, 700),
            (700, 800),
            (800, 900),
            (900, 1000),
            (1000, 1100),
            (1100, 1200),
        ],
        "the durations name the edges; nothing here re-derives them from the splitter"
    );
    assert!(rep.windows.iter().all(|w| w.oos_return.is_finite()));
    assert!(!rep.oos_equity_curve.is_empty());
    assert!((0.0..=1.0).contains(&rep.wf_consistency));
}

/// `purge` moves the validation windows, visible from the OUTSIDE: the first one starts 8 bars
/// later than the unpurged walk's, one window no longer fits at the end, and every window still
/// reports a return.
#[test]
fn purge_moves_the_validation_windows_and_the_walk_still_reports() {
    let plain =
        profile("[walkforward]\ntrain = \"400bars\"\ntest = \"100bars\"\nstep = \"100bars\"\n");
    let purged = profile(
        "[walkforward]\ntrain = \"400bars\"\ntest = \"100bars\"\nstep = \"100bars\"\n\
         purge = \"8h\"\n",
    );
    let plain = run_walkforward(&plain, store()).unwrap();
    let purged = run_walkforward(&purged, store()).unwrap();

    assert_eq!(ranges(&plain)[0], (400, 500));
    assert_eq!(ranges(&purged)[0], (408, 508), "an 8h purge at a 1h interval is 8 bars");
    assert_eq!(
        ranges(&purged),
        vec![(408, 508), (508, 608), (608, 708), (708, 808), (808, 908), (908, 1008), (1008, 1108),],
        "the gap costs the last window, which no longer fits before the end of the series"
    );
    assert!(purged.windows.iter().all(|w| w.oos_return.is_finite()));
    assert!(!purged.oos_equity_curve.is_empty());
}

/// A `n_splits` profile produces the IDENTICAL report it produced before the window forms existed
/// — the regression pin for the whole stage.
///
/// ⚠ Compared against LITERAL expected numbers rather than against a second run, so it cannot pass
/// by both sides moving together. A pin that re-derives its own expectation from the code under
/// test pins nothing.
///
/// ⚠ **Read the scope of these literals before reading the green.** They were MEASURED on a the CI box
/// lane on 2026-09-13, from this exact fixture, at the point where `walk_forward_over_windows`
/// existed but the DRIVERS had not yet been switched onto a resolved window list — i.e. from the
/// split-count path exactly as it had always run, one commit before
/// `walkforward_preflight` started calling `resolve_windows`. They were NOT taken from pre-stage
/// `main`, and could not be: the fixture profile and the `MemHistStore` seeding this file needs did
/// not exist there. So this pin proves the driver change did not move the split-count report; it
/// does NOT independently prove the split-count report was already correct.
#[test]
fn the_split_count_walk_is_unchanged_by_the_window_forms() {
    let rep = run_walkforward(&profile("[walkforward]\nn_splits = 4\n"), store()).unwrap();
    assert_eq!(rep.windows.len(), 4);
    assert_eq!(ranges(&rep), vec![(240, 480), (480, 720), (720, 960), (960, 1200)]);
    for w in &rep.windows {
        assert_eq!(w.oos_return.to_bits(), 4_582_517_104_360_834_688, "{:?}", w.test_range);
        assert!(w.chosen_params.is_none(), "the fixed walk records no choice");
    }
    assert_eq!(rep.oos_return.to_bits(), 4_591_773_110_269_063_312);
    assert_eq!(rep.oos_sharpe.to_bits(), 4_651_159_460_628_889_417);
    assert_eq!(rep.wf_consistency.to_bits(), 1.0_f64.to_bits());
    assert_eq!(rep.oos_equity_curve.len(), 960);
}
