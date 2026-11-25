use super::*;
use crate::metric_catalog::{METRICS, spec_for};
use crate::mtm::MarkToMarketRuntimeStatistics;
use crate::results_table::NOT_RECORDED;
use vike_model::Trade;

fn trade(pnl: f64, exit_ts: i64) -> Trade {
    crate::test_support::trade(pnl, 0.1, true, exit_ts)
}

/// Three months of samples so the monthly table has real buckets.
fn sample() -> (TradingResultsTable, Vec<f64>, Vec<i64>) {
    const DAY_MS: i64 = 86_400_000;
    let jan15: i64 = 1_705_276_800_000; // 2024-01-15 UTC
    let ts: Vec<i64> = (0..5).map(|i| jan15 + i * 30 * DAY_MS).collect();
    let equity = vec![1_000.0, 1_050.0, 990.0, 1_080.0, 1_120.0];
    let trades = vec![trade(50.0, ts[1]), trade(-60.0, ts[2]), trade(130.0, ts[4])];
    let sheet = TradingResultsTable::from_result_parts(
        Some("sess & <html>".into()),
        trades,
        equity.clone(),
        ts.clone(),
        252.0,
    );
    (sheet, equity, ts)
}

/// Tiny tag-balance checker: every open tag must be closed in LIFO order. Handles the
/// doctype, self-closing (`... />`) and void (`meta`) tags. Sufficient because the renderer
/// never emits `<` in text (values are escaped) and the stylesheet contains no `<`/`>`.
fn assert_balanced(html: &str) {
    const VOID: [&str; 4] = ["meta", "br", "hr", "link"];
    let mut stack: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let end = rest.find('>').expect("tag never closed with '>'");
        let tag = rest[..end].trim();
        rest = &rest[end + 1..];
        if tag.starts_with('!') {
            continue; // doctype
        }
        if let Some(name) = tag.strip_prefix('/') {
            let top = stack.pop().unwrap_or_else(|| panic!("stray closing </{name}>"));
            assert_eq!(top, name.trim(), "mismatched nesting: <{top}> closed by </{name}>");
        } else if !tag.ends_with('/') {
            let name: String = tag.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if !VOID.contains(&name.as_str()) {
                stack.push(name);
            }
        }
    }
    assert!(stack.is_empty(), "unclosed tags: {stack:?}");
}

#[test]
fn html_smoke_structure_and_values() {
    let (sheet, eq, ts) = sample();
    let html = render_html(&sheet, &[], &eq, &ts);

    // Structure: a full document with inline SVG charts and balanced nesting.
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert_eq!(html.matches("<svg").count(), 2, "equity + underwater charts");
    assert!(html.contains("<polyline"));
    assert!(html.contains("<style>"));
    assert_balanced(&html);

    // Metrics values render, and the assertion is built from the SAME `MetricUnit::render`
    // the document is — asserting against a hand-written `format!` here would reintroduce the
    // fifth spelling of the rule this table stopped keeping.
    for id in ["final_equity", "sharpe", "max_drawdown"] {
        let spec = spec_for(id).expect("a catalog id");
        let v = sheet.metric(id).expect("the sample sheet carries it");
        assert!(html.contains(&spec.unit.render(v)), "{id} renders");
        assert!(html.contains(&format!("<th>{id}</th>")), "{id} is labelled by its catalog id");
    }

    // The free-form name is escaped, never raw.
    assert!(html.contains("sess &amp; &lt;html&gt;"));
    assert!(!html.contains("sess & <html>"));

    // Titled by the document's own words, the retired "Tearsheet" word nowhere.
    assert!(html.contains("<title>Trading results — sess &amp; &lt;html&gt;</title>"), "{html}");
    assert!(html.contains("<h1>Trading results <span class=\"name\">"), "{html}");
    assert!(!html.contains("Tearsheet"), "{html}");

    // Self-contained: no external fetches of any kind.
    assert!(!html.contains("http://") && !html.contains("https://"));
    assert!(!html.contains("<script"));
}

