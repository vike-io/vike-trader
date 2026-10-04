//! The Studio compute-to-data BOUNDARY: convert the wire DTOs into this crate's own run types, run
//! the EXISTING `run_slice`/`run_paramscan_slice_with_params`/`run_walkforward_slice_with_params` next to
//! the data, and mirror the result back onto the wire.
//!
//! ⚠ **This module MOVED here from `vike-datahub` (ruling 7 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`), and the move is a
//! consequence of the LAYER RULE rather than a preference.** The three verbs it serves —
//! `RunSlice`, `RunSweep`, `RunWalkforward` — left the data daemon for
//! `vike-backend backtest --addr`, whose server lives in `vike-backtest` (layer 30). That crate
//! cannot name `vike-studio-core` (layer 35): the dependency direction is down-only and this crate
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

use vike_backtest::harness::RankMetric;
use vike_backtest::harness::walkforward::WindowSearch;
use vike_backtest::walkforward::{WalkForwardReport, WfWindow};
use vike_data::{HistStore, TsRange};
use vike_sim::EngineParams;

use crate::cost_model::{CostModel, FillMix, NOT_MODELLED};
use crate::{
    DataSlice, ParamscanEntry, RunError, SliceKind, StrategySpec, StudioParamscan,
    WindowSearchPlan, run_paramscan_slice_with_params, run_slice, run_walkforward_slice_searching,
};

