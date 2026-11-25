//! The run a backtest leaves on this daemon's disk (the owner's word of 2026-11-16, book W35: EVERY backtest is a
//! run). A profile verb's answer is saved under the daemon's runs root, as a study's run is (`RunStudy`), through
//! `vike_runs`' one writer: a SINGLE backtest (`Request::RunBacktest` answered by a [`Response::Report`]), a
//! walk-forward ([`Response::WalkforwardReport`]), a parameter scan ([`Response::ParamscanReport`]: ONE run, its
//! table of every point's overrides and report, best first), a combinatorial walk-forward
//! ([`Response::CombinatorialWalkforwardReport`]) or a permutation test ([`Response::PermutationReport`]). The run
//! holds the profile it ran as `config.toml`, the answer as `report.json`, the forecasts its strategy published as
//! `forecasts.json` (a single backtest's answer carries them under [`ANSWER_FORECASTS_KEY`], taken out here:
//! [`report_answer`]), its closed trades as `trades.json`, the manifest LAST.
//!
//! ⚠ **The trades ride the answer and NEVER reach a client** (book chapter 22: a forward test reads its backtest's
//! ledger). A single backtest's answer, from either route, carries `vike_runs::RunTrades` LAST under
//! [`ANSWER_TRADE_LEDGER_KEY`] ([`with_trade_ledger`]), because the sandboxed child that ran it can hand this process
//! nothing but the answer. `super::dispatch::handle_request` takes it off again ([`take_trade_ledger`]) before
//! anything else, saving on or off, so every client is answered today's bytes and only the saver reads it.
//!
//! ⚠ **A Studio run is saved as the profile run it IS** (`Request::RunSlice`, `RunParamscan`, `RunWalkforward`,
//! each of which the Studio wraps in `Request::Summarized`): the slice's profile as `config.toml` and the answer the
//! matching profile verb gives that profile as the documents ([`StudioRun`], [`save_studio`]). Its client is
//! answered the typed result it always was; the run leaves the [`StudioRun`] beside it (a `.rs` file's sandboxed child
//! sends it up in its own frame), and only the saver reads it. An editor buffer's run names `editor-buffer.rhai` as
//! its strategy file (`crate::studio_run::EDITOR_BUFFER_FILE`: the name the retention counts it by) and keeps the
//! buffer's text as that file in the run directory, its sha256 under `detail`, so it can be run again.
//!
//! ⚠ **A summarized single BAR run keeps its event log, not its whole curve** (decision 0078's amendment of
//! 2026-11-19): the run's [`StudioRun::equity_log`], when its log rebuilds the very curve the run is held with, or the
//! log a `.rs` file's run is held as in its curve's place (its child pinned the bars before it sent no curve:
//! `crate::run_summary::HeldLog`, the amendment of that day on what the hub holds), is
//! written as `vike_runs::EQUITY_LOG_FILE`, its recipe as the manifest's `detail.equity_log_recipe` and the recipe's
//! day stamps as `vike_runs::DAY_STAMPS_FILE` beside it ([`day_stamps_apart`]: every scan of the runs root reads every
//! manifest, and only a rebuild reads the stamps), and `crate::run_summary`'s `curve_page` rebuilds the curve on demand
//! ([`saved_recipe`], which also reads a run saved with its stamps inline under `detail.equity_log`). A tick run, and a
//! run the log does not cover (`vike_backtest::run::equity_log`'s module doc lists them), keeps
//! `vike_runs::EQUITY_CURVE_FILE` as before.
//!
//! ⚠ **A SUMMARIZED profile search is saved as its rows arrive** (decision 0078's amendment of 2026-11-19): the same
//! `paramscan` run with the same `report.json`, written row by row by [`ParamscanSave`] while the client gets each
//! point's metric table, so the whole report is never one text in this process nor one frame on a child's pipe.
//!
//! Only a data plan (`data.explain`, a [`Response::Report`] too) saves nothing. A study's one backtest is a step of
//! the study's run and asks `super::dispatch::answer_unsaved`; a process whose `StudyDirs::save_backtest_runs` is off
//! (a test server, the sandboxed child) saves nothing.
//!
//! The manifest's `kind` is `backtest` (`vike_runs::BACKTEST_RUN_KIND`, the WHAT axis), and HOW the
//! run was validated is `detail.protocol` plus the saved profile, the rule that constant's doc states.
//! So `vike-cli backtest ls` lists these runs and `vike-cli backtest diff` compares them as it does any
//! other: by their documents.
//!
//! A daemon given `config.saved_runs_per_strategy` then [`prune`]s: per strategy file it keeps that many of the newest
//! saved backtests and removes the rest, never a marked, tagged or named run and never a run of another kind
//! (`vike_runs::RunRetention`). Every saved run names its file under `detail` (a single run, a walk-forward, a scan,
//! a combinatorial walk, a permutation test), so all of them count against that file's number and none grows without
//! bound. Without the row nothing is removed.
//!
//! ⚠ **A run that may answer again keeps its answer and its reads** (decision 0078's amendment of 2026-11-20): a
//! request `super::answer_from_saved` keyed before it ran saves its key under `detail.input_key` and its kept answer as
//! `vike_runs::KEPT_ANSWER_FILE`, named under `detail.kept_answer` (`super::answer_from_saved::keep_into`, which also
//! refuses a run whose report names another strategy text than the one keyed). An IDENTICAL request is then answered
//! from it; a run saved without them is never one.
//!
//! Saving never costs the answer (`vike_runs`' rule): a run that cannot be saved is logged and the
//! answer goes back unchanged. ⚠ Nor may it cost a trading node its disk (REVIEW-LIVE-MONEY H1): a serving
//! daemon asks the optional writers' free-space floor first (`StudyDirs::disk_floor`,
//! `config.optional_writes_min_free_mib`), and below it the run is answered and NOT saved, logged at `error`. The sandboxed child never saves (it is jailed, and it answers through
//! `super::dispatch::handle_request_here`); this process saves what either route answered.

