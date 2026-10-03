//! The R7.5 Trade panel family — the largest tool body still left in `vike-app`'s CI-excluded
//! `main.rs` before this batch. Moved verbatim (tool-view extraction batch 3, audit F1): the
//! cross-venue equity hero, the per-venue Accounts table, the focused trade card (order ticket +
//! sizing/leverage/TP-SL) and the working-orders table.
//!
//! Adaptations, all mechanical:
//!   * `palette::` -> [`vike_ui_theme::palette`] and `font::` -> [`vike_ui_theme::font`] (vike-app's
//!     own `theme`/`font` modules were thin re-exports of exactly these when this moved).
//!   * the read-only `snap` arrives through [`ToolCtx`]; `&mut ToolView` stays a separate param
//!     (the `vike_panels::dom::draw` pattern) so the body keeps writing its OUT intents.
//!   * four decision helpers the render code used to inline ([`hero_totals`],
//!     [`split_base_quote`], [`unpriced_tooltip`], [`is_terminal_status`]) are now named, pure
//!     functions with unit tests — the POINT of the move, since `vike-app` had no CI tests then.
//!
//! Everything else is byte-for-byte the previous `trade_*` code: same widths, same colors, same
//! order-intent writes into `tv`.
//!
//! ⚠ Up and down are the chosen market-colour set since the design system's PR 4 — text in the
//! set's text colours, fills in its graphic colours; the fault line keeps `palette::DOWN` until the
//! status colours move (step 7).

use super::ToolCtx;
use crate::{orders::equity_panel, orders::order_entry, orders::trade_sizing, tools};
use vike_ui_theme::appearance::current;
use vike_ui_theme::market::MarketColors;
use vike_ui_theme::{icons, palette};

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

/// The ticket's TP/SL exits, read from its two price boxes: `(None, None)` with the toggle OFF, and
/// with it ON each leg is `Some` only when its box parses to a price above zero.
fn tpsl_legs(on: bool, tp: &str, sl: &str) -> (Option<f64>, Option<f64>) {
    if !on {
        return (None, None);
    }
    let leg = |s: &str| s.trim().parse::<f64>().ok().filter(|p| *p > 0.0);
    (leg(tp), leg(sl))
}

/// Whether the ticket's TP/SL state lets it arm: always with the toggle OFF, and with it ON only
/// when BOTH legs are priced (the controller's ruling in the node-bracket Task 3 review).
///
/// ⚠ This is the ONE refusal for a ticket with the toggle on and NEITHER leg priced: its legs are
/// `(None, None)`, which `crates/vike-app-core/src/orders/order_dispatch.rs`'s `plan_trade` cannot
/// tell from the toggle off. One priced leg, it refuses too (`TpSlNeedsBothLegs`).
fn tpsl_complete(on: bool, legs: (Option<f64>, Option<f64>)) -> bool {
    !on || (legs.0.is_some() && legs.1.is_some())
}

