//! The R7.5 Trade panel family — the largest tool body still left in `vike-app`'s CI-excluded
//! `main.rs` before this batch. Moved verbatim (tool-view extraction batch 3, audit F1): the
//! cross-venue equity hero, the per-venue Accounts table, the focused trade card (order ticket +
//! sizing/leverage/TP-SL) and the working-orders table.
//!
//! Adaptations, all mechanical:
//!   * `theme::` -> [`vike_ui_theme::palette`] and `font::` -> [`vike_ui_theme::font`] (vike-app's
//!     own `theme`/`font` modules are already thin re-exports of exactly these).
//!   * the read-only `snap` arrives through [`ToolCtx`]; `&mut ToolView` stays a separate param
//!     (the `vike_panels::dom::draw` pattern) so the body keeps writing its OUT intents.
//!   * four decision helpers the render code used to inline ([`hero_totals`],
//!     [`split_base_quote`], [`unpriced_tooltip`], [`is_terminal_status`]) are now named, pure
//!     functions with unit tests — the POINT of the move, since `vike-app` has no CI.
//!
//! Everything else is byte-for-byte the previous `trade_*` code: same widths, same colors, same
//! order-intent writes into `tv`.

use super::ToolCtx;
use crate::{equity_panel, order_entry, tools, trade_sizing};
use vike_ui_theme::{font, palette as theme};

/// The cross-venue Realized / Fees / Funding aggregates the equity hero's stat strip shows —
/// plain naive folds over the panel rows, in row order (matching every other display sum here).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HeroTotals {
    pub realized: f64,
    pub fees: f64,
    pub funding: f64,
}

/// Sum the per-venue rows into the hero's [`HeroTotals`]. Pure; empty rows ⇒ all-zero (which the
/// hero renders as a `+0.0000` P&L chip, not as a blank).
pub fn hero_totals(rows: &[equity_panel::EquityRow]) -> HeroTotals {
    HeroTotals {
        realized: rows.iter().map(|r| r.realized).sum(),
        fees: rows.iter().map(|r| r.fees).sum(),
        funding: rows.iter().map(|r| r.funding).sum(),
    }
}

/// Split a venue symbol into `(base, quote)` for the size-field units — longest quote suffix
/// first (`USDT`/`USDC` before `USD`, so `BTCUSDT` is not read as `BTCUSD` + `T`). A symbol with
/// no known quote suffix is all base and defaults to a `USDT` quote; a symbol that is ONLY a
/// quote suffix (`"USDT"`) keeps the whole symbol as base rather than yielding an empty label.
pub fn split_base_quote(symbol: &str) -> (&str, &str) {
    let (base, quote) = if let Some(b) = symbol.strip_suffix("USDT") {
        (b, "USDT")
    } else if let Some(b) = symbol.strip_suffix("USDC") {
        (b, "USDC")
    } else if let Some(b) = symbol.strip_suffix("USD") {
        (b, "USD")
    } else {
        (symbol, "USDT")
    };
    (if base.is_empty() { symbol } else { base }, quote)
}

