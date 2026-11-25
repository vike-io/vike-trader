use super::*;
use vike_analytics::results::{
    DAILY_PERIODS_PER_YEAR, DEFAULT_PERIODS_PER_YEAR, periods_per_year_for_interval,
};
use vike_analytics::{TradingRecord, metrics};
use vike_backtesthub_client::summary::{ReturnsHistogramWire, summarize};
use vike_backtesthub_client::wire_record::to_wire_record;
use vike_model::params::ParamValue;

/// The factor for the fixture curve below, which is sampled at 60s (`equity_timestamp`) — i.e. 1m
/// bars. Derived, never written as a number, so these tests read the same way the server does.
fn periods_per_year_1m() -> f64 {
    periods_per_year_for_interval("1m")
}

fn sample() -> TradingRecord {
    TradingRecord {
        equity_curve: vec![10_000.0, 10_100.0, 10_050.0, 10_200.0],
        equity_timestamp: vec![0, 60_000, 120_000, 180_000],
        final_equity: 10_200.0,
        n_trades: 2,
        ..Default::default()
    }
}

/// `r` as the backtesthub answers it: summarized at `periods_per_year`.
fn summary_of(r: &TradingRecord, periods_per_year: f64) -> RunSummaryWire {
    summarize(to_wire_record(r), periods_per_year).0
}

/// The Performance tab IS the catalog: every `METRICS` row in catalog order (`funding_paid`
/// included), each cell its unit's rendering — the text the CLI prints for the same run. A run that
/// did not arm closed-trade accounting carries no `MetricHome::ClosedTrades` row.
#[test]
fn perf_rows_are_the_whole_catalog_in_its_units() {
    use vike_analytics::metric_catalog::{METRICS, MetricHome, MetricUnit};
    let r = sample();
    let rows = summary_of(&r, periods_per_year_1m()).rows;
    let ids: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
    let catalog: Vec<&str> =
        METRICS.iter().filter(|m| m.home != MetricHome::ClosedTrades).map(|m| m.id).collect();
    assert_eq!(ids, catalog);
    let cell = |id: &str| rows.iter().find(|(k, _)| k == id).map(|(_, v)| v.clone()).unwrap();
    assert_eq!(
        cell("total_return"),
        MetricUnit::Percent.render(metrics::total_return(&r.equity_curve))
    );
    assert_eq!(cell("n_trades"), "2");
    assert_eq!(cell("funding_paid"), "0.00", "a perp run's funding cost is a row of its own");
    let whole: Vec<(String, String)> = perf_rows(&r, periods_per_year_1m())
        .into_iter()
        .map(|(id, text)| (id.to_string(), text))
        .collect();
    assert_eq!(rows, whole, "the summary's rows are the whole result's rows");
}

/// Render one headless frame and hand it to the shared geometry invariant.
///
/// ⚠ The `textures_delta.clear()` runs BEFORE the assertion, and that order is load-bearing —
/// the same trap `crates/vike-data-manager/src/test_support.rs`'s `run_frame` documents: asserting
/// first leaves the deltas unapplied, so dropping the frame during the assertion's unwind
/// panics a SECOND time in the destructor and the process ABORTS, printing a backtrace instead
/// of the coordinate that was wrong.
fn frame(f: impl FnMut(&mut egui::Ui)) {
    let context = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    let mut out = context.run_ui(raw, f);
    out.textures_delta.clear();
    vike_ui_theme::frame_sanity::assert_frame_sane(&out);
}

/// The results header shows the results' one label, as the summary carries it from the record
/// (`vike_analytics::results::TradingResults::source_label`): painted as its own text, on any tab.
#[test]
fn the_results_header_shows_the_results_label() {
    let summary = summary_of(&sample(), periods_per_year_1m());
    let label = summary.source_label.clone().expect("summarize labels the results");
    assert_eq!(label, "backtest (unnamed) 1970-01-01 00:00 – 1970-01-01 00:03 UTC");
    let pager = StudioResultsTradesPager::default();
    let mut tab = StudioResultsTab::Performance;
    let context = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };
    let mut out = context.run_ui(raw, |ui| {
        results_ui(ui, &mut tab, &summary, None, &pager, None, None);
    });
    out.textures_delta.clear();
    vike_ui_theme::frame_sanity::assert_frame_sane(&out);
    let painted = out.shapes.iter().any(
        |clipped| matches!(&clipped.shape, egui::Shape::Text(text) if text.galley.text() == label),
    );
    assert!(painted, "the header paints `{label}`");
}

/// THE KILL PROOF for the empty histogram: a curve shorter than two points has NO returns (a return
/// is a difference between two samples), so its summary's histogram is empty, and the
/// Distribution tab must paint the empty plot frame rather than bars centred at `inf`, which
/// `egui_plot`'s bounds check PANICS on (`Bad final plot bounds`, measured on egui_plot 0.37 when
/// the guard was removed from the old in-pane fold).
#[test]
fn a_degenerate_equity_curve_distributes_no_returns_and_paints_finite_geometry() {
    for equity_curve in [vec![10_000.0], Vec::new()] {
        let r = TradingRecord { equity_curve, ..Default::default() };
        let summary = summary_of(&r, periods_per_year_1m());
        assert_eq!(
            summary.returns_histogram,
            ReturnsHistogramWire::default(),
            "premise: this curve must yield NO returns, or the test proves nothing",
        );
        frame(|ui| distribution_tab(ui, &summary));
    }
}

