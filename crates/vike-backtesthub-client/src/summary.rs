//! **A Studio run's SUMMARY**: what the Studio shows of a run, made from the whole answer beside the
//! data, so the whole answer never crosses the wire (decision
//! `docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s amendment of 2026-11-16).
//!
//! # Why
//!
//! A whole answer is every bar's equity, its timestamp and every closed trade: 2.8 MB of JSON for the
//! "SMA cross" template over binance BTCUSDT 15m (66,786 bars, 2,194 trades), measured on the CI box on
//! 2026-11-16. It computed in 2.6 s and crossed the owner's link in about 76 s. A param scan answered
//! that much per grid point, so about 24 points passed the frame cap
//! (`vike_hub_protocol::frame::MAX_FRAME_LEN`) and failed at the write.
//!
//! # What a summary carries
//!
//! - every metric the Performance tab shows, rendered ([`RunSummaryWire::rows`]), and the results'
//!   one label ([`RunSummaryWire::source_label`]);
//! - the three numbers a pane colours or ranks by, the PSR, and the return histogram the
//!   Distribution tab draws, all computed over the WHOLE curve;
//! - the equity curve thinned to about [`CURVE_POINTS`] points by [`downsample`], which keeps every
//!   bucket's lowest and highest point, both ends and the span of the maximum drawdown, so the
//!   drawdown read off the thinned curve is the whole curve's, exactly; each kept point's timestamp rides beside
//!   it ([`RunSummaryWire::curve_timestamp`]), so a chart's date axis is exact.
//!
//! The trades stay on the server. The daemon holds them under [`RunSummaryWire::run`] and
//! `Request::ReadTrades` pages them ([`TRADES_PAGE_SIZE`] a page). A param scan answers each point's
//! [`ParamscanPointWire`] and the best point's summary; `Request::ReadPoint` answers another point's.
//!
//! A PROFILE's parameter search (`Request::RunParamscanProfile`, the CLI's and the MCP tool's) is summarized point by
//! point too (decision 0078's amendment of 2026-11-19): each point's overrides and metric table
//! ([`ParamscanRowSummaryWire`]), no trade and no curve, in pages ([`ParamscanProfileSummaryWire`],
//! [`ParamscanRowsPageWire`]), and its whole report, every point with its trades, in the run the backtesthub saved. A
//! whole search answer was every point's whole report in one frame, and past about 60 to 180 points of a real
//! strategy it passed the frame cap and the search failed.
//!
//! So does a single run's WHOLE equity curve (decision 0078's amendment of 2026-11-18): the daemon holds
//! it beside the trades and keeps it in the run it saves ([`RunSummaryWire::saved_run`]), and
//! `Request::ReadCurve` pages it ([`CurvePageWire`], [`CURVE_PAGE_SIZE`] steps a page), so a run of any
//! length reads back whole with no frame carrying it whole.
//!
//! # One summarizer for both sides
//!
//! [`summarize`] runs in the desktop over a named run's whole answer (its bounded window crosses
//! whole, decision 0064), over the record `crate::wire_record::from_wire_record` decodes.
//! [`summarize_record`] runs on the backtesthub over a Studio run or a summarized `RunBacktest`, over
//! the FULL record the run answered (decision 0078's amendment of 2026-11-19): the decoded record has
//! no closed-trade block, four simulator counters at zero and its refusals capped, so a summary of it
//! said less than the run's saved report. Both are one fold (`summarize_from`).

use serde::{Deserialize, Serialize};
use vike_analytics::metrics;
use vike_analytics::overfit::psr_of_curve;
use vike_analytics::results::TradingResults;
use vike_analytics::walkforward::WalkforwardResultsStep;
use vike_analytics::zero_trade::{DenialDetail, group_denials};
use vike_analytics::{TradingRecord, TradingResultsTable};
use vike_math::statistics::equal_width_bin;
use vike_model::Trade;
use vike_model::params::ParamValue;

use crate::wire_record::from_wire_record;
use crate::wire_studio::{CostModelStampWire, TradingRecordWire, WalkforwardResultsWire};

/// About how many points a summary's curve keeps ([`downsample`]): a chart is a couple of thousand
/// pixels wide, so more points draw nothing more.
pub const CURVE_POINTS: usize = 2000;