/// The Accounts row's unpriced-badge hover text: the comma-joined symbols of `venue`'s positions
/// that carry no `mark_source`. Derived here because `EquityRow` carries only the COUNT (the T4
/// model is display-thin). Falls back to a generic label when the venue is absent from the
/// snapshot or every position is priced.
pub fn unpriced_tooltip(snap: &vike_core::CoreSnapshot, venue: &str) -> String {
    snap.portfolio
        .venues
        .iter()
        .find(|v| v.venue == venue)
        .map(|vb| {
            vb.positions
                .iter()
                .filter(|p| p.mark_source.is_none())
                .map(|p| p.symbol.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unpriced positions".to_string())
}

/// Is this `OrderView::status` a TERMINAL one (no cancel button)? The exact set the orders table
/// has always used — anything else (NEW / ACCEPTED / PARTIALLY_FILLED / …) is still working and
/// keeps its inline ✕.
pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, "FILLED" | "CANCELED" | "REJECTED" | "DENIED" | "EXPIRED" | "LIQUIDATED")
}

/// Right-aligned fixed-width monospace numeric cell — the aligned-table idiom (mirrors the
/// Calendar table's `allocate_ui_with_layout` columns). Used by the Trade panel's venue +
/// orders tables so numbers line up on the decimal.
fn trade_num_cell(ui: &mut egui::Ui, w: f32, txt: String, col: egui::Color32) {
    use egui::{Align, Label, Layout, RichText, vec2};
    ui.allocate_ui_with_layout(vec2(w, 18.0), Layout::right_to_left(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.add(Label::new(RichText::new(txt).monospace().size(12.0).color(col)).truncate());
    });
}

/// Left-aligned fixed-width text cell (venue-name / side columns of the Trade tables).
fn trade_text_cell(ui: &mut egui::Ui, w: f32, rt: egui::RichText) {
    use egui::{Align, Label, Layout, vec2};
    ui.allocate_ui_with_layout(vec2(w, 18.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.add(Label::new(rt).truncate());
    });
}

/// The cross-venue equity HERO (full-width, top of the Trade panel): big `equity_total`, a P&L
/// chip (summed venue realized), the unpriced badge, and a Realized/Fees/Funding stat strip.
/// Replaces the old four account cards, which duplicated the primary venue's table row.
fn trade_equity_hero(
    ui: &mut egui::Ui,
    model: &equity_panel::EquityPanelModel,
    margin_used_total: f64,
) {
    use egui::{Color32, RichText};
    const UP: Color32 = theme::UP;
    const DOWN: Color32 = theme::DOWN;
    const TEXT: Color32 = theme::TEXT;
    const TEXT2: Color32 = theme::TEXT2;
    const WARN: Color32 = theme::WARN;

    let HeroTotals { realized, fees, funding } = hero_totals(&model.rows);

    ui.label(RichText::new("CROSS-VENUE EQUITY").size(10.5).color(TEXT2).strong());
    ui.horizontal(|ui| {
        ui.label(font::extralight(format!("{:.2}", model.equity_total)).size(28.0).color(TEXT));
        let (pc, ptxt) = if realized >= 0.0 {
            (UP, format!("P&L +{realized:.2}"))
        } else {
            (DOWN, format!("P&L {realized:.2}"))
        };
        ui.label(RichText::new(ptxt).size(11.0).color(pc).monospace());
        if model.missing_prices_total > 0 {
            ui.label(
                RichText::new(format!("⚠ {} unpriced", model.missing_prices_total))
                    .size(11.0)
                    .color(Color32::BLACK)
                    .background_color(WARN),
            );
        }
    });

    // stat strip: cross-venue Realized · Fees · Funding aggregates
    ui.add_space(7.0);
    let stat = |ui: &mut egui::Ui, k: &str, v: String, col: Color32| {
        ui.vertical(|ui| {
            ui.label(RichText::new(k).size(10.0).color(TEXT2));
            ui.label(RichText::new(v).size(13.0).color(col).monospace());
        });
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 22.0;
        stat(ui, "Realized", format!("{realized:+.4}"), if realized >= 0.0 { UP } else { DOWN });
        stat(ui, "Fees", format!("{fees:.4}"), TEXT2);
        stat(ui, "Funding", format!("{funding:+.4}"), if funding >= 0.0 { UP } else { DOWN });
        if margin_used_total > 0.0 {
            stat(ui, "Margin used", format!("{margin_used_total:.2}"), TEXT2);
        }
    });
}