/// The other half of the pair: a populated curve still reaches the binned bars.
#[test]
fn a_populated_equity_curve_still_reaches_the_binned_histogram() {
    let summary = summary_of(&sample(), periods_per_year_1m());
    assert_eq!(summary.returns_histogram.counts.iter().sum::<u64>(), 3, "every return binned");
    frame(|ui| distribution_tab(ui, &summary));
}

const SPOT: &str =
    "a spot account cannot sell more than it holds: it has 1, this sells 2; nothing was sent";

/// [`SPOT`]'s reason as a summary groups it, its numbers lifted out (decision 0078's amendment of 2026-11-19).
const SPOT_WORDS: &str =
    "a spot account cannot sell more than it holds: it has #, this sells #; nothing was sent";

fn refused(reasons: &[&str]) -> RunSummaryWire {
    let drop = |reason: &&str| vike_model::OrderDrop {
        symbol: "BTCUSDT".to_string(),
        reason: reason.to_string(),
        size: 2.0,
        weight: 1.0,
    };
    let dropped = reasons.iter().map(drop).collect();
    let simulator = vike_analytics::TradingRecordSimulator { dropped, ..Default::default() };
    let r = TradingRecord {
        source: vike_analytics::TradingRecordSource::Backtest(simulator),
        ..sample()
    };
    summary_of(&r, periods_per_year_1m())
}

/// A refused order is SEEN (the owner's ruling of 2026-11-12, part C): the line counts the run's
/// refused orders and names each reason in the gate's words, most frequent first, with the numbers of
/// the first order it refused put back where the grouping lifted them out (the owner's screenshot of
/// 2026-11-20 read "it has #, this sells #"); a run that refused none shows no line, so its pane is
/// the pane it was.
#[test]
fn the_refused_line_counts_the_orders_and_names_each_reason() {
    assert_eq!(refused_line(&summary_of(&sample(), periods_per_year_1m())), None);
    let one = refused(&[SPOT]);
    assert_eq!(one.refused, vec![(SPOT_WORDS.to_string(), 1)], "premise: the reason is grouped");
    assert_eq!(refused_line(&one), Some(format!("1 order refused: {SPOT}")));
    assert_eq!(
        refused_line(&refused(&[SPOT, SPOT, SPOT])),
        Some(format!("3 orders refused, the first: {SPOT}"))
    );
    assert_eq!(
        refused_line(&refused(&["insufficient-margin", SPOT, SPOT])),
        Some(format!(
            "3 orders refused (each reason as its first order read):\n  2 × {SPOT}\n  1 × insufficient-margin"
        ))
    );
    let seven = refused(&["a", "b", "c", "d", "e", "f", "g"]);
    let line = refused_line(&seven).expect("seven refusals");
    assert!(
        line.starts_with("7 orders refused (each reason as its first order read):\n  1 × a\n"),
        "{line}"
    );
    assert!(line.ends_with("\n  1 × e\n  … and 2 more reasons"), "{line}");
}

/// A summary whose reasons carry no example (an older server's) prints no `#`: each hole reads "…".
#[test]
fn a_refused_reason_without_an_example_prints_no_placeholder() {
    let summary =
        RunSummaryWire { refused: vec![(SPOT_WORDS.to_string(), 2)], ..Default::default() };
    let line = refused_line(&summary).expect("two refusals");
    assert_eq!(
        line,
        "2 orders refused: a spot account cannot sell more than it holds: it has …, this sells …; \
         nothing was sent"
    );
    assert!(!line.contains('#'), "{line}");
}

/// The line paints above the tabs on every tab without breaking the frame's geometry, and the
/// Trades tab with no page in hand asks for page 0 rather than painting rows it does not have.
#[test]
fn the_results_pane_with_refused_orders_paints_finite_geometry() {
    let summary = refused(&["insufficient-margin", SPOT, SPOT]);
    let pager = StudioResultsTradesPager::default();
    for mut tab in
        [StudioResultsTab::Equity, StudioResultsTab::Performance, StudioResultsTab::Distribution]
    {
        frame(|ui| {
            assert_eq!(results_ui(ui, &mut tab, &summary, None, &pager, None, None), None);
        });
    }
    let mut tab = StudioResultsTab::Trades;
    frame(|ui| {
        let asked = results_ui(ui, &mut tab, &summary, None, &pager, None, None);
        assert_eq!(asked, Some(StudioResultsAction::ShowTradesPage(0)));
    });
}

/// The Trades tab cuts trades in hand (a named run) itself, two hundred a page, and asks for none.
#[test]
fn trades_in_hand_page_here_and_ask_for_nothing() {
    let trade = |i: usize| Trade {
        entry_price: 100.0 + i as f64,
        exit_price: 101.0,
        size: 1.0,
        pnl: 1.0,
        fees: 0.1,
        entry_timestamp: i as i64,
        exit_timestamp: i as i64 + 1,
        symbol: "BTCUSDT".to_string(),
        mae: 0.0,
        mfe: 0.0,
        is_long: i.is_multiple_of(2),
    };
    let trades: Vec<Trade> = (0..450).map(trade).collect();
    let summary = RunSummaryWire { n_trades: 450, ..RunSummaryWire::default() };
    let on_page =
        |page: usize| StudioResultsTradesPager { of: (String::new(), None), page, shown: None };
    let pager = on_page(2);
    let cut = page_of(&summary, Some(&trades), &pager, 2).expect("in hand").expect("rows");
    assert_eq!(cut, &trades[400..450]);
    let mut tab = StudioResultsTab::Trades;
    frame(|ui| {
        let asked = results_ui(ui, &mut tab, &summary, Some(&trades), &on_page(1), None, None);
        assert_eq!(asked, None);
    });
}

