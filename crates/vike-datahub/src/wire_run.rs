//! PR-3 — the `RunSlice` boundary: convert the wire DTOs into the `vike-studio-core` / `vike-backtest`
//! types, run the EXISTING `vike_studio_core::run::run_slice` next to the data, and mirror the
//! `BacktestResult` back onto the wire. This whole module is behind `serve-datafusion` (declared so
//! in `lib.rs`), because it is the only place `vike-datahub` names `vike-studio-core` — which pulls
//! `vike-data/hist-datafusion`. A default build compiles NONE of this and answers a `RunSlice`
//! request with a clean error (see `server.rs`).
//!
//! This is the #719 pattern: the wire schema (the `Wire*` DTOs) lives in the LIGHT client crate; the
//! engine types stay serde-free; the two meet HERE, at the compute-to-data boundary. [`run_slice_local`]
//! is the ONE entry both the server arm and the parity test drive, so "local" and "over the wire" are
//! provably the same computation (the S2 parity guarantee).

use std::sync::Arc;

use vike_backtest::walkforward::{WalkForwardReport, WfWindow};
use vike_backtest::{BacktestResult, EngineParams};
use vike_data::{HistStore, TsRange};
use vike_studio_core::{
    DataSlice, RunError, SliceKind, StrategySpec, StudioSweep, SweepEntry, run_slice,
    run_sweep_slice_with_params, run_walkforward_slice_with_params,
};

use vike_datahub_client::wire_studio::{
    WireEngineParams, WireRunError, WireRunResult, WireSlice, WireSliceKind, WireSpec, WireSweep,
    WireSweepEntry, WireSweepResult, WireTrade, WireWalkforward, WireWalkforwardResult,
    WireWfWindow,
};

/// The store handle every runner accepts (`vike_studio_core::StoreHandle`), spelled locally so this
/// module names no extra import — `run_slice` takes `&StoreHandle`.
type StoreHandle = Arc<dyn HistStore + Send + Sync>;

/// [`WireSpec`] → `StrategySpec`. A native strategy's params arrive as TOML TEXT and are parsed back
/// into the params table at THIS boundary (`StrategySpec::native_from_toml_str`) — a parse failure
/// becomes a [`RunError::Strategy`] (the "no such/invalid strategy" class), never a panic.
pub fn to_strategy_spec(spec: &WireSpec) -> Result<StrategySpec, RunError> {
    match spec {
        WireSpec::Rhai(src) => Ok(StrategySpec::rhai(src.clone())),
        WireSpec::Native { name, params_toml } => {
            StrategySpec::native_from_toml_str(name.clone(), params_toml)
                .map_err(|e| RunError::Strategy(format!("params TOML parse failed: {e}")))
        }
    }
}

/// [`WireSlice`] → `DataSlice`, recomposing the decomposed `start`/`end` into a `vike_data::TsRange`.
pub fn to_data_slice(slice: &WireSlice) -> DataSlice {
    DataSlice {
        venue: slice.venue.clone(),
        symbols: slice.symbols.clone(),
        interval: slice.interval.clone(),
        range: TsRange { start: slice.start, end: slice.end },
        kind: match slice.kind {
            WireSliceKind::Bars => SliceKind::Bars,
            WireSliceKind::Ticks => SliceKind::Ticks,
        },
    }
}

/// [`WireEngineParams`] → `EngineParams`: start from `EngineParams::default()` and apply ONLY the
/// `Some` wire fields, so every omitted field (and `params: None` entirely) takes the engine default.
/// This is the ONE definition of that mapping; the server and the parity test both reach it through
/// [`run_slice_local`], so a local run and a remote run cannot diverge on how params were resolved.
pub fn to_engine_params(params: Option<&WireEngineParams>) -> EngineParams {
    let mut ep = EngineParams::default();
    if let Some(p) = params {
        if let Some(cash) = p.cash {
            ep.cash = cash;
        }
        if let Some(fee_rate) = p.fee_rate {
            ep.fee_rate = fee_rate;
        }
        if let Some(slippage) = p.slippage {
            ep.slippage = slippage;
        }
    }
    ep
}

/// `BacktestResult` → [`WireRunResult`]: copy ONLY the rendered fields (NOT the whole result — a
/// growing superset), mirroring each closed `Trade` onto a [`WireTrade`].
pub fn to_wire_result(r: &BacktestResult) -> WireRunResult {
    WireRunResult {
        equity_curve: r.equity_curve.clone(),
        equity_ts: r.equity_ts.clone(),
        final_equity: r.final_equity,
        n_trades: r.n_trades,
        per_symbol_pnl: r.per_symbol_pnl.clone(),
        trades: r.trades.iter().map(WireTrade::from_trade).collect(),
        stale_deferrals: r.stale_deferrals,
        session_deferrals: r.session_deferrals,
    }
}

