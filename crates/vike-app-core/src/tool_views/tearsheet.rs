//! The Tearsheet tool body — the live performance summary read back from THIS session's command
//! journal. Moved verbatim from `vike-app`'s `main.rs` (tool-view extraction batch 1); the ONE
//! adaptation is that the `VIKE_JOURNAL_DIR` env read moved UP to the binary
//! ([`ToolCtx::journal_dir`]) — libraries take configuration as parameters.

use super::ToolCtx;
use crate::tools;
use vike_ui_theme::palette;

/// The Tearsheet tool body: the live performance summary read back from THIS session's command
/// journal (the durability sink that `VIKE_JOURNAL_DIR` / `VIKE_RUN_PROFILE` enable — see
/// `vike_core::journal_config_from_env`). Reconstructs closed trades through the shared
/// `compute_fill` cost-basis seam and computes the stats via `vike_report::LiveTearsheet` — the same
/// numbers a backtest produces. READ-ONLY. Cached: the journal read + trade reconstruction runs on
/// first show and on an explicit Refresh, never per frame.
///
/// ⚠ This doc said "~20 stats" and the number was the SYMPTOM — [`tearsheet_rows`] carried a hand
/// copy of the roster, twenty rows against a catalog of thirty-eight. The count is deliberately not
/// restated here now: the rows come from `vike_analytics::metric_catalog::METRICS` and the panel
/// shows all of them, so the answer is whatever that table holds, and a number written here would
/// be the fifth copy of exactly the thing that rotted.
pub fn tearsheet_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::{Color32, RichText};
    const POS: Color32 = palette::UP;
    const NEG: Color32 = palette::DOWN;

    ui.horizontal(|ui| {
        ui.label(RichText::new("Live Tearsheet").strong().color(palette::TEXT));
        ui.label(RichText::new("· from the command journal").small().color(palette::TEXT3));
    });
    ui.add_space(4.0);

    // Journal dir = the SAME VIKE_JOURNAL_DIR that turns journaling ON for the producer, so the
    // panel reads exactly what this session records (read by the binary, threaded through
    // `ToolCtx`). Absent ⇒ nothing was journaled.
    let Some(dir) = ctx.journal_dir.as_deref() else {
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Journaling is off").color(palette::TEXT2));
            ui.label(
                RichText::new(
                    "Set VIKE_JOURNAL_DIR (or a VIKE_RUN_PROFILE with [sinks.journal]) and restart \
                     to record a journal this panel can read.",
                )
                .small()
                .color(palette::TEXT3),
            );
        });
        return;
    };

    // Controls: starting-capital seed + Refresh. The journal read + reconstruction is not a
    // per-frame cost, so it runs only on first show and when Refresh is clicked (seed edits apply
    // on the next Refresh).
    let mut reload = !tv.ts_loaded;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Seed").small().color(palette::TEXT3));
        ui.add(egui::DragValue::new(&mut tv.ts_seed).speed(100.0).prefix("$"));
        if ui.button("↻ Refresh").clicked() {
            reload = true;
        }
    });
    ui.add_space(4.0);

    if reload {
        tv.ts_loaded = true;
        tv.ts_err = None;
        tv.ts_rows.clear();
        match vike_report::LiveTearsheet::from_journal(dir, tv.ts_seed, 252.0) {
            Ok(sheet) => tv.ts_rows = tearsheet_rows(&sheet),
            Err(e) => tv.ts_err = Some(format!("{e}")),
        }
    }

    if let Some(err) = &tv.ts_err {
        ui.add_space(8.0);
        ui.colored_label(NEG, format!("Could not read journal: {err}"));
        ui.label(RichText::new(format!("dir: {}", dir.display())).small().color(palette::TEXT3));
        return;
    }

    egui::Grid::new("tearsheet_rows").num_columns(2).spacing([24.0, 4.0]).striped(true).show(
        ui,
        |ui| {
            for (label, value) in &tv.ts_rows {
                ui.label(RichText::new(label).color(palette::TEXT2));
                // Sign-color the value cell on the RENDERED string's first byte: '-' is a loss,
                // '+' a gain, else neutral.
                //
                // ⚠ Since the rows became `MetricUnit::render`'s output, nothing produces a leading
                // '+' — that method deliberately owns scaling and precision and nothing else — so
                // in practice only LOSSES color today. See [`tearsheet_rows`] for why that is
                // preferred over teaching the row source a second formatting rule, and for what
                // recovering the gain color would actually take (the grid reading the VALUE rather
                // than the string, which is a change to `tools::ToolView`'s row type).
                let col = match value.as_bytes().first() {
                    Some(b'+') => POS,
                    Some(b'-') => NEG,
                    _ => palette::TEXT,
                };
                ui.label(RichText::new(value).monospace().color(col));
                ui.end_row();
            }
        },
    );
}

