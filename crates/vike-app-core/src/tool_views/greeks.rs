//! The Greeks tool body — per-position + net portfolio Δ/Γ/ν/Θ over the trader's open Deribit
//! option positions, priced off the Options tool's fetched chains via the pure
//! [`crate::options_greeks::position_greeks`]. Moved verbatim from `vike-app`'s `main.rs`
//! (tool-view extraction batch 1).

use super::ToolCtx;
use vike_ui_theme::palette as theme;

/// The Greeks tool body: the trader's open Deribit option positions with per-position and net
/// portfolio Δ/Γ/ν/Θ. READ-ONLY (no order entry — that's the Options tool / order ticket).
///
/// Positions come from the core snapshot (`venue == "deribit"`); each is priced off the LIVE
/// option chains the Options tool already fetched (`td.opt_by_underlying` — spot + per-option IV),
/// via the pure `crate::options_greeks::position_greeks` helper (`r = 0.0`, matching the
/// chain's own enrichment). A position outside the loaded chain window (different expiry/strike, or
/// chain not fetched) shows with `—` greeks — the documented v1 limitation.
pub fn greeks_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    use crate::options_greeks::{position_greeks, PositionViewLite};
    use egui::{Color32, RichText};
    const C_TEXT: Color32 = theme::TEXT;
    const C_T2: Color32 = theme::TEXT2;
    const C_T3: Color32 = theme::TEXT3;
    const C_ACC: Color32 = theme::ACCENT;
    const POS: Color32 = theme::UP;
    const NEG: Color32 = theme::DOWN;
    let (td, snap) = (ctx.td, ctx.snap);

    // Build the lite positions from the snapshot — Deribit option positions only.
    let positions: Vec<PositionViewLite> = snap
        .positions
        .iter()
        .filter(|p| p.venue == "deribit")
        .map(|p| PositionViewLite {
            instrument: p.symbol.clone(),
            qty: p.size,
            avg_px: p.avg_px,
            // Wave 5d: the venue-reported per-position coin delta from the reconcile path
            // (Deribit `get_positions.delta`), surfaced on the snapshot as a side map keyed
            // (venue, symbol). `Some` for a reconciled Deribit perp/future leg (folds into
            // `net_delta_usd` as `coin_delta × spot`); `None` for options (priced from the chain)
            // and for any leg with no reconciled venue delta (stays `unpriced`, never a guess).
            coin_delta: snap.coin_delta(&p.venue, &p.symbol),
        })
        .collect();

    ui.horizontal(|ui| {
        ui.label(RichText::new("Portfolio Greeks").strong().color(C_TEXT));
        ui.label(RichText::new("· Deribit options").small().color(C_T3));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(&td.opt_status).small().color(C_T3));
        });
    });
    ui.add_space(4.0);

    if positions.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No open option positions").color(C_T2));
            ui.label(
                RichText::new("Deribit option positions appear here with live Δ/Γ/ν/Θ.")
                    .small()
                    .color(C_T3),
            );
        });
        return;
    }

    let report = position_greeks(&positions, &td.opt_by_underlying, 0.0);

    // A per-greek cell: coloured by sign, or a muted "—" when the option isn't in a loaded chain.
    let greek_cell = |ui: &mut egui::Ui, v: Option<f64>| {
        match v {
            Some(x) => {
                let col = if x >= 0.0 { POS } else { NEG };
                ui.label(RichText::new(format!("{x:+.4}")).monospace().color(col));
            }
            None => {
                ui.label(RichText::new("—").monospace().color(C_T3));
            }
        };
    };
    let num_cell = |ui: &mut egui::Ui, v: Option<f64>, dp: usize| match v {
        Some(x) => {
            ui.label(RichText::new(format!("{x:.*}", dp)).monospace().color(C_T2));
        }
        None => {
            ui.label(RichText::new("—").monospace().color(C_T3));
        }
    };

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new("greeks_positions")
            .num_columns(7)
            .striped(true)
            .spacing(egui::vec2(14.0, 4.0))
            .show(ui, |ui| {
                for h in ["Instrument", "Qty", "Avg Px", "Delta", "Gamma", "Vega", "Theta"] {
                    ui.label(RichText::new(h).small().strong().color(C_T3));
                }
                ui.end_row();
                for row in &report.rows {
                    ui.label(RichText::new(&row.instrument).monospace().color(C_TEXT));
                    let qcol = if row.qty >= 0.0 { POS } else { NEG };
                    ui.label(RichText::new(format!("{:+.4}", row.qty)).monospace().color(qcol));
                    num_cell(ui, Some(row.avg_px), 2);
                    greek_cell(ui, row.delta);
                    greek_cell(ui, row.gamma);
                    greek_cell(ui, row.vega);
                    greek_cell(ui, row.theta);
                    ui.end_row();
                }
                // NET summary row (bold, accent) — sums the priced positions only.
                ui.label(RichText::new("NET").strong().color(C_ACC));
                ui.label("");
                ui.label("");
                for v in [report.net_delta, report.net_gamma, report.net_vega, report.net_theta] {
                    let col = if v >= 0.0 { POS } else { NEG };
                    ui.label(RichText::new(format!("{v:+.4}")).monospace().strong().color(col));
                }
                ui.end_row();
            });
        // USD-normalized net (dimensionally correct across BTC/ETH/SOL; perp/future hedge legs
        // INCLUDED) — the strategy-readable `report.net_usd`. The raw per-unit NET row above is
        // retained but its Δ cannot be summed across underlyings; this line is the correct total.
        let net = &report.net_usd;
        ui.horizontal(|ui| {
            ui.label(RichText::new("NET Δ · USD").strong().color(C_ACC));
            let col = if net.net_delta_usd >= 0.0 { POS } else { NEG };
            ui.label(
                RichText::new(format!("{:+.0}", net.net_delta_usd)).monospace().strong().color(col),
            );
            for (u, d) in &net.per_underlying {
                ui.label(
                    RichText::new(format!("· {u} {:+.0}", d.delta_usd))
                        .small()
                        .monospace()
                        .color(C_T2),
                );
            }
        });
        if !net.unpriced.is_empty() {
            ui.label(
                RichText::new(format!(
                    "{} position(s) unpriced — excluded from the net: {}",
                    net.unpriced.len(),
                    net.unpriced.join(", ")
                ))
                .small()
                .color(C_T3),
            );
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Δ per $1 spot · Γ per $1² · ν per 1 vol-pt · Θ per day. \
                 Positions outside the loaded chain window show — (widen the Options ±N or load \
                 their expiry).",
            )
            .small()
            .color(C_T3),
        );
    });
}
