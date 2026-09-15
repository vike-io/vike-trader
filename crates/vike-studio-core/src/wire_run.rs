//! The Studio compute-to-data BOUNDARY: convert the wire DTOs into this crate's own run types, run
//! the EXISTING `run_slice`/`run_paramscan_slice_with_params`/`run_walkforward_slice_with_params` next to
//! the data, and mirror the result back onto the wire.
//!
//! ⚠ **This module MOVED here from `vike-datahub` (ruling 7 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`), and the move is a
//! consequence of the LAYER RULE rather than a preference.** The three verbs it serves —
//! `RunSlice`, `RunSweep`, `RunWalkforward` — left the data daemon for
//! `vike-backend backtest --addr`, whose server lives in `vike-backtest` (layer 50). That crate
//! cannot name `vike-studio-core` (layer 55): the dependency direction is down-only and this crate
//! already depends on `vike-backtest`. So the compute server dispatches the Studio three through a
//! feature-free injected table (`vike_backtest::compute_server`'s `StudioRunTable` — a `Box<dyn Fn>`
//! whose signature names ONLY the `Wire*` DTOs), and the constructor that fills that table with real
//! runners is [`studio_run_table`], here, in the crate that owns the types being converted INTO.
//!
//! It could not stay where it was: with the verbs gone, `vike-datahub` would have kept a
//! `vike-studio-core` edge and a Rhai compiler for a surface it no longer serves — which is exactly
//! the mixing ruling 7 exists to end.
//!
//! This is the #719 pattern, unchanged: the wire schema (the `Wire*` DTOs) lives in the LIGHT client
//! crate; the engine types stay serde-free; the two meet HERE. [`run_slice_local`] is the ONE entry
//! both the server arm and the parity test drive, so "local" and "over the wire" are provably the
//! same computation (the S2 parity guarantee).
//!
//! ⚠ It is NOT feature-gated in this crate, unlike its `serve-datafusion` life in `vike-datahub`.
//! The gate over there existed because naming `vike-studio-core` was what pulled
//! `vike-data/hist-datafusion` into a crate that wanted to stay DataFusion-free. Here the studio
//! types are the crate's own, and the DTOs come from `vike-datahub-client`, which is DataFusion-free
//! by construction — so a default `vike-studio-core` build compiles this and stays exactly as light
//! as `scripts/ci_feature_suite.sh`'s `studio-standalone` lane requires.

use std::sync::Arc;

use vike_backtest::walkforward::{WalkForwardReport, WfWindow};
use vike_backtest::{BacktestResult, EngineParams};
use vike_data::{HistStore, TsRange};

use crate::{
    DataSlice, ParamscanEntry, RunError, SliceKind, StrategySpec, StudioParamscan,
    run_paramscan_slice_with_params, run_slice, run_walkforward_slice_with_params,
};

use vike_datahub_client::wire_studio::{
    WireEngineParams, WireParamscan, WireParamscanEntry, WireParamscanResult, WireRunError,
    WireRunResult, WireSlice, WireSliceKind, WireSpec, WireTrade, WireWalkforward,
    WireWalkforwardResult, WireWfWindow,
};

/// The store handle every runner accepts (`crate::StoreHandle`), spelled locally so this
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

/// The full local `RunSlice` computation: convert the DTOs, run `crate::run::run_slice`
/// over `store`, and mirror the result back onto the wire — or a classified [`WireRunError`] on
/// failure. Both the server arm (`vike_backtest::compute_server`) and the parity test drive THIS function, so "run
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

