//! The Account window (`docs/superpowers/specs/2026-09-30-trade-window-design.md` §3.10): the old
//! Trade window without its ticket. One column, top to bottom: the cross-venue equity hero, the
//! per-venue Accounts table and the working-orders table, whose ✕ leaves through
//! `ToolView::account_cancel`. Order ENTRY lives in the Trade window now (`trade.rs` here, the
//! widget in `vike_panels::trade`).
//!
//! History: these sections moved here verbatim from `vike-app`'s CI-excluded `main.rs` (tool-view
//! extraction batch 3, audit F1), and the decision helpers the render code used to inline
//! ([`hero_totals`], [`unpriced_tooltip`]) became named, pure functions with unit tests, the POINT
//! of that move. The read-only `snap` arrives through [`ToolCtx`]; `&mut ToolView` stays a separate
//! param so the body keeps writing its OUT intent.
//!
//! ⚠ Up and down are the chosen market-colour set since the design system's PR 4 — text in the
//! set's text colours, fills in its graphic colours.

use super::ToolCtx;
use crate::{orders::equity_panel, tools};
use vike_ui_theme::components::{ON_FILL, Status, Tokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::Pair;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::account;

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

/// The Accounts row's unpriced-badge hover text: the comma-joined symbols of `venue`'s positions
/// that carry no `mark_source`. Derived here because `EquityRow` carries only the COUNT (the T4
/// model is display-thin). Falls back to a generic label when the venue is absent from the
/// snapshot or every position is priced.
pub fn unpriced_tooltip(snap: &vike_exec::CoreSnapshot, venue: &str) -> String {
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

/// Right-aligned fixed-width monospace numeric cell — the aligned-table idiom (mirrors the
/// Calendar table's `allocate_ui_with_layout` columns). Used by the Account window's venue +
/// orders tables so numbers line up on the decimal.
fn num_cell(ui: &mut egui::Ui, w: f32, txt: String, col: egui::Color32) {
    use egui::{Align, Label, Layout, RichText, vec2};
    let cell = vec2(w, account::CELL_H);
    ui.allocate_ui_with_layout(cell, Layout::right_to_left(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.add(
            Label::new(
                RichText::new(txt).monospace().size(role_px(ui.ctx(), TextRole::Strong)).color(col),
            )
            .truncate(),
        );
    });
}

/// Left-aligned fixed-width text cell (venue-name / side columns of the Account tables).
fn text_cell(ui: &mut egui::Ui, w: f32, rt: egui::RichText) {
    use egui::{Align, Label, Layout, vec2};
    let cell = vec2(w, account::CELL_H);
    ui.allocate_ui_with_layout(cell, Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.add(Label::new(rt).truncate());
    });
}

/// The cross-venue equity HERO (full-width, top of the Account window): big `equity_total`, a P&L
/// chip (summed venue realized), the unpriced badge, and a Realized/Fees/Funding stat strip.
/// Replaces the old four account cards, which duplicated the primary venue's table row.
fn equity_hero(ui: &mut egui::Ui, model: &equity_panel::EquityPanelModel, margin_used_total: f64) {
    use egui::{Color32, RichText};

    let t = Tokens::of(ui.ctx());
    let HeroTotals { realized, fees, funding } = hero_totals(&model.rows);

    ui.label(
        RichText::new("CROSS-VENUE EQUITY")
            .size(role_px(ui.ctx(), TextRole::Caption))
            .color(t.theme.text2)
            .strong(),
    );
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{:.2}", model.equity_total))
                .size(role_px(ui.ctx(), TextRole::Display))
                .color(t.theme.text),
        );
        let (pc, ptxt) = if realized >= 0.0 {
            (Pair::GainLoss.text(true, &t), format!("P&L +{realized:.2}"))
        } else {
            (Pair::GainLoss.text(false, &t), format!("P&L {realized:.2}"))
        };
        ui.label(RichText::new(ptxt).size(role_px(ui.ctx(), TextRole::Body)).color(pc).monospace());
        if model.missing_prices_total > 0 {
            ui.label(
                icons::WARNING.before(
                    ui.style(),
                    RichText::new(format!("{} unpriced", model.missing_prices_total))
                        .size(role_px(ui.ctx(), TextRole::Body))
                        .color(ON_FILL)
                        .background_color(Status::Warning.color()),
                ),
            );
        }
    });

    // stat strip: cross-venue Realized · Fees · Funding aggregates
    ui.add_space(space::LG);
    let stat = |ui: &mut egui::Ui, k: &str, v: String, col: Color32| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new(k).size(role_px(ui.ctx(), TextRole::Caption)).color(t.theme.text2),
            );
            ui.label(
                RichText::new(v).size(role_px(ui.ctx(), TextRole::Title)).color(col).monospace(),
            );
        });
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XL3;
        stat(ui, "Realized", format!("{realized:+.4}"), Pair::GainLoss.text(realized >= 0.0, &t));
        stat(ui, "Fees", format!("{fees:.4}"), t.theme.text2);
        stat(ui, "Funding", format!("{funding:+.4}"), Pair::GainLoss.text(funding >= 0.0, &t));
        if margin_used_total > 0.0 {
            stat(ui, "Margin used", format!("{margin_used_total:.2}"), t.theme.text2);
        }
    });
}