/// Flatten a `LiveTearsheet` into ordered `(label, value)` rows for the grid — **every
/// `vike_analytics::metric_catalog::METRICS` entry, in that table's own declaration order,
/// rendered by that entry's own `MetricUnit`.**
///
/// # It is now an adapter, and that is the whole change
///
/// This function used to be a twenty-row `vec![]` of `(label, format!(…))` pairs against a catalog
/// of thirty-eight, with its own precision per row and its own `* 100.0` on the percent ones; then
/// it became a catalog walk over a local `live_metric_value` accessor, which its own doc called the
/// residual and asked to have DELETED once the accessor moved onto the document type. That move
/// landed — `vike_report::LiveTearsheet::rendered_rows` is the one rendering of that document's
/// numbers, shared with the `Display` table and the HTML tearsheet — so the accessor and its pin
/// test are gone and what remains here is the `&'static str` → `String` conversion `ToolView`'s
/// row type needs. Nothing about the roster, the order or the formatting is decided in this file.
///
/// # A row this document does not carry now says so, where it used to VANISH
///
/// The old accessor answered `None` for the thirteen catalog ids the flat struct had no field for,
/// and those rows were OMITTED — defensible then, because `None` was a fact about the TYPE rather
/// than about the session, identical on every run. It is not a fact about the type any more:
/// `LiveTearsheet` carries the whole catalog, so `None` means THIS reconstruction recorded no
/// number, which is exactly the case `vike_analytics::report::BacktestReport::render_metrics`
/// prints `vike_report::NOT_RECORDED` for. So the panel prints it too. The rows that were missing
/// because the struct dropped them (`long_ratio`, the two ulcer figures, the four
/// return-distribution figures and the rest) now carry real numbers over the same fills, which is
/// the "honest fix" the deleted accessor's doc named; `funding_paid` keeps saying *not recorded* on
/// the journal door, deliberately, because a fill stream folds no funding cashflow and a `0.00`
/// there would tell an operator of a live PERP account that funding was free.
///
/// `NOT_RECORDED` begins with `n`, so the grid's sign-coloring reads it as neutral rather than as a
/// loss — worth stating because that colouring keys on the rendered string's first byte.
fn tearsheet_rows(s: &vike_report::LiveTearsheet) -> Vec<(String, String)> {
    s.rendered_rows().into_iter().map(|(id, text)| (id.to_string(), text)).collect()
}

#[cfg(test)]
mod tests {
    use super::tearsheet_rows;
    use vike_analytics::metric_catalog::METRICS;
    use vike_report::{LiveTearsheet, MetricValues, NOT_RECORDED, TEARSHEET_SCHEMA};