/// `RunError` → [`WireRunError`], classifying it into a machine `kind` (`"compile"` / `"data"` /
/// `"strategy"`) while preserving the underlying detail message. The server stringifies the result
/// kind-first into `Response::Error` (see [`WireRunError::to_error_string`]).
pub fn run_error_to_wire(e: &RunError) -> WireRunError {
    match e {
        RunError::Compile(m) => WireRunError::new("compile", m.clone()),
        RunError::Data(m) => WireRunError::new("data", m.clone()),
        RunError::Strategy(m) => WireRunError::new("strategy", m.clone()),
    }
}

/// The full local `RunSlice` computation: convert the DTOs, run `vike_studio_core::run::run_slice`
/// over `store`, and mirror the result back onto the wire — or a classified [`WireRunError`] on
/// failure. Both the server arm (`server.rs`) and the parity test drive THIS function, so "run
/// locally" and "run over the wire" are, by construction, the same computation (S2).
pub fn run_slice_local(
    spec: &WireSpec,
    slice: &WireSlice,
    params: Option<&WireEngineParams>,
    store: StoreHandle,
) -> Result<WireRunResult, WireRunError> {
    let strat_spec = to_strategy_spec(spec).map_err(|e| run_error_to_wire(&e))?;
    let data_slice = to_data_slice(slice);
    let engine_params = to_engine_params(params);
    match run_slice(&strat_spec, &data_slice, &store, engine_params) {
        Ok(result) => Ok(to_wire_result(&result)),
        Err(e) => Err(run_error_to_wire(&e)),
    }
}