/// The trades a page carries when the caller names no size: one screen of the Trades tab.
pub const TRADES_PAGE_SIZE: usize = 200;

/// The most trades one page may carry, whatever the caller asks for.
pub const TRADES_PAGE_SIZE_MAX: usize = 1000;

/// The steps a page of a whole equity curve carries when the caller names no size ([`CurvePageWire`]):
/// about 3.5 MB of JSON.
pub const CURVE_PAGE_SIZE: usize = 100_000;

/// The most steps one curve page may carry, whatever the caller asks for: a step's JSON is at most
/// about 46 bytes (a 20-digit timestamp, a 24-character float and two commas), so a page stays near 23 MB,
/// a third of the hub frame cap (`vike_hub_protocol::frame::MAX_FRAME_LEN`).
pub const CURVE_PAGE_SIZE_MAX: usize = 500_000;

/// How many bins the Distribution tab's return histogram has.
pub const HISTOGRAM_BINS: usize = 21;

/// One run as the Studio shows it. Built by [`summarize`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RunSummaryWire {
    /// The run the backtesthub holds this summary's trades under, for `Request::ReadTrades` and
    /// `Request::ReadPoint`. Empty when nothing holds them on a server (a named run, summarized in
    /// the desktop with its trades in hand).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run: String,
    /// The id of the run the backtesthub SAVED under its runs root for this single run, whose whole
    /// equity curve `Request::ReadCurve` reads after [`Self::run`] is no longer held (decision 0078's
    /// amendment of 2026-11-18). Empty when nothing was saved, and on a scan's points.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub saved_run: String,
    /// Which point of a param scan this is; `None` for a single run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<usize>,
    /// The Performance tab: every metric-catalog row as `(id, rendered text)`, in catalog order,
    /// `vike_analytics::results::TradingResults::rendered_rows` over the whole run.
    pub rows: Vec<(String, String)>,
    /// The run is INVALID: its equity reached zero (`vike_analytics::TradingRecord::equity_reached_zero`'s
    /// words, naming the day). Its rows then open with the INVALID row and keep only money and counts, and
    /// its Sharpe, return, PSR and return histogram are left out. Skipped when `None`, so every other
    /// summary is the bytes it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid: Option<String>,
    /// The annualized Sharpe (`vike_analytics::metrics::sharpe` at the slice's factor), `None` when
    /// not finite: the number a param scan ranks and colours by.
    pub sharpe: Option<f64>,
    /// The fractional return from the first equity to the last, `None` when not finite.
    pub total_return: Option<f64>,
    /// The equity at the end of the run.
    pub final_equity: f64,
    /// How many trades closed: the length of the list `Request::ReadTrades` pages.
    pub n_trades: usize,
    /// P(true Sharpe > 0) over the whole curve (`vike_analytics::overfit::psr_of_curve`), `None` when
    /// not finite: the Validation tab's PSR row.
    pub probabilistic_sharpe: Option<f64>,
    /// The equity curve thinned by [`downsample`]: `(step index, equity)` pairs in step order. Plain msgpack, not
    /// columns: about 2,000 points, which `crate::msgpack_body::table`'s two passes cost more than they save
    /// (measured: the JOAT summary took 728 µs to encode and decode with them, against 308 µs as JSON).
    pub curve: Vec<(usize, f64)>,
    /// Each [`Self::curve`] point's timestamp (epoch ms), in its order, so a chart draws the curve, and the
    /// drawdown read off it, on a date axis (2026-11-20). Empty when the run tracked none, and absent from an
    /// older server's frame: the client then draws step numbers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curve_timestamp: Vec<i64>,
    /// How many steps the whole curve has.
    pub curve_len: usize,
    /// The per-bar return histogram over the whole curve.
    pub returns_histogram: ReturnsHistogramWire,
    /// Each reason the run refused an order, and how many times, in first-seen order
    /// (`vike_analytics::zero_trade::aggregate_denials`' shape). **The summary's one list of its
    /// denials** (decision 0078's amendment of 2026-11-19): a reason is its words with the numbers
    /// lifted out (`vike_analytics::zero_trade::denial_template`), so a refusal whose sentence names
    /// its own quantity is one row, and [`Self::results`]' honesty block carries none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<(String, u64)>,
    /// Each of [`Self::refused`]' rows in its order: when its first and last denial were refused and a
    /// few examples with the numbers their sentences embedded (`vike_analytics::zero_trade::group_denials`).
    /// Absent from an older server's frame and skipped when empty; an older client ignores it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused_detail: Vec<DenialDetail>,
    /// What the run found wrong with its data and ran anyway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_warnings: Vec<String>,
    /// What the run was costed under (decision 0063), as the whole answer stated it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<CostModelStampWire>,
    /// The strategy file the run ran and the sha256 of its text, as the whole answer stated them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_sha256: String,
    /// **The one label of these results** (`vike_analytics::results::TradingResults::source_label`,
    /// built from the record with the strategy file attached): `backtest <file> <from> – <to> UTC`,
    /// the words every view heads the same results with (the owner, 2026-11-17), `vike-cli report
    /// run:` included. The Studio's results header shows it. Absent from a frame that carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    /// **The results table [`Self::rows`] were rendered from**, without its trades and curves (the
    /// closed-trade block's rows, open lots and buy-and-hold line are emptied), so the Studio's "Compare
    /// all" holds two runs side by side through the ONE comparison,
    /// `vike_analytics::results_comparison::TradingResultsComparison` (decision 0078's amendment of
    /// 2026-11-18, the owner's word of 2026-11-19). Additive on the same protocol version: absent from an
    /// older server's frame, which decodes as before, and skipped when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<TradingResults>,
    /// The id of the saved run this answer was read back from, when the backtesthub answered an IDENTICAL request from
    /// the run it saved instead of running it again (decision 0078's amendment of 2026-11-20): "answered from saved run
    /// <id>". Empty for an answer just computed; absent from an older server's frame, and skipped when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answered_from_saved_run: String,
}

