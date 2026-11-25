//! The tabbed results pane over a run's SUMMARY (`vike_backtesthub_client::summary::RunSummaryWire`).
//! Pure rendering + testable helpers (`validation_rows`, `validation_verdict_rows`, `refused_line`,
//! `metric_words`, `shown`); the tabs are `egui_plot` lines/bars + `egui::Grid` tables. Its parts: `header`
//! (the run line, the code line and the summary strip above the tabs), `equity` (the Equity tab) and
//! `walkforward` (the Walk-forward tab).
//!
//! # It SHOWS what the server computed, and derives only what it says it derives
//!
//! Since decision 0078's amendment of 2026-11-16 a Studio run answers its summary, not its whole
//! result: the backtesthub renders every metric-catalog row (`TradingResults::rendered_rows`, the rows
//! and the text the CLI, the HTML tearsheet and the desktop print), the PSR, the per-bar return
//! histogram and the annualized Sharpe over the WHOLE curve, and thins the equity curve to about two
//! thousand points (the thinned curve keeps the true maximum drawdown). This pane prints those rows
//! and draws those points. The trades are paged from the server ([`StudioResultsTradesPager`], two hundred a page);
//! a named run's arrive whole and are paged here.
//!
//! What it derives, each named where it is done: the drawdown under the thinned curve and the dates its axis
//! reads (`equity`'s module doc says how exact each is), the "Net result after fees" row (final equity minus
//! the curve's first, so it agrees with Total return), each trade's duration and its price return
//! (`vike_analytics::metrics::trade_return`).
//!
//! # How it WORDS what the server computed (the owner's screenshots of 2026-11-20)
//!
//! A row is named in words ([`metric_words`]: "Maximum drawdown", not `max_drawdown`; the catalog id and the
//! catalog's own description are its hover), and a percentage shows two decimals ([`shown`], [`percent`]): the
//! catalog renders four (`MetricUnit::Percent`), so the pane rounds that text and every other unit stays as the
//! catalog rendered it.
//!
//! The Validation tab reads the catalog's `returns_skewness`/`returns_kurtosis` (EXCESS kurtosis, the
//! CLI's) and the summary's PSR (`vike_analytics::overfit::psr_of_curve`, per OBSERVATION), and renders
//! the scan's own verdict rather than assembling one from the last scan and the last walk-forward,
//! which could be two different runs. Each block sits under a heading that says what it is about: the
//! point or run on screen, the whole scan, the walk-forward.
//!
//! # The annualization factor is the SLICE's
//!
//! Every annualized number here (Sharpe, Sortino, CAGR, Calmar, the scan table's ranking column) is
//! scaled by the count of return observations a year produces, one per BAR: the server derives it
//! from the run's own slice (`vike_analytics::results::periods_per_year_for`). It used to be applied
//! HERE, at render time, from whichever slice was picked when the pane drew, so picking another
//! interval after a run rescaled that run's numbers.
//!
//! ⚠ **A visible second-order effect, stated rather than discovered later.** `metrics::cagr` and
//! `metrics::calmar` return `0.0` when `periods_per_year / (bars - 1) > 1000`, a guard against
//! `growth^exponent` overflowing. A correct intraday factor is large, so a SHORT intraday run trips it.
//! The reading is honest — extrapolating twenty minutes to a year is not a growth rate — but the
//! sentinel PRINTS as `0.00%`, which reads like "flat" rather than "not annualizable". That sentinel
//! belongs to `vike_analytics::metrics`, not to this pane, so it is left alone here and named instead.
use vike_analytics::INVALID_LABEL;
use vike_analytics::metric_catalog::{MetricUnit, spec_for};
use vike_analytics::metrics::trade_return;
use vike_analytics::overfit::overfit_verdict;
use vike_analytics::zero_trade::denial_sentence;
use vike_backtesthub_client::summary::{
    ParamscanPointWire, ParamscanSummaryWire, RunSummaryWire, TRADES_PAGE_SIZE, TradesPageWire,
    WalkforwardSummaryWire,
};
use vike_model::Trade;
use vike_model::params::ParamValue;
use vike_ui_theme::components::tabs::{self, ThemeTab};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::maps;
use vike_ui_theme::metrics::space;
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::time_zone::{ThemeDisplayTimeZone, to_naive};

