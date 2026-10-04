//! The ladder (spec §3.4): Buy · Bid · Price · Ask · Sell, click to trade. Grown out of the DOM
//! (`vike_panels::dom`, deleted with this PR). The row painting, the markers and the honesty rules
//! are the DOM's, the columns and the click grammar are the spec's.
//!
//! # The click grammar
//!
//! The column picks the side (Buy and Bid buy, Ask and Sell sell, Price is inert), the row picks
//! the price, and the market picks limit or stop ([`click_type`]). Own orders sit in the Buy and
//! Sell columns as ONE marker per row and side; clicking it cancels every order it stands for, and
//! dragging it reprices them where the venue can modify an order. A ladder click never carries
//! TP/SL (spec §3.6): only the ticket's Buy and Sell buttons do.
//!
//! # What may not happen here
//!
//! - **No book, no ladder** ([`NO_BOOK_HEADLINE`]): with no level on either side nothing
//!   ladder-shaped is drawn — no captions, no rows, no click region.
//! - **No order on an untradable symbol** (spec §4.3): [`Tradable::No`] takes every click and drag
//!   out of the ladder, and the hint line and the hover say why.
//! - **No marker the node cannot attribute** (Ruling R9): while [`TradeInputs::orders_why`] is
//!   `Some`, no own-order marker is drawn and none can be clicked or dragged.
//! - **No order from a stale book** (the owner's decision of 2026-10-03): a click or a drag that
//!   was pressed, or is released, while [`TradeInputs::stale`] places nothing and moves nothing; a
//!   marker still cancels, and the hint line says so ([`HINT_STALE`]).
//! - **No click on a row that moved under the pointer**: while the button is down on the ladder
//!   the rows hold still where they were drawn, and the click is resolved against those rows.
//! - **No count a marker hides**: a marker that cannot hold its size beside its count keeps the
//!   count, because a click on it cancels every order it stands for.

use egui::{
    Align, Align2, Color32, CornerRadius, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    UiBuilder, pos2, vec2,
};
use vike_model::feed_status::ConnectionState;
use vike_model::{BookLevel, VenueCaps};
use vike_ui_theme::components::button::IconButton;
use vike_ui_theme::components::{ON_FILL, Status, Tokens, chip};
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::{Metrics, RADIUS};
use vike_ui_theme::type_scale::TextRole;

use super::{
    BookAbsence, LadderOrder, OrderType, Panel, StatusKind, Tradable, TradeAction, TradeInputs,
    TradeState, sizing,
};

/// Grouping steps in ticks (the price-consolidation selector).
pub const GROUP_STEPS: [i64; 6] = [1, 2, 5, 10, 25, 50];

/// The line under the ladder in the side layout (spec §3.4).
pub const HINT: &str = "Click: limit or stop · Own order: cancel";

/// [`HINT`] where no own order can be shown (`TradeInputs::orders_why`): there is no marker to
/// click, so the line does not offer one.
pub const HINT_NO_MARKERS: &str = "Click: limit or stop";

/// [`HINT`] while the book is stale ([`TradeInputs::stale`]): a click places nothing and a drag
/// moves nothing, and a marker still cancels (the owner's decision of 2026-10-03, round 2, item 2).
pub const HINT_STALE: &str = "Book stale: a click places nothing · Own order: cancel";

/// [`HINT_STALE`] where no own order can be shown, as [`HINT_NO_MARKERS`] is [`HINT`]'s.
pub const HINT_STALE_NO_MARKERS: &str = "Book stale: a click places nothing";

/// What the strip says for a click or a drop the stale book refused.
const STALE_WHY: &str = "Not sent: the book is stale, so the ladder places and moves no order \
                             until its depth is live again.";

/// The five columns (spec §3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    Buy,
    Bid,
    Price,
    Ask,
    Sell,
}

const COLS: [Col; 5] = [Col::Buy, Col::Bid, Col::Price, Col::Ask, Col::Sell];
/// Each column's share of the ladder's width, in [`COLS`]' order.
const COL_FRACTIONS: [f32; 5] = [0.14, 0.2, 0.32, 0.2, 0.14];
/// The column captions, in [`COLS`]' order.
const CAPTIONS: [&str; 5] = ["Buy", "Bid", "Price", "Ask", "Sell"];

/// The column a pointer `x` falls in.
pub fn col_at(x: f32, left: f32, width: f32) -> Col {
    let rel = ((x - left) / width).clamp(0.0, 0.999);
    let mut edge = 0.0;
    for (col, f) in COLS.iter().zip(COL_FRACTIONS) {
        edge += f;
        if rel < edge {
            return *col;
        }
    }
    Col::Sell
}

/// `col`'s rect within `row`.
pub fn col_rect(col: Col, row: Rect) -> Rect {
    let i = COLS.iter().position(|c| *c == col).unwrap_or(0);
    let start: f32 = COL_FRACTIONS[..i].iter().sum();
    Rect::from_min_size(
        pos2(row.min.x + start * row.width(), row.min.y),
        vec2(COL_FRACTIONS[i] * row.width(), row.height()),
    )
}

/// The side a click in `col` trades: Buy and Bid buy, Ask and Sell sell, Price is inert.
pub fn side_of(col: Col) -> Option<i32> {
    match col {
        Col::Buy | Col::Bid => Some(1),
        Col::Ask | Col::Sell => Some(-1),
        Col::Price => None,
    }
}

/// Limit or stop for a click at `price`: a buy above the best ask, or a sell below the best bid,
/// would cross, so it is a stop; otherwise a limit.
pub fn click_type(side: i32, price: f64, best_bid: f64, best_ask: f64) -> OrderType {
    let stop = if side > 0 { price > best_ask } else { price < best_bid };
    if stop { OrderType::Stop } else { OrderType::Limit }
}

/// A price on the tick, with thousands grouped: `65,432.5`, `0.5231`, `0.00001234`. A price that
/// is not a number prints a dash, never `NaN` or `inf`.
pub fn fmt_px(px: f64, tick: f64) -> String {
    if px.is_finite() {
        vike_ui_theme::fmt::fmt_thousands_prec(px, sizing::decimals_of(tick))
    } else {
        NO_PRICE.to_string()
    }
}

/// What [`fmt_px`] prints for a price that is not a number.
pub const NO_PRICE: &str = "—";

/// The tick the rows are keyed on: the catalog's, else the book's own, else one.
///
/// ⚠ Never a tick of 0 and never a vanishing one. The DOM clamped its tick to `f64::MIN_POSITIVE`,
/// which only moves the division by zero: every price over that tick saturates its key at
/// `i64::MAX`, and the row arithmetic above the centre key then overflows. A catalog with no row
/// for the symbol (a grid of zeros) falls back to the book's tick here; and a lot of 0 refuses
/// every size (`sizing::base_qty`), so a click on such a ladder places nothing.
fn row_tick(grid_tick: f64, book_tick: f64) -> f64 {
    [grid_tick, book_tick].into_iter().find(|t| *t > 0.0 && t.is_finite()).unwrap_or(1.0)
}

// ---------------------------------------------------------------------------
// MOVED from the DOM (`vike_panels::dom`, deleted with this PR): these are the only copies.
// ---------------------------------------------------------------------------

/// Quantize a price to its grouped row key: the tick index divided (floor) by the group size.
/// A row key spans `group` ticks; `key_price` is its inverse (the row's aligned low edge).
pub fn row_key(price: f64, tick: f64, group: i64) -> i64 {
    let g = group.max(1);
    let ti = (price / tick).round() as i64;
    ti.div_euclid(g)
}

