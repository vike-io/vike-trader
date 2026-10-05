//! The Trade window's geometry — pure, so every rule is tested without an egui frame, but for
//! [`window_size`], which measures the type the window is drawn in.

use egui::{Rect, Vec2, pos2, vec2};
use vike_ui_theme::components::Tokens;
use vike_ui_theme::metrics::Metrics;

use super::{Panel, View, instrument, status, ticket};

/// The ticket's width beside the ladder, in points (the prototype's).
pub const TICKET_W: f32 = 260.0;
/// The narrowest ladder drawn beside a ticket; any narrower and the ticket takes the body.
pub const MIN_LADDER_W: f32 = 200.0;
/// The side-panel window (spec §3.9).
pub const BESIDE_SIZE: Vec2 = vec2(600.0, 560.0);
/// The bottom-panel window (spec §3.9).
pub const UNDER_SIZE: Vec2 = vec2(320.0, 680.0);
/// The ticket alone.
pub const TICKET_ONLY_SIZE: Vec2 = vec2(TICKET_W + 20.0, 560.0);
/// The tick chart pane's width beside the ladder (the v3 design's: its window is 301 wider with the
/// chart on). A window made wider than its spec size gives the extra to the ladder; a window too
/// narrow for both gives the chart up first.
pub const CHART_W: f32 = 301.0;
/// The narrowest chart pane drawn; any narrower and it is left out, like a ladder under
/// [`MIN_LADDER_W`].
pub const MIN_CHART_W: f32 = 160.0;
/// The window with only the compact ticket in it (the panel under, no ladder, no chart): the
/// design's 320 pt column, its height counted by [`window_size`].
pub const COMPACT_ALONE_W: f32 = 320.0;

/// The desktop tool window's frame, a point each side.
const FRAME: f32 = 1.0;
/// Its body's margin left and right.
const SIDE_MARGIN: f32 = 8.0;
/// Its body's margin at the bottom.
const BOTTOM_MARGIN: f32 = 6.0;

/// A view toggle, and the gap between two of a group (the design's `.ib`). They sit in the title
/// bar (`vike_ui_theme::chrome`, 25 pt at every density), so they are the design's size at every
/// density too.
pub const VIEW_BUTTON: Vec2 = vec2(24.0, 20.0);
pub const VIEW_GAP: f32 = 2.0;
/// The room each side of the hairline that sets the two pane toggles apart from the two layout
/// toggles (the design's `.tsep`; the hairline's height is `chrome::SEPARATOR_H`).
pub const VIEW_GROUP_GAP: f32 = 5.0;

/// The width the four view toggles take in the bar's slot: two pairs, a group gap each side of a
/// hairline between them.
pub const VIEW_CONTROLS_W: f32 = 4.0 * VIEW_BUTTON.x + 2.0 * VIEW_GAP + 2.0 * VIEW_GROUP_GAP + 1.0;

/// What the desktop's tool window puts around the rect [`super::draw`] fills, across and down.
/// Across: its frame each side and its body's side margins. Down: its frame top and bottom, this
/// window's title bar (`vike_ui_theme::chrome::TITLE_BAR_H`, density-free, and nothing between it
/// and the body: `title_bar::body_gap`) and its body's bottom margin. A window `size` gives `draw` a rect `size - chrome()`.
///
/// ⚠ The frame and the margins are vike-app-core's
/// (`crates/vike-app-core/src/ui/workspace/state.rs`'s `TOOL_BODY_MARGIN`), mirrored here
/// because this crate sits below the shell; vike-app-core's
/// `the_trade_windows_body_is_its_size_less_the_chrome_its_layout_assumes` holds this function to
/// the window it draws.
pub fn chrome() -> Vec2 {
    vec2(
        2.0 * (FRAME + SIDE_MARGIN),
        2.0 * FRAME + vike_ui_theme::chrome::TITLE_BAR_H + BOTTOM_MARGIN,
    )
}

/// The size each view opens at by the spec (§3.9, the prototype's sizes): the floor
/// [`window_size`] grows from. The QA capture's window opens at it, before egui has the type to
/// measure anything with.
pub fn spec_size(view: View) -> Vec2 {
    let chart = if view.chart { CHART_W } else { 0.0 };
    match (view.ladder, view.panel) {
        (false, Panel::Beside) => vec2(TICKET_ONLY_SIZE.x + chart, TICKET_ONLY_SIZE.y),
        (true, Panel::Beside) => vec2(BESIDE_SIZE.x + chart, BESIDE_SIZE.y),
        // The compact ticket alone: the design's column, and a height `window_size` counts.
        (false, Panel::Under) if !view.chart => vec2(COMPACT_ALONE_W, UNDER_SIZE.y),
        (false, Panel::Under) => UNDER_SIZE,
        // Chart and ladder side by side over the ticket: the design's 621 × 659.
        (true, Panel::Under) if view.chart => vec2(UNDER_SIZE.x + CHART_W, UNDER_SIZE.y - 21.0),
        (true, Panel::Under) => UNDER_SIZE,
    }
}