mod equity;
mod header;
mod walkforward;

pub(crate) use header::{StudioResultsCode, StudioResultsOrigin, code_line_ui, run_line_ui};
pub(crate) use walkforward::walkforward_view;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum StudioResultsTab {
    #[default]
    Equity,
    Trades,
    Performance,
    Distribution,
    Validation,
    /// The last walk-forward: its overall line, each window's row and the stitched out-of-sample curve.
    Walkforward,
}

/// The Trades tab's page: which run it is of, which page, and the page once it arrived.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StudioResultsTradesPager {
    /// The run and the scan point the page is of (`RunSummaryWire::run`, `RunSummaryWire::point`):
    /// a page of another run is never shown.
    pub(crate) of: (String, Option<usize>),
    /// The page asked for, from 0.
    pub(crate) page: usize,
    /// The page once it arrived, or why it did not; `None` while it is on its way (or, for trades
    /// in hand, always: the pane cuts those itself).
    pub(crate) shown: Option<Result<TradesPageWire, String>>,
}

/// What the pane asks its owner to do: it holds no connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StudioResultsAction {
    /// Show page `0` of the trades of the run on screen (fetch it, unless the trades are in hand).
    ShowTradesPage(usize),
    /// Open point `0` of the param scan on screen.
    OpenScanPoint(usize),
}

/// The Performance grid of a whole result: `(catalog id, rendered value)` for EVERY metric-catalog
/// row, in catalog order (`TradingResults::rendered_rows`). A summary carries the same rows
/// ([`RunSummaryWire::rows`]); this is for a whole result in hand (the Saved pane's comparison).
#[cfg(test)]
pub(crate) fn perf_rows(
    r: &vike_analytics::TradingRecord,
    periods_per_year: f64,
) -> Vec<(&'static str, String)> {
    vike_analytics::results::TradingResults::from_record(None, r, periods_per_year).rendered_rows()
}

/// The catalog rows the Validation tab shows for a single run, in this order: the annualized
/// Sharpe and the two return-shape moments PSR is sensitive to.
const VALIDATION_IDS: [&str; 3] = ["sharpe", "returns_skewness", "returns_kurtosis"];

/// The Validation tab's label for the PSR row — not a catalog metric, so it carries its own words.
pub(crate) const PSR_LABEL: &str = "Probability that the Sharpe is above 0 (PSR)";