/// The Accounts section: an aligned per-venue table (Venue · mode · Equity · uPnL · Fees ·
/// Funding) with right-aligned tabular numbers, the per-venue unpriced badge (+ hover), and a
/// per-asset balances line from `balances_by_asset`.
fn accounts_section(
    ui: &mut egui::Ui,
    snap: &vike_exec::CoreSnapshot,
    model: &equity_panel::EquityPanelModel,
) {
    use egui::{Align, Label, Layout, RichText, vec2};

    let t = Tokens::of(ui.ctx());
    ui.label(
        RichText::new("Accounts")
            .size(role_px(ui.ctx(), TextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    ui.add_space(space::SM);
    // header row
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::MD;
        text_cell(
            ui,
            account::WV,
            RichText::new("Venue").size(role_px(ui.ctx(), TextRole::Caption)).color(t.theme.text3),
        );
        text_cell(
            ui,
            account::WM,
            RichText::new("").size(role_px(ui.ctx(), TextRole::Caption)).color(t.theme.text3),
        );
        for h in ["Equity", "uPnL", "Fees", "Funding"] {
            ui.allocate_ui_with_layout(
                vec2(account::WN, account::HEAD_CELL_H),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.set_min_width(account::WN);
                    ui.add(
                        Label::new(
                            RichText::new(h)
                                .size(role_px(ui.ctx(), TextRole::Caption))
                                .color(t.theme.text3),
                        )
                        .truncate(),
                    );
                },
            );
        }
    });
    ui.painter().hline(
        ui.min_rect().x_range(),
        ui.cursor().top(),
        egui::Stroke::new(stroke::HAIRLINE, t.theme.border),
    );
    ui.add_space(space::XS);

    if model.rows.is_empty() {
        ui.label(RichText::new("no venues").color(t.theme.text2));
    }
    for row in &model.rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::MD;
            text_cell(
                ui,
                account::WV,
                RichText::new(&row.venue)
                    .size(role_px(ui.ctx(), TextRole::Strong))
                    .color(t.theme.text)
                    .strong(),
            );
            text_cell(
                ui,
                account::WM,
                RichText::new(&row.mode)
                    .size(role_px(ui.ctx(), TextRole::Caption))
                    .color(t.theme.text3),
            );
            num_cell(ui, account::WN, format!("{:.2}", row.equity), t.theme.text);
            num_cell(
                ui,
                account::WN,
                format!("{:+.4}", row.unrealized),
                Pair::GainLoss.text(row.unrealized >= 0.0, &t),
            );
            num_cell(ui, account::WN, format!("{:.4}", row.fees), t.theme.text2);
            num_cell(
                ui,
                account::WN,
                format!("{:+.4}", row.funding),
                Pair::GainLoss.text(row.funding >= 0.0, &t),
            );
            if row.missing_prices > 0 {
                ui.label(
                    icons::WARNING.before(
                        ui.style(),
                        RichText::new(row.missing_prices.to_string())
                            .size(role_px(ui.ctx(), TextRole::Body))
                            .color(ON_FILL)
                            .background_color(Status::Warning.color()),
                    ),
                )
                .on_hover_text(unpriced_tooltip(snap, &row.venue));
            }
        });
    }

    if !snap.portfolio.balances_by_asset.is_empty() {
        ui.add_space(space::MD);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::XL;
            for (asset, qty) in &snap.portfolio.balances_by_asset {
                ui.label(
                    RichText::new(format!("{asset} {qty:.4}"))
                        .size(role_px(ui.ctx(), TextRole::Body))
                        .color(t.theme.text2)
                        .monospace(),
                );
            }
        });
    }
}