/// The per-bar returns of a run, binned: [`HISTOGRAM_BINS`] bins of width `width` from `low`.
/// `counts` is empty when the curve has fewer than two points (there is no return to bin).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReturnsHistogramWire {
    /// The lowest return, the first bin's left edge.
    pub low: f64,
    /// Each bin's width.
    pub width: f64,
    /// How many returns fall in each bin, left to right.
    pub counts: Vec<u64>,
}

impl ReturnsHistogramWire {
    /// `returns` in [`HISTOGRAM_BINS`] bins over `[min, max]`: the fold the Distribution tab ran in the
    /// desktop, moved here with the curve it needs. A width under `1e-9` is held at `1e-9`, so a flat
    /// curve's returns land in the first bin rather than dividing by zero.
    pub fn of(returns: &[f64]) -> Self {
        if returns.is_empty() {
            return Self::default();
        }
        let (low, high) =
            returns.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        let width = ((high - low) / HISTOGRAM_BINS as f64).max(1e-9);
        let mut counts = vec![0u64; HISTOGRAM_BINS];
        for &x in returns {
            counts[equal_width_bin(x, low, width, HISTOGRAM_BINS)] += 1;
        }
        Self { low, width, counts }
    }
}

/// One point of a param scan: its overrides and the numbers the scan table prints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamscanPointWire {
    /// The `(name, value)` overrides that produced this point.
    pub overrides: Vec<(String, ParamValue)>,
    /// [`RunSummaryWire::sharpe`] of the point.
    pub sharpe: Option<f64>,
    /// [`RunSummaryWire::total_return`] of the point.
    pub total_return: Option<f64>,
    /// [`RunSummaryWire::final_equity`] of the point.
    pub final_equity: f64,
    /// [`RunSummaryWire::n_trades`] of the point.
    pub n_trades: usize,
    /// The point's maximum drawdown, a positive fraction of the peak (its results' `max_drawdown`, the
    /// number [`RunSummaryWire::results`] of the point carries), `None` when not finite and in an older
    /// server's frame (2026-11-20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_drawdown: Option<f64>,
    /// [`RunSummaryWire::invalid`] of the point: the scan table says INVALID for it, and the harness
    /// ranked it after every valid point, never by its Sharpe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid: Option<String>,
}

impl ParamscanPointWire {
    /// The point `summary` is, under `overrides`.
    pub fn of(overrides: Vec<(String, ParamValue)>, summary: &RunSummaryWire) -> Self {
        Self {
            overrides,
            sharpe: summary.sharpe,
            total_return: summary.total_return,
            final_equity: summary.final_equity,
            n_trades: summary.n_trades,
            max_drawdown: summary.results.as_ref().and_then(|results| finite(results.max_drawdown)),
            invalid: summary.invalid.clone(),
        }
    }
}