/// A fetched page is shown only for the run and the page it is of.
#[test]
fn a_page_of_another_run_is_never_shown() {
    let summary = RunSummaryWire { run: "a-1".to_string(), n_trades: 5, ..Default::default() };
    let page = TradesPageWire { total: 5, page: 0, page_size: 200, trades: Vec::new() };
    let held = |run: &str| StudioResultsTradesPager {
        of: (run.to_string(), None),
        page: 0,
        shown: Some(Ok(page.clone())),
    };
    assert!(page_of(&summary, None, &held("a-1"), 0).is_some());
    assert!(page_of(&summary, None, &held("b-2"), 0).is_none());
    assert!(page_of(&summary, None, &held("a-1"), 1).is_none());
}

#[test]
fn validation_rows_include_psr() {
    // a synthetic upward-drifting equity curve -> positive Sharpe, computable PSR
    let r = TradingRecord {
        equity_curve: (0..260).map(|i| 10_000.0 * (1.0 + 0.0004 * i as f64)).collect(),
        final_equity: 10_000.0 * (1.0 + 0.0004 * 259.0),
        n_trades: 3,
        ..Default::default()
    };
    let summary = summary_of(&r, periods_per_year_1m());
    let rows = validation_rows(&summary);
    assert!(rows.iter().any(|(k, _)| k == PSR_LABEL), "PSR row present");
    assert!(rows.iter().any(|(k, _)| k == "sharpe"));
    // D6: the kurtosis shown is the catalog's EXCESS kurtosis, the very row the Performance tab
    // and the CLI print; the old non-excess "Kurtosis" (3 higher, same word) is gone.
    let kurt = rows.iter().find(|(k, _)| k == "returns_kurtosis").expect("the catalog row");
    assert!(summary.rows.contains(kurt), "{kurt:?} must be the Performance tab's own cell");
    assert!(!rows.iter().any(|(k, _)| k == "Kurtosis"));
}

/// The annualized rows move with the run's factor, and the rows that are NOT annualized are
/// untouched by it. (On this 4-point fixture the 1m factor trips `metrics::cagr`'s overflow guard
/// to `0.0`, the effect the module doc names; that it MOVED is what is asserted.)
#[test]
fn the_annualized_rows_scale_with_the_supplied_factor() {
    let r = sample();
    let daily = summary_of(&r, DAILY_PERIODS_PER_YEAR).rows;
    let minute = summary_of(&r, periods_per_year_1m()).rows;
    let cell = |rows: &[(String, String)], id: &str| -> String {
        rows.iter().find(|(k, _)| k == id).map(|(_, v)| v.clone()).expect("row present")
    };
    for id in ["sharpe", "sortino", "cagr", "calmar"] {
        assert_ne!(cell(&daily, id), cell(&minute, id), "{id} is annualized");
    }
    for id in ["total_return", "final_equity", "win_rate", "profit_factor", "n_trades"] {
        assert_eq!(cell(&daily, id), cell(&minute, id), "{id} is not an annualized metric");
    }
}

/// The daily ANCHOR: on a `1d` slice the rows are exactly those at the literal `252.0` the pane once
/// held, cell for cell.
#[test]
fn a_daily_slice_still_prints_the_pre_fix_numbers() {
    let r = sample();
    assert_eq!(
        summary_of(&r, 252.0).rows,
        summary_of(&r, periods_per_year_for_interval("1d")).rows
    );
}

/// A run with no bar interval (a tick slice) is annualized at `DEFAULT_PERIODS_PER_YEAR`
/// (`vike_analytics::results::periods_per_year_for`'s `None`), which keeps "the anchor it has always had"
/// only while it and `DAILY_PERIODS_PER_YEAR` are the same number.
#[test]
fn the_fallback_factor_is_the_daily_anchor() {
    assert_eq!(DEFAULT_PERIODS_PER_YEAR.to_bits(), DAILY_PERIODS_PER_YEAR.to_bits());
}

/// The Validation tab's split, pinned: the `sharpe` row is ANNUALIZED and moves with the factor;
/// the return-shape moments and the PSR are per-OBSERVATION and must not.
#[test]
fn the_annualized_row_moves_with_the_factor_while_the_psr_does_not() {
    let r = TradingRecord {
        equity_curve: (0..260).map(|i| 10_000.0 * (1.0 + 0.0004 * i as f64)).collect(),
        final_equity: 10_000.0 * (1.0 + 0.0004 * 259.0),
        n_trades: 3,
        ..Default::default()
    };
    let daily = validation_rows(&summary_of(&r, DAILY_PERIODS_PER_YEAR));
    let minute = validation_rows(&summary_of(&r, periods_per_year_1m()));
    let row = |rows: &[(String, String)], label: &str| -> String {
        rows.iter().find(|(k, _)| k == label).map(|(_, v)| v.clone()).expect("row present")
    };
    assert_ne!(
        row(&daily, "sharpe"),
        row(&minute, "sharpe"),
        "the displayed Sharpe is annualized and must follow the interval",
    );
    for label in ["returns_skewness", "returns_kurtosis", PSR_LABEL] {
        assert_eq!(
            row(&daily, label),
            row(&minute, label),
            "{label} is per-observation and must never see the annualization factor",
        );
    }
}

