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

use vike_backtest::HarnessError;
use vike_backtest::run::profile::ProfileBacktest;
use vike_backtest::search::paramscan::RankMetric;
use vike_backtest::walkforward::runner::run_walkforward_optimized;
use vike_backtesthub_client::proto::Response;
use vike_backtesthub_client::wire_studio::{
    CostModelStampWire, DataSliceWire, ParamscanWire, StrategyEngineConfigWire, StrategySpecWire,
    StudioParamscanWire, TradingRecordWire, WalkforwardResultsWire, WalkforwardWire,
};
use vike_data::HistoryStore;
use vike_model::FeeSchedule;
use vike_studio_core::cost_model::{FillMix, NOT_MODELLED, WALKFORWARD_FILL_MIX};
use vike_studio_core::wire_run::{
    to_data_slice, to_strategy_spec, to_wire_paramscan_result, to_wire_walkforward_result,
};
use vike_studio_core::{DataSlice, RunError, StrategySpec};

use super::paramscan::sweep_table;
use super::{
    SliceRun, StrategyFiles, data_error, paramscan_profile, run_slice_profile, slice_profile,
    walkforward_profile,
};
use crate::backtesthub_server::saved_run::{StudioRun, shared_report, single_answer_of};
use crate::run_summary::WholeRecords;

