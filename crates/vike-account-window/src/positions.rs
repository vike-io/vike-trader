//! The Account window's Positions tab: every account's open positions with their unrealized P/L,
//! VIEW ONLY (the owner's ruling, top layers wave 3: "только просмотр").
//!
//! ⚠ Read from `CoreSnapshot::portfolio.venues[*].positions`, one block per ACCOUNT, never from
//! `CoreSnapshot::positions`, which holds the primary engine's alone (the wire's mirror of
//! `venues[0].positions`). A row names its account as the Accounts tab does (`main` for the
//! unlabelled one), so two accounts of one venue never read as one.
//!
//! **Nothing here sends.** `positions_section` takes the rows by shared reference and no OUT slot,
//! and this file names none of the paths an order or its withdrawal leaves by;
//! `crates/vike-account-window/tests/positions_view_only.rs` holds both.

use vike_execution::CoreSnapshot;
use vike_model::AccountTier;
use vike_model::events::PositionSide;
use vike_ui_theme::components::venue_chip::chip_pad;
use vike_ui_theme::components::{ThemeTokens, role_px};
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::account;
use vike_utility::number_text::format_number;

use crate::accounts::{TIER_UNKNOWN, tier_mark};
use crate::{number_cell, text_cell, text_w};

/// One open position of one account: a row of the Positions tab.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountWindowPositionsRow {
    /// The account's venue (its block's).
    pub venue: String,
    /// WHICH account of `venue`: its label, or `main` for the unlabelled one (the Accounts row's word).
    pub account: String,
    /// The account's engine key on the node (`VenueBlock::route_key`).
    pub route_key: String,
    /// The account's tier as the node said it; `None` is not said, drawn `MODE ?`, never PAPER.
    pub tier: Option<AccountTier>,
    pub symbol: String,
    /// `Long` or `Short`: a one-way (`Both`) leg is read off its signed size.
    pub side: PositionSide,
    /// The size, unsigned (the side says which way).
    pub size: f64,
    pub average_price: f64,
    pub unrealized: f64,
}

/// What an empty Positions tab says.
pub const NO_POSITIONS: &str = "No open positions.";

/// Every account's open positions, blocks in snapshot order and positions in their engine's order. A
/// FLAT row (size 0, which an engine keeps after a close) is no open position and is left out. Pure.
pub fn positions_rows(snapshot: &CoreSnapshot) -> Vec<AccountWindowPositionsRow> {
    snapshot
        .portfolio
        .venues
        .iter()
        .flat_map(|block| {
            block.positions.iter().filter(|p| p.size != 0.0).map(move |p| {
                AccountWindowPositionsRow {
                    venue: block.venue.clone(),
                    account: crate::equity::account_word(block).to_string(),
                    route_key: block.route_key.clone(),
                    tier: block.tier,
                    symbol: p.symbol.as_str().to_string(),
                    side: match p.position_side {
                        PositionSide::Both if p.size > 0.0 => PositionSide::Long,
                        PositionSide::Both => PositionSide::Short,
                        side => side,
                    },
                    size: p.size.abs(),
                    average_price: p.average_price,
                    unrealized: p.unrealized,
                }
            })
        })
        .collect()
}

/// The Positions table's word columns, left to right: the three words that say WHICH account (venue,
/// label, tier chip), the symbol and the side.
const WORD_HEADS: [&str; 5] = ["Venue", "Account", "Tier", "Symbol", "Side"];

/// The number columns after them, right-aligned.
const NUMBER_HEADS: [&str; 3] = ["Size", "Avg price", "uPnL"];

