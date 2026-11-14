//! The Studio verbs' SERVER entries — `RunSlice`, `RunSweep`, `RunWalkforward` and `RunStudy` as the
//! backtesthub serves them: decode the DTOs (`vike_studio_core::wire_run`'s server halves), run the
//! slice's PROFILE (`super`'s pipeline: a Studio run is a profile run) next to the data, and mirror the
//! answer back onto the wire, stamped with what the profile charged ([`to_wire_cost_model`]).
//!
//! `crate::backtesthub_server` and `tests/studio_wire_parity.rs` drive THESE functions, so "run
//! locally" and "run over the wire" are, by construction, the same computation (S2).
//!
//! ⚠ **They were `vike_studio_core::wire_run`'s until 2026-11-10** and moved here, beside the server
//! that calls them, because they run the harness and that crate is what the desktop links: the thin
//! client links no engine (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s
//! amendment of that day). The conversions they call stayed there, each beside its client half.

use std::sync::Arc;

use vike_backtest::harness::{ProfileBacktest, RankMetric};
use vike_backtest::walkforward::runner::run_walkforward_optimized;
use vike_backtesthub_client::wire_studio::{
    BacktestResultWire, CostModelStampWire, DataSliceWire, ParamscanWire, StrategyEngineConfigWire,
    StrategySpecWire, StudioParamscanWire, WalkforwardReportWire, WalkforwardWire,
};
use vike_data::HistoryStore;
use vike_data::TimestampRange;
use vike_model::FeeSchedule;
use vike_studio_core::RunError;
use vike_studio_core::cost_model::{FillMix, NOT_MODELLED, WALKFORWARD_FILL_MIX};
use vike_studio_core::wire_run::{
    to_data_slice, to_strategy_spec, to_wire_paramscan_result, to_wire_walkforward_result,
};

use super::{
    StrategyFiles, data_error, paramscan_profile, run_slice_profile, slice_profile,
    walkforward_profile,
};

/// **The profile that ran + the REALISED mix + the ranking (when anything was ranked) → the wire
/// stamp** (`docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`).
///
/// The ONE builder, so the three verbs below cannot describe their cost models differently, and
/// every field is read off what the run used: the profile's own `[engine.fee]` table, resolved by
/// the same `ProfileEngineFee::build_for` / `ProfileEngineFee::per_series` calls the harness charged with — never a
/// second fee-lane derivation.
///
/// - `"derived"`: no `fee_rate` was sent, so the profile states `kind = "venue"` (each symbol at its
///   own fee lane); `lane` names the lanes, `variant` the schedule's shape (`"PerSymbol"` when the
///   slice straddles lanes, and then the rates are zero because no one pair is the run's).
/// - `"override"`: a sent `fee_rate`, flat (a zero is the explicit `kind = "free"`).
///
/// [`NOT_MODELLED`] rides along unconditionally: a stamp that said only what IS modelled would
/// invite the reader to infer that nothing else exists.
pub fn to_wire_cost_model(
    profile: &ProfileBacktest,
    mix: FillMix,
    rank_metric: Option<&str>,
) -> CostModelStampWire {
    let engine = &profile.engine;
    let derived = engine.fee.as_ref().filter(|fee| fee.kind == "venue");
    let (source, lane, variant, maker_rate, taker_rate) = match derived {
        Some(fee) => {
            let series = profile.data.resolved_series();
            let kept = &profile.kept.rows;
            let mut lanes: Vec<&str> = series
                .iter()
                .map(|s| vike_backtest::harness::profile::kept_lane(&s.venue, &s.symbol, kept))
                .collect();
            lanes.sort_unstable();
            lanes.dedup();
            let schedule = fee.build_for(&series, kept).ok();
            let (variant, (maker, taker)) = if fee.per_series(&series, kept).is_empty() {
                (variant_name(schedule), schedule.map_or((0.0, 0.0), |s| s.maker_taker_rates()))
            } else {
                ("PerSymbol", (0.0, 0.0))
            };
            ("derived", Some(lanes.join(", ")), variant, maker, taker)
        }
        None => ("override", None, "none", engine.fee_rate, engine.fee_rate),
    };
    CostModelStampWire {
        source: source.to_string(),
        lane,
        variant: variant.to_string(),
        maker_rate,
        taker_rate,
        reason: None,
        maker_fills: mix.maker_fills,
        taker_fills: mix.taker_fills,
        fees_paid: mix.fees_paid,
        rank_metric: rank_metric.map(str::to_string),
        not_modelled: NOT_MODELLED.iter().map(|s| s.to_string()).collect(),
    }
}

