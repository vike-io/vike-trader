//! The ladder's own-order markers (the design's pills): what a pill is and how it is painted, the
//! words it shows in the room it has, and what hovering it lists.

use super::fmt_price;
use crate::trade_window::{TradeWindowLadderOrder, sizing};
use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2};
use vike_ui_theme::color::with_alpha;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::maps::{self, ThemeMapRow};
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::roles::ThemeColourRole;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;

/// How one own-order pill is painted (the design's `span.pill`): its fill, the colour of its
/// one-point edge, the colour of its words, and whether that edge is dashed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TradeWindowLadderMarkerStyle {
    pub(super) fill: Color32,
    pub(super) edge: Color32,
    pub(super) ink: Color32,
    pub(super) dashed: bool,
}

// The alpha the design fills a limit pill with (`rgba(…, .22)`) and edges it with (`.55`) are the
// `trade::PILL_FILL_ALPHA` and `trade::PILL_EDGE_ALPHA` rows.

/// What an own-order pill is: a side and a type. Each is a row of `ui-theme.toml`'s `order_pill` map
/// (`vike_ui_theme::maps::order_pill`), which names the roles the pill is drawn in, so which colour a
/// pill wears — and that a stop's edge is dashed — is a line of the TOML and nothing here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TradeWindowLadderMarkerKind {
    BuyLimit,
    BuyStop,
    SellLimit,
    SellStop,
}

impl TradeWindowLadderMarkerKind {
    /// The pill of an order on `side` (positive is a buy) that is a stop or a limit.
    pub(super) fn of(side: i32, is_stop: bool) -> TradeWindowLadderMarkerKind {
        match (side > 0, is_stop) {
            (true, false) => TradeWindowLadderMarkerKind::BuyLimit,
            (true, true) => TradeWindowLadderMarkerKind::BuyStop,
            (false, false) => TradeWindowLadderMarkerKind::SellLimit,
            (false, true) => TradeWindowLadderMarkerKind::SellStop,
        }
    }

    /// The row of the `order_pill` map this pill wears. Exhaustive: a pill without a row does not
    /// compile.
    pub(super) fn row(self) -> &'static ThemeMapRow {
        match self {
            TradeWindowLadderMarkerKind::BuyLimit => &maps::order_pill::BUY_LIMIT,
            TradeWindowLadderMarkerKind::BuyStop => &maps::order_pill::BUY_STOP,
            TradeWindowLadderMarkerKind::SellLimit => &maps::order_pill::SELL_LIMIT,
            TradeWindowLadderMarkerKind::SellStop => &maps::order_pill::SELL_STOP,
        }
    }
}

/// A LIMIT order's pill is its side's colour at a fifth of its strength with its side's colour
/// around it. A STOP's is hollow and its edge DASHED, in the side's whole colour (the design's
/// `.pill.stop`; owner decision 2). The edge also says whether the pill can be dragged (audit br6):
/// the side's colour when this venue can reprice, the caption grey when it cannot — on every venue
/// that can, the design's pill exactly. Dragging rings either kind in the accent ([`paint_marker`])
/// — the focus ring, one of the accent's shapes (spec §2).
///
/// The ROLES come from the pill's row (`fill`, `stroke`, `text`, and `flag` for the dash; the grey
/// of a pill that cannot be dragged is the `NOT_DRAGGABLE` row's `stroke`). What stays here is the
/// tint: a pill with a fill is drawn at `trade::PILL_FILL_ALPHA` of it with its edge at
/// `trade::PILL_EDGE_ALPHA` of the edge's role, and a hollow one (its fill is `none`) keeps its edge
/// whole.
pub(super) fn pill_style(
    side: i32,
    is_stop: bool,
    draggable: bool,
    t: &ThemeTokens,
) -> TradeWindowLadderMarkerStyle {
    let row = TradeWindowLadderMarkerKind::of(side, is_stop).row();
    let hollow = row.fill == ThemeColourRole::None;
    let fill = if hollow {
        row.fill.resolve(t)
    } else {
        with_alpha(row.fill.resolve(t), trade::PILL_FILL_ALPHA)
    };
    let edge = match (draggable, hollow) {
        (false, _) => maps::order_pill::NOT_DRAGGABLE.stroke.resolve(t),
        (true, true) => row.stroke.resolve(t),
        (true, false) => with_alpha(row.stroke.resolve(t), trade::PILL_EDGE_ALPHA),
    };
    TradeWindowLadderMarkerStyle {
        fill,
        edge,
        ink: row.text.resolve(t),
        dashed: row.flag == Some(true),
    }
}