/// The Accounts section: an aligned per-venue table (Venue · mode · Equity · uPnL · Fees ·
/// Funding) with right-aligned tabular numbers, the per-venue unpriced badge (+ hover), and a
/// per-asset balances line from `balances_by_asset`.
fn trade_accounts_section(
    ui: &mut egui::Ui,
    snap: &vike_core::CoreSnapshot,
    model: &equity_panel::EquityPanelModel,
) {
    use egui::{Align, Color32, Label, Layout, RichText, vec2};
    const UP: Color32 = theme::UP;
    const DOWN: Color32 = theme::DOWN;
    const TEXT: Color32 = theme::TEXT;
    const TEXT2: Color32 = theme::TEXT2;
    const TEXT3: Color32 = theme::TEXT3;
    const WARN: Color32 = theme::WARN;
    const BORD: Color32 = theme::BORDER;

    const WV: f32 = 66.0; // venue
    const WM: f32 = 24.0; // mode tag
    const WN: f32 = 66.0; // numeric columns

    ui.label(RichText::new("Accounts").size(12.0).color(TEXT).strong());
    ui.add_space(3.0);
    // header row
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        trade_text_cell(ui, WV, RichText::new("Venue").size(10.0).color(TEXT3));
        trade_text_cell(ui, WM, RichText::new("").size(10.0).color(TEXT3));
        for h in ["Equity", "uPnL", "Fees", "Funding"] {
            ui.allocate_ui_with_layout(
                vec2(WN, 16.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.set_min_width(WN);
                    ui.add(Label::new(RichText::new(h).size(10.0).color(TEXT3)).truncate());
                },
            );
        }
    });
    ui.painter().hline(ui.min_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, BORD));
    ui.add_space(2.0);

    if model.rows.is_empty() {
        ui.label(RichText::new("no venues").color(TEXT2));
    }
    for row in &model.rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            trade_text_cell(ui, WV, RichText::new(&row.venue).size(12.0).color(TEXT).strong());
            trade_text_cell(ui, WM, RichText::new(&row.mode).size(10.0).color(TEXT3));
            trade_num_cell(ui, WN, format!("{:.2}", row.equity), TEXT);
            trade_num_cell(
                ui,
                WN,
                format!("{:+.4}", row.unrealized),
                if row.unrealized >= 0.0 { UP } else { DOWN },
            );
            trade_num_cell(ui, WN, format!("{:.4}", row.fees), TEXT2);
            trade_num_cell(
                ui,
                WN,
                format!("{:+.4}", row.funding),
                if row.funding >= 0.0 { UP } else { DOWN },
            );
            if row.missing_prices > 0 {
                ui.label(
                    RichText::new(format!("⚠ {}", row.missing_prices))
                        .size(11.0)
                        .color(Color32::BLACK)
                        .background_color(WARN),
                )
                .on_hover_text(unpriced_tooltip(snap, &row.venue));
            }
        });
    }

    if !snap.portfolio.balances_by_asset.is_empty() {
        ui.add_space(5.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for (asset, qty) in &snap.portfolio.balances_by_asset {
                ui.label(
                    RichText::new(format!("{asset} {qty:.4}")).size(11.0).color(TEXT2).monospace(),
                );
            }
        });
    }
}

