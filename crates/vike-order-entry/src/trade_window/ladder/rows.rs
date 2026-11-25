//! The ladder's rows: the price grid they are keyed on (the tick, the grouping, a price's row key and
//! the price a key names), the book and the tape summed into them, the size each prints, where the
//! rows are ([`TradeWindowLadderRowFrame`], the frame the tick chart paints on too), and the rows
//! themselves: the painting, then the click grammar and the scroll wheel.

use super::columns::{TradeWindowLadderColumn, TradeWindowLadderColumns, click_type, side_of};
use super::markers::{TradeWindowLadderMarkerState, marker_face, marker_lines, paint_marker};
use super::{
    GROUP_STEPS, NO_VOLUME, STALE_WHY, TradeWindowLadderDrawn, TradeWindowLadderMarker,
    TradeWindowLadderPress, book_centre, fmt_price, row_count,
};
use crate::trade_window::tick_chart::TradeWindowTickChartPrint;
use crate::trade_window::{
    TradeWindowAction, TradeWindowInputs, TradeWindowLadderOrder, TradeWindowState,
    TradeWindowStatusLineKind, TradeWindowTicketKind, TradeWindowTradable, sizing,
};
use egui::{
    Align2, Color32, CursorIcon, FontId, Galley, Painter, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, pos2, vec2,
};
use std::sync::Arc;
use vike_marketdata::L2BookLevel;
use vike_ui_theme::color::with_alpha;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::maps;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;
use vike_venues::VenueCapabilities;

/// The tick the rows are keyed on: the catalog's, else the book's own, else one.
///
/// ⚠ Never a tick of 0 and never a vanishing one. The DOM clamped its tick to `f64::MIN_POSITIVE`,
/// which only moves the division by zero: every price over that tick saturates its key at
/// `i64::MAX`, and the row arithmetic above the centre key then overflows. A catalog with no row
/// for the symbol (a grid of `None`s) falls back to the book's tick here; and a lot not known refuses
/// every size (`sizing::base_quantity`), so a click on such a ladder places nothing.
pub(crate) fn row_tick(grid_tick: Option<f64>, book_tick: f64) -> f64 {
    grid_tick.or(crate::trade_window::grid_value(book_tick)).unwrap_or(1.0)
}

// ---------------------------------------------------------------------------
// MOVED from the DOM (`vike_panels::dom`, deleted with this PR): these are the only copies.
// ---------------------------------------------------------------------------

/// Quantize a price to its grouped row key: the tick index divided (floor) by the group size.
/// A row key spans `group` ticks; `key_price` is its inverse (the row's aligned low edge).
pub(crate) fn row_key(price: f64, tick: f64, group: i64) -> i64 {
    let g = group.max(1);
    let ti = (price / tick).round() as i64;
    ti.div_euclid(g)
}

/// The price a row key stands for: its aligned low edge, `key · group` ticks read back by
/// [`vike_marketdata::TickFraction`], the rule the book reads its levels by (owner ruling
/// 2026-11-13), so a click sends the grid's own price: 35 ticks of 0.01 is 0.35, not the
/// 0.35000000000000003 of `35 × 0.01` ([`row_key`]'s inverse).
pub(crate) fn key_price(key: i64, tick: f64, group: i64) -> f64 {
    let g = group.max(1);
    vike_marketdata::TickFraction::new(tick).price_of(key as f64 * g as f64)
}

/// The price the row `key` NAMES, or `None` where it names none: its [`key_price`] where that is a
/// number above zero AND its tick count is one a float holds exactly. A row at zero or below (a
/// ladder scrolled under zero, a coarse grouping on a cheap instrument) names none, and neither
/// does a key the row arithmetic saturated (a tick so fine that every price's key overflowed:
/// `i64::MAX` times a 1e-300 tick is 9e-282, a price nobody sees or clicks). Such a row draws no
/// price and takes no order and no drop, as the ticket refuses a price that is not above zero
/// (final review A, minor 1).
fn named_price(key: i64, tick: f64, group: i64) -> Option<f64> {
    /// The largest tick count a float holds exactly.
    const EXACT: u64 = 1 << 53;
    key.checked_mul(group.max(1))
        .filter(|ticks| ticks.unsigned_abs() <= EXACT)
        .map(|_| key_price(key, tick, group))
        .filter(|p| *p > 0.0 && p.is_finite())
}

