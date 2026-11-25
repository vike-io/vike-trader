//! The ladder (spec §3.4, drawn as the owner's v3 design draws it): Buy · Bid · Price · Ask · Sell
//! and, when the toolbar's first button asks for it, Vol — click to trade. Grown out of the DOM
//! (`vike_panels::dom`, deleted with this PR). The row painting, the markers and the honesty rules
//! are the DOM's, the columns and the click grammar are the spec's.
//!
//! # The design's measures
//!
//! Top to bottom: a toolbar one control high, the column captions, the rows, and in the side
//! layout a hint line pinned to the bottom, each of the first two and the last set off by a
//! one-point hairline in the theme's hover grey. The design's pixels are points here (its 25 px
//! toolbar is `control_height` and the hairline, its 18 px rows are `row_height`), and the columns are its
//! own widths (`TradeWindowLadderColumns`) scaled to the width the ladder is given. The toolbar carries the Vol
//! toggle at its left and, at its right, the grouping and the recenter button (`toolbar`); the
//! buttons and their glyphs are painted here, on the design's own grid, because the kit's icon
//! button is a `control_height` square and the design's is a 24 × 20 one.
//!
//! # The click grammar
//!
//! The column picks the side (Buy and Bid buy, Ask and Sell sell, Price and Vol are inert), the
//! row picks the price, and the market picks limit or stop (`columns::click_type`). Own orders sit in the
//! Buy and Sell columns as ONE pill per row and side; clicking it cancels every order it stands
//! for, and dragging it reprices them where the venue can modify an order. A ladder click never
//! carries TP/SL (spec §3.6): only the ticket's Buy and Sell buttons do.
//!
//! # What may not happen here
//!
//! - **No book, no ladder** ([`absence::NO_BOOK_HEADLINE`]): with no level on either side nothing
//!   ladder-shaped is drawn — no captions, no rows, no click region.
//! - **No order on an untradable symbol** (spec §4.3): [`TradeWindowTradable::No`](super::TradeWindowTradable::No) takes every click and drag
//!   out of the ladder, and the hint line and the hover say why.
//! - **No marker the node cannot attribute** (Ruling R9): while [`TradeWindowInputs::orders_why`] is
//!   `Some`, no own-order marker is drawn and none can be clicked or dragged.
//! - **No order from a stale book** (the owner's decision of 2026-10-03): a click or a drag that
//!   was pressed, or is released, while [`TradeWindowInputs::stale`] places nothing and moves nothing; a
//!   marker still cancels, and the hint line says so ([`HINT_STALE`]).
//! - **No click on a row that moved under the pointer**: while the button is down on the ladder
//!   the rows hold still where they were drawn, and the click is resolved against those rows.
//! - **No count a marker hides**: a marker that cannot hold its size beside its count keeps the
//!   count, because a click on it cancels every order it stands for.
//! - **No volume nobody reported**: the Vol column prints what [`TradeWindowInputs::tape`] holds and nothing
//!   else. A row no print on the tape fell in prints `NO_VOLUME`, and with no report at all
//!   the whole column does — the toggle never fills it with numbers of its own.
//! - **No digit cut off**: a size too wide for its column is printed to fewer decimals, never with
//!   its integer part clipped (`rows::size_galley`).

pub mod absence;
mod columns;
mod markers;
pub(crate) mod rows;
#[cfg(test)]
mod tests;
mod toolbar;