/// A param scan as the Studio shows it: every point's numbers, ranked best-first as the whole answer
/// ranked them, and the best point's whole summary. `dsr`/`pbo` are `None` when not assessed, as on
/// `crate::wire_studio::StudioParamscanWire`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamscanSummaryWire {
    /// The run the backtesthub holds every point's summary and trades under (`Request::ReadPoint`).
    pub run: String,
    /// Every point, best first. Columns in a msgpack body ([`crate::msgpack_body`]).
    #[serde(with = "crate::msgpack_body::table")]
    pub points: Vec<ParamscanPointWire>,
    /// The best point's summary; `None` when the scan has no point.
    pub best: Option<RunSummaryWire>,
    /// Deflated Sharpe across the points, `None` when not finite.
    pub dsr: Option<f64>,
    /// Probability of backtest overfitting, `None` when not assessable.
    pub pbo: Option<f64>,
    /// The best point's index in [`Self::points`] (always 0: the points are ranked).
    pub best_index: usize,
    /// What the scan's points were costed under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<CostModelStampWire>,
    /// The id of the saved run this answer was read back from, when the backtesthub answered an IDENTICAL request from
    /// the run it saved instead of running it again (decision 0078's amendment of 2026-11-20): "answered from saved run
    /// <id>". Empty for an answer just computed; absent from an older server's frame, and skipped when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answered_from_saved_run: String,
}

/// A walk-forward as the Studio shows it: the whole answer with its stitched out-of-sample curve
/// thinned by [`downsample`]. A step carries everything known of it (its range, its own results and
/// the parameters it chose), and `overall` the walk's results (its data warnings among them), so
/// there is no step detail to ask for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalkforwardSummaryWire {
    /// Every step, in split order.
    pub steps: Vec<WalkforwardResultsStep>,
    /// The whole walk's results over the stitched curve, computed from the WHOLE curve.
    pub overall: TradingResults,
    /// The stitched out-of-sample equity curve, thinned: `(step index, equity)` pairs (plain msgpack, as
    /// [`RunSummaryWire::curve`]).
    pub equity_curve: Vec<(usize, f64)>,
    /// Each [`Self::equity_curve`] point's timestamp (epoch ms), as [`RunSummaryWire::curve_timestamp`]; empty when
    /// the walk kept none, and absent from an older server's frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub equity_curve_timestamp: Vec<i64>,
    /// How many samples the stitched curve has.
    pub equity_curve_len: usize,
    /// The share of steps profitable out of sample.
    pub profitable_step_share: f64,
    /// What the windows were costed under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_model: Option<CostModelStampWire>,
    /// The id of the saved run this answer was read back from, when the backtesthub answered an IDENTICAL request from
    /// the run it saved instead of running it again (decision 0078's amendment of 2026-11-20): "answered from saved run
    /// <id>". Empty for an answer just computed; absent from an older server's frame, and skipped when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answered_from_saved_run: String,
}

impl WalkforwardSummaryWire {
    /// The whole walk-forward answer, its curve thinned.
    pub fn of(whole: WalkforwardResultsWire) -> Self {
        let equity_curve = downsample(&whole.equity_curve, CURVE_POINTS);
        Self {
            equity_curve_timestamp: kept_timestamps(
                &equity_curve,
                &whole.equity_timestamp,
                whole.equity_curve.len(),
            ),
            equity_curve,
            equity_curve_len: whole.equity_curve.len(),
            steps: whole.steps,
            overall: whole.overall,
            profitable_step_share: whole.profitable_step_share,
            cost_model: whole.cost_model,
            answered_from_saved_run: String::new(),
        }
    }
}

/// One page of a held run's trades, in close order.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TradesPageWire {
    /// How many trades the run holds in all.
    pub total: usize,
    /// Which page this is, from 0.
    pub page: usize,
    /// The page size it was cut at.
    pub page_size: usize,
    /// The page's trades. Columns in a msgpack body ([`crate::msgpack_body`]).
    #[serde(with = "crate::msgpack_body::table")]
    pub trades: Vec<Trade>,
}