/// The size a window opens at, and the size a view change asks for: [`spec_size`]'s width, and its
/// height or, where the full ticket needs more to show its WHOLE form, that (ruling B1 of the
/// render check, 2026-10-03: at Comfortable density TP/SL sat 61 pt below the fold of the 600 ×
/// 560 window). Counted as [`super::draw`] lays a window out, in `ctx`'s look and type: the
/// window around the body ([`chrome`]), the instrument bar, the ticket in the state a window opens
/// in (`ticket::opening_h`) and the status strip, no gap between them. The compact ticket under the ladder
/// has fixed rows and never scrolls, so that view keeps its size.
///
/// ⚠ The strip is counted at its least height (`status::idle_height`), the strip of the state a
/// window opens in: TP/SL starts off there, so nothing is refused. It does NOT count the growth of
/// a strip that says why the ticket sends nothing (`status::height`): a TP/SL ticked later and
/// refused on the account's lane takes three Caption lines in a 320 pt window and four in a 280 pt
/// one, 8.0 / 9.4 pt and 20.0 / 21.4 pt taller (Comfortable / Normal, Standard text; FW6), and a
/// full ticket's form gives that room up. A form that then scrolls by a few points is exactly the
/// case its divider exists for, so the opening height is not raised for a state no window opens in.
///
/// # Panics
///
/// With a context that has run no frame yet: egui has no fonts until one has
/// (`egui::Context::fonts_mut`), and the type is what this measures.
pub fn window_size(view: View, ctx: &egui::Context) -> Vec2 {
    let spec = spec_size(view);
    let t = Tokens::of(ctx);
    let m = &t.metrics;
    let chrome = chrome();
    let content_w = spec.x - chrome.x;
    let bar = instrument::height(content_w, &t);
    let strip = status::idle_height(ctx, &t);
    // The body `draw` splits at the spec's size: what the bar and the strip leave.
    let body_h = (spec.y - chrome.y - bar - strip).max(0.0);
    let rects = split(Rect::from_min_size(pos2(0.0, 0.0), vec2(content_w, body_h)), view, m);
    if compact_ticket(view, &rects) {
        // The compact ticket stands alone in its own column: its height is its rows, the bar's and
        // the strip's, so the window is that and no taller. With a ladder or a chart over it the
        // spec height holds.
        if rects.ladder.is_none() && rects.chart.is_none() {
            let h = chrome.y + bar + under_ticket_h(m) + strip;
            return vec2(spec.x, h.ceil());
        }
        return spec;
    }
    let ticket = ticket::opening_h(ctx, &t, rects.ticket.width());
    let h = chrome.y + bar + ticket + strip;
    vec2(spec.x, spec.y.max(h.ceil()))
}

/// The compact ticket's height at density `m`: [`ticket::compact_h`], the height of the ticket with
/// its Buy and Sell stacked, one to a row. It is fixed before the ticket is drawn, so it holds the
/// stacked case; side by side it leaves a row's room empty under the cancel row.
pub fn under_ticket_h(m: &Metrics) -> f32 {
    ticket::compact_h(m)
}

/// The hairline between the tick chart and the ladder beside it: the chart paints its own edge there.
pub const PANE_SEAM: f32 = 1.0;

/// Where the tick chart, the ladder and the ticket go inside the body. The chart and the ladder
/// share the same top and bottom, so the chart's price rows are the ladder's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyRects {
    pub chart: Option<Rect>,
    pub ladder: Option<Rect>,
    pub ticket: Rect,
}