/// The captions handed in sit right under the heading, each its own line and escaped, in order: the results'
/// SOURCE LABEL, then a note such as the fees in other assets; with none there is no such line.
#[test]
fn the_source_label_and_the_notes_sit_under_the_heading() {
    let (sheet, eq, ts) = sample();
    let label = "binance/MAIN (demo) 2024-01-15 00:00 – 2024-05-14 00:00 UTC & <more>";
    let fees = "fees in other assets: 0.0123 BNB (not converted; left out of total_fees and the realized curve)";
    let html = render_html(&sheet, &[label, fees], &eq, &ts);
    let escaped = "binance/MAIN (demo) 2024-01-15 00:00 – 2024-05-14 00:00 UTC &amp; &lt;more&gt;";
    let lines = format!("<p class=\"caption\">{escaped}</p><p class=\"caption\">{fees}</p>");
    assert!(html.contains(&format!("</h1>{lines}")), "{html}");
    assert_balanced(&html);
    let none = render_html(&sheet, &[], &eq, &ts);
    assert!(!none.contains("binance/MAIN"), "no label, no line");
    assert_eq!(html.replace(&lines, ""), none);
}

/// ⚠ **The roster property for the HTML shape.** The table used to hold 25 hand-typed rows
/// against a 38-id catalog; a test that asserted a handful of them would have stayed green
/// through that whole gap, so this one asserts EVERY catalog row has a cell — which is the
/// claim that fails the day somebody reintroduces a local row list.
#[test]
fn the_metrics_table_carries_every_catalog_row() {
    let (sheet, eq, ts) = sample();
    let html = render_html(&sheet, &[], &eq, &ts);
    // A closed-trade row appears only where a run armed that block, which no live sheet does.
    for m in METRICS.iter().filter(|m| m.home != crate::metric_catalog::MetricHome::ClosedTrades) {
        assert!(
            html.contains(&format!("<th>{}</th>", m.id)),
            "{} has no row in the HTML tearsheet",
            m.id
        );
    }
}

/// An UNRECORDED metric renders the words, never `0.00`. `funding_paid` is the reachable
/// example on this door — `TradingResultsTable::from_result_parts` declares it unrecorded because a
/// fill-stream reconstruction folds no funding cashflow — and a cell reading `0.00` there
/// would tell a perp operator that funding was free.
#[test]
fn an_unrecorded_metric_renders_words_not_a_zero() {
    let (sheet, eq, ts) = sample();
    assert_eq!(sheet.metric("funding_paid"), None, "the door this sample came through");
    let html = render_html(&sheet, &[], &eq, &ts);
    assert!(html.contains(NOT_RECORDED), "the unrecorded cell says so: {html}");
}

#[test]
fn monthly_table_renders_calendar_buckets() {
    let (sheet, eq, ts) = sample();
    let html = render_html(&sheet, &[], &eq, &ts);
    assert!(html.contains("Monthly returns"));
    assert!(html.contains("class=\"monthly\""));
    assert!(html.contains("<th>2024</th>"), "year row present");
    // Jan 2024: 1000 -> 1050 = +5.00% — the periods.rs oracle's number, rendered.
    assert!(html.contains("+5.00%"), "January bucket renders its return");
}

#[test]
fn monthly_table_omitted_without_timestamps() {
    let (sheet, eq, _) = sample();
    let html = render_html(&sheet, &[], &eq, &[]);
    assert!(!html.contains("Monthly returns"), "no ts -> no calendar bucketing");
    assert_balanced(&html);
    // Charts still render off the bare curve.
    assert_eq!(html.matches("<svg").count(), 2);
}

#[test]
fn empty_curve_degrades_to_note_not_broken_svg() {
    let (sheet, _, _) = sample();
    let html = render_html(&sheet, &[], &[], &[]);
    assert!(!html.contains("<svg"));
    assert!(html.contains("(no equity samples)"));
    assert_balanced(&html);
}

/// A non-finite equity sample must never reach the SVG. `y_at` would map it into the
/// `points`/`d` attribute as a literal `NaN`/`inf` token — invalid SVG that browsers do not
/// report, they just truncate the shape there (a partial/blank chart). Both charts degrade to
/// the explicit note instead, which also names the reason.
#[test]
fn non_finite_equity_sample_degrades_to_note_not_broken_svg() {
    let (sheet, _, ts) = sample();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let eq = vec![1_000.0, 1_050.0, bad, 1_080.0, 1_120.0];
        let html = render_html(&sheet, &[], &eq, &ts);
        assert!(!html.contains("<svg"), "no chart for a non-finite series ({bad})");
        assert!(!html.contains("<polyline"), "no polyline coordinates at all ({bad})");
        assert!(!html.contains("points="), "no coordinate attribute to corrupt ({bad})");
        assert!(html.contains("are not finite"), "the note names the reason ({bad})");
        // The note must NOT claim there are no samples — there are five, they are unusable.
        assert!(!html.contains("(no equity samples)"), "wrong note for a corrupt curve ({bad})");
        assert_balanced(&html);
    }
}