use std::io::Write;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};
use vike_analytics::TradingRecord;
use vike_analytics::results::TradingResults;
use vike_backtest::HarnessError;
use vike_backtest::run::equity_log::{EquityLog, EquityLogRecipe, curve_fnv};
use vike_backtest::run::profile::ProfileBacktest;
use vike_backtesthub_client::proto::{Request, Response};
use vike_data::BarDay;
use vike_model::Clock;
use vike_model::time::utc_rfc3339;
use vike_runs::{
    BACKTEST_RUN_KIND, MANIFEST_SCHEMA, RunConfig, RunEquityCurve, RunExtras, RunForecasts,
    RunManifest, RunTrades, create_run_dir, write_run_with,
};

/// The key a single backtest's answer carries its trade ledger under, LAST, between this daemon's two halves only:
/// [`with_trade_ledger`] appends it, [`take_trade_ledger`] takes it off before a client sees the answer. No report key
/// is spelled so, and the ledger itself names none, so its last structural occurrence is the one appended.
pub(super) const ANSWER_TRADE_LEDGER_KEY: &str = "trade_ledger";

/// `answer` (a JSON object, [`report_answer`]'s) with `ledger` appended as its LAST key, every byte before it kept, so
/// [`take_trade_ledger`] gives back exactly `answer`. An answer that is no non-empty object carries none.
pub(super) fn with_trade_ledger(
    answer: String,
    ledger: &RunTrades,
) -> Result<String, serde_json::Error> {
    match answer.strip_suffix('}') {
        Some(open) if open.len() > 1 => {
            let ledger = serde_json::to_string(ledger)?;
            Ok(format!("{open},\"{ANSWER_TRADE_LEDGER_KEY}\":{ledger}}}"))
        }
        _ => Ok(answer),
    }
}

/// THE strip: `response` as a client gets it, and the ledger text [`with_trade_ledger`] appended, when it did. Only a
/// `Report` carries one; every other answer comes back as it is.
pub(crate) fn take_trade_ledger(response: Response) -> (Response, Option<String>) {
    let Response::Report(json) = response else { return (response, None) };
    let mark = format!(",\"{ANSWER_TRADE_LEDGER_KEY}\":");
    match json.rfind(&mark) {
        Some(at) if json.ends_with('}') => {
            let ledger = json[at + mark.len()..json.len() - 1].to_string();
            let mut answer = json;
            answer.truncate(at);
            answer.push('}');
            (Response::Report(answer), Some(ledger))
        }
        _ => (Response::Report(json), None),
    }
}

/// The key a single backtest's answer carries its strategy's forecasts under (book W35), beside the report's own
/// keys and only when it published one: [`report_answer`] writes it, [`save`] takes it out into `forecasts.json`.
pub(super) const ANSWER_FORECASTS_KEY: &str = "forecasts";

/// A single backtest's answer: `report`'s JSON, its keys in the report's order, then the [`RunForecasts`]
/// document under [`ANSWER_FORECASTS_KEY`] when the strategy published any, so a run that published none
/// answers the bytes it always did.
pub(super) fn report_answer(
    report: &TradingResults,
    forecasts: Option<RunForecasts>,
) -> Result<String, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct WithForecasts<'a> {
        #[serde(flatten)]
        report: &'a TradingResults,
        forecasts: RunForecasts,
    }
    match forecasts {
        None => serde_json::to_string(report),
        Some(forecasts) => serde_json::to_string(&WithForecasts { report, forecasts }),
    }
}

/// **THE single backtest's answer**: `profile`'s report over the `result` it ran to, stamped as the local door stamps
/// it (`crates/vike-backtesthub/src/backtest_cli.rs`'s single-run arm: a `--local` run and an `--addr` run must not
/// answer differently about what the fills cost), its forecasts ([`report_answer`]) and its trade ledger LAST
/// ([`with_trade_ledger`], the engine's own bound). `RunBacktest` answers it; a Studio single run leaves it for the
/// saver ([`StudioRun`]).
pub(crate) fn single_answer(
    profile: &ProfileBacktest,
    result: &TradingRecord,
) -> Result<String, serde_json::Error> {
    single_answer_of(profile, result, shared_report(profile, result))
}

/// **The part of [`single_answer`]'s report a summary shares** (decision 0078's amendment of 2026-11-19): `result`'s
/// `TradingResults::from_record` at `profile`'s annualization, unnamed and unstamped, computed ONCE for a summarized
/// single run and handed to both [`single_answer_of`] and the run's summary
/// (`vike_backtesthub_client::summary::summarize_record`), so the summary's numbers are the saved report's own.
pub(crate) fn shared_report(profile: &ProfileBacktest, result: &TradingRecord) -> TradingResults {
    TradingResults::from_record(None, result, vike_backtest::run::report::periods_per_year(profile))
}

/// [`single_answer`] over `report`, [`shared_report`]'s for `result`: named and stamped here
/// (`vike_backtest::run::report::stamp_run_report`). `from_record` writes its name into the one field and reads it
/// nowhere else, so this is the bytes `single_answer` always answered.
pub(crate) fn single_answer_of(
    profile: &ProfileBacktest,
    result: &TradingRecord,
    report: TradingResults,
) -> Result<String, serde_json::Error> {
    let report = vike_backtest::run::report::stamp_run_report(profile, result, report);
    let forecasts = RunForecasts::bounded(&result.forecasts);
    let ledger = crate::backtest_cli::run_record::run_trades_from(result);
    report_answer(&report, forecasts).and_then(|json| with_trade_ledger(json, &ledger))
}