impl TradesPageWire {
    /// Page `page` of `trades`, `page_size` a page (0 means [`TRADES_PAGE_SIZE`], and more than
    /// [`TRADES_PAGE_SIZE_MAX`] is held there). A page past the end is empty.
    pub fn of(trades: &[Trade], page: usize, page_size: usize) -> Self {
        let page_size = match page_size {
            0 => TRADES_PAGE_SIZE,
            n => n.min(TRADES_PAGE_SIZE_MAX),
        };
        let start = page.saturating_mul(page_size).min(trades.len());
        let end = start.saturating_add(page_size).min(trades.len());
        Self { total: trades.len(), page, page_size, trades: trades[start..end].to_vec() }
    }
}

/// One page of a run's WHOLE equity curve, in step order (`Request::ReadCurve`'s answer, decision 0078's
/// amendment of 2026-11-18): the pages of a curve, put end to end, are the curve the run computed, bit
/// for bit, and no frame carries more than one page of it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CurvePageWire {
    /// How many steps the whole curve has.
    pub total: usize,
    /// Which page this is, from 0.
    pub page: usize,
    /// The page size it was cut at.
    pub page_size: usize,
    /// The page's equities. One column in a msgpack body ([`crate::msgpack_body`]).
    #[serde(with = "crate::msgpack_body::f64_column")]
    pub equity: Vec<f64>,
    /// The page's timestamps, aligned to [`Self::equity`]; empty when the run tracked none. One column in a
    /// msgpack body.
    #[serde(with = "crate::msgpack_body::i64_column")]
    pub timestamp: Vec<i64>,
}

impl CurvePageWire {
    /// Page `page` of the curve `equity` (with its `timestamp`s, or none), `page_size` steps a page (0
    /// means [`CURVE_PAGE_SIZE`], and more than [`CURVE_PAGE_SIZE_MAX`] is held there). A page past the
    /// end is empty.
    pub fn of(equity: &[f64], timestamp: &[i64], page: usize, page_size: usize) -> Self {
        let page_size = match page_size {
            0 => CURVE_PAGE_SIZE,
            n => n.min(CURVE_PAGE_SIZE_MAX),
        };
        let cut = |len: usize| {
            let start = page.saturating_mul(page_size).min(len);
            start..start.saturating_add(page_size).min(len)
        };
        Self {
            total: equity.len(),
            page,
            page_size,
            equity: equity[cut(equity.len())].to_vec(),
            timestamp: timestamp[cut(timestamp.len())].to_vec(),
        }
    }
}

/// The rows a page of a summarized parameter search carries when the caller names no size
/// ([`ParamscanRowsPageWire`]): a row's metric table is at most about 3 kB of JSON (99 catalog numbers with the
/// closed-trade block), so a page is at most about 3 MB.
pub const PARAMSCAN_ROWS_PAGE_SIZE: usize = 1000;

/// The most rows one page of a summarized parameter search may carry, whatever the caller asks for: about 15 MB
/// of JSON, a quarter of the hub frame cap (`vike_hub_protocol::frame::MAX_FRAME_LEN`).
pub const PARAMSCAN_ROWS_PAGE_SIZE_MAX: usize = 5_000;

/// The most points one exhaustive parameter grid may expand to, each a backtest: ten pages of
/// [`PARAMSCAN_ROWS_PAGE_SIZE_MAX`] rows. The harness refuses a larger grid in words before it allocates one
/// (`vike_backtest::search::paramscan::expand_paramscan_overrides`), and the Studio refuses to send one. Two
/// rows of `1..5000` were 25 million points and gigabytes on the backtest server.
pub const PARAMSCAN_POINTS_MAX: usize = 50_000;

/// **One point of a SUMMARIZED parameter search** (decision 0078's amendment of 2026-11-19, the owner's "do 1-7"):
/// `vike_backtest::search::paramscan::ParamscanRow` without its trades, curves and lists. The keys are that row's, so a
/// reader of the whole report's `rows[].overrides`, `rows[].report.<metric>`, `rows[].error` and `rows[].score` reads
/// these too; only the report's nested blocks are gone. The whole point is the saved run's
/// ([`ParamscanProfileSummaryWire::saved_run`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamscanRowSummaryWire {
    /// The `(name, value)` overrides that produced this point.
    pub overrides: Vec<(String, ParamValue)>,
    /// The point's metric table: every metric-catalog number its report carries, keyed by catalog id
    /// (`vike_analytics::TradingResultsTable::from_report`), so a search ranks and prints its Sharpe, return,
    /// drawdown, trade count, profit factor and the rest without its trades. `None` for a point that failed.
    pub report: Option<TradingResultsTable>,
    /// Why the point failed; `None` when it ran.
    pub error: Option<String>,
    /// The objective's score of the point, when an objective ranked the search (the whole row's `score`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// **A SUMMARIZED parameter search of a profile** (`Request::Summarized` around `Request::RunParamscanProfile`): the