/// Map a non-finite `f64` to `None`, a finite one to `Some` — the guard [`to_wire_paramscan_result`]
/// applies to `StudioParamscan`'s `dsr`/`pbo`. Both are plain `f64` that are `NaN` in NORMAL operation (a
/// single-point / `< 2`-trial grid leaves PBO unassessable), and `serde_json` encodes a `NaN` as JSON
/// `null` that then FAILS to decode back into a plain `f64` — so carrying them as `Option<f64>` (this
/// maps the non-finite case to `None`) is what keeps an ordinary sweep from becoming a client-side
/// decode error (see [`WireParamscanResult`]'s docs).
fn finite_or_none(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

/// `ParamscanEntry` → [`WireParamscanEntry`]: the point's overrides plus the rendered [`to_wire_result`] of
/// its `BacktestResult`.
fn to_wire_paramscan_entry(e: &ParamscanEntry) -> WireParamscanEntry {
    WireParamscanEntry { overrides: e.overrides.clone(), result: to_wire_result(&e.result) }
}

/// `WfWindow` → [`WireWfWindow`]: one out-of-sample window's `test_range` + `oos_return`, plus the
/// parameters that window's own search CHOSE — each `toml::Value` RENDERED to its TOML text.
///
/// ⚠ That rendering is the ONE place a mirror here stops being a faithful copy, and the reason is a
/// property of the CLIENT crate rather than a choice made at this boundary: `vike-datahub-client`
/// carries no `toml` dependency — it is the light, DataFusion-free half the GUI links — so the
/// alternative to text was adding a TOML parser to the crate whose weight is its entire point.
/// [`WireWfWindow`]'s own doc argues the trade and names what it costs; the inverse (parse the text
/// back into a `toml::Value`) is `vike_studio::remote`'s `to_wf_window`.
///
/// This function names no `toml` path either, and does not need one: `Display` is reached through
/// the std `ToString` blanket impl, so `vike-datahub` mirrors a `toml::Value` without depending on
/// `toml` any more than the client does. ⚠ The residual, named rather than guarded: that `Display`
/// impl UNWRAPS its own serializer, so a value it could not render would panic here instead of
/// degrading. Nothing on this path constructs one — `chosen_params` values come from
/// `harness::sweep::expand_paramscan`, i.e. they were parsed OUT of a `[sweep]` array and are
/// re-renderable by construction — and this verb's server arm cannot produce a `Some` at all yet
/// (see [`WireWfWindow`]'s field doc for why the field is mirrored regardless).
fn to_wire_wf_window(w: &WfWindow) -> WireWfWindow {
    WireWfWindow {
        test_range: w.test_range,
        oos_return: w.oos_return,
        chosen_params: w
            .chosen_params
            .as_ref()
            .map(|chosen| chosen.iter().map(|(k, v)| (k.clone(), v.to_string())).collect()),
    }
}

/// `StudioParamscan` → [`WireParamscanResult`]: mirror each ranked entry and carry `dsr`/`pbo` through
/// [`finite_or_none`] so a non-assessable (`NaN`) score crosses the wire as `None`, not a
/// decode-breaking `null`.
pub fn to_wire_paramscan_result(s: &StudioParamscan) -> WireParamscanResult {
    WireParamscanResult {
        entries: s.entries.iter().map(to_wire_paramscan_entry).collect(),
        dsr: finite_or_none(s.dsr),
        pbo: finite_or_none(s.pbo),
        best_index: s.best_index,
    }
}

/// `WalkForwardReport` → [`WireWalkforwardResult`]: a FULL mirror — every window (including what
/// each one CHOSE, when the walk searched), the stitched OOS equity curve, and the three summary
/// scalars (all provably finite, so no `Option` guard is needed).
///
/// "Full" is the field ROSTER, not the field TYPES: every `WalkForwardReport` field crosses, but a
/// window's chosen values cross as TOML text rather than as `toml::Value` — see
/// [`to_wire_wf_window`].
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
/// `crate::run_paramscan_slice_with_params` over `store`, and mirror the ranked result back
/// onto the wire — or a classified [`WireRunError`] on failure. `params` is the optional cost/cash
/// override applied to EVERY grid point (`None` = engine defaults), threaded through [`to_engine_params`]
/// — the SAME mapping `run_slice_local` uses. Both the server arm (`vike_backtest::compute_server`) and the parity test
/// drive THIS function, so "run locally" and "run over the wire" are, by construction, the same
/// computation.
pub fn run_sweep_local(
    spec: &WireSpec,
    slice: &WireSlice,
    sweep: &WireParamscan,
    params: Option<&WireEngineParams>,
    store: StoreHandle,
) -> Result<WireParamscanResult, WireRunError> {
    let strat_spec = to_strategy_spec(spec).map_err(|e| run_error_to_wire(&e))?;
    let data_slice = to_data_slice(slice);
    // Own the wire params so the per-point factory closure is `Send + Sync` (the bounded parallel
    // sweep pool requires it); each point rebuilds a fresh `EngineParams` from the same knobs.
    let params = params.cloned();
    let result =
        run_paramscan_slice_with_params(&strat_spec, &data_slice, &store, &sweep.axes, || {
            to_engine_params(params.as_ref())
        });
    match result {
        Ok(result) => Ok(to_wire_paramscan_result(&result)),
        Err(e) => Err(run_error_to_wire(&e)),
    }
}

/// The full local `RunWalkforward` computation: convert the DTOs, run
/// `crate::run_walkforward_slice_with_params` over `store`, and mirror the stitched report
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

/// The three STUDIO runners, packed into the table `vike-backend backtest --addr` mounts.
///
/// ⚠ **This is the seam ruling 7 needed, and the reason it points UP.** The compute daemon's server
/// lives in `vike-backtest` (layer 50) because `backtest` is that crate's verb; the runners it has
/// to call live here (layer 55) because they are this crate's. Layer 50 may not name layer 55 — and
/// not merely by declaration: THIS crate depends on `vike-backtest`, so the edge could never point
/// the other way without a cycle. So the daemon declares a table whose type names only the `Wire*`
/// DTOs (`vike_backtest::compute_server`'s `StudioRunTable`), and the composition root — which sits
/// above BOTH — fills it by calling this function. It is the identical shape
/// `vike_datahub::backfill`'s `real_backfill_table` uses to keep venue collectors out of a data
/// server's default tree, applied to a layer problem instead of a weight one.
///
/// Each entry IS the `*_local` function above it, handed over as a function item rather than wrapped
/// in a forwarding closure — the same entry this module's parity tests drive, so "run locally" and
/// "run over the wire" remain the same computation by construction rather than by review (the S2
/// parity guarantee, unchanged by the move).
pub fn studio_run_table() -> vike_backtest::compute_server::StudioRunTable {
    vike_backtest::compute_server::StudioRunTable::new(
        Box::new(run_slice_local),
        Box::new(run_sweep_local),
        Box::new(run_walkforward_local),
    )
}

/// The STUDY runner the COMPUTE daemon serves `RunStudy` with — the research-plane sibling of
/// [`studio_run_table`], and built HERE for the same reason: `vike-backtest` declares the seam and
/// cannot fill it, because this crate is above it in the layer graph.
///
/// `runs_root` and `learner_bin` are the DAEMON's configuration, resolved by the rung that already
/// walks for that daemon's settings (`vike_backtest::backtest_cli`'s `--addr` arm) and captured
/// here. ⚠ NOT by the multicall dispatcher: `crates/vike-ops/tests/multicall_gate.rs`'s
/// `the_dispatcher_starts_nothing` forbids it a `state_path::` call, so the root passes THIS
/// FUNCTION ITEM as a `vike_backtest::compute_server::StudyRunFactory` and the daemon supplies the
/// paths. They are deliberately not wire fields — both name paths on the backend's box, and
/// `crates/vike-cli/src/cmd/study.rs`'s `parse` refuses `--store`/`--lightgbm` BY NAME for exactly
/// that reason.
///
/// ⚠ **No `DataFusionHist` and no `study-cli` feature.** The daemon hands in an already-open
/// `StoreHandle`, which is what lets this live in an UNGATED module without putting a datafusion
/// crate in this crate's default normal-dep tree — the property `scripts/ci_feature_suite.sh`'s
/// `studio-standalone` lane greps for. The window grammar and the `[learner]` reader are
/// `crate::study_dispatch`'s (MOVED there out of the gated `study_cli`), so the two doors cannot
/// answer differently about what `--from 1785906000` means.
pub fn study_run_fn(
    runs_root: std::path::PathBuf,
    learner_bin: Option<std::path::PathBuf>,
) -> vike_backtest::compute_server::StudyRunFn {
    Box::new(move |w: &vike_datahub_client::proto::WireStudy, store: StoreHandle| {
        let params: toml::Value = toml::from_str(&w.recipe_toml)
            .map_err(|e| format!("recipe TOML did not parse: {e}"))?;
        let (Some(from_ms), Some(to_ms)) = (
            crate::study_dispatch::boundary_ms(&w.from),
            crate::study_dispatch::boundary_ms(&w.to),
        ) else {
            return Err("--from/--to must be YYYY-MM-DD, YYYY-MM-DDTHH or unix seconds".to_string());
        };
        if to_ms <= from_ms {
            return Err("--to must be after --from".to_string());
        }

        // Beside the runs root, NEVER the system temp directory — the rule
        // `crates/vike-ops/tests/system_temp_gate.rs` and `temp_path_gate.rs` both carry, and the
        // same expression `study_cli` uses.
        let scratch = runs_root
            .parent()
            .map_or_else(|| std::path::PathBuf::from("scratch"), |p| p.join("scratch"))
            .join("vike-study");

        // Absent is a WORKING state: the study refuses BY NAME (`StudyError::NoLearner`) and a run
        // made with no learner is a different experiment rather than a failed one.
        let learner: Option<Arc<dyn vike_user_research::StudyLearner>> = match &learner_bin {
            None => None,
            Some(bin) => {
                let (base, _from_recipe) = crate::study_dispatch::learner_params(&params);
                Some(Arc::new(
                    vike_ml::GbdtLearner::new(bin, &scratch, base).map_err(|e| e.to_string())?,
                ))
            }
        };

        let plan = crate::StudyRunPlan {
            name: w.study.clone(),
            dir: std::path::PathBuf::from("user_data/research/studies/rust").join(&w.study),
            tier: crate::listing::StudyTier::Rust,
            params,
            // ⚠ No PATH: the recipe arrived as TEXT and this daemon never saw a file. `name` is the
            // study, which is the most a run on the far side of a wire can honestly record about
            // where its configuration came from.
            config: vike_model::runs::RunConfig { path: None, name: Some(w.study.clone()) },
            store,
            window: TsRange::of(from_ms, to_ms),
            scratch,
            runs_root: runs_root.clone(),
            produced_by: "vike-backend backtest".to_string(),
            git_sha: None,
            learner,
        };

        let run = crate::run_study_plan(plan, &vike_model::LiveClock).map_err(|e| e.to_string())?;
        // ⚠ METRICS GO THROUGH `metric_json`, never through a raw f64. `serde_json` renders NaN and
        // ±inf as `null`, which silently loses exactly the tag `study_run::nonfinite_tag` exists to
        // carry — and a study's whole output is metrics.
        let metrics: Vec<serde_json::Value> = run
            .outcome
            .metrics()
            .iter()
            .map(|(name, value)| crate::study_run::metric_json(name, *value))
            .collect();
        serde_json::to_string(&serde_json::json!({
            "run_id": run.run_id,
            "dir": run.dir.display().to_string(),
            "manifest": run.manifest,
            "metrics": metrics,
        }))
        .map_err(|e| format!("study report serialize failed: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // The PRODUCTION source of every `chosen_params` value: a `[sweep]` grid expanded into points.
    // Reached through the harness rather than by hand because this crate cannot name `toml::Value`
    // at all — no `toml` dependency, the same fact that put the rendering on the wire.
    use vike_backtest::harness::{BacktestProfile, expand_paramscan};

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

    /// The fixture the `chosen_params` rendering test expands: a ONE-point `[sweep]` grid whose
    /// four axes carry the four TOML scalar shapes a real grid produces. Built through the
    /// PRODUCTION path (`harness::sweep::expand_paramscan`, which is where every `chosen_params` value
    /// comes from) rather than by hand, because this crate cannot name `toml::Value` — no `toml`
    /// dependency, the same fact that put the rendering on the wire to begin with.
    const CHOSEN_PARAMS_PROFILE: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0

[sweep]
flag = [true]
label = ["fast-lane"]
size = [1.5]
slow = [30]
"#;

    /// A FIXED-parameter window — every window this verb serves today — mirrors with no choice
    /// recorded, and the two scalar fields still copy bit-for-bit.
    #[test]
    fn to_wire_wf_window_carries_a_fixed_window_with_no_choice() {
        let w = WfWindow { test_range: (10, 20), oos_return: 0.25, chosen_params: None };
        let wire = to_wire_wf_window(&w);
        assert_eq!(wire.test_range, (10, 20));
        assert_eq!(wire.oos_return.to_bits(), 0.25f64.to_bits());
        assert_eq!(wire.chosen_params, None, "a fixed-parameter walk records no choice");
    }

    /// An OPTIMIZED window's choice crosses as rendered TOML text, one pair per swept axis, in
    /// `expand_paramscan`'s key-sorted order.
    ///
    /// The renderings are PINNED verbatim (`true` / `"fast-lane"` / `1.5` / `30`) because they ARE
    /// the wire bytes: a change to any of them changes what `vike_studio::remote`'s `to_wf_window`
    /// parses back. The string case is the one worth watching — its TOML quoting is the whole
    /// difference between a rendered `String` and a rendered bare value on the way back.
    #[test]
    fn to_wire_wf_window_renders_each_chosen_value_as_toml_text() {
        let profile =
            BacktestProfile::from_toml_str(CHOSEN_PARAMS_PROFILE).expect("fixture profile parses");
        let points = expand_paramscan(&profile).expect("the one-point grid expands");
        assert_eq!(points.len(), 1, "every axis holds one value, so the grid is one point");

        let w = WfWindow {
            test_range: (0, 50),
            oos_return: -0.01,
            chosen_params: Some(points[0].overrides.clone()),
        };
        assert_eq!(
            to_wire_wf_window(&w).chosen_params,
            Some(vec![
                ("flag".to_string(), "true".to_string()),
                ("label".to_string(), "\"fast-lane\"".to_string()),
                ("size".to_string(), "1.5".to_string()),
                ("slow".to_string(), "30".to_string()),
            ])
        );
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