/// The focused trade card: bordered frame with the instrument header, an inline position line,
/// and the order ticket. (A1 keeps today's market ticket; A2 replaces it with the sizing/
/// leverage controls.)
fn trade_card(ui: &mut egui::Ui, snap: &vike_core::CoreSnapshot, tv: &mut tools::ToolView) {
    use egui::{Color32, RichText};
    const UP: Color32 = theme::UP;
    const DOWN: Color32 = theme::DOWN;
    const ACCENT: Color32 = theme::ACCENT;
    const TEXT: Color32 = theme::TEXT;
    const TEXT2: Color32 = theme::TEXT2;
    const TEXT3: Color32 = theme::TEXT3;
    const CARD: Color32 = theme::CARD;
    const SURFACE: Color32 = theme::SURFACE;
    const BORD: Color32 = theme::BORDER;

    let mark = snap.marks.iter().find(|(_, s, _)| s == &snap.symbol).map(|(_, _, p)| *p);
    let pos = snap.positions.iter().find(|p| p.symbol == snap.symbol);
    let size = pos.map_or(0.0, |p| p.size);

    egui::Frame::new()
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0, BORD))
        .inner_margin(egui::Margin::same(12))
        .corner_radius(6.0)
        .show(ui, |ui| {
            // header: symbol · PAPER badge · mark (right) · fault
            ui.horizontal(|ui| {
                ui.label(RichText::new(&snap.symbol).size(16.0).color(TEXT).strong());
                ui.label(
                    RichText::new("PAPER")
                        .size(10.0)
                        .color(Color32::BLACK)
                        .background_color(ACCENT),
                );
                if let Some(fault) = &snap.fault {
                    ui.label(RichText::new(format!("⚠ {fault}")).size(11.0).color(DOWN));
                }
                if let Some(m) = mark {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{m:.2}")).size(15.0).color(TEXT).monospace(),
                        );
                    });
                }
            });

            // position line
            ui.add_space(9.0);
            egui::Frame::new()
                .fill(SURFACE)
                .inner_margin(egui::Margin::symmetric(9, 6))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    if let Some(p) = pos.filter(|p| p.size.abs() > 1e-12) {
                        let upnl = p.unrealized;
                        let col = if p.size > 0.0 { UP } else { DOWN };
                        ui.horizontal(|ui| {
                            let (rect, painter) =
                                ui.allocate_painter(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            painter.circle_filled(rect.rect.center(), 4.0, col);
                            ui.label(
                                RichText::new(if p.size > 0.0 { "LONG" } else { "SHORT" })
                                    .size(11.0)
                                    .color(col)
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(format!("{:.6}", p.size.abs()))
                                    .color(TEXT)
                                    .monospace(),
                            );
                            ui.label(
                                RichText::new(format!("@ {:.2}", p.avg_px))
                                    .color(TEXT2)
                                    .monospace(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{upnl:+.4}"))
                                            .color(if upnl >= 0.0 { UP } else { DOWN })
                                            .monospace(),
                                    );
                                },
                            );
                        });
                        // truthful (Phase B) per-position leverage + liquidation estimate — only
                        // when leverage > 1× (a 1× long has no price-driven liquidation).
                        if p.leverage > 0.0 && p.liq_price > 0.0 {
                            ui.label(
                                RichText::new(format!(
                                    "{:.0}× · liq {:.2}",
                                    p.leverage, p.liq_price
                                ))
                                .size(10.0)
                                .color(TEXT3)
                                .monospace(),
                            );
                        }
                    } else {
                        ui.label(RichText::new("Flat").color(TEXT3));
                    }
                });

            // ---- order ticket (A2: sizing + leverage) ----
            use trade_sizing::SizeMode;
            const HOVER: Color32 = theme::HOVER;
            let mark_px = mark.unwrap_or(0.0);
            let lev = tv.trade_leverage.max(1.0);
            // Phase B: TRUTHFUL free buying power from the primary venue block (the margin engine
            // computes it via `margin::free_buying_power`); falls back to equity only before the
            // first block is built.
            let free_bp = snap
                .portfolio
                .venues
                .first()
                .map_or_else(|| snap.portfolio.equity.max(0.0), |v| v.free_bp);
            // split the symbol into (base, quote) for the size-field units
            let (base, quote) = split_base_quote(&snap.symbol);

            // leverage + margin-mode pills
            ui.add_space(11.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let mode_txt = if tv.trade_margin_isolated { "Isolated" } else { "Cross" };
                if ui
                    .add(
                        egui::Button::new(RichText::new(mode_txt).size(11.0).color(TEXT2))
                            .fill(SURFACE)
                            .stroke(egui::Stroke::new(1.0, BORD)),
                    )
                    .clicked()
                {
                    tv.trade_margin_isolated = !tv.trade_margin_isolated;
                }
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!("{lev:.0}× ▾")).size(11.0).color(ACCENT),
                        )
                        .fill(SURFACE)
                        .stroke(egui::Stroke::new(1.0, BORD)),
                    )
                    .clicked()
                {
                    tv.trade_lev_open = !tv.trade_lev_open;
                }
            });
            if tv.trade_lev_open {
                ui.add_space(4.0);
                // changing leverage re-tunes the live engine's per-symbol IM (= 1/leverage) via
                // Command::SetMargin, so the gate + liq-price update truthfully.
                //
                // The venue is the SNAPSHOT's primary, not a `"binance"` literal — the same fix
                // `order_dispatch` applies to the order path, and for the same reason: this card
                // already renders and sizes against `snap.symbol`, so pairing that symbol with a
                // hardcoded venue re-margined a market the panel was not showing. Byte-identical
                // on the local FAT build (`vike_run::build_node`'s primary IS binance/BTCUSDT);
                // correct for a control-enabled `--observe` client whose remote daemon's primary
                // is polymarket/hyperliquid.
                if ui
                    .add(egui::Slider::new(&mut tv.trade_leverage, 1.0..=125.0).step_by(1.0))
                    .changed()
                {
                    tv.trade_set_margin = Some((
                        snap.venue.clone(),
                        snap.symbol.clone(),
                        1.0 / tv.trade_leverage.max(1.0),
                    ));
                }
            }

            // order-type tabs (Market / Limit / Stop)
            ui.add_space(9.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let w = (ui.available_width() - 8.0) / 3.0;
                for (k, lbl) in [
                    (order_entry::OrderKind::Market, "Market"),
                    (order_entry::OrderKind::Limit, "Limit"),
                    (order_entry::OrderKind::Stop, "Stop"),
                ] {
                    let on = tv.trade_order_kind == k;
                    if ui
                        .add(
                            egui::Button::new(RichText::new(lbl).size(11.0).color(if on {
                                TEXT
                            } else {
                                TEXT2
                            }))
                            .fill(if on { HOVER } else { SURFACE })
                            .stroke(egui::Stroke::new(1.0, if on { ACCENT } else { BORD }))
                            .min_size(egui::vec2(w, 22.0)),
                        )
                        .clicked()
                    {
                        tv.trade_order_kind = k;
                    }
                }
            });
            // conditional price / trigger field
            match tv.trade_order_kind {
                order_entry::OrderKind::Limit => {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Price").size(11.0).color(TEXT2));
                        ui.add(
                            egui::TextEdit::singleline(&mut tv.trade_price)
                                .desired_width(f32::INFINITY),
                        );
                    });
                }
                order_entry::OrderKind::Stop => {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Trigger").size(11.0).color(TEXT2));
                        ui.add(
                            egui::TextEdit::singleline(&mut tv.trade_trigger)
                                .desired_width(f32::INFINITY),
                        );
                    });
                }
                order_entry::OrderKind::Market => {}
            }

            // sizing-mode segmented
            ui.add_space(9.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let w = (ui.available_width() - 8.0) / 3.0;
                for (m, lbl) in
                    [(SizeMode::Qty, "Qty"), (SizeMode::Amount, "Amount"), (SizeMode::Cost, "Cost")]
                {
                    let on = tv.trade_size_mode == m;
                    if ui
                        .add(
                            egui::Button::new(RichText::new(lbl).size(11.0).color(if on {
                                TEXT
                            } else {
                                TEXT2
                            }))
                            .fill(if on { HOVER } else { SURFACE })
                            .stroke(egui::Stroke::new(1.0, if on { ACCENT } else { BORD }))
                            .min_size(egui::vec2(w, 22.0)),
                        )
                        .clicked()
                    {
                        tv.trade_size_mode = m;
                    }
                }
            });

            // size field + derived conversion line
            ui.add_space(8.0);
            let size_label = match tv.trade_size_mode {
                SizeMode::Qty => format!("Qty ({base})"),
                SizeMode::Amount => format!("Amount ({quote})"),
                SizeMode::Cost => format!("Cost ({quote})"),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(size_label).size(11.0).color(TEXT2));
                ui.add(egui::TextEdit::singleline(&mut tv.trade_qty).desired_width(f32::INFINITY));
            });
            let input = tv.trade_qty.trim().parse::<f64>().unwrap_or(0.0);
            let qty = trade_sizing::size_to_qty(tv.trade_size_mode, input, mark_px, lev);
            ui.add_space(3.0);
            let derived = match tv.trade_size_mode {
                SizeMode::Qty => {
                    format!("≈ {:.2} {quote}", trade_sizing::notional(qty, mark_px))
                }
                _ => format!(
                    "≈ {:.6} {base} · margin {:.2} {quote}",
                    qty,
                    trade_sizing::margin_cost(qty, mark_px, lev)
                ),
            };
            ui.label(RichText::new(derived).size(10.5).color(TEXT3).monospace());

            // %-of-buying-power presets (write a base qty; switch to Qty mode)
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let w = (ui.available_width() - 12.0) / 4.0;
                for pct in [0.25_f64, 0.5, 0.75, 1.0] {
                    let lbl = if pct >= 1.0 {
                        "Max".to_string()
                    } else {
                        format!("{}%", (pct * 100.0) as i32)
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(lbl).size(10.5).color(TEXT2))
                                .fill(SURFACE)
                                .stroke(egui::Stroke::new(1.0, BORD))
                                .min_size(egui::vec2(w, 20.0)),
                        )
                        .clicked()
                    {
                        let q = trade_sizing::pct_to_qty(pct, free_bp, lev, mark_px);
                        tv.trade_qty = format!("{q:.6}");
                        tv.trade_size_mode = SizeMode::Qty;
                    }
                }
            });

            // optional TP/SL bracket — attaches OCO exits (arm on the entry's fill) to the order
            ui.add_space(8.0);
            if ui
                .selectable_label(
                    tv.trade_tpsl_on,
                    RichText::new("+ TP / SL").size(11.0).color(if tv.trade_tpsl_on {
                        ACCENT
                    } else {
                        TEXT2
                    }),
                )
                .clicked()
            {
                tv.trade_tpsl_on = !tv.trade_tpsl_on;
            }
            if tv.trade_tpsl_on {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("TP").size(11.0).color(TEXT2));
                    ui.add(
                        egui::TextEdit::singleline(&mut tv.trade_tp).desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SL").size(11.0).color(TEXT2));
                    ui.add(
                        egui::TextEdit::singleline(&mut tv.trade_sl).desired_width(f32::INFINITY),
                    );
                });
            }

            // BUY / SELL + cost/max readout
            ui.add_space(10.0);
            // resolve the order type + its price/trigger (Limit needs a price, Stop a trigger)
            let kind = tv.trade_order_kind;
            let price = tv.trade_price.trim().parse::<f64>().ok().filter(|p| *p > 0.0);
            let trigger = tv.trade_trigger.trim().parse::<f64>().ok().filter(|p| *p > 0.0);
            let (tp, sl) = if tv.trade_tpsl_on {
                (
                    tv.trade_tp.trim().parse::<f64>().ok().filter(|p| *p > 0.0),
                    tv.trade_sl.trim().parse::<f64>().ok().filter(|p| *p > 0.0),
                )
            } else {
                (None, None)
            };
            let type_ok = match kind {
                order_entry::OrderKind::Market => true,
                order_entry::OrderKind::Limit => price.is_some(),
                order_entry::OrderKind::Stop => trigger.is_some(),
            };
            let armed = qty > 0.0 && type_ok && snap.fault.is_none();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let bw = (ui.available_width() - 6.0) / 2.0;
                let buy = ui.add_enabled(
                    armed,
                    egui::Button::new(
                        RichText::new("BUY").size(14.0).color(Color32::BLACK).strong(),
                    )
                    .fill(UP)
                    .min_size(egui::vec2(bw, 36.0)),
                );
                let sell = ui.add_enabled(
                    armed,
                    egui::Button::new(
                        RichText::new("SELL").size(14.0).color(Color32::BLACK).strong(),
                    )
                    .fill(DOWN)
                    .min_size(egui::vec2(bw, 36.0)),
                );
                if buy.clicked() {
                    tv.trade_submit = Some(tools::TradeSubmit {
                        side: 1,
                        qty,
                        reduce_only: false,
                        kind,
                        price,
                        trigger_price: trigger,
                        tp,
                        sl,
                    });
                }
                if sell.clicked() {
                    tv.trade_submit = Some(tools::TradeSubmit {
                        side: -1,
                        qty,
                        reduce_only: false,
                        kind,
                        price,
                        trigger_price: trigger,
                        tp,
                        sl,
                    });
                }
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "Cost {:.2} {quote} · Max {:.4} {base}",
                    trade_sizing::margin_cost(qty, mark_px, lev),
                    trade_sizing::max_qty(free_bp, lev, mark_px)
                ))
                .size(10.0)
                .color(TEXT3)
                .monospace(),
            );

            if size.abs() > 1e-12 {
                ui.add_space(6.0);
                let flat = ui.add(
                    egui::Button::new(RichText::new("Flatten position").color(TEXT))
                        .min_size(egui::vec2(ui.available_width(), 28.0)),
                );
                if flat.clicked() {
                    tv.trade_submit = Some(tools::TradeSubmit {
                        side: vike_model::closing_side(size),
                        qty: size.abs(),
                        reduce_only: true,
                        kind: order_entry::OrderKind::Market,
                        price: None,
                        trigger_price: None,
                        tp: None,
                        sl: None,
                    });
                }
            }
            ui.add_space(7.0);
            let help = match kind {
                order_entry::OrderKind::Market => {
                    "Market orders fill at the next 1m bar open (paper engine)."
                }
                order_entry::OrderKind::Limit => "Limit orders rest until price trades through.",
                order_entry::OrderKind::Stop => "Stop orders trigger a market fill at the trigger.",
            };
            ui.label(RichText::new(help).size(10.0).color(TEXT3));
        });
}

