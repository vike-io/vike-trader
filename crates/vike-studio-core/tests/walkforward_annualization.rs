//! ⚠ **The cross-plane annualization gate: the Studio door must annualize a walk-forward by the
//! BAR INTERVAL, and it must land on the SAME factor the harness door derives.**
//!
//! `vike_analytics::metrics::sharpe(curve, periods_per_year)` scales by `sqrt(periods_per_year)`,
//! where that number is the count of RETURN OBSERVATIONS a year produces — one per bar. The harness
//! plane has always derived it from the bar interval; the Studio plane passed a bare `252.0`, the
//! DAILY count, whatever the bars were. Same strategy, same 1h series, and `oos_sharpe` differed by
//! `sqrt(24) ≈ 4.9x` between the CLI/MCP door and the Studio door — `sqrt(1440) ≈ 37.9x` on the 1m
//! series the datahub roundtrip fixtures actually use. Two doc comments asserted the two planes
//! could not disagree. Nothing in the tree compared them.
//!
//! # Why the existing roundtrip gates cannot see this
//!
//! `crates/vike-datahub/tests/run_sweep_walkforward_roundtrip.rs` and its `_profile_` sibling
//! compare a LOCAL run against the SAME run over the wire — one plane, twice — so both sides carry
//! whatever factor that plane applies and a wrong one is bit-identical to itself. They are
//! transport gates, and they remain correct as transport gates; the divergence they cannot express
//! is between two DOORS.
//!
//! # What this file asserts, and how it avoids grading its own homework
//!
//! Every claim here is recovered from the report the Studio door RETURNED, not from the argument it
//! was given: `WalkForwardReport::oos_equity_curve` IS the stitched vector `oos_sharpe` was
//! computed from, so re-running `sharpe` over it at a candidate factor reproduces the reported
//! number bit-for-bit only when that candidate is the factor actually applied. The expected factors
//! are written as NUMBERS (`6_048.0`, `362_880.0`) rather than as a call to the derivation, because
//! an assertion that two expressions agree still passes when both regress together.
//!
//! ⚠ **The precondition that makes all of it load-bearing**, stated rather than assumed:
//! `metrics::sharpe` answers `0.0` for a curve with no return dispersion — at EVERY factor. Over a
//! degenerate fixture every assertion below would pass no matter what the Studio applied, so
//! [`walkforward_at`] refuses a report whose `oos_sharpe` is zero or non-finite before any of them
//! runs.
//!
//! Two limits this file does NOT cover, so nobody reads it as wider than it is: the TICK door
//! (`SliceKind::Ticks`) never reaches `walk_forward_strategy` at all — `load_slice_bars` rejects a
//! tick slice — and the 252 anchor itself is left exactly where it was (these markets trade 24/7,
//! so a defensible crypto anchor is 365; moving it would move every existing daily report and is a
//! separate decision from fixing the intraday scale).

use std::sync::Arc;

use vike_backtest::harness::BacktestProfile;
use vike_backtest::harness::report::periods_per_year;
use vike_backtest::metrics::sharpe;
use vike_backtest::report::{DAILY_PERIODS_PER_YEAR, periods_per_year_for_interval};
use vike_backtest::walkforward::WalkForwardReport;
use vike_data::test_support::MemHistStore;
use vike_data::{HistStore, TsRange};
use vike_model::Bar;
use vike_studio_core::{DataSlice, StoreHandle, StrategySpec, run_walkforward_slice};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
/// Long enough that every OOS window still holds many `sma(20)` crosses after the warmup — the
/// fixture length `vike_studio_core::run`'s own tests and the datahub roundtrip gates both use.
const N_BARS: usize = 400;
const N_SPLITS: usize = 4;

/// `252 * 24` — a year of HOURLY return observations. Spelled as the number so a regression that
/// re-derived it wrongly on BOTH sides of a comparison still fails here.
const HOURLY_PERIODS_PER_YEAR: f64 = 6_048.0;
/// `252 * 1440` — a year of MINUTELY return observations, and the scale the datahub roundtrip
/// fixtures run at.
const MINUTELY_PERIODS_PER_YEAR: f64 = 362_880.0;