/// A finite curve still charts — the finiteness gate must not have broken the happy path.
#[test]
fn finite_curve_still_charts_after_the_gate() {
    let (sheet, eq, ts) = sample();
    let html = render_html(&sheet, &[], &eq, &ts);
    assert_eq!(html.matches("<svg").count(), 2, "equity + underwater still render");
    assert!(!html.contains("are not finite"));
}

/// A non-finite monthly bucket renders as the EMPTY marker, never as a signed number:
/// `NaN < 0.0` is false, so the naive sign test classed it `pos` and printed a green "+NaN%".
#[test]
fn non_finite_monthly_cell_renders_empty_not_nan_percent() {
    let (sheet, _, ts) = sample();
    let eq = vec![1_000.0, 1_050.0, f64::NAN, 1_080.0, 1_120.0];
    let html = render_html(&sheet, &[], &eq, &ts);
    assert!(html.contains("Monthly returns"), "the table still renders: {html}");
    assert!(!html.contains("NaN%"), "no NaN cell may be printed as a percentage: {html}");
    assert!(!html.contains(">+NaN"), "and none classed positive: {html}");
    assert_balanced(&html);
}

/// The seed-only curve a NO-TRADES journal produces (`equity = [seed]`, `ts = [0]`) must not
/// caption an epoch date range directly above two "(no equity samples)" notes.
#[test]
fn single_sample_curve_renders_no_epoch_date_caption() {
    let (sheet, _, _) = sample();
    let html = render_html(&sheet, &[], &[1_000.0], &[0]);
    assert!(!html.contains("1970-01-01"), "no epoch date caption: {html}");
    assert!(!html.contains("equity samples</p>"), "no sample-count caption: {html}");
    assert!(html.contains("(no equity samples)"), "the charts still say so");
    assert_balanced(&html);
}

/// ... while a real 2+ sample curve keeps its date-range caption.
#[test]
fn multi_sample_curve_keeps_its_date_caption() {
    let (sheet, eq, ts) = sample();
    let html = render_html(&sheet, &[], &eq, &ts);
    assert!(html.contains("2024-01-15"), "first sample date renders: {html}");
    assert!(html.contains("5 equity samples"), "sample count renders: {html}");
}

#[test]
fn underwater_series_is_running_peak_relative() {
    let drawdown = underwater(&[100.0, 120.0, 90.0, 120.0, 130.0]);
    assert_eq!(drawdown[0], 0.0);
    assert!(drawdown[0].is_sign_positive(), "a peak is 0.00%, never -0.00%");
    assert_eq!(drawdown[1], 0.0);
    assert!((drawdown[2] - (90.0 / 120.0 - 1.0)).abs() < 1e-12);
    assert_eq!(drawdown[3], 0.0);
    assert_eq!(drawdown[4], 0.0);
}

fn sample_stats() -> MarkToMarketRuntimeStatistics {
    MarkToMarketRuntimeStatistics {
        traded_notional: 220.0,
        turnover: 0.5,
        peak_gross_exposure: 1.25,
        peak_net_exposure: 0.75,
        peak_margin_used: 5_000.0,
    }
}

/// OFF/default: `render_html` omits the runtime section entirely (the byte-identical path).
#[test]
fn default_render_html_has_no_runtime_section() {
    let (sheet, eq, ts) = sample();
    let html = render_html(&sheet, &[], &eq, &ts);
    assert!(!html.contains("Runtime exposure"), "default render omits the runtime section");
    assert!(!html.contains("peak gross exposure"));
    assert_balanced(&html);
}

/// With stats: the section is APPENDED — everything up to `</main>` is byte-for-byte the
/// default document, and the appended rows render and stay balanced.
#[test]
fn render_with_stats_appends_runtime_section_byte_identically() {
    let (sheet, eq, ts) = sample();
    let base = render_html(&sheet, &[], &eq, &ts);
    let full = render_html_with_stats(&sheet, &[], &eq, &ts, &sample_stats());

    let marker = "</main></body></html>\n";
    let base_head = base.strip_suffix(marker).expect("default ends with the main-close marker");
    assert!(full.starts_with(base_head), "stats render must not alter the default body");
    assert!(full.ends_with(marker));

    assert!(full.contains("Runtime exposure"));
    assert!(full.contains("turnover"));
    assert!(full.contains("peak gross exposure"));
    assert!(full.contains("peak net exposure"));
    assert!(full.contains("peak margin used"));
    assert!(full.contains("5000.00"), "peak margin renders as a currency amount");
    assert_balanced(&full);
}