/// The price a row key stands for: its aligned low edge, `key · group · tick` ([`row_key`]'s
/// inverse).
pub fn key_price(key: i64, tick: f64, group: i64) -> f64 {
    let g = group.max(1);
    key as f64 * g as f64 * tick
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

/// Aggregate raw `(price, qty)` levels into grouped rows keyed by [`row_key`], summing qty.
/// Returns a map of `row_key → total_qty` — the render/hit-test view of one book side.
fn group_book(levels: &[BookLevel], tick: f64, group: i64) -> std::collections::HashMap<i64, f64> {
    let mut m: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    for &BookLevel { price: px, qty } in levels {
        *m.entry(row_key(px, tick, group)).or_insert(0.0) += qty;
    }
    m
}

/// The next coarser grouping in [`GROUP_STEPS`]; the coarsest stays where it is.
pub fn next_group(g: i64) -> i64 {
    GROUP_STEPS.iter().copied().find(|&s| s > g).unwrap_or(g)
}

/// The next finer grouping in [`GROUP_STEPS`]; one tick per row stays where it is.
pub fn prev_group(g: i64) -> i64 {
    GROUP_STEPS.iter().rev().copied().find(|&s| s < g).unwrap_or(g)
}

/// Coin-qty label with size-tiered decimals. KEPT local, not `vike_ui_theme::fmt::fmt_compact`:
/// a DOM qty needs sub-unit precision ("0.0050"), and large sizes stay plain digits ("1234",
/// never "1.23K") — pinned in the tests.
pub fn fmt_qty(q: f64) -> String {
    let a = q.abs();
    if a >= 1000.0 {
        format!("{:.0}", q)
    } else if a >= 1.0 {
        format!("{:.2}", q)
    } else {
        format!("{:.4}", q)
    }
}

/// Whether the ladder may begin a drag-to-reprice on an own resting order for the given venue caps —
/// the modify-gate the widget consults at the drag-start, the drop, AND the marker affordance.
/// Extracted so the gate logic is unit-testable without an egui frame (audit br6).
#[inline]
fn drag_to_reprice_allowed(caps: &VenueCaps) -> bool {
    caps.allows_modify()
}

/// How one resting-order marker is painted: `(fill, outline, letter)`.
///
/// A LIMIT order is filled in its side's colour and carries the on-fill letter. A STOP is hollow —
/// the background inside, its side's colour around it and on its letter — where it used to be the
/// trading palette's amber (owner decision 2). The outline also says whether the marker can be
/// dragged (audit br6): the text colour (a stop's: its side's colour) when this venue can reprice,
/// the caption grey when it cannot. Dragging rings either kind in the accent — the focus ring, one
/// of the accent's shapes (spec §2).
fn marker_colours(
    side: i32,
    is_stop: bool,
    draggable: bool,
    dragging: bool,
    t: &Tokens,
) -> (Color32, Stroke, Color32) {
    let (graphic, text) = if side > 0 {
        (t.market.up, t.market.up_text)
    } else {
        (t.market.down, t.market.down_text)
    };
    let (fill, edge, letter) =
        if is_stop { (t.theme.bg, graphic, text) } else { (graphic, t.theme.text, ON_FILL) };
    let edge = if draggable { edge } else { t.theme.text3 };
    let outline = if dragging { Stroke::new(1.6, t.theme.accent) } else { Stroke::new(1.3, edge) };
    (fill, outline, letter)
}

/// The icon a bookless ladder shows above its words, from its OWN link's state — the kit's three
/// renderings (spec §4.2), all of them STILL (owner decision 5). While connecting there is none:
/// the kit draws "still asking" as a spinner, and a bookless ladder must not move. The words are
/// what a screen reader hears for the icon (`icons::named`).
fn absence_icon(link: ConnectionState, t: &Tokens) -> Option<(Icon, Color32, &'static str)> {
    use ConnectionState as C;
    match link {
        C::Connecting => None,
        C::Connected | C::Unknown => Some((icons::EMPTY, t.theme.text3, "empty")),
        C::Disconnected | C::Error => {
            Some((icons::UNREACHABLE, Status::Warning.color(), "unreachable"))
        }
    }
}

/// The whole of what a bookless ladder shows: no ladder at all, no rows, no bars, no interaction.
///
/// Rendered as real `label` widgets inside a child `Ui` rather than as `painter.text`, so every
/// line lands in the accessibility tree and the window's a11y tests can assert the words on a
/// GPU-less runner. A painted string is invisible to those tests, and an empty state nothing can
/// gate is one refactor away from becoming a fabricated ladder again.
fn no_book(
    ui: &mut egui::Ui,
    t: &Tokens,
    region: Rect,
    absence: Option<BookAbsence<'_>>,
    link: ConnectionState,
) {
    // An inset frame: unmistakably NOT the ladder's own painted grid.
    ui.painter().rect_stroke(
        region.shrink(6.0),
        CornerRadius::same(RADIUS),
        Stroke::new(1.0, t.theme.border),
        StrokeKind::Inside,
    );
    let a = absence.unwrap_or_default();
    let headline = if a.headline.is_empty() { NO_BOOK_HEADLINE } else { a.headline };
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(region.shrink(18.0))
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.y = t.metrics.gap;
    child.add_space((region.height() * 0.5 - 46.0).max(0.0));
    if let Some((icon, colour, words)) = absence_icon(link, t) {
        let glyph = icon.rich().size(t.text.px(TextRole::Heading)).color(colour);
        icons::named(child.label(glyph), words);
    }
    child.label(egui::RichText::new(headline).font(t.font(TextRole::Title)).color(t.theme.text));
    child.label(
        egui::RichText::new(NO_BOOK_SUBLINE).font(t.font(TextRole::Body)).color(t.theme.text2),
    );
    if !a.cause.is_empty() {
        // the link's own status line, read character by character: mono
        child.label(egui::RichText::new(a.cause).font(t.mono(TextRole::Body)).color(t.theme.text));
    }
    if !a.next_step.is_empty() {
        child.label(
            egui::RichText::new(a.next_step).font(t.font(TextRole::Body)).color(t.theme.text3),
        );
    }
}

/// The headline a bookless ladder falls back to when the caller supplied none.
pub const NO_BOOK_HEADLINE: &str = "NO ORDER BOOK";

/// The line that states, in so many words, that nothing is being drawn — the sentence whose
/// ABSENCE let a synthetic ladder pass for a real one for as long as it did.
pub const NO_BOOK_SUBLINE: &str =
    "nothing is drawn below: no depth has been received for this venue + symbol";

// ---------------------------------------------------------------------------
// The markers
// ---------------------------------------------------------------------------

/// What one marker can say, and how it is drawn.
#[derive(Clone, Debug, PartialEq)]
struct MarkerFace {
    /// The count FIRST, then the summed size: `×2 0.030`; one order is its size alone, `0.010`.
    full: String,
    /// The count alone, `×2` (`×1` for one order): what the marker says where `full` does not fit.
    count: String,
    /// Hollow: only when every order it stands for is a stop.
    stop: bool,
}

/// The face of the marker standing for `own`. The size prints to the lot's decimals — an order is
/// a lot multiple, and the DOM's fixed four decimals printed an order under 0.00005 as `0.0000` —
/// or, while the lot is not known, to the size's own.
fn marker_face(own: &[&LadderOrder], lot: f64) -> MarkerFace {
    let qty: f64 = own.iter().map(|o| o.qty).sum();
    let size = sizing::qty_text(qty, lot);
    let count = format!("×{}", own.len());
    let full = if own.len() > 1 { format!("{count} {size}") } else { size };
    MarkerFace { full, count, stop: !own.is_empty() && own.iter().all(|o| o.is_stop) }
}

/// The words a marker shows in `room` points, given each one's width: the count and the size where
/// they fit, else the count alone. ⚠ The COUNT is never what gives way, because a click on the
/// marker cancels EVERY order it stands for; and no text is ever cut — where not even the count
/// fits, the marker is drawn with no words rather than with half a glyph.
fn marker_words(face: &MarkerFace, room: f32, width: impl Fn(&str) -> f32) -> Option<&str> {
    [face.full.as_str(), face.count.as_str()].into_iter().find(|w| width(w) <= room)
}

/// What hovering the marker standing for `own` lists: each order, its side, its type, its size
/// and its price, then their summed size (the owner's decision of 2026-10-03, card 1). A marker
/// at the shipped column widths shows only its count where it stands for several orders, and its
/// click cancels every one of them: the hover is where the trader reads what they are.
fn marker_lines(own: &[&LadderOrder], tick: f64, lot: f64, base: &str) -> Vec<String> {
    let mut lines: Vec<String> = own
        .iter()
        .map(|o| {
            let side = if o.side > 0 { "Buy" } else { "Sell" };
            let kind = if o.is_stop { "stop" } else { "limit" };
            let size = sizing::qty_text(o.qty, lot);
            format!("{side} {kind} {size} @ {}", fmt_px(o.price, tick))
        })
        .collect();
    let total: f64 = own.iter().map(|o| o.qty).sum();
    let orders = if own.len() == 1 { "order" } else { "orders" };
    lines.push(format!("Total: {} {base} in {} {orders}", sizing::qty_text(total, lot), own.len()));
    lines
}

/// How far a marker sits inside its cell above and below, and how far its WORDS keep from the
/// cell's sides ([`marker_room`]). Across, the marker itself is its words and `m.pad` each side, at
/// most the cell's width — so at the shipped column widths a marker with words spans the whole cell
/// ([`paint_marker`]), and its words, never cut, decide what it says ([`marker_words`]).
const MARKER_INSET: f32 = 3.0;

/// The width a marker's words may take in `cell`.
fn marker_room(cell: Rect) -> f32 {
    cell.width() - 2.0 * MARKER_INSET
}

/// Paint the marker with `face` in `cell`: its words ([`marker_words`]) plus `m.pad` on each side,
/// centred, and never wider than the cell.
fn paint_marker(
    painter: &egui::Painter,
    t: &Tokens,
    cell: Rect,
    side: i32,
    face: &MarkerFace,
    draggable: bool,
    dragging: bool,
) {
    let (fill, outline, ink) = marker_colours(side, face.stop, draggable, dragging, t);
    let layout = |w: &str| painter.layout_no_wrap(w.to_string(), t.mono(TextRole::Caption), ink);
    let galley = marker_words(face, marker_room(cell), |w| layout(w).size().x).map(layout);
    let text_w = galley.as_ref().map_or(0.0, |g| g.size().x);
    let w = (text_w + 2.0 * t.metrics.pad).min(cell.width());
    let mrect = Rect::from_center_size(cell.center(), vec2(w, cell.height() - 2.0 * MARKER_INSET));
    let ring = if dragging { mrect.expand(1.5) } else { mrect };
    painter.rect_filled(mrect, CornerRadius::same(RADIUS), fill);
    painter.rect_stroke(ring, CornerRadius::same(RADIUS), outline, StrokeKind::Middle);
    if let Some(g) = galley {
        painter.galley(mrect.center() - g.size() / 2.0, g, ink);
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The gap between the toolbar's edge and its controls (the DOM's `STRIP_INSET`).
const INSET: f32 = 1.0;

/// Where the ladder's pieces sit in the rect it is given, top to bottom. One function, so the
/// painting, the hit-testing and the tests cannot disagree about a row's position.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Parts {
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
struct Drawn {
    top_key: i64,
    tick: f64,
    group: i64,
    /// Every own-order marker these rows showed.
    marks: Vec<Mark>,
    /// What the button went down on, from the press to the release; `None` while it is up.
    press: Option<Press>,
}

/// One drawn marker: its row's key, its side, and the orders it stood for.
#[derive(Clone, Debug)]
struct Mark {
    key: i64,
    side: i32,
    coids: Vec<String>,
}

/// What the button went down on — the cell, and the orders the marker in it stood for (none for a
/// cell with no marker) — decided ONCE, in the frame it went down, against the rows and the
/// markers as they were drawn (final review A, I-1); and whether the book was stale then.
#[derive(Clone, Debug)]
struct Press {
    key: i64,
    col: Col,
    coids: Vec<String>,
    /// The book was stale when the button went down: the rows the trader pressed were drawn off
    /// a frozen book, and stay where they were drawn until the release, so a click or a drag that
    /// began there places and moves nothing, however live the book is by the release (the FW1
    /// review; the owner's decision of 2026-10-03, round 2, item 2).
    stale: bool,
}

fn parts(area: Rect, m: &Metrics, hint: bool) -> Parts {
    let toolbar = Rect::from_min_size(area.min, vec2(area.width(), m.control_h + 2.0 * INSET));
    let head = Rect::from_min_size(pos2(area.min.x, toolbar.max.y), vec2(area.width(), m.row_h));
    let hint = hint.then(|| {
        Rect::from_min_max(pos2(area.min.x, (area.max.y - m.row_h).max(head.max.y)), area.max)
    });
    let bottom = hint.map_or(area.max.y, |h| h.min.y).max(head.max.y);
    Parts {
        toolbar,
        head,
        rows: Rect::from_min_max(head.left_bottom(), pos2(area.max.x, bottom)),
        hint,
    }
}

/// Draw the ladder into `ui` (its region). Every intent leaves through `actions`; a new order
/// leaves through [`TradeState::submit`], so one-click off holds it for a confirm.
pub fn draw(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let tick = row_tick(inputs.grid.tick, inputs.book.tick_size);
    let area = ui.available_rect_before_wrap();
    // ⚠ Computed FIRST, because it decides what may be drawn at all.
    let bookless = inputs.book.bid_levels() + inputs.book.ask_levels() == 0;
    let p = parts(area, &t.metrics, state.view.panel == Panel::Beside && !bookless);

    toolbar(ui, t, state, p.toolbar, inputs.stale && !bookless);

    if bookless {
        // Nothing ladder-shaped: no captions, no rows, no hint, no click region. A drag that was
        // under way when the book went has nothing left to drop onto.
        state.drag = None;
        let region = Rect::from_min_max(p.toolbar.left_bottom(), area.max);
        ui.painter().rect_filled(region, 0.0, t.theme.bg);
        // The link as the FEED badge reads it: an empty source is one nobody reported on.
        no_book(ui, t, region, inputs.absence, super::instrument::link_of(inputs.source));
        return;
    }

    for (col, words) in COLS.iter().zip(CAPTIONS) {
        ui.painter().text(
            col_rect(*col, p.head).center(),
            Align2::CENTER_CENTER,
            words,
            t.font(TextRole::Caption),
            t.theme.text3,
        );
    }
    ui.painter().rect_filled(p.rows, 0.0, t.theme.bg);
    rows(ui, t, state, inputs, actions, p.rows, tick);
    if let Some(r) = p.hint {
        hint_line(ui, t, r, inputs);
    }
}

/// The toolbar above the rows: price grouping, recenter, and STALE when the book is.
fn toolbar(ui: &mut egui::Ui, t: &Tokens, state: &mut TradeState, rect: Rect, stale: bool) {
    let m = t.metrics;
    let mut bar = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(m.pad, INSET)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    bar.shrink_clip_rect(rect);
    bar.spacing_mut().item_spacing.x = m.gap;
    // The word `Group` goes first where the row is short of room (a ladder at its narrowest, a
    // loose look, STALE showing): the − and + say what they do on hover, and STALE must never be
    // the one pushed out of the ladder (MEASURED by the fit sweep: into the ticket beside it).
    let text_w = |text: &str, font: egui::FontId| {
        bar.painter().layout_no_wrap(text.to_string(), font, t.theme.text).size().x
    };
    let group = state.group.to_string();
    // −, +, Recenter and the grouping, a gap apart; STALE at least as wide as the kit's badge.
    let mut need = 3.0 * m.control_h + text_w(&group, t.mono(TextRole::Body)) + 3.0 * m.gap;
    if stale {
        need += text_w("STALE", t.mono(TextRole::Caption)) + 2.0 * m.pad + m.gap;
    }
    let word = text_w("Group", t.font(TextRole::Caption)) + m.gap;
    if need + word <= bar.available_width() {
        bar.label(RichText::new("Group").font(t.font(TextRole::Caption)).color(t.theme.text3));
    }
    // Keyed ([`super::keyed`]): the word `Group` comes and goes with the room STALE takes, and a
    // press held across it was released on the button that took the pressed one's place (W4 fix
    // round 2, the NEW-1 audit).
    let key = ui.id().with("trade_toolbar");
    let less = IconButton::new(icons::DECREASE, "Less price grouping");
    if super::keyed(&mut bar, key.with("group_less"), less).clicked() {
        state.group = prev_group(state.group);
    }
    bar.label(RichText::new(group).font(t.mono(TextRole::Body)).color(t.theme.text));
    let more = IconButton::new(icons::INCREASE, "More price grouping");
    if super::keyed(&mut bar, key.with("group_more"), more).clicked() {
        state.group = next_group(state.group);
    }
    // The rows follow the book's mid until scrolled; this un-latches them.
    let recenter = IconButton::new(icons::RECENTER, "Recenter on the market");
    if super::keyed(&mut bar, key.with("recenter"), recenter).clicked() {
        state.center = None;
    }
    if stale {
        chip::badge(&mut bar, "STALE", Status::Warning);
    }
}

/// The line under the rows in the side layout: how a click trades; where the account does not
/// trade the symbol, why a click does nothing (spec §4.3); and while the book is stale, that a
/// click places nothing and a marker still cancels.
fn hint_line(ui: &mut egui::Ui, t: &Tokens, rect: Rect, inputs: &TradeInputs<'_>) {
    let markers = inputs.orders_why.is_none();
    let words = match super::why_untradable(inputs) {
        Some(why) => why,
        None if inputs.stale && markers => HINT_STALE.to_string(),
        None if inputs.stale => HINT_STALE_NO_MARKERS.to_string(),
        None if markers => HINT.to_string(),
        None => HINT_NO_MARKERS.to_string(),
    };
    let mut line = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(t.metrics.pad, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    line.shrink_clip_rect(rect);
    line.add(
        egui::Label::new(RichText::new(words).font(t.font(TextRole::Caption)).color(t.theme.text3))
            .truncate(),
    );
}

/// The rows and everything they take: the painting, then the click grammar and the scroll wheel.
fn rows(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
    region: Rect,
    tick: f64,
) {
    let row_h = t.metrics.row_h;
    let best_bid = inputs.book.best_bid().map(|BookLevel { price: p, .. }| p);
    let best_ask = inputs.book.best_ask().map(|BookLevel { price: p, .. }| p);
    // `last` drives the outlined row (the window's price: the book's mid on the desktop, where no
    // last trade price arrives); the ladder CENTERS on the live book's mid so the real depth is
    // always in view even when a last-trade mark would sit a few ticks off the top of book. Fall
    // back to `last` when the book has no two-sided top.
    let last_opt = inputs.last.or_else(|| inputs.book.mid()).or(best_bid).or(best_ask);
    let last = last_opt.unwrap_or(0.0);
    let book_center = inputs.book.mid().or(best_bid).or(best_ask).unwrap_or(last);

    let n_rows = (region.height() / row_h).floor().max(1.0) as usize;
    let half = (n_rows / 2) as i64;
    let group = state.group;

    // The click region, sensed BEFORE the rows are laid out, because whether the button is down on
    // it decides where the rows are.
    let id = ui.id().with("trade_ladder");
    let resp = ui.interact(region, id, Sense::click_and_drag());

    // The top row's key: from the manual latch (scroll), else the live book's mid. The row
    // arithmetic saturates: a key is a tick index, and a price far off its tick must not overflow.
    //
    // ⚠ While the button is down on the ladder — from the press to the release that makes it a
    // click or ends a drag — the rows stay where they were DRAWN when it went down. A click is
    // resolved at the release, and re-centring on a mid that moved in between would trade the row
    // that is under the pointer THEN, not the price the trader saw under it at the press.
    let center = state.center.unwrap_or(book_center);
    let fresh = row_key(center, tick, group).saturating_add(half);
    let held = resp.is_pointer_button_down_on() || resp.clicked() || resp.drag_stopped();
    // What the frame before drew, on this grid.
    let before =
        ui.data(|d| d.get_temp::<Drawn>(id)).filter(|d| d.tick == tick && d.group == group);
    let top_key = before.as_ref().filter(|_| held).map_or(fresh, |d| d.top_key);
    let key_of = |i: usize| top_key.saturating_sub(i as i64);
    let row_at = |y: f32| -> i64 {
        let idx = ((y - region.min.y) / row_h).floor().clamp(0.0, (n_rows - 1) as f32);
        key_of(idx as usize)
    };

    let (bids, asks) = inputs.book.top_n(400);
    let bid_map = group_book(&bids, tick, group);
    let ask_map = group_book(&asks, tick, group);

    // Peak grouped qty over the visible window → depth-bar scale.
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

    let last_key = row_key(last, tick, group);
    let pos_key = inputs.position.filter(|p| p.size != 0.0).map(|p| row_key(p.avg_px, tick, group));

    // ⚠ Own orders the node does not attribute to an account are not drawn and cannot be hit
    // (Ruling R9): a marker there could cancel an order of another account.
    let own: &[LadderOrder] = if inputs.orders_why.is_some() { &[] } else { inputs.orders };
    let own_at = |key: i64, side: i32| -> Vec<&LadderOrder> {
        own.iter().filter(|o| o.side == side && row_key(o.price, tick, group) == key).collect()
    };
    let coids = |orders: &[&LadderOrder]| -> Vec<String> {
        orders.iter().map(|o| o.client_order_id.clone()).collect()
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
            _ => ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()).map(|at| {
                let (key, col) = (row_at(at.y), col_at(at.x, region.min.x, region.width()));
                let shown = |side: i32| match &before {
                    Some(d) => d
                        .marks
                        .iter()
                        .find(|m| m.key == key && m.side == side)
                        .map_or_else(Vec::new, |m| m.coids.clone()),
                    None => coids(&own_at(key, side)),
                };
                let under = match col {
                    Col::Buy => shown(1),
                    Col::Sell => shown(-1),
                    Col::Bid | Col::Price | Col::Ask => Vec::new(),
                };
                Press { key, col, coids: under, stale: inputs.stale }
            }),
        }
    };

    // --- the rows ---
    let painter = ui.painter().clone();
    let draggable = drag_to_reprice_allowed(&inputs.caps);
    let mut marks = Vec::new();
    for i in 0..n_rows {
        let key = key_of(i);
        let top = region.min.y + i as f32 * row_h;
        let row = Rect::from_min_size(pos2(region.min.x, top), vec2(region.width(), row_h));
        let is_last = key == last_key;
        // The position's average-price row is the neutral hover fill; odd rows are the kit table's
        // zebra (spec §4.2). The LAST row is an accent OUTLINE, painted after its contents below —
        // the spec's "last-price outline", never a tinted fill.
        if pos_key == Some(key) {
            painter.rect_filled(row, 0.0, t.theme.hover);
        } else if i % 2 == 1 {
            painter.rect_filled(row, 0.0, t.theme.surface);
        }

        // Depth bars grow outward from the price column: the market set's depth fills, at the
        // DOM's own alphas (spec §3.2). The sizes are the set's TEXT colours.
        let bidq = bid_map.get(&key).copied().unwrap_or(0.0);
        let askq = ask_map.get(&key).copied().unwrap_or(0.0);
        let bidcol = col_rect(Col::Bid, row);
        let askcol = col_rect(Col::Ask, row);
        if bidq > 0.0 {
            let w = (bidq / vmax) as f32 * bidcol.width();
            let bar = Rect::from_min_max(
                pos2(bidcol.max.x - w, top + 1.0),
                pos2(bidcol.max.x, top + row_h - 1.0),
            );
            painter.rect_filled(bar, 0.0, t.market.up_depth);
            painter.text(
                pos2(bidcol.max.x - 4.0, row.center().y),
                Align2::RIGHT_CENTER,
                fmt_qty(bidq),
                t.mono(TextRole::Body),
                t.market.up_text,
            );
        }
        if askq > 0.0 {
            let w = (askq / vmax) as f32 * askcol.width();
            let bar = Rect::from_min_max(
                pos2(askcol.min.x, top + 1.0),
                pos2(askcol.min.x + w, top + row_h - 1.0),
            );
            painter.rect_filled(bar, 0.0, t.market.down_depth);
            painter.text(
                pos2(askcol.min.x + 4.0, row.center().y),
                Align2::LEFT_CENTER,
                fmt_qty(askq),
                t.mono(TextRole::Body),
                t.market.down_text,
            );
        }
        // The price column: the last price in the text colour, every other rung secondary. A
        // number is never the accent (spec §2). A row that names no price draws none.
        let pcol = if is_last { t.theme.text } else { t.theme.text2 };
        if let Some(price) = named_price(key, tick, group) {
            painter.text(
                col_rect(Col::Price, row).center(),
                Align2::CENTER_CENTER,
                fmt_px(price, tick),
                t.mono(TextRole::Body),
                pcol,
            );
        }

        // ONE marker per side, standing for every own order on this row and side.
        for (side, col) in [(1, Col::Buy), (-1, Col::Sell)] {
            let here = own_at(key, side);
            if !here.is_empty() {
                let face = marker_face(&here, inputs.grid.lot);
                let dragging = state
                    .drag
                    .as_ref()
                    .is_some_and(|d| here.iter().any(|o| d.contains(&o.client_order_id)));
                let cell = col_rect(col, row);
                paint_marker(&painter, t, cell, side, &face, draggable, dragging);
                marks.push(Mark { key, side, coids: coids(&here) });
            }
        }
        if is_last {
            painter.rect_stroke(row, 0.0, Stroke::new(1.0, t.theme.accent), StrokeKind::Inside);
        }
    }
    let drawn = Drawn { top_key, tick, group, marks, press: press.clone() };
    ui.data_mut(|d| d.insert_temp(id, drawn));

    // The spread line at the last/mid boundary: the analysis line, the one neutral between plus
    // and minus (spec §3.2).
    let spread_y = region.min.y + top_key.saturating_sub(last_key) as f32 * row_h + row_h;
    if spread_y > region.min.y && spread_y < region.max.y {
        painter.line_segment(
            [pos2(region.min.x, spread_y), pos2(region.max.x, spread_y)],
            Stroke::new(1.0, t.theme.analysis_line),
        );
    }

    // stale overlay: dim the whole book region into the theme, so a frozen book never reads as live
    if inputs.stale {
        painter.rect_filled(region, 0.0, t.theme.scrim());
    }

    // --- the click grammar, over the rows as they were DRAWN (`top_key` above) ---
    // A drag this ladder did not see end must never reprice later.
    if !(resp.dragged() || resp.drag_stopped()) {
        state.drag = None;
    }
    let tradable = matches!(inputs.tradable, Tradable::Yes);
    if let Some(why) = super::why_untradable(inputs) {
        // Nothing here may produce an order (Review Focus 2); the hover says why.
        state.drag = None;
        let _ = resp.clone().on_hover_text(why);
    }
    // ⚠ A STALE book places no order and moves none from here: limit or stop is chosen against a
    // top of book that may be long gone, and a drop prices an order off it. A marker still
    // cancels — a cancel takes risk away, and names its orders, not a price. The hint line says
    // so, and a refused gesture says so on the strip (the owner's decision of 2026-10-03, round 2,
    // item 2).
    let stale = || TradeAction::Note { kind: StatusKind::Error, text: STALE_WHY.to_string() };
    if tradable && let Some(pos) = resp.interact_pointer_pos() {
        // A drag begins where the button went DOWN, not where the pointer is once it has moved far
        // enough to count as one, and only on a marker, on a venue that can modify (audit br6). It
        // takes the orders the marker under the press stood for ([`Press`]).
        if resp.drag_started()
            && drag_to_reprice_allowed(&inputs.caps)
            && let Some(p) = &press
            && !p.coids.is_empty()
        {
            if inputs.stale || p.stale {
                actions.push(stale());
            } else {
                state.drag = Some(p.coids.clone());
            }
        }
        // The drop: every dragged order not already on the drop row moves to it. Belt-and-braces
        // re-check of the gate, so a drag left dangling by a venue switch never emits a Modify. A
        // drop OUTSIDE the rows moves nothing: `row_at` would clamp it onto the edge row, a price
        // the pointer was never on. ⚠ Nor does an Escape: egui ends a drag on Escape as a
        // `drag_stopped` with the pointer wherever it is, which is a cancel, not a drop there. And
        // a row that names no price ([`named_price`]) takes no drop.
        let key = row_at(pos.y);
        if resp.drag_stopped()
            && let Some(coids) = state.drag.take()
            && drag_to_reprice_allowed(&inputs.caps)
            && region.contains(pos)
            && !ui.input(|i| i.key_pressed(egui::Key::Escape))
            && let Some(new_price) = named_price(key, tick, group)
        {
            // Only the dragged orders still working NOW: one that filled or was cancelled while
            // the button was down is sent nothing (the node would refuse it, and the strip would
            // show a rejection for an order the trader no longer sees). With none left, nothing
            // at all (the FW1 review).
            let working = |coid: &String| own.iter().any(|o| &o.client_order_id == coid);
            let stays = |coid: &String| {
                own.iter()
                    .any(|o| &o.client_order_id == coid && row_key(o.price, tick, group) == key)
            };
            if inputs.stale {
                // The book went stale while the drag was under way.
                actions.push(stale());
            } else {
                actions.extend(
                    coids
                        .into_iter()
                        .filter(|c| working(c) && !stays(c))
                        .map(|coid| TradeAction::Modify { coid, new_price }),
                );
            }
        }
        // A click trades the cell the button went DOWN on ([`Press`]): a marker there was a cancel
        // of the orders it stood for, whatever became of them since — NEVER an order — and an empty
        // cell an order, even where a marker has appeared since.
        if resp.clicked()
            && state.drag.is_none()
            && let Some(Press { key, col, coids, stale: stale_at_press }) = press.clone()
            && let Some(side) = side_of(col)
        {
            if !coids.is_empty() {
                actions.push(TradeAction::Cancel(coids));
            } else if inputs.stale || stale_at_press {
                // Stale at the release, or at the press: the row was chosen off a frozen book.
                actions.push(stale());
            } else if let Some(price) = named_price(key, tick, group) {
                // The order's own price FIRST: a size typed in the quote currency converts at the
                // price the order is placed at, as the ticket's does — not at the last price, which
                // buys more than the amount entered above it and nothing at all while there is none.
                // A row that names no price takes no order.
                // The TICKET's size rule (`ticket::checked_qty`): at least one lot, a finite count
                // of them, and no less than the smallest order. A click it refuses sends nothing
                // and says why on the strip, as a disabled ticket button says why on hover.
                let qty = sizing::base_qty(&state.size, state.unit, Some(price), inputs.grid.lot);
                match super::ticket::checked_qty(qty, state, inputs, Some(price)) {
                    Ok(qty) => {
                        let order_type = click_type(
                            side,
                            price,
                            best_bid.unwrap_or(price),
                            best_ask.unwrap_or(price),
                        );
                        state.submit(
                            TradeAction::Place {
                                side,
                                order_type,
                                price: Some(price),
                                qty,
                                reduce_only: state.reduce_only,
                                exits: None,
                                origin: super::Origin::Ladder,
                            },
                            actions,
                        );
                    }
                    Err(why) => {
                        actions.push(TradeAction::Note { kind: StatusKind::Error, text: why })
                    }
                }
            }
        }
    }

    // The marker under the pointer lists the orders it stands for and their summed size, read off
    // this frame's orders as the marker itself is (the owner's decision of 2026-10-03, card 1).
    let hovered_side = |at: Pos2| match col_at(at.x, region.min.x, region.width()) {
        Col::Buy => Some(1),
        Col::Sell => Some(-1),
        Col::Bid | Col::Price | Col::Ask => None,
    };
    if state.drag.is_none()
        && let Some(at) = resp.hover_pos()
        && let Some(side) = hovered_side(at)
    {
        let here = own_at(row_at(at.y), side);
        if !here.is_empty() {
            let lines = marker_lines(&here, tick, inputs.grid.lot, inputs.base);
            let _ = resp.clone().on_hover_ui_at_pointer(|ui| {
                for line in lines {
                    let words = RichText::new(line).font(t.mono(TextRole::Caption));
                    ui.label(words.color(t.theme.text));
                }
            });
        }
    }

    // The scroll wheel nudges the centre and latches it; Recenter un-latches it.
    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
    if resp.hovered() && scroll.abs() > 0.5 {
        let step = key_price(1, tick, group);
        let base = state.center.unwrap_or(book_center);
        state.center = Some(base + (scroll.signum() as f64) * step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::{AccountMode, Grid, Origin, SizeUnit, View, layout};
    use egui::accesskit::Role;
    use egui::{Event, PointerButton, Pos2};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use vike_model::L2Book;
    use vike_ui_theme::appearance::Appearance;
    use vike_ui_theme::metrics::Density;
    use vike_ui_theme::type_scale::TextSize;

    #[test]
    fn the_five_columns_split_the_width_in_order() {
        let cols: Vec<Col> = (0..100).map(|x| col_at(x as f32 * 3.4, 0.0, 340.0)).collect();
        let mut order = cols.clone();
        order.dedup();
        assert_eq!(order, vec![Col::Buy, Col::Bid, Col::Price, Col::Ask, Col::Sell]);
        // A pointer past either edge (a drag that left the ladder) is the edge column, not none.
        assert_eq!(col_at(-50.0, 0.0, 340.0), Col::Buy);
        assert_eq!(col_at(9_999.0, 0.0, 340.0), Col::Sell);
    }

    /// The rect a column is drawn in is the span [`col_at`] hit-tests it over: what a trader sees
    /// in a cell is what a click there trades.
    #[test]
    fn each_column_is_drawn_where_it_is_hit_tested() {
        let row = Rect::from_min_size(pos2(10.0, 0.0), vec2(340.0, 18.0));
        for col in [Col::Buy, Col::Bid, Col::Price, Col::Ask, Col::Sell] {
            let r = col_rect(col, row);
            for x in [r.min.x + 0.5, r.center().x, r.max.x - 0.5] {
                assert_eq!(col_at(x, row.min.x, row.width()), col, "{col:?} at {x}");
            }
        }
        assert!((col_rect(Col::Sell, row).max.x - row.max.x).abs() < 1e-3, "the columns fill it");
    }

    #[test]
    fn buy_and_bid_buy_ask_and_sell_sell_price_is_inert() {
        assert_eq!(side_of(Col::Buy), Some(1));
        assert_eq!(side_of(Col::Bid), Some(1));
        assert_eq!(side_of(Col::Price), None);
        assert_eq!(side_of(Col::Ask), Some(-1));
        assert_eq!(side_of(Col::Sell), Some(-1));
    }

    /// A buy above the best ask, or a sell below the best bid, is a stop; at or inside the market,
    /// a limit (spec §3.4). No modifier key forces a stop any more.
    #[test]
    fn a_click_resolves_limit_or_stop_by_the_market() {
        assert_eq!(click_type(1, 101.0, 99.0, 100.0), OrderType::Stop);
        assert_eq!(click_type(1, 100.0, 99.0, 100.0), OrderType::Limit);
        assert_eq!(click_type(-1, 98.0, 99.0, 100.0), OrderType::Stop);
        assert_eq!(click_type(-1, 99.0, 99.0, 100.0), OrderType::Limit);
    }

    /// Prices print exactly on the tick at both ends of the range (Review Focus 3): a seven-digit
    /// price grouped, a sub-satoshi tick to its last digit.
    #[test]
    fn prices_print_on_the_tick_with_thousands_grouped() {
        assert_eq!(fmt_px(65_432.5, 0.1), "65,432.5");
        assert_eq!(fmt_px(0.5231, 0.0001), "0.5231");
        assert_eq!(fmt_px(0.000_012_34, 1e-8), "0.00001234");
        assert_eq!(fmt_px(123.0, 1.0), "123");
        assert_eq!(fmt_px(1_234_567.0, 0.5), "1,234,567.0");
        assert_eq!(fmt_px(0.000_000_000_12, 1e-11), "0.00000000012");
        for px in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(fmt_px(px, 0.1), NO_PRICE, "{px}");
        }
    }

    #[test]
    fn row_keys_group_ticks_and_invert() {
        assert_eq!(row_key(100.3, 0.1, 5), row_key(100.1, 0.1, 5));
        let k = row_key(100.3, 0.1, 5);
        assert!((key_price(k, 0.1, 5) - 100.0).abs() < 1e-9);
        // A price between two ticks keys on the NEAREST one.
        assert_eq!(row_key(100.04, 0.1, 1), row_key(100.0, 0.1, 1));
        assert_eq!(row_key(100.06, 0.1, 1), row_key(100.1, 0.1, 1));
    }

    #[test]
    fn grouping_steps_up_and_down_the_roster() {
        assert_eq!(next_group(1), 2);
        assert_eq!(next_group(5), 10);
        assert_eq!(next_group(25), 50);
        assert_eq!(next_group(50), 50);
        assert_eq!(prev_group(10), 5);
        assert_eq!(prev_group(5), 2);
        assert_eq!(prev_group(1), 1);
    }

    /// The rows key on the catalog's tick, else on the book's own; a grid that knows no tick never
    /// divides a price by zero (pre-flight: a zero tick saturates every key and overflows the row
    /// arithmetic).
    #[test]
    fn the_rows_key_on_the_catalogs_tick_else_the_books() {
        assert_eq!(row_tick(0.1, 0.5), 0.1);
        assert_eq!(row_tick(0.0, 0.5), 0.5);
        assert_eq!(row_tick(f64::NAN, 0.5), 0.5);
        assert_eq!(row_tick(0.0, 0.0), 1.0);
    }

    // ---- MOVED from the DOM's tests with the items they pin (pre-flight Minor 8) ----

    #[test]
    fn group_book_sums_qty_into_rows() {
        let tick = 1.0;
        let levels: Vec<BookLevel> = vec![
            BookLevel::new(100.0, 1.0),
            BookLevel::new(101.0, 2.0),
            BookLevel::new(105.0, 4.0),
            BookLevel::new(109.0, 8.0),
        ];
        // group 10 → all four collapse into one row summing to 15
        let m = group_book(&levels, tick, 10);
        assert_eq!(m.len(), 1);
        let key = row_key(100.0, tick, 10);
        assert_eq!(m.get(&key).copied(), Some(15.0));
        // group 5 → {100,101} and {105,109} split into two rows
        let m5 = group_book(&levels, tick, 5);
        assert_eq!(m5.len(), 2);
        assert_eq!(m5.get(&row_key(100.0, tick, 5)).copied(), Some(3.0));
        assert_eq!(m5.get(&row_key(105.0, tick, 5)).copied(), Some(12.0));
    }

    /// Pins the rendered strings, INCLUDING the divergences that keep this formatter local
    /// instead of swapping to `vike_ui_theme::fmt::fmt_compact` (GUI audit F7 — a swap would
    /// change rendered text).
    #[test]
    fn fmt_qty_tiers_decimals_by_size() {
        // sub-unit qtys keep four decimals — fmt_compact would print "0"
        assert_eq!(fmt_qty(0.001), "0.0010");
        assert_eq!(fmt_qty(0.05), "0.0500");
        assert_eq!(fmt_qty(0.1), "0.1000");
        // unit-scale: two decimals; signed for position sizes
        assert_eq!(fmt_qty(2.5), "2.50");
        assert_eq!(fmt_qty(-2.5), "-2.50");
        // large: plain digits, never a K/M compaction — fmt_compact would print "1.23K"
        assert_eq!(fmt_qty(1000.0), "1000");
        assert_eq!(fmt_qty(1234.0), "1234");
    }

    /// The drag-to-reprice gate (audit br6): the ladder offers the drag ONLY when the venue's
    /// declared caps wire a native modify. A modify-less venue (or the unknown/default caps) blocks
    /// it — the exact condition that stops an unsupported `Command::Modify`.
    #[test]
    fn drag_to_reprice_gate_follows_caps() {
        assert!(drag_to_reprice_allowed(&vike_model::venues::venue_caps::BINANCE));
        assert!(drag_to_reprice_allowed(&vike_model::venues::venue_caps::BYBIT));
        assert!(drag_to_reprice_allowed(&vike_model::venues::venue_caps::OKX));
        assert!(!drag_to_reprice_allowed(&vike_model::venues::venue_caps::OANDA));
        assert!(!drag_to_reprice_allowed(&vike_model::venues::venue_caps::POLYMARKET));
        assert!(!drag_to_reprice_allowed(&VenueCaps::UNSUPPORTED));
    }

    /// A LIMIT marker is filled with its side's colour and carries the on-fill text; a STOP is
    /// hollow — the background inside, its side's colour around it and on its text (owner decision
    /// 2). A venue that cannot reprice greys the outline of both kinds (audit br6), and dragging
    /// rings either kind in the accent (spec §2: the focus ring).
    #[test]
    fn a_limit_marker_is_filled_and_a_stop_marker_is_hollow() {
        let t = Tokens::from_appearance(&Appearance::default());
        let (fill, edge, ink) = marker_colours(1, false, true, false, &t);
        assert_eq!((fill, edge.color, ink), (t.market.up, t.theme.text, ON_FILL));
        let (fill, edge, ink) = marker_colours(-1, true, true, false, &t);
        assert_eq!((fill, edge.color, ink), (t.theme.bg, t.market.down, t.market.down_text));
        for stop in [false, true] {
            assert_eq!(marker_colours(1, stop, false, false, &t).1.color, t.theme.text3, "{stop}");
            assert_eq!(marker_colours(1, stop, true, true, &t).1.color, t.theme.accent, "{stop}");
        }
    }

    /// The three bookless renderings (owner decision 5, spec §4.2): nothing while connecting — a
    /// bookless ladder does not move, so no spinner — the tray when the link is up or not known,
    /// the unreachable cloud in the warning colour when it is down.
    #[test]
    fn a_bookless_ladder_shows_no_icon_while_connecting_the_tray_when_up_and_the_cloud_when_down() {
        use vike_model::feed_status::ConnectionState as C;
        let t = Tokens::from_appearance(&Appearance::default());
        assert_eq!(absence_icon(C::Connecting, &t), None);
        for up in [C::Connected, C::Unknown] {
            assert_eq!(
                absence_icon(up, &t).map(|(i, c, _)| (i, c)),
                Some((icons::EMPTY, t.theme.text3))
            );
        }
        for down in [C::Disconnected, C::Error] {
            assert_eq!(
                absence_icon(down, &t).map(|(i, c, _)| (i, c)),
                Some((icons::UNREACHABLE, Status::Warning.color()))
            );
        }
    }

    /// A bookless ladder reads its OWN link through `instrument::link_of`, as the FEED badge does
    /// (W2 review minor 7): an empty source is a link nobody reported on, so it shows the tray —
    /// never the unreachable cloud, which would tell the trader a link is down that nobody said
    /// anything about.
    #[test]
    fn a_bookless_ladder_whose_link_nobody_reported_shows_the_tray_not_the_cloud() {
        let mut h = scene(Setup::default());
        h.state_mut().book = L2Book::new(1.0);
        h.run();
        assert!(h.query_by_label("empty").is_some(), "the tray");
        assert!(h.query_by_label("unreachable").is_none(), "not the cloud");
    }

    // ---- the markers ----

    fn order(coid: &str, side: i32, price: f64, qty: f64, is_stop: bool) -> LadderOrder {
        LadderOrder { client_order_id: coid.to_string(), side, price, qty, is_stop }
    }

    fn face(full: &str, count: &str, stop: bool) -> MarkerFace {
        MarkerFace { full: full.to_string(), count: count.to_string(), stop }
    }

    /// One marker stands for every own order on its row and side: the count FIRST, then the summed
    /// size to the lot's decimals. It is hollow only when every order it stands for is a stop.
    #[test]
    fn one_marker_says_how_many_orders_it_stands_for_then_their_size() {
        let (a, b, c) = (
            order("a", 1, 98.0, 0.01, false),
            order("b", 1, 98.0, 0.02, true),
            order("c", 1, 98.0, 0.01, true),
        );
        assert_eq!(marker_face(&[&a], 0.001), face("0.010", "×1", false));
        assert_eq!(marker_face(&[&a, &b], 0.001), face("×2 0.030", "×2", false), "mixed: filled");
        assert_eq!(marker_face(&[&b, &c], 0.001), face("×2 0.030", "×2", true), "stops: hollow");
        // Review fix 4: the DOM's fixed four decimals printed this order as `0.0000`.
        let tiny = order("t", 1, 98.0, 0.000_01, false);
        assert_eq!(marker_face(&[&tiny], 0.000_01).full, "0.00001");
        // The lot not known yet: the size's own decimals.
        assert_eq!(marker_face(&[&a, &b], 0.0).full, "×2 0.03");
    }

    /// Where the size does not fit, the marker keeps the COUNT — a click cancels every order it
    /// stands for — and where not even the count fits it says nothing rather than half of it.
    #[test]
    fn a_marker_that_cannot_hold_its_size_keeps_its_count() {
        let f = face("×2 0.030", "×2", false);
        let glyphs = |w: &str| w.chars().count() as f32 * 6.0;
        assert_eq!(marker_words(&f, 48.0, glyphs), Some("×2 0.030"));
        assert_eq!(marker_words(&f, 47.0, glyphs), Some("×2"));
        assert_eq!(marker_words(&f, 11.0, glyphs), None);
        let one = face("123.456", "×1", false);
        assert_eq!(marker_words(&one, 30.0, glyphs), Some("×1"), "one order's count, too");
    }

    /// The window's frame and body margin across the window, a bound above the desktop's
    /// (`crates/vike-app-core/src/ui/workspace/state.rs`'s `TOOL_BODY_MARGIN` is 8 a side).
    const WINDOW_SIDES: f32 = 20.0;

    /// Review fix 4, measured: at the sizes the window opens at, in every look up to the widest
    /// (Large text; Comfortable's wider gap narrows the ladder beside the ticket), the Buy column
    /// holds a two-digit count (`×99`) and one order's size (`0.010`) in the mono caption the
    /// marker is drawn in. Where a size does not fit beside its count, [`marker_words`] keeps the
    /// count.
    #[test]
    fn a_markers_count_and_a_single_size_fit_the_order_column_at_the_window_sizes() {
        const WORDS: [&str; 2] = ["×99", "0.010"];
        let mut h = Harness::builder().build_ui_state(
            |ui, widths: &mut Vec<(TextSize, &'static str, f32)>| {
                // Measured in the app's type: kittest's first frame has egui's default fonts.
                if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                    return;
                }
                widths.clear();
                for text_size in TextSize::ALL {
                    let a = Appearance { text_size, ..Appearance::default() };
                    let t = Tokens::from_appearance(&a);
                    for words in WORDS {
                        let font = t.mono(TextRole::Caption);
                        let g = ui.painter().layout_no_wrap(words.to_string(), font, t.theme.text);
                        widths.push((text_size, words, g.size().x));
                    }
                }
            },
            Vec::new(),
        );
        h.run();
        let widths = h.state().clone();
        assert_eq!(widths.len(), TextSize::ALL.len() * WORDS.len(), "measured: {widths:?}");
        for density in Density::ALL {
            let m = density.metrics();
            for (panel, size) in
                [(Panel::Beside, layout::BESIDE_SIZE), (Panel::Under, layout::UNDER_SIZE)]
            {
                let body = Rect::from_min_size(Pos2::ZERO, size - vec2(WINDOW_SIDES, 0.0));
                let view = View { ladder: true, panel };
                let ladder = layout::split(body, view, &m).ladder.expect("a ladder at this size");
                let room = marker_room(col_rect(Col::Buy, ladder));
                for (text_size, words, w) in &widths {
                    assert!(
                        *w <= room,
                        "{density:?} / {text_size:?} / {panel:?}: {words:?} is {w} wide, the \
                         order column holds {room}"
                    );
                }
            }
        }
    }

    // ---- the ladder on screen (kittest; the a11y tree and real pointer events) ----

    /// Bids `mid − 0.5` and two below, asks `mid + 0.5` and two above, on a 1.0 tick.
    fn book_at(mid: f64) -> L2Book {
        let (b, a) = (mid - 0.5, mid + 0.5);
        let mut book = L2Book::new(1.0);
        book.apply_snapshot(
            1,
            &[BookLevel::new(b, 1.0), BookLevel::new(b - 1.0, 2.0), BookLevel::new(b - 2.0, 3.0)],
            &[BookLevel::new(a, 1.0), BookLevel::new(a + 1.0, 2.0), BookLevel::new(a + 2.0, 3.0)],
        );
        book
    }

    /// Bids 99/98/97, asks 100/101/102: mid 99.5, so the rows centre on 100.
    const MID: f64 = 99.5;
    const GRID: Grid = Grid { tick: 1.0, lot: 0.001, min_qty: 0.001 };
    /// The size a synced window starts with on [`GRID`]: the middle quick size, ten lots.
    const SIZE: f64 = 0.01;

    /// What a scene varies; everything else is one account trading `BTCUSDT`.
    struct Setup {
        tradable: Tradable<'static>,
        orders: Vec<LadderOrder>,
        orders_why: Option<&'static str>,
        caps: VenueCaps,
        panel: Panel,
        mode: AccountMode,
        last: Option<f64>,
        grid: Grid,
    }

    impl Default for Setup {
        fn default() -> Self {
            Setup {
                tradable: Tradable::Yes,
                orders: Vec::new(),
                orders_why: None,
                caps: VenueCaps::UNSUPPORTED,
                panel: Panel::Beside,
                mode: AccountMode::Demo,
                last: Some(MID),
                grid: GRID,
            }
        }
    }

    struct Scene {
        /// The book the ladder draws: a test may move the market between frames.
        book: L2Book,
        /// The account's working orders: a test may fill or place one between frames.
        orders: Vec<LadderOrder>,
        state: TradeState,
        emitted: Vec<TradeAction>,
        /// The rect the ladder was given and the tokens it drew with, on the last frame.
        drawn: Option<(Rect, Tokens)>,
        /// Whether the book is stale: a test may flip it between frames, as the feed does.
        stale: bool,
    }

    /// The ladder alone, as `trade::draw` hands it its region: synced, then drawn.
    fn scene(setup: Setup) -> Harness<'static, Scene> {
        let mut state = TradeState::default();
        state.view.panel = setup.panel;
        let orders = setup.orders.clone();
        Harness::builder().with_size(egui::vec2(420.0, 560.0)).build_ui_state(
            move |ui, s: &mut Scene| {
                // The toolbar draws icons; their family is bound only by the app's type.
                if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                    return;
                }
                let t = Tokens::of(ui.ctx());
                let Scene { book, orders, state, emitted, drawn, stale } = s;
                let inputs = TradeInputs {
                    venue: "binance",
                    venue_label: "Test venue",
                    account: None,
                    symbol: "BTCUSDT",
                    base: "BTC",
                    quote: "USDT",
                    mode: setup.mode,
                    tradable: setup.tradable,
                    grid: setup.grid,
                    book,
                    last: setup.last,
                    stale: *stale,
                    source: "",
                    absence: None,
                    orders: orders.as_slice(),
                    orders_why: setup.orders_why,
                    position: None,
                    buying_power: None,
                    caps: setup.caps,
                    bracket_why: None,
                    bracket_wire: false,
                    matches: &[],
                    recent: &[],
                    accounts: &[],
                    accounts_why: None,
                    unconnected: &[],
                    status: None,
                };
                let mut acts = Vec::new();
                state.sync(&inputs, &mut acts);
                *drawn = Some((ui.available_rect_before_wrap(), t));
                draw(ui, &t, state, &inputs, &mut acts);
                emitted.extend(acts);
            },
            Scene {
                book: book_at(MID),
                orders,
                state,
                emitted: Vec::new(),
                drawn: None,
                stale: false,
            },
        )
    }

    /// A settled scene with nothing emitted yet.
    fn settled(setup: Setup) -> Harness<'static, Scene> {
        let mut h = scene(setup);
        h.run();
        h.state_mut().emitted.clear();
        h
    }

    /// The rows' rect, from the geometry the ladder drew with.
    fn rows_rect(h: &Harness<'static, Scene>) -> Rect {
        let (area, t) = h.state().drawn.expect("the ladder drew");
        parts(area, &t.metrics, h.state().state.view.panel == Panel::Beside).rows
    }

    /// The centre of `col`'s cell on the row holding `price`, from the geometry the ladder drew
    /// with (one tick per row, centred on the trader's own centre where one is set, else on
    /// [`MID`]).
    fn cell(h: &Harness<'static, Scene>, col: Col, price: f64) -> Pos2 {
        let row_h = h.state().drawn.expect("the ladder drew").1.metrics.row_h;
        let rows = rows_rect(h);
        let n_rows = (rows.height() / row_h).floor().max(1.0) as usize;
        let centre = h.state().state.center.unwrap_or(MID);
        let idx = row_key(centre, 1.0, 1) + (n_rows / 2) as i64 - row_key(price, 1.0, 1);
        let y = rows.min.y + (idx as f32 + 0.5) * row_h;
        pos2(col_rect(col, rows).center().x, y)
    }

    fn press(h: &Harness<'static, Scene>, pos: Pos2, pressed: bool) {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }

    /// A primary click at `pos`, and what it emitted.
    fn click_at(h: &mut Harness<'static, Scene>, pos: Pos2) -> Vec<TradeAction> {
        h.event(Event::PointerMoved(pos));
        press(h, pos, true);
        press(h, pos, false);
        h.run();
        std::mem::take(&mut h.state_mut().emitted)
    }

    /// A primary click on `col`'s cell of the row holding `price`, and what it emitted.
    fn click(h: &mut Harness<'static, Scene>, col: Col, price: f64) -> Vec<TradeAction> {
        let pos = cell(h, col, price);
        click_at(h, pos)
    }

    /// A primary drag from `from` to `to` over several frames, and what it emitted.
    fn drag_to(h: &mut Harness<'static, Scene>, from: Pos2, to: Pos2) -> Vec<TradeAction> {
        h.event(Event::PointerMoved(from));
        press(h, from, true);
        h.step();
        for k in 1..=4 {
            h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 4.0)));
            h.step();
        }
        press(h, to, false);
        h.run();
        std::mem::take(&mut h.state_mut().emitted)
    }

    /// A primary drag in `col` from the row holding `from` to the row holding `to`.
    fn drag(h: &mut Harness<'static, Scene>, col: Col, from: f64, to: f64) -> Vec<TradeAction> {
        let (from, to) = (cell(h, col, from), cell(h, col, to));
        drag_to(h, from, to)
    }

    /// Whether `got` is exactly one plain ladder order: `side`, `order_type` at `price`, `qty`, no
    /// reduce-only and no exits (a ladder click never carries TP/SL, spec §3.6).
    fn place_qty(
        side: i32,
        order_type: OrderType,
        price: f64,
        want: f64,
        got: &[TradeAction],
    ) -> bool {
        let [
            TradeAction::Place {
                side: s,
                order_type: o,
                price: p,
                qty,
                reduce_only,
                exits,
                origin,
            },
        ] = got
        else {
            return false;
        };
        (*s, *o, *p, *reduce_only, *exits, *origin)
            == (side, order_type, Some(price), false, None, Origin::Ladder)
            && (qty - want).abs() < 1e-12
    }

    /// [`place_qty`] at the size a synced window starts with.
    fn place(side: i32, order_type: OrderType, price: f64, got: &[TradeAction]) -> bool {
        place_qty(side, order_type, price, SIZE, got)
    }

    /// Review Focus 2's ladder half (pre-flight I8): a symbol the account does not trade takes no
    /// order from the ladder. ⚠ The POSITIVE CONTROL comes first: the same four cells of the same
    /// scene, tradable, each PLACE the order the click grammar names — so "nothing was emitted"
    /// below cannot be true merely because no click reached a row.
    #[test]
    fn an_untradable_symbol_ignores_ladder_clicks() {
        let cases = [
            (Col::Buy, 98.0, 1, OrderType::Limit),
            (Col::Bid, 101.0, 1, OrderType::Stop),
            (Col::Ask, 101.0, -1, OrderType::Limit),
            (Col::Sell, 98.0, -1, OrderType::Stop),
        ];
        let mut h = settled(Setup::default());
        for (col, price, side, kind) in cases {
            let got = click(&mut h, col, price);
            assert!(place(side, kind, price, &got), "CONTROL: {col:?} @ {price} emitted {got:?}");
        }
        assert!(click(&mut h, Col::Price, 98.0).is_empty(), "the price column is inert");

        let mut h = settled(Setup {
            tradable: Tradable::No { trades: &[], why: None },
            ..Setup::default()
        });
        for (col, price, _, _) in cases {
            let got = click(&mut h, col, price);
            assert!(got.is_empty(), "untradable: {col:?} @ {price} emitted {got:?}");
            assert_eq!(h.state().state.held, None, "and holds nothing for a confirm either");
        }
    }

    /// Clicking a marker cancels EVERY order it stands for. On a node that does not say which
    /// account an order is on (`orders_why`, Ruling R9) the ladder draws no marker, so the same
    /// click places an order instead — never a cancel for an order it cannot attribute.
    #[test]
    fn a_marker_click_cancels_every_order_it_stands_for_and_an_unattributed_ladder_has_none() {
        let orders = vec![order("b1", 1, 98.0, 0.01, false), order("b2", 1, 98.0, 0.02, false)];
        let mut h = settled(Setup { orders: orders.clone(), ..Setup::default() });
        let got = click(&mut h, Col::Buy, 98.0);
        assert_eq!(got, [TradeAction::Cancel(vec!["b1".to_string(), "b2".to_string()])]);
        let got = click(&mut h, Col::Bid, 98.0);
        assert!(place(1, OrderType::Limit, 98.0, &got), "the Bid cell still trades: {got:?}");

        let why = "This node does not say which account an order is on.";
        let mut h = settled(Setup { orders, orders_why: Some(why), ..Setup::default() });
        let got = click(&mut h, Col::Buy, 98.0);
        assert!(place(1, OrderType::Limit, 98.0, &got), "no marker, so no cancel: {got:?}");
    }

    /// Dragging a marker moves every order it stands for to the row it is dropped on, and only
    /// where the venue can modify an order: the same drag on a modify-less venue emits nothing.
    #[test]
    fn a_drag_reprices_only_where_the_venue_can_modify() {
        let orders = vec![order("b1", 1, 98.0, 0.01, false), order("b2", 1, 98.0, 0.02, false)];
        let mut h = settled(Setup {
            orders: orders.clone(),
            caps: vike_model::venues::venue_caps::BINANCE,
            ..Setup::default()
        });
        let got = drag(&mut h, Col::Buy, 98.0, 96.0);
        assert_eq!(
            got,
            [
                TradeAction::Modify { coid: "b1".to_string(), new_price: 96.0 },
                TradeAction::Modify { coid: "b2".to_string(), new_price: 96.0 },
            ]
        );
        assert_eq!(h.state().state.drag, None, "the drop ends the drag");

        let mut h = settled(Setup { orders, ..Setup::default() });
        let got = drag(&mut h, Col::Buy, 98.0, 96.0);
        assert!(got.is_empty(), "no modify on this venue, so no drag: {got:?}");
    }

    /// A drop outside the rows moves nothing (the row under it would be the edge row, clamped, a
    /// price the pointer was never on), and neither does a drop on the row the orders sit on.
    #[test]
    fn a_drop_outside_the_rows_or_on_the_orders_own_row_moves_nothing() {
        let setup = || Setup {
            orders: vec![order("b1", 1, 98.0, 0.01, false)],
            caps: vike_model::venues::venue_caps::BINANCE,
            ..Setup::default()
        };
        let mut h = settled(setup());
        let got = drag(&mut h, Col::Buy, 98.0, 96.0);
        let moved = TradeAction::Modify { coid: "b1".to_string(), new_price: 96.0 };
        assert_eq!(got, [moved], "CONTROL: a drop on a row inside reprices");

        let mut h = settled(setup());
        let (from, rows) = (cell(&h, Col::Buy, 98.0), rows_rect(&h));
        for to in [pos2(from.x, rows.min.y - 4.0), pos2(from.x, rows.max.y + 4.0)] {
            let got = drag_to(&mut h, from, to);
            assert!(got.is_empty(), "a drop at {to:?}, outside {rows:?}: {got:?}");
            assert_eq!(h.state().state.drag, None, "and the drag is over");
        }
        let to = cell(&h, Col::Bid, 98.0);
        assert!(drag_to(&mut h, from, to).is_empty(), "the orders' own row: nothing moves");
    }

    /// An Escape while a marker is dragged CANCELS the drag: egui ends it as a `drag_stopped` with
    /// the pointer where it is, and the drop used to read that as a drop there and reprice the
    /// orders to the row under the pointer. ⚠ The CONTROL comes first: the same drag, released
    /// instead, reprices — so "nothing moved" below cannot be true merely because the drag never
    /// started.
    #[test]
    fn an_escape_during_a_drag_moves_nothing() {
        let setup = || Setup {
            orders: vec![order("b1", 1, 98.0, 0.01, false)],
            caps: vike_model::venues::venue_caps::BINANCE,
            ..Setup::default()
        };
        let mut h = settled(setup());
        let moved = TradeAction::Modify { coid: "b1".to_string(), new_price: 96.0 };
        assert_eq!(drag(&mut h, Col::Buy, 98.0, 96.0), [moved], "CONTROL: a release reprices");

        let mut h = settled(setup());
        let (from, to) = (cell(&h, Col::Buy, 98.0), cell(&h, Col::Buy, 96.0));
        h.event(Event::PointerMoved(from));
        press(&h, from, true);
        h.step();
        for k in 1..=4 {
            h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 4.0)));
            h.step();
        }
        assert!(h.state().state.drag.is_some(), "the drag is under way");
        h.key_press(egui::Key::Escape);
        h.step();
        press(&h, to, false);
        h.run();
        let got = std::mem::take(&mut h.state_mut().emitted);
        assert!(got.is_empty(), "an Escape cancels the drag: {got:?}");
        assert_eq!(h.state().state.drag, None, "and the drag is over");
    }

    /// The W4 carry: a ladder click takes the TICKET's size rule (`ticket::checked_qty`), so one
    /// rule applies wherever an order is made. A size under the instrument's smallest order is
    /// refused, says why on the strip, and sends nothing; the smallest order itself goes. Before,
    /// the ladder checked only for one lot, and such a size reached the venue's own refusal.
    #[test]
    fn a_ladder_click_takes_the_tickets_size_rule() {
        let grid = Grid { min_qty: 0.05, ..GRID };
        let mut h = settled(Setup { grid, ..Setup::default() });
        h.state_mut().state.size = "0.01".to_string();
        let got = click(&mut h, Col::Bid, 98.0);
        match got.as_slice() {
            [TradeAction::Note { kind: StatusKind::Error, text }] => {
                assert!(text.contains("smallest order size"), "{text}");
            }
            other => panic!("one error note and no order, got {other:?}"),
        }
        assert_eq!(h.state().state.held, None, "nothing is held for a confirm either");
        h.state_mut().state.size = "0.05".to_string();
        let got = click(&mut h, Col::Bid, 98.0);
        assert!(place_qty(1, OrderType::Limit, 98.0, 0.05, &got), "CONTROL: {got:?}");
    }

    /// W1 review hand-off: what `paint_marker` PAINTS, not only what `marker_words` chooses. In a
    /// cell too narrow for its size a marker paints its COUNT, whole and inside the cell, with no
    /// clip cutting it; in a wide one, the count and the size. Re-adding a clip that cut the full
    /// words to the marker (the DOM's) fails here, where every pure test would still pass.
    #[test]
    fn a_marker_too_narrow_for_its_size_paints_its_count_unclipped() {
        let (a, b) = (order("a", 1, 98.0, 0.01, false), order("b", 1, 98.0, 0.02, false));
        let face = marker_face(&[&a, &b], 0.001);
        assert_eq!(face.full, "×2 0.030");
        for (cell_w, want) in [(36.0, "×2"), (120.0, "×2 0.030")] {
            let face = face.clone();
            let mut h = Harness::builder().with_size(egui::vec2(200.0, 80.0)).build_ui_state(
                move |ui, cell: &mut Option<Rect>| {
                    if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                        return;
                    }
                    let t = Tokens::of(ui.ctx());
                    let at = ui.min_rect().min + vec2(10.0, 10.0);
                    let c = Rect::from_min_size(at, vec2(cell_w, t.metrics.row_h));
                    paint_marker(ui.painter(), &t, c, 1, &face, true, false);
                    *cell = Some(c);
                },
                None,
            );
            h.run();
            let cell = h.state().expect("the marker was painted");
            let texts: Vec<(String, Rect, Rect)> = h
                .output()
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => {
                        Some((t.galley.text().to_string(), t.visual_bounding_rect(), c.clip_rect))
                    }
                    _ => None,
                })
                .collect();
            let [(text, drawn, clip)] = texts.as_slice() else {
                panic!("{cell_w} pt: one text, got {texts:?}");
            };
            assert_eq!(text, want, "{cell_w} pt");
            assert!(cell.expand(0.5).contains_rect(*drawn), "{cell_w} pt: {drawn:?} in {cell:?}");
            assert!(clip.contains_rect(*drawn), "{cell_w} pt: nothing of it is clipped away");
        }
    }

    /// A measurement for the owner, not a check: what a marker can say at the column widths the
    /// window ships with (W1 review hand-off; the owner decides whether to widen the order columns
    /// or list the orders on hover). Run it with
    /// `cargo test -p vike-panels --lib print_marker_room -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement for the owner: what a marker can say at the shipped column widths"]
    fn print_marker_room() {
        const WORDS: [&str; 5] = ["×2", "×2 0.030", "0.010", "×3 1.250", "1234.567"];
        let mut h = Harness::builder().build_ui_state(
            |ui, out: &mut Vec<String>| {
                if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                    return;
                }
                out.clear();
                for density in Density::ALL {
                    let m = density.metrics();
                    for text_size in TextSize::ALL {
                        let a = Appearance { density, text_size, ..Appearance::default() };
                        let t = Tokens::from_appearance(&a);
                        for (panel, size) in
                            [(Panel::Beside, layout::BESIDE_SIZE), (Panel::Under, layout::UNDER_SIZE)]
                        {
                            // The desktop's tool-window body keeps 8 pt each side.
                            let body = Rect::from_min_size(Pos2::ZERO, size - vec2(16.0, 0.0));
                            let view = View { ladder: true, panel };
                            let Some(ladder) = layout::split(body, view, &m).ladder else {
                                continue;
                            };
                            let col = col_rect(Col::Buy, ladder);
                            let widths: Vec<String> = WORDS
                                .iter()
                                .map(|w| {
                                    let font = t.mono(TextRole::Caption);
                                    let g = ui.painter().layout_no_wrap(w.to_string(), font, t.theme.text);
                                    format!("{w:?} {:.1}", g.size().x)
                                })
                                .collect();
                            out.push(format!(
                                "{density:?}/{text_size:?}/{panel:?} {}x{}: Buy column {:.1} pt, room \
                                 {:.1} pt; {}",
                                size.x,
                                size.y,
                                col.width(),
                                marker_room(col),
                                widths.join(", ")
                            ));
                        }
                    }
                }
            },
            Vec::new(),
        );
        h.run();
        for line in h.state() {
            println!("{line}");
        }
    }

    /// W4 fix round 2 (the NEW-1 audit, carried to the toolbar): the toolbar drops its word `Group`
    /// when STALE needs the room, and STALE comes and goes with the FEED, not with the trader. A
    /// press on "More price grouping" held across that is still a press on it: the grouping goes
    /// up, and Recenter — the button in its slot once the word is gone — is not clicked. The
    /// ladder is sized so the toolbar holds the word without STALE and not with it (measured as
    /// the toolbar measures).
    #[test]
    fn a_press_held_while_stale_drops_the_word_group_still_groups() {
        let mut h = settled(Setup::default());
        let (_, t) = h.state().drawn.expect("the ladder drew");
        let m = t.metrics;
        let w = |text: &str, font: egui::FontId| {
            h.ctx.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font, Color32::WHITE)).size().x
        };
        let need = 3.0 * m.control_h + w("1", t.mono(TextRole::Body)) + 3.0 * m.gap;
        let word = w("Group", t.font(TextRole::Caption)) + m.gap;
        // The toolbar's row: the ladder (the harness less its 8 pt margins) less its own padding.
        let width = need + word + 2.0 + 2.0 * m.pad + 16.0;
        h.set_size(egui::vec2(width, 560.0));
        h.run();
        let has_word = |h: &Harness<'static, Scene>| labels(h).iter().any(|l| l == "Group");
        assert!(has_word(&h), "CONTROL: the word fits without STALE");
        let at = h.get_by_label("More price grouping").rect().center();
        h.event(Event::PointerMoved(at));
        press(&h, at, true);
        h.step();
        h.state_mut().stale = true;
        h.step();
        h.step();
        press(&h, at, false);
        h.run();
        assert!(!has_word(&h), "CONTROL: STALE took the word's room under the press");
        assert_eq!(h.state().state.group, next_group(1), "the press grouped");
    }

    /// A drag the ladder did not see end — `state.drag` with no button down on the ladder — is
    /// dropped, so it can never reprice later; and a click after it trades.
    #[test]
    fn a_drag_the_ladder_did_not_see_end_is_dropped() {
        let mut h = settled(Setup::default());
        h.state_mut().state.drag = Some(vec!["gone".to_string()]);
        h.run();
        assert_eq!(h.state().state.drag, None);
        let got = click(&mut h, Col::Bid, 98.0);
        assert!(place(1, OrderType::Limit, 98.0, &got), "{got:?}");
    }

    /// Review fix 2: a size typed in the quote currency converts at the CLICKED price, the price
    /// the order is placed at — not at the last price (which bought 9.849 here, not 10) — and it
    /// still converts with no last price at all (where it placed nothing).
    #[test]
    fn a_quote_size_converts_at_the_clicked_price() {
        for last in [Some(MID), None] {
            let mut h = settled(Setup { last, ..Setup::default() });
            h.state_mut().state.unit = SizeUnit::Quote;
            h.state_mut().state.size = "980".to_string();
            let got = click(&mut h, Col::Bid, 98.0);
            assert!(place_qty(1, OrderType::Limit, 98.0, 10.0, &got), "{last:?}: {got:?}");
            let got = click(&mut h, Col::Bid, 101.0);
            assert!(place_qty(1, OrderType::Stop, 101.0, 9.702, &got), "{last:?}: {got:?}");
        }
    }

    /// Review fix 3: a click trades the row that was under the pointer when the button went DOWN.
    /// The market moves one row while it is down; the rows hold still until the release, and the
    /// click is resolved against them. ⚠ The CONTROL comes after: with the button up the rows
    /// follow the market, so the same point is then the next row — the move did shift the rows.
    #[test]
    fn a_click_trades_the_row_under_the_pointer_when_the_button_went_down() {
        let mut h = settled(Setup::default());
        let at = cell(&h, Col::Bid, 98.0);
        h.event(Event::PointerMoved(at));
        press(&h, at, true);
        h.step();
        h.state_mut().book = book_at(MID + 1.0);
        h.step();
        press(&h, at, false);
        h.run();
        let got = std::mem::take(&mut h.state_mut().emitted);
        assert!(place(1, OrderType::Limit, 98.0, &got), "the price under the press: {got:?}");

        let got = click_at(&mut h, at);
        assert!(place(1, OrderType::Limit, 99.0, &got), "CONTROL: the rows moved: {got:?}");
    }

    /// One-click OFF — where a LIVE account starts — HOLDS a ladder click for a confirm
    /// (`TradeState::submit`) rather than sending it.
    #[test]
    fn a_ladder_click_on_a_live_account_is_held_for_a_confirm() {
        let mut h = settled(Setup { mode: AccountMode::Live, ..Setup::default() });
        assert!(!h.state().state.one_click, "a LIVE account starts with one-click off");
        let got = click(&mut h, Col::Bid, 98.0);
        assert!(got.is_empty(), "nothing is sent: {got:?}");
        let held = h.state().state.held.clone();
        assert!(place(1, OrderType::Limit, 98.0, held.as_slice()), "held: {held:?}");
    }

    /// The row arithmetic saturates: under a tick so small that every price's key saturates `i64`,
    /// the ladder still draws and takes a click without a panic (a debug build panics on an
    /// overflow) — and that click sends no order: a saturated key names no price. This test used to
    /// pass on a `Place` at the saturated key times the tick, about 9e-282, a price no row showed
    /// and nobody clicked (final review A, minor 1).
    #[test]
    fn a_vanishing_tick_saturates_rather_than_overflowing() {
        let grid = Grid { tick: 1e-300, ..GRID };
        let mut h = settled(Setup { grid, ..Setup::default() });
        let at = cell(&h, Col::Bid, 98.0);
        let got = click_at(&mut h, at);
        assert!(rows_rect(&h).contains(at), "CONTROL: the click landed on the rows");
        let sent: Vec<&TradeAction> = got
            .iter()
            .filter(|a| matches!(a, TradeAction::Place { .. } | TradeAction::Modify { .. }))
            .collect();
        assert!(sent.is_empty(), "no order at a price nobody clicked: {sent:?}");
    }

    /// A press, `change` to the scene while the button is held (each a frame of its own, as a
    /// trader's hand gives them), then the release at the same point; what it emitted.
    fn press_change_release(
        h: &mut Harness<'static, Scene>,
        at: Pos2,
        change: impl FnOnce(&mut Scene),
    ) -> Vec<TradeAction> {
        h.state_mut().emitted.clear();
        h.event(Event::PointerMoved(at));
        press(h, at, true);
        h.step();
        change(h.state_mut());
        h.step();
        h.step();
        press(h, at, false);
        h.run();
        std::mem::take(&mut h.state_mut().emitted)
    }

    /// Final review A, I-1 (MONEY PATH): whether a click CANCELS or PLACES is decided where the
    /// button went down, against the marker drawn there — never again at the release. The trader
    /// presses the hollow marker of a sell stop at 98 to pull it, and the stop fills while the
    /// button is down: it is gone from the orders at the release, and a release that looked again
    /// found an empty Sell cell and sent a NEW sell (a stop at 98, below the bid) — an order that
    /// opens a short. A press on a marker is a cancel, and a cancel of an order that has gone is a
    /// harmless refusal. CONTROL: the stop stays, and the same press cancels it. And the reverse
    /// race: a press on an EMPTY cell places, even where a marker appears under the held button
    /// (a fast second click landing on the order the first click placed).
    #[test]
    fn a_press_on_a_marker_cancels_whatever_happens_to_its_orders_before_the_release() {
        let stop = order("s1", -1, 98.0, 0.01, true);
        for (what, fills) in [("CONTROL: the stop stays", false), ("the stop fills", true)] {
            let mut h = settled(Setup { orders: vec![stop.clone()], ..Setup::default() });
            let at = cell(&h, Col::Sell, 98.0);
            let got = press_change_release(&mut h, at, |s| {
                if fills {
                    s.orders.clear();
                }
            });
            assert_eq!(got, [TradeAction::Cancel(vec!["s1".to_string()])], "{what}");
            assert_eq!(h.state().state.held, None, "{what}: nothing held for a confirm");
        }
        let mut h = settled(Setup::default());
        let at = cell(&h, Col::Buy, 98.0);
        let got = press_change_release(&mut h, at, |s| {
            s.orders.push(order("b1", 1, 98.0, 0.01, false));
        });
        assert!(place(1, OrderType::Limit, 98.0, &got), "a press on an empty cell places: {got:?}");
    }

    /// Every text the last frame painted.
    fn painted(h: &Harness<'static, Scene>) -> Vec<String> {
        h.output()
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    /// Final review A, minor 1: a row at a price of zero or below names no price — none is drawn —
    /// and takes no click and no drop, as the ticket refuses a price that is not above zero. Such a
    /// row is on screen as soon as the ladder is scrolled under zero, or at a coarse grouping on a
    /// cheap instrument; an order there is a venue's rejection, or, on paper, a stop that never
    /// fires. CONTROL: on the same ladder, a row above zero is drawn, places and takes a drop.
    #[test]
    fn a_row_at_or_below_zero_names_no_price_and_takes_no_click_or_drop() {
        let setup = || Setup {
            orders: vec![order("b1", 1, 3.0, 0.01, false)],
            caps: vike_model::venues::venue_caps::BINANCE,
            ..Setup::default()
        };
        let mut h = settled(setup());
        h.state_mut().state.center = Some(1.0);
        h.run();
        let texts = painted(&h);
        assert!(texts.iter().any(|t| t == "2"), "CONTROL: the row at 2 names its price: {texts:?}");
        assert!(
            !texts.iter().any(|t| t == "0" || t.starts_with('-')),
            "no price at or below zero is drawn: {texts:?}"
        );
        for price in [0.0, -2.0] {
            let got = click(&mut h, Col::Bid, price);
            assert!(got.is_empty(), "a click at {price}: {got:?}");
            assert_eq!(h.state().state.held, None, "and nothing is held at {price}");
        }
        let got = click(&mut h, Col::Bid, 1.0);
        assert!(place(1, OrderType::Limit, 1.0, &got), "CONTROL: a row above zero: {got:?}");
        let got = drag(&mut h, Col::Buy, 3.0, -1.0);
        assert!(got.is_empty(), "a drop below zero moves nothing: {got:?}");
        let got = drag(&mut h, Col::Buy, 3.0, 2.0);
        let moved = TradeAction::Modify { coid: "b1".to_string(), new_price: 2.0 };
        assert_eq!(got, [moved], "CONTROL: a drop above zero reprices");
    }

    /// The owner's decision of 2026-10-03, card 1: hovering a marker lists the orders it stands
    /// for — each one's side, type, size and price — and their summed size, because at the shipped
    /// column widths a marker of several orders shows only its count, and its click cancels every
    /// one of them. CONTROL: a marker of one order lists that order alone, and an empty cell lists
    /// nothing.
    #[test]
    fn hovering_a_marker_lists_its_orders_and_their_summed_size() {
        let orders = vec![
            order("b1", 1, 98.0, 0.01, false),
            order("b2", 1, 98.0, 0.02, true),
            order("b3", 1, 96.0, 0.005, false),
        ];
        let mut h = settled(Setup { orders, ..Setup::default() });
        h.ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
        let hover = |h: &mut Harness<'static, Scene>, col, price| {
            h.hover_at(pos2(1.0, 1.0));
            h.run();
            let at = cell(h, col, price);
            h.hover_at(at);
            h.run();
            labels(h)
        };
        let two = hover(&mut h, Col::Buy, 98.0);
        for line in ["Buy limit 0.010 @ 98", "Buy stop 0.020 @ 98", "Total: 0.030 BTC in 2 orders"]
        {
            assert!(two.iter().any(|l| l == line), "{line:?} in {two:?}");
        }
        assert!(!two.iter().any(|l| l.contains("@ 96")), "only this row's orders: {two:?}");
        let one = hover(&mut h, Col::Buy, 96.0);
        for line in ["Buy limit 0.005 @ 96", "Total: 0.005 BTC in 1 order"] {
            assert!(one.iter().any(|l| l == line), "CONTROL: {line:?} in {one:?}");
        }
        assert!(!one.iter().any(|l| l.contains("@ 98")), "CONTROL: {one:?}");
        let none = hover(&mut h, Col::Buy, 97.0);
        assert!(!none.iter().any(|l| l.starts_with("Total:")), "an empty cell: {none:?}");
    }

    /// The owner's decision of 2026-10-03 (round 2, item 2): while the book is STALE the ladder
    /// places no order and moves none — limit or stop is chosen against a top of book that may be
    /// long gone, and a drop prices an order off it — but a marker still CANCELS: a cancel takes
    /// risk away and names its orders, not a price. The hint line says why, and a refused click or
    /// drop says so on the strip. CONTROL: on a live book the same click places and the same drag
    /// reprices.
    #[test]
    fn a_stale_ladder_places_and_moves_nothing_but_a_marker_still_cancels() {
        let setup = || Setup {
            orders: vec![order("b1", 1, 98.0, 0.01, false)],
            caps: vike_model::venues::venue_caps::BINANCE,
            ..Setup::default()
        };
        let mut h = settled(setup());
        assert!(place(1, OrderType::Limit, 97.0, &click(&mut h, Col::Bid, 97.0)), "CONTROL");
        let moved = TradeAction::Modify { coid: "b1".to_string(), new_price: 96.0 };
        assert_eq!(drag(&mut h, Col::Buy, 98.0, 96.0), [moved], "CONTROL: a live drag reprices");

        let mut h = settled(setup());
        h.state_mut().stale = true;
        h.run();
        let ls = labels(&h);
        assert!(ls.iter().any(|l| l == HINT_STALE) && !ls.iter().any(|l| l == HINT), "{ls:?}");
        let refused = [TradeAction::Note { kind: StatusKind::Error, text: STALE_WHY.to_string() }];
        assert_eq!(click(&mut h, Col::Bid, 97.0), refused, "a click on a stale book");
        assert_eq!(h.state().state.held, None, "nothing is held for a confirm either");
        let got = drag(&mut h, Col::Buy, 98.0, 96.0);
        assert!(!got.iter().any(|a| matches!(a, TradeAction::Modify { .. })), "a drag: {got:?}");
        assert_eq!(h.state().state.drag, None, "and no drag is left under way");
        let got = click(&mut h, Col::Buy, 98.0);
        assert_eq!(got, [TradeAction::Cancel(vec!["b1".to_string()])], "a marker still cancels");

        // Where no own order can be shown, the stale hint offers no marker either.
        let mut h = settled(Setup { orders_why: Some("not attributed"), ..setup() });
        h.state_mut().stale = true;
        h.run();
        let ls = labels(&h);
        assert!(
            ls.iter().any(|l| l == HINT_STALE_NO_MARKERS) && !ls.iter().any(|l| l == HINT_STALE),
            "{ls:?}"
        );
    }

    /// [`drag_to`] with `change` made to the scene after `moves` of its four pointer moves (0: in
    /// the frame after the press, before the pointer has moved at all), each move a frame of its
    /// own; what the drag emitted.
    fn drag_change(
        h: &mut Harness<'static, Scene>,
        from: Pos2,
        to: Pos2,
        moves: usize,
        change: impl FnOnce(&mut Scene),
    ) -> Vec<TradeAction> {
        h.state_mut().emitted.clear();
        h.event(Event::PointerMoved(from));
        press(h, from, true);
        h.step();
        let mut change = Some(change);
        for k in 1..=4 {
            if k == moves + 1
                && let Some(c) = change.take()
            {
                c(h.state_mut());
            }
            h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 4.0)));
            h.step();
        }
        if let Some(c) = change.take() {
            c(h.state_mut());
        }
        press(h, to, false);
        h.run();
        std::mem::take(&mut h.state_mut().emitted)
    }

    /// The FW1 review (MONEY PATH): a click on the ladder is judged against the book at the PRESS
    /// as well as at the release. A press on a STALE ladder, whose rows were drawn off a frozen
    /// book and stay where they were drawn until the release, places nothing even when the book is
    /// live again by the release: the trader chose that row against a book that may be long gone.
    /// CONTROL: live at the press and at the release, the click places; live at the press and stale
    /// at the release, it is refused, as before.
    #[test]
    fn a_press_on_a_stale_ladder_places_nothing_even_when_the_book_is_live_at_the_release() {
        let refused = [TradeAction::Note { kind: StatusKind::Error, text: STALE_WHY.to_string() }];
        for (what, stale_at_press, stale_at_release, places) in [
            ("CONTROL: live throughout", false, false, true),
            ("CONTROL: stale at the release", false, true, false),
            ("stale at the press, live at the release", true, false, false),
        ] {
            let mut h = settled(Setup::default());
            h.state_mut().stale = stale_at_press;
            h.run();
            let at = cell(&h, Col::Bid, 97.0);
            let got = press_change_release(&mut h, at, |s| s.stale = stale_at_release);
            if places {
                assert!(place(1, OrderType::Limit, 97.0, &got), "{what}: {got:?}");
            } else {
                assert_eq!(got, refused, "{what}");
                assert_eq!(h.state().state.held, None, "{what}: nothing held for a confirm");
            }
        }
    }

    /// The FW1 review: a drag whose book goes stale while it is under way moves nothing at the
    /// drop and says why: the drop's own gate, which nothing else reaches, because the drag-start
    /// gate has already let this drag begin. And a drag PRESSED on a stale ladder moves nothing
    /// either, even where the book is live again before the pointer has moved far enough to start
    /// it, as a click pressed there places nothing. CONTROL: the same drag on a live book reprices.
    #[test]
    fn a_drag_on_a_book_stale_at_its_press_or_at_its_drop_moves_nothing() {
        let setup = || Setup {
            orders: vec![order("b1", 1, 98.0, 0.01, false)],
            caps: vike_model::venues::venue_caps::BINANCE,
            ..Setup::default()
        };
        let refused = [TradeAction::Note { kind: StatusKind::Error, text: STALE_WHY.to_string() }];
        let mut h = settled(setup());
        let (from, to) = (cell(&h, Col::Buy, 98.0), cell(&h, Col::Buy, 96.0));
        let moved = TradeAction::Modify { coid: "b1".to_string(), new_price: 96.0 };
        assert_eq!(drag_change(&mut h, from, to, 2, |_| {}), [moved], "CONTROL: a live drag");

        let mut h = settled(setup());
        let got = drag_change(&mut h, from, to, 2, |s| {
            assert!(s.state.drag.is_some(), "CONTROL: the drag is under way");
            s.stale = true;
        });
        assert_eq!(got, refused, "the book went stale while the drag was under way");

        let mut h = settled(setup());
        h.state_mut().stale = true;
        h.run();
        let got = drag_change(&mut h, from, to, 0, |s| s.stale = false);
        assert!(
            !got.iter().any(|a| matches!(a, TradeAction::Modify { .. })),
            "a drag pressed on a stale ladder: {got:?}"
        );
        assert_eq!(h.state().state.drag, None, "and no drag is left under way");
    }

    /// The FW1 review: a drag moves the orders the marker under the press stood for, and of those
    /// only the ones still working at the drop. One that filled or was cancelled while the button
    /// was down is sent no `Modify` (the node refuses it, and the strip would show a rejection for
    /// an order the trader no longer sees); with none left, the drop sends nothing at all.
    /// CONTROL: with both still working, both move.
    #[test]
    fn a_drag_moves_only_the_orders_still_working_at_the_drop() {
        let setup = || Setup {
            orders: vec![order("b1", 1, 98.0, 0.01, false), order("b2", 1, 98.0, 0.02, false)],
            caps: vike_model::venues::venue_caps::BINANCE,
            ..Setup::default()
        };
        let modify = |coid: &str| TradeAction::Modify { coid: coid.to_string(), new_price: 96.0 };
        for (what, gone, want) in [
            ("CONTROL: both still working", vec![], vec![modify("b1"), modify("b2")]),
            ("b1 filled during the drag", vec!["b1"], vec![modify("b2")]),
            ("both went during the drag", vec!["b1", "b2"], vec![]),
        ] {
            let mut h = settled(setup());
            let (from, to) = (cell(&h, Col::Buy, 98.0), cell(&h, Col::Buy, 96.0));
            let got = drag_change(&mut h, from, to, 2, |s| {
                s.orders.retain(|o| !gone.contains(&o.client_order_id.as_str()));
            });
            assert_eq!(got, want, "{what}");
        }
    }

    /// The FW1 review (I-1's other half): the press is judged against the markers the trader SAW,
    /// the frame before it, never against the orders as they stand in the frame the press lands
    /// in. An order that went in the very frame of the press was still on screen under the
    /// pointer, so the press is its cancel; one that went a frame earlier was already gone from the
    /// screen, so the press is on an empty cell and places.
    #[test]
    fn a_press_is_judged_against_the_markers_drawn_before_it() {
        let stop = || order("s1", -1, 98.0, 0.01, true);
        // The pointer comes to rest over the cell in a frame of its own, then `change`, then the
        // press goes down in the next frame, and is released later. That models the HARNESS, which
        // runs one frame per queued event (`Harness::step`), so a move and a press queued together
        // are two frames here; a real hand cannot be judged later than this, because eframe hands a
        // move and a press that arrive together to ONE pass, and egui hit-tests that pass against
        // the widgets of the pass before it.
        let rest_change_press = |h: &mut Harness<'static, Scene>, at, change: fn(&mut Scene)| {
            h.event(Event::PointerMoved(at));
            h.step();
            change(h.state_mut());
            press(h, at, true);
            h.step();
            press(h, at, false);
            h.run();
            std::mem::take(&mut h.state_mut().emitted)
        };
        let gone = |s: &mut Scene| s.orders.clear();
        let mut h = settled(Setup { orders: vec![stop()], ..Setup::default() });
        let at = cell(&h, Col::Sell, 98.0);
        let got = rest_change_press(&mut h, at, gone);
        assert_eq!(got, [TradeAction::Cancel(vec!["s1".to_string()])], "gone in the press's frame");

        let mut h = settled(Setup { orders: vec![stop()], ..Setup::default() });
        gone(h.state_mut());
        h.run();
        h.state_mut().emitted.clear();
        let got = rest_change_press(&mut h, at, |_| {});
        assert!(place(-1, OrderType::Stop, 98.0, &got), "gone a frame before: {got:?}");
    }

    /// Every `Role::Label` value on screen (egui files a label's text under its VALUE).
    fn labels(h: &Harness<'static, Scene>) -> Vec<String> {
        h.root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == Role::Label)
            .map(|n| n.accesskit_node().value().as_deref().unwrap_or("").to_string())
            .collect()
    }

    /// Spec §3.4's hint line sits under the ladder in the side layout (pre-flight Minor 9). Where
    /// the account does not trade the symbol it says that instead, because a click there does
    /// nothing. Under the ladder the compact ticket needs the height, so there is no line.
    #[test]
    fn the_hint_line_is_drawn_beside_the_ticket_and_says_why_a_click_does_nothing() {
        let h = settled(Setup::default());
        assert!(labels(&h).iter().any(|l| l == HINT), "{:?}", labels(&h));

        let h = settled(Setup {
            tradable: Tradable::No { trades: &[], why: None },
            ..Setup::default()
        });
        let reason = "This account cannot trade BTCUSDT now.".to_string();
        let ls = labels(&h);
        assert!(ls.contains(&reason) && !ls.iter().any(|l| l == HINT), "{ls:?}");

        let h = settled(Setup { panel: Panel::Under, ..Setup::default() });
        assert!(!labels(&h).iter().any(|l| l == HINT), "no hint under: {:?}", labels(&h));

        // No own order can be shown (`orders_why`): no marker to click, so none is offered.
        let h = settled(Setup { orders_why: Some("not attributed"), ..Setup::default() });
        let ls = labels(&h);
        assert!(ls.iter().any(|l| l == HINT_NO_MARKERS) && !ls.iter().any(|l| l == HINT), "{ls:?}");
    }
}