/// The working-orders section: an aligned table (Side · Qty · Type · Status · ✕) with a
/// colored side chip and cancel on non-terminal orders.
fn trade_orders_section(
    ui: &mut egui::Ui,
    snap: &vike_core::CoreSnapshot,
    tv: &mut tools::ToolView,
) {
    use egui::{Color32, RichText};
    const UP: Color32 = theme::UP;
    const DOWN: Color32 = theme::DOWN;
    const TEXT: Color32 = theme::TEXT;
    const TEXT2: Color32 = theme::TEXT2;
    const TEXT3: Color32 = theme::TEXT3;

    ui.label(
        RichText::new(format!("Orders ({})", snap.orders.len())).size(12.0).color(TEXT).strong(),
    );
    ui.add_space(3.0);
    if snap.orders.is_empty() {
        ui.label(RichText::new("No working orders.").size(12.0).color(TEXT3));
        return;
    }
    egui::ScrollArea::vertical().max_height(160.0).auto_shrink([false, false]).show(ui, |ui| {
        for o in &snap.orders {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let sc = if o.side > 0 { UP } else { DOWN };
                trade_text_cell(
                    ui,
                    36.0,
                    RichText::new(if o.side > 0 { "BUY" } else { "SELL" })
                        .size(11.0)
                        .color(sc)
                        .strong(),
                );
                trade_num_cell(ui, 78.0, format!("{:.4}", o.qty), TEXT);
                trade_text_cell(ui, 46.0, RichText::new(&o.order_type).color(TEXT2).size(11.0));
                trade_text_cell(ui, 74.0, RichText::new(o.status.as_str()).color(TEXT2).size(11.0));
                if !is_terminal_status(o.status.as_str()) && ui.small_button("✕").clicked() {
                    tv.trade_cancel = Some(o.client_order_id.clone());
                }
            });
        }
    });
}

