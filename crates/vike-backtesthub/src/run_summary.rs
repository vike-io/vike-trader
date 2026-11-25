//! The SUMMARIZED runs (decision `docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s
//! amendments of 2026-11-16 and 2026-11-18).
//!
//! `Request::Summarized` wraps a Studio `RunSlice`, `RunParamscan` or `RunWalkforward`, or a profile's
//! `RunBacktest` (what `vike-cli backtest run` sends by default). The daemon runs the wrapped request
//! exactly as it would alone (a `RunBacktest` through `crate::backtesthub_server::dispatch`'s
//! `answer_whole_here`, which answers the run's whole result where the plain verb answers its report),
//! then [`summarize_answer`] makes its summary (`vike_backtesthub_client::summary`), so the whole answer
//! never crosses the wire, and [`hold_summary`] keeps the run's trades, every point of a scan and a single
//! run's WHOLE equity curve HERE, in this process, for `Request::ReadTrades`, `Request::ReadPoint` and
//! `Request::ReadCurve`.
//!
//! A `.rs` file's run is summarized INSIDE its sandboxed child, by the same [`summarize_answer`], and the
//! child streams each point's summary, its trades and a single run's curve in parts
//! (`crate::strategy_sandbox`'s `run_summarized_in_child`): no frame of the child's pipe carries more than
//! one point's summary or more than one part of its trades or curve, and its last frame is the answer the
//! client gets, so a sandboxed scan has no point-count ceiling the in-process one does not have. ⚠ A single BAR
//! run whose bars the store PINS (decision 0078's amendment of 2026-11-19 on what the hub holds: the child asks
//! before it chooses, `crate::strategy_sandbox`'s `pinned_log`) sends its event log in the curve's place, and this
//! process holds that log ([`HeldLog`]), not the curve, rebuilding the curve for the first page read.
//!
//! ⚠ **Held in memory, the newest [`HELD_RUNS`] only, and none across a restart.** A read of a run that
//! is gone is refused in words naming both limits; the Studio then asks for a new run. A walk-forward
//! holds nothing: its windows are all there is to show of it. The whole curve outlives the memory: a
//! serving daemon saves it with the run (`crate::backtesthub_server::saved_run`), and [`curve_page`]
//! reads it back from there by the saved run's id, or by the held name once the held copy is gone
//! ([`remember_saved`]). A bar run saved since decision 0078's amendment of 2026-11-19 keeps its EVENT LOG
//! (`vike_runs::EQUITY_LOG_FILE`) in the curve's place, and [`curve_page`] REBUILDS its curve from it over the bars
//! the daemon's store holds ([`CurveRebuild`], `vike_backtest::run::equity_log::rebuild_curve`, whose doc holds the
//! refusals), keeping the last few rebuilt curves for the pages after the first; every other saved run, and every run
//! saved before, reads its `vike_runs::EQUITY_CURVE_FILE` as it always did. The client reads the same pages either way.
//!
//! A PROFILE's parameter search (`Request::RunParamscanProfile`, the CLI's and the MCP tool's) is summarized here too
//! (decision 0078's amendment of 2026-11-19, the owner's "do 1-7"): [`summarize_paramscan`] walks its ranked report
//! row by row, each point's overrides and metric table for the client and its whole row's JSON for the saver
//! (`crate::backtesthub_server::saved_run`'s `ParamscanSave`, which writes the saved run's `report.json` as the rows
//! arrive), and [`hold_paramscan`] holds the rows alone for `Request::ReadParamscanRows` ([`paramscan_rows_page`]).
//! A `.rs` file's child hands each row up in its own frame, so no frame of either link carries more than one point.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use vike_analytics::results::{TradingResults, periods_per_year_for};
use vike_analytics::{TradingRecord, TradingResultsTable};
use vike_backtest::run::equity_log::{EquityLog, rebuild_curve};
use vike_backtest::run::profile::ProfileBacktest;
use vike_backtest::search::paramscan::ParamscanReport;
use vike_backtesthub_client::proto::{Request, Response, request_kind};
use vike_backtesthub_client::summary::{
    CurvePageWire, PARAMSCAN_ROWS_PAGE_SIZE, ParamscanPointWire, ParamscanProfileSummaryWire,
    ParamscanRowSummaryWire, ParamscanRowsPageWire, ParamscanSummaryWire, RunSummaryWire,
    TradesPageWire, WalkforwardSummaryWire, summarize, summarize_record,
};
use vike_backtesthub_client::wire_studio::{DataSliceWire, ReplayKindWire, TradingRecordWire};
use vike_data::HistoryStore;
use vike_model::Trade;
use vike_runs::{RunEquityCurve, RunReadError};