/// What a Studio run leaves for this daemon's saver beside the answer its client reads (book W35: every backtest is a
/// run): the slice's profile as TEXT and the answer the matching profile verb gives that profile. A `.rs` file's
/// sandboxed child sends it up in its own frame; no client reads it.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct StudioRun {
    /// The slice's profile (`vike_backtest::run::profile::ProfileBacktest::slice_text`, a scan's `[paramscan]` grid or a
    /// walk's `[walkforward]` table added): the run's `config.toml`.
    pub(crate) profile_toml: String,
    /// What the profile verb answers that profile: `RunBacktest`'s [`Response::Report`] ([`single_answer`], its trade
    /// ledger LAST), `RunParamscanProfile`'s [`Response::ParamscanReport`], `RunWalkforwardProfile`'s
    /// [`Response::WalkforwardReport`].
    pub(crate) answer: Response,
    /// An editor buffer's text, kept in the run as its file (`crate::studio_run::EDITOR_BUFFER_FILE`) so it can be
    /// run again; `None` for a strategy file, which lives in the strategies folder.
    pub(crate) source: Option<String>,
    /// A single BAR run's event log (`vike_backtest::run::run_backtest_logged`), kept in place of its whole curve
    /// when it is saved with one (this module's doc); `None` for every other run.
    #[serde(default)]
    pub(crate) equity_log: Option<EquityLog>,
}

impl StudioRun {
    /// The saved run of a Studio run whose profile text is `profile_toml` and whose profile-verb answer is `answer`
    /// (`source`, an editor buffer's text), or `None`, logged, when either cannot be written down: saving never costs
    /// the answer.
    pub(crate) fn of(
        profile_toml: Result<String, HarnessError>,
        answer: Result<Response, serde_json::Error>,
        source: Option<&str>,
    ) -> Option<Self> {
        let built = profile_toml.map_err(|e| e.to_string()).and_then(|profile_toml| {
            let answer = answer.map_err(|e| e.to_string())?;
            Ok(StudioRun {
                profile_toml,
                answer,
                source: source.map(str::to_string),
                equity_log: None,
            })
        });
        match built {
            Ok(run) => Some(run),
            Err(why) => {
                tracing::warn!(
                    "backtesthub: the Studio run was answered and will NOT be saved: {why}"
                );
                None
            }
        }
    }
}

/// What a saved run needs from before its answer: the profile text it ran, its start second, and whether a
/// [`Response::Report`] to it is a single backtest's; for a Studio run, also the editor buffer it ran.
pub(super) struct SavedRunStart {
    profile_toml: String,
    started_at: i64,
    /// `RunBacktest` with a profile that asks for a run, not a data plan (`data.explain`), or a Studio single run.
    single: bool,
    /// [`StudioRun::source`].
    source: Option<String>,
    /// What a request that may answer again keeps (`super::answer_from_saved::Kept`), set once it answered.
    pub(super) kept: Option<super::answer_from_saved::Kept>,
}

/// `Some` for a request whose answer may be saved (a profile on `RunBacktest`, summarized or not,
/// `RunParamscanProfile` or `RunWalkforwardProfile`; a Studio run, summarized or not), read before the run so the
/// manifest says when it STARTED.
pub(super) fn start(request: &Request, clock: &dyn Clock) -> Option<SavedRunStart> {
    // A summarized run is saved as the run it wraps (a Studio run, or since decision 0078's amendment of
    // 2026-11-18 a `RunBacktest`), from the `StudioRun` it leaves beside its summary.
    let request = match request {
        Request::Summarized(inner) => &**inner,
        request => request,
    };
    let (profile_toml, single) = match request {
        Request::RunBacktest(text) => (text.clone(), !asks_for_a_plan(text)),
        Request::RunParamscanProfile { profile_toml: text, .. }
        | Request::RunWalkforwardProfile { profile_toml: text } => (text.clone(), false),
        // Its profile is the slice's, known once it ran: [`save_studio`] takes it from the [`StudioRun`].
        Request::RunSlice { .. } => (String::new(), true),
        Request::RunParamscan { .. } | Request::RunWalkforward { .. } => (String::new(), false),
        _ => return None,
    };
    let started_at = clock.now_ms() / 1000;
    Some(SavedRunStart { profile_toml, started_at, single, source: None, kept: None })
}

/// Whether a profile asks for its data PLAN (`[data] explain = true`): that answer is a [`Response::Report`] too,
/// and no run.
fn asks_for_a_plan(profile_toml: &str) -> bool {
    toml::from_str::<toml::Value>(profile_toml)
        .ok()
        .and_then(|doc| doc.get("data")?.get("explain")?.as_bool())
        .unwrap_or(false)
}

/// Save `response` under `runs_root` when it is a single backtest's report, a walk-forward's, a scan's or one of the
/// two validation reports, and hand it back unchanged either way. `trade_ledger` is what [`take_trade_ledger`] took off a single
/// backtest's answer: its `trades.json`. `no_room` is the free-space floor's refusal
/// (`vike_boot::disk_floor::DiskFloor::refusal` over `runs_root`, REVIEW-LIVE-MONEY H1): with one, nothing is
/// written and the skip is logged at `error`, because a full disk would stop every trading core on the node.
pub(super) fn save(
    response: Response,
    trade_ledger: Option<String>,
    start: Option<SavedRunStart>,
    runs_root: &Path,
    clock: &dyn Clock,
    no_room: Option<String>,
) -> Response {
    saved_as(&response, trade_ledger, None, None, start, runs_root, clock, no_room);
    response
}

