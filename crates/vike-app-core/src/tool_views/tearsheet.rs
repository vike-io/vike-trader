//! The Tearsheet tool body — the live performance summary read back from THIS session's command
//! journal. Moved verbatim from `vike-app`'s `main.rs` (tool-view extraction batch 1); the ONE
//! adaptation is that the `VIKE_JOURNAL_DIR` env read moved UP to the binary
//! ([`ToolCtx::journal_dir`]) — libraries take configuration as parameters.

use super::ToolCtx;
use crate::tools;
use vike_ui_theme::palette as theme;

/// The Tearsheet tool body: the live performance summary read back from THIS session's command
/// journal (the durability sink that `VIKE_JOURNAL_DIR` / `VIKE_RUN_PROFILE` enable — see
/// `vike_core::journal_config_from_env`). Reconstructs closed trades through the shared
/// `compute_fill` cost-basis seam and computes ~20 stats via `vike_report::LiveTearsheet` — the same
/// numbers a backtest produces. READ-ONLY. Cached: the journal read + trade reconstruction runs on
/// first show and on an explicit Refresh, never per frame.
pub fn tearsheet_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::{Color32, RichText};
    const C_TEXT: Color32 = theme::TEXT;
    const C_T2: Color32 = theme::TEXT2;
    const C_T3: Color32 = theme::TEXT3;
    const POS: Color32 = theme::UP;
    const NEG: Color32 = theme::DOWN;

    ui.horizontal(|ui| {
        ui.label(RichText::new("Live Tearsheet").strong().color(C_TEXT));
        ui.label(RichText::new("· from the command journal").small().color(C_T3));
    });
    ui.add_space(4.0);

    // Journal dir = the SAME VIKE_JOURNAL_DIR that turns journaling ON for the producer, so the
    // panel reads exactly what this session records (read by the binary, threaded through
    // `ToolCtx`). Absent ⇒ nothing was journaled.
    let Some(dir) = ctx.journal_dir.as_deref() else {
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Journaling is off").color(C_T2));
            ui.label(
                RichText::new(
                    "Set VIKE_JOURNAL_DIR (or a VIKE_RUN_PROFILE with [sinks.journal]) and restart \
                     to record a journal this panel can read.",
                )
                .small()
                .color(C_T3),
            );
        });
        return;
    };

    // Controls: starting-capital seed + Refresh. The journal read + reconstruction is not a
    // per-frame cost, so it runs only on first show and when Refresh is clicked (seed edits apply
    // on the next Refresh).
    let mut reload = !tv.ts_loaded;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Seed").small().color(C_T3));
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
        ui.label(RichText::new(format!("dir: {}", dir.display())).small().color(C_T3));
        return;
    }

    egui::Grid::new("tearsheet_rows").num_columns(2).spacing([24.0, 4.0]).striped(true).show(
        ui,
        |ui| {
            for (label, value) in &tv.ts_rows {
                ui.label(RichText::new(label).color(C_T2));
                // Sign-color the value cell: a leading '+' is a gain, '-' a loss, else neutral.
                let col = match value.as_bytes().first() {
                    Some(b'+') => POS,
                    Some(b'-') => NEG,
                    _ => C_TEXT,
                };
                ui.label(RichText::new(value).monospace().color(col));
                ui.end_row();
            }
        },
    );
}