/// The SMA(5/20)-cross from `vike_studio_core::run`'s own tests, copied verbatim (the datahub
/// roundtrip gates carry the same copy for the same reason): it TRADES over the seeded walk below,
/// so the walk-forward windows stitch an OOS curve with real return dispersion — which is the
/// precondition this file's assertions rest on.
const CROSS_SCRIPT: &str = r#"
const QTY = 1.0;
fn on_bar() {
    let f = sma(5); let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

/// A store holding `N_BARS` deterministic random-walk bars at `interval`.
///
/// The bar STEP is derived from the same interval string the series is keyed on, so the fixture
/// cannot claim one scale while stepping at another — a 1h series stepped at 60s would be a lie no
/// assertion here could catch, and it is exactly the shape of mistake this branch is fixing.
///
/// `MemHistStore` rather than `DataFusionHist`: it is the workspace's own shared double, it stores
/// real bars, and a store that needs no temp directory keeps this gate runnable on a box with no
/// DataFusion at all. `symbol: None` on every bar is not an omission — the bar schema persists no
/// symbol column, so the store erases it either way, and `StrategyEngine::new` re-tags each bar
/// with the series it was registered under.
fn seeded_store(interval: &str) -> StoreHandle {
    let step_ms = vike_model::time::interval_ms(interval).expect("the fixture interval parses");
    let mut px = 100.0f64;
    let mut seed = 0x1234_5678u64;
    let bars: Vec<Bar> = (0..N_BARS)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            px = (px + ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0).max(1.0);
            Bar {
                ts: step_ms * (i as i64 + 1),
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect();
    let store = MemHistStore::new();
    store.append_bars(VENUE, SYMBOL, interval, &bars, None).expect("the double accepts bars");
    Arc::new(store) as StoreHandle
}

/// Drive the STUDIO door — `vike_studio_core::run_walkforward_slice`, the entry the Studio pane,
/// the `RunWalkforward` verb and the MCP surface all reach — over a fresh `interval` fixture, and
/// refuse a report the assertions below could not distinguish.
fn walkforward_at(interval: &str) -> WalkForwardReport {
    let store = seeded_store(interval);
    let spec = StrategySpec::rhai(CROSS_SCRIPT);
    let slice = DataSlice::bars(VENUE, SYMBOL, interval, TsRange::all());
    let rep = run_walkforward_slice(&spec, &slice, &store, N_SPLITS)
        .unwrap_or_else(|e| panic!("the Studio walk-forward must run over {interval} bars: {e:?}"));

    assert_eq!(rep.windows.len(), N_SPLITS, "{interval}: one OOS window per split");
    assert!(
        rep.oos_sharpe.is_finite() && rep.oos_sharpe != 0.0,
        "PRECONDITION FAILED for {interval}: the stitched OOS curve has no return dispersion, so \
         `metrics::sharpe` answers 0.0 at EVERY annualization factor and every assertion in this \
         file would pass vacuously. Fix the fixture (the strategy stopped trading), not the \
         assertion — got oos_sharpe {}",
        rep.oos_sharpe
    );
    rep
}

/// The annualization factor a report ACTUALLY applied, recovered from its own two numbers.
///
/// `sharpe(curve, ppy) = (mean/std) * sqrt(ppy)`, so `sharpe(curve, 1.0)` is the un-annualized
/// ratio exactly (`1.0_f64.sqrt()` is `1.0`, and multiplying by it is exact) and the quotient
/// squared is `ppy`. This names no derivation and no constant, so it still fails if the derivation
/// is deleted and replaced by a literal.
fn recovered_periods_per_year(rep: &WalkForwardReport) -> f64 {
    let unannualized = sharpe(&rep.oos_equity_curve, 1.0);
    (rep.oos_sharpe / unannualized).powi(2)
}

/// Both recoveries of the applied factor: the bit-exact re-computation, and the arithmetic one.
///
/// The first is bit-exact because it is the same call over the same inputs — `oos_equity_curve` IS
/// the vector `oos_sharpe` was computed from. The second is a float round trip (one division, one
/// square) and is compared with a relative bound, which is a rounding allowance and not a tolerance
/// on the CLAIM: a wrong factor here is wrong by a whole `sqrt(24)` or more.
fn assert_annualized_at(rep: &WalkForwardReport, expected: f64, interval: &str) {
    assert_eq!(
        rep.oos_sharpe.to_bits(),
        sharpe(&rep.oos_equity_curve, expected).to_bits(),
        "{interval}: oos_sharpe must be the stitched OOS curve annualized at {expected}, got {}",
        rep.oos_sharpe
    );
    let recovered = recovered_periods_per_year(rep);
    assert!(
        ((recovered - expected) / expected).abs() < 1e-9,
        "{interval}: the factor recovered from the report itself is {recovered}, not {expected}"
    );
}

/// A minimal BAR profile at `interval` — the HARNESS door's input. Only the two fields
/// `periods_per_year` reads are meaningful; the rest is the smallest thing that parses (the same
/// fixture shape `crates/vike-backtest/src/harness/report.rs`'s own tests use).
fn bar_profile(interval: &str) -> BacktestProfile {
    let profile = format!(
        r#"
name = "annualization-seam"
[data]
kind = "bar"
interval = "{interval}"
from = "0"
to = "1"
venue = "binance"
symbols = ["BTCUSDT"]
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#
    );
    toml::from_str(&profile).expect("fixture profile must parse")
}

/// The bug itself, on hourly bars: `252` understates the reported Sharpe by `sqrt(24)`.
#[test]
fn the_studio_door_annualizes_an_hourly_walk_forward_at_6048_not_252() {
    let rep = walkforward_at("1h");
    assert_annualized_at(&rep, HOURLY_PERIODS_PER_YEAR, "1h");
    assert_ne!(
        rep.oos_sharpe.to_bits(),
        sharpe(&rep.oos_equity_curve, DAILY_PERIODS_PER_YEAR).to_bits(),
        "1h bars annualized at the DAILY 252 understate oos_sharpe by sqrt(24) ~ 4.9x"
    );
}

/// The same bug at the scale the CI roundtrip fixtures actually run at: `sqrt(1440) ~ 37.9x`.
#[test]
fn the_studio_door_annualizes_a_one_minute_walk_forward_at_362_880_not_252() {
    let rep = walkforward_at("1m");
    assert_annualized_at(&rep, MINUTELY_PERIODS_PER_YEAR, "1m");
    assert_ne!(
        rep.oos_sharpe.to_bits(),
        sharpe(&rep.oos_equity_curve, DAILY_PERIODS_PER_YEAR).to_bits(),
        "1m bars annualized at the DAILY 252 understate oos_sharpe by sqrt(1440) ~ 37.9x"
    );
}

/// The anchor does not move: a DAILY Studio walk-forward reports exactly what it always did, so
/// this fix is not a silent rescaling of every daily number ever recorded.
#[test]
fn the_studio_door_still_annualizes_a_daily_walk_forward_at_the_252_anchor() {
    let rep = walkforward_at("1d");
    assert_annualized_at(&rep, DAILY_PERIODS_PER_YEAR, "1d");
}

/// The isolation claim: change ONLY the declared interval and nothing but the annualization moves.
///
/// The two fixtures carry byte-identical closes in byte-identical order — only the `ts` step and
/// the interval string differ — and under `EngineParams::default` the bar path consults no
/// timestamp at all (`settlement_period_ms`, `max_price_staleness_ms`, `funding_source` and
/// `resolution` are all `None`, `session_gate` is off and `timeframes` is empty). So the stitched
/// curve must be bit-identical across the two runs, and the Sharpes must differ by exactly the
/// square root of the factor ratio, `sqrt(60)`.
///
/// ⚠ If the CURVE half of this test fails, the fixture changed (something on the bar path became
/// timestamp-sensitive) — read that before touching the annualization half.
#[test]
fn the_same_bars_at_a_different_interval_move_only_the_annualization() {
    let hourly = walkforward_at("1h");
    let minutely = walkforward_at("1m");

    assert_eq!(
        hourly.oos_equity_curve.len(),
        minutely.oos_equity_curve.len(),
        "the same closes must stitch the same number of OOS points at either interval"
    );
    for (i, (a, b)) in hourly.oos_equity_curve.iter().zip(&minutely.oos_equity_curve).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "stitched equity[{i}]: 1h {a} vs 1m {b}");
    }
    assert_eq!(hourly.oos_return.to_bits(), minutely.oos_return.to_bits(), "oos_return");

    let expected = (MINUTELY_PERIODS_PER_YEAR / HOURLY_PERIODS_PER_YEAR).sqrt();
    let ratio = minutely.oos_sharpe / hourly.oos_sharpe;
    assert!(
        ((ratio - expected) / expected).abs() < 1e-9,
        "one curve at two intervals must differ by sqrt(1440/24) = {expected}, got {ratio}"
    );
}

/// The SEAM itself, with no store and no strategy in the way: for one interval, the harness door
/// (`harness::report::periods_per_year`, what a `BacktestProfile` resolves through) and the shared
/// derivation the Studio door reaches must answer the same number — and that number is pinned, so
/// the two regressing together is still a failure.
#[test]
fn both_doors_resolve_the_same_annualization_for_the_same_interval() {
    for (interval, expected) in [
        ("1h", HOURLY_PERIODS_PER_YEAR),
        ("1m", MINUTELY_PERIODS_PER_YEAR),
        ("1d", DAILY_PERIODS_PER_YEAR),
    ] {
        let harness_door = periods_per_year(&bar_profile(interval));
        let shared = periods_per_year_for_interval(interval);
        assert_eq!(
            harness_door.to_bits(),
            expected.to_bits(),
            "{interval}: the harness door must annualize at {expected}, got {harness_door}"
        );
        assert_eq!(
            shared.to_bits(),
            harness_door.to_bits(),
            "{interval}: the two doors must resolve ONE factor — harness {harness_door}, \
             shared derivation {shared}"
        );
    }
}