// ---------------------------------------------------------------------------
// The markers
// ---------------------------------------------------------------------------

/// What one marker can say, and how it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TradeWindowLadderMarkerFace {
    /// The count FIRST, then the summed size: `×2 0.030`; one order is its size alone, `0.010`.
    pub(super) full: String,
    /// The count alone, `×2` (`×1` for one order): what the marker says where `full` does not fit.
    pub(super) count: String,
    /// Hollow: only when every order it stands for is a stop.
    pub(super) stop: bool,
}

/// The face of the marker standing for `own`. The size prints to the lot's decimals — an order is
/// a lot multiple, and the DOM's fixed four decimals printed an order under 0.00005 as `0.0000` —
/// or, while the lot is not known, to the size's own.
pub(super) fn marker_face(
    own: &[&TradeWindowLadderOrder],
    lot: Option<f64>,
) -> TradeWindowLadderMarkerFace {
    let quantity: f64 = own.iter().map(|o| o.quantity).sum();
    let size = sizing::quantity_text(quantity, lot);
    let count = format!("×{}", own.len());
    let full = if own.len() > 1 { format!("{count} {size}") } else { size };
    TradeWindowLadderMarkerFace {
        full,
        count,
        stop: !own.is_empty() && own.iter().all(|o| o.is_stop),
    }
}

/// The words a marker shows in `room` points, given each one's width: the count and the size where
/// they fit, else the count alone. ⚠ The COUNT is never what gives way, because a click on the
/// marker cancels EVERY order it stands for; and no text is ever cut — where not even the count
/// fits, the marker is drawn with no words rather than with half a glyph.
pub(super) fn marker_words(
    face: &TradeWindowLadderMarkerFace,
    room: f32,
    width: impl Fn(&str) -> f32,
) -> Option<&str> {
    [face.full.as_str(), face.count.as_str()].into_iter().find(|w| width(w) <= room)
}

/// What hovering the marker standing for `own` lists: each order, its side, its type, its size
/// and its price, with its id's words under it where it has any ([`TradeWindowLadderOrder::words`]), then their
/// summed size (the owner's decision of 2026-10-03, card 1). A marker at the shipped column widths
/// shows only its count where it stands for several orders, and its click cancels every one of
/// them: the hover is where the trader reads what they are.
pub(super) fn marker_lines(
    own: &[&TradeWindowLadderOrder],
    tick: f64,
    lot: Option<f64>,
    base: &str,
) -> Vec<String> {
    let mut lines: Vec<String> = own
        .iter()
        .flat_map(|o| {
            let side = if o.side > 0 { "Buy" } else { "Sell" };
            let kind = if o.is_stop { "stop" } else { "limit" };
            let size = sizing::quantity_text(o.quantity, lot);
            let line = format!("{side} {kind} {size} @ {}", fmt_price(o.price, Some(tick)));
            std::iter::once(line).chain(o.words.as_ref().map(|w| format!("  {w}")))
        })
        .collect();
    let total: f64 = own.iter().map(|o| o.quantity).sum();
    let orders = if own.len() == 1 { "order" } else { "orders" };
    lines.push(format!(
        "Total: {} {base} in {} {orders}",
        sizing::quantity_text(total, lot),
        own.len()
    ));
    lines
}

/// What stands between a pill's edge and its words: the design's `space::SM` of padding inside its
/// `stroke::HAIRLINE` border. A pill is its words and this each side, at most the cell's width
/// ([`paint_marker`]).
const PILL_EDGE: f32 = space::SM + stroke::HAIRLINE;

// The pill's corner (`trade::PILL_RADIUS`: the design's `border-radius: 3px`, tighter than the kit's),
// the dashes of a stop's edge (`trade::DASH` on, `trade::DASH_GAP` off: as the design's browser draws a
// one-point dashed border) and the wash a pill takes under the pointer (`trade::PILL_LIFT_ALPHA`: the
// design's `brightness(1.25)`, the text colour at that alpha) are `[[value]]` rows.

