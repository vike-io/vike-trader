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

/// The desktop tool window's frame, a point each side.
const FRAME: f32 = 1.0;
/// Its body's margin left and right.
const SIDE_MARGIN: f32 = 8.0;
/// Its title bar's height.
const BAR: f32 = 30.0;
/// What the title bar leaves under it before the body: its 4 pt item spacing, less the point its
/// window controls stop short of the bar's bottom edge (the bar's last row ends there).
const UNDER_BAR: f32 = 3.0;
/// Its body's margin at the bottom.
const BOTTOM_MARGIN: f32 = 6.0;

/// What the desktop's tool window puts around the rect [`super::draw`] fills, across and down, at
/// density `m`. Across: its frame each side and its body's side margins. Down: its frame top and
/// bottom, its title bar and what the bar leaves under it, the density's gap the desktop puts
/// between the bar and the body, and its body's bottom margin. A window `size` gives `draw` a
/// rect `size - chrome(m)`.
///
/// ⚠ Those numbers are vike-app-core's (`crates/vike-app-core/src/ui/workspace/title_bar.rs`'s
/// `BAR_H`, `crates/vike-app-core/src/ui/workspace/state.rs`'s `TOOL_BODY_MARGIN`), mirrored here
/// because this crate sits below the shell; vike-app-core's
/// `the_trade_windows_body_is_its_size_less_the_chrome_its_layout_assumes` holds this function to
/// the window it draws.
pub fn chrome(m: &Metrics) -> Vec2 {
    vec2(2.0 * (FRAME + SIDE_MARGIN), 2.0 * FRAME + BAR + UNDER_BAR + m.gap + BOTTOM_MARGIN)
}

/// The size each view opens at by the spec (§3.9, the prototype's sizes): the floor
/// [`window_size`] grows from. The QA capture's window opens at it, before egui has the type to
/// measure anything with.
pub fn spec_size(view: View) -> Vec2 {
    match (view.ladder, view.panel) {
        (false, _) => TICKET_ONLY_SIZE,
        (true, Panel::Beside) => BESIDE_SIZE,
        (true, Panel::Under) => UNDER_SIZE,
    }
}

/// The size a window opens at, and the size a view change asks for: [`spec_size`]'s width, and its
/// height or, where the full ticket needs more to show its WHOLE form, that (ruling B1 of the
/// render check, 2026-10-03: at Comfortable density TP/SL sat 61 pt below the fold of the 600 ×
/// 560 window). Counted as [`super::draw`] lays a window out, in `ctx`'s look and type: the
/// window around the body ([`chrome`]), the instrument bar, the ticket in the state a window opens
/// in (`ticket::opening_h`) and the status strip, a gap apart. The compact ticket under the ladder
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
    let chrome = chrome(m);
    let content_w = spec.x - chrome.x;
    let bar = instrument::height(content_w, &t);
    let strip = status::idle_height(ctx, &t);
    // The body `draw` splits at the spec's size: what the bar, the strip and their gaps leave.
    let body_h = (spec.y - chrome.y - bar - strip - 2.0 * m.gap).max(0.0);
    let rects = split(Rect::from_min_size(pos2(0.0, 0.0), vec2(content_w, body_h)), view, m);
    if compact_ticket(view, &rects) {
        return spec;
    }
    let ticket = ticket::opening_h(ctx, &t, rects.ticket.width());
    let h = chrome.y + bar + m.gap + ticket + m.gap + strip;
    vec2(spec.x, spec.y.max(h.ceil()))
}

/// The compact ticket's rows, at most: the position strip, the size, the quick sizes, the toggles,
/// Buy and Sell (a row each where both labels do not fit side by side, else one), and the cancel
/// row. The ticket's height is fixed before it is drawn, so it holds the stacked case; side by side
/// leaves the seventh row empty under the cancel row.
const UNDER_ROWS: f32 = 7.0;