fn sweep_with(dsr: Option<f64>, pbo: Option<f64>) -> ParamscanSummaryWire {
    ParamscanSummaryWire {
        run: "a-1".to_string(),
        points: vec![ParamscanPointWire {
            overrides: vec![("fast".into(), ParamValue::Float(5.0))],
            sharpe: Some(1.0),
            total_return: Some(0.21),
            final_equity: 121.0,
            n_trades: 2,
            max_drawdown: None,
            invalid: None,
        }],
        best: None,
        dsr,
        pbo,
        best_index: 0,
        cost_model: None,
        answered_from_saved_run: String::new(),
    }
}

fn walk_with(profitable_step_share: f64) -> WalkforwardSummaryWire {
    let stitched = vike_analytics::TradingRecord {
        equity_curve: vec![100.0, 99.0],
        final_equity: 99.0,
        ..Default::default()
    };
    WalkforwardSummaryWire {
        steps: Vec::new(),
        overall: vike_analytics::results::TradingResults::from_record(None, &stitched, 252.0),
        equity_curve: vec![(0, 100.0), (1, 99.0)],
        equity_curve_timestamp: Vec::new(),
        equity_curve_len: 2,
        profitable_step_share,
        cost_model: None,
        answered_from_saved_run: String::new(),
    }
}

/// The verdict row naming the probability of backtest overfitting.
const PBO_ROW: &str = "Probability of backtest overfitting (PBO)";

#[test]
fn verdict_rows_reflect_dsr() {
    let sweep = sweep_with(Some(0.95), Some(0.1));
    let rows = validation_verdict_rows(Some(&sweep));
    assert!(rows.iter().any(|(k, _)| *k == "Deflated Sharpe"));
    assert!(rows.iter().any(|(k, _)| *k == "Overfitting risk"));
}