/// A fraction as this pane shows a percentage: two decimals (`0.041937` → `4.19%`), the owner's rule of
/// 2026-11-20 for every percentage on the pane, catalog rows included ([`shown`]).
pub(crate) fn percent(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

/// A catalog row's text as this pane shows it: a `Percent` row at [`percent`]'s two decimals, read back from the
/// catalog's four; every other unit, and a text that is no number, exactly as the catalog rendered it.
pub(crate) fn shown(id: &str, text: &str) -> String {
    let is_percent = spec_for(id).is_some_and(|spec| matches!(spec.unit, MetricUnit::Percent));
    match text.strip_suffix('%').map(str::parse::<f64>) {
        Some(Ok(v)) if is_percent => percent(v / 100.0),
        _ => text.to_string(),
    }
}

/// The words this pane names a catalog row by (the owner's rule: words, no abbreviation and no identifier):
/// the ones written here, else the id with its underscores as spaces and its first letter capitalized. The id
/// itself stays the row's hover, beside the catalog's own description.
pub(crate) fn metric_words(id: &str) -> String {
    let words = match id {
        "total_return" => "Total return",
        "sharpe" => "Sharpe",
        "max_drawdown" => "Maximum drawdown",
        "n_trades" => "Trades closed",
        // The catalog sums the CLOSED trades' fees; a position still open at the end paid its entry fee out of the
        // equity, and that fee is in no trade yet (the simulator's `fill_cash`).
        "total_fees" => "Fees of closed trades",
        "final_equity" => "Final equity",
        // The catalog's `net_profit` is the closed trades' PnL BEFORE fees (`total_fees` is apart).
        "net_profit" => "Profit before fees",
        "returns_skewness" => "Skewness of returns",
        "returns_kurtosis" => "Kurtosis of returns",
        "returns_volatility" => "Volatility of returns",
        "cagr" => "Compound annual growth rate",
        "sqn" => "System quality number",
        "value_at_risk_95" => "Value at risk (95%)",
        "expected_shortfall_95" => "Expected shortfall (95%)",
        other => {
            let spaced = other.replace('_', " ");
            let mut chars = spaced.chars();
            let first = chars.next().map(char::to_uppercase);
            return first.map(|upper| upper.chain(chars).collect::<String>()).unwrap_or_default();
        }
    };
    words.to_string()
}

/// A catalog row as `(words, shown text)`, or `None` when the summary carries no such row.
fn row_cell(summary: &RunSummaryWire, id: &str) -> Option<(String, String)> {
    let (_, text) = summary.rows.iter().find(|(k, _)| k == id)?;
    Some((metric_words(id), shown(id, text)))
}

/// The single-run Validation rows: the summary's [`VALIDATION_IDS`] rows (catalog id, catalog text), then
/// the PSR (`None`, not finite, renders as the catalog renders a `NaN`). The tab words and rounds them.
///
/// The PSR is per OBSERVATION by contract (`overfit::psr_of_curve`), while the DISPLAYED `sharpe`
/// row is ANNUALIZED: a different quantity that must never be fed back into the PSR.
///
/// A run whose equity reached zero ([`RunSummaryWire::invalid`]) is its one INVALID row: no Sharpe,
/// no moment and no probability.
pub(crate) fn validation_rows(summary: &RunSummaryWire) -> Vec<(String, String)> {
    if let Some(why) = &summary.invalid {
        return vec![(INVALID_LABEL.to_string(), why.clone())];
    }
    let mut out: Vec<(String, String)> = VALIDATION_IDS
        .iter()
        .filter_map(|id| summary.rows.iter().find(|(k, _)| k == id).cloned())
        .collect();
    out.push((PSR_LABEL.to_string(), percent(summary.probabilistic_sharpe.unwrap_or(f64::NAN))));
    out
}

/// The Validation tab's first heading: what its first rows are about — the scan point on screen (its
/// parameters), else this run.
pub(crate) fn selected_heading(
    summary: &RunSummaryWire,
    sweep: Option<&ParamscanSummaryWire>,
) -> String {
    match summary.point.and_then(|i| sweep?.points.get(i)) {
        Some(point) => format!("Selected point: {}", params_label(&point.overrides)),
        None => "This run".to_string(),
    }
}

/// The overfit verdict on the last SCAN: its deflated Sharpe, its probability of backtest overfitting, and
/// `vike_analytics::overfit::overfit_verdict` over them with no walk-forward term. A walk-forward is never
/// folded in: it can be another strategy over another slice, and the tab shows it under its own heading. An
/// unassessed number reads "—" (not assessed — the verdict's own reason row says so, never a vote toward Low
/// risk).
pub(crate) fn validation_verdict_rows(
    sweep: Option<&ParamscanSummaryWire>,
) -> Vec<(&'static str, String)> {
    let assessed = |x: Option<f64>| x.map_or_else(|| "—".to_string(), percent);
    let mut rows: Vec<(&'static str, String)> = Vec::new();
    match sweep {
        Some(s) => {
            let v = overfit_verdict(s.pbo.unwrap_or(f64::NAN), s.dsr.unwrap_or(f64::NAN), None);
            rows.push(("Deflated Sharpe", assessed(s.dsr)));
            rows.push(("Probability of backtest overfitting (PBO)", assessed(s.pbo)));
            rows.push(("Overfitting risk", format!("{:?}", v.level)));
            rows.extend(v.reasons.into_iter().map(|reason| ("Note", reason)));
        }
        None => rows.push(("Overfitting risk", "— (run a param scan to assess it)".to_string())),
    }
    rows
}

/// How many distinct refusal reasons [`refused_line`] names before it counts the rest.
const REFUSED_REASONS_SHOWN: usize = 5;

/// The orders the run REFUSED, the line the pane shows above its tabs, or `None` when it refused
/// none: the count, then each distinct reason in the gate's own words, most frequent first
/// ([`RunSummaryWire::refused`], aggregated as the report's denials are). A reason is grouped with its
/// numbers lifted out, so each is shown with the numbers of the FIRST order it refused put back
/// (`vike_analytics::zero_trade::denial_sentence` over [`RunSummaryWire::refused_detail`]'s first example):
/// the template's own `#` is a hole, never a number to print. A summary with no example (an older server's)
/// shows "…" in each hole. A run that holds its first buy and closes nothing because the spot law refused its
/// shorts says so here, beside the flat curve.
pub(crate) fn refused_line(summary: &RunSummaryWire) -> Option<String> {
    let first_of = |i: usize| summary.refused_detail.get(i).and_then(|d| d.examples.first());
    let mut reasons: Vec<(String, u64)> = summary
        .refused
        .iter()
        .enumerate()
        .map(|(i, (template, n))| {
            let numbers = match first_of(i) {
                Some(first) => first.numbers.clone(),
                None => vec!["…".to_string(); template.len()],
            };
            (denial_sentence(template, &numbers), *n)
        })
        .collect();
    let total: u64 = reasons.iter().map(|(_, n)| n).sum();
    let orders = if total == 1 { "order" } else { "orders" };
    match reasons.as_slice() {
        [] => None,
        [(reason, n)] if *n > 1 && first_of(0).is_some() => {
            Some(format!("{total} {orders} refused, the first: {reason}"))
        }
        [(reason, _)] => Some(format!("{total} {orders} refused: {reason}")),
        _ => {
            reasons.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            let mut lines =
                vec![format!("{total} {orders} refused (each reason as its first order read):")];
            let shown = reasons.iter().take(REFUSED_REASONS_SHOWN);
            lines.extend(shown.map(|(reason, n)| format!("  {n} × {reason}")));
            let more = reasons.len().saturating_sub(REFUSED_REASONS_SHOWN);
            if more > 0 {
                let word = if more == 1 { "reason" } else { "reasons" };
                lines.push(format!("  … and {more} more {word}"));
            }
            Some(lines.join("\n"))
        }
    }
}

const RESULT_TABS: [ThemeTab<'static, StudioResultsTab>; 6] = [
    ThemeTab { value: StudioResultsTab::Equity, label: "Equity", count: None },
    ThemeTab { value: StudioResultsTab::Trades, label: "Trades", count: None },
    ThemeTab { value: StudioResultsTab::Performance, label: "Performance", count: None },
    ThemeTab { value: StudioResultsTab::Distribution, label: "Distribution", count: None },
    ThemeTab { value: StudioResultsTab::Validation, label: "Validation", count: None },
    ThemeTab { value: StudioResultsTab::Walkforward, label: "Walk-forward", count: None },
];

/// The results' one label, built from the record (`RunSummaryWire::source_label`): the words `vike-cli report
/// run:` heads the same run with (the owner: every view labels a result alike). A scan point's record names no
/// file (`(unnamed)`), so its label names the file the scan was sent with (`file`, empty for an editor text saved
/// to none) and the point's parameter values, which the scan carries: the client sent both.
pub(crate) fn results_label(
    summary: &RunSummaryWire,
    sweep: Option<&ParamscanSummaryWire>,
    file: &str,
) -> Option<String> {
    let label = summary.source_label.as_deref()?;
    let point = summary.point.and_then(|i| sweep?.points.get(i));
    Some(match point {
        Some(point) => {
            let values = crate::panes::param_scan::point_label(point);
            let named = if file.is_empty() { String::new() } else { format!(" {file}") };
            format!("{} · {values}", label.replacen(" (unnamed)", &named, 1))
        }
        None => label.to_string(),
    })
}

/// The whole results pane: the results' label ([`results_label`]), the refused orders when there are any
/// ([`refused_line`]), the summary strip, the tab strip, and whichever tab is selected. `trades` is the run's
/// trades when they are in hand (a named run), `pager` the Trades tab's page otherwise; `scan` the last scan
/// with the file it was sent with; `walkforward` the last walk-forward with the parameter values its request
/// carried. Answers what the owner must do: fetch a page of trades, or open another point of the scan.
pub(crate) fn results_ui(
    ui: &mut egui::Ui,
    tab: &mut StudioResultsTab,
    summary: &RunSummaryWire,
    trades: Option<&[Trade]>,
    pager: &StudioResultsTradesPager,
    scan: Option<(&ParamscanSummaryWire, &str)>,
    walkforward: Option<(&WalkforwardSummaryWire, &[(String, ParamValue)])>,
) -> Option<StudioResultsAction> {
    let sweep = scan.map(|(sweep, _)| sweep);
    ui.add_space(space::XS);
    if let Some(label) = results_label(summary, sweep, scan.map_or("", |(_, file)| file)) {
        ui.label(egui::RichText::new(label).strong());
    }
    if let Some(line) = refused_line(summary) {
        ui.label(egui::RichText::new(line).color(ThemeStatus::Warning.color()));
    }
    header::strip_ui(ui, summary);
    tabs::underline(ui, tab, &RESULT_TABS);
    ui.add_space(space::XS);
    match tab {
        StudioResultsTab::Equity => {
            equity::equity_tab(ui, summary);
            None
        }
        StudioResultsTab::Trades => trades_tab(ui, summary, trades, pager),
        StudioResultsTab::Performance => {
            performance_tab(ui, summary);
            None
        }
        StudioResultsTab::Distribution => {
            distribution_tab(ui, summary);
            None
        }
        StudioResultsTab::Validation => {
            validation_tab(ui, summary, sweep, walkforward.map(|(w, _)| w))
        }
        StudioResultsTab::Walkforward => {
            walkforward::walkforward_tab(ui, walkforward);
            None
        }
    }
}

/// The page of `summary`'s trades the pager stands on, or `None` when it must be fetched first.
fn page_of<'a>(
    summary: &RunSummaryWire,
    trades: Option<&'a [Trade]>,
    pager: &'a StudioResultsTradesPager,
    page: usize,
) -> Option<Result<&'a [Trade], &'a str>> {
    if let Some(all) = trades {
        let start = (page * TRADES_PAGE_SIZE).min(all.len());
        let end = (start + TRADES_PAGE_SIZE).min(all.len());
        return Some(Ok(&all[start..end]));
    }
    if pager.of != (summary.run.clone(), summary.point) || pager.page != page {
        return None;
    }
    match &pager.shown {
        Some(Ok(shown)) if shown.page == page => Some(Ok(shown.trades.as_slice())),
        Some(Err(why)) => Some(Err(why.as_str())),
        _ => None,
    }
}

/// An epoch-ms instant as the pane prints a time, UTC to the minute; "—" for `0`, a trade's time when the
/// run recorded none (`Trade::entry_timestamp`'s default).
pub(crate) fn utc_minute(ms: i64) -> String {
    if ms == 0 {
        return "—".to_string();
    }
    to_naive(ms, ThemeDisplayTimeZone::Utc)
        .map_or_else(|| ms.to_string(), |t| t.format("%Y-%m-%d %H:%M").to_string())
}

/// A span of `ms` milliseconds in words: its two largest units that follow one another (`"3 days 4 hours"`,
/// `"12 minutes"`, `"2 hours"` for two hours and five seconds), or the milliseconds under a second.
pub(crate) fn duration_words(ms: i64) -> String {
    let units = [
        ("day", vike_model::MS_PER_DAY),
        ("hour", vike_model::MS_PER_HOUR),
        ("minute", vike_model::MS_PER_MINUTE),
        ("second", vike_model::MS_PER_SECOND),
    ];
    let (mut rest, mut parts) = (ms, Vec::new());
    for (word, size) in units {
        let n = rest / size;
        rest %= size;
        match (n, parts.is_empty()) {
            (0, true) => continue,
            (0, false) => break,
            _ => parts.push(format!("{n} {word}{}", if n == 1 { "" } else { "s" })),
        }
        if parts.len() == 2 {
            break;
        }
    }
    if parts.is_empty() { format!("{ms} milliseconds") } else { parts.join(" ") }
}

/// A trade's duration in words, or "—" when the run recorded no time for either end.
fn trade_duration(t: &Trade) -> String {
    if t.entry_timestamp == 0 || t.exit_timestamp == 0 {
        return "—".to_string();
    }
    duration_words(t.exit_timestamp - t.entry_timestamp)
}

/// The Trades tab's column headers, in order.
const TRADE_COLUMNS: [&str; 11] = [
    "#",
    "Side",
    "Entry time (UTC)",
    "Exit time (UTC)",
    "Duration",
    "Entry price",
    "Exit price",
    "Size",
    "Return",
    "Profit before fees",
    "Fees",
];

/// The Trades tab: one page of [`TRADES_PAGE_SIZE`] trades, with Previous / Next. A page not in hand
/// is asked for ([`StudioResultsAction::ShowTradesPage`]) while "Loading trades…" stands in for it.
/// Each trade's times are the ones its row carries (`Trade::entry_timestamp`, `Trade::exit_timestamp`);
/// its Return is the price move in its own direction (`vike_analytics::metrics::trade_return`), before fees.
fn trades_tab(
    ui: &mut egui::Ui,
    summary: &RunSummaryWire,
    trades: Option<&[Trade]>,
    pager: &StudioResultsTradesPager,
) -> Option<StudioResultsAction> {
    let tk = ThemeTokens::of(ui.ctx());
    let mine = pager.of == (summary.run.clone(), summary.point);
    let page = if mine { pager.page } else { 0 };
    let pages = summary.n_trades.div_ceil(TRADES_PAGE_SIZE).max(1);
    let mut action = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(page > 0, egui::Button::new("‹ Previous"))
            .on_disabled_hover_text("this is the first page")
            .clicked()
        {
            action = Some(StudioResultsAction::ShowTradesPage(page - 1));
        }
        let first = (page * TRADES_PAGE_SIZE + 1).min(summary.n_trades);
        let last = ((page + 1) * TRADES_PAGE_SIZE).min(summary.n_trades);
        ui.label(format!("Trades {first}–{last} of {}", summary.n_trades));
        if ui
            .add_enabled(page + 1 < pages, egui::Button::new("Next ›"))
            .on_disabled_hover_text("this is the last page")
            .clicked()
        {
            action = Some(StudioResultsAction::ShowTradesPage(page + 1));
        }
    });
    let rows = match page_of(summary, trades, pager, page) {
        Some(Ok(rows)) => rows,
        Some(Err(why)) => {
            ui.label(egui::RichText::new(why).color(ThemeStatus::Error.color()));
            return action;
        }
        None => {
            ui.label("Loading trades…");
            return action.or(Some(StudioResultsAction::ShowTradesPage(page)));
        }
    };
    // Solid bars: egui's floating ones show only under a hovering pointer, so a narrow pane read as a table
    // cut off at its edge.
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    egui::ScrollArea::both().max_width(to_pane_edge(ui)).show(ui, |ui| {
        egui::Grid::new("trades").striped(true).show(ui, |ui| {
            for h in TRADE_COLUMNS {
                ui.strong(h);
            }
            ui.end_row();
            for (i, t) in rows.iter().enumerate() {
                ui.label((page * TRADES_PAGE_SIZE + i + 1).to_string());
                // Side, Return and the result are the glance columns — color them by direction/sign.
                if t.is_long {
                    ui.colored_label(maps::side::LONG.text.resolve(&tk), "long");
                } else {
                    ui.colored_label(maps::side::SHORT.text.resolve(&tk), "short");
                }
                ui.monospace(utc_minute(t.entry_timestamp));
                ui.monospace(utc_minute(t.exit_timestamp));
                ui.monospace(trade_duration(t));
                ui.monospace(format!("{:.2}", t.entry_price));
                ui.monospace(format!("{:.2}", t.exit_price));
                ui.monospace(format!("{:.4}", t.size));
                let moved = trade_return(t);
                let moved_color = ThemeSidePair::GainLoss.text(moved >= 0.0, &tk);
                ui.label(egui::RichText::new(percent(moved)).monospace().color(moved_color));
                let pnl_color = ThemeSidePair::GainLoss.text(t.pnl >= 0.0, &tk);
                ui.label(egui::RichText::new(format!("{:.2}", t.pnl)).monospace().color(pnl_color));
                ui.monospace(format!("{:.2}", t.fees));
                ui.end_row();
            }
        });
    });
    action
}