/// Flatten a `LiveTearsheet` into ordered `(label, value)` rows for the grid. Signed metrics carry a
/// leading `+`/`-` so [`tearsheet_tool_content`]'s renderer can sign-color them.
fn tearsheet_rows(s: &vike_report::LiveTearsheet) -> Vec<(String, String)> {
    let ratio = |v: f64| if v.is_finite() { format!("{v:.2}") } else { "∞".to_string() };
    vec![
        ("Trades".to_string(), s.n_trades.to_string()),
        ("Final equity".to_string(), format!("{:.2}", s.final_equity)),
        ("Total return".to_string(), format!("{:+.2}%", s.total_return * 100.0)),
        ("Net profit".to_string(), format!("{:+.2}", s.net_profit)),
        ("Gross profit".to_string(), format!("{:.2}", s.gross_profit)),
        ("Gross loss".to_string(), format!("{:.2}", s.gross_loss)),
        ("Total fees".to_string(), format!("{:.4}", s.total_fees)),
        ("Win rate".to_string(), format!("{:.1}%", s.win_rate * 100.0)),
        ("Profit factor".to_string(), ratio(s.profit_factor)),
        ("Sharpe".to_string(), format!("{:.2}", s.sharpe)),
        ("Sortino".to_string(), format!("{:.2}", s.sortino)),
        ("Calmar".to_string(), format!("{:.2}", s.calmar)),
        ("Max drawdown".to_string(), format!("-{:.2}%", s.max_drawdown.abs() * 100.0)),
        ("CAGR".to_string(), format!("{:+.2}%", s.cagr * 100.0)),
        ("SQN".to_string(), format!("{:.2}", s.sqn)),
        ("Avg win".to_string(), format!("{:+.2}", s.avg_win)),
        ("Avg loss".to_string(), format!("{:+.2}", s.avg_loss)),
        ("Payoff ratio".to_string(), ratio(s.payoff_ratio)),
        ("Largest win".to_string(), format!("{:+.2}", s.largest_win)),
        ("Largest loss".to_string(), format!("{:+.2}", s.largest_loss)),
    ]
}

#[cfg(test)]
mod tests {
    use super::tearsheet_rows;
    use vike_report::LiveTearsheet;

    /// A fully-populated sheet with distinctive values so every formatting rule is visible.
    fn sheet() -> LiveTearsheet {
        LiveTearsheet {
            name: None,
            n_trades: 3,
            final_equity: 10_500.5,
            total_return: 0.1234,
            net_profit: 500.5,
            gross_profit: 700.0,
            gross_loss: -199.5,
            total_fees: 1.2345,
            win_rate: 0.6667,
            profit_factor: f64::INFINITY,
            sharpe: 1.5,
            sortino: 2.25,
            calmar: 0.75,
            max_drawdown: -0.25,
            cagr: -0.05,
            sqn: 1.1,
            avg_win: 350.0,
            avg_loss: -199.5,
            payoff_ratio: 1.75,
            expected_payoff: 166.83,
            largest_win: 400.0,
            largest_loss: -199.5,
            consecutive_wins: 2,
            consecutive_losses: 1,
            value_at_risk_95: -0.02,
            expected_shortfall_95: -0.03,
        }
    }

    #[test]
    fn rows_pin_grid_order_and_formatting() {
        let rows = tearsheet_rows(&sheet());
        // The grid order is the rendered order — pin it.
        let labels: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Trades",
                "Final equity",
                "Total return",
                "Net profit",
                "Gross profit",
                "Gross loss",
                "Total fees",
                "Win rate",
                "Profit factor",
                "Sharpe",
                "Sortino",
                "Calmar",
                "Max drawdown",
                "CAGR",
                "SQN",
                "Avg win",
                "Avg loss",
                "Payoff ratio",
                "Largest win",
                "Largest loss",
            ]
        );
        let get = |label: &str| &rows.iter().find(|(l, _)| l == label).unwrap().1;
        assert_eq!(get("Trades"), "3");
        // Signed metrics carry the leading sign the renderer colors on.
        assert_eq!(get("Total return"), "+12.34%");
        assert_eq!(get("Net profit"), "+500.50");
        assert_eq!(get("CAGR"), "-5.00%");
        assert_eq!(get("Avg loss"), "-199.50");
        // Drawdown renders magnitude with a forced leading '-'.
        assert_eq!(get("Max drawdown"), "-25.00%");
        // Degenerate ∞ ratios (no losing trades) render as the infinity glyph, not "inf".
        assert_eq!(get("Profit factor"), "∞");
        assert_eq!(get("Payoff ratio"), "1.75");
        assert_eq!(get("Win rate"), "66.7%");
        assert_eq!(get("Total fees"), "1.2345");
    }
}