/// D7: the verdict is the SCAN's own (the trial ledger's rule, no walk-forward term): the rows take
/// no walk-forward at all, and the tab shows one under its own heading. With no scan there is no
/// verdict to show.
#[test]
fn the_scan_verdict_takes_no_walk_forward() {
    let risk = |rows: &[(&'static str, String)]| {
        rows.iter().find(|(k, _)| *k == "Overfitting risk").map(|(_, v)| v.clone()).unwrap()
    };
    assert_eq!(risk(&validation_verdict_rows(Some(&sweep_with(Some(0.95), Some(0.10))))), "Low");
    let no_sweep = validation_verdict_rows(None);
    assert!(risk(&no_sweep).contains("run a param scan"), "{no_sweep:?}");
}

#[test]
fn verdict_rows_include_a_pbo_percentage() {
    let sweep = sweep_with(Some(0.95), Some(0.10));
    let rows = validation_verdict_rows(Some(&sweep));
    let pbo = rows.iter().find(|(k, _)| *k == PBO_ROW).expect("PBO row present");
    assert_eq!(pbo.1, "10.00%", "the pane's two decimals");
}

#[test]
fn high_pbo_and_low_dsr_reads_high_risk() {
    let sweep = sweep_with(Some(0.20), Some(0.80)); // pbo>0.5 (+2) and dsr<0.5 (+2) -> High
    let rows = validation_verdict_rows(Some(&sweep));
    let risk = rows.iter().find(|(k, _)| *k == "Overfitting risk").expect("risk row");
    assert_eq!(risk.1, "High");
}

#[test]
fn an_unassessed_pbo_reads_em_dash_and_not_assessed() {
    let sweep = sweep_with(Some(0.95), None);
    let rows = validation_verdict_rows(Some(&sweep));
    let pbo = rows.iter().find(|(k, _)| *k == PBO_ROW).expect("PBO row present");
    assert_eq!(pbo.1, "—");
    assert!(
        rows.iter().any(|(_, v)| v.contains("not assessed")),
        "the verdict should carry a 'not assessed' note for an unassessed PBO"
    );
}

/// **A run whose equity reached zero is INVALID on every tab that would show a Sharpe or a probability**: the
/// Performance rows open with the INVALID row, the Validation tab is that one row, and its scan point's Sharpe and
/// return cells read INVALID (the final equity stays a number). The pane with such a point paints finite geometry.
#[test]
fn a_run_whose_equity_reached_zero_is_invalid_on_validation_performance_and_the_scan_table() {
    let r = TradingRecord {
        equity_curve: vec![10_000.0, 2_000.0, -500.0, 300.0],
        final_equity: 300.0,
        ..sample()
    };
    let summary = summary_of(&r, periods_per_year_1m());
    let why = "equity reached 0 on 1970-01-01: metrics are not meaningful".to_string();
    assert_eq!(summary.rows[0], (INVALID_LABEL.to_string(), why.clone()));
    assert!(!summary.rows.iter().any(|(k, _)| k == "sharpe"), "{:?}", summary.rows);
    assert_eq!(validation_rows(&summary), vec![(INVALID_LABEL.to_string(), why)]);
    let point = ParamscanPointWire::of(vec![("fast".into(), ParamValue::Float(5.0))], &summary);
    let [sharpe, total_return, final_equity] = scan_cells(&point);
    let cells = (sharpe.as_str(), total_return.as_str(), final_equity.as_str());
    assert_eq!(cells, (INVALID_LABEL, INVALID_LABEL, "300.00"));
    // The summary strip stays: a return or a ratio reads INVALID, the counts and the money stay numbers.
    assert!(summary.invalid.is_some(), "an INVALID summary");
    let cells = header::strip_cells(&summary);
    let values: Vec<&str> = cells.iter().map(|(_, v)| v.as_str()).collect();
    assert_eq!(values[..3], [INVALID_LABEL; 3], "return, Sharpe, drawdown: {cells:?}");
    assert_eq!(cells.last(), Some(&("Final equity".to_string(), "300.00".to_string())));
    // The drawdown pane stops at the whole of the peak: equity below zero is a 100% drawdown.
    let drops = super::equity::drawdowns(&summary);
    assert_eq!(drops.iter().copied().fold(0.0, f64::max), 1.0, "{drops:?}");
}

/// **The summary strip** reads the summary's own catalog rows, worded, a percentage at two decimals, in
/// strip order; a tick run kept by time adds its curve step.
#[test]
fn the_strip_shows_the_key_numbers_as_the_performance_tab_does() {
    let mut summary = summary_of(&sample(), periods_per_year_1m());
    let cells = header::strip_cells(&summary);
    let words: Vec<&str> = cells.iter().map(|(w, _)| w.as_str()).collect();
    assert_eq!(
        words,
        [
            "Total return",
            "Sharpe",
            "Maximum drawdown",
            "Trades closed",
            "Fees of closed trades",
            "Final equity"
        ]
    );
    assert_eq!(cells[0].1, "2.00%", "10,000 to 10,200, at two decimals");
    assert_eq!(cells[3].1, "2");
    assert_eq!(cells[5].1, "10200.00");
    summary.results.as_mut().expect("the summary's table").returns_interval_ms = Some(60_000);
    let cells = header::strip_cells(&summary);
    assert_eq!(cells.last(), Some(&("Curve step".to_string(), "1 minute".to_string())));
}

/// A percentage row rounds to two decimals; every other unit, and no number, stays the catalog's text.
#[test]
fn a_percentage_shows_two_decimals_and_nothing_else_moves() {
    assert_eq!(shown("total_return", "4.1937%"), "4.19%");
    assert_eq!(shown("max_drawdown", "-21.8440%"), "-21.84%");
    assert_eq!(shown("sharpe", "-0.5930"), "-0.5930");
    assert_eq!(shown("final_equity", "7815.60"), "7815.60");
    assert_eq!(shown(INVALID_LABEL, "equity reached 0%"), "equity reached 0%");
    assert_eq!(percent(0.041937), "4.19%");
}

/// Rows are named in words: the ones written down, else the id spelled out.
#[test]
fn a_row_is_named_in_words() {
    assert_eq!(metric_words("max_drawdown"), "Maximum drawdown");
    assert_eq!(metric_words("net_profit"), "Profit before fees");
    assert_eq!(metric_words("total_fees"), "Fees of closed trades");
    assert_eq!(metric_words("gross_profit"), "Gross profit");
    assert_eq!(metric_words(INVALID_LABEL), INVALID_LABEL);
}

/// "Net result after fees" is the final equity minus the curve's first, so it agrees with Total return.
#[test]
fn the_net_result_is_the_equity_change() {
    let summary = summary_of(&sample(), periods_per_year_1m());
    assert_eq!(
        net_result_row(&summary),
        Some(("Net result after fees".to_string(), "200.00".to_string()))
    );
    assert_eq!(net_result_row(&RunSummaryWire::default()), None);
}

/// The dates are read back from the results label and spread over the steps; a label without a span,
/// a one-step curve and a step outside the curve have none.
#[test]
fn the_dates_come_from_the_labels_span() {
    let label = "backtest a – b.rs 1970-01-01 00:00 – 1970-01-01 00:03 UTC";
    let span = equity::label_span(label);
    assert_eq!(span, Some((0, 180_000)));
    assert_eq!(equity::label_span("walk-forward step [0, 5)"), None);
    assert_eq!(equity::time_at(span, 4, 1.0), Some(60_000));
    assert_eq!(equity::time_at(span, 4, 3.0), Some(180_000));
    assert_eq!(equity::time_at(span, 4, -0.5), None, "the plot's margin");
    assert_eq!(equity::time_at(span, 1, 0.0), None);
}

/// The hover reads the nearest point's own time (the summary's `curve_timestamp`), its equity and its drawdown below
/// the peak so far; from an older server, which sends no times, the label's span spread over the steps, "about".
#[test]
fn the_hover_reads_date_equity_and_drawdown_at_the_pointer() {
    let summary = summary_of(&sample(), periods_per_year_1m());
    assert_eq!(summary.curve_timestamp, [0, 60_000, 120_000, 180_000], "the run's own times");
    let mut walk = vike_analytics::metrics::DrawdownWalk::default();
    let drops: Vec<f64> = summary.curve.iter().map(|&(_, e)| walk.push(e)).collect();
    assert_eq!(
        equity::hover_text(&summary, &drops, 2.4).as_deref(),
        Some("1970-01-01 00:02 UTC\nequity 10050.00\ndrawdown 0.50%")
    );
    let older = RunSummaryWire {
        curve_timestamp: Vec::new(),
        source_label: Some("backtest x.rs 1970-01-01 00:00 – 1970-01-01 00:03 UTC".to_string()),
        ..summary
    };
    assert_eq!(
        equity::hover_text(&older, &drops, 2.4).as_deref(),
        Some("about 1970-01-01 00:02 UTC\nequity 10050.00\ndrawdown 0.50%")
    );
    assert_eq!(equity::hover_text(&RunSummaryWire::default(), &[], 0.0), None);
}

/// The axis reads a point's own time at its step and interpolates between two points (a gap, a weekend, is in the
/// times, not spread over the steps); outside the curve, and with times that are not the curve's, nothing.
#[test]
fn the_axis_reads_the_runs_own_times() {
    let curve = [(0, 1.0), (10, 1.0), (20, 1.0)];
    let times = [1_000, 2_000, 4_000];
    let at = |x: f64| equity::curve_time(&curve, &times, x);
    let read = [0.0, 5.0, 10.0, 15.0, 20.0].map(at);
    assert_eq!(read, [Some(1_000), Some(1_500), Some(2_000), Some(3_000), Some(4_000)]);
    assert_eq!((at(-1.0), at(21.0)), (None, None), "the plot's margin");
    assert_eq!(equity::curve_time(&curve, &times[..2], 5.0), None, "not the curve's times");
}

/// The Equity tab paints finite geometry over a real curve, a flat one, one point and none: the
/// drawdown pane under a flat curve is all zeros, and an empty one draws the empty frames.
#[test]
fn the_equity_tab_paints_finite_geometry_whatever_the_curve() {
    let flat = TradingRecord { equity_curve: vec![10_000.0; 4], ..sample() };
    for r in [sample(), flat, TradingRecord { equity_curve: vec![10_000.0], ..Default::default() }]
    {
        let summary = summary_of(&r, periods_per_year_1m());
        frame(|ui| equity::equity_tab(ui, &summary));
    }
    frame(|ui| equity::equity_tab(ui, &RunSummaryWire::default()));
}

/// A span in words: the two largest units that follow one another, milliseconds under a second.
#[test]
fn a_duration_reads_in_words() {
    assert_eq!(duration_words(500), "500 milliseconds");
    assert_eq!(duration_words(1_000), "1 second");
    assert_eq!(duration_words(90_000), "1 minute 30 seconds");
    assert_eq!(duration_words(3_605_000), "1 hour");
    assert_eq!(duration_words(2 * 86_400_000 + 3 * 3_600_000 + 60_000), "2 days 3 hours");
    assert_eq!(utc_minute(60_000), "1970-01-01 00:01");
    assert_eq!(utc_minute(0), "—", "a time the run did not record");
}

/// The run line says what is on its way, else how long each kind took; nothing before anything ran.
#[test]
fn the_run_line_says_what_runs_and_what_it_took() {
    let mut origin = StudioResultsOrigin::default();
    assert_eq!(header::run_line_words(&[("Backtest", false, &origin)]), None);
    origin.send(StudioResultsCode::ServerFile("probe/x.rs".to_string()));
    let running = header::run_line_words(&[("Backtest", true, &origin)]).expect("on its way");
    assert!(running.starts_with("Backtest running for "), "{running}");
    origin.answer();
    let took = header::run_line_words(&[("Backtest", false, &origin)]).expect("answered");
    assert!(took.starts_with("Backtest took ") && took.ends_with(" seconds"), "{took}");
    assert_eq!(
        origin.answered().map(|(_, code)| code.words()).as_deref(),
        Some("the server's file probe/x.rs, as saved there")
    );
    assert!(origin.values().is_empty(), "a plain send carries no values");
    // A fixed walk-forward's values travel with its send and are the answered one's.
    let set = vec![("qty".to_string(), ParamValue::Float(0.1))];
    origin.send_with_values(StudioResultsCode::ServerFile("probe/x.rs".to_string()), set.clone());
    assert!(origin.values().is_empty(), "not until it answers");
    origin.answer();
    assert_eq!(origin.values(), set.as_slice());
}

/// An opened scan point's header names the file the scan was sent with and its parameter values, where its
/// record names no file.
#[test]
fn a_scan_points_label_names_its_file_and_values() {
    let mut summary = summary_of(&sample(), periods_per_year_1m());
    let label = "backtest (unnamed) 2026-05-20 00:00 – 2026-11-20 00:00 UTC";
    summary.source_label = Some(label.to_string());
    let sweep = sweep_with(None, None);
    let label = |s: &RunSummaryWire, file| results_label(s, Some(&sweep), file);
    assert_eq!(label(&summary, "x.rs").as_deref(), summary.source_label.as_deref(), "a single run");
    summary.point = Some(0);
    let want = "backtest momentum.rs 2026-05-20 00:00 – 2026-11-20 00:00 UTC · fast=5";
    assert_eq!(label(&summary, "momentum.rs").as_deref(), Some(want));
    let want = "backtest 2026-05-20 00:00 – 2026-11-20 00:00 UTC · fast=5";
    assert_eq!(label(&summary, "").as_deref(), Some(want), "an editor text saved to no file");
    assert_eq!(StudioResultsCode::ServerFile("momentum.rs".into()).file(), "momentum.rs");
}

/// The code line's notice: an editor text that moved since the run, and a server file's run, whose
/// editor text never ran (the owner's desktop test of 2026-11-20).
#[test]
fn the_code_line_says_when_the_editor_does_not_show_what_ran() {
    let buffer = StudioResultsCode::Buffer { text: "a".into(), file: String::new(), saved: false };
    assert_eq!(buffer.stale("a"), None);
    assert!(buffer.stale("b").expect("an edit since").contains("older version"));
    let rust = StudioResultsCode::RustFile { file: "x.rs".into(), text: "a".into() };
    assert_eq!(rust.stale("a"), None);
    let file = StudioResultsCode::ServerFile("probe/x.rs".into());
    assert!(file.stale("anything").expect("never the editor").contains("probe/x.rs"));
    assert_eq!(buffer.words(), "the editor's Rhai text, saved to no file");
}

/// The Validation tab's first heading names the point on screen, else this run.
#[test]
fn the_validation_heading_names_what_its_rows_are_about() {
    let mut summary = summary_of(&sample(), periods_per_year_1m());
    let sweep = sweep_with(Some(0.95), Some(0.10));
    assert_eq!(selected_heading(&summary, Some(&sweep)), "This run");
    summary.point = Some(0);
    assert_eq!(selected_heading(&summary, Some(&sweep)), "Selected point: fast=5");
    let walk = walk_with(0.5);
    let mut tab = StudioResultsTab::Validation;
    let pager = StudioResultsTradesPager::default();
    let walked = Some((&walk, [].as_slice()));
    frame(|ui| {
        let asked = results_ui(ui, &mut tab, &summary, None, &pager, Some((&sweep, "")), walked);
        assert_eq!(asked, None);
    });
}

/// The Walk-forward tab: the overall line, one row a window (its tested bars and chosen parameters), the
/// curve; with no walk-forward, the sentence that says how to run one.
#[test]
fn the_walk_forward_tab_shows_its_windows_and_its_overall_line() {
    let mut walk = walk_with(0.5);
    let cells = walkforward::walkforward_overall_cells(&walk);
    assert!(cells.contains(&("Profitable windows".to_string(), "50.00%".to_string())), "{cells:?}");
    walk.steps.push(vike_analytics::walkforward::WalkforwardResultsStep {
        test_range: (10, 20),
        window_times: None,
        chosen_params: Some(vec![("fast".into(), ParamValue::Float(5.0))]),
        results: walk.overall.clone(),
    });
    let set = vec![("qty".to_string(), ParamValue::Float(0.1))];
    // From an older server, no dates: the bars it tested.
    let row = walkforward::walkforward_step_row(0, &walk.steps[0], &set);
    let shown = (row[0].as_str(), row[1].as_str(), row[2].as_str(), row[3].as_str());
    assert_eq!(shown, ("1", "—", "bars 10–20", "fast=5"));
    // A fixed walk chose nothing: its windows ran the values its request carried, and say which.
    let mut fixed = walk.steps[0].clone();
    fixed.chosen_params = None;
    let row = walkforward::walkforward_step_row(0, &fixed, &set);
    assert_eq!(row[3], "qty=0.1 (as set: a fixed walk)");
    let row = walkforward::walkforward_step_row(0, &fixed, &[]);
    assert_eq!(row[3], "the strategy's own (a fixed walk)");
    // The window's dates, train and test, first bar to last (2026-11-20).
    let hour = 3_600_000;
    fixed.window_times = Some(vike_analytics::walkforward::WalkforwardWindowTimes {
        train_start: Some(0),
        train_end: Some(9 * hour),
        test_start: 10 * hour,
        test_end: 19 * hour,
    });
    let row = walkforward::walkforward_step_row(0, &fixed, &[]);
    assert_eq!(row[1], "1970-01-01 00:00 – 1970-01-01 09:00");
    assert_eq!(row[2], "1970-01-01 10:00 – 1970-01-01 19:00");
    walk.steps.push(fixed);
    // The stitched curve's own times put its axis on dates.
    walk.equity_curve_timestamp = vec![0, 60_000];
    let mut tab = StudioResultsTab::Walkforward;
    let summary = summary_of(&sample(), periods_per_year_1m());
    let pager = StudioResultsTradesPager::default();
    frame(|ui| {
        let walked = Some((&walk, set.as_slice()));
        assert_eq!(results_ui(ui, &mut tab, &summary, None, &pager, None, walked), None);
    });
    frame(|ui| {
        assert_eq!(results_ui(ui, &mut tab, &summary, None, &pager, None, None), None);
    });
}

/// Every accessibility node of `f`'s third frame (a scroll area shows its bars from its second), as its
/// role, its words and its rect.
fn accessible(
    mut f: impl FnMut(&mut egui::Ui),
) -> Vec<(egui::accesskit::Role, String, egui::Rect)> {
    let context = egui::Context::default();
    context.enable_accesskit();
    let raw = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
        ..Default::default()
    };
    let mut nodes = Vec::new();
    for _ in 0..3 {
        let mut out = context.run_ui(raw(), &mut f);
        out.textures_delta.clear();
        let update = out.platform_output.accesskit_update.take().expect("accesskit is on");
        nodes = update
            .nodes
            .into_iter()
            .filter_map(|(_, node)| {
                let b = node.bounds()?;
                let words = node.value().or(node.label()).unwrap_or_default().to_string();
                let rect = egui::Rect::from_min_max(
                    egui::pos2(b.x0 as f32, b.y0 as f32),
                    egui::pos2(b.x1 as f32, b.y1 as f32),
                );
                Some((node.role(), words, rect))
            })
            .collect();
    }
    nodes
}