/// Split `body` for `view` at density `m`. A ladder that would be narrower than [`MIN_LADDER_W`],
/// or shorter than five rows, is not drawn, and the ticket takes the whole body. The chart gives way
/// first: where the room beside the ticket cannot hold a ladder and a chart of [`MIN_CHART_W`], the
/// ladder keeps it.
pub fn split(body: Rect, view: View, m: &Metrics) -> BodyRects {
    let alone = BodyRects { chart: None, ladder: None, ticket: body };
    if !view.ladder && !view.chart {
        return alone;
    }
    match view.panel {
        Panel::Beside => {
            // The ticket's region is [`TICKET_W`] wide and runs one padding past the body's right edge:
            // the ticket insets its content by that padding, so its controls end at the body's edge
            // as the design's do, and begin one padding right of the rule that parts it from the
            // ladder, which in turn stands one padding off the ladder's last column.
            let ticket_w = TICKET_W.min(body.width() + m.pad);
            let ticket = Rect::from_min_max(
                pos2(body.max.x + m.pad - ticket_w, body.min.y),
                pos2(body.max.x + m.pad, body.max.y),
            );
            let room = ticket.min.x - m.pad - 1.0 - body.min.x;
            let pane = |x: f32, w: f32| {
                Rect::from_min_size(pos2(body.min.x + x, body.min.y), vec2(w, body.height()))
            };
            let (chart_w, ladder_w) = match (view.chart, view.ladder) {
                (true, true) if room >= MIN_LADDER_W + PANE_SEAM + MIN_CHART_W => {
                    let chart = CHART_W.min(room - MIN_LADDER_W - PANE_SEAM);
                    (chart, room - chart - PANE_SEAM)
                }
                (_, true) if room >= MIN_LADDER_W => (0.0, room),
                (true, false) if room >= MIN_CHART_W => (room, 0.0),
                _ => return alone,
            };
            BodyRects {
                chart: (chart_w > 0.0).then(|| pane(0.0, chart_w)),
                ladder: (ladder_w > 0.0)
                    .then(|| pane(chart_w + if chart_w > 0.0 { PANE_SEAM } else { 0.0 }, ladder_w)),
                ticket,
            }
        }
        Panel::Under => {
            let ticket_h = under_ticket_h(m).min(body.height());
            let top_h = body.height() - ticket_h - m.gap;
            if top_h < 5.0 * m.row_h {
                return alone;
            }
            let ticket = Rect::from_min_max(pos2(body.min.x, body.max.y - ticket_h), body.max);
            let pane = |x: f32, w: f32| {
                Rect::from_min_size(pos2(body.min.x + x, body.min.y), vec2(w, top_h))
            };
            let w = body.width();
            let half = ((w - PANE_SEAM) / 2.0).floor();
            let (chart_w, ladder_w) = match (view.chart, view.ladder) {
                (true, true) if half >= MIN_CHART_W && w - half - PANE_SEAM >= MIN_LADDER_W => {
                    (half, w - half - PANE_SEAM)
                }
                (_, true) => (0.0, w),
                (true, false) => (w, 0.0),
                (false, false) => unreachable!("handled above"),
            };
            BodyRects {
                chart: (chart_w > 0.0).then(|| pane(0.0, chart_w)),
                ladder: (ladder_w > 0.0).then(|| {
                    pane(
                        if chart_w > 0.0 && chart_w < w { chart_w + PANE_SEAM } else { 0.0 },
                        ladder_w,
                    )
                }),
                ticket,
            }
        }
    }
}