/// The order row's DELAY badge (decision 0140): `DELAYED` / `FROZEN` when the node recorded that the
/// order was priced off data that was not real-time, `None` for a real-time or unlabelled order (no
/// badge: the ordinary case says nothing). Information, never a gate: the row and its ✕ are the
/// same either way. The trade window's status line reads the same words.
pub(crate) fn delay_badge(delay: Option<vike_model::DataDelay>) -> Option<&'static str> {
    match delay? {
        vike_model::DataDelay::RealTime => None,
        vike_model::DataDelay::Delayed { .. } => Some("DELAYED"),
        vike_model::DataDelay::Frozen => Some("FROZEN"),
    }
}

/// The working-orders section: an aligned table (Side · Qty · Type · Status · delay badge · ✕)
/// with a colored side chip and cancel on non-terminal orders.
fn orders_section(ui: &mut egui::Ui, snap: &vike_exec::CoreSnapshot, tv: &mut tools::ToolView) {
    use egui::RichText;

    let t = Tokens::of(ui.ctx());
    ui.label(
        RichText::new(format!("Orders ({})", snap.orders.len()))
            .size(role_px(ui.ctx(), TextRole::Strong))
            .color(t.theme.text)
            .strong(),
    );
    ui.add_space(space::SM);
    if snap.orders.is_empty() {
        ui.label(
            RichText::new("No working orders.")
                .size(role_px(ui.ctx(), TextRole::Strong))
                .color(t.theme.text3),
        );
        return;
    }
    let list_h = account::ORDERS_MAX_H;
    egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
        for o in &snap.orders {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::LG;
                let sc = Pair::BuySell.text(o.side > 0, &t);
                text_cell(
                    ui,
                    account::ORDER_W_SIDE,
                    RichText::new(if o.side > 0 { "BUY" } else { "SELL" })
                        .size(role_px(ui.ctx(), TextRole::Body))
                        .color(sc)
                        .strong(),
                );
                num_cell(ui, account::ORDER_W_QTY, format!("{:.4}", o.qty), t.theme.text);
                text_cell(
                    ui,
                    account::ORDER_W_TYPE,
                    RichText::new(o.order_type.as_str())
                        .color(t.theme.text2)
                        .size(role_px(ui.ctx(), TextRole::Body)),
                );
                text_cell(
                    ui,
                    account::ORDER_W_STATUS,
                    RichText::new(o.status.as_str())
                        .color(t.theme.text2)
                        .size(role_px(ui.ctx(), TextRole::Body)),
                );
                // Priced off delayed or frozen data (decision 0140): shown, never a gate.
                if let Some(word) = delay_badge(o.data_delay) {
                    vike_ui_theme::components::chip::badge(ui, word, Status::Warning);
                }
                // A working order keeps its ✕: `is_live` is false for exactly the five terminals and
                // LIQUIDATED, the set this table has always treated as done.
                if o.status.is_live()
                    && icons::named(ui.small_button(icons::CANCEL), "Cancel order").clicked()
                {
                    tv.account_cancel = Some(o.client_order_id.clone());
                }
            });
        }
    });
}