use super::{
    TradeWindowAction, TradeWindowInputs, TradeWindowState, TradeWindowTicketPlace, sizing,
};
use absence::no_book;
use columns::{CAPTIONS, COLS, TradeWindowLadderColumn, TradeWindowLadderColumns, price_floor};
use egui::{Align, Align2, Layout, Rect, RichText, UiBuilder, pos2, vec2};
use rows::{SIZE_PAD, TradeWindowLadderRowSpace, row_tick, rows};
use toolbar::toolbar;
use vike_marketdata::L2BookLevel;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::metrics::{ThemeDensityMetrics, space, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;

/// Grouping steps in ticks (the price-consolidation selector).
pub(crate) const GROUP_STEPS: [i64; 6] = [1, 2, 5, 10, 25, 50];

/// The line under the ladder in the side layout (spec §3.4).
pub const HINT: &str = "Click: limit or stop · Own order: cancel";

/// [`HINT`] where no own order can be shown (`TradeWindowInputs::orders_why`): there is no marker to
/// click, so the line does not offer one.
pub const HINT_NO_MARKERS: &str = "Click: limit or stop";

/// [`HINT`] while the book is stale ([`TradeWindowInputs::stale`]): a click places nothing and a drag
/// moves nothing, and a marker still cancels (the owner's decision of 2026-10-03, round 2, item 2).
pub const HINT_STALE: &str = "Book stale: a click places nothing · Own order: cancel";

/// [`HINT_STALE`] where no own order can be shown, as [`HINT_NO_MARKERS`] is [`HINT`]'s.
pub const HINT_STALE_NO_MARKERS: &str = "Book stale: a click places nothing";

/// What the strip says for a click or a drop the stale book refused.
const STALE_WHY: &str = "Not sent: the book is stale, so the ladder places and moves no order \
                             until its depth is live again.";

/// A price on the tick, with thousands grouped: `65,432.5`, `0.5231`, `0.00001234`; whole while no
/// tick is known (`None`). A price that is not a number prints a dash, never `NaN` or `inf`.
pub fn fmt_price(price: f64, tick: Option<f64>) -> String {
    if price.is_finite() {
        vike_ui_theme::fmt::format_thousands_precision(price, tick.map_or(0, sizing::decimals_of))
    } else {
        NO_PRICE.to_string()
    }
}

/// What [`fmt_price`] prints for a price that is not a number.
pub(crate) const NO_PRICE: &str = "—";

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// What the hint line is taller than a row: its hairline and the two points the design leaves.
const HINT_EXTRA: f32 = stroke::HAIRLINE + space::XS;

// The alpha of the volume bar (`trade::VOLUME_BAR_ALPHA`: the design's `rgba(…, .28)`) and of the wash a
// cell takes under the pointer (`trade::WASH_ALPHA`: `.05`).

/// What the Vol column prints for a row nobody reported a traded volume for.
pub(crate) const NO_VOLUME: &str = "—";

/// What the toolbar's first button is called while the Vol column is off, and on: what a click on
/// it does.
pub(crate) const VOLUME_SHOW: &str = "Show the volume column";
pub(crate) const VOLUME_HIDE: &str = "Hide the volume column";

/// Where the ladder's pieces sit in the rect it is given, top to bottom. One function, so the
/// painting, the hit-testing and the tests cannot disagree about a row's position.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TradeWindowLadderParts {
    /// Grouping, recenter and STALE.
    toolbar: Rect,
    /// The five column captions.
    head: Rect,
    /// The rows: the click-to-trade region.
    rows: Rect,
    /// The hint line, when there is one.
    hint: Option<Rect>,
}

/// The rows as last DRAWN: the top row's key, the grid it was keyed on, and the markers they
/// showed. Kept in egui's temp memory under the ladder's id, so a click resolves against the rows
/// — and the markers — the trader saw.
#[derive(Clone, Debug)]
struct TradeWindowLadderDrawn {
    top_key: i64,
    tick: f64,
    group: i64,
    /// Every own-order marker these rows showed.
    marks: Vec<TradeWindowLadderMarker>,
    /// What the button went down on, from the press to the release; `None` while it is up.
    press: Option<TradeWindowLadderPress>,
}

/// One drawn marker: its row's key, its side, and the orders it stood for.
#[derive(Clone, Debug)]
struct TradeWindowLadderMarker {
    key: i64,
    side: i32,
    order_client_ids: Vec<String>,
}