/// [`save`]'s write, answering the id of the run it saved (`None`: nothing was saved). `equity_curve` is a
/// summarized single run's whole curve, written into the run as `vike_runs::EQUITY_CURVE_FILE`, and `equity_log` the
/// log [`save_studio`] keeps in its place (this module's doc): at most one of the two is handed.
fn saved_as(
    response: &Response,
    trade_ledger: Option<String>,
    equity_curve: Option<&RunEquityCurve>,
    equity_log: Option<&EquityLog>,
    start: Option<SavedRunStart>,
    runs_root: &Path,
    clock: &dyn Clock,
    no_room: Option<String>,
) -> Option<String> {
    let start = start?;
    let (protocol, json) = match response {
        Response::CombinatorialWalkforwardReport(json) => ("combinatorial_walkforward", json),
        Response::PermutationReport(json) => ("permutation", json),
        Response::WalkforwardReport(json) => ("walkforward", json),
        Response::ParamscanReport(json) => ("paramscan", json),
        Response::Report(json) if start.single => ("single", json),
        _ => return None,
    };
    if let Some(why) = no_room {
        tracing::error!("backtesthub: the {protocol} run was answered and NOT saved: {why}");
        return None;
    }
    let single = protocol == "single";
    let trades = trade_ledger.as_deref().filter(|_| single);
    let (curve, log) = (equity_curve.filter(|_| single), equity_log.filter(|_| single));
    match save_run(protocol, json, trades, curve, log, &start, runs_root, clock.now_ms() / 1000) {
        Ok(run_id) => {
            tracing::info!("backtesthub: saved the {protocol} run {run_id}");
            Some(run_id)
        }
        Err(why) => {
            tracing::warn!("backtesthub: the {protocol} run was answered and NOT saved: {why}");
            None
        }
    }
}

/// [`save`] for a Studio run, or a summarized `RunBacktest`: `run` is saved as the profile run it is, its profile as
/// `config.toml` and the profile verb's answer for that profile as the documents (a single run's report, forecasts and
/// trade ledger as `trades.json`, and the whole `equity_curve` a summarized single run is held with, or its event log
/// in the curve's place, this module's doc; a scan's or a walk's report), an editor buffer's text beside them, on
/// [`save`]'s own terms (`start`, the floor's `no_room`). `held_log` is the log a summarized single run is held as
/// INSTEAD of its curve (`crate::run_summary::held_log`: a `.rs` file's child pinned it and sent no curve).
/// Answers the saved run's id, which `super::dispatch::handle_request` writes into the summary
/// ([`with_saved_run`]). The client's answer is otherwise not this function's: it goes back as the run gave it.
pub(super) fn save_studio(
    run: StudioRun,
    equity_curve: Option<&RunEquityCurve>,
    held_log: Option<&EquityLog>,
    start: Option<SavedRunStart>,
    runs_root: &Path,
    clock: &dyn Clock,
    no_room: Option<String>,
) -> Option<String> {
    let start = start?;
    let (document, trade_ledger) = take_trade_ledger(run.answer);
    // The profile this process read off the request where it holds one (a `RunBacktest`'s, `start`'s), not the copy a
    // sandboxed child sent up: a rebuild reads `config.toml`. A Studio run's profile is the one its run built.
    let profile_toml =
        if start.profile_toml.is_empty() { run.profile_toml } else { start.profile_toml };
    let start = SavedRunStart { profile_toml, source: run.source, ..start };
    // The log in the curve's place: the one the run is held as (pinned in its child, no curve came up beside it), or
    // the run's own when it rebuilds the very curve this run is held with. Hashed HERE although the run hashed it too:
    // this check alone decides which of the two files a run held with its curve keeps. A log with no curve held keeps
    // nothing (a run that was not summarized).
    let log = held_log.or_else(|| {
        run.equity_log.as_ref().filter(|log| {
            equity_curve.is_some_and(|c| curve_fnv(&c.equity, &c.timestamp) == log.recipe.curve_fnv)
        })
    });
    let curve = equity_curve.filter(|_| log.is_none());
    saved_as(&document, trade_ledger, curve, log, Some(start), runs_root, clock, no_room)
}

/// `response` with the id of the run it was saved as (`saved_run`, [`save_studio`]'s answer) written into a single
/// run's summary (`vike_backtesthub_client::summary::RunSummaryWire::saved_run`), so a client can read the run's whole
/// curve from disk once this daemon no longer holds it. Any other answer goes back as it came.
pub(super) fn with_saved_run(response: Response, saved_run: Option<String>) -> Response {
    match (response, saved_run) {
        (Response::RunSummary(mut summary), Some(id)) => {
            // A reader that names the held run (`summary.run`) still finds the curve here once the held copy is gone.
            crate::run_summary::remember_saved(&summary.run, &id);
            summary.saved_run = id;
            Response::RunSummary(summary)
        }
        (response, _) => response,
    }
}

/// Apply the saved-run retention (`config.saved_runs_per_strategy`) under `runs_root` after a save, logging each run
/// it removes. A refusal or a failure is logged and removes nothing more: like saving, pruning never costs an answer.
pub(super) fn prune(retention: &vike_runs::RunRetention, runs_root: &Path) {
    match retention.prune(runs_root) {
        Ok(removed) => {
            // A removed run answers no identical request again (`super::answer_from_saved`'s index).
            super::answer_from_saved::forget(runs_root, &removed);
            for run_id in removed {
                tracing::info!(
                    "backtesthub: removed the saved run {run_id} (config.saved_runs_per_strategy = {})",
                    retention.keep_per_strategy
                );
            }
        }
        Err(why) => tracing::warn!("backtesthub: the saved-run retention removed nothing: {why}"),
    }
}

