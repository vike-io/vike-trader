//! The Options window's Portfolio Greeks tab — per-position + net portfolio Δ/Γ/ν/Θ over the
//! trader's open Deribit option positions, priced off the options poll's fetched chains via the pure
//! [`crate::orders::option_chain_position_greeks::position_greeks`]. Moved from `vike-app`'s `main.rs` (tool-view
//! extraction batch 1) as the Greeks window's body; that window became this tab on 2026-11-17
//! ([`super::options_tool_content`]).

use crate::ui::windows::WindowToolContext;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::metrics::space;
use vike_ui_theme::side::ThemeSidePair;

/// The Portfolio Greeks tab: the trader's open Deribit option positions with per-position and net
/// portfolio Δ/Γ/ν/Θ. READ-ONLY (no order entry — that's the Options tool / order ticket).
///
/// Positions come from the core snapshot (`venue == "deribit"`); each is priced off the LIVE
/// option chains the options poll fetched for its expiry (`tool_feeds.options_by_underlying` — spot +
/// per-option IV; each position's contract looked up by name in `tool_feeds.options_index`),
/// via the pure `crate::orders::option_chain_position_greeks::position_greeks` helper (`r = 0.0`, matching the
/// chain's own enrichment). A position outside the loaded chain window (different expiry/strike, or
/// chain not fetched) shows with `—` greeks — the documented v1 limitation.
pub(super) fn greeks_tool_content(ui: &mut egui::Ui, context: &WindowToolContext<'_>) {
    use crate::orders::option_chain_position_greeks::{
        OptionChainPositionGreeksInput, position_greeks,
    };
    use egui::RichText;
    let t = ThemeTokens::of(ui.ctx());
    let (positive, negative) = ThemeSidePair::PositiveNegative.texts(&t);
    let (td, snapshot) = (context.tool_feeds, context.snapshot);

    // Build the lite positions from the snapshot — Deribit option positions only.
    let positions: Vec<OptionChainPositionGreeksInput> = snapshot
        .positions
        .iter()
        .filter(|p| p.venue == "deribit")
        .map(|p| OptionChainPositionGreeksInput {
            instrument: p.symbol.to_string(),
            quantity: p.size,
            average_price: p.average_price,
            // Wave 5d: the venue-reported per-position coin delta from the reconcile path
            // (Deribit `get_positions.delta`), surfaced on the snapshot as a side map keyed
            // (venue, symbol). `Some` for a reconciled Deribit perp/future leg (folds into
            // `net_delta_usd` as `coin_delta × spot`); `None` for options (priced from the chain)
            // and for any leg with no reconciled venue delta (stays `unpriced`, never a guess).
            coin_delta: snapshot.coin_delta(&p.venue, &p.symbol),
            // `snapshot.positions` are the primary engine's, the venue's first block
            contract_value: snapshot.contract_value_of(&p.venue, &p.symbol),
        })
        .collect();
    // Ask the options poll for what pricing these needs: each held option's own expiry and a hedge
    // leg's front (its spot). Recorded every frame this window draws, so closing it stops the asks.
    td.options_link
        .want_positions(positions.iter().map(|p| p.instrument.as_str()), &td.options_index);

    ui.horizontal(|ui| {
        ui.label(RichText::new("Portfolio Greeks").strong().color(t.theme.text));
        ui.label(RichText::new("· Deribit options").small().color(t.theme.text_tertiary));
    });
    // The poll's status on a row of its own, wrapping at the window's width. It changes length pass
    // by pass after the window opens (empty, then waiting for the instrument list, then a refusal or
    // the live line); right-aligned beside the heading it ran over it and re-laid the row at every
    // change. An empty status still takes its one line, so its arrival moves nothing below it.
    ui.label(RichText::new(&td.options_status).small().color(t.theme.text_tertiary));
    ui.add_space(space::SM);

    if positions.is_empty() {
        ui.add_space(space::XL3);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No open option positions").color(t.theme.text_secondary));
            ui.label(
                RichText::new("Deribit option positions appear here with live Δ/Γ/ν/Θ.")
                    .small()
                    .color(t.theme.text_tertiary),
            );
        });
        return;
    }

    let report = position_greeks(&positions, &td.options_by_underlying, &td.options_index, 0.0);

    // A per-greek cell: coloured by sign, or a muted "—" when the option isn't in a loaded chain.
    let greek_cell = |ui: &mut egui::Ui, v: Option<f64>| {
        match v {
            Some(x) => {
                let col = if x >= 0.0 { positive } else { negative };
                ui.label(RichText::new(format!("{x:+.4}")).monospace().color(col));
            }
            None => {
                ui.label(RichText::new("—").monospace().color(t.theme.text_tertiary));
            }
        };
    };
    let number_cell = |ui: &mut egui::Ui, v: Option<f64>, dp: usize| match v {
        Some(x) => {
            ui.label(
                RichText::new(format!("{x:.*}", dp)).monospace().color(t.theme.text_secondary),
            );
        }
        None => {
            ui.label(RichText::new("—").monospace().color(t.theme.text_tertiary));
        }
    };

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new("greeks_positions")
            .num_columns(7)
            .striped(true)
            .spacing(egui::vec2(space::XL2, space::SM))
            .show(ui, |ui| {
                for h in ["Instrument", "Qty", "Avg Px", "Delta", "Gamma", "Vega", "Theta"] {
                    ui.label(RichText::new(h).small().strong().color(t.theme.text_tertiary));
                }
                ui.end_row();
                for row in &report.rows {
                    ui.label(RichText::new(&row.instrument).monospace().color(t.theme.text));
                    let qcol = if row.quantity >= 0.0 { positive } else { negative };
                    ui.label(
                        RichText::new(format!("{:+.4}", row.quantity)).monospace().color(qcol),
                    );
                    number_cell(ui, Some(row.average_price), 2);
                    greek_cell(ui, row.delta);
                    greek_cell(ui, row.gamma);
                    greek_cell(ui, row.vega);
                    greek_cell(ui, row.theta);
                    ui.end_row();
                }
                // NET summary row (bold, text ink) — sums the priced positions only.
                ui.label(RichText::new("NET").strong().color(t.theme.text));
                ui.label("");
                ui.label("");
                for v in [report.net_delta, report.net_gamma, report.net_vega, report.net_theta] {
                    let col = if v >= 0.0 { positive } else { negative };
                    ui.label(RichText::new(format!("{v:+.4}")).monospace().strong().color(col));
                }
                ui.end_row();
            });
        // USD-normalized net (dimensionally correct across BTC/ETH/SOL; perp/future hedge legs
        // INCLUDED) — the strategy-readable `report.net_usd`. The raw per-unit NET row above is
        // retained but its Δ cannot be summed across underlyings; this line is the correct total.
        let net = &report.net_usd;
        ui.horizontal(|ui| {
            ui.label(RichText::new("NET Δ · USD").strong().color(t.theme.text));
            let col = if net.net_delta_usd >= 0.0 { positive } else { negative };
            ui.label(
                RichText::new(format!("{:+.0}", net.net_delta_usd)).monospace().strong().color(col),
            );
            for (u, d) in &net.per_underlying {
                ui.label(
                    RichText::new(format!("· {u} {:+.0}", d.delta_usd))
                        .small()
                        .monospace()
                        .color(t.theme.text_secondary),
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
                .color(t.theme.text_tertiary),
            );
        }
        ui.add_space(space::MD);
        ui.label(
            RichText::new(
                "Δ per $1 spot · Γ per $1² · ν per 1 vol-pt · Θ per day. \
                 Positions outside the loaded chain window show — (widen the Options ±N or load \
                 their expiry).",
            )
            .small()
            .color(t.theme.text_tertiary),
        );
    });
}