/// The Account window: cross-venue equity, then per-venue accounts, then working orders, in one
/// column. Reads the core's lossy `CoreSnapshot` and writes a working order's ✕ into
/// `tv.account_cancel`, which the shell hands to the dispatcher.
pub fn account_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    let snap = ctx.snap;
    // cap the composed content width so a maximized window doesn't stretch the tables across the
    // whole screen; a normal-sized Account window (< cap) is unaffected.
    ui.set_max_width(ui.available_width().min(account::MAX_W));

    let model = equity_panel::EquityPanelModel::from_snapshot(snap);

    equity_hero(ui, &model, snap.portfolio.margin_used_total);
    ui.add_space(space::XL);
    accounts_section(ui, snap, &model);
    ui.add_space(space::XL2);
    orders_section(ui, snap, tv);
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_exec::{BalanceMode, CoreSnapshot, PositionView, PriceSource, VenueBlock};
    use vike_model::TradingState;

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
            venue: "binance".into(),
            symbol: symbol.into(),
            position_side: vike_model::events::PositionSide::Both,
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
            extra_symbols: Default::default(),
            tier: None,
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: BalanceMode::Delta,
            contract_values: Default::default(),
            contract_value_default: vike_model::ContractValue::default(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: Default::default(),
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
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
        let out = ctx.run_ui(egui::RawInput::default(), |ui| equity_hero(ui, &model, 0.0));
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

    fn order_view(
        coid: &str,
        side: i32,
        delay: Option<vike_model::DataDelay>,
    ) -> vike_exec::OrderView {
        vike_exec::OrderView {
            client_order_id: coid.into(),
            venue: "ibkr".into(),
            account: None,
            symbol: "AAPL.SMART.USD".into(),
            side,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(190.0),
            trigger_price: None,
            status: vike_model::OrderStatus::Accepted,
            venue_order_id: None,
            filled_qty: 0.0,
            avg_fill_px: 0.0,
            reason: None,
            data_delay: delay,
        }
    }

    #[test]
    fn only_a_delayed_or_frozen_order_gets_a_badge() {
        use vike_model::DataDelay;
        assert_eq!(delay_badge(None), None);
        assert_eq!(delay_badge(Some(DataDelay::RealTime)), None);
        assert_eq!(delay_badge(Some(DataDelay::DELAYED)), Some("DELAYED"));
        let stated = DataDelay::Delayed { lag_ms: Some(900_000) };
        assert_eq!(delay_badge(Some(stated)), Some("DELAYED"));
        assert_eq!(delay_badge(Some(DataDelay::Frozen)), Some("FROZEN"));
    }

    /// The order-row LAYOUT (decision 0140), read off a real frame: the delayed order's row carries
    /// ONE `DELAYED` badge, on its own row (nearer that row than the other), right of its status;
    /// the real-time order's row carries none.
    #[test]
    fn the_delay_badge_sits_on_the_delayed_orders_row_right_of_its_status() {
        let ctx = egui::Context::default();
        vike_ui_theme::appearance::install(&ctx, &vike_ui_theme::appearance::Appearance::default());
        let mut snap = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
        snap.orders = vec![
            order_view("d", 1, Some(vike_model::DataDelay::DELAYED)),
            order_view("r", -1, Some(vike_model::DataDelay::RealTime)),
        ];
        let mut tv = tools::ToolView::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| orders_section(ui, &snap, &mut tv));
        let texts: Vec<(String, egui::Rect)> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => {
                    Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2())))
                }
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        let find = |w: &str| -> Vec<egui::Rect> {
            texts.iter().filter(|(s, _)| s == w).map(|(_, r)| *r).collect()
        };
        let badges = find("DELAYED");
        assert_eq!(badges.len(), 1, "one badge, for the one delayed order: {texts:?}");
        let (buy, sell) = (find("BUY")[0], find("SELL")[0]);
        assert!(buy.center().y < sell.center().y, "the delayed order is the first row");
        let dy = |a: egui::Rect, b: egui::Rect| (a.center().y - b.center().y).abs();
        let statuses = find("ACCEPTED");
        assert_eq!(statuses.len(), 2, "{texts:?}");
        let buy_status = statuses
            .iter()
            .copied()
            .min_by(|a, b| dy(buy, *a).total_cmp(&dy(buy, *b)))
            .expect("the delayed row's status");
        assert!(dy(buy, badges[0]) < dy(sell, badges[0]), "the badge is on the delayed row");
        assert!(badges[0].left() >= buy_status.right(), "right of the status");
    }

    /// Review Focus 5: the default appearance paints exactly what the old Trade window (this
    /// window, before it lost its ticket) painted before.
    #[test]
    fn the_default_trade_colours_are_todays() {
        use vike_ui_theme::appearance::Appearance;
        use vike_ui_theme::market::{MarketColors, MarketId};
        let classic = MarketColors::of(MarketId::Classic);
        assert_eq!(pnl_colour(Appearance::default(), 12.5), classic.up_text);
        assert_eq!(pnl_colour(Appearance::default(), -12.5), classic.down_text);
    }
}