/// How many summarized runs this daemon holds the trades, points and whole curves of; a new one past it
/// drops the oldest. A held run costs its thinned summaries, its trades (a 24-point scan of 2,194 trades a
/// point is about 9 MB) and a single run's whole curve, 16 bytes a step (about 22 MB for 32 months of
/// 1-minute bars).
pub(crate) const HELD_RUNS: usize = 8;

/// A held run's points, each one's summary and trades, shared ([`held_parts`]).
pub(crate) type HeldPoints = Arc<Vec<(RunSummaryWire, Vec<Trade>)>>;

/// One held run: each point's summary and trades (a single run is one point), a single run's whole
/// equity curve or the event log it is rebuilt from (a scan keeps neither: each point's thinned curve is in its
/// summary), and a profile search's rows (its points keep no summary and no trade here: the whole search is in the
/// run it saved). The points and rows are SHARED, so a run kept to answer again reads them with no copy
/// ([`held_parts`]).
struct HeldRun {
    run: String,
    points: HeldPoints,
    curve: Option<Arc<RunEquityCurve>>,
    rows: Arc<Vec<ParamscanRowSummaryWire>>,
    log: Option<Arc<HeldLog>>,
}

/// **A summarized single BAR run's whole curve held as its EVENT LOG** (decision 0078's amendment of 2026-11-19 on
/// what the hub holds): what a `.rs` file's sandboxed child sends up in the curve's place once the store pinned every
/// bar the run marked (`crate::strategy_sandbox`'s `pinned_log`), a few kB where the curve is 16 bytes a step. This
/// process holds it, saves it, and rebuilds the curve from it for the first `Request::ReadCurve` page
/// ([`HeldLog::curve`]).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct HeldLog {
    /// The run's profile as text (`crate::backtesthub_server::saved_run::StudioRun::profile_toml`): what a rebuild
    /// resolves, as it resolves a saved run's `config.toml`.
    pub(crate) profile_toml: String,
    /// The log and its recipe, its day stamps the store's (`vike_backtest::run::equity_log::stamp`).
    pub(crate) log: EquityLog,
    /// The curve once a read rebuilt it, for the pages after the first.
    #[serde(skip)]
    rebuilt: OnceLock<Arc<RunEquityCurve>>,
}

impl HeldLog {
    /// `log` held for the run whose profile text is `profile_toml`, no curve rebuilt yet.
    pub(crate) fn new(profile_toml: String, log: EquityLog) -> Self {
        Self { profile_toml, log, rebuilt: OnceLock::new() }
    }

    /// The whole curve the log rebuilds over `rebuild`'s store (`vike_backtest::run::equity_log::rebuild_curve`, which
    /// refuses rather than answer a curve from changed data, its hash checked against the recipe's `curve_fnv`), once.
    fn curve(&self, rebuild: Option<&CurveRebuild<'_>>) -> Result<Arc<RunEquityCurve>, String> {
        if let Some(curve) = self.rebuilt.get() {
            return Ok(Arc::clone(curve));
        }
        let rebuild =
            rebuild.ok_or("it is held as its event log, and nothing here rebuilds a curve")?;
        let (recipe, events) = (&self.log.recipe, &self.log.events);
        let (equity, timestamp) =
            rebuild_curve(&self.profile_toml, recipe, events, rebuild.store, rebuild.build)
                .map_err(|why| format!("its curve was not rebuilt from its event log: {why}"))?;
        let curve = Arc::new(RunEquityCurve { equity, timestamp });
        Ok(Arc::clone(self.rebuilt.get_or_init(|| curve)))
    }
}

/// The held runs, oldest first.
static HELD: Mutex<VecDeque<HeldRun>> = Mutex::new(VecDeque::new());