/// Aggregate raw `(price, qty)` levels into grouped rows keyed by [`row_key`], summing quantity.
/// Returns a map of `row_key → total_qty` — the render/hit-test view of one book side.
pub(super) fn group_book(
    levels: &[L2BookLevel],
    tick: f64,
    group: i64,
) -> std::collections::HashMap<i64, f64> {
    let mut m: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for &L2BookLevel { price, qty: quantity } in levels {
        *m.entry(row_key(price, tick, group)).or_insert(0.0) += quantity;
    }
    m
}

/// What [`TradeWindowInputs::tape`] holds, summed into grouped rows keyed by [`row_key`] as [`group_book`]
/// sums a side of the book: the size printed at each row over the tape's window. A print whose price
/// or size is not a number, or whose size is not above zero, is no trade and is left out: the column
/// never prints a `NaN` or a negative volume.
pub(super) fn group_tape(
    tape: &[TradeWindowTickChartPrint],
    tick: f64,
    group: i64,
) -> std::collections::HashMap<i64, f64> {
    let mut m: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for p in tape {
        if p.price.is_finite() && p.size.is_finite() && p.size > 0.0 {
            *m.entry(row_key(p.price, tick, group)).or_insert(0.0) += p.size;
        }
    }
    m
}

/// The next coarser grouping in [`GROUP_STEPS`]; the coarsest stays where it is.
pub(crate) fn next_group(g: i64) -> i64 {
    GROUP_STEPS.iter().copied().find(|&s| s > g).unwrap_or(g)
}

/// The next finer grouping in [`GROUP_STEPS`]; one tick per row stays where it is.
pub(crate) fn previous_group(g: i64) -> i64 {
    GROUP_STEPS.iter().rev().copied().find(|&s| s < g).unwrap_or(g)
}

/// The decimals a book size or a traded volume prints with: the lot's, as the ticket prints a size
/// (`1.262` on a lot of 0.001), or [`UNKNOWN_LOT_DECIMALS`] while the catalog has not said what the
/// lot is (`None`).
#[cfg(test)]
pub(super) fn lot_decimals(lot: Option<f64>) -> usize {
    lot.map_or(UNKNOWN_LOT_DECIMALS, sizing::decimals_of)
}

/// The decimals a size of a lot nobody knows prints with: the decimals the design prints with.
pub(crate) const UNKNOWN_LOT_DECIMALS: usize = 3;

/// A book size or a traded volume as the ladder prints it: to [`size_decimals`], plain digits
/// whatever its size (`1234.567`, never `1.23K`). The ONE formatter of the ladder's depth and
/// volume columns; an own order's size is [`sizing::quantity_text`]'s, which never prints an order as
/// `0` for want of a lot.
#[cfg(test)]
pub(super) fn fmt_size(q: f64, lot: Option<f64>) -> String {
    format!("{:.*}", size_decimals(q, lot), q)
}

/// The decimals the size `q` prints with: the lot's, and for a lot nobody knows (every row of
/// the datahub's catalog carries none) the design's three, and more only for a size three would print
/// as zero — `0.0002` on a spot book must never read `0.000`. At most [`MOST_DECIMALS`].
pub(crate) fn size_decimals(q: f64, lot: Option<f64>) -> usize {
    if let Some(lot) = lot {
        return sizing::decimals_of(lot);
    }
    let a = q.abs();
    if a == 0.0 || !a.is_finite() || a >= SMALLEST_AT_THREE {
        return UNKNOWN_LOT_DECIMALS;
    }
    // Two significant digits of a tiny size: `0.00020`, not `0.0002` and not `0.000`.
    let leading_zeros = (-a.log10()).ceil() as usize;
    (leading_zeros + 1).clamp(UNKNOWN_LOT_DECIMALS, MOST_DECIMALS)
}

/// The smallest size three decimals print with a significant digit to spare.
const SMALLEST_AT_THREE: f64 = 0.01;
/// The most decimals a size of an unknown lot prints with.
pub(crate) const MOST_DECIMALS: usize = 8;