/// The key under a saved run's `detail` its event log's recipe is kept under WITHOUT its day stamps, which
/// `vike_runs::DAY_STAMPS_FILE` holds ([`day_stamps_apart`]). A key of its own, not [`INLINE_RECIPE_KEY`]'s: a build
/// from before the file reads the recipe there, and would rebuild a curve with no day to check; it finds no recipe at
/// all here and refuses the read in words instead.
pub(crate) const EQUITY_LOG_RECIPE_KEY: &str = "equity_log_recipe";

/// The key a run saved before `vike_runs::DAY_STAMPS_FILE` keeps its recipe under, each series' day stamps INLINE. Read
/// as it always was; nothing writes it any more.
pub(crate) const INLINE_RECIPE_KEY: &str = "equity_log";

/// The key under [`EQUITY_LOG_RECIPE_KEY`]'s object that names the day stamps file: its [`DayStampsKept`].
const DAY_STAMPS_KEY: &str = "day_stamps";

/// What a manifest records of its run's `vike_runs::DAY_STAMPS_FILE`: the FNV-1a 64 of its bytes, and how many day
/// stamps it holds, every series together. A file that does not match both is not this run's.
#[derive(serde::Serialize, serde::Deserialize)]
struct DayStampsKept {
    fnv: String,
    count: usize,
}

/// `recipe` as a saved run's manifest keeps it under [`EQUITY_LOG_RECIPE_KEY`] (every series' `days` empty, and
/// [`DayStampsKept`] under [`DAY_STAMPS_KEY`]), and the bytes of `vike_runs::DAY_STAMPS_FILE`: each series' day stamps
/// in the recipe's order, as msgpack WITH field names (`rmp_serde::to_vec_named`). Msgpack, not columns of our own: the
/// one columns codec is the bars' (`BAR_COLUMNS_MAGIC`), whose rows a day stamp is not, and only a curve rebuild reads
/// this file; the names let a later build read a file an earlier one wrote. Every `u64` stamp reads back exact.
fn day_stamps_apart(recipe: &EquityLogRecipe) -> Result<(Value, Vec<u8>), String> {
    let days: Vec<&Option<Vec<BarDay>>> = recipe.series.iter().map(|s| &s.days).collect();
    let bytes = rmp_serde::to_vec_named(&days)
        .map_err(|e| format!("the event log's day stamps do not serialize: {e}"))?;
    let count = recipe.series.iter().filter_map(|s| s.days.as_ref()).map(Vec::len).sum();
    let mut kept = recipe.clone();
    kept.series.iter_mut().for_each(|s| s.days = None);
    let mut value = serde_json::to_value(&kept)
        .map_err(|e| format!("the event log's recipe does not serialize: {e}"))?;
    let named = DayStampsKept { fnv: vike_utility::fnv::fnv1a64_hex(&bytes), count };
    value[DAY_STAMPS_KEY] = serde_json::to_value(named)
        .map_err(|e| format!("the event log's recipe does not serialize: {e}"))?;
    Ok((value, bytes))
}

/// The event-log recipe of the saved run in `dir`, whose manifest's `detail` is `detail`, its day stamps in place:
/// read from `vike_runs::DAY_STAMPS_FILE` for a run saved since that file ([`day_stamps_apart`], refused unless the
/// file's hash and count are the manifest's), or inline under [`INLINE_RECIPE_KEY`] for a run saved before it, as
/// that run always read.
pub(crate) fn saved_recipe(dir: &Path, detail: &Value) -> Result<EquityLogRecipe, String> {
    let unread = |e: serde_json::Error| format!("its recipe does not read: {e}");
    let Some(apart) = detail.get(EQUITY_LOG_RECIPE_KEY) else {
        let inline = detail.get(INLINE_RECIPE_KEY).ok_or("its manifest records no recipe")?;
        return EquityLogRecipe::deserialize(inline).map_err(unread);
    };
    let named = apart.get(DAY_STAMPS_KEY).ok_or("its recipe names no day stamps file")?;
    let named = DayStampsKept::deserialize(named).map_err(unread)?;
    let mut recipe = EquityLogRecipe::deserialize(apart).map_err(unread)?;
    let bytes = vike_runs::read_day_stamps(dir).map_err(|e| e.to_string())?;
    let file = vike_runs::DAY_STAMPS_FILE;
    let fnv = vike_utility::fnv::fnv1a64_hex(&bytes);
    if fnv != named.fnv {
        return Err(format!("its {file} hashes {fnv}, and its manifest names {}", named.fnv));
    }
    let days: Vec<Option<Vec<BarDay>>> =
        rmp_serde::from_slice(&bytes).map_err(|e| format!("its {file} does not read: {e}"))?;
    let count: usize = days.iter().flatten().map(Vec::len).sum();
    if days.len() != recipe.series.len() || count != named.count {
        return Err(format!(
            "its {file} holds {count} day stamps of {} series, and its manifest names {} of {}",
            days.len(),
            named.count,
            recipe.series.len()
        ));
    }
    recipe.series.iter_mut().zip(days).for_each(|(s, days)| s.days = days);
    Ok(recipe)
}