/// The width from here to the pane's own right edge, its clip: a line wider than the pane above (the
/// tab strip in a narrow pane) widens the Ui past that edge, and a scroll area that wide ended under the
/// next panel, its last column out of reach (the Trades tab's Fees, the owner's pass of 2026-11-21).
pub(crate) fn to_pane_edge(ui: &egui::Ui) -> f32 {
    ui.clip_rect().right() - ui.available_rect_before_wrap().left()
}

/// The Performance tab's row of its own, beside `net_profit` (which is BEFORE fees): the final equity minus the
/// curve's first, so every fee, funding payment and position still open at the end is in it and it agrees with
/// Total return. `None` for a run with no curve.
pub(crate) fn net_result_row(summary: &RunSummaryWire) -> Option<(String, String)> {
    let &(_, start) = summary.curve.first()?;
    Some((
        "Net result after fees".to_string(),
        MetricUnit::Money.render(summary.final_equity - start),
    ))
}

/// Every catalog row in words ([`metric_words`], the id and the catalog's description on hover), scrolled
/// (the catalog is longer than the pane is tall), with [`net_result_row`] after `net_profit`. A value whose
/// rendered text is negative reads in the market set's DOWN colour and every other one stays neutral — the
/// desktop tearsheet's rule (`crates/vike-account-window/src/history/summary.rs`'s `tearsheet_rows`),
/// keyed on what the catalog printed rather than on a row list of this pane's: a positive
/// `max_drawdown` or `ulcer_index` is not good news, while a negative number in this catalog never is.
fn performance_tab(ui: &mut egui::Ui, summary: &RunSummaryWire) {
    let tk = ThemeTokens::of(ui.ctx());
    let net = net_result_row(summary);
    let row = |ui: &mut egui::Ui, words: &str, text: &str, hover: Option<String>| {
        let label = ui.label(words);
        if let Some(hover) = hover {
            label.on_hover_text(hover);
        }
        let value = egui::RichText::new(text).monospace();
        if text.starts_with('-') {
            ui.label(value.color(ThemeSidePair::PositiveNegative.text(false, &tk)));
        } else {
            ui.label(value);
        }
        ui.end_row();
    };
    egui::ScrollArea::vertical().id_salt("perf-scroll").show(ui, |ui| {
        egui::Grid::new("perf").striped(true).show(ui, |ui| {
            for (id, text) in &summary.rows {
                let hover = spec_for(id).map(|spec| format!("{id}: {}", spec.what));
                row(ui, &metric_words(id), &shown(id, text), hover);
                if id == "net_profit"
                    && let Some((words, text)) = &net
                {
                    let why = "final equity minus starting equity: every fee, funding payment and \
                               position still open at the end is in it, so it agrees with Total return";
                    row(ui, words, text, Some(why.to_string()));
                }
            }
        });
    });
}