/// The compact ticket's height at density `m`.
pub fn under_ticket_h(m: &Metrics) -> f32 {
    UNDER_ROWS * m.control_h + (UNDER_ROWS - 1.0) * m.gap + 2.0 * m.pad
}

/// Where the ladder and the ticket go inside the body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyRects {
    pub ladder: Option<Rect>,
    pub ticket: Rect,
}

/// Split `body` for `view` at density `m`. A ladder that would be narrower than [`MIN_LADDER_W`],
/// or shorter than five rows, is not drawn, and the ticket takes the whole body.
pub fn split(body: Rect, view: View, m: &Metrics) -> BodyRects {
    let alone = BodyRects { ladder: None, ticket: body };
    if !view.ladder {
        return alone;
    }
    match view.panel {
        Panel::Beside => {
            let ticket_w = TICKET_W.min(body.width());
            let ladder_w = body.width() - ticket_w - m.gap;
            if ladder_w < MIN_LADDER_W {
                return alone;
            }
            BodyRects {
                ladder: Some(Rect::from_min_size(body.min, vec2(ladder_w, body.height()))),
                ticket: Rect::from_min_max(pos2(body.max.x - ticket_w, body.min.y), body.max),
            }
        }
        Panel::Under => {
            let ticket_h = under_ticket_h(m).min(body.height());
            let ladder_h = body.height() - ticket_h - m.gap;
            if ladder_h < 5.0 * m.row_h {
                return alone;
            }
            BodyRects {
                ladder: Some(Rect::from_min_size(body.min, vec2(body.width(), ladder_h))),
                ticket: Rect::from_min_max(pos2(body.min.x, body.max.y - ticket_h), body.max),
            }
        }
    }
}

/// Whether the ticket in `rects` is the compact one: under a ladder that is drawn.
pub fn compact_ticket(view: View, rects: &BodyRects) -> bool {
    view.panel == Panel::Under && rects.ladder.is_some()
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

    /// The spec's sizes, the floor a window's opening size grows from (`trade_fit.rs` measures the
    /// growth, which needs the type).
    #[test]
    fn the_spec_sizes_are_the_prototypes() {
        assert_eq!(spec_size(View { ladder: true, panel: Panel::Beside }), vec2(600.0, 560.0));
        assert_eq!(spec_size(View { ladder: true, panel: Panel::Under }), vec2(320.0, 680.0));
        assert_eq!(spec_size(View { ladder: false, panel: Panel::Beside }), TICKET_ONLY_SIZE);
        assert_eq!(spec_size(View { ladder: false, panel: Panel::Under }), TICKET_ONLY_SIZE);
    }

    #[test]
    fn beside_puts_the_ladder_left_of_a_fixed_width_ticket() {
        let m = Density::Normal.metrics();
        let r = split(body(600.0, 480.0), View { ladder: true, panel: Panel::Beside }, &m);
        let ladder = r.ladder.expect("a 600 pt body has room for a ladder");
        assert_eq!(r.ticket.width(), TICKET_W);
        assert!(ladder.max.x <= r.ticket.min.x, "the ladder ends before the ticket starts");
        assert_eq!(ladder.height(), 480.0);
        assert!(!compact_ticket(View { ladder: true, panel: Panel::Beside }, &r));
    }

    #[test]
    fn under_puts_the_ladder_above_a_compact_ticket() {
        for d in Density::ALL {
            let m = d.metrics();
            let v = View { ladder: true, panel: Panel::Under };
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
        let hidden = split(body(600.0, 480.0), View { ladder: false, panel: Panel::Beside }, &m);
        assert_eq!((hidden.ladder, hidden.ticket), (None, body(600.0, 480.0)));
        let squeezed =
            split(body(TICKET_W + 50.0, 480.0), View { ladder: true, panel: Panel::Beside }, &m);
        assert_eq!(squeezed.ladder, None, "no ladder narrower than MIN_LADDER_W is drawn");
        assert_eq!(squeezed.ticket, body(TICKET_W + 50.0, 480.0));
    }
}