/// One run directory for `json`, the manifest written last. `Ok` is the minted run id. `equity_log`'s recipe is the
/// manifest's `detail.equity_log_recipe` and its day stamps `vike_runs::DAY_STAMPS_FILE` ([`day_stamps_apart`]).
fn save_run(
    protocol: &str,
    json: &str,
    trade_ledger: Option<&str>,
    equity_curve: Option<&RunEquityCurve>,
    equity_log: Option<&EquityLog>,
    start: &SavedRunStart,
    runs_root: &Path,
    finished_at: i64,
) -> Result<String, String> {
    let mut report: Value =
        serde_json::from_str(json).map_err(|e| format!("the answer is not JSON: {e}"))?;
    let trades: Option<RunTrades> = trade_ledger
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| format!("the answer's trade ledger does not read: {e}"))?;
    // A single backtest's forecasts leave the report for their own document (book W35).
    let forecasts: Option<RunForecasts> =
        match report.as_object_mut().and_then(|doc| doc.remove(ANSWER_FORECASTS_KEY)) {
            Some(doc) => Some(
                serde_json::from_value(doc)
                    .map_err(|e| format!("the answer's forecasts do not read: {e}"))?,
            ),
            None => None,
        };
    // A permutation answer says which of its two protocols ran.
    let protocol = match (protocol, report.get("trained").and_then(Value::as_bool)) {
        ("permutation", Some(true)) => "permutation_training",
        ("permutation", _) => "permutation_test",
        (other, _) => other,
    };
    let mut detail = detail_of(protocol, &report);
    let kept = super::answer_from_saved::keep_into(start.kept.as_ref(), &mut detail);
    let day_stamps = match equity_log {
        Some(log) => {
            let (recipe, days) = day_stamps_apart(&log.recipe)?;
            detail[EQUITY_LOG_RECIPE_KEY] = recipe;
            Some(days)
        }
        None => None,
    };
    // No input address, as a study's run has none: this producer computes no fingerprint.
    let run = create_run_dir(runs_root, start.started_at, None).map_err(|e| e.to_string())?;
    let manifest = manifest_of(&run.run_id, detail, start, finished_at, &report);
    // An editor buffer's text as the file its profile names, before the manifest marks the run finished: with
    // `config.toml` beside it, the run runs again (its sha256 is `detail.strategy_sha256`).
    if let Some(text) = &start.source {
        let path = run.path.join(crate::studio_run::EDITOR_BUFFER_FILE);
        vike_utility::atomic_file::write_atomic(&path, text.as_bytes())
            .map_err(|e| format!("the editor buffer could not be written: {e}"))?;
    }
    let extras = RunExtras {
        config_toml: Some(&start.profile_toml),
        forecasts: forecasts.as_ref(),
        trades: trades.as_ref(),
        equity_curve,
        equity_log: equity_log.map(|log| log.events.as_slice()),
        day_stamps: day_stamps.as_deref(),
        kept_answer: kept.map(super::answer_from_saved::Kept::bytes),
        ..RunExtras::default()
    };
    write_run_with(&run.path, &manifest, &report, &extras).map_err(|e| e.to_string())?;
    if let Some(kept) = kept {
        super::answer_from_saved::remember(runs_root, &run.run_id, kept);
    }
    Ok(run.run_id)
}

/// The manifest of saved run `run_id`: kind `backtest`, its `detail`, `start`'s profile name and start second, and the
/// strategy SDK `report` names.
fn manifest_of(
    run_id: &str,
    detail: Value,
    start: &SavedRunStart,
    finished_at: i64,
    report: &Value,
) -> RunManifest {
    let name = toml::from_str::<toml::Value>(&start.profile_toml)
        .ok()
        .and_then(|doc| doc.get("name")?.as_str().map(str::to_string));
    RunManifest {
        schema: MANIFEST_SCHEMA,
        run_id: run_id.to_string(),
        kind: BACKTEST_RUN_KIND.to_string(),
        produced_by: "vike-backend backtesthub".to_string(),
        started_at: utc_rfc3339(start.started_at),
        finished_at: utc_rfc3339(finished_at),
        git_sha: None,
        // The report's own `strategy_sdk` (`vike_analytics::TradingResults::strategy_sdk`), read where
        // `detail_of` reads the file.
        strategy_sdk: named_by(report, "strategy_sdk")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        fingerprint: None,
        // The profile arrived as TEXT, so it has no path; its own `name` is what a run on the far
        // side of a wire can record about it (the study door's rule).
        config: RunConfig { path: None, name },
        detail,
        // shortcut: no kept instrument row is recorded (the run read them in this daemon); record them
        // here if one of these runs is ever compared across a list refresh.
        kept_rows: Vec::new(),
    }
}

/// The opening of a parameter scan's whole report as `serde_json` writes it, before its first row
/// (`vike_backtest::search::paramscan::ParamscanReport`'s first field is `rows`): what [`ParamscanSave`] writes first
/// and what its tail (the report with no rows) opens with, `[]` after it.
const SCAN_REPORT_OPENING: &str = "{\"rows\":[";

/// How many bytes of a summarized search's `report.json` [`ParamscanSave`] writes between two looks at the free-space
/// floor. The floor is at least `vike_config::config::DEFAULT_OPTIONAL_WRITES_MIN_FREE_MIB` (or a share of a large
/// disk), so a save overshoots it by at most this much, a sliver of it, while one `statvfs` per 64 MiB costs nothing
/// beside the rows' own write.
const ROOM_CHECK_BYTES: u64 = 64 << 20;

/// The free-space floor a [`ParamscanSave`] asks: why a write under the given directory must not happen now, in words,
/// or `None` when there is room (`vike_boot::disk_floor::DiskFloor::refusal`, or a test's stand-in).
pub(crate) type RoomCheck<'a> = Box<dyn Fn(&Path) -> Option<String> + 'a>;