/// Whether the ticket in `rects` is the compact one: the panel under, with a ladder or a chart over
/// it, or with neither asked for (the compact ticket's own column). A view whose ladder was left out
/// for want of room keeps the full ticket, which then takes the whole body.
pub fn compact_ticket(view: View, rects: &BodyRects) -> bool {
    view.panel == Panel::Under
        && (rects.ladder.is_some() || rects.chart.is_some() || (!view.ladder && !view.chart))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::{Panel, View};
    use egui::{Rect, pos2, vec2};
    use vike_ui_theme::metrics::Density;

    fn body(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h))
    }

    const fn view(chart: bool, ladder: bool, panel: Panel) -> View {
        View { chart, ladder, panel }
    }

    /// The spec's sizes, the floor a window's opening size grows from (`trade_fit.rs` measures the
    /// growth, which needs the type): the owner's v3 design's eight views, its windows' sizes less
    /// the one point or three its frame counts differently.
    #[test]
    fn the_spec_sizes_are_the_designs() {
        use Panel::{Beside, Under};
        assert_eq!(spec_size(view(false, true, Beside)), vec2(600.0, 560.0));
        assert_eq!(spec_size(view(true, true, Beside)), vec2(600.0 + CHART_W, 560.0));
        assert_eq!(spec_size(view(false, false, Beside)), TICKET_ONLY_SIZE);
        assert_eq!(spec_size(view(true, false, Beside)), vec2(TICKET_ONLY_SIZE.x + CHART_W, 560.0));
        assert_eq!(spec_size(view(false, true, Under)), vec2(320.0, 680.0));
        assert_eq!(spec_size(view(true, true, Under)), vec2(320.0 + CHART_W, 659.0));
        assert_eq!(spec_size(view(true, false, Under)), vec2(320.0, 680.0));
        assert_eq!(spec_size(view(false, false, Under)).x, COMPACT_ALONE_W);
    }

    /// The chart sits left of the ladder, on the ladder's own rows, and the ticket keeps its width;
    /// a window too narrow for both gives the chart up first.
    #[test]
    fn beside_puts_the_chart_left_of_the_ladder_and_drops_it_first() {
        let m = Density::Normal.metrics();
        let v = view(true, true, Panel::Beside);
        let r = split(body(CHART_W + 323.0 + TICKET_W + m.gap, 480.0), v, &m);
        let (chart, ladder) = (r.chart.expect("room for a chart"), r.ladder.expect("a ladder"));
        assert_eq!(chart.width(), CHART_W);
        assert_eq!((chart.min.y, chart.max.y), (ladder.min.y, ladder.max.y), "one set of rows");
        assert_eq!(ladder.min.x, chart.max.x + PANE_SEAM);
        assert!(ladder.max.x <= r.ticket.min.x);
        let narrow = split(body(MIN_LADDER_W + TICKET_W + m.gap + 20.0, 480.0), v, &m);
        assert_eq!(narrow.chart, None, "the chart gives way before the ladder");
        assert!(narrow.ladder.is_some());
        let wide = body(CHART_W + TICKET_W + m.gap, 480.0);
        let chart_only = split(wide, view(true, false, Panel::Beside), &m);
        assert!(chart_only.chart.is_some_and(|c| c.width() >= CHART_W), "the chart takes the rest");
        assert_eq!(chart_only.ladder, None);
    }

    /// Under, the chart and the ladder share the top, half the width each, and the compact ticket
    /// is under both; with neither, the compact ticket stands alone.
    #[test]
    fn under_puts_the_chart_and_the_ladder_side_by_side_over_the_compact_ticket() {
        let m = Density::Normal.metrics();
        let both = view(true, true, Panel::Under);
        let r = split(body(601.0, 600.0), both, &m);
        let (chart, ladder) = (r.chart.expect("a chart"), r.ladder.expect("a ladder"));
        assert_eq!((chart.width(), ladder.width()), (300.0, 300.0));
        assert!(compact_ticket(both, &r));
        let alone = view(false, false, Panel::Under);
        let r = split(body(301.0, 300.0), alone, &m);
        assert_eq!((r.chart, r.ladder, r.ticket), (None, None, body(301.0, 300.0)));
        assert!(compact_ticket(alone, &r), "the compact ticket's own column");
    }

    #[test]
    fn beside_puts_the_ladder_left_of_a_fixed_width_ticket() {
        let m = Density::Normal.metrics();
        let r = split(
            body(600.0, 480.0),
            View { chart: false, ladder: true, panel: Panel::Beside },
            &m,
        );
        let ladder = r.ladder.expect("a 600 pt body has room for a ladder");
        assert_eq!(r.ticket.width(), TICKET_W);
        assert!(ladder.max.x <= r.ticket.min.x, "the ladder ends before the ticket starts");
        assert_eq!(ladder.height(), 480.0);
        assert!(!compact_ticket(View { chart: false, ladder: true, panel: Panel::Beside }, &r));
    }

    #[test]
    fn under_puts_the_ladder_above_a_compact_ticket() {
        for d in Density::ALL {
            let m = d.metrics();
            let v = View { chart: false, ladder: true, panel: Panel::Under };
            let r = split(body(320.0, 600.0), v, &m);
            let ladder = r.ladder.expect("a 600 pt tall body has room for a ladder");
            assert_eq!(r.ticket.height(), under_ticket_h(&m), "{d:?}");
            assert!(ladder.max.y <= r.ticket.min.y, "{d:?}: the ladder ends above the ticket");
            assert!(compact_ticket(v, &r), "{d:?}");
        }
    }

    #[test]
    fn a_hidden_or_squeezed_ladder_leaves_the_ticket_the_whole_body() {
        let m = Density::Normal.metrics();
        let hidden = split(
            body(600.0, 480.0),
            View { chart: false, ladder: false, panel: Panel::Beside },
            &m,
        );
        assert_eq!((hidden.ladder, hidden.ticket), (None, body(600.0, 480.0)));
        let squeezed = split(
            body(TICKET_W + 50.0, 480.0),
            View { chart: false, ladder: true, panel: Panel::Beside },
            &m,
        );
        assert_eq!(squeezed.ladder, None, "no ladder narrower than MIN_LADDER_W is drawn");
        assert_eq!(squeezed.ticket, body(TICKET_W + 50.0, 480.0));
    }
}