/// The old ticket's whole ARM verdict — `(armed, why_not)` — so the composition that decides
/// whether BUY/SELL can send anything is a pure function a test can drive, rather than a `let` in
/// the frame no test reaches.
///
/// `armed` needs a size, an order type whose price it needs is present (Limit a price, Stop a
/// trigger), no TP/SL objection and no core fault. `why_not` is the TP/SL objection the ticket
/// SHOWS under the buttons, and the reason they are disabled:
/// * TP/SL with a STOP entry — `DispatchRejectReason::BracketNeedsMarketOrLimit`. A bracket has no
///   stop entry, and the planner refuses it (`plan_trade`), but that refusal reaches only the LOG:
///   with the ticket armed the click looked like an order that went nowhere.
/// * TP/SL with a leg missing — `DispatchRejectReason::TpSlNeedsBothLegs` ([`tpsl_complete`]).
///
/// A size or price the ticket lacks has no line of its own: the boxes show what is missing.
fn ticket_armed(
    qty: f64,
    kind: order_entry::OrderKind,
    price: Option<f64>,
    trigger: Option<f64>,
    tpsl_on: bool,
    legs: (Option<f64>, Option<f64>),
    faulted: bool,
) -> (bool, Option<crate::orders::order_dispatch::DispatchRejectReason>) {
    use crate::orders::order_dispatch::DispatchRejectReason;
    let type_ok = match kind {
        order_entry::OrderKind::Market => true,
        order_entry::OrderKind::Limit => price.is_some(),
        order_entry::OrderKind::Stop => trigger.is_some(),
    };
    let why_not = if tpsl_on && kind == order_entry::OrderKind::Stop {
        Some(DispatchRejectReason::BracketNeedsMarketOrLimit)
    } else if !tpsl_complete(tpsl_on, legs) {
        Some(DispatchRejectReason::TpSlNeedsBothLegs)
    } else {
        None
    };
    (qty > 0.0 && type_ok && why_not.is_none() && !faulted, why_not)
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

    let mc = MarketColors::of(current(ui.ctx()).market);
    let HeroTotals { realized, fees, funding } = hero_totals(&model.rows);

    ui.label(RichText::new("CROSS-VENUE EQUITY").size(10.5).color(palette::TEXT2).strong());
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{:.2}", model.equity_total)).size(28.0).color(palette::TEXT),
        );
        let (pc, ptxt) = if realized >= 0.0 {
            (mc.up_text, format!("P&L +{realized:.2}"))
        } else {
            (mc.down_text, format!("P&L {realized:.2}"))
        };
        ui.label(RichText::new(ptxt).size(11.0).color(pc).monospace());
        if model.missing_prices_total > 0 {
            ui.label(
                icons::WARNING.before(
                    ui.style(),
                    RichText::new(format!("{} unpriced", model.missing_prices_total))
                        .size(11.0)
                        .color(Color32::BLACK)
                        .background_color(palette::WARN),
                ),
            );
        }
    });

    // stat strip: cross-venue Realized · Fees · Funding aggregates
    ui.add_space(7.0);
    let stat = |ui: &mut egui::Ui, k: &str, v: String, col: Color32| {
        ui.vertical(|ui| {
            ui.label(RichText::new(k).size(10.0).color(palette::TEXT2));
            ui.label(RichText::new(v).size(13.0).color(col).monospace());
        });
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 22.0;
        stat(
            ui,
            "Realized",
            format!("{realized:+.4}"),
            if realized >= 0.0 { mc.up_text } else { mc.down_text },
        );
        stat(ui, "Fees", format!("{fees:.4}"), palette::TEXT2);
        stat(
            ui,
            "Funding",
            format!("{funding:+.4}"),
            if funding >= 0.0 { mc.up_text } else { mc.down_text },
        );
        if margin_used_total > 0.0 {
            stat(ui, "Margin used", format!("{margin_used_total:.2}"), palette::TEXT2);
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

    const WV: f32 = 66.0; // venue
    const WM: f32 = 24.0; // mode tag
    const WN: f32 = 66.0; // numeric columns

    let mc = MarketColors::of(current(ui.ctx()).market);
    ui.label(RichText::new("Accounts").size(12.0).color(palette::TEXT).strong());
    ui.add_space(3.0);
    // header row
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        trade_text_cell(ui, WV, RichText::new("Venue").size(10.0).color(palette::TEXT3));
        trade_text_cell(ui, WM, RichText::new("").size(10.0).color(palette::TEXT3));
        for h in ["Equity", "uPnL", "Fees", "Funding"] {
            ui.allocate_ui_with_layout(
                vec2(WN, 16.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.set_min_width(WN);
                    ui.add(
                        Label::new(RichText::new(h).size(10.0).color(palette::TEXT3)).truncate(),
                    );
                },
            );
        }
    });
    ui.painter().hline(
        ui.min_rect().x_range(),
        ui.cursor().top(),
        egui::Stroke::new(1.0, palette::BORDER),
    );
    ui.add_space(2.0);

    if model.rows.is_empty() {
        ui.label(RichText::new("no venues").color(palette::TEXT2));
    }
    for row in &model.rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            trade_text_cell(
                ui,
                WV,
                RichText::new(&row.venue).size(12.0).color(palette::TEXT).strong(),
            );
            trade_text_cell(ui, WM, RichText::new(&row.mode).size(10.0).color(palette::TEXT3));
            trade_num_cell(ui, WN, format!("{:.2}", row.equity), palette::TEXT);
            trade_num_cell(
                ui,
                WN,
                format!("{:+.4}", row.unrealized),
                if row.unrealized >= 0.0 { mc.up_text } else { mc.down_text },
            );
            trade_num_cell(ui, WN, format!("{:.4}", row.fees), palette::TEXT2);
            trade_num_cell(
                ui,
                WN,
                format!("{:+.4}", row.funding),
                if row.funding >= 0.0 { mc.up_text } else { mc.down_text },
            );
            if row.missing_prices > 0 {
                ui.label(
                    icons::WARNING.before(
                        ui.style(),
                        RichText::new(row.missing_prices.to_string())
                            .size(11.0)
                            .color(Color32::BLACK)
                            .background_color(palette::WARN),
                    ),
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
                    RichText::new(format!("{asset} {qty:.4}"))
                        .size(11.0)
                        .color(palette::TEXT2)
                        .monospace(),
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

    let mc = MarketColors::of(current(ui.ctx()).market);
    let mark = snap.marks.iter().find(|(_, s, _)| s == &snap.symbol).map(|(_, _, p)| *p);
    let pos = snap.positions.iter().find(|p| p.symbol == snap.symbol);
    let size = pos.map_or(0.0, |p| p.size);

    egui::Frame::new()
        .fill(palette::CARD)
        .stroke(egui::Stroke::new(1.0, palette::BORDER))
        .inner_margin(egui::Margin::same(12))
        .corner_radius(6.0)
        .show(ui, |ui| {
            // header: symbol · PAPER badge · mark (right) · fault
            ui.horizontal(|ui| {
                ui.label(RichText::new(&snap.symbol).size(16.0).color(palette::TEXT).strong());
                ui.label(
                    RichText::new("PAPER")
                        .size(10.0)
                        .color(Color32::BLACK)
                        .background_color(palette::ACCENT),
                );
                if let Some(fault) = &snap.fault {
                    ui.label(
                        icons::WARNING.before(
                            ui.style(),
                            RichText::new(fault).size(11.0).color(palette::DOWN),
                        ),
                    );
                }
                if let Some(m) = mark {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{m:.2}"))
                                .size(15.0)
                                .color(palette::TEXT)
                                .monospace(),
                        );
                    });
                }
            });

            // position line
            ui.add_space(9.0);
            egui::Frame::new()
                .fill(palette::SURFACE)
                .inner_margin(egui::Margin::symmetric(9, 6))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    if let Some(p) = pos.filter(|p| p.size.abs() > 1e-12) {
                        let upnl = p.unrealized;
                        let (dot, word) = if p.size > 0.0 {
                            (mc.up, mc.up_text)
                        } else {
                            (mc.down, mc.down_text)
                        };
                        ui.horizontal(|ui| {
                            let (rect, painter) =
                                ui.allocate_painter(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            painter.circle_filled(rect.rect.center(), 4.0, dot);
                            ui.label(
                                RichText::new(if p.size > 0.0 { "LONG" } else { "SHORT" })
                                    .size(11.0)
                                    .color(word)
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(format!("{:.6}", p.size.abs()))
                                    .color(palette::TEXT)
                                    .monospace(),
                            );
                            ui.label(
                                RichText::new(format!("@ {:.2}", p.avg_px))
                                    .color(palette::TEXT2)
                                    .monospace(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{upnl:+.4}"))
                                            .color(if upnl >= 0.0 {
                                                mc.up_text
                                            } else {
                                                mc.down_text
                                            })
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
                                .color(palette::TEXT3)
                                .monospace(),
                            );
                        }
                    } else {
                        ui.label(RichText::new("Flat").color(palette::TEXT3));
                    }
                });

            // ---- order ticket (A2: sizing + leverage) ----
            use trade_sizing::SizeMode;
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
                        egui::Button::new(RichText::new(mode_txt).size(11.0).color(palette::TEXT2))
                            .fill(palette::SURFACE)
                            .stroke(egui::Stroke::new(1.0, palette::BORDER)),
                    )
                    .clicked()
                {
                    tv.trade_margin_isolated = !tv.trade_margin_isolated;
                }
                // The leverage, then the dropdown caret at its size and colour.
                const LEV_PT: f32 = 11.0;
                if ui
                    .add(
                        egui::Button::new((
                            RichText::new(format!("{lev:.0}×")).size(LEV_PT).color(palette::ACCENT),
                            icons::DISCLOSE_OPEN.rich().size(LEV_PT).color(palette::ACCENT),
                        ))
                        .fill(palette::SURFACE)
                        .stroke(egui::Stroke::new(1.0, palette::BORDER)),
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
                // on the local FAT build (`vike_mount::build_node`'s primary IS binance/BTCUSDT);
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
                                palette::TEXT
                            } else {
                                palette::TEXT2
                            }))
                            .fill(if on { palette::HOVER } else { palette::SURFACE })
                            .stroke(egui::Stroke::new(
                                1.0,
                                if on { palette::ACCENT } else { palette::BORDER },
                            ))
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
                        ui.label(RichText::new("Price").size(11.0).color(palette::TEXT2));
                        ui.add(
                            egui::TextEdit::singleline(&mut tv.trade_price)
                                .desired_width(f32::INFINITY),
                        );
                    });
                }
                order_entry::OrderKind::Stop => {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Trigger").size(11.0).color(palette::TEXT2));
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
                                palette::TEXT
                            } else {
                                palette::TEXT2
                            }))
                            .fill(if on { palette::HOVER } else { palette::SURFACE })
                            .stroke(egui::Stroke::new(
                                1.0,
                                if on { palette::ACCENT } else { palette::BORDER },
                            ))
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
                ui.label(RichText::new(size_label).size(11.0).color(palette::TEXT2));
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
            ui.label(RichText::new(derived).size(10.5).color(palette::TEXT3).monospace());

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
                            egui::Button::new(RichText::new(lbl).size(10.5).color(palette::TEXT2))
                                .fill(palette::SURFACE)
                                .stroke(egui::Stroke::new(1.0, palette::BORDER))
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
                        palette::ACCENT
                    } else {
                        palette::TEXT2
                    }),
                )
                .clicked()
            {
                tv.trade_tpsl_on = !tv.trade_tpsl_on;
            }
            if tv.trade_tpsl_on {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("TP").size(11.0).color(palette::TEXT2));
                    ui.add(
                        egui::TextEdit::singleline(&mut tv.trade_tp).desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SL").size(11.0).color(palette::TEXT2));
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
            let (tp, sl) = tpsl_legs(tv.trade_tpsl_on, &tv.trade_tp, &tv.trade_sl);
            // A ticket whose TP/SL it cannot send (a Stop entry, or a leg missing) does not arm,
            // and says why — the whole verdict is `ticket_armed`, so a test can drive it.
            let (armed, why_not) = ticket_armed(
                qty,
                kind,
                price,
                trigger,
                tv.trade_tpsl_on,
                (tp, sl),
                snap.fault.is_some(),
            );
            if let Some(why) = why_not {
                let look = current(ui.ctx());
                let px = look.text_size.px(vike_ui_theme::type_scale::TextRole::Caption);
                let text3 = vike_ui_theme::theme::Theme::of(look.theme).text3;
                ui.label(RichText::new(why.to_string()).size(px).color(text3));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let bw = (ui.available_width() - 6.0) / 2.0;
                let buy = ui.add_enabled(
                    armed,
                    egui::Button::new(
                        RichText::new("BUY").size(14.0).color(Color32::BLACK).strong(),
                    )
                    .fill(mc.up)
                    .min_size(egui::vec2(bw, 36.0)),
                );
                let sell = ui.add_enabled(
                    armed,
                    egui::Button::new(
                        RichText::new("SELL").size(14.0).color(Color32::BLACK).strong(),
                    )
                    .fill(mc.down)
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
                .color(palette::TEXT3)
                .monospace(),
            );

            if size.abs() > 1e-12 {
                ui.add_space(6.0);
                let flat = ui.add(
                    egui::Button::new(RichText::new("Flatten position").color(palette::TEXT))
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
            ui.label(RichText::new(help).size(10.0).color(palette::TEXT3));
        });
}

/// The working-orders section: an aligned table (Side · Qty · Type · Status · ✕) with a
/// colored side chip and cancel on non-terminal orders.
fn trade_orders_section(
    ui: &mut egui::Ui,
    snap: &vike_core::CoreSnapshot,
    tv: &mut tools::ToolView,
) {
    use egui::RichText;

    let mc = MarketColors::of(current(ui.ctx()).market);
    ui.label(
        RichText::new(format!("Orders ({})", snap.orders.len()))
            .size(12.0)
            .color(palette::TEXT)
            .strong(),
    );
    ui.add_space(3.0);
    if snap.orders.is_empty() {
        ui.label(RichText::new("No working orders.").size(12.0).color(palette::TEXT3));
        return;
    }
    egui::ScrollArea::vertical().max_height(160.0).auto_shrink([false, false]).show(ui, |ui| {
        for o in &snap.orders {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let sc = if o.side > 0 { mc.up_text } else { mc.down_text };
                trade_text_cell(
                    ui,
                    36.0,
                    RichText::new(if o.side > 0 { "BUY" } else { "SELL" })
                        .size(11.0)
                        .color(sc)
                        .strong(),
                );
                trade_num_cell(ui, 78.0, format!("{:.4}", o.qty), palette::TEXT);
                trade_text_cell(
                    ui,
                    46.0,
                    RichText::new(&o.order_type).color(palette::TEXT2).size(11.0),
                );
                trade_text_cell(
                    ui,
                    74.0,
                    RichText::new(o.status.as_str()).color(palette::TEXT2).size(11.0),
                );
                if !is_terminal_status(o.status.as_str())
                    && icons::named(ui.small_button(icons::CANCEL), "Cancel order").clicked()
                {
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
            account: None,
            route_key: venue.to_string(),
            symbol: String::new(),
            extra_symbols: Vec::new(),
            mode: None,
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

    /// The P&L chip's colour, read off a real frame of the equity hero under appearance `a`.
    fn pnl_colour(a: vike_ui_theme::appearance::Appearance, realized: f64) -> egui::Color32 {
        let ctx = egui::Context::default();
        vike_ui_theme::appearance::install(&ctx, &a);
        let model = equity_panel::EquityPanelModel {
            equity_total: 1000.0,
            missing_prices_total: 0,
            rows: vec![equity_panel::EquityRow {
                venue: "bybit".to_string(),
                equity: 1000.0,
                realized,
                unrealized: 0.0,
                fees: 0.0,
                funding: 0.0,
                mode: "auth".to_string(),
                missing_prices: 0,
            }],
        };
        let out = ctx.run_ui(egui::RawInput::default(), |ui| trade_equity_hero(ui, &model, 0.0));
        let colour = out.shapes.iter().find_map(|c| match &c.shape {
            egui::Shape::Text(t) if t.galley.text().starts_with("P&L") => {
                Some(t.galley.job.sections[0].format.color)
            }
            _ => None,
        });
        out.drop_without_applying_deltas();
        colour.expect("the hero paints a P&L chip")
    }

    #[test]
    fn the_pnl_chip_is_the_chosen_market_sets_text_colour() {
        use vike_ui_theme::appearance::Appearance;
        use vike_ui_theme::market::{MarketColors, MarketId};
        for m in MarketId::ALL {
            let c = MarketColors::of(m);
            let a = Appearance { market: m, ..Appearance::default() };
            assert_eq!(pnl_colour(a, 12.5), c.up_text, "{m:?} profit");
            assert_eq!(pnl_colour(a, -12.5), c.down_text, "{m:?} loss");
        }
    }

    /// Review Focus 5: the default appearance paints exactly what the Trade window painted before.
    #[test]
    fn the_default_trade_colours_are_todays() {
        use vike_ui_theme::appearance::Appearance;
        assert_eq!(pnl_colour(Appearance::default(), 12.5), palette::UP);
        assert_eq!(pnl_colour(Appearance::default(), -12.5), palette::DOWN);
    }

    // -------------------------------------------------------------------------------------
    // The half-filled TP/SL ticket (the controller's ruling in the node-bracket Task 3 review)
    // -------------------------------------------------------------------------------------

    /// ⚠ **A ticket that asked for TP/SL with a leg missing is REFUSED, never sent as a plain
    /// order.** The ticket reads each box to `Some` only when it parses to a price above zero, so
    /// with the toggle ON a blank, unparseable or non-positive leg used to reach the planner as
    /// exactly one `Some`, and `plan_trade`'s bracket arm needs both: the ticket fell through to a
    /// PLAIN order that went live with no protection and no message. Each case asserts both
    /// refusals: the ticket does not arm, and the planner, handed the same legs, plans NOTHING.
    fn assert_a_half_ticket_is_refused(tp: &str, sl: &str) {
        let legs = tpsl_legs(true, tp, sl);
        assert!(!tpsl_complete(true, legs), "the ticket must not arm: tp {tp:?}, sl {sl:?}");
        let (tp_leg, sl_leg) = legs;
        let plan = crate::orders::order_dispatch::plan_dispatch(
            crate::orders::order_dispatch::DispatchInputs {
                trade_orders: vec![tools::TradeSubmit {
                    side: 1,
                    qty: 1.0,
                    reduce_only: false,
                    kind: order_entry::OrderKind::Limit,
                    price: Some(100.0),
                    trigger_price: None,
                    tp: tp_leg,
                    sl: sl_leg,
                }],
                ..Default::default()
            },
            &order_entry::OrderLimits::default(),
            &CoreSnapshot::empty("binance", "BTCUSDT"),
            0,
            0,
        );
        assert!(plan.commands.is_empty(), "no order of any kind: {:?}", plan.commands);
        assert_eq!(
            plan.rejects.iter().map(|r| r.reason).collect::<Vec<_>>(),
            vec![crate::orders::order_dispatch::DispatchRejectReason::TpSlNeedsBothLegs],
            "tp {tp:?}, sl {sl:?}"
        );
    }

    #[test]
    fn a_tpsl_ticket_with_a_blank_take_profit_is_refused() {
        assert_a_half_ticket_is_refused("", "90");
    }

    #[test]
    fn a_tpsl_ticket_with_a_blank_stop_loss_is_refused() {
        assert_a_half_ticket_is_refused("120", "  ");
    }

    #[test]
    fn a_tpsl_ticket_with_an_unparseable_stop_loss_is_refused() {
        assert_a_half_ticket_is_refused("120", "ninety");
    }

    #[test]
    fn a_tpsl_ticket_with_a_stop_loss_at_or_below_zero_is_refused() {
        assert_a_half_ticket_is_refused("120", "0");
        assert_a_half_ticket_is_refused("120", "-5");
    }

    /// ⚠ **Both legs missing is the case only the TICKET can refuse.** Its legs are `(None, None)`,
    /// which the planner cannot tell from the toggle being OFF, so the ticket's own gate is the
    /// refusal. The controls: the toggle off arms with no exits, and two good legs arm a bracket.
    #[test]
    fn a_tpsl_ticket_with_neither_leg_does_not_arm_and_the_controls_do() {
        assert_eq!(tpsl_legs(true, "", "x"), (None, None));
        assert!(!tpsl_complete(true, (None, None)), "toggle on, no legs: not armed");
        assert_eq!(tpsl_legs(false, "120", "90"), (None, None), "toggle off reads no exits");
        assert!(tpsl_complete(false, (None, None)), "toggle off: armed, a plain order");
        let both = tpsl_legs(true, " 120 ", "90");
        assert_eq!(both, (Some(120.0), Some(90.0)));
        assert!(tpsl_complete(true, both), "two good legs: armed, a bracket");
    }

    // -------------------------------------------------------------------------------------
    // The ticket's ARM verdict (`ticket_armed`) — the composition BUY/SELL are enabled by
    // -------------------------------------------------------------------------------------

    /// The ticket's verdict for a 1.0 order of `kind` with a limit price and a trigger present,
    /// the TP/SL toggle at `on` and its boxes reading `tp`/`sl`, no core fault — read through the
    /// ticket's own `tpsl_legs`, exactly as the frame reads it.
    fn verdict(
        kind: order_entry::OrderKind,
        on: bool,
        tp: &str,
        sl: &str,
    ) -> (bool, Option<crate::orders::order_dispatch::DispatchRejectReason>) {
        ticket_armed(1.0, kind, Some(100.0), Some(105.0), on, tpsl_legs(on, tp, sl), false)
    }

    /// ⚠ **The toggle on with a leg missing never arms — including BOTH legs blank, which only this
    /// verdict can refuse** (the planner reads `(None, None)` as the toggle off and would send a
    /// PLAIN order). Each case is disarmed AND carries the line the ticket shows under the buttons.
    #[test]
    fn a_tpsl_ticket_missing_a_leg_does_not_arm_and_says_why() {
        use crate::orders::order_dispatch::DispatchRejectReason::TpSlNeedsBothLegs;
        for kind in [order_entry::OrderKind::Market, order_entry::OrderKind::Limit] {
            for (tp, sl) in [("", ""), ("", "90"), ("120", ""), ("120", "x"), ("0", "90")] {
                assert_eq!(
                    verdict(kind, true, tp, sl),
                    (false, Some(TpSlNeedsBothLegs)),
                    "{kind:?} tp {tp:?} sl {sl:?}"
                );
            }
        }
    }

    /// ⚠ **A Stop entry with TP/SL never arms, and the ticket says why.** A bracket has no stop
    /// entry; the planner refuses one (`BracketNeedsMarketOrLimit`), but only into the LOG, so an
    /// ARMED ticket turned a click into an order that silently went nowhere. The Stop reason wins
    /// whatever the legs read, because filling them in cannot fix it.
    #[test]
    fn a_stop_entry_with_tpsl_does_not_arm_and_says_why() {
        use crate::orders::order_dispatch::DispatchRejectReason::BracketNeedsMarketOrLimit;
        for (tp, sl) in [("120", "90"), ("", ""), ("120", "")] {
            assert_eq!(
                verdict(order_entry::OrderKind::Stop, true, tp, sl),
                (false, Some(BracketNeedsMarketOrLimit)),
                "tp {tp:?} sl {sl:?}"
            );
        }
        // ...and a Stop with the toggle OFF is a plain stop order, armed.
        assert_eq!(verdict(order_entry::OrderKind::Stop, false, "120", "90"), (true, None));
    }

    /// The controls: a plain order (the toggle off, whatever its boxes still hold) and a bracket
    /// with two good legs on a Market or Limit entry arm with nothing to say; and the verdict's
    /// other inputs still disarm without a TP/SL line — no size, a Limit with no price, a Stop with
    /// no trigger, a core fault.
    #[test]
    fn a_plain_order_and_a_two_legged_bracket_arm() {
        use order_entry::OrderKind::{Limit, Market, Stop};
        for kind in [Market, Limit, Stop] {
            assert_eq!(verdict(kind, false, "", ""), (true, None), "{kind:?}: a plain order");
        }
        for kind in [Market, Limit] {
            assert_eq!(verdict(kind, true, "120", "90"), (true, None), "{kind:?}: a bracket");
        }
        let both = (Some(120.0), Some(90.0));
        assert_eq!(
            ticket_armed(0.0, Market, None, None, true, both, false),
            (false, None),
            "no size"
        );
        assert_eq!(
            ticket_armed(1.0, Limit, None, None, true, both, false),
            (false, None),
            "no price"
        );
        assert_eq!(ticket_armed(1.0, Stop, None, None, false, (None, None), false), (false, None));
        assert_eq!(ticket_armed(1.0, Market, None, None, true, both, true), (false, None), "fault");
    }
}