/// The per-bar return histogram the server binned (`ReturnsHistogramWire`).
///
/// ⚠ EMPTY IS A REAL CASE: a curve shorter than two points has no return, and a histogram drawn from
/// nothing put every bar centre at `inf`, which `egui_plot`'s bounds check PANICS on (`Bad final plot
/// bounds`, measured on egui_plot 0.37) and took the Studio down. An empty histogram paints the empty
/// plot frame (axes and grid, no bars), the honest rendering of "no returns to distribute".
fn distribution_tab(ui: &mut egui::Ui, summary: &RunSummaryWire) {
    use egui_plot::{Bar, BarChart, Plot};
    // Returns across a zero crossing of the equity are no distribution of anything: say so instead.
    if let Some(why) = &summary.invalid {
        let line = format!("{INVALID_LABEL}: {why}");
        ui.label(egui::RichText::new(line).color(ThemeStatus::Warning.color()));
        return;
    }
    let histogram = &summary.returns_histogram;
    let bars: Vec<Bar> = histogram
        .counts
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            Bar::new(histogram.low + (i as f64 + 0.5) * histogram.width, c as f64)
                .width(histogram.width * 0.9)
        })
        .collect();
    Plot::new("dist")
        .height(ui.available_height())
        .show(ui, |p| p.bar_chart(BarChart::new("returns", bars)));
}

