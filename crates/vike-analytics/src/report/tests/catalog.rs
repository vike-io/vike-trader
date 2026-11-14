//! The long-form metric catalog seam: every id resolves, every row is its unit's own rendering.

use super::*;

/// ⚠ **The seam the catalog exists to hold, gated BOTH ways.** A row in
/// [`crate::metric_catalog::METRICS`] with no field behind it renders "not recorded" forever on
/// a report that recorded everything, and a field with no row is a number nobody can ask for.
/// Both are silent, which is why this is a test rather than a convention.
#[test]
fn every_catalog_id_resolves_and_every_stored_metric_is_in_the_catalog() {
    use crate::metric_catalog::{METRICS, MetricHome, MetricSelection};

    let report = BacktestReport::from_result(None, &armed_result(), 252.0);
    for m in METRICS {
        assert!(
            report.metric_value(m.id).is_some(),
            "catalog id `{}` resolves to no value on a fully-composed report — either the \
                 `metric_value`/`value_of` arm is missing or the row names a field that does not \
                 exist",
            m.id
        );
    }

    // ...and the other direction, through the serialized form: every key of the `extended`
    // block must be a catalog row. Done on the JSON rather than on the struct because Rust
    // cannot enumerate fields, which is the same reason
    // `crates/vike-backtest/tests/run_record_completeness.rs` reads its type as text.
    let block = serde_json::to_value(report.extended.as_ref().unwrap()).unwrap();
    for key in block.as_object().expect("the extended block is an object").keys() {
        let spec = crate::metric_catalog::spec_for(key);
        assert!(spec.is_some(), "`extended.{key}` is stored and no catalog row names it");
        assert_eq!(
            spec.unwrap().home,
            MetricHome::Extended,
            "`{key}` is stored in the extended block and its catalog row says otherwise"
        );
    }
    let closed = report.closed_trades.as_ref().unwrap();
    let blocks = [
        ("all", serde_json::to_value(&closed.all)),
        ("account", serde_json::to_value(&closed.account)),
    ];
    for (block, value) in blocks {
        for key in value.unwrap().as_object().expect("a closed-trade block is an object").keys() {
            let spec = crate::metric_catalog::spec_for(&format!("closed_{key}"));
            assert!(
                spec.is_some(),
                "`closed_trades.{block}.{key}` is stored and no catalog row names it"
            );
            assert_eq!(
                spec.unwrap().home,
                MetricHome::ClosedTrades,
                "`closed_{key}` has another home"
            );
        }
    }

    // The selection every consumer will actually ask for must render every row.
    let rendered = report.render_metrics(&MetricSelection::Full);
    for m in METRICS {
        assert!(rendered.contains(m.id), "`--metrics full` omits {}", m.id);
    }
    // ⚠ Asserted over the WHOLE rendering rather than per id, and deliberately: the rows are
    // column-padded, so a per-id `format!("{id} not recorded")` needle can never match and the
    // per-id spelling of this check was VACUOUS — it passed whatever the renderer did.
    assert!(
        !rendered.contains("not recorded"),
        "`--metrics full` reports an unrecorded metric on a report that recorded every one:\n\
             {rendered}"
    );
}

/// ⚠ **The renderer DELEGATES its cells rather than re-deciding them.** `render_metrics`
/// carried its own private `match` over `crate::metric_catalog::MetricUnit` — the second of
/// four spellings of one rule — and this asserts that every row it prints is byte-identical to
/// what the unit itself answers. So an edit to one can no longer move the other, which is the
/// drift that let `crates/vike-analytics/src/html.rs`'s `render_html_inner` (vike-report's at the
/// time) render two MONEY rows at four decimals while this door rendered them at two.
#[test]
fn every_rendered_row_is_the_units_own_rendering() {
    use crate::metric_catalog::{METRICS, MetricSelection, spec_for};

    let report = BacktestReport::from_result(None, &armed_result(), 252.0);
    let rendered = report.render_metrics(&MetricSelection::Full);
    for m in METRICS {
        let v = report.metric_value(m.id).expect("a fully-composed report answers every id");
        let cell = spec_for(m.id).expect("the id came out of METRICS").unit.render(v);
        // Matched as "the line that starts with this id ends with this cell", because the rows
        // are column-padded: a `contains(&format!("{id}  {cell}"))` needle would depend on the
        // padding width and silently never match, which is how the sibling "not recorded"
        // assertion in `every_catalog_id_resolves_and_every_stored_metric_is_in_the_catalog`
        // was once VACUOUS.
        assert!(
            rendered.lines().any(|l| l.starts_with(m.id) && l.ends_with(cell.as_str())),
            "`{}` is not rendered as its unit's own `{cell}`:\n{rendered}",
            m.id
        );
    }

    // ...and the PERCENT rows are actually scaled in the real rendering, not merely in the
    // unit's unit test: `win_rate` is 0.5 on this fixture, so the cell is 50%, never 0.5%.
    assert!(
        rendered.lines().any(|l| l.starts_with("win_rate") && l.ends_with("50.0000%")),
        "a fraction reached the table unscaled:\n{rendered}"
    );
}