/// The width a pill's words may take in `cell`. The least that stands between a pill's edge and its
/// words is `space::XS`, the border and a point: where the cell is too narrow for the words and
/// [`PILL_EDGE`], the padding gives way down to this before the words do, so at the shipped column
/// widths a pill with words can fill its cell, and its words, never cut, decide what it says
/// ([`marker_words`]).
pub(super) fn marker_room(cell: Rect) -> f32 {
    cell.width() - 2.0 * space::XS
}

/// The corner arcs of a rounded rectangle, quarter-turn by quarter-turn, as `(cos, sin)` of 0, 30,
/// 60 and 90 degrees: a dashed edge needs the outline as a polyline, and the platform's sine is
/// kept out of production code (decision 0032).
const QUARTER: [(f32, f32); 4] = [(1.0, 0.0), (0.866_025_4, 0.5), (0.5, 0.866_025_4), (0.0, 1.0)];

/// `rect`'s outline as a closed polyline with corners of radius `r`, clockwise from the top-left
/// corner's start.
fn outline(rect: Rect, r: f32) -> Vec<Pos2> {
    let corners = [
        (pos2(rect.min.x + r, rect.min.y + r), [-1.0, 0.0, 0.0, -1.0]),
        (pos2(rect.max.x - r, rect.min.y + r), [0.0, 1.0, -1.0, 0.0]),
        (pos2(rect.max.x - r, rect.max.y - r), [1.0, 0.0, 0.0, 1.0]),
        (pos2(rect.min.x + r, rect.max.y - r), [0.0, -1.0, 1.0, 0.0]),
    ];
    let mut path = Vec::with_capacity(4 * QUARTER.len() + 1);
    for (centre, [cx, sx, cy, sy]) in corners {
        for (c, s) in QUARTER {
            path.push(pos2(centre.x + r * (cx * c + sx * s), centre.y + r * (cy * c + sy * s)));
        }
    }
    path.push(path[0]);
    path
}

/// What a pill is doing besides standing there: whether its venue can reprice it, whether it is
/// being dragged now, and where the pointer is (for the wash a pill takes under it).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TradeWindowLadderMarkerState {
    pub(super) draggable: bool,
    pub(super) dragging: bool,
    pub(super) pointer: Option<Pos2>,
}

/// Paint the pill with `face` in `cell`: its words ([`marker_words`]) and [`PILL_EDGE`] each side,
/// centred, and never wider than the cell.
pub(super) fn paint_marker(
    painter: &Painter,
    t: &ThemeTokens,
    cell: Rect,
    side: i32,
    face: &TradeWindowLadderMarkerFace,
    state: TradeWindowLadderMarkerState,
) {
    let TradeWindowLadderMarkerState { draggable, dragging, pointer } = state;
    let style = pill_style(side, face.stop, draggable, t);
    let layout =
        |w: &str| painter.layout_no_wrap(w.to_string(), t.mono(ThemeTextRole::Caption), style.ink);
    let galley = marker_words(face, marker_room(cell), |w| layout(w).size().x).map(layout);
    let text_w = galley.as_ref().map_or(0.0, |g| g.size().x);
    let w = (text_w + 2.0 * PILL_EDGE).min(cell.width());
    // The pill sits `space::XS` inside its row above and below: the design's 14 pt pill in an 18 pt
    // row.
    let pill = Rect::from_center_size(cell.center(), vec2(w, cell.height() - 2.0 * space::XS));
    let round = CornerRadius::same(trade::PILL_RADIUS);
    let edge = Stroke::new(stroke::HAIRLINE, style.edge);
    painter.rect_filled(pill, round, style.fill);
    if style.dashed {
        painter.extend(Shape::dashed_line(
            &outline(
                pill.shrink(edge.width / 2.0),
                f32::from(trade::PILL_RADIUS) - edge.width / 2.0,
            ),
            edge,
            trade::DASH,
            trade::DASH_GAP,
        ));
    } else {
        painter.rect_stroke(pill, round, edge, StrokeKind::Inside);
    }
    if pointer.is_some_and(|at| pill.contains(at)) {
        painter.rect_filled(pill, round, with_alpha(t.theme.text, trade::PILL_LIFT_ALPHA));
    }
    if dragging {
        let ring = Stroke::new(trade::DRAG_RING_WIDTH, t.theme.accent);
        painter.rect_stroke(pill.expand(stroke::LINE), round, ring, StrokeKind::Middle);
    }
    if let Some(g) = galley {
        painter.galley(pill.center() - g.size() / 2.0, g, style.ink);
    }
}