/// A block heading on the Validation tab: what the rows under it are about.
fn block_heading(ui: &mut egui::Ui, words: &str) {
    ui.add_space(space::LG);
    ui.label(egui::RichText::new(words).strong());
}

/// The Validation tab, in blocks under headings that say what each is about (the owner's screenshot of
/// 2026-11-20 read a point's Sharpe beside the whole scan's verdict as one contradiction): the point or run on
/// screen ([`selected_heading`]), the whole scan ([`validation_verdict_rows`]) and the last walk-forward.
fn validation_tab(
    ui: &mut egui::Ui,
    summary: &RunSummaryWire,
    sweep: Option<&ParamscanSummaryWire>,
    walkforward: Option<&WalkforwardSummaryWire>,
) -> Option<StudioResultsAction> {
    block_heading(ui, &selected_heading(summary, sweep));
    egui::Grid::new("validation_grid").num_columns(2).striped(true).show(ui, |ui| {
        for (k, v) in validation_rows(summary) {
            match spec_for(&k) {
                Some(_) => ui.label(metric_words(&k)),
                None => ui.label(k.as_str()),
            };
            ui.label(shown(&k, &v));
            ui.end_row();
        }
    });
    // An INVALID run shows no verdict (a deflated Sharpe and a PBO are probabilities). The scan's table
    // is the Param scan pane's (`crate::panes::param_scan`), beside the button that ran it.
    if summary.invalid.is_none() {
        let scan = match sweep {
            Some(s) => format!("Whole scan ({} points)", s.points.len()),
            None => "Overfitting".to_string(),
        };
        block_heading(ui, &scan);
        egui::Grid::new("validation_verdict_grid").num_columns(2).striped(true).show(ui, |ui| {
            for (k, v) in validation_verdict_rows(sweep) {
                ui.label(k);
                ui.label(v);
                ui.end_row();
            }
        });
    }
    if let Some(w) = walkforward {
        block_heading(ui, &format!("Walk-forward ({} windows)", w.steps.len()));
        egui::Grid::new("validation_walkforward_grid").num_columns(2).striped(true).show(
            ui,
            |ui| {
                ui.label("Profitable windows");
                ui.label(percent(w.profitable_step_share));
                ui.end_row();
            },
        );
        ui.label(
            egui::RichText::new("Its windows and its curve are on the Walk-forward tab.").weak(),
        );
    }
    None
}