/// ⚠ **THE PUBLISHED COMPACT TABLE, pinned cell by cell.** Its value cells now come from
/// `crate::metric_catalog::MetricUnit::render` instead of four format literals spelled in
/// `Display` itself, and the substitution is byte-identical BY CONSTRUCTION (the unit's Percent
/// arm is the same `format!("{:.4}%", v * 100.0)` expression). This asserts the exact strings
/// anyway, because "byte-identical by construction" is the claim a reader most wants evidence
/// for on the one report format that is already on people's screens and in their fixtures.
///
/// The fixture's numbers are chosen by `sample_result`: equity `1000 -> 1005` is a `0.5000%`
/// return with a `1.9802%` peak-to-trough, and one win in two trades is a `50.0000%` win rate.
#[test]
fn the_compact_table_cells_are_the_units_own_renderings() {
    let table = BacktestReport::from_result(None, &sample_result(), 252.0).to_string();

    for want in [
        "final_equity:  1005.00",
        "total_return:  0.5000%",
        "n_trades:      2",
        "win_rate:      50.0000%",
        "max_drawdown:  1.9802%",
    ] {
        assert!(table.contains(want), "the compact table lost `{want}`:\n{table}");
    }
    // The percent rows are SCALED — the failure this whole task is about would render
    // `0.005000%` here, and a reader would call it five thousandths of a percent.
    assert!(!table.contains("0.0050%"), "a fraction reached the table unscaled:\n{table}");
}

/// ⚠ **The funding line says which way its sign points.** `funding_paid` is received-positive (its
/// catalog row and `BacktestResult::funding_paid`'s doc), so a line that read `funding_paid:  12.34`
/// told an operator the run PAID 12.34 when it received it. The table names the sign instead; the
/// `report.json` key and the `--metrics funding_paid` id are unchanged. Still printed only when
/// nonzero, so a run without funding keeps its table byte for byte.
#[test]
fn the_funding_line_says_a_positive_amount_was_received() {
    let received = BacktestResult { funding_paid: 12.34, ..sample_result() };
    let table = BacktestReport::from_result(None, &received, 252.0).to_string();
    assert!(table.contains("\nfunding (net, + = received): 12.34\n"), "{table}");
    assert!(!table.contains("funding_paid:"), "a received amount labelled as paid:\n{table}");
    let plain = BacktestReport::from_result(None, &sample_result(), 252.0).to_string();
    assert!(!plain.contains("funding ("), "a run with no funding prints no funding line:\n{plain}");
}

/// A run that armed closed-trade accounting: the sample run with a block of its own.
fn armed_result() -> BacktestResult {
    let all = crate::closed_trades::TradeClosedStatistics {
        trades: 2,
        net_profit: 4.5,
        ..Default::default()
    };
    let closed = crate::closed_trades::TradeClosedMetrics { all, ..Default::default() };
    BacktestResult { closed_trades: Some(closed), ..sample_result() }
}

/// ⚠ **A closed-trade row appears only where the run armed it.** Unarmed, the report's JSON has no
/// `closed_trades` key and `--metrics full` prints none of those rows, so every existing report is
/// byte-identical; named by id, the row says why it is absent rather than printing a zero.
#[test]
fn closed_trade_rows_appear_only_when_armed() {
    use crate::metric_catalog::{METRICS, MetricHome, MetricSelection, parse_metric_selection};

    let plain = BacktestReport::from_result(None, &sample_result(), 252.0);
    assert!(!serde_json::to_string(&plain).unwrap().contains("closed_trades"));
    let full = plain.render_metrics(&MetricSelection::Full);
    assert!(!full.contains("closed_"), "an unarmed run lists closed rows:\n{full}");
    assert!(!full.contains("not recorded"), "{full}");
    let named = plain.render_metrics(&parse_metric_selection("closed_net_profit").unwrap());
    assert!(named.contains("did not arm `engine.closed_trades`"), "{named}");

    let armed = BacktestReport::from_result(None, &armed_result(), 252.0);
    let full = armed.render_metrics(&MetricSelection::Full);
    for m in METRICS.iter().filter(|m| m.home == MetricHome::ClosedTrades) {
        assert!(full.contains(m.id), "an armed run omits {}", m.id);
    }
    assert_eq!(armed.metric_value("closed_net_profit"), Some(4.5));
    // The compact table gains the closed trades' profit factor only where the run armed them.
    assert!(!plain.to_string().contains("profit_factor:"), "{plain}");
    // ...under its own id, never the round trips' `profit_factor`, which is another number.
    let table = armed.to_string();
    assert!(table.contains("\nclosed_profit_factor: 0.0000 (net of commission)\n"), "{armed}");
    assert!(!table.contains("\nprofit_factor:"), "{armed}");
    let back: BacktestReport =
        serde_json::from_str(&serde_json::to_string(&armed).unwrap()).unwrap();
    assert_eq!(back.closed_trades, armed.closed_trades, "the block reads back");
}

/// `Compact` delegates to `Display` rather than re-rendering, so the published human table
/// cannot acquire a second spelling that drifts from it.
#[test]
fn the_compact_selection_is_the_display_table_verbatim() {
    use crate::metric_catalog::MetricSelection;
    let report = BacktestReport::from_result(Some("demo".into()), &sample_result(), 252.0);
    assert_eq!(report.render_metrics(&MetricSelection::Compact), report.to_string());
}