/// Map a non-finite `f64` to `None`, a finite one to `Some` — the guard [`to_wire_sweep_result`]
/// applies to `StudioSweep`'s `dsr`/`pbo`. Both are plain `f64` that are `NaN` in NORMAL operation (a
/// single-point / `< 2`-trial grid leaves PBO unassessable), and `serde_json` encodes a `NaN` as JSON
/// `null` that then FAILS to decode back into a plain `f64` — so carrying them as `Option<f64>` (this
/// maps the non-finite case to `None`) is what keeps an ordinary sweep from becoming a client-side
/// decode error (see [`WireSweepResult`]'s docs).
fn finite_or_none(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

/// `SweepEntry` → [`WireSweepEntry`]: the point's overrides plus the rendered [`to_wire_result`] of
/// its `BacktestResult`.
fn to_wire_sweep_entry(e: &SweepEntry) -> WireSweepEntry {
    WireSweepEntry { overrides: e.overrides.clone(), result: to_wire_result(&e.result) }
}

/// `WfWindow` → [`WireWfWindow`]: one out-of-sample window's `test_range` + `oos_return`.
fn to_wire_wf_window(w: &WfWindow) -> WireWfWindow {
    WireWfWindow { test_range: w.test_range, oos_return: w.oos_return }
}

/// `StudioSweep` → [`WireSweepResult`]: mirror each ranked entry and carry `dsr`/`pbo` through
/// [`finite_or_none`] so a non-assessable (`NaN`) score crosses the wire as `None`, not a
/// decode-breaking `null`.
pub fn to_wire_sweep_result(s: &StudioSweep) -> WireSweepResult {
    WireSweepResult {
        entries: s.entries.iter().map(to_wire_sweep_entry).collect(),
        dsr: finite_or_none(s.dsr),
        pbo: finite_or_none(s.pbo),
        best_index: s.best_index,
    }
}

/// `WalkForwardReport` → [`WireWalkforwardResult`]: a FULL mirror — every window, the stitched OOS
/// equity curve, and the three summary scalars (all provably finite, so no `Option` guard is needed).
pub fn to_wire_walkforward_result(r: &WalkForwardReport) -> WireWalkforwardResult {
    WireWalkforwardResult {
        windows: r.windows.iter().map(to_wire_wf_window).collect(),
        oos_equity_curve: r.oos_equity_curve.clone(),
        oos_return: r.oos_return,
        oos_sharpe: r.oos_sharpe,
        wf_consistency: r.wf_consistency,
    }
}

/// The full local `RunSweep` computation: convert the DTOs, run
/// `vike_studio_core::run_sweep_slice_with_params` over `store`, and mirror the ranked result back
/// onto the wire — or a classified [`WireRunError`] on failure. `params` is the optional cost/cash
/// override applied to EVERY grid point (`None` = engine defaults), threaded through [`to_engine_params`]
/// — the SAME mapping `run_slice_local` uses. Both the server arm (`server.rs`) and the parity test
/// drive THIS function, so "run locally" and "run over the wire" are, by construction, the same
/// computation.
pub fn run_sweep_local(
    spec: &WireSpec,
    slice: &WireSlice,
    sweep: &WireSweep,
    params: Option<&WireEngineParams>,
    store: StoreHandle,
) -> Result<WireSweepResult, WireRunError> {
    let strat_spec = to_strategy_spec(spec).map_err(|e| run_error_to_wire(&e))?;
    let data_slice = to_data_slice(slice);
    // Own the wire params so the per-point factory closure is `Send + Sync` (the bounded parallel
    // sweep pool requires it); each point rebuilds a fresh `EngineParams` from the same knobs.
    let params = params.cloned();
    let result = run_sweep_slice_with_params(&strat_spec, &data_slice, &store, &sweep.axes, || {
        to_engine_params(params.as_ref())
    });
    match result {
        Ok(result) => Ok(to_wire_sweep_result(&result)),
        Err(e) => Err(run_error_to_wire(&e)),
    }
}

/// The full local `RunWalkforward` computation: convert the DTOs, run
/// `vike_studio_core::run_walkforward_slice_with_params` over `store`, and mirror the stitched report
/// back onto the wire — or a classified [`WireRunError`] on failure. `params` is the optional
/// cost/cash override applied to every OOS window (`None` = engine defaults), threaded through
/// [`to_engine_params`]. The SAME entry the server arm and the parity test drive, keeping "local"
/// and "over the wire" one computation.
pub fn run_walkforward_local(
    spec: &WireSpec,
    slice: &WireSlice,
    walkforward: &WireWalkforward,
    params: Option<&WireEngineParams>,
    store: StoreHandle,
) -> Result<WireWalkforwardResult, WireRunError> {
    let strat_spec = to_strategy_spec(spec).map_err(|e| run_error_to_wire(&e))?;
    let data_slice = to_data_slice(slice);
    let params = params.cloned();
    let result = run_walkforward_slice_with_params(
        &strat_spec,
        &data_slice,
        &store,
        walkforward.n_splits,
        || to_engine_params(params.as_ref()),
    );
    match result {
        Ok(result) => Ok(to_wire_walkforward_result(&result)),
        Err(e) => Err(run_error_to_wire(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_engine_params_none_is_all_default() {
        let ep = to_engine_params(None);
        let def = EngineParams::default();
        assert_eq!(ep.cash.to_bits(), def.cash.to_bits());
        assert_eq!(ep.fee_rate.to_bits(), def.fee_rate.to_bits());
        assert_eq!(ep.slippage.to_bits(), def.slippage.to_bits());
    }

    #[test]
    fn to_engine_params_applies_only_set_fields() {
        let def = EngineParams::default();
        let wp = WireEngineParams { cash: Some(5000.0), fee_rate: None, slippage: Some(0.25) };
        let ep = to_engine_params(Some(&wp));
        assert_eq!(ep.cash, 5000.0);
        assert_eq!(ep.slippage, 0.25);
        // an omitted field keeps the engine default (bit-for-bit)
        assert_eq!(ep.fee_rate.to_bits(), def.fee_rate.to_bits());
    }

    #[test]
    fn to_data_slice_recomposes_the_range_and_kind() {
        let slice = WireSlice {
            venue: "polymarket".to_string(),
            symbols: vec!["TKN".to_string()],
            interval: String::new(),
            start: Some(10),
            end: None,
            kind: WireSliceKind::Ticks,
        };
        let ds = to_data_slice(&slice);
        assert_eq!(ds.venue, "polymarket");
        assert_eq!(ds.symbols, vec!["TKN".to_string()]);
        assert_eq!(ds.range.start, Some(10));
        assert_eq!(ds.range.end, None);
        assert_eq!(ds.kind, SliceKind::Ticks);
    }

    #[test]
    fn run_error_to_wire_classifies_each_variant() {
        assert_eq!(run_error_to_wire(&RunError::Compile("x".into())).kind, "compile");
        assert_eq!(run_error_to_wire(&RunError::Data("x".into())).kind, "data");
        assert_eq!(run_error_to_wire(&RunError::Strategy("x".into())).kind, "strategy");
        // the detail survives
        assert_eq!(run_error_to_wire(&RunError::Data("no bars".into())).message, "no bars");
    }
}