/// What the button went down on — the cell, and the orders the marker in it stood for (none for a
/// cell with no marker) — decided ONCE, in the frame it went down, against the rows and the
/// markers as they were drawn (final review A, I-1); and whether the book was stale then.
#[derive(Clone, Debug)]
struct TradeWindowLadderPress {
    key: i64,
    col: TradeWindowLadderColumn,
    order_client_ids: Vec<String>,
    /// The book was stale when the button went down: the rows the trader pressed were drawn off
    /// a frozen book, and stay where they were drawn until the release, so a click or a drag that
    /// began there places and moves nothing, however live the book is by the release (the FW1
    /// review; the owner's decision of 2026-10-03, round 2, item 2).
    stale: bool,
}

/// The rect the ladder lays its rows in, inside the ladder's own rect `area`: the tick chart beside
/// it paints on this rect's vertical extent ([`rows::TradeWindowLadderRowFrame`]). `None` where the ladder
/// draws no rows (no book).
pub(super) fn rows_region(
    area: Rect,
    t: &ThemeTokens,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
) -> Option<Rect> {
    let bookless = inputs.book.bid_levels() + inputs.book.ask_levels() == 0;
    (!bookless)
        .then(|| parts(area, &t.metrics, state.view.panel == TradeWindowTicketPlace::Beside).rows)
}

/// The toolbar, the captions and the hint line are each set off by a `stroke::HAIRLINE` rule, in
/// the theme's hover grey, as the design draws them.
fn parts(area: Rect, m: &ThemeDensityMetrics, hint: bool) -> TradeWindowLadderParts {
    let toolbar =
        Rect::from_min_size(area.min, vec2(area.width(), m.control_height + stroke::HAIRLINE));
    let head = Rect::from_min_size(
        pos2(area.min.x, toolbar.max.y),
        vec2(area.width(), m.row_height + stroke::HAIRLINE),
    );
    let hint = hint.then(|| {
        let h = m.row_height + HINT_EXTRA;
        Rect::from_min_max(pos2(area.min.x, (area.max.y - h).max(head.max.y)), area.max)
    });
    let bottom = hint.map_or(area.max.y, |h| h.min.y).max(head.max.y);
    TradeWindowLadderParts {
        toolbar,
        head,
        rows: Rect::from_min_max(head.left_bottom(), pos2(area.max.x, bottom)),
        hint,
    }
}

/// How many rows fit in `height`: at least one.
pub(super) fn row_count(height: f32, row_h: f32) -> usize {
    (height / row_h).floor().max(1.0) as usize
}

/// The price the rows centre on while the trader has not scrolled: the live book's mid, so the
/// real depth is always in view even when a last-trade mark would sit a few ticks off the top of
/// book. Falls back to the best price on either side, then to the window's price.
pub(super) fn book_centre(inputs: &TradeWindowInputs<'_>) -> f64 {
    let best_bid = inputs.book.best_bid().map(|L2BookLevel { price: p, .. }| p);
    let best_ask = inputs.book.best_ask().map(|L2BookLevel { price: p, .. }| p);
    let last = inputs.last.or_else(|| inputs.book.mid()).or(best_bid).or(best_ask).unwrap_or(0.0);
    inputs.book.mid().or(best_bid).or(best_ask).unwrap_or(last)
}