/// A schedule's VARIANT name, `"none"` for no schedule.
///
/// ⚠ **Load-bearing, not a decoration.** Several lanes resolve to shapes that report a ZERO pair
/// through `maker_taker_rates` (the per-share-with-floor equities shape and the prediction-market
/// curve), so without the variant "maker 0, taker 0" reads as free trading. An exhaustive match on
/// purpose: a variant added to `FeeSchedule` reddens this line rather than falling through to a
/// label that would lie.
fn variant_name(schedule: Option<FeeSchedule>) -> &'static str {
    match schedule {
        None => "none",
        Some(FeeSchedule::PercentMakerTaker { .. }) => "PercentMakerTaker",
        Some(FeeSchedule::PerShareWithFloor { .. }) => "PerShareWithFloor",
        Some(FeeSchedule::PercentOfUnderlying { .. }) => "PercentOfUnderlying",
        Some(FeeSchedule::BpsWithFloor { .. }) => "BpsWithFloor",
        Some(FeeSchedule::ProbabilityScaled { .. }) => "ProbabilityScaled",
        Some(FeeSchedule::Free) => "Free",
    }
}

/// The full local `RunSlice` computation: convert the DTOs, run the slice's profile over `store`
/// (`super::run_slice`'s two halves), and mirror the result back onto the wire — or the classified
/// [`RunError`] on failure, which the server arm sends as [`RunError::wire_line`]. Both the server
/// arm (`crate::backtesthub_server`) and the parity test drive THIS function, so "run locally" and
/// "run over the wire" are, by construction, the same computation (S2).
pub fn run_slice_local(
    spec: &StrategySpecWire,
    files: &StrategyFiles,
    slice: &DataSliceWire,
    params: Option<&StrategyEngineConfigWire>,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<BacktestResultWire, RunError> {
    let strategy_spec = to_strategy_spec(spec)?;
    let profile =
        slice_profile(&strategy_spec, files, &to_data_slice(slice), params, store.as_ref())?;
    let result = run_slice_profile(&profile, &store)?;
    let mut wire = vike_backtesthub_client::wire_result::to_wire_result(&result);
    // The file the run ran and its hash, on the top-level answer as the stamp is.
    wire.strategy_file = profile.strategy.file.clone();
    wire.strategy_sha256 = profile.strategy.sha256().to_string();
    // No ranking happened — a single run chose nothing, so the metric field stays empty rather
    // than naming a default nobody applied.
    wire.cost_model = Some(to_wire_cost_model(&profile, FillMix::of(&result), None));
    Ok(wire)
}

/// The full local `RunSweep` computation: convert the DTOs, search the slice's profile over its
/// grid (`super::run_paramscan_slice_with_params`'s body), and mirror the ranked result back onto
/// the wire — or the classified [`RunError`] on failure. `params` prices EVERY grid point, one
/// profile for all, so no cost model can differ between candidates ranked against each other. Both
/// the server arm (`crate::backtesthub_server`) and the parity test drive THIS function,
/// so "run locally" and "run over the wire" are, by construction, the same computation.
pub fn run_paramscan_local(
    spec: &StrategySpecWire,
    files: &StrategyFiles,
    slice: &DataSliceWire,
    sweep: &ParamscanWire,
    params: Option<&StrategyEngineConfigWire>,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<StudioParamscanWire, RunError> {
    let strategy_spec = to_strategy_spec(spec)?;
    let profile =
        slice_profile(&strategy_spec, files, &to_data_slice(slice), params, store.as_ref())?;
    let result = paramscan_profile(profile.clone(), &sweep.axes, &store)?;
    // The realised mix sums EVERY point, because every point was run and the question the mix
    // answers — did the maker side ever engage — is about the sweep, not about its winner alone.
    let mut mix = FillMix::default();
    for entry in &result.entries {
        mix.add(&entry.result);
    }
    let mut wire = to_wire_paramscan_result(&result);
    // The sweep ranks by Sharpe and takes no metric argument, so the stamp names the metric the
    // code uses rather than one the caller could have chosen.
    wire.cost_model = Some(to_wire_cost_model(&profile, mix, Some(RankMetric::Sharpe.name())));
    Ok(wire)
}

/// The full local `RunWalkforward` computation: convert the DTOs, walk the slice's profile forward
/// over `store` (`super::run_walkforward_slice_searching`'s body) — SEARCHING inside each window
/// when the request asked for it — and mirror the stitched report back onto the wire, or a
/// classified [`RunError`] on failure. The SAME entry the server arm and the parity test drive,
/// keeping "local" and "over the wire" one computation — and therefore the stamp one answer.
///
/// # The SERVER-SIDE re-check, and why it is not the client's refusal wearing a second hat
///
/// The selector is capability-negotiated: a client whose peer does not advertise
/// `vike_backtesthub_client::proto::FEATURE_WALKFORWARD_SEARCH` refuses locally, without sending
/// (`BacktesthubClient::run_walkforward`), because a daemon predating the field would DROP it and
/// answer a normal FIXED-walk report. That refusal answers *your daemon is too old*.
///
/// This function answers a different question — *that is not a search I have* — by handing the
/// selector to the profile's own `[walkforward]` validation: an unrecognised `method` or `rank_by`,
/// or a `"sweep"` with no axes, is the profile's named refusal, never a silent demotion to the
/// fixed walk. An absent selector is the FIXED walk.
pub fn run_walkforward_local(
    spec: &StrategySpecWire,
    files: &StrategyFiles,
    slice: &DataSliceWire,
    walkforward: &WalkforwardWire,
    params: Option<&StrategyEngineConfigWire>,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<WalkforwardReportWire, RunError> {
    let strategy_spec = to_strategy_spec(spec)?;
    let profile =
        slice_profile(&strategy_spec, files, &to_data_slice(slice), params, store.as_ref())?;
    let profile = walkforward_profile(profile, walkforward.n_splits, walkforward.search.as_ref())?;
    // The metric is named on the stamp ONLY when a search actually ranked something — naming a
    // default that chose nothing would be the report claiming a provenance it does not have.
    let ranked_by = profile.walkforward.as_ref().and_then(|config| {
        let searches = config.window_search().is_ok_and(|s| s.searches());
        searches.then(|| config.rank_metric().unwrap_or_default().name())
    });
    let mut report = run_walkforward_optimized(&profile, store.clone()).map_err(data_error)?;
    report.strategy_file = profile.strategy.file.clone();
    report.strategy_sha256 = profile.strategy.sha256().to_string();
    let mut wire = to_wire_walkforward_result(&report);
    // The profile walk counts no fills per window, so the mix is not measured here and says so.
    let mut stamp = to_wire_cost_model(&profile, FillMix::default(), ranked_by);
    stamp.not_modelled.push(WALKFORWARD_FILL_MIX.to_string());
    wire.cost_model = Some(stamp);
    Ok(wire)
}

/// The STUDY runner the backtesthub serves `RunStudy` with — the research-plane sibling of
/// [`run_slice_local`], [`run_paramscan_local`] and [`run_walkforward_local`], and called DIRECTLY by
/// `crate::backtesthub_server` like them.
///
/// `runs_root` and `learner_bin` are the DAEMON's configuration (`vike_backtesthub`'s `StudyDirs`),
/// resolved by the rung that already walks for that daemon's settings. ⚠ NOT by the multicall
/// dispatcher: `crates/vike-ops/tests/container_deploy/multicall_gate.rs`'s
/// `the_dispatcher_starts_nothing` forbids it a `state_path::` call. They are deliberately not wire
/// fields — both name paths on the backend's box, and `crates/vike-cli/src/cmd/study.rs`'s `parse`
/// refuses `--store`/`--lightgbm` BY NAME for exactly that reason.
///
/// ⚠ **No `HistoryStoreDataFusion`.** The daemon hands in an already-open `Arc<dyn HistoryStore + Send + Sync>`; this crate's normal
/// tree holds no store (`ci.yml`'s `datahub-all` guard (K)). The window grammar and the `[learner]`
/// reader are `vike_studio_core::study_dispatch`'s, shared with `crate::study_cli`, so the two doors
/// cannot answer differently about what `--from 1785906000` means.
pub fn run_study_local(
    w: &vike_backtesthub_client::proto::StudyRunRequestWire,
    store: Arc<dyn HistoryStore + Send + Sync>,
    runs_root: &std::path::Path,
    learner_bin: Option<&std::path::Path>,
) -> Result<String, String> {
    let params: toml::Value =
        toml::from_str(&w.recipe_toml).map_err(|e| format!("recipe TOML did not parse: {e}"))?;
    let (Some(from_ms), Some(to_ms)) = (
        vike_studio_core::study_dispatch::boundary_ms(&w.from),
        vike_studio_core::study_dispatch::boundary_ms(&w.to),
    ) else {
        return Err("--from/--to must be YYYY-MM-DD, YYYY-MM-DDTHH or unix seconds".to_string());
    };
    if to_ms <= from_ms {
        return Err("--to must be after --from".to_string());
    }

    let scratch = vike_studio_core::study_dispatch::default_study_scratch(runs_root);

    // Absent is a WORKING state: the study refuses BY NAME (`StudyError::NoLearner`) and a run
    // made with no learner is a different experiment rather than a failed one.
    let learner: Option<Arc<dyn vike_user_research::StudyLearner>> = match learner_bin {
        None => None,
        Some(bin) => {
            let (base, _from_recipe) = vike_studio_core::study_dispatch::learner_params(&params);
            Some(Arc::new(
                vike_ml::GbdtLearner::new(bin, &scratch, base).map_err(|e| e.to_string())?,
            ))
        }
    };

    let plan = vike_studio_core::StudyRunPlan {
        name: w.study.clone(),
        dir: vike_studio_core::study_dispatch::compiled_study_dir(&w.study),
        tier: vike_studio_core::listing::StudyTier::Rust,
        params,
        // ⚠ No PATH: the recipe arrived as TEXT and this daemon never saw a file. `name` is the
        // study, which is the most a run on the far side of a wire can honestly record about
        // where its configuration came from.
        config: vike_runs::RunConfig { path: None, name: Some(w.study.clone()) },
        store,
        window: TimestampRange::of(from_ms, to_ms),
        scratch,
        runs_root: runs_root.to_path_buf(),
        produced_by: "vike-backend backtesthub".to_string(),
        git_sha: None,
        learner,
        // The daemon is a host that CAN run one backtest for a study: the harness is in this
        // closure, so a study asking `ctx.sim()` gets it here (it gets none in the desktop).
        sim: Some(Arc::new(crate::harness_sim::HarnessSim::default())),
    };

    let run = vike_studio_core::run_study_plan(plan, &vike_model::ClockLive)
        .map_err(|e| e.to_string())?;
    // ⚠ METRICS GO THROUGH `StudyMetric`, never through a raw f64. `serde_json` renders NaN and
    // ±inf as `null`, which silently loses exactly the tag `study_run::nonfinite_tag` exists to
    // carry — and a study's whole output is metrics. Each also carries its `text`, the ONE printed
    // form (`StudyMetric::text`), so `vike-cli study run` prints what the Studio and `vike-study` do.
    let metrics: Vec<serde_json::Value> = run
        .outcome
        .metrics()
        .iter()
        .map(|(name, value)| {
            let m = vike_studio_core::study_run::StudyMetric::of(name, *value);
            serde_json::json!({
                "name": m.name,
                "value": m.value,
                "nonfinite": m.nonfinite,
                "text": m.text(),
            })
        })
        .collect();
    serde_json::to_string(&serde_json::json!({
        "run_id": run.run_id,
        "dir": run.dir.display().to_string(),
        "manifest": run.manifest,
        "metrics": metrics,
    }))
    .map_err(|e| format!("study report serialize failed: {e}"))
}

#[path = "wire_tests.rs"]
#[cfg(test)]
mod wire_tests;