/// The next held run's number in this process.
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// A fresh run name: this process's id and a counter, so a name from before a restart is not
/// mistaken for a run held now.
fn next_run() -> String {
    format!("{:x}-{}", std::process::id(), NEXT_RUN.fetch_add(1, Ordering::Relaxed))
}

/// Hold `run`, dropping the oldest held run past [`HELD_RUNS`].
fn hold(run: HeldRun) {
    let mut held = HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    held.push_back(run);
    while held.len() > HELD_RUNS {
        held.pop_front();
    }
}

/// The annualization the Studio's panes used for `slice`: its bar interval's, or the no-period
/// default for a tick slice (`vike_analytics::results::periods_per_year_for`).
fn periods_of(slice: &DataSliceWire) -> f64 {
    match slice.kind {
        ReplayKindWire::Bars => periods_per_year_for(Some(&slice.interval)),
        ReplayKindWire::Ticks => periods_per_year_for(None),
    }
}

/// A summarized answer before any process holds it: what the client is answered, with the trades (and a
/// scan's points, or a single run's whole curve) to hold. [`summarize_answer`] makes it, in this process or
/// in a `.rs` file's sandboxed child, which streams it to this one (`crate::strategy_sandbox`'s
/// `run_summarized_in_child`), and [`hold_summary`] holds it and answers the client.
#[derive(Debug)]
pub(crate) enum Summary {
    /// A single run: its summary, its trades and its whole equity curve, and the event log held in the curve's place
    /// when a `.rs` file's child pinned it ([`HeldLog`]; the curve is then not held, nor sent up when it fits a frame).
    Run(Box<RunSummaryWire>, Vec<Trade>, RunEquityCurve, Option<Box<HeldLog>>),
    /// A param scan: the answer (no run named yet) and every point's summary and trades, in point order.
    Scan(Box<ParamscanSummaryWire>, Vec<(RunSummaryWire, Vec<Trade>)>),
    /// An answer with nothing to hold: a walk-forward's summary, or the run's own refusal.
    Answer(Box<Response>),
}

/// The annualization of `inner`'s summary (its slice's, or for a `RunBacktest` its profile's, the
/// annualization its report uses: `vike_backtest::run::report::periods_per_year`), or why a request that
/// is none of the four has none. `strategies_dir` is where the profile reads its strategy file, as the run
/// reads it.
pub(crate) fn periods_of_request(inner: &Request, strategies_dir: &Path) -> Result<f64, String> {
    match inner {
        Request::RunSlice { slice, .. }
        | Request::RunParamscan { slice, .. }
        | Request::RunWalkforward { slice, .. } => Ok(periods_of(slice)),
        // A profile that does not parse runs to its own refusal, which passes through unsummarized, so
        // the factor it gets here is never used.
        Request::RunBacktest(profile_toml) => {
            Ok(ProfileBacktest::from_toml_str_in(profile_toml, strategies_dir)
                .map_or(periods_per_year_for(None), |profile| {
                    vike_backtest::run::report::periods_per_year(&profile)
                }))
        }
        other => Err(format!(
            "Summarized wraps a Studio RunSlice, RunParamscan or RunWalkforward, or a RunBacktest; {} \
             has no summary",
            request_kind(other)
        )),
    }
}

/// **What the process that ran a summarized run still holds behind its whole answer** (decision 0078's amendment of
/// 2026-11-19): each point's FULL record, in the answer's order (a single run's one, a scan's best first), and a
/// single run's report over it (`crate::backtesthub_server::saved_run::shared_report`), computed once for its saved
/// report and its summary. The whole answer's wire record decodes without the closed-trade block, four simulator
/// counters and the refusals past the decode cap (`vike_backtesthub_client::wire_record::from_wire_record`), so
/// [`summarize_answer`] summarizes these instead. Empty for an answer that holds no run (a walk-forward, a refusal):
/// that answer is summarized as it is.
#[derive(Debug, Default)]
pub(crate) struct WholeRecords {
    pub(crate) records: Vec<TradingRecord>,
    pub(crate) report: Option<TradingResults>,
}