/// One catalog metric's cell: `v` as that row's own `MetricUnit` renders it, so a scan row prints
/// a number exactly as the Performance tab and the CLI print the same metric.
fn cell(id: &str, v: f64) -> String {
    spec_for(id).map(|s| s.unit.render(v)).unwrap_or_default()
}

/// The text of a scan point's `sharpe`, `total_return` and `final_equity` cells, in their catalog units. A
/// point whose equity reached zero ([`ParamscanPointWire::invalid`]) reads INVALID in the first two, never a
/// number.
pub(crate) fn scan_cells(point: &ParamscanPointWire) -> [String; 3] {
    let ratio = |id: &str, v: Option<f64>| match point.invalid {
        Some(_) => INVALID_LABEL.to_string(),
        None => cell(id, v.unwrap_or(f64::NAN)),
    };
    [
        ratio("sharpe", point.sharpe),
        ratio("total_return", point.total_return),
        cell("final_equity", point.final_equity),
    ]
}

/// `k=v` join of parameter values, e.g. `"fast=5, slow=20"`: a scan point's overrides, a walk-forward window's
/// chosen parameters, each value as the Param scan pane prints it (`crate::panes::param_scan::value_text`).
fn params_label(params: &[(String, ParamValue)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", crate::panes::param_scan::value_text(v)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests;