use vike_datahub_client::wire_studio::{
    WireCostModel, WireEngineParams, WireParamscan, WireParamscanResult, WireRunError,
    WireRunResult, WireSlice, WireSliceKind, WireSpec, WireWalkforward, WireWalkforwardResult,
};
use vike_datahub_client::{WireParamscanEntry, WireWfWindow};

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
            // ⚠ **The arm LABELLED native reached the COMPILER, and this is that hole closed.**
            // `docs/decisions/0064-a-named-run-carries-no-source.md` recorded the finding and named
            // this exact function: the arm checked neither the NAME nor the PARAMS, and
            // `vike_backtest::harness::registry::strategy_by_name` is a `&str` lookup whose `"rhai"`
            // arm compiles a `src` param. So `Native { name: "rhai", params_toml: "src = '…'" }`
            // — a spec whose own variant says *native* — compiled arbitrary Rhai sent over the wire.
            //
            // Both halves are needed and neither is redundant: the NAME is what selects the
            // compiling arm, and the PARAMS are what it compiles. Refusing only the name leaves a
            // future registry arm that reads `src` equally reachable; refusing only `src` leaves
            // `"rhai"` free to grow a second payload key.
            //
            // ⚠ This is a CHECK, not the structural fence, and 0064 says why a check is the weaker
            // answer — *"a refactor can move it, a future arm can sidestep it, a reviewer can pass
            // over it"*. The structural fence it prefers (a crate that CANNOT name `vike-script`,
            // held by `crates/vike-ops/tests/layer_gate.rs`) is the named-run path's, and is not
            // available here: this crate reaches the registry legitimately for every real native
            // strategy. So the check is pinned by tests that assert the REFUSAL rather than the
            // parse, and this comment is the reason a reader may not delete it as belt-and-braces.
            const COMPILING_NAME: &str = "rhai";
            const SOURCE_KEY: &str = "src";

            if name.trim() == COMPILING_NAME {
                return Err(RunError::Strategy(format!(
                    "a `native` spec may not name `{COMPILING_NAME}`: that registry arm COMPILES \
                     its `{SOURCE_KEY}` parameter, so the arm labelled native would be a source \
                     door. Send source as `WireSpec::Rhai`, which is the arm that says what it is \
                     and carries the scope that goes with it."
                )));
            }
            let spec = StrategySpec::native_from_toml_str(name.clone(), params_toml)
                .map_err(|e| RunError::Strategy(format!("params TOML parse failed: {e}")))?;
            if params_carry_source(params_toml) {
                return Err(RunError::Strategy(format!(
                    "a `native` spec's params may not carry `{SOURCE_KEY}`: it is the key the \
                     compiling registry arm reads, and a native strategy has no use for it. Send \
                     source as `WireSpec::Rhai`."
                )));
            }
            Ok(spec)
        }
        // ⚠ **No name/`src` guard here, and that absence is deliberate rather than an oversight
        // copied past.** The Native arm above exists because `name` is a lookup key into
        // `vike_backtest::harness::registry::strategy_by_name`, whose `match` also holds the
        // COMPILING `"rhai"` arm — so a native-labelled name can reach the compiler if the check is
        // skipped (0064's decision 5 finding). A `WireSpec::Plugin`'s `name` is never looked up in
        // that registry at all: it identifies which loaded plugin's `create` to call, a resolution
        // that goes through `vike_strategy_plugin::loader::load` (Task 4) and never touches
        // `strategy_by_name`. So there is no compiling arm this name could alias into, and a `src`
        // key in `params_toml` is inert here for the same reason it is inert on the Named-run
        // door — nothing on this path reads it.
        //
        // The scope question this raises — does carrying a Plugin spec change what credential may
        // send `RunSlice`/`RunSweep`/`RunWalkforward`? — is answered at
        // `crates/vike-backtest/src/compute_server.rs`'s module doc, not here: that file's `Scope`
        // is per-VERB (via `vike_datahub_client::proto::required_scope`), not per-`WireSpec`-variant,
        // so this arm's own scope is not a decision this function makes.
        WireSpec::Plugin { name, sha, params_toml } => {
            StrategySpec::plugin_from_toml_str(name.clone(), sha.clone(), params_toml)
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

/// **[`to_engine_params`] plus the COST MODEL the slice implies** — the ONE place a Studio DTO run
/// decides what it is priced under, and the reason all three `*_local` entries below reach it.
///
/// Ruling: `docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`.
///
/// It is [`to_engine_params`] with one addition — `CostModel::resolve` over the slice's own `venue`
/// and `symbols`, applied to the params it just built. The PRECEDENCE is
/// `crate::cost_model::CostModel::resolve`'s and is stated there in full; what matters here is the
/// consequence: a caller who sent `params.fee_rate` gets that flat rate and NO derived schedule,
/// and a caller who sent none gets the lane's real schedule instead of `EngineParams::default`'s
/// ZERO.
///
/// ⚠ **Before this existed, every Studio run over this door was a zero-cost backtest and nothing
/// said so.** That is the first thing the search arm had to fix, because a search RANKS candidates
/// and the perturbation a cost model applies is per-candidate — proportional to that candidate's
/// own turnover and maker/taker mix — so it can reorder a grid rather than merely scaling it.
///
/// The returned [`CostModel`] is what [`to_wire_cost_model`] stamps onto the answer, so the stamp
/// is COMPUTED from the value the run used rather than restated beside it.
pub fn engine_params_for(
    slice: &WireSlice,
    params: Option<&WireEngineParams>,
) -> (EngineParams, CostModel) {
    let mut ep = to_engine_params(params);
    let model = CostModel::resolve(&slice.venue, &slice.symbols, params.and_then(|p| p.fee_rate));
    model.apply(&mut ep);
    (ep, model)
}

/// `CostModel` + the REALISED mix + the ranking (when anything was ranked) → the wire stamp.
///
/// The ONE builder, so the three verbs below cannot describe their cost models differently, and
/// every field is read off the values the run actually used — `0063`'s "computed, never restated".
/// [`NOT_MODELLED`] rides along unconditionally: a stamp that said only what IS modelled would
/// invite the reader to infer that nothing else exists.
pub fn to_wire_cost_model(
    model: &CostModel,
    mix: FillMix,
    rank_metric: Option<&str>,
) -> WireCostModel {
    WireCostModel {
        source: model.source_token().to_string(),
        lane: model.lane().map(str::to_string),
        variant: model.variant().to_string(),
        maker_rate: model.maker_rate,
        taker_rate: model.taker_rate,
        reason: model.reason().map(str::to_string),
        maker_fills: mix.maker_fills,
        taker_fills: mix.taker_fills,
        fees_paid: mix.fees_paid,
        rank_metric: rank_metric.map(str::to_string),
        not_modelled: NOT_MODELLED.iter().map(|s| s.to_string()).collect(),
    }
}

// ⚠ `to_wire_result` MOVED to `vike_backtest::wire_result` on 2026-09-16 and is NOT re-exported
// here. The NAMED RUN (`docs/decisions/0064-a-named-run-carries-no-source.md`) answers with the
// same `WireRunResult` from `vike_backtest::compute_server`, which sits at layer 30 and cannot name
// this crate at 35 — the same seam `StudioRunTable` exists for. So the rendering went DOWN to the
// crate that owns `BacktestResult`, per the workspace's rule that when two sides must not disagree
// the cure is a shared crate BELOW both. A second copy would be two renderings of one result, and
// the first divergence would be a Studio and a named run reporting different equity curves for the
// same run.
//
// No `pub use` shim: every call site names the canonical path, which is this workspace's stated
// convention for a symbol that changes homes.

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
    let (engine_params, cost_model) = engine_params_for(slice, params);
    match run_slice(&strat_spec, &data_slice, &store, engine_params) {
        Ok(result) => {
            let mut wire = vike_backtest::wire_result::to_wire_result(&result);
            // No ranking happened — a single run chose nothing, so the metric field stays empty
            // rather than naming a default nobody applied.
            wire.cost_model = Some(to_wire_cost_model(&cost_model, FillMix::of(&result), None));
            Ok(wire)
        }
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

/// `ParamscanEntry` → [`WireParamscanEntry`]: the point's overrides plus the rendered `vike_backtest::wire_result::to_wire_result` of
/// its `BacktestResult`.
fn to_wire_paramscan_entry(e: &ParamscanEntry) -> WireParamscanEntry {
    WireParamscanEntry {
        overrides: e.overrides.clone(),
        result: vike_backtest::wire_result::to_wire_result(&e.result),
    }
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
/// degrading. Nothing on this path constructs one: on the PROFILE door `chosen_params` values come
/// from `harness::sweep::expand_paramscan`, i.e. they were parsed OUT of a `[sweep]` array and are
/// re-renderable by construction; on THIS door every value is a `toml::Value::Float` built by
/// `crate::run`'s searching driver out of a wire axis, which is an `f64` and renders
/// unconditionally.
///
/// ⚠ This paragraph used to end "this verb's server arm cannot produce a `Some` at all yet". It can
/// now — that is the search arm (`0063`), and a fixed walk still records nothing.
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
        // Stamped by `run_sweep_local`, which is the only caller that knows what the run was priced
        // under (this function sees a ranked result, not the params that produced it).
        cost_model: None,
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
        // Stamped by `run_walkforward_local` — the report carries no record of what priced it.
        cost_model: None,
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
    let owned_params = params.cloned();
    // ⚠ Resolved ONCE, outside the factory, and then APPLIED per point. Every candidate is
    // therefore priced under one model by construction rather than by three functions agreeing —
    // which matters more here than on the single-run verb, because these candidates are RANKED
    // against each other and a cost model that differed between them would be choosing the winner.
    let cost_model = CostModel::resolve(
        &slice.venue,
        &slice.symbols,
        owned_params.as_ref().and_then(|p| p.fee_rate),
    );
    let factory_model = cost_model.clone();
    let result =
        run_paramscan_slice_with_params(&strat_spec, &data_slice, &store, &sweep.axes, || {
            let mut ep = to_engine_params(owned_params.as_ref());
            factory_model.apply(&mut ep);
            ep
        });
    match result {
        Ok(result) => {
            // The realised mix sums EVERY point, because every point was run and the question the
            // mix answers — did the maker side ever engage — is about the sweep, not about its
            // winner alone.
            let mut mix = FillMix::default();
            for entry in &result.entries {
                mix.add(&entry.result);
            }
            let mut wire = to_wire_paramscan_result(&result);
            // `run_paramscan_slice_with_params` ranks by annualized Sharpe and takes no metric
            // argument (`rank_entries_by_sharpe`), so the stamp names the metric the code uses
            // rather than one the caller could have chosen.
            wire.cost_model = Some(to_wire_cost_model(&cost_model, mix, Some("sharpe")));
            Ok(wire)
        }
        Err(e) => Err(run_error_to_wire(&e)),
    }
}

/// The full local `RunWalkforward` computation: convert the DTOs, walk the slice forward over
/// `store` — SEARCHING inside each window when the request asked for it — and mirror the stitched
/// report back onto the wire, or a classified [`WireRunError`] on failure. `params` is the optional
/// cost/cash override applied to every OOS window, threaded through [`engine_params_for`] so an
/// absent `fee_rate` DERIVES the slice's own fee schedule instead of leaving the run at zero. The
/// SAME entry the server arm and the parity test drive, keeping "local" and "over the wire" one
/// computation — and therefore keeping the cost-model stamp one answer.
///
/// # The SERVER-SIDE re-check, and why it is not the client's refusal wearing a second hat
///
/// The selector is capability-negotiated: a client whose peer does not advertise
/// `vike_datahub_client::FEATURE_WALKFORWARD_SEARCH` refuses locally, without sending
/// (`DatahubClient::run_walkforward`), because a daemon predating the field would DROP it and
/// answer a normal FIXED-walk report. That refusal answers *your daemon is too old*.
///
/// This function answers a different question — *that is not a search I have* — and it answers it
/// by PARSING rather than by falling through: an unrecognised `method`, or an unrecognised
/// `rank_by`, is a named [`WireRunError`], never a silent demotion to the fixed walk. Neither
/// refusal is reachable by the other's route: a new client against a new daemon never trips the
/// first, and a typo in the method string never trips it either.
///
/// ⚠ A `"sweep"` method with an EMPTY grid is refused one layer down, by
/// `crate::run_walkforward_slice_searching` — which is the right place for it, because the same
/// hazard exists for an in-process caller who never touched a wire.
pub fn run_walkforward_local(
    spec: &WireSpec,
    slice: &WireSlice,
    walkforward: &WireWalkforward,
    params: Option<&WireEngineParams>,
    store: StoreHandle,
) -> Result<WireWalkforwardResult, WireRunError> {
    let strat_spec = to_strategy_spec(spec).map_err(|e| run_error_to_wire(&e))?;
    let data_slice = to_data_slice(slice);
    let plan = to_window_search_plan(walkforward)?;
    // The metric is named on the stamp ONLY when a search actually ranked something — naming a
    // default that chose nothing would be the report claiming a provenance it does not have.
    let ranked_by = plan.search.searches().then(|| plan.rank.name());
    let owned_params = params.cloned();
    // Resolved ONCE and applied per window/candidate — see `run_sweep_local`'s note on why the
    // single resolution matters most where candidates are ranked against each other.
    let cost_model = CostModel::resolve(
        &slice.venue,
        &slice.symbols,
        owned_params.as_ref().and_then(|p| p.fee_rate),
    );
    let factory_model = cost_model.clone();
    let result = run_walkforward_slice_searching(
        &strat_spec,
        &data_slice,
        &store,
        walkforward.n_splits,
        plan,
        || {
            let mut ep = to_engine_params(owned_params.as_ref());
            factory_model.apply(&mut ep);
            ep
        },
    );
    match result {
        Ok((report, mix)) => {
            let mut wire = to_wire_walkforward_result(&report);
            wire.cost_model = Some(to_wire_cost_model(&cost_model, mix, ranked_by));
            Ok(wire)
        }
        Err(e) => Err(run_error_to_wire(&e)),
    }
}

/// [`WireWalkforward`]'s optional selector → the driver's [`WindowSearchPlan`] — the SERVER-SIDE
/// parse, and the whole of the re-check [`run_walkforward_local`]'s doc describes.
///
/// Both strings go through the ENGINE's own parsers rather than a second `match` here
/// (`WindowSearch::from_str_ci`, `RankMetric::from_str_ci`), which is the same rule the profile
/// door follows for `[walkforward].rank_by`: one spelling authority, so this door and that one
/// cannot drift apart on what `"sweep"` or `"max_dd"` mean. What is local is only the MESSAGE,
/// because the key a reader must edit differs between the two doors.
///
/// An absent selector is the FIXED walk — byte-identical to every frame this verb carried before
/// the field existed.
fn to_window_search_plan(
    walkforward: &WireWalkforward,
) -> Result<WindowSearchPlan<'_>, WireRunError> {
    let Some(selector) = walkforward.search.as_ref() else {
        return Ok(WindowSearchPlan::none());
    };
    let search = WindowSearch::from_str_ci(&selector.method).ok_or_else(|| {
        WireRunError::new(
            "data",
            format!(
                "unknown walk-forward search method {:?} (want none | sweep; absent = none, the \
                 no-search control). It is refused rather than run as the fixed walk, because a \
                 report that silently walked FIXED parameters answers a different question from \
                 the one asked.",
                selector.method
            ),
        )
    })?;
    let rank = match selector.rank_by.as_deref() {
        None => RankMetric::default(),
        Some(raw) => RankMetric::from_str_ci(raw.trim()).ok_or_else(|| {
            WireRunError::new(
                "data",
                format!(
                    "unknown walk-forward rank_by {raw:?} (want sharpe | return | max_dd | \
                     equity; absent = sharpe)"
                ),
            )
        })?,
    };
    Ok(WindowSearchPlan { search, grid: &selector.grid.axes, rank })
}

/// The three STUDIO runners, packed into the table `vike-backend backtest --addr` mounts.
///
/// ⚠ **This is the seam ruling 7 needed, and the reason it points UP.** The compute daemon's server
/// lives in `vike-backtest` (layer 30) because `backtest` is that crate's verb; the runners it has
/// to call live here (layer 35) because they are this crate's. Layer 30 may not name layer 35 — and
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
    Box::new(move |w: &vike_datahub_client::WireStudy, store: StoreHandle| {
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

#[path = "wire_run_tests.rs"]
#[cfg(test)]
mod wire_run_tests;

/// Does this params text carry the key the COMPILING registry arm reads?
///
/// ⚠ Parsed, not grepped. A substring search for `src` matches `srcs`, a symbol called `"src"`, and
/// any comment — and misses `[params]\nsrc = "…"` spelled with a quoted key or odd whitespace. The
/// question is *does the TABLE have this key*, so the table is what gets asked. Text that will not
/// parse answers `false` here and is refused by the parse itself one line up.
fn params_carry_source(params_toml: &str) -> bool {
    toml::from_str::<toml::Value>(params_toml)
        .ok()
        .and_then(|v| v.as_table().map(|t| t.contains_key("src")))
        .unwrap_or(false)
}

#[path = "native_arm_is_not_a_source_door.rs"]
#[cfg(test)]
mod native_arm_is_not_a_source_door;