/// whole `ParamscanReport`'s keys but its rows, the first page of its rows summarized ([`ParamscanRowSummaryWire`],
/// best first) and where the rest is. The backtesthub holds every row under [`Self::run`] for
/// `Request::ReadParamscanRows`, and saves the whole report, every point with its trades, as [`Self::saved_run`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamscanProfileSummaryWire {
    /// The run the backtesthub holds the rows under (`Request::ReadParamscanRows`).
    pub run: String,
    /// The id of the run the backtesthub SAVED: its `report.json` is the whole report, its `rows` in this order, so
    /// point `i` of the search is that document's `rows[i]` (`vike-cli backtest show <saved_run>`). Empty when nothing
    /// was saved.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub saved_run: String,
    /// How many points the search has.
    pub total: usize,
    /// The page size [`Self::rows`] was cut at: page `n` of `Request::ReadParamscanRows` at this size holds rows
    /// `n * page_size ..`.
    pub page_size: usize,
    /// The first page of the points, best first. Columns in a msgpack body ([`crate::msgpack_body`]).
    #[serde(with = "crate::msgpack_body::table")]
    pub rows: Vec<ParamscanRowSummaryWire>,
    /// What ranked the rows, as the whole report spells it.
    pub rank_by: String,
    /// The search method's own cost line, as the whole report carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The currency of every money cell when it is not the dollar, as the whole report carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_currency: Option<String>,
    /// The strategy file every point ran and the sha256 of its text, as the whole report carries them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy_sha256: String,
    /// The SDK the file's artifact was built against, as the whole report carries it; `None` (not written) for a
    /// `.rhai` file, a development tree's build and an older backtesthub's summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_sdk: Option<u32>,
    /// What the search found wrong with its data and ran anyway: every point reads one slice, so the first row's
    /// that carries any speaks for all of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_warnings: Vec<String>,
    /// What the strategy's hooks failed with over EVERY point together (each row's report's `strategy_failures`,
    /// merged), since a summarized row carries its metric table alone; each point's own is in the saved run's
    /// rows. Skipped when nothing failed, and absent from an older backtesthub's summary.
    #[serde(default, skip_serializing_if = "vike_model::StrategyFailures::is_empty")]
    pub strategy_failures: vike_model::StrategyFailures,
    /// [`RunSummaryWire::answered_from_saved_run`]: the saved run this search's answer was read back from, its
    /// [`Self::saved_run`] then too. Empty for a search just run.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answered_from_saved_run: String,
}

/// One page of a summarized parameter search's rows (`Request::ReadParamscanRows`'s answer), best first.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ParamscanRowsPageWire {
    /// How many points the search has.
    pub total: usize,
    /// Which page this is, from 0.
    pub page: usize,
    /// The page size it was cut at.
    pub page_size: usize,
    /// The page's rows. Columns in a msgpack body ([`crate::msgpack_body`]).
    #[serde(with = "crate::msgpack_body::table")]
    pub rows: Vec<ParamscanRowSummaryWire>,
}

impl ParamscanRowsPageWire {
    /// Page `page` of `rows`, `page_size` a page (0 means [`PARAMSCAN_ROWS_PAGE_SIZE`], and more than
    /// [`PARAMSCAN_ROWS_PAGE_SIZE_MAX`] is held there). A page past the end is empty.
    pub fn of(rows: &[ParamscanRowSummaryWire], page: usize, page_size: usize) -> Self {
        let page_size = match page_size {
            0 => PARAMSCAN_ROWS_PAGE_SIZE,
            n => n.min(PARAMSCAN_ROWS_PAGE_SIZE_MAX),
        };
        let start = page.saturating_mul(page_size).min(rows.len());
        let end = start.saturating_add(page_size).min(rows.len());
        Self { total: rows.len(), page, page_size, rows: rows[start..end].to_vec() }
    }
}