/// The R7.5 Trade window: cross-venue equity + per-venue accounts + a focused trade card +
/// working orders. Responsive: two columns when wide (accounts/orders left, trade card right),
/// stacked (trade card first) when narrow. Reads the core's lossy `CoreSnapshot` and writes
/// order intents into `tv` OUT fields the App forwards to `core.try_command`.
pub fn trade_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    let snap = ctx.snap;
    // cap the composed content width so a maximized window doesn't stretch the tables/card
    // across the whole screen; a normal-sized Trade window (< cap) is unaffected.
    ui.set_max_width(ui.available_width().min(960.0));

    let model = equity_panel::EquityPanelModel::from_snapshot(snap);

    trade_equity_hero(ui, &model, snap.portfolio.margin_used_total);
    ui.add_space(12.0);

    if ui.available_width() >= 620.0 {
        let full = ui.available_width();
        let right_w = 258.0;
        let left_w = (full - right_w - 12.0).max(260.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(left_w);
                trade_accounts_section(ui, snap, &model);
                ui.add_space(14.0);
                trade_orders_section(ui, snap, tv);
            });
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.set_width(right_w);
                trade_card(ui, snap, tv);
            });
        });
    } else {
        trade_card(ui, snap, tv);
        ui.add_space(14.0);
        trade_accounts_section(ui, snap, &model);
        ui.add_space(14.0);
        trade_orders_section(ui, snap, tv);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_core::{CoreSnapshot, PositionView, VenueBlock};
    use vike_exec::{BalanceMode, PriceSource, TradingState};

    fn row(venue: &str, realized: f64, fees: f64, funding: f64) -> equity_panel::EquityRow {
        equity_panel::EquityRow {
            venue: venue.to_string(),
            equity: 0.0,
            realized,
            unrealized: 0.0,
            fees,
            funding,
            mode: "\u{0394}".to_string(),
            missing_prices: 0,
        }
    }

    fn position(symbol: &str, mark_source: Option<PriceSource>) -> PositionView {
        PositionView {
            venue: "binance".to_string(),
            symbol: symbol.to_string(),
            position_side: "BOTH".to_string(),
            size: 1.0,
            avg_px: 100.0,
            unrealized: 0.0,
            mark_source,
            leverage: 0.0,
            liq_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }
    }

    fn venue_block(venue: &str, positions: Vec<PositionView>) -> VenueBlock {
        VenueBlock {
            venue: venue.to_string(),
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: BalanceMode::Delta,
            multipliers: Default::default(),
            multiplier_default: 1.0,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_bp: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            positions,
        }
    }

    #[test]
    fn hero_totals_sums_every_row() {
        let rows = [row("binance", 1.5, 0.25, -0.5), row("bybit", -0.5, 0.75, 0.25)];
        let t = hero_totals(&rows);
        assert_eq!(t.realized, 1.0);
        assert_eq!(t.fees, 1.0);
        assert_eq!(t.funding, -0.25);
    }

    #[test]
    fn hero_totals_of_no_venues_is_all_zero() {
        assert_eq!(hero_totals(&[]), HeroTotals::default());
    }

    #[test]
    fn split_base_quote_prefers_the_longest_quote_suffix() {
        // USDT/USDC must win over the USD prefix of their own names.
        assert_eq!(split_base_quote("BTCUSDT"), ("BTC", "USDT"));
        assert_eq!(split_base_quote("SOLUSDC"), ("SOL", "USDC"));
        assert_eq!(split_base_quote("XBTUSD"), ("XBT", "USD"));
    }

    #[test]
    fn split_base_quote_defaults_an_unknown_quote_to_usdt() {
        assert_eq!(split_base_quote("EURGBP"), ("EURGBP", "USDT"));
        assert_eq!(split_base_quote("BTC-PERPETUAL"), ("BTC-PERPETUAL", "USDT"));
    }

    #[test]
    fn split_base_quote_never_yields_an_empty_base() {
        // a symbol that IS only the quote suffix keeps the whole symbol as the base label
        assert_eq!(split_base_quote("USDT"), ("USDT", "USDT"));
        assert_eq!(split_base_quote("USD"), ("USD", "USD"));
    }

    #[test]
    fn unpriced_tooltip_lists_only_the_positions_with_no_mark_source() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![venue_block(
            "binance",
            vec![
                position("BTCUSDT", Some(PriceSource::LastTrade)),
                position("PEPEUSDT", None),
                position("WIFUSDT", None),
            ],
        )];
        assert_eq!(unpriced_tooltip(&snap, "binance"), "PEPEUSDT, WIFUSDT");
    }

    #[test]
    fn unpriced_tooltip_falls_back_when_everything_is_priced_or_the_venue_is_absent() {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues =
            vec![venue_block("binance", vec![position("BTCUSDT", Some(PriceSource::LastTrade))])];
        assert_eq!(unpriced_tooltip(&snap, "binance"), "unpriced positions");
        assert_eq!(unpriced_tooltip(&snap, "okx"), "unpriced positions");
    }

    #[test]
    fn is_terminal_status_covers_the_whole_terminal_set() {
        for s in ["FILLED", "CANCELED", "REJECTED", "DENIED", "EXPIRED", "LIQUIDATED"] {
            assert!(is_terminal_status(s), "{s} should be terminal");
        }
    }

    #[test]
    fn is_terminal_status_leaves_working_orders_cancellable() {
        for s in ["NEW", "ACCEPTED", "PARTIALLY_FILLED", "SUBMITTED", "", "filled"] {
            assert!(!is_terminal_status(s), "{s} should still be working");
        }
    }
}
