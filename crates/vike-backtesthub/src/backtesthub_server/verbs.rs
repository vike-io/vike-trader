//! The profile-shaped verbs: run, search or walk a wire profile, or answer its data plan instead.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_analytics::report::BacktestReport;
#[cfg(doc)]
use vike_backtesthub_client::proto::Request;
use vike_backtesthub_client::proto::Response;
use vike_backtesthub_client::proto::SearchSelectionWire;
use vike_data::HistoryStore;

use super::REMOTE_STORE_LABEL;
use crate::studio_run::StrategyFiles;
use vike_backtest::data_plan;
use vike_backtest::harness::{self, ProfileBacktest};
use vike_backtest::search::select;

/// Parse and validate a profile that arrived OVER THE WIRE: `ProfileBacktest::from_toml_str`, the
/// same guard the file path applies, then the one refusal only this door owes.
///
/// ⚠ **A wire profile may not make this daemon open a file.** It has no profile directory
/// (`ProfileBacktest::base_dir` is `None`), so `[engine.resolution] path` would name anything on this
/// box, from an MCP session scoped `read-only` too (its roster serves the `run_*` tools):
/// `/dev/zero` reads until the daemon is out of memory, and the `{path}: {error}` answer says whether
/// a file exists. So the key is refused here, before the first read and in one sentence whatever
/// the file. Inline `[engine.resolution.winners]` crosses as it
/// always did, and a local run (`ProfileBacktest::from_path`, which does not come through here) keeps
/// its sidecar file.
///
/// `data.file` (a CSV a run reads without importing it, decision 0268) is refused on the same terms:
/// the file read is for a profile run where the file lives, and a client's data reaches this daemon
/// through an import into a lane (`data.source`), which crosses as a word.
///
/// Those are the ONLY keys of the released `profile.json` a run opens as a file. Of the other string
/// keys, `data.venue`/`symbols`/`series` and `engine.fee.venue`/`symbol` are store or table names
/// (history reads go through the datahub, decision 0084), and the rest are words and times.
/// `strategy.params` is read by strategies that open nothing, and `strategy.file` names a file of this
/// daemon's own `strategies/` folder only, read as the strategy's text (a Rhai script runs with
/// `import` refused: `crates/vike-rhai/src/engine/host.rs`'s `set_module_resolver` note).
/// A new key that names a file is refused here too.
fn wire_profile(
    profile_toml: &str,
    files: &StrategyFiles,
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> Result<ProfileBacktest, String> {
    let mut profile = ProfileBacktest::from_toml_str_in(profile_toml, &files.dir)
        .map_err(|e| format!("profile parse/validate failed: {e}"))?;
    // Its strategy FILE, read under this daemon's own `strategies/` folder and nowhere else: the
    // path is refused absolute or climbing out, and a link is not followed (`ProfileStrategy::read_file`).
    files.prepare(&mut profile.strategy).map_err(|e| format!("profile refused: {e}"))?;
    if profile.engine.resolution.as_ref().is_some_and(|r| r.path.is_some()) {
        return Err("profile refused: `[engine.resolution] path` names a file on this daemon's \
                    box, and a profile sent to it may not open one. List the winners under \
                    `[engine.resolution.winners]`, or run the profile file where it lives: \
                    `vike-cli backtest run --local --profile <file>`."
            .to_string());
    }
    if profile.data.file.is_some() {
        return Err(
            "profile refused: `data.file` names a file on this daemon's box, and a profile \
                    sent to it may not open one. Import the file (`csv-bars`) and read its lane \
                    with `data.source`, or run the profile where the file lives: `vike-cli \
                    backtest run --local --profile <file>`."
                .to_string(),
        );
    }
    // The KEPT instrument rows the run reads its per-symbol facts from, asked of the datahub once here (a
    // sandboxed child's piped store answers the rows its parent asked for: `crate::strategy_sandbox`).
    profile.read_kept_rows(store.as_ref());
    Ok(profile)
}

/// **The `data.explain` door on the REMOTE route**: `Some(response)` when the profile asked for a
/// PLAN instead of a run, `None` when it did not.
///
/// # Why it needs no wire verb of its own, and what that buys
///
/// `data.explain` is a PROFILE key, and only the profile text crosses the wire — so the plan is
/// reachable on this route with no [`Request`] variant, no protocol version bump and no client
/// change. [`Response::Report`] already carries JSON text, and
/// `crates/vike-cli/src/cmd/backtest/execute.rs`'s `Route::Single` arm already pretty-prints whatever JSON
/// came back, so an operator typing `vike-cli backtest run --explain-data` against a remote daemon
/// gets the plan on stdout and exit 0. The alternative — a dedicated verb — would have put the
/// answer behind a protocol bump and a client that has to know the shape, for a document the client
/// never interprets.
///
/// That is also why the document carries its own rendered SENTENCES (see
/// `vike_backtest::data_plan::DataPlan::to_json`): this side has the plan and the far side has no renderer
/// for it.
///
/// # ⚠ It is the FIRST thing all three profile arms do, and nothing computes
///
/// Called immediately after `from_toml_str` in each of them, so a planning request never builds an
/// evaluator and never touches the engine. It BUILDS the strategy once, only to ask which series it
/// reads by name (`vike_backtest::data_plan::declared_inputs`, decision 0271), and runs nothing of it.
/// The profile has still been VALIDATED by then, deliberately: a plan for a profile that could not
/// run is a plan for nothing, and a strategy that cannot be built is answered as the run would answer
/// it.
fn explain_instead_of_running(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> Option<Response> {
    if !profile.data.explain {
        return None;
    }
    // The series the strategy reads by name (decision 0271): the strategy is BUILT to say which.
    let inputs = match data_plan::declared_inputs(profile) {
        Ok(i) => i,
        Err(e) => return Some(Response::Error(e.to_string())),
    };
    let plan =
        match data_plan::plan_data(profile, &inputs, store.as_ref(), REMOTE_STORE_LABEL, None) {
            Ok(p) => p,
            Err(e) => return Some(Response::Error(e.to_string())),
        };
    let doc = match data_plan::explain_document(profile, &plan) {
        Ok(d) => d,
        Err(e) => return Some(Response::Error(e.to_string())),
    };
    match serde_json::to_string(&doc) {
        Ok(json) => Some(Response::Report(json)),
        Err(e) => Some(Response::Error(format!("data plan serialize failed: {e}"))),
    }
}

/// `Some(answer)` when the connection's peer-gone flag is set: the [`Response::Error`] a watched
/// verb answers instead of its report, `cancelled: the client closed its connection, so {stopped}
/// and no report was built`. Read AFTER the work and BEFORE its result, whichever way the work went:
/// an `Err` from a run whose points or windows were cancelled is the cancellation's doing, not the
/// profile's — and a report over half-skipped work would rank, or stitch, whatever happened to
/// finish.
///
/// `None` (nobody watching, or a client still there) leaves the verb's answer exactly what it was
/// before the watch existed.
fn cancelled_answer(client_gone: Option<&AtomicBool>, stopped: &str) -> Option<Response> {
    harness::run::is_cancelled(client_gone).then(|| {
        Response::Error(format!(
            "cancelled: the client closed its connection, so {stopped} and no report was built"
        ))
    })
}

/// Parse the wire profile TOML, run it over `store`, and return the [`BacktestReport`] as JSON text.
///
/// `ProfileBacktest::from_toml_str` parses AND validates (bad range, empty slice, cross-venue snap,
/// …) in one step — the exact guard the file path applies — so a malformed profile becomes a clean
/// [`Response::Error`] before the engine is touched. A run failure (missing strategy, data error,
/// resolution build error) or a report-serialize failure likewise becomes `Response::Error`, so the
/// caller always learns the outcome.
///
/// `client_gone` (the connection's peer-gone flag, `super::connection`'s `PeerWatch`) reaches
/// `harness::run::run_backtest_cancellable`, which reads it at the run's stage boundaries — per
/// series load and before the engine — and a set flag answers [`cancelled_answer`]'s sentence. A run
/// already inside the engine finishes first; its doc says why.
///
/// # ⚠ A profile that declares a `[paramscan]` grid is REFUSED here
///
/// The symmetric twin of [`run_paramscan_profile`]'s "profile has no [paramscan] table" refusal.
/// This verb runs ONE point, so a grid profile sent to it used to come back as a single backtest
/// over `[strategy.params]` with nothing in the answer saying the grid was ignored. The local
/// engine (`crates/vike-backtesthub/src/backtest_cli.rs`'s `run`) branches on the same
/// `ProfileBacktest::is_paramscan` and searches; `vike-cli backtest` routes such a profile to
/// [`Request::RunParamscanProfile`] before it gets here. The refusal is what keeps every OTHER
/// client of this verb (the MCP `run_backtest` tool) from receiving one point as if it were the
/// answer. Like its twin it comes AFTER the `data.explain` door: a plan is still a plan.
pub(super) fn run_backtest(
    profile_toml: &str,
    files: &StrategyFiles,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    client_gone: Option<&AtomicBool>,
) -> Response {
    let profile = match wire_profile(profile_toml, files, store) {
        Ok(p) => p,
        Err(refused) => return Response::Error(refused),
    };
    if let Some(plan) = explain_instead_of_running(&profile, store) {
        return plan;
    }
    if profile.is_paramscan() {
        return Response::Error(
            "profile has a [paramscan] table — a single backtest runs ONE point and would ignore \
             the grid; run it as a parameter search (RunParamscanProfile: `vike-cli backtest` \
             routes it there, the MCP tool is `run_sweep`), or drop the [paramscan] table"
                .to_string(),
        );
    }
    let outcome = harness::run::run_backtest_cancellable(&profile, Arc::clone(store), client_gone);
    if let Some(cancelled) = cancelled_answer(client_gone, "the run stopped at its next stage") {
        return cancelled;
    }
    let result = match outcome {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    // Stamped on the same terms as the local door (`crates/vike-backtesthub/src/backtest_cli.rs`'s
    // single-run arm): a `--local` run and an `--addr` run must not answer differently about what
    // the fills cost, and the profile is right here.
    let report = BacktestReport::from_result(
        profile.name.clone(),
        &result,
        harness::report::periods_per_year(&profile),
    )
    .with_realism(harness::report::realism_stamp_of_run(&profile, &result))
    .with_strategy(&profile.strategy.file, profile.strategy.sha256());
    match serde_json::to_string(&report) {
        Ok(json) => Response::Report(json),
        Err(e) => Response::Error(format!("report serialize failed: {e}")),
    }
}

/// Parse the wire profile TOML, run its `[paramscan]` grid over `store` with the SELECTED search
/// method, and return the RANKED [`harness::ParamscanReport`] as JSON text (v7).
///
/// One authority for the profile, exactly like [`run_backtest`]: `ProfileBacktest::from_toml_str`
/// parses AND validates — the same guard the file path applies — so a remote search and a local
/// `backtest --profile … --rank-by …` are the same computation over the same `[engine]` (fee
/// schedule included).
///
/// The run is `harness::optimize` over the evaluator `select::evaluator_for` chose. ⚠ For
/// grid + a classic metric that resolves to `SearchEvaluatorStore::classic` + `SearchGrid` — **the
/// identical call `harness::run_paramscan_execution` makes**, which is what keeps
/// `crates/vike-backtesthub/tests/backtesthub_profile_roundtrip.rs`'s
/// `profile_sweep_is_byte_identical_local_and_remote` green. Routing everything through the uniform
/// objective path instead would add a `score` key to every row and change `rank_by`'s STRING for
/// three of the four metrics; `select::uses_classic_evaluator` carries that argument.
///
/// THE SELECTOR IS SHARED with the engine binary's argv parser (`search::select`), so the
/// ownership rule, the three value parsers and every refusal sentence exist ONCE: a remote
/// `--optimizer grid --trials 8` is refused with the sentence a `--local` one is refused with.
/// RANKING HAPPENS HERE, over the real `BacktestReport`s — so no client re-implements
/// sharpe/return/max_dd — and an unrecognized `rank_by` is a clean [`Response::Error`] naming the
/// valid set, never a silent fallback.
///
/// # The contrast with [`run_walkforward_profile`] one function down
///
/// A search's METHOD has no profile home and can never get one: `ProfileBacktest` is
/// `deny_unknown_fields`, so a top-level `optimizer = "tpe"` is a hard parse error, and the
/// parameter-grid table's every key is an AXIS — `[paramscan].method = "tpe"` declares an axis
/// named `method`. There is exactly one place it can live, and that is
/// [`Request::RunParamscanProfile`]'s `search` field.
///
/// Walk-forward's shape is different, and its own doc argues it where it lives: that verb carries
/// only `profile_toml` so there cannot be a second place to say "optimize". What a walked-forward
/// SEARCH should name, and where, is ruling R7's question (walk-forward is a MODIFIER over a run,
/// not a third run kind) for the stage that owns that routing. **Stage 7 widens ONE verb and states
/// that boundary rather than pretending the other verb's shape is settled.**
///
/// # `client_gone`: the search stops when nobody is waiting for it
///
/// `Some(flag)` arms `SearchEvaluatorStore::with_cancel` with the connection's peer-gone flag
/// (`super::connection`'s `PeerWatch` sets it when the client's socket closes). Once set, the
/// remaining points are skipped and the answer is a [`Response::Error`] naming the cancellation —
/// never a report, because a report over half-skipped rows would rank the points that happened to
/// finish. A search the flag never touched answers exactly as it did before the flag existed.
pub(super) fn run_paramscan_profile(
    profile_toml: &str,
    files: &StrategyFiles,
    rank_by: Option<&str>,
    search: Option<&SearchSelectionWire>,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    client_gone: Option<&AtomicBool>,
) -> Response {
    // ⚠ ONE selector, shared with the engine binary's argv parser — `search::select`. The
    // ownership rule (`--trials` is tpe-or-genetic, `--euler-depth` euler, `--seed` both stochastic
    // methods), the value parsers and their refusal texts are that module's, so a remote
    // `--optimizer grid --trials 8` reads exactly like a `--local` one. Duplicating the table here
    // is the two-rosters defect stage 7 exists to kill.
    let rank = match select::resolve_rank(rank_by) {
        Ok(r) => r,
        Err(e) => return Response::Error(e),
    };
    let empty = SearchSelectionWire::default();
    let w = search.unwrap_or(&empty);
    let method = match select::resolve(&select::SearchSelection {
        optimizer: w.optimizer.as_deref(),
        euler_depth: w.euler_depth.as_deref(),
        trials: w.trials.as_deref(),
        seed: w.seed.as_deref(),
    }) {
        Ok(m) => m,
        Err(e) => return Response::Error(e),
    };
    let profile = match wire_profile(profile_toml, files, store) {
        Ok(p) => p,
        Err(refused) => return Response::Error(refused),
    };
    // ⚠ BEFORE the grid check, deliberately: a search's data SLICE is the same for every trial, so
    // a coverage problem here is a problem with the whole search — and the plan is exactly what an
    // operator wants before spending a grid on it. The plan's own notes say the `[paramscan]` table
    // is present.
    if let Some(plan) = explain_instead_of_running(&profile, store) {
        return plan;
    }
    if !profile.is_paramscan() {
        return Response::Error(
            "profile has no [paramscan] table — a parameter search needs a grid, e.g. \
             `[paramscan]\nfast = [5, 10, 15]`"
                .to_string(),
        );
    }
    // ⚠ `objective` is declared BEFORE `evaluation`: `SearchEvaluatorStore::new` borrows it for the evaluator's
    // whole life and locals drop in reverse declaration order.
    let (objective, label) = select::objective_for(rank);
    let exec = harness::ParamscanExecution::from_env();
    let evaluation =
        select::evaluator_for(method, rank, &profile, Arc::clone(store), &objective, label, exec);
    let evaluation = match client_gone {
        Some(flag) => evaluation.with_cancel(flag),
        None => evaluation,
    };
    // Bound, not inlined: `optimizer_for` returns a `Box<dyn Optimizer>` whose borrow must outlive
    // the call.
    let opt = select::optimizer_for(method);
    let outcome = harness::optimize(opt.as_ref(), &profile, &evaluation);
    // ⚠ Read AFTER the search and BEFORE its result — `cancelled_answer`'s doc says why.
    if let Some(cancelled) = cancelled_answer(client_gone, "the search stopped scheduling points") {
        return cancelled;
    }
    let report = match outcome {
        Ok(o) => o.report,
        Err(e) => return Response::Error(e.to_string()),
    };
    match serde_json::to_string(&report) {
        Ok(json) => Response::ParamscanReport(json),
        Err(e) => Response::Error(format!("paramscan report serialize failed: {e}")),
    }
}

/// Parse the wire profile TOML and walk it forward over its `[walkforward]` out-of-sample windows,
/// returning the stitched `WalkforwardReport` as JSON text (v7). Same one-parser contract as
/// [`run_backtest`]; every failure (missing `[walkforward]`, tick/multi-symbol profile, empty
/// slice) is a clean [`Response::Error`].
///
/// # The verb now serves TWO walk-forward protocols, and the PROFILE picks — not the wire
///
/// `harness::run_walkforward` walks FIXED parameters (were these settings stable out of sample?);
/// `walkforward::runner::run_walkforward_optimized` lets each window search its own TRAINING half
/// and carry only that window's winner onto its validation half (does the procedure of fit-then-
/// trade survive out of sample?). Those are different questions with the same report type, so
/// which one ran is a fact an operator must be able to establish — and the only thing that says it
/// is `[walkforward].search`, resolved here through the profile's own
/// `harness::ProfileWalkforward::window_search`.
///
/// **There is deliberately no wire field for it**, and [`Request::RunWalkforwardProfile`] carries
/// only `profile_toml` precisely so there cannot be one: a second place to say "optimize" is a
/// second place for the two to disagree, and the disagreement would be invisible in the answer
/// (both protocols return a `WalkforwardReport`, and the fixed walk's windows simply carry no
/// `chosen_params`). The `[walkforward]` table is the authority for `mode` and `rank_by` for the
/// same reason; this arm reads neither, because the driver it hands off to reads both.
///
/// ⚠ The control is routed to `run_walkforward`'s OWN body (`run_walkforward_cancellable`, which
/// `run_walkforward` is with no flag) rather than to the optimizing driver's `WalkforwardWindowSearch::None`
/// arm. That is a choice of ROUTE, not of answer:
/// `crates/vike-backtest/src/walkforward/runner_tests.rs`'s
/// `the_control_reproduces_the_fixed_parameter_walk_exactly` pins the two bit-identical. It is
/// routed this way so the byte-identity gate over THIS verb —
/// `crates/vike-backtesthub/tests/backtesthub_profile_roundtrip.rs`'s
/// `profile_walkforward_is_byte_identical_local_and_remote`, which compares this answer against an
/// in-process `harness::run_walkforward` — keeps comparing the same call, rather than resting on a
/// bit-identity that another crate proves.
///
/// ⚠ Declared gap: that gate covers the CONTROL only. Nothing in this crate's tests yet ships a
/// `search = "sweep"` profile over the wire, so the SEARCHED route is proven at the harness level
/// and by inspection here, not end-to-end through a socket. It is named rather than implied because
/// a green run of the existing suite says nothing about it.
///
/// # `client_gone`, on both routes
///
/// The peer-gone flag reaches whichever driver the profile picked —
/// `walkforward::runner::run_walkforward_cancellable` reads it once per window,
/// `walkforward::runner::run_walkforward_optimized_cancellable` once per training-half point and
/// once per window — and a set flag answers [`cancelled_answer`]'s sentence instead of a stitched
/// report. With the flag clear, or `None`, both are the uncancellable drivers exactly, which is what
/// keeps the byte-identity gate above comparing the same computation.
pub(super) fn run_walkforward_profile(
    profile_toml: &str,
    files: &StrategyFiles,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    client_gone: Option<&AtomicBool>,
) -> Response {
    let profile = match wire_profile(profile_toml, files, store) {
        Ok(p) => p,
        Err(refused) => return Response::Error(refused),
    };
    // ⚠ The plan here describes the WHOLE range every window is cut from, not one window — a
    // shortfall at either end lands in the first or last window rather than spreading across all of
    // them, and the plan's notes say so. Nothing slices the plan per window, because the answer an
    // operator needs before a walk is whether the tape reaches both ends of it.
    if let Some(plan) = explain_instead_of_running(&profile, store) {
        return plan;
    }
    // Which protocol this profile asked for. An ABSENT `[walkforward]` table is NOT answered here:
    // it falls through as the control and `run_walkforward`'s own pre-flight raises it, so the
    // "profile has no [walkforward] table" message keeps one spelling in the workspace.
    let search = match profile.walkforward.as_ref() {
        Some(config) => match config.window_search() {
            Ok(s) => s,
            // Unreachable through `from_toml_str` — `ProfileBacktest::validate` resolved this same
            // string at load — but spelled as a clean error rather than an `expect`, because this
            // runs on a connection thread where a panic costs the peer its answer.
            Err(e) => return Response::Error(e.to_string()),
        },
        None => vike_backtest::walkforward::runner::WalkforwardWindowSearch::None,
    };
    // Exhaustive, no `_` arm, for the reason [`required_scope`] gives about verbs: a new search
    // mode must be routed by whoever adds it, not inherited from whichever side a wildcard picked.
    let walk = match search {
        vike_backtest::walkforward::runner::WalkforwardWindowSearch::None => {
            vike_backtest::walkforward::runner::run_walkforward_cancellable(
                &profile,
                Arc::clone(store),
                client_gone,
            )
        }
        vike_backtest::walkforward::runner::WalkforwardWindowSearch::Sweep => {
            vike_backtest::walkforward::runner::run_walkforward_optimized_cancellable(
                &profile,
                Arc::clone(store),
                client_gone,
            )
        }
    };
    if let Some(cancelled) = cancelled_answer(client_gone, "the walk stopped at its next window") {
        return cancelled;
    }
    let report = match walk {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    match serde_json::to_string(&report) {
        Ok(json) => Response::WalkforwardReport(json),
        Err(e) => Response::Error(format!("walk-forward report serialize failed: {e}")),
    }
}
