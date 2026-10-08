//! The profile-shaped verbs: run, search or walk a wire profile, or answer its data plan instead.

use std::sync::Arc;

use vike_analytics::report::BacktestReport;
use vike_datahub_client::WireSearch;
#[cfg(doc)]
use vike_datahub_client::proto::Request;
use vike_datahub_client::proto::Response;

#[cfg(doc)]
use super::StudyRunFn;
use super::{REMOTE_STORE_LABEL, StoreHandle};
use crate::data_plan;
use crate::harness::{self, BacktestProfile};
use crate::search::select;

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
/// `crate::data_plan::DataPlan::to_json`): this side has the plan and the far side has no renderer
/// for it.
///
/// # ⚠ It is the FIRST thing all three profile arms do, and nothing computes
///
/// Called immediately after `from_toml_str` in each of them, so a planning request never builds an
/// evaluator, never compiles a Rhai strategy and never touches the engine. The profile has still
/// been VALIDATED by then, deliberately: a plan for a profile that could not run is a plan for
/// nothing.
fn explain_instead_of_running(profile: &BacktestProfile, store: &StoreHandle) -> Option<Response> {
    if !profile.data.explain {
        return None;
    }
    let plan = match data_plan::plan_data(profile, store.as_ref(), REMOTE_STORE_LABEL, None) {
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

/// Parse the wire profile TOML, run it over `store`, and return the [`BacktestReport`] as JSON text.
///
/// `BacktestProfile::from_toml_str` parses AND validates (bad range, empty slice, cross-venue snap,
/// …) in one step — the exact guard the file path applies — so a malformed profile becomes a clean
/// [`Response::Error`] before the engine is touched. A run failure (missing strategy, data error,
/// resolution build error) or a report-serialize failure likewise becomes `Response::Error`, so the
/// caller always learns the outcome.
pub(super) fn run_backtest(profile_toml: &str, store: &StoreHandle) -> Response {
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
    };
    if let Some(plan) = explain_instead_of_running(&profile, store) {
        return plan;
    }
    let result = match harness::run_backtest(&profile, Arc::clone(store)) {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    // Stamped on the same terms as the local door (`crates/vike-backtest/src/backtest_cli.rs`'s
    // single-run arm): a `--local` run and an `--addr` run must not answer differently about what
    // the fills cost, and the profile is right here.
    let report = BacktestReport::from_result(
        profile.name.clone(),
        &result,
        harness::report::periods_per_year(&profile),
    )
    .with_realism(harness::report::realism_stamp(&profile));
    match serde_json::to_string(&report) {
        Ok(json) => Response::Report(json),
        Err(e) => Response::Error(format!("report serialize failed: {e}")),
    }
}

/// Parse the wire profile TOML, run its `[paramscan]` grid over `store` with the SELECTED search
/// method, and return the RANKED [`harness::ParamscanReport`] as JSON text (v7).
///
/// One authority for the profile, exactly like [`run_backtest`]: `BacktestProfile::from_toml_str`
/// parses AND validates — the same guard the file path applies — so a remote search and a local
/// `backtest --profile … --rank-by …` are the same computation over the same `[engine]` (fee
/// schedule included).
///
/// The run is `harness::optimize` over the evaluator `select::evaluator_for` chose. ⚠ For
/// grid + a classic metric that resolves to `StoreEvaluator::classic` + `GridSearch` — **the
/// identical call `harness::run_paramscan_exec` makes**, which is what keeps
/// `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
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
/// A search's METHOD has no profile home and can never get one: `BacktestProfile` is
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
pub(super) fn run_paramscan_profile(
    profile_toml: &str,
    rank_by: Option<&str>,
    search: Option<&WireSearch>,
    store: &StoreHandle,
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
    let empty = WireSearch::default();
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
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
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
    // ⚠ `objective` is declared BEFORE `eval`: `StoreEvaluator::new` borrows it for the evaluator's
    // whole life and locals drop in reverse declaration order.
    let (objective, label) = select::objective_for(rank);
    let exec = harness::ParamscanExec::from_env();
    let eval = match select::evaluator_for(
        method,
        rank,
        &profile,
        Arc::clone(store),
        &objective,
        label,
        exec,
    ) {
        Ok(e) => e,
        Err(e) => return Response::Error(e.to_string()),
    };
    // Bound, not inlined: `optimizer_for` returns a `Box<dyn Optimizer>` whose borrow must outlive
    // the call.
    let opt = select::optimizer_for(method);
    let report = match harness::optimize(opt.as_ref(), &profile, &eval) {
        Ok(o) => o.report,
        Err(e) => return Response::Error(e.to_string()),
    };
    match serde_json::to_string(&report) {
        Ok(json) => Response::ParamscanReport(json),
        Err(e) => Response::Error(format!("sweep report serialize failed: {e}")),
    }
}

/// The refusal [`Request::RunStudy`] gets when no [`StudyRunFn`] is mounted — the study sibling of
/// [`studio_verb_unmounted`], and a BUILD/composition fact rather than a request fact.
pub(super) fn study_verb_unmounted() -> Response {
    Response::Error(
        "RunStudy is served only by a build that mounts the study runner, and this one did not. \
         The shipped `vike-backend backtest --addr` mounts it; a bare `cargo run -p vike-backtest \
         --bin backtest` cannot, because that runner lives in `vike-studio-core`, which sits ABOVE \
         this crate in the layer graph and can only be handed in by a composition root. Run the \
         study on the box that holds the store instead: `vike-backend study`"
            .to_string(),
    )
}

/// Parse the wire profile TOML and walk it forward over its `[walkforward]` out-of-sample windows,
/// returning the stitched `WalkForwardReport` as JSON text (v7). Same one-parser contract as
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
/// `harness::WalkforwardCfg::window_search`.
///
/// **There is deliberately no wire field for it**, and [`Request::RunWalkforwardProfile`] carries
/// only `profile_toml` precisely so there cannot be one: a second place to say "optimize" is a
/// second place for the two to disagree, and the disagreement would be invisible in the answer
/// (both protocols return a `WalkForwardReport`, and the fixed walk's windows simply carry no
/// `chosen_params`). The `[walkforward]` table is the authority for `mode` and `rank_by` for the
/// same reason; this arm reads neither, because the driver it hands off to reads both.
///
/// ⚠ The control is routed to `run_walkforward` ITSELF rather than to the optimizing driver's
/// `WindowSearch::None` arm. That is a choice of ROUTE, not of answer:
/// `crates/vike-backtest/src/walkforward/runner_tests.rs`'s
/// `the_control_reproduces_the_fixed_parameter_walk_exactly` pins the two bit-identical. It is
/// routed this way so the byte-identity gate over THIS verb —
/// `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
/// `profile_walkforward_is_byte_identical_local_and_remote`, which compares this answer against an
/// in-process `harness::run_walkforward` — keeps comparing the same call, rather than resting on a
/// bit-identity that another crate proves and only in its `datafusion-store` lane.
///
/// ⚠ Declared gap: that gate covers the CONTROL only. Nothing in this crate's tests yet ships a
/// `search = "sweep"` profile over the wire, so the SEARCHED route is proven at the harness level
/// and by inspection here, not end-to-end through a socket. It is named rather than implied because
/// a green run of the existing suite says nothing about it.
pub(super) fn run_walkforward_profile(profile_toml: &str, store: &StoreHandle) -> Response {
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
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
        Some(cfg) => match cfg.window_search() {
            Ok(s) => s,
            // Unreachable through `from_toml_str` — `BacktestProfile::validate` resolved this same
            // string at load — but spelled as a clean error rather than an `expect`, because this
            // runs on a connection thread where a panic costs the peer its answer.
            Err(e) => return Response::Error(e.to_string()),
        },
        None => crate::walkforward::runner::WindowSearch::None,
    };
    // Exhaustive, no `_` arm, for the reason [`required_scope`] gives about verbs: a new search
    // mode must be routed by whoever adds it, not inherited from whichever side a wildcard picked.
    let walk = match search {
        crate::walkforward::runner::WindowSearch::None => {
            harness::run_walkforward(&profile, Arc::clone(store))
        }
        crate::walkforward::runner::WindowSearch::Sweep => {
            crate::walkforward::runner::run_walkforward_optimized(&profile, Arc::clone(store))
        }
    };
    let report = match walk {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    match serde_json::to_string(&report) {
        Ok(json) => Response::WalkforwardReport(json),
        Err(e) => Response::Error(format!("walk-forward report serialize failed: {e}")),
    }
}