/// Draw the ladder into `ui` (its region). Every intent leaves through `actions`; a new order
/// leaves through [`TradeWindowState::submit`], so one-click off holds it for a confirm.
pub fn draw(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let tick = row_tick(inputs.grid.tick_size, inputs.book.tick_size);
    let area = ui.available_rect_before_wrap();
    // ⚠ Computed FIRST, because it decides what may be drawn at all.
    let bookless = inputs.book.bid_levels() + inputs.book.ask_levels() == 0;
    let beside = state.view.panel == TradeWindowTicketPlace::Beside;
    let p = parts(area, &t.metrics, beside && !bookless);

    toolbar(ui, t, state, p.toolbar, inputs.stale && !bookless, beside);

    if bookless {
        // Nothing ladder-shaped: no captions, no rows, no hint, no click region. A drag that was
        // under way when the book went has nothing left to drop onto.
        state.drag = None;
        let region = Rect::from_min_max(p.toolbar.left_bottom(), area.max);
        ui.painter().rect_filled(region, 0.0, t.theme.background);
        // The link as the FEED badge reads it: a venue with no line is one nobody reported on.
        let link = super::instrument_bar::link_of(inputs.source);
        no_book(
            ui,
            t,
            region,
            inputs.absence,
            link,
            inputs.top.map(|q| (q, inputs.grid.tick_size)),
        );
        return;
    }

    // The Vol column exists in the side layout only, where the design has room for it, and only
    // while the toolbar's first button has it on.
    let centre = state.center.or(state.anchor).unwrap_or_else(|| book_centre(inputs));
    let floor = price_floor(
        ui.painter(),
        t,
        tick,
        state.group,
        centre,
        row_count(p.rows.height(), t.metrics.row_height),
    );
    let cols = TradeWindowLadderColumns::new(
        state.view.panel,
        beside && state.volume,
        p.rows.width(),
        floor,
    );
    head(ui, t, p.head, &cols);
    ui.painter().rect_filled(p.rows, 0.0, t.theme.background);
    rows(
        ui,
        t,
        state,
        inputs,
        actions,
        TradeWindowLadderRowSpace { region: p.rows, cols: &cols, tick },
    );
    if let Some(r) = p.hint {
        hint_line(ui, t, r, inputs);
    }
}

/// The captions over the columns, and the hairline under them. Bid's is set against its column's
/// right and Ask's against its left, each the size's own distance in, as the sizes under them are;
/// Vol's against the ladder's right edge, as its numbers are.
fn head(ui: &mut egui::Ui, t: &ThemeTokens, rect: Rect, cols: &TradeWindowLadderColumns) {
    let painter = ui.painter();
    let line = Rect::from_min_max(pos2(rect.min.x, rect.max.y - stroke::HAIRLINE), rect.max);
    painter.rect_filled(line, 0.0, t.theme.hover);
    let band = Rect::from_min_max(rect.min, pos2(rect.max.x, line.min.y));
    for (col, words) in COLS.iter().zip(CAPTIONS) {
        if !cols.shown(*col) {
            continue;
        }
        let c = cols.rect(*col, band);
        let (anchor, x) = match col {
            TradeWindowLadderColumn::Bid => (Align2::RIGHT_CENTER, c.max.x - SIZE_PAD),
            TradeWindowLadderColumn::Ask => (Align2::LEFT_CENTER, c.min.x + SIZE_PAD),
            TradeWindowLadderColumn::Volume => (Align2::RIGHT_CENTER, c.max.x),
            TradeWindowLadderColumn::Buy
            | TradeWindowLadderColumn::Price
            | TradeWindowLadderColumn::Sell => (Align2::CENTER_CENTER, c.center().x),
        };
        painter.text(
            pos2(x, c.center().y),
            anchor,
            words,
            t.font(ThemeTextRole::Caption),
            t.theme.text_tertiary,
        );
    }
}

/// The line under the rows in the side layout: how a click trades; where the account does not
/// trade the symbol, why a click does nothing (spec §4.3); and while the book is stale, that a
/// click places nothing and a marker still cancels. A hairline sets it off from the rows.
fn hint_line(ui: &mut egui::Ui, t: &ThemeTokens, rect: Rect, inputs: &TradeWindowInputs<'_>) {
    let markers = inputs.orders_why.is_none();
    let words = match super::describe::why_untradable(inputs) {
        Some(why) => why,
        None if inputs.stale && markers => HINT_STALE.to_string(),
        None if inputs.stale => HINT_STALE_NO_MARKERS.to_string(),
        None if markers => HINT.to_string(),
        None => HINT_NO_MARKERS.to_string(),
    };
    let line = Rect::from_min_size(rect.min, vec2(rect.width(), stroke::HAIRLINE));
    ui.painter().rect_filled(line, 0.0, t.theme.hover);
    let mut text = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(rect.min.x, line.max.y), rect.max))
            .layout(Layout::left_to_right(Align::Center)),
    );
    text.shrink_clip_rect(rect);
    text.add(
        egui::Label::new(
            RichText::new(words).font(t.font(ThemeTextRole::Caption)).color(t.theme.text_tertiary),
        )
        .truncate(),
    );
}