    /// A sheet with distinctive values on the rows the formatting assertions read, and NOTHING
    /// recorded elsewhere — so both halves of `rendered_rows` are exercised: a rendered number and
    /// [`NOT_RECORDED`].
    ///
    /// Built through `MetricValues::from_fn` rather than as a struct literal because the document
    /// has no per-metric fields left to name; the ids below are the same strings an operator types
    /// at `--metrics`.
    fn sheet() -> LiveTearsheet {
        const VALUES: &[(&str, f64)] = &[
            ("n_trades", 3.0),
            ("final_equity", 10_500.5),
            ("total_return", 0.1234),
            ("net_profit", 500.5),
            ("gross_profit", 700.0),
            ("gross_loss", -199.5),
            ("total_fees", 1.2345),
            ("win_rate", 0.6667),
            ("profit_factor", f64::INFINITY),
            ("sharpe", 1.5),
            ("sortino", 2.25),
            ("calmar", 0.75),
            ("max_drawdown", -0.25),
            ("cagr", -0.05),
            ("sqn", 1.1),
            ("avg_win", 350.0),
            ("avg_loss", -199.5),
            ("payoff_ratio", 1.75),
            ("expected_payoff", 166.83),
            ("largest_win", 400.0),
            ("largest_loss", -199.5),
            ("consecutive_wins", 2.0),
            ("consecutive_losses", 1.0),
            ("value_at_risk_95", -0.02),
            ("expected_shortfall_95", -0.03),
        ];
        LiveTearsheet {
            schema: TEARSHEET_SCHEMA,
            name: None,
            metrics: MetricValues::from_fn(|id| {
                VALUES.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
            }),
        }
    }

    /// The grid shows the WHOLE catalog, in the catalog's own declaration order — pinned against
    /// `METRICS` rather than against a literal, because a literal here would be the hand copy this
    /// file has already deleted twice.
    ///
    /// ⚠ This replaces `the_rows_a_live_tearsheet_cannot_answer_are_pinned`, whose whole subject —
    /// a pinned SET of catalog rows the panel silently skipped — no longer exists: nothing is
    /// filtered, so there is no omission to keep honest. That makes this the stronger claim of the
    /// two, and it is also why the old `UNANSWERABLE` list is not carried forward as data.
    #[test]
    fn the_panel_renders_every_catalog_row_in_the_catalogs_own_order() {
        let rows = tearsheet_rows(&sheet());
        let rendered: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
        let expected: Vec<&str> = METRICS.iter().map(|m| m.id).collect();
        assert_eq!(rendered, expected);
        // …and it is far WIDER than the twenty-row hand copy that started this, which is the
        // defect that motivated every step of it.
        assert!(rendered.len() > 20, "{} rows", rendered.len());
    }

    /// The formatting is `MetricUnit::render`'s, per the row's OWN unit — so the scaling, the
    /// precision and the non-finite spelling are all decided in one place for this panel, the
    /// CLI, the HTML tearsheet and the stored report alike.
    #[test]
    fn values_are_rendered_by_the_rows_own_unit() {
        let rows = tearsheet_rows(&sheet());
        let get = |id: &str| rows.iter().find(|(l, _)| l == id).map(|(_, v)| v.as_str());
        // Count: no decimals.
        assert_eq!(get("n_trades"), Some("3"));
        assert_eq!(get("consecutive_wins"), Some("2"));
        // Money: two decimals, no scaling.
        assert_eq!(get("final_equity"), Some("10500.50"));
        assert_eq!(get("avg_loss"), Some("-199.50"));
        // Percent: the `* 100.0` a hand-rolled renderer forgets, four decimals.
        assert_eq!(get("total_return"), Some("12.3400%"));
        assert_eq!(get("cagr"), Some("-5.0000%"));
        assert_eq!(get("max_drawdown"), Some("-25.0000%"));
        assert_eq!(get("win_rate"), Some("66.6700%"));
        // Ratio: four decimals, and the house inf sentinel reaches the operator as Rust's own
        // `inf` — the same bytes every other door prints, rather than this panel's old `∞` glyph.
        assert_eq!(get("payoff_ratio"), Some("1.7500"));
        assert_eq!(get("profit_factor"), Some("inf"));
        // A row this reconstruction recorded no number for says so — it is PRESENT and reads
        // `not recorded`, where the deleted accessor made it vanish from the panel entirely.
        assert_eq!(get("omega"), Some(NOT_RECORDED));
    }
}