/// **The ONE place a whole answer becomes a summary**: `whole`, the wrapped run's own answer, summarized
/// at `periods_per_year`, point by point for a scan, each point over its full record in `held`
/// ([`WholeRecords`]) when the run left one and over its decoded wire record otherwise. A failure passes
/// through as the run's own answer.
pub(crate) fn summarize_answer(
    whole: Response,
    held: WholeRecords,
    periods_per_year: f64,
) -> Summary {
    let WholeRecords { records, mut report } = held;
    let mut records = records.into_iter();
    let mut summarize_point = |whole: TradingRecordWire| match records.next() {
        Some(record) => summarize_record(whole, record, report.take(), periods_per_year),
        None => summarize(whole, periods_per_year),
    };
    match whole {
        Response::RunResult(whole) => {
            let curve = RunEquityCurve {
                equity: whole.equity_curve.clone(),
                timestamp: whole.equity_timestamp.clone(),
            };
            let (summary, trades) = summarize_point(whole);
            Summary::Run(Box::new(summary), trades, curve, None)
        }
        Response::ParamscanResult(whole) => {
            let mut points = Vec::with_capacity(whole.entries.len());
            let mut held = Vec::with_capacity(whole.entries.len());
            for (index, entry) in whole.entries.into_iter().enumerate() {
                let (mut summary, trades) = summarize_point(entry.result);
                summary.point = Some(index);
                points.push(ParamscanPointWire::of(entry.overrides, &summary));
                held.push((summary, trades));
            }
            let best = held.get(whole.best_index).map(|(summary, _)| summary.clone());
            let scan = ParamscanSummaryWire {
                run: String::new(),
                points,
                best,
                dsr: whole.dsr,
                pbo: whole.pbo,
                best_index: whole.best_index,
                cost_model: whole.cost_model,
                answered_from_saved_run: String::new(),
            };
            Summary::Scan(Box::new(scan), held)
        }
        Response::WalkforwardResult(whole) => Summary::Answer(Box::new(
            Response::WalkforwardSummary(Box::new(WalkforwardSummaryWire::of(*whole))),
        )),
        other => Summary::Answer(Box::new(other)),
    }
}

/// Hold `summary`'s trades, points and curve in THIS process under a fresh run name, and answer the
/// client with the summary, the name written into it and into every point.
pub(crate) fn hold_summary(summary: Summary) -> Response {
    match summary {
        Summary::Run(mut summary, trades, curve, log) => {
            let run = next_run();
            summary.run = run.clone();
            let points = Arc::new(vec![((*summary).clone(), trades)]);
            let curve = log.is_none().then_some(curve).map(Arc::new);
            hold(HeldRun { run, points, curve, rows: Arc::default(), log: log.map(Arc::from) });
            Response::RunSummary(summary)
        }
        Summary::Scan(mut scan, mut held) => {
            let run = next_run();
            for (summary, _) in &mut held {
                summary.run = run.clone();
            }
            if let Some(best) = &mut scan.best {
                best.run = run.clone();
            }
            scan.run = run.clone();
            let points = Arc::new(held);
            hold(HeldRun { run, points, curve: None, rows: Arc::default(), log: None });
            Response::ParamscanSummary(scan)
        }
        Summary::Answer(answer) => *answer,
    }
}

/// **Answer `Request::Summarized(inner)` in this process**: `run` answers `inner` as the daemon would
/// alone, with the records behind its answer ([`WholeRecords`]), then [`summarize_answer`] and
/// [`hold_summary`]. An inner request that is none of the four is refused in words; `strategies_dir` as
/// [`periods_of_request`]'s.
pub(crate) fn summarized(
    inner: Request,
    strategies_dir: &Path,
    run: impl FnOnce(Request) -> (Response, WholeRecords),
) -> Response {
    match periods_of_request(&inner, strategies_dir) {
        Ok(periods_per_year) => {
            let (whole, held) = run(inner);
            hold_summary(summarize_answer(whole, held, periods_per_year))
        }
        Err(why) => Response::Error(why),
    }
}

