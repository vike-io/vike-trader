//! The ladder's columns (spec §3.4): Buy · Bid · Price · Ask · Sell and the Vol column, their captions
//! and the design's widths, where a click in each trades, and the width the widest price in view needs.

use super::fmt_price;
use super::rows::key_price;
use crate::trade_window::{TradeWindowTicketKind, TradeWindowTicketPlace};
use egui::{Painter, Rect, pos2};
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;

/// The columns (spec §3.4), left to right. `Vol` is the traded-volume column the toolbar's first
/// button adds; it is not there, and has no width, until it is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TradeWindowLadderColumn {
    Buy,
    Bid,
    Price,
    Ask,
    Sell,
    Volume,
}

pub(super) const COLS: [TradeWindowLadderColumn; 6] = [
    TradeWindowLadderColumn::Buy,
    TradeWindowLadderColumn::Bid,
    TradeWindowLadderColumn::Price,
    TradeWindowLadderColumn::Ask,
    TradeWindowLadderColumn::Sell,
    TradeWindowLadderColumn::Volume,
];
/// The column captions, in [`COLS`]' order.
pub(super) const CAPTIONS: [&str; 6] = ["Buy", "Bid", "Price", "Ask", "Sell", "Vol"];

/// The design's column widths, in its points, in [`COLS`]' order: beside the ticket with the Vol
/// column on, and with it off (Price takes its 30), then under it, where there is no Vol and Buy
/// and Sell are wider. The numbers are `trade::COL_*` rows.
const BESIDE_VOLUME: [f32; 6] = [
    trade::COLUMN_BUY_SELL_WIDTH,
    trade::COLUMN_BID_ASK_WIDTH,
    trade::COLUMN_PRICE_WIDTH,
    trade::COLUMN_BID_ASK_WIDTH,
    trade::COLUMN_BUY_SELL_WIDTH,
    trade::COLUMN_VOLUME_WIDTH,
];
const BESIDE_NO_VOLUME: [f32; 6] = [
    trade::COLUMN_BUY_SELL_WIDTH,
    trade::COLUMN_BID_ASK_WIDTH,
    trade::COLUMN_PRICE_WIDTH + trade::COLUMN_VOLUME_WIDTH,
    trade::COLUMN_BID_ASK_WIDTH,
    trade::COLUMN_BUY_SELL_WIDTH,
    0.0,
];
const UNDER: [f32; 6] = [
    trade::COLUMN_BUY_SELL_UNDER_WIDTH,
    trade::COLUMN_BID_ASK_WIDTH,
    trade::COLUMN_PRICE_WIDTH,
    trade::COLUMN_BID_ASK_WIDTH,
    trade::COLUMN_BUY_SELL_UNDER_WIDTH,
    0.0,
];

/// Where the columns are in a row: the design's widths ([`BESIDE_VOLUME`] and its siblings) scaled
/// to the width the ladder has, so every column keeps its share at any width.
///
/// The one thing that bends them is the Price column, which is floored at the width the widest
/// price in view needs: a price too wide for its column would run into the Bid and Ask sizes
/// beside it. What it takes it takes from the other columns in proportion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TradeWindowLadderColumns {
    /// Each column's left edge from the row's own, [`COLS`]' order, and the row's width last.
    pub(super) edge: [f32; 7],
}

impl TradeWindowLadderColumns {
    pub(super) fn new(
        panel: TradeWindowTicketPlace,
        volume: bool,
        width: f32,
        price_floor: f32,
    ) -> TradeWindowLadderColumns {
        let design = match (panel, volume) {
            (TradeWindowTicketPlace::Beside, true) => BESIDE_VOLUME,
            (TradeWindowTicketPlace::Beside, false) => BESIDE_NO_VOLUME,
            (TradeWindowTicketPlace::Under, _) => UNDER,
        };
        let total: f32 = design.iter().sum();
        let mut w = design.map(|d| d / total * width);
        let price = TradeWindowLadderColumn::Price as usize;
        if w[price] < price_floor && price_floor < width {
            let take = price_floor - w[price];
            let others = width - w[price];
            for (i, x) in w.iter_mut().enumerate() {
                if i != price {
                    *x -= take * *x / others;
                }
            }
            w[price] = price_floor;
        }
        let mut edge = [0.0; 7];
        let mut at = 0.0;
        for (i, width) in w.iter().enumerate() {
            at += width;
            edge[i + 1] = at;
        }
        TradeWindowLadderColumns { edge }
    }

    pub(super) fn width(&self) -> f32 {
        self.edge[6]
    }

    /// Whether `col` has any width: only Vol can be without.
    pub(super) fn shown(&self, col: TradeWindowLadderColumn) -> bool {
        let i = col as usize;
        self.edge[i + 1] > self.edge[i]
    }

    /// `col`'s rect within `row`.
    pub(super) fn rect(&self, col: TradeWindowLadderColumn, row: Rect) -> Rect {
        let i = col as usize;
        Rect::from_min_max(
            pos2(row.min.x + self.edge[i], row.min.y),
            pos2(row.min.x + self.edge[i + 1], row.max.y),
        )
    }

    /// The column a pointer `x` falls in, for a row whose left edge is `left`: a pointer past
    /// either edge (a drag that left the ladder) is the edge column, not none.
    pub(super) fn at(&self, x: f32, left: f32) -> TradeWindowLadderColumn {
        let rel = (x - left).clamp(0.0, (self.width() - 0.001).max(0.0));
        COLS.iter()
            .enumerate()
            .find(|(i, _)| self.shown(COLS[*i]) && rel < self.edge[i + 1])
            .map_or(TradeWindowLadderColumn::Sell, |(_, c)| *c)
    }
}

/// The side a click in `col` trades: Buy and Bid buy, Ask and Sell sell, Price and Vol are inert.
pub(crate) fn side_of(col: TradeWindowLadderColumn) -> Option<i32> {
    match col {
        TradeWindowLadderColumn::Buy | TradeWindowLadderColumn::Bid => Some(1),
        TradeWindowLadderColumn::Ask | TradeWindowLadderColumn::Sell => Some(-1),
        TradeWindowLadderColumn::Price | TradeWindowLadderColumn::Volume => None,
    }
}

/// Limit or stop for a click at `price`: a buy above the best ask, or a sell below the best bid,
/// would cross, so it is a stop; otherwise a limit.
pub(crate) fn click_type(
    side: i32,
    price: f64,
    best_bid: f64,
    best_ask: f64,
) -> TradeWindowTicketKind {
    let stop = if side > 0 { price > best_ask } else { price < best_bid };
    if stop { TradeWindowTicketKind::Stop } else { TradeWindowTicketKind::Limit }
}

/// The width the widest price in view needs in the Body role: the rows' top and bottom prices
/// (every one between them has no more digits than one of the two), so [`TradeWindowLadderColumns`] never gives the
/// Price column less than its text.
pub(super) fn price_floor(
    painter: &Painter,
    t: &ThemeTokens,
    tick: f64,
    group: i64,
    centre: f64,
    n_rows: usize,
) -> f32 {
    let reach = (n_rows / 2) as f64 * key_price(1, tick, group);
    let width = |p: f64| {
        painter
            .layout_no_wrap(fmt_price(p, Some(tick)), t.mono(ThemeTextRole::Body), t.theme.text)
            .size()
            .x
    };
    width(centre + reach).max(width((centre - reach).max(tick)))
}