/// The Positions tab: one row per open position, each column as wide as the widest thing it holds,
/// with no total row (a total would add currencies and tiers together, decision 0209). Draws
/// [`NO_POSITIONS`] for none.
pub(crate) fn positions_section(ui: &mut egui::Ui, rows: &[AccountWindowPositionsRow]) {
    use egui::{Align, Label, Layout, RichText, vec2};

    let t = ThemeTokens::of(ui.ctx());
    let (strong, caption) =
        (role_px(ui.ctx(), ThemeTextRole::Strong), role_px(ui.ctx(), ThemeTextRole::Caption));
    if rows.is_empty() {
        ui.label(RichText::new(NO_POSITIONS).size(strong).color(t.theme.text_tertiary));
        return;
    }
    let head_text = |h: &str| RichText::new(h).size(caption).color(t.theme.text_tertiary);
    let words = |r: &AccountWindowPositionsRow| {
        let long = r.side == PositionSide::Long;
        [
            RichText::new(&r.venue).size(strong).color(t.theme.text).strong(),
            RichText::new(&r.account).size(strong).color(t.theme.text_secondary),
            RichText::new(&r.symbol).size(strong).color(t.theme.text),
            RichText::new(r.side.to_string())
                .size(strong)
                .color(ThemeSidePair::LongShort.text(long, &t))
                .strong(),
        ]
    };
    let numbers = |r: &AccountWindowPositionsRow| {
        [
            (format_number(r.size), t.theme.text),
            (format_number(r.average_price), t.theme.text_secondary),
            (
                format!("{:+.4}", r.unrealized),
                ThemeSidePair::GainLoss.text(r.unrealized >= 0.0, &t),
            ),
        ]
    };
    // Each column's least width: the widest thing it holds, its header included.
    let mut w = WORD_HEADS.map(|h| text_w(ui, head_text(h)));
    let mut nw = NUMBER_HEADS.map(|h| text_w(ui, head_text(h)));
    w[2] = w[2].max(
        text_w(ui, RichText::new(TIER_UNKNOWN).monospace().size(caption)) + 4.0 * chip_pad(&t),
    );
    for r in rows {
        for (at, rt) in [0, 1, 3, 4].into_iter().zip(words(r)) {
            w[at] = w[at].max(text_w(ui, rt));
        }
        for (at, (txt, _)) in nw.iter_mut().zip(numbers(r)) {
            *at = at.max(text_w(ui, RichText::new(txt).monospace().size(strong)));
        }
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::MD;
        for (h, cw) in WORD_HEADS.iter().zip(w) {
            text_cell(ui, cw, head_text(h));
        }
        for (h, cw) in NUMBER_HEADS.iter().zip(nw) {
            ui.allocate_ui_with_layout(
                vec2(cw, account::HEADER_CELL_HEIGHT),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.set_min_width(cw);
                    ui.add(Label::new(head_text(h)).truncate());
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
    for r in rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::MD;
            let [venue, label, symbol, side] = words(r);
            text_cell(ui, w[0], venue);
            text_cell(ui, w[1], label);
            ui.allocate_ui_with_layout(
                vec2(w[2], account::CELL_HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| {
                    // Told its width, so a chip narrower than the column does not slide what follows.
                    ui.set_min_width(w[2]);
                    tier_mark(ui, r.tier);
                },
            );
            text_cell(ui, w[3], symbol);
            text_cell(ui, w[4], side);
            for ((txt, col), cw) in numbers(r).into_iter().zip(&nw) {
                number_cell(ui, *cw, txt, col);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_execution::{BalanceMode, PositionView, VenueBlock};
    use vike_model::TradingState;
    use vike_model::accounts::account_keys::{parse_wire_account, route_key_of};

    fn position(symbol: &str, side: PositionSide, size: f64, unrealized: f64) -> PositionView {
        PositionView {
            venue: "bybit".into(),
            symbol: symbol.into(),
            position_side: side,
            size,
            average_price: 100.0,
            unrealized,
            mark_source: None,
            leverage: 0.0,
            liquidation_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }
    }

    fn block(
        venue: &str,
        label: Option<&str>,
        tier: Option<AccountTier>,
        positions: Vec<PositionView>,
    ) -> VenueBlock {
        let account = label.map(|l| parse_wire_account(l).expect("a test label parses"));
        VenueBlock {
            account_code: None,
            order_client_id_shape: None,
            venue: venue.to_string(),
            route_key: account
                .as_ref()
                .map_or_else(|| venue.to_string(), |l| route_key_of(venue, l)),
            account,
            symbol: String::new(),
            extra_symbols: Default::default(),
            tier,
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
            ledger_currency: Default::default(),
            currency_assumed: false,
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_buying_power: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            positions,
        }
    }

    /// **Every account's positions, and only from its own block**: two accounts of one venue stay two
    /// accounts (their labels and tiers), a tier the node did not say stays `None`, a flat row is left
    /// out, a one-way leg reads its side off its sign, and `CoreSnapshot::positions` (the primary
    /// engine's mirror) is never read.
    #[test]
    fn rows_come_from_every_block_and_keep_two_accounts_of_one_venue_apart() {
        let mut snapshot = CoreSnapshot::empty("bybit", "BTCUSDT");
        snapshot.portfolio.venues = vec![
            block(
                "bybit",
                None,
                Some(AccountTier::Demo),
                vec![
                    position("BTCUSDT", PositionSide::Both, 0.5, 12.5),
                    position("ETHUSDT", PositionSide::Both, 0.0, 0.0),
                    position("SOLUSDT", PositionSide::Both, -3.0, -1.25),
                ],
            ),
            block(
                "bybit",
                Some("LIVE"),
                Some(AccountTier::Live),
                vec![position("BTCUSDT", PositionSide::Short, 0.25, 2.0)],
            ),
            block("okx", None, None, vec![position("XRPUSDT", PositionSide::Long, 10.0, 0.5)]),
        ];
        // Planted where a reader of the wrong field would find it.
        snapshot.positions = vec![position("WRONGUSDT", PositionSide::Long, 1.0, 0.0)];
        let rows = positions_rows(&snapshot);
        let seen: Vec<_> = rows
            .iter()
            .map(|r| {
                (r.venue.as_str(), r.account.as_str(), r.tier, r.symbol.as_str(), r.side, r.size)
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                ("bybit", "main", Some(AccountTier::Demo), "BTCUSDT", PositionSide::Long, 0.5),
                ("bybit", "main", Some(AccountTier::Demo), "SOLUSDT", PositionSide::Short, 3.0),
                ("bybit", "LIVE", Some(AccountTier::Live), "BTCUSDT", PositionSide::Short, 0.25),
                ("okx", "main", None, "XRPUSDT", PositionSide::Long, 10.0),
            ]
        );
        assert_eq!(rows[2].route_key, "bybit#LIVE", "the row keeps its own account's key");
        assert_eq!(rows[1].unrealized, -1.25);
    }

    #[test]
    fn a_snapshot_with_no_open_position_has_no_row() {
        let mut snapshot = CoreSnapshot::empty("bybit", "BTCUSDT");
        snapshot.portfolio.venues = vec![block("bybit", None, None, Vec::new())];
        assert!(positions_rows(&snapshot).is_empty());
    }
}