/// **A PROFILE's parameter search, summarized** (`Request::Summarized` around `Request::RunParamscanProfile`, decision
/// 0078's amendment of 2026-11-19, the owner's "do 1-7"): `report`'s rows, best first, each handed to `each` as its
/// summary (its overrides and metric table, `vike_analytics::TradingResultsTable::from_report`) and its own JSON (the
/// whole report's row, the bytes the plain verb answers it in, which the saver writes), one row at a time, so no text
/// of the whole report is ever made; then the search's header (no run named, no row) and the report's JSON with no
/// rows, the tail the saver closes the document with. `each`'s failure (a sandboxed child's pipe) stops the walk.
pub(crate) fn summarize_paramscan(
    mut report: ParamscanReport,
    mut each: impl FnMut(ParamscanRowSummaryWire, String) -> std::io::Result<()>,
) -> Result<(ParamscanProfileSummaryWire, String), String> {
    let encode = |e: serde_json::Error| format!("paramscan report serialize failed: {e}");
    let rows = std::mem::take(&mut report.rows);
    let tail = serde_json::to_string(&report).map_err(encode)?;
    // Every point reads one slice: the first row that says anything about it speaks for all of them.
    let data_warnings = rows
        .iter()
        .filter_map(|row| row.report.as_ref())
        .map(|report| &report.data_warnings)
        .find(|lines| !lines.is_empty())
        .cloned()
        .unwrap_or_default();
    // A summarized row carries its metric table alone: the strategy's failures of every point, together.
    let mut strategy_failures = vike_model::StrategyFailures::default();
    for report in rows.iter().filter_map(|row| row.report.as_ref()) {
        strategy_failures.merge(&report.strategy_failures);
    }
    let total = rows.len();
    for row in rows {
        let json = serde_json::to_string(&row).map_err(encode)?;
        let summary = ParamscanRowSummaryWire {
            report: row.report.as_ref().map(TradingResultsTable::from_report),
            overrides: row.overrides,
            error: row.error,
            score: row.score,
        };
        each(summary, json)
            .map_err(|e| format!("a summarized search's row was not handed on: {e}"))?;
    }
    // The ranking's name as the whole report spells it (`RankBy` serializes as one string).
    let rank_by = match serde_json::to_value(&report.rank_by) {
        Ok(serde_json::Value::String(name)) => name,
        _ => report.rank_by.label().to_string(),
    };
    let summary = ParamscanProfileSummaryWire {
        run: String::new(),
        saved_run: String::new(),
        total,
        page_size: 0,
        rows: Vec::new(),
        rank_by,
        summary: report.summary,
        account_currency: report.account_currency,
        strategy_file: report.strategy_file,
        strategy_sha256: report.strategy_sha256,
        strategy_sdk: report.strategy_sdk,
        data_warnings,
        strategy_failures,
        answered_from_saved_run: String::new(),
    };
    Ok((summary, tail))
}

/// Hold a summarized profile search's `rows` in THIS process under a fresh run name, for `Request::ReadParamscanRows`,
/// and answer the client with its header and its first page.
pub(crate) fn hold_paramscan(
    mut summary: ParamscanProfileSummaryWire,
    rows: Vec<ParamscanRowSummaryWire>,
) -> Response {
    let run = next_run();
    let first = ParamscanRowsPageWire::of(&rows, 0, PARAMSCAN_ROWS_PAGE_SIZE);
    summary.run = run.clone();
    summary.total = first.total;
    summary.page_size = first.page_size;
    summary.rows = first.rows;
    hold(HeldRun { run, points: Arc::default(), curve: None, rows: Arc::new(rows), log: None });
    Response::ParamscanProfileSummary(Box::new(summary))
}

/// **Answer `Request::ReadParamscanRows`**: page `page` of held search `run`'s rows, or the refusal in words.
pub(crate) fn paramscan_rows_page(run: &str, page: usize, page_size: usize) -> Response {
    let held = HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match held.iter().find(|held| held.run == run) {
        Some(found) => {
            Response::ParamscanRows(ParamscanRowsPageWire::of(&found.rows, page, page_size))
        }
        None => Response::Error(format!(
            "the backtesthub no longer holds search {run}: it keeps the last {HELD_RUNS} summarized runs \
             in memory, and none across a restart; the whole search is in the run it saved (the summary's \
             `saved_run`). Run it again."
        )),
    }
}

