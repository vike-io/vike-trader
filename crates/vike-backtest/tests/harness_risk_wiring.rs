//! `[risk]` → `EngineParams.risk_limits` → a REAL `SimBroker` denial, end-to-end through
//! `run_backtest` (runprofile-wiring-step2). The unit tests in `harness/profile.rs` already prove
//! the TOML parses into `vike_exec::ProfileRisk`; this file proves the wiring actually reaches the
//! engine — "the field is populated" is not "the check runs" (the failure class a prior session
//! shipped tests that didn't actually pin).
//!
//! Only needs the `hist-replay` feature (no concrete `DataFusionHist`/`datafusion-store`): a
//! minimal local `HistStore` impl hands `run_backtest` a fixed bar series directly, so this test
//! rides the FAST hist-replay lane, not the heavier datafusion-store one.
#![cfg(feature = "hist-replay")]

use std::sync::Arc;

use vike_backtest::harness::{run_backtest, BacktestProfile};
use vike_backtest::BacktestResult;
use vike_data::{DataError, ExecFillRow, ExecOrderRow, HistStore, TsRange};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

const VENUE: &str = "test";
const SYMBOL: &str = "TESTUSDT";

/// A `HistStore` that hands back ONE fixed bar series for [`HistStore::load_bars`] regardless of
/// the requested `(venue, symbol, interval, range)` — everything else is an inert stub. Not
/// `MemHistStore` (whose `load_bars` is UNCONDITIONALLY empty by design — it exists for the
/// properties/equity/exec-log/funding/chain seams only, never bars/ticks) — this test needs REAL
/// bars flowing through `run_backtest`'s bar-mode branch into a real `StrategyEngine`.
struct FixedBarsStore(Vec<Bar>);

impl HistStore for FixedBarsStore {
    fn load_bars(&self, _v: &str, _s: &str, _i: &str, _r: TsRange) -> Result<Vec<Bar>, DataError> {
        Ok(self.0.clone())
    }
    fn scan_quotes(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        Ok(Vec::new())
    }
    fn scan_trades(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<TradeTick>, DataError> {
        Ok(Vec::new())
    }
    fn append_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _b: &[Bar],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_quotes(
        &self,
        _v: &str,
        _s: &str,
        _t: &[QuoteTick],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_trades(
        &self,
        _v: &str,
        _s: &str,
        _t: &[TradeTick],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_book_updates(
        &self,
        _v: &str,
        _s: &str,
        _u: &[BookUpdate],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_book_updates(
        &self,
        _v: &str,
        _s: &str,
        _r: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Ok(Vec::new())
    }
    fn append_symbol_properties(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[(i64, SymbolProperties)],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_symbol_properties(
        &self,
        _v: &str,
        _s: &str,
        _r: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Ok(Vec::new())
    }
    fn append_equity(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[EquitySample],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_equity(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<EquitySample>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_fills(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[ExecFillRow],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_fills(&self, _v: &str, _s: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_orders(
        &self,
        _v: &str,
        _s: &str,
        _rows: &[ExecOrderRow],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_orders(&self, _v: &str, _s: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        Ok(Vec::new())
    }
    fn resample_quotes_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn resample_trades_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    // append_funding / scan_funding / append_chain_snapshot / scan_chain / chain_as_of* /
    // properties_as_of / list_series / inventory / series_gaps all have empty/no-op DEFAULTS on
    // the trait — no override needed.
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