/// **The Trades table scrolls to its last column inside its pane** (the owner's pass of 2026-11-21: at the
/// widest scroll, Fees stayed under the next panel): in a pane narrower than the tab strip above the table,
/// which widens the Ui past the pane's edge, the table's sideways bar ends at that edge.
#[test]
fn the_trades_table_scrolls_inside_its_pane() {
    let trade = |i: usize| Trade {
        entry_price: 100.0,
        exit_price: 101.0,
        size: 1.0,
        pnl: 1.0,
        fees: 0.1,
        entry_timestamp: i as i64,
        exit_timestamp: i as i64 + 1,
        symbol: "BTCUSDT".to_string(),
        mae: 0.0,
        mfe: 0.0,
        is_long: true,
    };
    let trades: Vec<Trade> = (0..5).map(trade).collect();
    let summary = RunSummaryWire { n_trades: 5, ..RunSummaryWire::default() };
    let pager = StudioResultsTradesPager::default();
    let pane = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 600.0));
    let nodes = accessible(|ui| {
        ui.scope_builder(egui::UiBuilder::new().max_rect(pane), |ui| {
            ui.set_clip_rect(pane);
            let mut tab = StudioResultsTab::Trades;
            results_ui(ui, &mut tab, &summary, Some(&trades), &pager, None, None);
        });
    });
    let sideways: Vec<egui::Rect> = nodes
        .iter()
        .filter(|(role, _, r)| *role == egui::accesskit::Role::ScrollBar && r.width() > r.height())
        .map(|(_, _, r)| *r)
        .collect();
    assert!(!sideways.is_empty(), "the table is wider than the pane: it scrolls sideways");
    for bar in sideways {
        assert!(
            bar.max.x <= pane.max.x + 0.5,
            "the bar {bar:?} runs past the pane's edge {pane:?}"
        );
    }
}