/// The [`Summary`] a sandboxed child streamed: its last answer, the points it closed, and the trades and
/// curve (or the event log in the curve's place) it sent after the last point (a single run's). The child's answer
/// names the shape: a run's summary, a scan's, or an answer with nothing to hold.
pub(crate) fn summary_from_child(
    answer: Response,
    points: Vec<(RunSummaryWire, Vec<Trade>)>,
    trades: Vec<Trade>,
    curve: RunEquityCurve,
    log: Option<Box<HeldLog>>,
) -> Summary {
    match answer {
        Response::RunSummary(summary) => Summary::Run(summary, trades, curve, log),
        Response::ParamscanSummary(scan) => Summary::Scan(scan, points),
        other => Summary::Answer(Box::new(other)),
    }
}

/// **Answer `Request::ReadTrades`**: page `page` of held run `run`'s trades (point `point` of a scan).
pub(crate) fn trades_page(
    run: &str,
    point: Option<usize>,
    page: usize,
    page_size: usize,
) -> Response {
    with_point(run, point, |_, trades| {
        Response::TradesPage(TradesPageWire::of(trades, page, page_size))
    })
}

/// **Answer `Request::ReadPoint`**: the summary of point `point` of held scan `run`.
pub(crate) fn point_summary(run: &str, point: usize) -> Response {
    with_point(run, Some(point), |summary, _| Response::RunSummary(Box::new(summary.clone())))
}

/// The whole equity curve of held single run `run`, or `None` when no such run is held (or it is a scan, or it is
/// held as its event log: [`held_log`]). What the saver writes into the run it saves
/// (`crate::backtesthub_server::saved_run`).
pub(crate) fn held_curve(run: &str) -> Option<Arc<RunEquityCurve>> {
    let held = HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    held.iter().find(|held| held.run == run).and_then(|held| held.curve.clone())
}

/// The event log held single run `run` is held as in its curve's place ([`HeldLog`]), or `None`. What the saver
/// writes into the run it saves in the curve's place.
pub(crate) fn held_log(run: &str) -> Option<Arc<HeldLog>> {
    let held = HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    held.iter().find(|held| held.run == run).and_then(|held| held.log.clone())
}

/// The points and rows held run `run` holds (a single run's one point with its trades, a scan's points, a profile
/// search's rows), SHARED, not copied: what a run that may be answered from its saved run encodes for the reads after
/// its answer (`crate::backtesthub_server::answer_from_saved`), outside the lock. Empty when no such run is held.
pub(crate) fn held_parts(run: &str) -> (HeldPoints, Arc<Vec<ParamscanRowSummaryWire>>) {
    let held = HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    held.iter()
        .find(|held| held.run == run)
        .map(|held| (Arc::clone(&held.points), Arc::clone(&held.rows)))
        .unwrap_or_default()
}

/// The whole curve the FINISHED saved run in `dir` keeps, to hold: its `vike_runs::EQUITY_CURVE_FILE`, or its
/// `vike_runs::EQUITY_LOG_FILE` with its recipe and `config.toml` as the [`HeldLog`] the curve is rebuilt from at its
/// first page, as a fresh `.rs` run's is held. `(None, None)` for a run that keeps neither readably: its pages then read
/// the saved run, as before.
pub(crate) fn held_of_saved(dir: &Path) -> (Option<Arc<RunEquityCurve>>, Option<Arc<HeldLog>>) {
    if let Ok(curve) = vike_runs::read_equity_curve(dir) {
        return (Some(Arc::new(curve)), None);
    }
    let log = || -> Option<HeldLog> {
        let events = vike_runs::read_equity_log(dir).ok()?;
        let manifest = vike_runs::read_manifest(dir).ok()?;
        let recipe =
            crate::backtesthub_server::saved_run::saved_recipe(dir, &manifest.detail).ok()?;
        let profile = vike_runs::read_config_toml(dir).ok()?;
        Some(HeldLog::new(profile, EquityLog { recipe, events }))
    };
    (None, log().map(Arc::new))
}