/// `q` laid out to `decimals` and, where that is wider than `room`, to fewer: a size that does not
/// fit loses its LAST digits, never its first — `1234.567` becomes `1234.57`, never `34.567`. How far
/// a size may run past its column before it is printed to fewer decimals is `trade::FIT_SLACK`: the
/// width of a hairline of rounding in the font, not of a digit.
pub(super) fn size_galley(
    painter: &Painter,
    font: &FontId,
    ink: Color32,
    q: f64,
    decimals: usize,
    room: f32,
) -> Arc<Galley> {
    let lay = |d: usize| painter.layout_no_wrap(format!("{:.*}", d, q), font.clone(), ink);
    let mut at = decimals;
    let mut galley = lay(at);
    while galley.size().x > room + trade::FIT_SLACK && at > 0 {
        at -= 1;
        galley = lay(at);
    }
    galley
}

/// Whether the ladder may begin a drag-to-reprice on an own resting order for the given venue caps —
/// the modify-gate the widget consults at the drag-start, the drop, AND the marker affordance.
/// Extracted so the gate logic is unit-testable without an egui frame (audit br6).
#[inline]
pub(super) fn drag_to_reprice_allowed(capabilities: &VenueCapabilities) -> bool {
    capabilities.allows_modify()
}

/// What a size keeps clear of the side of its cell it is set against (Bid's right, Ask's left).
pub(super) const SIZE_PAD: f32 = space::SM;

/// Where the ladder's rows are, which is where the chart's must be ([`TradeWindowLadderRowFrame::of`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TradeWindowLadderRowFrame {
    /// The rows' rect: its vertical extent is the chart plot's.
    pub(crate) region: Rect,
    /// The key of the top row ([`row_key`]).
    pub(crate) top_key: i64,
    pub(crate) tick: f64,
    pub(crate) group: i64,
    pub(crate) row_h: f32,
    pub(crate) n_rows: usize,
}

impl TradeWindowLadderRowFrame {
    /// The frame of rows laid in `region` for this window: the same rows [`rows`] lays there, by
    /// the same arithmetic (the row height, the tick, the grouping, and the mid or the manual centre
    /// the top row is counted from). `None` where the book has no level, as the ladder then has no
    /// rows.
    #[must_use]
    pub(crate) fn of(
        region: Rect,
        t: &ThemeTokens,
        state: &TradeWindowState,
        inputs: &TradeWindowInputs<'_>,
    ) -> Option<TradeWindowLadderRowFrame> {
        if inputs.book.bid_levels() + inputs.book.ask_levels() == 0 {
            return None;
        }
        let tick = row_tick(inputs.grid.tick_size, inputs.book.tick_size);
        let row_h = t.metrics.row_height;
        let n_rows = (region.height() / row_h).floor().max(1.0) as usize;
        let best_bid = inputs.book.best_bid().map(|l| l.price);
        let best_ask = inputs.book.best_ask().map(|l| l.price);
        let last =
            inputs.last.or_else(|| inputs.book.mid()).or(best_bid).or(best_ask).unwrap_or(0.0);
        let centre = state
            .center
            .or(state.anchor)
            .unwrap_or_else(|| inputs.book.mid().or(best_bid).or(best_ask).unwrap_or(last));
        let top_key = row_key(centre, tick, state.group).saturating_add((n_rows / 2) as i64);
        Some(TradeWindowLadderRowFrame { region, top_key, tick, group: state.group, row_h, n_rows })
    }

    /// The row a price falls in, counted from the top, or `None` if it is above or below the rows.
    #[must_use]
    pub(crate) fn row_of(&self, price: f64) -> Option<usize> {
        if !price.is_finite() {
            return None;
        }
        let from_top = self.top_key.checked_sub(row_key(price, self.tick, self.group))?;
        usize::try_from(from_top).ok().filter(|i| *i < self.n_rows)
    }

    /// The vertical centre of the row a price falls in.
    #[must_use]
    pub(crate) fn y_of(&self, price: f64) -> Option<f32> {
        self.row_of(price).map(|i| self.region.min.y + (i as f32 + 0.5) * self.row_h)
    }

    /// The price the row `i` from the top is named by (its low edge).
    #[must_use]
    pub(crate) fn price_of(&self, i: usize) -> f64 {
        key_price(self.top_key - i as i64, self.tick, self.group)
    }
}

/// Where the rows are and what they are keyed on: their rect, the columns they are cut into, and the
/// tick their keys count.
pub(super) struct TradeWindowLadderRowSpace<'a> {
    pub(super) region: Rect,
    pub(super) cols: &'a TradeWindowLadderColumns,
    pub(super) tick: f64,
}