/// A whole run's answer → its summary and its trades, the metrics annualized at `periods_per_year`
/// (the slice's: `vike_analytics::results::periods_per_year_for`), over the record
/// [`from_wire_record`] decodes. The caller keeps the trades (the backtesthub holds them for
/// `Request::ReadTrades`; a named run's desktop keeps them in hand). A server that still holds the
/// run's full record summarizes it with [`summarize_record`].
pub fn summarize(whole: TradingRecordWire, periods_per_year: f64) -> (RunSummaryWire, Vec<Trade>) {
    summarize_from(whole, None, None, periods_per_year)
}

/// **[`summarize`] over the FULL record** `whole` was rendered from (`crate::wire_record::to_wire_record`),
/// so the summary shows what the run's saved report shows (decision 0078's amendment of 2026-11-19): its
/// closed-trade block, every simulator counter of its honesty block and zero-trade diagnosis, and every
/// refusal, which the decoded record leaves out or caps. `report` is `record`'s
/// `TradingResults::from_record(None, ..)`, unstamped, when the caller already computed it for the saved
/// report: it is then not computed again and `periods_per_year` is not read. `whole` gives the stamps and
/// the grouped refusals alone.
pub fn summarize_record(
    whole: TradingRecordWire,
    record: TradingRecord,
    report: Option<TradingResults>,
    periods_per_year: f64,
) -> (RunSummaryWire, Vec<Trade>) {
    summarize_from(whole, Some(record), report, periods_per_year)
}

/// [`summarize`] and [`summarize_record`]: over `record`, or the one `whole` decodes to.
fn summarize_from(
    whole: TradingRecordWire,
    record: Option<TradingRecord>,
    report: Option<TradingResults>,
    periods_per_year: f64,
) -> (RunSummaryWire, Vec<Trade>) {
    // A decoded record's refusals are the wire's grouped rows, whose counts stay exact past the decode's
    // expansion cap; a full record's are its report's own groups, with their times (below).
    let wire_groups = record
        .is_none()
        .then(|| group_denials(whole.refused_orders.iter().map(|(drop, n)| (drop, *n, None))));
    let cost_model = whole.cost_model.clone();
    let strategy_file = whole.strategy_file.clone();
    let strategy_sha256 = whole.strategy_sha256.clone();
    let mut result = record.unwrap_or_else(|| from_wire_record(whole));
    let curve = &result.equity_curve;
    // With the strategy file attached BEFORE the label is read, so the label names the file.
    // The Studio wire record carries no SDK (decision 0296 approved the field on the report only).
    let results = report
        .unwrap_or_else(|| TradingResults::from_record(None, &result, periods_per_year))
        .with_strategy(&strategy_file, &strategy_sha256, None);
    // The report's own Sharpe, return and PSR moments: the same functions over this very curve, so the same bits.
    let returns = metrics::returns(curve);
    let (sharpe, total_return) = (results.sharpe, results.total_return);
    let probabilistic_sharpe = match &results.extended {
        Some(extended) => extended.sharpe_moments(returns.len()).psr(),
        None => psr_of_curve(curve),
    };
    let source_label = results.source_label();
    let rows =
        results.rendered_rows().into_iter().map(|(id, text)| (id.to_string(), text)).collect();
    // The table alone for a comparison (`RunSummaryWire::results`): no trades and no curve. Its source and
    // period cross as `source_label` (the fields are `serde(skip)`), so the table equals its decoded copy.
    let mut table = results;
    table.source = None;
    table.period = None;
    if let Some(closed) = table.closed_trades.as_mut() {
        closed.trades.clear();
        closed.open_trades.clear();
        closed.buy_hold_curve.clear();
    }
    // The denials ONCE (decision 0078's amendment of 2026-11-19): in `refused`, which every reader of a summary
    // reads, and `refused_detail` beside it, taken out of the table's honesty block.
    let held = table.honesty.as_mut().map(|honesty| {
        (std::mem::take(&mut honesty.denials), std::mem::take(&mut honesty.denial_details))
    });
    let (refused, refused_detail) = wire_groups.or(held).unwrap_or_default();
    // A run whose equity reached zero carries no Sharpe, return, probability or return histogram.
    let invalid = result.equity_reached_zero();
    let valid = |v: Option<f64>| v.filter(|_| invalid.is_none());
    let thinned = downsample(curve, CURVE_POINTS);
    let summary = RunSummaryWire {
        run: String::new(),
        saved_run: String::new(),
        point: None,
        rows,
        sharpe: valid(finite(sharpe)),
        total_return: valid(finite(total_return)),
        final_equity: result.final_equity,
        n_trades: result.n_trades,
        probabilistic_sharpe: valid(finite(probabilistic_sharpe)),
        curve_timestamp: kept_timestamps(&thinned, &result.equity_timestamp, curve.len()),
        curve: thinned,
        curve_len: curve.len(),
        returns_histogram: match invalid {
            None => ReturnsHistogramWire::of(&returns),
            Some(_) => ReturnsHistogramWire::default(),
        },
        invalid,
        refused,
        refused_detail,
        data_warnings: std::mem::take(&mut result.data_warnings),
        cost_model,
        strategy_file,
        strategy_sha256,
        source_label,
        results: Some(table),
        answered_from_saved_run: String::new(),
    };
    (summary, result.trades)
}