/// **A summarized answer from a saved run, held again** (decision 0078's amendment of 2026-11-20): `answer` (its held
/// names blank) held under a fresh name with the `points` and `rows` it held when it ran, as [`hold_summary`] and
/// [`hold_paramscan`] hold them, so the reads after it page what they paged then. A single run holds `whole`, the saved
/// run's curve or event log ([`held_of_saved`]), as a fresh run holds its own, so its pages need the saved run no more
/// (it may be pruned); with neither, they read `saved_run`, remembered for the held name. Any other answer goes back as
/// it is.
pub(crate) fn hold_again(
    answer: Response,
    mut points: Vec<(RunSummaryWire, Vec<Trade>)>,
    rows: Vec<ParamscanRowSummaryWire>,
    whole: (Option<Arc<RunEquityCurve>>, Option<Arc<HeldLog>>),
    saved_run: &str,
) -> Response {
    let run = next_run();
    for (summary, _) in &mut points {
        summary.run = run.clone();
    }
    match answer {
        Response::RunSummary(mut summary) => {
            summary.run = run.clone();
            summary.saved_run = saved_run.to_string();
            remember_saved(&run, saved_run);
            let (curve, log) = whole;
            let points = Arc::new(points);
            hold(HeldRun { run, points, curve, rows: Arc::default(), log });
            Response::RunSummary(summary)
        }
        Response::ParamscanSummary(mut scan) => {
            scan.run = run.clone();
            if let Some(best) = &mut scan.best {
                best.run = run.clone();
            }
            let points = Arc::new(points);
            hold(HeldRun { run, points, curve: None, rows: Arc::default(), log: None });
            Response::ParamscanSummary(scan)
        }
        Response::ParamscanProfileSummary(mut search) => {
            search.saved_run = saved_run.to_string();
            hold_paramscan(*search, rows)
        }
        other => other,
    }
}

/// How many held run names this daemon remembers the saved run of ([`remember_saved`]), so a client that
/// reads a curve by the summary's `run` still reads it once the held copy is dropped: far more runs than
/// [`HELD_RUNS`], a few dozen bytes each.
const SAVED_AS_REMEMBERED: usize = 4096;

/// `(held run name, saved run id)`, oldest first.
static SAVED_AS: Mutex<VecDeque<(String, String)>> = Mutex::new(VecDeque::new());

/// Remember that held run `run` was saved as `saved_run` (`crate::backtesthub_server::saved_run`'s
/// `with_saved_run`), so [`curve_page`] reads its whole curve from the saved run after [`HELD_RUNS`] newer runs
/// dropped the held copy. The oldest pair past [`SAVED_AS_REMEMBERED`] is forgotten.
pub(crate) fn remember_saved(run: &str, saved_run: &str) {
    let mut saved = SAVED_AS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    saved.push_back((run.to_string(), saved_run.to_string()));
    while saved.len() > SAVED_AS_REMEMBERED {
        saved.pop_front();
    }
}

/// The saved run held run `run` was saved as ([`remember_saved`]), if this process remembers one.
fn saved_as(run: &str) -> Option<String> {
    let saved = SAVED_AS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    saved.iter().rev().find(|(held, _)| held == run).map(|(_, id)| id.clone())
}

/// What [`curve_page`] rebuilds a saved bar run's curve with when the run keeps its event log in the curve's place:
/// the daemon's store, whose bars the rebuild reads, and the daemon's own build (`super::backtesthub_server::StudyDirs`).
pub(crate) struct CurveRebuild<'a> {
    pub(crate) store: &'a Arc<dyn HistoryStore + Send + Sync>,
    pub(crate) build: Option<&'a str>,
}

/// The curves rebuilt last, by run directory, newest last: the pages after a run's first read no log again.
static REBUILT: Mutex<VecDeque<(PathBuf, Arc<RunEquityCurve>)>> = Mutex::new(VecDeque::new());