/// **The profile that ran + the REALISED mix + the ranking (when anything was ranked) → the wire
/// stamp** (`docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`).
///
/// The ONE builder, so the three verbs below cannot describe their cost models differently, and
/// every field is read off what the run used: the strategy's commission, resolved by the same
/// `vike_backtest::run::costs::run_costs` the harness charged with (the owner's ruling of
/// 2026-11-18: a strategy PARAMETER, no venue table).
///
/// - `"override"`: a `commission` param the run set (a params panel row), or a sweep `axes` over a
///   commission param, whose points each paid their own: `variant` then reads `varies by point` and
///   [`CostModelStampWire::reason`] names the axis;
/// - `"strategy"`: none set, and the strategy states one (`vike_model::Strategy::stated_fee`), which is
///   what it charged;
/// - `"none"`: neither, so every fill paid no commission, said in [`CostModelStampWire::reason`].
///
/// [`NOT_MODELLED`] rides along unconditionally: a stamp that said only what IS modelled would
/// invite the reader to infer that nothing else exists.
pub fn to_wire_cost_model<'a>(
    profile: &ProfileBacktest,
    axes: impl IntoIterator<Item = &'a str>,
    mix: FillMix,
    rank_metric: Option<&str>,
) -> CostModelStampWire {
    let (swept, _) = vike_backtest::run::costs::swept_cost_params(axes);
    let costs = vike_backtest::run::costs::run_costs(profile).ok();
    let schedule = costs.as_ref().and_then(|c| c.commission.engine_fee().1);
    let (source, reason) = match &costs {
        _ if !swept.is_empty() => (
            "override",
            Some(format!(
                "the commission {}: each point paid its own, summed in fees_paid",
                vike_backtest::run::costs::varies_by_point(&swept)
            )),
        ),
        Some(c) if c.commission_set => ("override", None),
        Some(_) if schedule.is_some() => ("strategy", None),
        _ => (
            "none",
            Some(
                "no commission param was set and the strategy states none: every fill paid no \
                 commission (no venue table is applied)"
                    .to_string(),
            ),
        ),
    };
    // A swept commission has no one schedule: the base's rates would describe none of the points.
    let schedule = schedule.filter(|_| swept.is_empty());
    let variant = if swept.is_empty() { variant_name(schedule) } else { "varies by point" };
    let (maker_rate, taker_rate) = schedule.map_or((0.0, 0.0), |s| s.maker_taker_rates());
    CostModelStampWire {
        source: source.to_string(),
        lane: None,
        variant: variant.to_string(),
        maker_rate,
        taker_rate,
        reason,
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
) -> Result<TradingRecordWire, RunError> {
    run_slice_leaving_run(spec, files, slice, params, store).map(|(wire, _, _)| wire)
}

/// **A Studio run's profile as TEXT**: the slice's (`ProfileBacktest::slice_text`, its strategy's params
/// included) with `tables` at its top level (a scan's `[paramscan]` grid, a walk's `[walkforward]`
/// table), what the run's saved `config.toml` holds so it runs again as that profile.
fn studio_profile_text(
    slice: &DataSlice,
    costs: Option<&StrategyEngineConfigWire>,
    profile: &ProfileBacktest,
    tables: Vec<(&str, Result<toml::Value, toml::ser::Error>)>,
) -> Result<String, HarnessError> {
    let text = ProfileBacktest::slice_text(
        &slice.venue,
        &slice.symbols,
        &slice.interval,
        slice.range,
        slice.kind,
        costs,
        profile.one_strategy()?,
    )?;
    if tables.is_empty() {
        return Ok(text);
    }
    let parse = |e: String| HarnessError::Parse(e);
    let mut doc: toml::Table = text.parse().map_err(|e: toml::de::Error| parse(e.to_string()))?;
    for (key, table) in tables {
        doc.insert(key.to_string(), table.map_err(|e| parse(e.to_string()))?);
    }
    toml::to_string(&doc).map_err(|e| parse(e.to_string()))
}

/// The run a Studio run of `spec` leaves for the backtesthub's saver: `profile_toml` and `answer` (the
/// profile verb's answer for that profile), and an editor buffer's text so it can be run again.
fn studio_run_of(
    spec: &StrategySpec,
    profile_toml: Result<String, HarnessError>,
    answer: Result<Response, serde_json::Error>,
) -> Option<StudioRun> {
    let source = match spec {
        StrategySpec::Rhai(text) | StrategySpec::RhaiWithParameters { source: text, .. } => {
            Some(text.as_str())
        }
        _ => None,
    };
    StudioRun::of(profile_toml, answer, source)
}

/// [`run_slice_local`]'s answer and the run it leaves for the backtesthub's saver
/// (`crate::backtesthub_server::saved_run::StudioRun`: the slice's profile as text, and the answer `RunBacktest`
/// gives that profile). The answer is [`run_slice_local`]'s, byte for byte; a run that cannot be written down is
/// logged and left out, never the answer. Third, the run's full record and its report, computed once for that saved
/// answer and the run's summary ([`WholeRecords`]).
pub(crate) fn run_slice_leaving_run(
    spec: &StrategySpecWire,
    files: &StrategyFiles,
    slice: &DataSliceWire,
    params: Option<&StrategyEngineConfigWire>,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<(TradingRecordWire, Option<StudioRun>, WholeRecords), RunError> {
    let strategy_spec = to_strategy_spec(spec)?;
    let data_slice = to_data_slice(slice);
    let profile = slice_profile(
        &strategy_spec,
        files,
        &data_slice,
        params,
        store.as_ref(),
        SliceRun::Single,
    )?;
    let (result, equity_log) = run_slice_profile(&profile, &store)?;
    let report = shared_report(&profile, &result);
    let studio_run = studio_run_of(
        &strategy_spec,
        studio_profile_text(&data_slice, params, &profile, Vec::new()),
        single_answer_of(&profile, &result, report.clone()).map(Response::Report),
    )
    .map(|run| StudioRun { equity_log, ..run });
    let whole = whole_answer(&profile, &result);
    Ok((whole, studio_run, WholeRecords { records: vec![result], report: Some(report) }))
}

/// **A single run's WHOLE answer**: `result` (the run of `profile`) on the wire, stamped with the strategy file it
/// ran and what it was costed under. What a Studio `RunSlice` answers, and what a summarized `RunBacktest`
/// summarizes (`crate::backtesthub_server::verbs`'s `run_backtest_whole`), so the CLI's summary and a Run's are one
/// summary of one shape.
pub(crate) fn whole_answer(
    profile: &ProfileBacktest,
    result: &vike_analytics::TradingRecord,
) -> TradingRecordWire {
    let mut wire = vike_backtesthub_client::wire_record::to_wire_record(result);
    // The file the run ran and its hash (a portfolio's over its mounts), on the top-level answer as the
    // stamp is.
    (wire.strategy_file, wire.strategy_sha256, _) = profile.strategy_identity();
    // No ranking happened — a single run chose nothing, so the metric field stays empty rather
    // than naming a default nobody applied.
    wire.cost_model = Some(to_wire_cost_model(profile, [], FillMix::of(result), None));
    wire
}

/// The full local `RunSweep` computation: convert the DTOs, search the slice's profile over its
/// grid (`super::run_paramscan_slice_with_params`'s body), and mirror the ranked result back onto
/// the wire — or the classified [`RunError`] on failure. Every grid point runs the one profile, so
/// candidates differ in cost only where an axis is a cost param (`commission`, `slippage`, their
/// units): then each point paid its own and the stamp says the commission varies by point
/// ([`to_wire_cost_model`]). Both the server arm (`crate::backtesthub_server`) and the parity test drive THIS function,
/// so "run locally" and "run over the wire" are, by construction, the same computation.
pub fn run_paramscan_local(
    spec: &StrategySpecWire,
    files: &StrategyFiles,
    slice: &DataSliceWire,
    sweep: &ParamscanWire,
    params: Option<&StrategyEngineConfigWire>,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<StudioParamscanWire, RunError> {
    run_paramscan_leaving_run(spec, files, slice, sweep, params, store).map(|(wire, _, _)| wire)
}

/// [`run_paramscan_local`]'s answer and the ONE run the scan leaves for the backtesthub's saver: the slice's
/// profile with the grid as its `[paramscan]` table, and the ranked table `RunParamscanProfile` answers that profile
/// (every point's overrides and report, the best set first). The answer is [`run_paramscan_local`]'s, byte for byte.
/// Third, every point's full record in the answer's order, for its summary ([`WholeRecords`]).
pub(crate) fn run_paramscan_leaving_run(
    spec: &StrategySpecWire,
    files: &StrategyFiles,
    slice: &DataSliceWire,
    sweep: &ParamscanWire,
    params: Option<&StrategyEngineConfigWire>,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<(StudioParamscanWire, Option<StudioRun>, WholeRecords), RunError> {
    let strategy_spec = to_strategy_spec(spec)?;
    let data_slice = to_data_slice(slice);
    let profile = slice_profile(
        &strategy_spec,
        files,
        &data_slice,
        params,
        store.as_ref(),
        SliceRun::Points,
    )?;
    let (result, table) = paramscan_profile(profile.clone(), &sweep.axes, &store)?;
    let grid = toml::Value::try_from(sweep_table(&sweep.axes));
    let studio_run = studio_run_of(
        &strategy_spec,
        studio_profile_text(&data_slice, params, &profile, vec![("paramscan", grid)]),
        serde_json::to_string(&table).map(Response::ParamscanReport),
    );
    // The realised mix sums EVERY point, because every point was run and the question the mix
    // answers — did the maker side ever engage — is about the sweep, not about its winner alone.
    let mut mix = FillMix::default();
    for entry in &result.entries {
        mix.add(&entry.result);
    }
    let mut wire = to_wire_paramscan_result(&result);
    // The sweep ranks by Sharpe and takes no metric argument, so the stamp names the metric the
    // code uses rather than one the caller could have chosen.
    let axes = sweep.axes.iter().map(|(key, _)| key.as_str());
    wire.cost_model =
        Some(to_wire_cost_model(&profile, axes, mix, Some(RankMetric::Sharpe.name())));
    let records = result.entries.into_iter().map(|entry| entry.result).collect();
    Ok((wire, studio_run, WholeRecords { records, report: None }))
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
) -> Result<WalkforwardResultsWire, RunError> {
    run_walkforward_leaving_run(spec, files, slice, walkforward, params, store)
        .map(|(wire, _)| wire)
}

/// The `[walkforward]` table a walk profile ran (`super::walkforward_profile` sets these three keys alone).
#[derive(serde::Serialize)]
struct WalkforwardTable<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    n_splits: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rank_by: Option<&'a str>,
}

/// [`run_walkforward_local`]'s answer and the run the walk leaves for the backtesthub's saver: the slice's profile
/// with its `[walkforward]` table (and a searching walk's `[paramscan]` grid), and the report
/// `RunWalkforwardProfile` answers that profile. The answer is [`run_walkforward_local`]'s, byte for byte.
pub(crate) fn run_walkforward_leaving_run(
    spec: &StrategySpecWire,
    files: &StrategyFiles,
    slice: &DataSliceWire,
    walkforward: &WalkforwardWire,
    params: Option<&StrategyEngineConfigWire>,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<(WalkforwardResultsWire, Option<StudioRun>), RunError> {
    let strategy_spec = to_strategy_spec(spec)?;
    let data_slice = to_data_slice(slice);
    let profile = slice_profile(
        &strategy_spec,
        files,
        &data_slice,
        params,
        store.as_ref(),
        SliceRun::Points,
    )?;
    let profile = walkforward_profile(profile, walkforward.n_splits, walkforward.search.as_ref())?;
    // The metric is named on the stamp ONLY when a search actually ranked something — naming a
    // default that chose nothing would be the report claiming a provenance it does not have.
    let ranked_by = profile.walkforward.as_ref().and_then(|config| {
        let searches = config.window_search().is_ok_and(|s| s.searches());
        searches.then(|| config.rank_metric().unwrap_or_default().name())
    });
    let report = run_walkforward_optimized(&profile, store.clone()).map_err(data_error)?;
    let config = profile.walkforward.as_ref();
    let walk = WalkforwardTable {
        n_splits: config.and_then(|c| c.n_splits),
        search: config.and_then(|c| c.search.as_deref()),
        rank_by: config.and_then(|c| c.rank_by.as_deref()),
    };
    let mut tables = vec![("walkforward", toml::Value::try_from(walk))];
    if let Some(grid) = &profile.paramscan {
        tables.push(("paramscan", toml::Value::try_from(grid)));
    }
    let studio_run = studio_run_of(
        &strategy_spec,
        studio_profile_text(&data_slice, params, &profile, tables),
        serde_json::to_string(&report).map(Response::WalkforwardReport),
    );
    let mut wire = to_wire_walkforward_result(&report);
    // The profile walk counts no fills per window, so the mix is not measured here and says so.
    let axes = profile.paramscan.iter().flat_map(|grid| grid.keys()).map(String::as_str);
    let mut stamp = to_wire_cost_model(&profile, axes, FillMix::default(), ranked_by);
    stamp.not_modelled.push(WALKFORWARD_FILL_MIX.to_string());
    wire.cost_model = Some(stamp);
    Ok((wire, studio_run))
}

/// The STUDY runner the backtesthub serves `RunStudy` with — the research-plane sibling of
/// [`run_slice_local`], [`run_paramscan_local`] and [`run_walkforward_local`], and called DIRECTLY by
/// `crate::backtesthub_server` like them.
///
/// `runs_root`, `learner_bin`, `neural_trainer_bin` and `strategies` (where a strategy file the study's one backtest names is
/// read) are the DAEMON's configuration (`vike_backtesthub`'s `StudyDirs`),
/// resolved by the rung that already walks for that daemon's settings. ⚠ NOT by the multicall
/// dispatcher: `crates/vike-ops/tests/container_deploy/multicall_gate.rs`'s
/// `the_dispatcher_starts_nothing` forbids it a `state_path::` call. They are deliberately not wire
/// fields — both name paths on the backend's box, and `crates/vike-cli/src/cmd/research/study.rs`'s `parse`
/// refuses `--store`/`--lightgbm` BY NAME for exactly that reason.
///
/// ⚠ **No `HistoryStoreDataFusion`.** The daemon hands in an already-open `Arc<dyn HistoryStore + Send + Sync>`; this crate's normal
/// tree holds no store (`ci.yml`'s `datahub-all` guard (K)). The window grammar and the `[learner]`
/// reader are `vike_study_runner::study_dispatch`'s, shared with `crate::study_cli`, so the two doors
/// cannot answer differently about what `--from 1785906000` means.
///
/// ⚠ **Every tier runs here since 2026-11-19** (studies run only on the server, the owner's ruling of
/// that day): the study is the one THIS server's listing finds in the request's tier
/// (`crate::study_files::listed_study`; no tier is the Rust tier, as before), a Rhai study runs IN this
/// process the way a `.rhai` strategy file does (`vike_rhai::sandboxed_engine` with the study limits: no
/// `import`, no file, an operation cap and size caps), and a Rust study FILE in its jailed child as
/// before. The recipe is the request's TEXT, or a recipe FILE of the study's folder named by
/// `recipe`; an empty `from`/`to` is unbounded on that side.
pub fn run_study_local(
    w: &vike_backtesthub_client::proto::StudyRunRequestWire,
    store: Arc<dyn HistoryStore + Send + Sync>,
    runs_root: &std::path::Path,
    learner_bin: Option<&std::path::Path>,
    neural_trainer_bin: Option<&std::path::Path>,
    strategies: &std::path::Path,
) -> Result<String, String> {
    // The cheap refusals first, before a folder is read: the recipe TEXT (when no file is named) and
    // the window.
    let text = match &w.recipe {
        None => Some(crate::study_files::recipe_text(&w.recipe_toml)?),
        Some(_) => None,
    };
    let window = crate::study_files::study_window(&w.from, &w.to)?;
    let study =
        crate::study_files::listed_study(&crate::study_sandbox::studies_root(), &w.study, w.tier)?;
    let (params, config) = match (text, &w.recipe) {
        // ⚠ No PATH: the recipe arrived as TEXT and this daemon never saw a file. `name` is the
        // study, which is the most a run on the far side of a wire can honestly record about
        // where its configuration came from.
        (Some(params), _) => {
            (params, vike_runs::RunConfig { path: None, name: Some(w.study.clone()) })
        }
        (None, file) => {
            let file = file.as_deref().unwrap_or_default();
            crate::study_files::recipe_file(&study, file, &w.recipe_toml)?
        }
    };

    let scratch = vike_study_runner::study_dispatch::default_study_scratch(runs_root);

    // Absent is a WORKING state: the study refuses BY NAME (`StudyError::NoLearner`) and a run
    // made with no learner is a different experiment rather than a failed one.
    // A recipe whose `[learner]` says `kind = "neural"` fits NETWORKS: the trainer process runs here,
    // outside the study's jail, and the study meets it only through the learner seam (decision 0292).
    let neural = vike_study_runner::study_dispatch::neural_spec(&params)?;
    let learner: Option<Arc<dyn vike_user_research::StudyLearner>> = match (neural, learner_bin) {
        (Some(spec), _) => match neural_trainer_bin {
            None => None,
            Some(bin) => {
                Some(Arc::new(vike_ml_training::NeuralLearner::new(bin, &scratch, &spec)?))
            }
        },
        (None, None) => None,
        (None, Some(bin)) => {
            let (base, _from_recipe) = vike_study_runner::study_dispatch::learner_params(&params)?;
            Some(Arc::new(
                vike_ml_training::GbdtLearner::new(bin, &scratch, base)
                    .map_err(|e| e.to_string())?,
            ))
        }
    };

    let plan = vike_study_runner::study_dispatch::StudyRunPlan {
        name: study.name,
        dir: study.dir,
        tier: study.tier,
        params,
        config,
        store,
        window,
        scratch,
        runs_root: runs_root.to_path_buf(),
        produced_by: "vike-backend backtesthub".to_string(),
        git_sha: None,
        learner,
        // The daemon is a host that CAN run one backtest for a study: the harness is in this
        // closure, so a study asking `ctx.sim()` gets it here (it gets none in the desktop). It runs in
        // this daemon, the route every backtest takes (a strategy FILE in its own sandboxed child), over
        // the store the study hands it (`crate::study_sandbox`'s module doc).
        sim: Some(Arc::new(crate::harness_sim::HarnessSim::new(strategies.to_path_buf()))),
        // A Rust study is a FILE (owner's rule, 2026-11-15): built by the strategy builder and run in
        // the jailed study process, never in this daemon. A Rhai study runs here and never calls it.
        file_runner: Some(crate::study_sandbox::runner()),
    };

    let ran = vike_study_runner::study_dispatch::run_study_plan(plan, &vike_model::ClockLive);
    let report = crate::study_files::study_report(w, ran)?;
    serde_json::to_string(&report).map_err(|e| format!("study report serialize failed: {e}"))
}

#[path = "wire_tests.rs"]
#[cfg(test)]
mod wire_tests;