/// **The saved run of a SUMMARIZED profile search, written as its rows arrive** (decision 0078's amendment of
/// 2026-11-19, the owner's "do 1-7"): the run a plain `RunParamscanProfile` saves (`detail.protocol` `paramscan`, the
/// profile as `config.toml`, its `report.json` the whole report: every point with its trades, the plain verb's own
/// compact JSON byte for byte), but each row is written as the search hands it over (in this process, or up a
/// sandboxed child's pipe one row a frame), so no whole text of the report is made, held or framed. The manifest goes
/// LAST, as `vike_runs`' writer orders it.
///
/// ⚠ **A save that cannot finish REMOVES the directory it created** ([`Self::stop`], and its `Drop` for every other
/// way out): a point's report over a child's frame cap, a write that fails, the free-space floor refusing part-way
/// ([`ROOM_CHECK_BYTES`]), or a search that ends without its summary (a cancellation, a child that died mid-answer).
/// The half-written `report.json` is no document, the retention never counts a run with no manifest, so left in place
/// it would hold its bytes forever. Only the directory this save minted is removed, never another. Saving never costs
/// the answer: the summary goes back without `saved_run`, and the daemon's log says why.
pub(crate) struct ParamscanSave<'a> {
    /// `None`: nothing is saved (saving is off, the disk is below its floor, or the save stopped).
    start: Option<SavedRunStart>,
    runs_root: &'a Path,
    /// The free-space floor, asked before the first byte and every [`Self::room_every`] bytes after.
    room: Option<RoomCheck<'a>>,
    /// [`ROOM_CHECK_BYTES`]; a field so a test can look more often.
    room_every: u64,
    /// Bytes written since the floor was last asked.
    unchecked: u64,
    /// The run directory THIS save minted and its `report.json`, open from the first row on.
    report: Option<(vike_runs::RunDir, std::io::BufWriter<std::fs::File>)>,
    /// The report with no rows, its JSON ([`Self::tail`]).
    tail: Option<String>,
}

impl<'a> ParamscanSave<'a> {
    /// A save of the search `start` names under `runs_root` (`start` is `None` when this process saves nothing), the
    /// free-space floor `room` asked as it writes (`None`: no floor, a test's daemon).
    pub(super) fn new(
        start: Option<SavedRunStart>,
        runs_root: &'a Path,
        room: Option<RoomCheck<'a>>,
    ) -> Self {
        let room_every = ROOM_CHECK_BYTES;
        Self { start, runs_root, room, room_every, unchecked: 0, report: None, tail: None }
    }

    /// Write the next row's JSON; `None` is a row whose whole report could not reach this process (over a child's
    /// frame cap), and the run is then not saved.
    pub(crate) fn row(&mut self, json: Option<&str>) {
        if self.start.is_none() {
            return;
        }
        let written = match json {
            Some(json) => self.write_row(json),
            None => Err("a point's whole report is over the sandbox's frame cap".to_string()),
        };
        if let Err(why) = written {
            self.stop(&why);
        }
    }

    /// The whole report with no rows, as JSON: what closes the document ([`Self::finish`]).
    pub(crate) fn tail(&mut self, tail: String) {
        self.tail = Some(tail);
    }

    /// What the search keeps to answer again (`super::answer_from_saved::Kept`), written beside its report when it
    /// closes.
    pub(super) fn keep(&mut self, kept: Option<super::answer_from_saved::Kept>) {
        if let Some(start) = &mut self.start {
            start.kept = kept;
        }
    }

    /// Close the run (the report's tail, then `config.toml`, the manifest LAST) and name it in `response`, the
    /// search's summary. Any other answer (a refusal, a data plan, the search's error) saves nothing and goes back as
    /// it came; a directory already begun for it is removed (`Drop`).
    pub(super) fn finish(mut self, response: Response, clock: &dyn Clock) -> Response {
        let Response::ParamscanProfileSummary(mut summary) = response else { return response };
        if self.start.is_some() {
            match self.close(clock.now_ms() / 1000) {
                Ok(run_id) => {
                    tracing::info!("backtesthub: saved the paramscan run {run_id}");
                    summary.saved_run = run_id;
                }
                Err(why) => self.stop(&why),
            }
        }
        Response::ParamscanProfileSummary(summary)
    }

    /// One row written, after a comma unless it is the first; the floor asked first when it is due.
    fn write_row(&mut self, json: &str) -> Result<(), String> {
        let first = self.report.is_none();
        if first || self.unchecked >= self.room_every {
            if let Some(why) = self.room.as_ref().and_then(|room| room(self.runs_root)) {
                tracing::error!(
                    "backtesthub: the paramscan run's save hit the free-space floor: {why}"
                );
                return Err(format!("the free-space floor refused the rest of report.json: {why}"));
            }
            self.unchecked = 0;
        }
        let file = self.open()?;
        let separator: &[u8] = if first { b"" } else { b"," };
        file.write_all(separator)
            .and_then(|()| file.write_all(json.as_bytes()))
            .map_err(|e| format!("report.json could not be written: {e}"))?;
        self.unchecked += (separator.len() + json.len()) as u64;
        Ok(())
    }