/// The rows and everything they take: the painting, then the click grammar and the scroll wheel.
pub(super) fn rows(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
    space: TradeWindowLadderRowSpace<'_>,
) {
    let TradeWindowLadderRowSpace { region, cols, tick } = space;
    let row_h = t.metrics.row_height;
    let best_bid = inputs.book.best_bid().map(|L2BookLevel { price: p, .. }| p);
    let best_ask = inputs.book.best_ask().map(|L2BookLevel { price: p, .. }| p);
    // `last` drives the outlined row (the window's price: the book's mid on the desktop, where no
    // last trade price arrives); the ladder CENTERS on [`book_centre`] so the real depth is always
    // in view even when a last-trade mark would sit a few ticks off the top of book.
    let last = inputs.last.or_else(|| inputs.book.mid()).or(best_bid).or(best_ask).unwrap_or(0.0);
    let book_center = book_centre(inputs);

    let n_rows = row_count(region.height(), row_h);
    let half = (n_rows / 2) as i64;
    let group = state.group;

    // The click region, sensed BEFORE the rows are laid out, because whether the button is down on
    // it decides where the rows are.
    let id = ui.scope_id().with("trade_ladder");
    let response = ui.interact(region, id, Sense::click_and_drag());

    // The top row's key: from the manual latch (scroll), else the live book's mid. The row
    // arithmetic saturates: a key is a tick index, and a price far off its tick must not overflow.
    //
    // ⚠ While the button is down on the ladder — from the press to the release that makes it a
    // click or ends a drag — the rows stay where they were DRAWN when it went down. A click is
    // resolved at the release, and re-centring on a mid that moved in between would trade the row
    // that is under the pointer THEN, not the price the trader saw under it at the press.
    let center = state.center.or(state.anchor).unwrap_or(book_center);
    let fresh = row_key(center, tick, group).saturating_add(half);
    let held =
        response.is_pointer_button_down_on() || response.clicked() || response.drag_stopped();
    // What the frame before drew, on this grid.
    let before = ui
        .data(|d| d.get_temp::<TradeWindowLadderDrawn>(id))
        .filter(|d| d.tick == tick && d.group == group);
    let top_key = before.as_ref().filter(|_| held).map_or(fresh, |d| d.top_key);
    let key_of = |i: usize| top_key.saturating_sub(i as i64);
    let row_at = |y: f32| -> i64 {
        let index = ((y - region.min.y) / row_h).floor().clamp(0.0, (n_rows - 1) as f32);
        key_of(index as usize)
    };

    let (bids, asks) = inputs.book.top_n(400);
    let bid_map = group_book(&bids, tick, group);
    let ask_map = group_book(&asks, tick, group);

    // Peak grouped quantity over the visible window → depth-bar scale.
    let mut vmax = 0.0_f64;
    for i in 0..n_rows {
        let key = key_of(i);
        let q =
            bid_map.get(&key).copied().unwrap_or(0.0) + ask_map.get(&key).copied().unwrap_or(0.0);
        if q > vmax {
            vmax = q;
        }
    }
    let vmax = if vmax <= 0.0 { 1.0 } else { vmax };

    // The Vol column's numbers: what the tape holds, summed into the rows by the grouping, and the
    // peak over the visible rows that scales its bars. An empty tape is nothing here: a row without
    // an entry prints [`NO_VOLUME`].
    let traded = group_tape(inputs.tape, tick, group);
    let traded_peak =
        (0..n_rows).filter_map(|i| traded.get(&key_of(i))).copied().fold(0.0_f64, f64::max);
    let traded_peak = if traded_peak <= 0.0 { 1.0 } else { traded_peak };

    let last_key = row_key(last, tick, group);
    let position_key =
        inputs.position.filter(|p| p.size != 0.0).map(|p| row_key(p.average_price, tick, group));

    // ⚠ Own orders the node does not attribute to an account are not drawn and cannot be hit
    // (Ruling R9): a marker there could cancel an order of another account.
    let own: &[TradeWindowLadderOrder] =
        if inputs.orders_why.is_some() { &[] } else { inputs.orders };
    let own_at = |key: i64, side: i32| -> Vec<&TradeWindowLadderOrder> {
        own.iter().filter(|o| o.side == side && row_key(o.price, tick, group) == key).collect()
    };
    let order_client_ids = |orders: &[&TradeWindowLadderOrder]| -> Vec<String> {
        orders.iter().map(|o| o.order_client_id.clone()).collect()
    };

    // ⚠ The press: whether the button went down on a marker — a cancel — or on an empty cell — an
    // order — is decided in the frame it goes down, against the rows AND the markers the trader saw
    // there (the frame before's), and never again at the release. A marker whose orders fill, or
    // are cancelled from elsewhere, while the button is down is gone at the release; a release that
    // looked again found an empty cell and placed a NEW order at that row: a click meant to pull a
    // stop that had just fired opened a position the other way (final review A, I-1). A cancel of
    // an order that has gone is a harmless refusal.
    let press = if !held {
        None
    } else {
        let new = ui.input(|i| i.pointer.primary_pressed());
        match before.as_ref().and_then(|d| d.press.clone()) {
            Some(p) if !new => Some(p),
            _ => ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos()).map(
                |at| {
                    let (key, col) = (row_at(at.y), cols.at(at.x, region.min.x));
                    let shown = |side: i32| match &before {
                        Some(d) => d
                            .marks
                            .iter()
                            .find(|m| m.key == key && m.side == side)
                            .map_or_else(Vec::new, |m| m.order_client_ids.clone()),
                        None => order_client_ids(&own_at(key, side)),
                    };
                    let under = match col {
                        TradeWindowLadderColumn::Buy => shown(1),
                        TradeWindowLadderColumn::Sell => shown(-1),
                        TradeWindowLadderColumn::Bid
                        | TradeWindowLadderColumn::Price
                        | TradeWindowLadderColumn::Ask
                        | TradeWindowLadderColumn::Volume => Vec::new(),
                    };
                    TradeWindowLadderPress {
                        key,
                        col,
                        order_client_ids: under,
                        stale: inputs.stale,
                    }
                },
            ),
        }
    };

    // --- the rows ---
    let painter = ui.painter().clone();
    let draggable = drag_to_reprice_allowed(&inputs.capabilities);
    let tradable = matches!(inputs.tradable, TradeWindowTradable::Yes);
    // The cell under the pointer takes the design's wash and the pointing hand — where a click
    // there would do something: not while a drag is under way, and never on an account that
    // trades nothing here.
    let pointer = response.hover_pos();
    let hot = pointer
        .filter(|_| tradable && state.drag.is_none())
        .map(|at| (row_at(at.y), cols.at(at.x, region.min.x)))
        .filter(|(_, col)| side_of(*col).is_some());
    if hot.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let mono = t.mono(ThemeTextRole::Body);
    let mono_small = t.mono(ThemeTextRole::Caption);
    let mono_last = t.mono_semibold(ThemeTextRole::Body);
    let mut marks = Vec::new();
    for i in 0..n_rows {
        let key = key_of(i);
        let top = region.min.y + i as f32 * row_h;
        let row = Rect::from_min_size(pos2(region.min.x, top), vec2(region.width(), row_h));
        let is_last = key == last_key;
        // The position's average-price row is the neutral hover fill; odd rows are the kit table's
        // zebra (spec §4.2). The LAST row is an accent OUTLINE, painted after its contents below —
        // the spec's "last-price outline", never a tinted fill.
        if position_key == Some(key) {
            painter.rect_filled(row, 0.0, t.theme.hover);
        } else if i % 2 == 1 {
            painter.rect_filled(row, 0.0, t.theme.surface);
        }
        if let Some((hot_key, hot_col)) = hot
            && hot_key == key
        {
            painter.rect_filled(
                cols.rect(hot_col, row),
                0.0,
                with_alpha(t.theme.text, trade::WASH_ALPHA),
            );
        }

        // Depth bars grow outward from the price column: the market set's depth fills, at the
        // DOM's own alphas (spec §3.2), a whole percent of the column as the design draws them.
        // The sizes are the set's TEXT colours.
        let bidq = bid_map.get(&key).copied().unwrap_or(0.0);
        let askq = ask_map.get(&key).copied().unwrap_or(0.0);
        let (bidcol, askcol) = (
            cols.rect(TradeWindowLadderColumn::Bid, row),
            cols.rect(TradeWindowLadderColumn::Ask, row),
        );
        // A depth bar sits `space::XS` inside its row above and below: the design's 14 pt bar in an
        // 18 pt row.
        let (bar_top, bar_bottom) = (top + space::XS, top + row_h - space::XS);
        let share = |q: f64| ((q / vmax * 100.0).round() / 100.0) as f32;
        if bidq > 0.0 {
            let w = share(bidq) * bidcol.width();
            let bar =
                Rect::from_min_max(pos2(bidcol.max.x - w, bar_top), pos2(bidcol.max.x, bar_bottom));
            painter.rect_filled(bar, 0.0, t.market.up_depth);
            let ink = maps::side::BID.text.resolve(t);
            let room = bidcol.width() - SIZE_PAD;
            let g = size_galley(
                &painter,
                &mono,
                ink,
                bidq,
                size_decimals(bidq, inputs.grid.lot_size),
                room,
            );
            let at = pos2(bidcol.max.x - SIZE_PAD - g.size().x, row.center().y - g.size().y / 2.0);
            painter.galley(at, g, ink);
        }
        if askq > 0.0 {
            let w = share(askq) * askcol.width();
            let bar =
                Rect::from_min_max(pos2(askcol.min.x, bar_top), pos2(askcol.min.x + w, bar_bottom));
            painter.rect_filled(bar, 0.0, t.market.down_depth);
            let ink = maps::side::ASK.text.resolve(t);
            let room = askcol.width() - SIZE_PAD;
            let g = size_galley(
                &painter,
                &mono,
                ink,
                askq,
                size_decimals(askq, inputs.grid.lot_size),
                room,
            );
            let at = pos2(askcol.min.x + SIZE_PAD, row.center().y - g.size().y / 2.0);
            painter.galley(at, g, ink);
        }
        // The price column: the last price in the text colour and the semibold weight, every other
        // rung secondary. A number is never the accent (spec §2). A row that names no price draws
        // none.
        if let Some(price) = named_price(key, tick, group) {
            let (font, ink) =
                if is_last { (&mono_last, t.theme.text) } else { (&mono, t.theme.text_secondary) };
            painter.text(
                cols.rect(TradeWindowLadderColumn::Price, row).center(),
                Align2::CENTER_CENTER,
                fmt_price(price, Some(tick)),
                font.clone(),
                ink,
            );
        }
        // The traded volume, where the toolbar's first button has the column on: grey, against the
        // ladder's right edge, over a bar that grows from it. The bar sits `trade::VOLUME_BAR_INSET` inside its
        // row above and below (the design's 8 pt bar in an 18 pt row would be 5).
        if cols.shown(TradeWindowLadderColumn::Volume) {
            let volume = cols.rect(TradeWindowLadderColumn::Volume, row);
            let mid = row.center().y;
            match traded.get(&key) {
                Some(&v) => {
                    let w = ((v / traded_peak * 100.0).round() / 100.0) as f32 * volume.width();
                    let bar = Rect::from_min_max(
                        pos2(volume.max.x - w, top + trade::VOLUME_BAR_INSET),
                        pos2(volume.max.x, top + row_h - trade::VOLUME_BAR_INSET),
                    );
                    painter.rect_filled(
                        bar,
                        0.0,
                        with_alpha(t.theme.text_tertiary, trade::VOLUME_BAR_ALPHA),
                    );
                    let g = size_galley(
                        &painter,
                        &mono_small,
                        t.theme.text_tertiary,
                        v,
                        size_decimals(v, inputs.grid.lot_size),
                        volume.width(),
                    );
                    painter.galley(
                        pos2(volume.max.x - g.size().x, mid - g.size().y / 2.0),
                        g,
                        t.theme.text_tertiary,
                    );
                }
                None => {
                    let at = pos2(volume.max.x, mid);
                    painter.text(
                        at,
                        Align2::RIGHT_CENTER,
                        NO_VOLUME,
                        mono_small.clone(),
                        t.theme.text_tertiary,
                    );
                }
            }
        }

        // ONE marker per side, standing for every own order on this row and side.
        for (side, col) in [(1, TradeWindowLadderColumn::Buy), (-1, TradeWindowLadderColumn::Sell)]
        {
            let here = own_at(key, side);
            if !here.is_empty() {
                let face = marker_face(&here, inputs.grid.lot_size);
                let dragging = state
                    .drag
                    .as_ref()
                    .is_some_and(|d| here.iter().any(|o| d.contains(&o.order_client_id)));
                let cell = cols.rect(col, row);
                paint_marker(
                    &painter,
                    t,
                    cell,
                    side,
                    &face,
                    TradeWindowLadderMarkerState { draggable, dragging, pointer },
                );
                marks.push(TradeWindowLadderMarker {
                    key,
                    side,
                    order_client_ids: order_client_ids(&here),
                });
            }
        }
        if is_last {
            painter.rect_stroke(
                row,
                0.0,
                Stroke::new(stroke::HAIRLINE, t.theme.accent),
                StrokeKind::Inside,
            );
        }
    }
    let drawn = TradeWindowLadderDrawn { top_key, tick, group, marks, press: press.clone() };
    ui.data_mut(|d| d.insert_temp(id, drawn));

    // stale overlay: dim the whole book region into the theme, so a frozen book never reads as live
    if inputs.stale {
        painter.rect_filled(region, 0.0, t.theme.scrim());
    }

    // --- the click grammar, over the rows as they were DRAWN (`top_key` above) ---
    // A drag this ladder did not see end must never reprice later.
    if !(response.dragged() || response.drag_stopped()) {
        state.drag = None;
    }
    if let Some(why) = crate::trade_window::describe::why_untradable(inputs) {
        // Nothing here may produce an order (Review Focus 2); the hover says why.
        state.drag = None;
        let _ = response.clone().on_hover_text(why);
    }
    // ⚠ A STALE book places no order and moves none from here: limit or stop is chosen against a
    // top of book that may be long gone, and a drop prices an order off it. A marker still
    // cancels — a cancel takes risk away, and names its orders, not a price. The hint line says
    // so, and a refused gesture says so on the strip (the owner's decision of 2026-10-03, round 2,
    // item 2).
    let stale = || TradeWindowAction::Note {
        kind: TradeWindowStatusLineKind::Error,
        text: STALE_WHY.to_string(),
    };
    if tradable && let Some(screen_position) = response.interact_pointer_pos() {
        // A drag begins where the button went DOWN, not where the pointer is once it has moved far
        // enough to count as one, and only on a marker, on a venue that can modify (audit br6). It
        // takes the orders the marker under the press stood for ([`TradeWindowLadderPress`]).
        if response.drag_started()
            && drag_to_reprice_allowed(&inputs.capabilities)
            && let Some(p) = &press
            && !p.order_client_ids.is_empty()
        {
            if inputs.stale || p.stale {
                actions.push(stale());
            } else {
                state.drag = Some(p.order_client_ids.clone());
            }
        }
        // The drop: every dragged order not already on the drop row moves to it. Belt-and-braces
        // re-check of the gate, so a drag left dangling by a venue switch never emits a Modify. A
        // drop OUTSIDE the rows moves nothing: `row_at` would clamp it onto the edge row, a price
        // the pointer was never on. ⚠ Nor does an Escape: egui ends a drag on Escape as a
        // `drag_stopped` with the pointer wherever it is, which is a cancel, not a drop there. And
        // a row that names no price ([`named_price`]) takes no drop.
        let key = row_at(screen_position.y);
        if response.drag_stopped()
            && let Some(order_client_ids) = state.drag.take()
            && drag_to_reprice_allowed(&inputs.capabilities)
            && region.contains(screen_position)
            && !ui.input(|i| i.key_pressed(egui::Key::Escape))
            && let Some(new_price) = named_price(key, tick, group)
        {
            // Only the dragged orders still working NOW: one that filled or was cancelled while
            // the button was down is sent nothing (the node would refuse it, and the strip would
            // show a rejection for an order the trader no longer sees). With none left, nothing
            // at all (the FW1 review).
            let working = |order_client_id: &String| {
                own.iter().any(|o| &o.order_client_id == order_client_id)
            };
            let stays = |order_client_id: &String| {
                own.iter().any(|o| {
                    &o.order_client_id == order_client_id && row_key(o.price, tick, group) == key
                })
            };
            if inputs.stale {
                // The book went stale while the drag was under way.
                actions.push(stale());
            } else {
                actions.extend(
                    order_client_ids.into_iter().filter(|c| working(c) && !stays(c)).map(
                        |order_client_id| TradeWindowAction::Modify { order_client_id, new_price },
                    ),
                );
            }
        }
        // A click trades the cell the button went DOWN on ([`TradeWindowLadderPress`]): a marker there was a cancel
        // of the orders it stood for, whatever became of them since — NEVER an order — and an empty
        // cell an order, even where a marker has appeared since.
        if response.clicked()
            && state.drag.is_none()
            && let Some(TradeWindowLadderPress {
                key,
                col,
                order_client_ids,
                stale: stale_at_press,
            }) = press.clone()
            && let Some(side) = side_of(col)
        {
            if !order_client_ids.is_empty() {
                actions.push(TradeWindowAction::Cancel(order_client_ids));
            } else if inputs.stale || stale_at_press {
                // Stale at the release, or at the press: the row was chosen off a frozen book.
                actions.push(stale());
            } else if let Some(price) = named_price(key, tick, group) {
                // The order's own price FIRST: a size typed in the quote currency converts at the
                // price the order is placed at, as the ticket's does — not at the last price, which
                // buys more than the amount entered above it and nothing at all while there is none.
                // A row that names no price takes no order.
                // The TICKET's size rule (`ticket::checked_quantity`): at least one lot, a finite count
                // of them, and no less than the smallest order. A click it refuses sends nothing
                // and says why on the strip, as a disabled ticket button says why on hover.
                let quantity = sizing::base_quantity(
                    &state.size,
                    state.unit,
                    Some(price),
                    inputs.grid.lot_size,
                    inputs.value,
                );
                match crate::trade_window::ticket::checked_quantity(
                    quantity,
                    state,
                    inputs,
                    Some(price),
                ) {
                    Ok(quantity) => {
                        let order_type = click_type(
                            side,
                            price,
                            best_bid.unwrap_or(price),
                            best_ask.unwrap_or(price),
                        );
                        if state.post_only && order_type == TradeWindowTicketKind::Stop {
                            // ⚠ A maker-only trader's click through the market is NOT turned into a
                            // stop, which takes liquidity when it fires: it is refused, with the words.
                            actions.push(TradeWindowAction::Note {
                                kind: TradeWindowStatusLineKind::Error,
                                text: crate::trade_window::ticket::POST_LADDER_WHY.to_string(),
                            });
                        } else {
                            state.submit(
                                TradeWindowAction::Place {
                                    side,
                                    order_type,
                                    price: Some(price),
                                    quantity,
                                    reduce_only: state.reduce_only,
                                    post_only: state.post_only,
                                    exits: None,
                                    origin: crate::trade_window::TradeWindowOrigin::Ladder,
                                },
                                actions,
                            );
                        }
                    }
                    Err(why) => actions.push(TradeWindowAction::Note {
                        kind: TradeWindowStatusLineKind::Error,
                        text: why,
                    }),
                }
            }
        }
    }

    // The marker under the pointer lists the orders it stands for and their summed size, read off
    // this frame's orders as the marker itself is (the owner's decision of 2026-10-03, card 1).
    let hovered_side = |at: Pos2| match cols.at(at.x, region.min.x) {
        TradeWindowLadderColumn::Buy => Some(1),
        TradeWindowLadderColumn::Sell => Some(-1),
        TradeWindowLadderColumn::Bid
        | TradeWindowLadderColumn::Price
        | TradeWindowLadderColumn::Ask
        | TradeWindowLadderColumn::Volume => None,
    };
    if state.drag.is_none()
        && let Some(at) = response.hover_pos()
        && let Some(side) = hovered_side(at)
    {
        let here = own_at(row_at(at.y), side);
        if !here.is_empty() {
            let lines = marker_lines(&here, tick, inputs.grid.lot_size, inputs.base);
            let _ = response.clone().on_hover_ui_at_pointer(|ui| {
                for line in lines {
                    let words = RichText::new(line).font(t.mono(ThemeTextRole::Caption));
                    ui.label(words.color(t.theme.text));
                }
            });
        }
    }

    // The scroll wheel nudges the centre and latches it; Recenter un-latches it.
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
    if response.hovered() && scroll.abs() > 0.5 {
        let step = key_price(1, tick, group);
        let base = state.center.or(state.anchor).unwrap_or(book_center);
        state.center = Some(base + (scroll.signum() as f64) * step);
    }
}