/// The whole curve of the FINISHED saved run in `dir`: its `vike_runs::EQUITY_CURVE_FILE` when it keeps one (a run
/// saved before the event log, a tick run, a run its log does not cover), else rebuilt from its
/// `vike_runs::EQUITY_LOG_FILE`, its recipe (`crate::backtesthub_server::saved_run`'s `saved_recipe`: its day stamps
/// from `vike_runs::DAY_STAMPS_FILE`, or inline in a run saved before that file) and its `config.toml` through
/// `rebuild` (`vike_backtest::run::equity_log::rebuild_curve`, which refuses rather than answer a curve from changed
/// data).
fn saved_curve(
    dir: &Path,
    rebuild: Option<&CurveRebuild<'_>>,
) -> Result<Arc<RunEquityCurve>, String> {
    let missing = match vike_runs::read_equity_curve(dir) {
        Ok(curve) => return Ok(Arc::new(curve)),
        Err(missing @ RunReadError::Missing { .. }) => missing,
        Err(other) => return Err(other.to_string()),
    };
    let kept = REBUILT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, curve)) = kept.iter().find(|(path, _)| path == dir) {
        return Ok(Arc::clone(curve));
    }
    drop(kept);
    let events = match vike_runs::read_equity_log(dir) {
        Ok(events) => events,
        Err(RunReadError::Missing { .. }) => return Err(missing.to_string()),
        Err(other) => return Err(other.to_string()),
    };
    let rebuild = rebuild.ok_or("it keeps its event log, and nothing here rebuilds a curve")?;
    let manifest = vike_runs::read_manifest(dir).map_err(|e| e.to_string())?;
    let recipe = crate::backtesthub_server::saved_run::saved_recipe(dir, &manifest.detail)?;
    let profile = vike_runs::read_config_toml(dir).map_err(|e| e.to_string())?;
    let (equity, timestamp) =
        rebuild_curve(&profile, &recipe, &events, rebuild.store, rebuild.build)
            .map_err(|why| format!("its curve was not rebuilt from its event log: {why}"))?;
    let curve = Arc::new(RunEquityCurve { equity, timestamp });
    let mut kept = REBUILT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    kept.push_back((dir.to_path_buf(), Arc::clone(&curve)));
    while kept.len() > HELD_RUNS {
        kept.pop_front();
    }
    Ok(curve)
}

/// **Answer `Request::ReadCurve`**: page `page` of run `run`'s whole equity curve, from the held single run
/// of that name (its curve, or rebuilt through `rebuild` from the event log it is held as: [`HeldLog::curve`]), else
/// from the FINISHED saved run under `runs_root` it was saved as ([`remember_saved`]) or, for a saved run's own id,
/// of that id (`vike_runs::finished_run_dir`, which holds the id to one directory name): kept, or rebuilt through
/// `rebuild` ([`saved_curve`]), else the refusal in words.
pub(crate) fn curve_page(
    run: &str,
    page: usize,
    page_size: usize,
    runs_root: &Path,
    rebuild: Option<&CurveRebuild<'_>>,
) -> Response {
    let saved = || {
        let id = saved_as(run).unwrap_or_else(|| run.to_string());
        let dir = vike_runs::finished_run_dir(runs_root, &id).map_err(|e| e.to_string())?;
        saved_curve(&dir, rebuild)
    };
    let held = match held_curve(run) {
        Some(curve) => Some(Ok(curve)),
        None => held_log(run).map(|log| log.curve(rebuild)),
    };
    match held.unwrap_or_else(saved) {
        Ok(curve) => {
            Response::CurvePage(CurvePageWire::of(&curve.equity, &curve.timestamp, page, page_size))
        }
        Err(why) => Response::Error(format!(
            "the backtesthub holds no whole equity curve of run {run}: it keeps one for a single run \
             (a scan's points keep none) among the last {HELD_RUNS} summarized runs in memory, and in \
             the run it saved, named by the summary's `saved_run` ({why}). Run it again."
        )),
    }
}

/// `answer` over point `point` of held run `run` (`None`: the run's first and, for a single run,
/// only point), or the refusal in words when the run is gone or has no such point.
fn with_point(
    run: &str,
    point: Option<usize>,
    answer: impl FnOnce(&RunSummaryWire, &[Trade]) -> Response,
) -> Response {
    let held = HELD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(found) = held.iter().find(|held| held.run == run) else {
        return Response::Error(format!(
            "the backtesthub no longer holds run {run}: it keeps the last {HELD_RUNS} summarized runs \
             in memory, and none across a restart. Run it again."
        ));
    };
    let index = point.unwrap_or(0);
    match found.points.get(index) {
        Some((summary, trades)) => answer(summary, trades),
        None => Response::Error(format!(
            "run {run} has {} point(s); there is no point {index}",
            found.points.len()
        )),
    }
}

#[path = "run_summary_tests.rs"]
#[cfg(test)]
mod run_summary_tests;
