//! `[risk]` → `EngineParams.risk_limits` → a REAL `SimBroker` denial, end-to-end through
//! `run_backtest` (runprofile-wiring-step2). The unit tests in `harness/profile.rs` already prove
//! the TOML parses into `vike_model::ProfileRisk`; this file proves the wiring actually reaches the
//! engine — "the field is populated" is not "the check runs" (the failure class a prior session
//! shipped tests that didn't actually pin).
//!
//! Needs no `datafusion-store` (no concrete `DataFusionHist`): a minimal local `HistStore` impl
//! hands `run_backtest` a fixed bar series directly, so this compiles and runs at DEFAULT
//! features — the FAST half of `scripts/ci_feature_suite.sh`'s `backtest-hist-replay` arm, not the
//! heavier `datafusion-store` one.

use std::sync::Arc;

use vike_analytics::BacktestResult;
use vike_backtest::harness::{BacktestProfile, run_backtest};
use vike_data::{DataError, HistStore, TsRange};
use vike_model::Bar;

const VENUE: &str = "test";
const SYMBOL: &str = "TESTUSDT";

/// A `HistStore` that hands back ONE fixed bar series for [`HistStore::load_bars`] regardless of
/// the requested `(venue, symbol, interval, range)` — everything else is an inert stub. Not
/// `MemHistStore`, which stores bars but answers only for the exact `(venue, symbol, interval)`
/// triple appended to it, while its tick scans stay empty — this test needs REAL bars flowing
/// through `run_backtest`'s bar-mode branch into a real `StrategyEngine`, whatever key it asks.
struct FixedBarsStore(Vec<Bar>);

impl HistStore for FixedBarsStore {
    fn load_bars(&self, _v: &str, _s: &str, _i: &str, _r: TsRange) -> Result<Vec<Bar>, DataError> {
        Ok(self.0.clone())
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
    // append_funding / scan_funding / append_chain_snapshot / scan_chain / chain_as_of* /
    // properties_as_of / list_series / inventory / series_gaps all have DEFAULTS on the trait — no
    // override needed. ⚠ "empty/no-op" is what that read until the funding and chain pairs were
    // made to REFUSE, and this list now splits THREE ways rather than two — ⚠ read the VARIANT off
    // the method in `crates/vike-data/src/store/hist.rs`, never off whichever neighbour is nearest:
    //   * `DataError::Unsupported` — append_funding, scan_funding, append_chain_snapshot,
    //     scan_chain, and chain_as_of* derived over the last of those.
    //   * `DataError::Query`       — list_series, inventory. They refuse for the same reason and in
    //     an older variant: their defaults predate `Unsupported` and nothing swept them.
    //   * an empty `Ok`            — properties_as_of (derived over this double's own
    //     `scan_symbol_properties`) and series_gaps.
    // Nothing here calls any of them, which is why this double still needs no arm — but a test that
    // starts calling one from either refusing group gets an `Err`, not a `[]`, and a `matches!`
    // written off the wrong group is a false negative rather than a failure.
}

const HOUR: i64 = 3_600_000;

/// One flat bar per hour, `n` bars starting at `start`, all priced `price`.
fn flat_bars(start: i64, n: i64, price: f64) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            ts: start + i * HOUR,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

const CASH: f64 = 1_000_000.0;

fn profile_toml(risk_section: &str) -> String {
    format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "bar"
interval = "1h"
from = "0"
to = "36000000"

[engine]
cash = {CASH}
fee_rate = 0.001

[strategy]
name = "buy_hold"
[strategy.params]
size = 1000.0

{risk_section}
"#
    )
}

/// `buy_hold` never closes (`Trade` — entry+exit — only exists for a ROUND TRIP), so a filled-but-
/// still-open position never shows up in `BacktestResult::trades`/`n_trades`. `fee_rate > 0` in
/// [`profile_toml`] gives a fill an observable side effect that DOES survive an open position:
/// `SimBroker::fold_fill` debits the transaction fee from cash immediately
/// (`sim_broker.rs::apply_fill`: `self.cash -= fee`), so a filled order strictly LOWERS
/// `final_equity` below `CASH` on this flat-price tape, while a denied order (no fill at all)
/// leaves it at EXACTLY `CASH`. This is the "did a real fill happen" signal these tests read,
/// alongside the RiskGate's own `dropped` reason string.
fn filled(result: &BacktestResult) -> bool {
    result.final_equity < CASH
}

/// A `[risk]` section with a `max_notional_per_order` cap far below what `buy_hold`'s single
/// market order would cost (`1000 units × 100.0 = 100_000` notional vs. a `250.0` cap) MUST deny
/// the order through the real `SimBroker` gate — not merely populate a struct field. Asserts on
/// `BacktestResult` (`dropped` carries the live `RiskGate`'s own reason string; `final_equity`
/// stays untouched because no fill, hence no fee, ever happened): the engine's observable
/// behavior, per the task's own "assert on behaviour, not the struct" requirement.
#[test]
fn risk_limits_from_profile_deny_a_violating_order() {
    let toml = profile_toml("[risk]\nmax_notional_per_order = 250.0");
    let profile = BacktestProfile::from_toml_str(&toml).expect("profile parses");
    assert!(profile.risk.is_some(), "the `[risk]` section must parse");

    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(FixedBarsStore(flat_bars(0, 10, 100.0)));
    let result = run_backtest(&profile, store).expect("run_backtest succeeds");

    assert!(
        !filled(&result),
        "the over-cap order must never fill (final_equity {} should equal cash {CASH}): {result:?}",
        result.final_equity
    );
    assert!(
        result.dropped.iter().any(|(_, reason, _, _)| reason == "over-max-notional"),
        "the RiskGate's own denial reason must be surfaced in `dropped`: {:?}",
        result.dropped
    );
}

/// The exact same profile MINUS `[risk]` must let the order through — the byte-identical-default
/// claim proven against the SAME strategy/bars/cash, not just "risk_limits is None" in isolation.
#[test]
fn no_risk_section_is_byte_identical_to_today() {
    let toml = profile_toml("");
    let profile = BacktestProfile::from_toml_str(&toml).expect("profile parses");
    assert!(profile.risk.is_none(), "no `[risk]` section -> None");

    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(FixedBarsStore(flat_bars(0, 10, 100.0)));
    let result = run_backtest(&profile, store).expect("run_backtest succeeds");

    assert!(
        filled(&result),
        "with no gate armed the buy_hold order must fill (final_equity {} should be < cash \
         {CASH}): {result:?}",
        result.final_equity
    );
    assert!(result.dropped.is_empty(), "nothing should be gate-dropped: {:?}", result.dropped);
}

/// A generous cap (well above the order's notional) must likewise let the order through — proves
/// the gate is a real pass/deny judge, not a knob that always denies once armed.
#[test]
fn risk_limits_from_profile_admit_a_compliant_order() {
    let toml = profile_toml("[risk]\nmax_notional_per_order = 1000000.0");
    let profile = BacktestProfile::from_toml_str(&toml).expect("profile parses");

    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(FixedBarsStore(flat_bars(0, 10, 100.0)));
    let result = run_backtest(&profile, store).expect("run_backtest succeeds");

    assert!(filled(&result), "a compliant order must still fill: {result:?}");
    assert!(result.dropped.is_empty(), "nothing should be gate-dropped: {:?}", result.dropped);
}