/// `v` when finite, else `None`: JSON writes a non-finite number as `null` and cannot read it back
/// into an `f64`.
pub fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// **`curve` thinned to about `target` points**, as `(step index, value)` pairs in step order.
///
/// A curve of at most `target` points is returned whole. A longer one keeps:
/// - its first and last point;
/// - the lowest and the highest point of each of `target / 2` equal buckets, so every peak and dip
///   a chart could show stays where it was;
/// - the peak and the trough of its maximum drawdown, as `vike_analytics::metrics::max_drawdown`
///   finds them.
///
/// The last rule makes the thinned curve's maximum drawdown EQUAL the whole curve's: the running
/// peak over the kept points can only be lower than the whole curve's, so no kept point draws down
/// further than the whole curve did, and the kept peak and trough draw down exactly as far.
pub fn downsample(curve: &[f64], target: usize) -> Vec<(usize, f64)> {
    let n = curve.len();
    if n <= target.max(2) {
        return curve.iter().copied().enumerate().collect();
    }
    let buckets = (target / 2).max(1);
    let mut keep: Vec<usize> = Vec::with_capacity(buckets * 2 + 4);
    keep.push(0);
    keep.push(n - 1);
    for bucket in 0..buckets {
        let (start, end) = (bucket * n / buckets, (bucket + 1) * n / buckets);
        if start >= end {
            continue;
        }
        let (mut low, mut high) = (start, start);
        for i in start..end {
            if curve[i] < curve[low] {
                low = i;
            }
            if curve[i] > curve[high] {
                high = i;
            }
        }
        keep.push(low);
        keep.push(high);
    }
    if let Some((peak, trough)) = max_drawdown_span(curve) {
        keep.push(peak);
        keep.push(trough);
    }
    keep.sort_unstable();
    keep.dedup();
    keep.into_iter().map(|i| (i, curve[i])).collect()
}

/// The timestamp of each point [`downsample`] `kept` of a curve of `len` steps, from `timestamp`, the whole curve's
/// (aligned to it). Empty when `timestamp` is not one a step: a run that tracked none.
fn kept_timestamps(kept: &[(usize, f64)], timestamp: &[i64], len: usize) -> Vec<i64> {
    if timestamp.len() != len {
        return Vec::new();
    }
    kept.iter().map(|&(step, _)| timestamp[step]).collect()
}

/// The `(peak, trough)` step indices of `curve`'s maximum drawdown, found by
/// `vike_analytics::metrics::max_drawdown`'s own walk (the peak runs from the first point; a drawdown
/// counts only under a positive peak), or `None` when the curve never draws down.
fn max_drawdown_span(curve: &[f64]) -> Option<(usize, usize)> {
    let mut peak = 0;
    let mut worst = 0.0;
    let mut span = None;
    for (i, &v) in curve.iter().enumerate() {
        if v > curve[peak] {
            peak = i;
        }
        if let Some(drawdown) = vike_math::time_series::drawdown_fraction(curve[peak], v)
            && drawdown > worst
        {
            worst = drawdown;
            span = Some((peak, i));
        }
    }
    span
}

#[path = "summary_tests.rs"]
#[cfg(test)]
mod summary_tests;