    /// The open report, opened with [`SCAN_REPORT_OPENING`] on the first call, which mints the run directory.
    fn open(&mut self) -> Result<&mut std::io::BufWriter<std::fs::File>, String> {
        if self.report.is_none() {
            let started_at = self.start.as_ref().map_or(0, |start| start.started_at);
            let run =
                create_run_dir(self.runs_root, started_at, None).map_err(|e| e.to_string())?;
            let path = run.path.join(vike_runs::REPORT_FILE);
            let opened = std::fs::File::create(&path)
                .map(std::io::BufWriter::new)
                .and_then(|mut file| file.write_all(SCAN_REPORT_OPENING.as_bytes()).map(|()| file));
            match opened {
                Ok(file) => self.report = Some((run, file)),
                Err(e) => {
                    let removed = remove_unfinished(self.runs_root, &run);
                    return Err(format!(
                        "{} could not be written ({removed}): {e}",
                        path.display()
                    ));
                }
            }
        }
        Ok(&mut self.report.as_mut().expect("opened above").1)
    }

    /// Close the report with its tail, write `config.toml` and the manifest: the saved run's id.
    fn close(&mut self, finished_at: i64) -> Result<String, String> {
        let tail = self.tail.take().ok_or("the search handed over no report tail")?;
        let rest = tail
            .strip_prefix(SCAN_REPORT_OPENING)
            .and_then(|rest| rest.strip_prefix(']'))
            .ok_or("the report's tail does not open with its empty rows")?;
        let file = self.open()?;
        file.write_all(b"]")
            .and_then(|()| file.write_all(rest.as_bytes()))
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.flush())
            // Synced before the manifest below names it, as `vike_runs`' writers sync every document they write.
            .and_then(|()| file.get_ref().sync_all())
            .map_err(|e| format!("report.json could not be written: {e}"))?;
        let (run, start) = match (&self.report, &self.start) {
            (Some((run, _)), Some(start)) => (run, start),
            _ => return Err("the run is not open".to_string()),
        };
        // The header (the report's every key but its rows) names the strategy file, which the retention counts the run
        // by, as the whole report does for the plain verb's save.
        let header: Value = serde_json::from_str(&tail)
            .map_err(|e| format!("the report's tail is not JSON: {e}"))?;
        let config = run.path.join(vike_runs::CONFIG_FILE);
        vike_utility::atomic_file::write_atomic(&config, start.profile_toml.as_bytes())
            .map_err(|e| format!("{} could not be written: {e}", config.display()))?;
        let mut detail = detail_of("paramscan", &header);
        let kept = super::answer_from_saved::keep_into(start.kept.as_ref(), &mut detail);
        if let Some(kept) = kept {
            let path = run.path.join(vike_runs::KEPT_ANSWER_FILE);
            vike_utility::atomic_file::write_atomic(&path, kept.bytes())
                .map_err(|e| format!("{} could not be written: {e}", path.display()))?;
        }
        let manifest = manifest_of(&run.run_id, detail, start, finished_at, &header);
        let path = run.path.join(vike_runs::MANIFEST_FILE);
        vike_runs::write_json(&path, vike_runs::MANIFEST_FILE, &manifest)
            .map_err(|e| e.to_string())?;
        if let Some(kept) = kept {
            super::answer_from_saved::remember(self.runs_root, &run.run_id, kept);
        }
        let run_id = run.run_id.clone();
        // Finished: there is nothing left to stop, and nothing to remove.
        self.report = None;
        self.start = None;
        Ok(run_id)
    }

    /// Save nothing more and say why: the directory this save minted, its half-written `report.json` with it, is
    /// removed first, so no unfinished run is left behind.
    fn stop(&mut self, why: &str) {
        self.start = None;
        let removed = match self.report.take() {
            Some((run, file)) => {
                drop(file);
                format!(" ({})", remove_unfinished(self.runs_root, &run))
            }
            None => String::new(),
        };
        tracing::warn!("backtesthub: the paramscan run was answered and NOT saved{removed}: {why}");
    }
}

impl Drop for ParamscanSave<'_> {
    /// A save dropped with its report still open (the search ended without its summary) removes what it began.
    fn drop(&mut self) {
        if self.report.is_some() {
            self.stop("the search ended without its summary: cancelled, or its sandboxed child stopped mid-answer");
        }
    }
}

/// Remove `run`, the run directory a [`ParamscanSave`] minted under `runs_root` and could not finish, through the one
/// remover of a run directory (`vike_runs::remove_unfinished_run`), and say what became of it.
fn remove_unfinished(runs_root: &Path, run: &vike_runs::RunDir) -> String {
    match vike_runs::remove_unfinished_run(runs_root, run) {
        Ok(()) => format!("its unfinished directory {} was removed", run.path.display()),
        Err(why) => format!("its unfinished directory was NOT removed: {why}"),
    }
}

/// The manifest's `detail`: HOW the run was validated (`protocol`), and the strategy file and the sha256 of its text
/// as the report names them (two records with one hash ran the same code, the engine's `persist_run` keys). Every
/// protocol names them, so the retention counts every saved run by its file: a walk-forward on its `overall` results,
/// a combinatorial walk on each of its paths' (one file for all: a walk varies no strategy), every other report at
/// its top.
fn detail_of(protocol: &str, report: &Value) -> Value {
    let mut detail = json!({ "protocol": protocol });
    for key in ["strategy_file", "strategy_sha256"] {
        if let Some(value) = named_by(report, key) {
            detail[key] = value.clone();
        }
    }
    detail
}

/// `key` as the report names it: at its top, on a walk-forward's `overall`, or on a combinatorial walk's first path.
fn named_by<'a>(report: &'a Value, key: &str) -> Option<&'a Value> {
    report
        .get(key)
        .or_else(|| report.get("overall")?.get(key))
        .or_else(|| report.get("paths")?.get(0)?.get("overall")?.get(key))
}

#[path = "saved_run_tests.rs"]
#[cfg(test)]
mod saved_run_tests;