/// **The Walk-forward table's Trained and Tested dates stand apart** (the owner's pass of 2026-11-21: at
/// the item spacing the two ran together): a column gap of [`space::XL2`] between them.
#[test]
fn the_walk_forward_dates_have_a_gap_between_them() {
    let mut walk = walk_with(0.5);
    let hour = 3_600_000;
    walk.steps.push(vike_analytics::walkforward::WalkforwardResultsStep {
        test_range: (10, 20),
        window_times: Some(vike_analytics::walkforward::WalkforwardWindowTimes {
            train_start: Some(0),
            train_end: Some(9 * hour),
            test_start: 10 * hour,
            test_end: 19 * hour,
        }),
        chosen_params: None,
        results: walk.overall.clone(),
    });
    let nodes = accessible(|ui| walkforward::walkforward_view(ui, &walk, &[]));
    let at = |words: &str| nodes.iter().find(|(_, w, _)| w == words).map(|(_, _, r)| *r);
    let trained = at("1970-01-01 00:00 – 1970-01-01 09:00").expect("the Trained cell");
    let tested = at("1970-01-01 10:00 – 1970-01-01 19:00").expect("the Tested cell");
    let gap = tested.min.x - trained.max.x;
    assert!(
        gap >= space::XL2 - 0.5,
        "Trained ends at {}, Tested starts {gap} after",
        trained.max.x
    );
}