/// A run that armed closed-trade accounting: the default document up to its equity chart, which
/// gains the dashed buy-and-hold line, then the trades analysis in three columns and the list of
/// trades after the monthly table. The unarmed document is untouched.
#[test]
fn a_closed_trade_run_adds_buy_and_hold_and_its_two_sections() {
    use vike_trade_ledger::{TradeClosedEntry, TradeClosedMetrics, TradeClosedStatistics};
    let (sheet, eq, ts) = sample();
    let entry = TradeClosedEntry {
        tag: "Long <A>".into(),
        side: 1,
        entry_timestamp: ts[0],
        exit_timestamp: ts[1],
        net: -12.5,
        net_percent: -0.0125,
        bars: 3,
        ..Default::default()
    };
    let closed = TradeClosedMetrics {
        all: TradeClosedStatistics { trades: 1, net_profit: -12.5, ..Default::default() },
        long: TradeClosedStatistics { trades: 1, ..Default::default() },
        trades: vec![entry],
        buy_hold_curve: vec![(ts[0], 1_000.0), (ts[4], 900.0)],
        ..Default::default()
    };
    let plain = render_html(&sheet, &[], &eq, &ts);
    let html = render_html_with_closed_trades(&sheet, &[], &eq, &ts, &closed);
    assert_balanced(&html);
    assert!(html.contains("stroke-dasharray:4 3") && html.contains("dashed: buy and hold"));
    assert!(!plain.contains("stroke-dasharray") && !plain.contains("Closed-trade accounting"));
    assert!(html.contains("<th>all</th><th>long</th><th>short</th>"));
    assert!(
        html.contains("<tr><th>closed_trades</th><td>1</td><td>1</td><td>0</td></tr>"),
        "{html}"
    );
    assert!(
        !html.contains("<th>closed_drawdown</th>"),
        "an account figure has no long or short column"
    );
    assert!(html.contains("<h2>List of trades</h2>"));
    assert!(
        html.contains("<td>Long &lt;A&gt;</td><td>long</td><td>2024-01-15 00:00</td>"),
        "{html}"
    );
    assert!(html.contains("<td class=\"neg\">-12.50</td><td class=\"neg\">-1.2500%</td>"));
    // Everything before the equity chart is the default document's.
    let cut = plain.find("<h2>Equity curve</h2>").expect("a chart");
    assert_eq!(&html[..cut], &plain[..cut]);
}

/// The comparison document: both sides named and escaped, one row per comparison row with the
/// `b − a` cell beside them, an INVALID side's words, balanced nesting and the shared stylesheet.
#[test]
fn the_comparison_document_holds_both_sides_and_the_difference() {
    use crate::record::TradingRecord;
    use crate::results::TradingResults;
    use crate::results_comparison::TradingResultsComparison;
    let side = |name: &str, equity: Vec<f64>, pnl: f64| {
        let record = TradingRecord {
            trades: vec![trade(pnl, 1)],
            n_trades: 1,
            final_equity: equity.last().copied().unwrap_or(0.0),
            equity_curve: equity,
            ..TradingRecord::default()
        };
        TradingResults::from_record(Some(name.to_string()), &record, 252.0)
    };
    let comparison = TradingResultsComparison::of(
        side("back & <test>", vec![1_000.0, 1_050.0], 50.0),
        side("ruined", vec![1_000.0, 0.0], -1_000.0),
    );
    let html = render_comparison_html(&comparison, &["b: fees in other assets: 0.5 BNB"]);
    assert!(html.contains("<p class=\"caption\">b: fees in other assets: 0.5 BNB</p>"), "{html}");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<style>") && !html.contains("<script"));
    assert_balanced(&html);
    assert!(
        html.contains(
            "<title>Comparison — backtest back &amp; &lt;test&gt; vs backtest ruined</title>"
        ),
        "{html}"
    );
    assert!(!html.contains("back & <test>"), "the free-form name is escaped");
    assert!(html.contains("<p class=\"caption\">b INVALID: equity reached 0"), "{html}");
    assert!(html.contains("<th>a</th><th>b</th><th>b − a</th>"));
    assert!(
        html.contains(
            "<tr><th>final_equity</th><td>1050.00</td><td>0.00</td><td>-1050.00</td></tr>"
        ),
        "{html}"
    );
    assert!(
        html.contains("<tr><th>sharpe</th>") && html.contains("<td>INVALID</td><td></td></tr>")
    );
    assert_eq!(
        html.matches("</td></tr>").count(),
        comparison.rows().len(),
        "one body row per comparison row"
    );
}