/// **The start's label is drawn over the equity curve, on a background** (the owner's pass of 2026-11-21:
/// the curve, which begins at it, ran through "start 10000.00"): nothing of the plot is painted after it,
/// and a filled box under it holds its words.
#[test]
fn the_start_label_is_drawn_over_the_curve() {
    let summary = summary_of(&sample(), periods_per_year_1m());
    let context = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
        ..Default::default()
    };
    let mut out = context.run_ui(raw, |ui| equity::equity_tab(ui, &summary));
    out.textures_delta.clear();
    let shapes = &out.shapes;
    let is_label = |c: &egui::epaint::ClippedShape| matches!(&c.shape, egui::Shape::Text(t) if t.galley.text() == "start 10000.00");
    let label = shapes.iter().position(is_label).expect("the start's label is painted");
    let egui::Shape::Text(text) = &shapes[label].shape else { unreachable!("found as a text") };
    let clip = shapes[label].clip_rect;
    assert!(
        shapes[..label.saturating_sub(1)].iter().any(|c| c.clip_rect == clip),
        "the plot's own shapes share the label's clip, or the next check proves nothing"
    );
    assert!(
        shapes[label + 1..].iter().all(|c| c.clip_rect != clip),
        "a shape of the plot is painted over the label"
    );
    let words = egui::Rect::from_min_size(text.pos, text.galley.size());
    assert!(
        matches!(&shapes[label - 1].shape, egui::Shape::Rect(r)
            if r.fill.a() > 0 && r.rect.expand(1.0).contains_rect(words)),
        "a filled box under the label holds its words"
    );
}
