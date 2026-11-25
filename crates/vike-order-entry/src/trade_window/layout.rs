//! The Trade window's geometry — pure, so every rule is tested without an egui frame, but for
//! [`window_size`], which measures the type the window is drawn in.

use egui::{Rect, Vec2, pos2, vec2};
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::metrics::{ThemeDensityMetrics, space, stroke};
use vike_ui_theme::type_scale::{ThemeTextRole, ThemeTextSize};
use vike_ui_theme::value::trade;

use super::{TradeWindowPanes, TradeWindowTicketPlace, instrument_bar, status_line, ticket};

// This window's measures are `[[value]]` rows of `ui-theme.toml`, read as `trade::NAME`
// (`vike_ui_theme::value::trade`): the ticket's width beside the ladder at Small text
// (`trade::TICKET_WIDTH`, which [`ticket_w`] grows with the text), the narrowest ladder and chart drawn
// (`trade::MIN_LADDER_WIDTH`, `trade::MIN_CHART_WIDTH`), the chart pane's width (`trade::CHART_WIDTH`), the
// side-panel and bottom-panel windows (`trade::BESIDE_SIZE`, `trade::UNDER_SIZE`: spec §3.9), the
// compact ticket's column (`trade::COMPACT_ALONE_WIDTH`) and a view toggle (`trade::VIEW_BUTTON`).

/// The ticket alone: its width and the pad round it, over the design's height.
pub const TICKET_ONLY_SIZE: Vec2 =
    vec2(trade::TICKET_WIDTH + trade::TICKET_ONLY_PADDING, trade::TICKET_ONLY_HEIGHT);

/// How much bigger the text is than at Small, the scale the design was drawn at: 1.0 at Small, 1.1 at
/// Standard, 1.3 at Large. The instrument bar's thresholds (its private `scale`) and the ticket's
/// width grow by it.
pub(crate) fn text_scale(t: &ThemeTokens) -> f32 {
    t.text.px(ThemeTextRole::Caption) / ThemeTextSize::Small.px(ThemeTextRole::Caption)
}

/// The ticket's width beside the ladder in `t`'s look: [`trade::TICKET_WIDTH`] grown with the text (the owner's
/// pick "3b" on the mock-up of 2026-10-05), so the design's rows keep their proportions at the bigger
/// sizes. At a fixed 260 the five quick sizes wrapped three and two from Standard up.
///
/// The ladder does NOT pay for it: a window opens as much wider as the ticket grew ([`spec_size`]:
/// 26 pt at Standard, 78 pt at Large). The mock-up the owner chose from showed the ladder losing that
/// room in a 600 pt window; the build found it cannot, because the ladder's own order column then
/// holds 28.9 pt at Compact density and Large text and its `0.010` marker is 39 wide
/// (`ladder/tests.rs`'s `a_markers_count_and_a_single_size_fit_the_order_column_at_the_window_sizes`).
pub fn ticket_w(t: &ThemeTokens) -> f32 {
    (trade::TICKET_WIDTH * text_scale(t)).round()
}

/// The gap between two view toggles of a group (the design's `.ib`; a toggle is `trade::VIEW_BUTTON`).
/// They sit in the title bar (`vike_ui_theme::chrome`, 25 pt at every density), so they are the
/// design's size at every density too.
pub(crate) const VIEW_GAP: f32 = space::XS;
/// One pair of view toggles: two toggles and the gap between them.
const VIEW_PAIR_W: f32 = trade::VIEW_BUTTON.x + VIEW_GAP + trade::VIEW_BUTTON.x;
/// The width the four view toggles take in the bar's slot: two pairs, a group gap each side of a
/// hairline between them. Every piece is a named value (the toggle is `trade::VIEW_BUTTON`, the
/// gaps are `VIEW_GAP` and `trade::VIEW_GROUP_GAP`), so it spells the two pairs out rather than
/// multiplying: a number typed here would be a second copy of the layout.
pub const VIEW_CONTROLS_W: f32 =
    VIEW_PAIR_W + trade::VIEW_GROUP_GAP + stroke::HAIRLINE + trade::VIEW_GROUP_GAP + VIEW_PAIR_W;

/// What the desktop's tool window puts around the rect [`super::draw`] fills, across and down.
/// Across: its frame (`stroke::HAIRLINE`, each side) and its body's side margins (`space::LG`).
/// Down: its frame top and bottom, this window's title bar (`vike_ui_theme::chrome::TITLE_BAR_HEIGHT`,
/// density-free, and nothing between it and the body: `title_bar::body_gap`) and its body's bottom
/// margin (`space::MD`). A window `size` gives `draw` a rect `size - chrome()`.
///
/// ⚠ The frame and the margins are vike-app-core's
/// (`crates/vike-app-core/src/ui/window/state/mod.rs`'s `TOOL_BODY_MARGIN`), mirrored here
/// because this crate sits below the shell; vike-app-core's
/// `the_trade_windows_body_is_its_size_less_the_chrome_its_layout_assumes` holds this function to
/// the window it draws.
pub fn chrome() -> Vec2 {
    vec2(
        2.0 * (stroke::HAIRLINE + space::LG),
        2.0 * stroke::HAIRLINE + vike_ui_theme::chrome::TITLE_BAR_HEIGHT + space::MD,
    )
}

/// The size each view opens at by the spec (§3.9, the prototype's sizes): the floor
/// [`window_size`] grows from. The QA capture's window opens at it, before egui has the type to
/// measure anything with. `ticket_w` is the ticket's width in the look ([`ticket_w`]; [`trade::TICKET_WIDTH`] at
/// Small): the ticket-alone window is it and the pad round it, and a window with a ladder beside the
/// ticket is as much wider than the design's as the ticket grew.
pub fn spec_size(view: TradeWindowPanes, ticket_w: f32) -> Vec2 {
    let chart = if view.chart { trade::CHART_WIDTH } else { 0.0 };
    // The side-by-side windows open as much wider as the ticket grew, so the ladder keeps its width.
    let grown = ticket_w - trade::TICKET_WIDTH;
    match (view.ladder, view.panel) {
        (false, TradeWindowTicketPlace::Beside) => {
            vec2(ticket_w + trade::TICKET_ONLY_PADDING + chart, TICKET_ONLY_SIZE.y)
        }
        (true, TradeWindowTicketPlace::Beside) => {
            vec2(trade::BESIDE_SIZE.x + grown + chart, trade::BESIDE_SIZE.y)
        }
        // The compact ticket alone: the design's column, and a height `window_size` counts.
        (false, TradeWindowTicketPlace::Under) if !view.chart => {
            vec2(trade::COMPACT_ALONE_WIDTH, trade::UNDER_SIZE.y)
        }
        (false, TradeWindowTicketPlace::Under) => trade::UNDER_SIZE,
        // Chart and ladder side by side over the ticket: the design's 621 × 659.
        (true, TradeWindowTicketPlace::Under) if view.chart => vec2(
            trade::UNDER_SIZE.x + trade::CHART_WIDTH,
            trade::UNDER_SIZE.y - trade::UNDER_BOTH_LESS_HEIGHT,
        ),
        (true, TradeWindowTicketPlace::Under) => trade::UNDER_SIZE,
    }
}

/// The size a window opens at, and the size a view change asks for: [`spec_size`]'s width, and its
/// height or, where the full ticket needs more to show its WHOLE form, that (ruling B1 of the
/// render check, 2026-10-03: at Comfortable density TP/SL sat 61 pt below the fold of the 600 ×
/// 560 window). Counted as [`super::draw`] lays a window out, in `context`'s look and type: the
/// window around the body ([`chrome`]), the instrument bar, the ticket in the state a window opens
/// in (`ticket::full::opening_h`) and the status strip, no gap between them. The compact ticket under the ladder
/// has fixed rows and never scrolls, so that view keeps its size.
///
/// ⚠ The strip is counted at its least height (`status_line::idle_height`), the strip of the state a
/// window opens in: TP/SL starts off there, so nothing is refused. It does NOT count the growth of
/// a strip that says why the ticket sends nothing (`status_line::height`): a TP/SL ticked later and
/// refused on the account's lane takes three Caption lines in a 320 pt window and four in a 280 pt
/// one, 8.0 / 9.4 pt and 20.0 / 21.4 pt taller (Comfortable / Normal, Standard text; FW6), and a
/// full ticket's form gives that room up. A form that then scrolls by a few points is exactly the
/// case its divider exists for, so the opening height is not raised for a state no window opens in.
///
/// # Panics
///
/// With a context that has run no frame yet: egui has no fonts until one has
/// (`egui::Context::fonts_mut`), and the type is what this measures.
pub fn window_size(view: TradeWindowPanes, context: &egui::Context) -> Vec2 {
    let t = ThemeTokens::of(context);
    let spec = spec_size(view, ticket_w(&t));
    let m = &t.metrics;
    let chrome = chrome();
    let content_w = spec.x - chrome.x;
    let bar = instrument_bar::height(content_w, &t);
    let strip = status_line::idle_height(context, &t);
    // The body `draw` splits at the spec's size: what the bar and the strip leave.
    let body_h = (spec.y - chrome.y - bar - strip).max(0.0);
    let rects =
        split(Rect::from_min_size(pos2(0.0, 0.0), vec2(content_w, body_h)), view, m, ticket_w(&t));
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
    let ticket = ticket::full::opening_h(context, &t, rects.ticket.width());
    let h = chrome.y + bar + ticket + strip;
    vec2(spec.x, spec.y.max(h.ceil()))
}

/// The compact ticket's height at density `m`: [`ticket::compact::compact_h`], the height of the ticket with
/// its Buy and Sell stacked, one to a row. It is fixed before the ticket is drawn, so it holds the
/// stacked case; side by side it leaves a row's room empty under the cancel row.
pub(crate) fn under_ticket_h(m: &ThemeDensityMetrics) -> f32 {
    ticket::compact::compact_h(m)
}

/// The hairline between the tick chart and the ladder beside it: the chart paints its own edge there.
pub(crate) const PANE_SEAM: f32 = stroke::HAIRLINE;

/// Where the tick chart, the ladder and the ticket go inside the body. The chart and the ladder
/// share the same top and bottom, so the chart's price rows are the ladder's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TradeWindowBodyRects {
    pub chart: Option<Rect>,
    pub ladder: Option<Rect>,
    pub ticket: Rect,
}

/// Split `body` for `view` at density `m`, the ticket `ticket_w` wide beside the ladder. A ladder that would be narrower than [`trade::MIN_LADDER_WIDTH`],
/// or shorter than five rows, is not drawn, and the ticket takes the whole body. The chart gives way
/// first: where the room beside the ticket cannot hold a ladder and a chart of [`trade::MIN_CHART_WIDTH`], the
/// ladder keeps it.
pub fn split(
    body: Rect,
    view: TradeWindowPanes,
    m: &ThemeDensityMetrics,
    ticket_w: f32,
) -> TradeWindowBodyRects {
    let alone = TradeWindowBodyRects { chart: None, ladder: None, ticket: body };
    if !view.ladder && !view.chart {
        return alone;
    }
    match view.panel {
        TradeWindowTicketPlace::Beside => {
            // The ticket's region is `ticket_w` wide ([`ticket_w`]) and runs one padding past the body's right edge:
            // the ticket insets its content by that padding, so its controls end at the body's edge
            // as the design's do, and begin one padding right of the rule that parts it from the
            // ladder, which in turn stands one padding off the ladder's last column.
            let ticket_w = ticket_w.min(body.width() + m.padding);
            let ticket = Rect::from_min_max(
                pos2(body.max.x + m.padding - ticket_w, body.min.y),
                pos2(body.max.x + m.padding, body.max.y),
            );
            let room = ticket.min.x - m.padding - stroke::HAIRLINE - body.min.x;
            let pane = |x: f32, w: f32| {
                Rect::from_min_size(pos2(body.min.x + x, body.min.y), vec2(w, body.height()))
            };
            let (chart_w, ladder_w) = match (view.chart, view.ladder) {
                (true, true)
                    if room >= trade::MIN_LADDER_WIDTH + PANE_SEAM + trade::MIN_CHART_WIDTH =>
                {
                    let chart = trade::CHART_WIDTH.min(room - trade::MIN_LADDER_WIDTH - PANE_SEAM);
                    (chart, room - chart - PANE_SEAM)
                }
                (_, true) if room >= trade::MIN_LADDER_WIDTH => (0.0, room),
                (true, false) if room >= trade::MIN_CHART_WIDTH => (room, 0.0),
                _ => return alone,
            };
            TradeWindowBodyRects {
                chart: (chart_w > 0.0).then(|| pane(0.0, chart_w)),
                ladder: (ladder_w > 0.0)
                    .then(|| pane(chart_w + if chart_w > 0.0 { PANE_SEAM } else { 0.0 }, ladder_w)),
                ticket,
            }
        }
        TradeWindowTicketPlace::Under => {
            let ticket_h = under_ticket_h(m).min(body.height());
            let top_h = body.height() - ticket_h - m.gap;
            if top_h < 5.0 * m.row_height {
                return alone;
            }
            let ticket = Rect::from_min_max(pos2(body.min.x, body.max.y - ticket_h), body.max);
            let pane = |x: f32, w: f32| {
                Rect::from_min_size(pos2(body.min.x + x, body.min.y), vec2(w, top_h))
            };
            let w = body.width();
            let half = ((w - PANE_SEAM) / 2.0).floor();
            let (chart_w, ladder_w) = match (view.chart, view.ladder) {
                (true, true)
                    if half >= trade::MIN_CHART_WIDTH
                        && w - half - PANE_SEAM >= trade::MIN_LADDER_WIDTH =>
                {
                    (half, w - half - PANE_SEAM)
                }
                (_, true) => (0.0, w),
                (true, false) => (w, 0.0),
                (false, false) => unreachable!("handled above"),
            };
            TradeWindowBodyRects {
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
pub fn compact_ticket(view: TradeWindowPanes, rects: &TradeWindowBodyRects) -> bool {
    view.panel == TradeWindowTicketPlace::Under
        && (rects.ladder.is_some() || rects.chart.is_some() || (!view.ladder && !view.chart))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade_window::{TradeWindowPanes, TradeWindowTicketPlace};
    use egui::{Rect, pos2, vec2};
    use vike_ui_theme::metrics::ThemeDensity;
    use vike_ui_theme::type_scale::ThemeTextSize;

    fn body(w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(w, h))
    }

    const fn view(chart: bool, ladder: bool, panel: TradeWindowTicketPlace) -> TradeWindowPanes {
        TradeWindowPanes { chart, ladder, panel }
    }

    /// The spec's sizes, the floor a window's opening size grows from (`trade_fit.rs` measures the
    /// growth, which needs the type): the owner's v3 design's eight views, its windows' sizes less
    /// the one point or three its frame counts differently.
    #[test]
    fn the_spec_sizes_are_the_designs() {
        use TradeWindowTicketPlace::{Beside, Under};
        let spec = |v| spec_size(v, trade::TICKET_WIDTH);
        assert_eq!(spec(view(false, true, Beside)), vec2(600.0, 560.0));
        assert_eq!(spec(view(true, true, Beside)), vec2(600.0 + trade::CHART_WIDTH, 560.0));
        assert_eq!(spec(view(false, false, Beside)), TICKET_ONLY_SIZE);
        assert_eq!(
            spec(view(true, false, Beside)),
            vec2(TICKET_ONLY_SIZE.x + trade::CHART_WIDTH, 560.0)
        );
        assert_eq!(spec(view(false, true, Under)), vec2(320.0, 680.0));
        assert_eq!(spec(view(true, true, Under)), vec2(320.0 + trade::CHART_WIDTH, 659.0));
        assert_eq!(spec(view(true, false, Under)), vec2(320.0, 680.0));
        assert_eq!(spec(view(false, false, Under)).x, trade::COMPACT_ALONE_WIDTH);
    }

    /// The ticket grows with the text, as the owner chose (3b): the design's 260 at Small, 286 at
    /// Standard, 338 at Large — and the window a view opens at is as much wider, so the ladder keeps the
    /// width it has at Small at every size and density.
    #[test]
    fn the_ticket_grows_with_the_text_and_the_window_with_it() {
        use vike_ui_theme::appearance::ThemeAppearance;
        let widths: Vec<f32> = ThemeTextSize::ALL
            .map(|text_size| {
                let t = ThemeTokens::from_appearance(&ThemeAppearance {
                    text_size,
                    ..ThemeAppearance::default()
                });
                ticket_w(&t)
            })
            .into();
        assert_eq!(widths, [260.0, 286.0, 338.0], "Small, Standard, Large");
        assert_eq!(
            spec_size(view(false, true, TradeWindowTicketPlace::Beside), 286.0).x,
            600.0 + 26.0
        );
        assert_eq!(
            spec_size(view(true, true, TradeWindowTicketPlace::Beside), 338.0).x,
            600.0 + 78.0 + trade::CHART_WIDTH
        );
        let v = view(false, true, TradeWindowTicketPlace::Beside);
        for density in ThemeDensity::ALL {
            let small = ThemeTokens::from_appearance(&ThemeAppearance {
                density,
                ..ThemeAppearance::default()
            });
            let small_body = body(trade::BESIDE_SIZE.x - chrome().x, 480.0);
            let small_ladder = split(small_body, v, &small.metrics, trade::TICKET_WIDTH)
                .ladder
                .expect("a ladder at Small");
            for text_size in ThemeTextSize::ALL {
                let t = ThemeTokens::from_appearance(&ThemeAppearance {
                    density,
                    text_size,
                    ..ThemeAppearance::default()
                });
                let content_w = spec_size(v, ticket_w(&t)).x - chrome().x;
                let r = split(body(content_w, 480.0), v, &t.metrics, ticket_w(&t));
                let ladder =
                    r.ladder.unwrap_or_else(|| panic!("{density:?} {text_size:?}: no ladder"));
                assert_eq!(
                    ladder.width(),
                    small_ladder.width(),
                    "{density:?} {text_size:?}: the ladder keeps the width it has at Small"
                );
                assert_eq!(r.ticket.width(), ticket_w(&t), "{density:?} {text_size:?}");
            }
        }
    }

    /// The chart sits left of the ladder, on the ladder's own rows, and the ticket keeps its width;
    /// a window too narrow for both gives the chart up first.
    #[test]
    fn beside_puts_the_chart_left_of_the_ladder_and_drops_it_first() {
        let m = ThemeDensity::Normal.metrics();
        let v = view(true, true, TradeWindowTicketPlace::Beside);
        let r = split(
            body(trade::CHART_WIDTH + 323.0 + trade::TICKET_WIDTH + m.gap, 480.0),
            v,
            &m,
            trade::TICKET_WIDTH,
        );
        let (chart, ladder) = (r.chart.expect("room for a chart"), r.ladder.expect("a ladder"));
        assert_eq!(chart.width(), trade::CHART_WIDTH);
        assert_eq!((chart.min.y, chart.max.y), (ladder.min.y, ladder.max.y), "one set of rows");
        assert_eq!(ladder.min.x, chart.max.x + PANE_SEAM);
        assert!(ladder.max.x <= r.ticket.min.x);
        let narrow = split(
            body(trade::MIN_LADDER_WIDTH + trade::TICKET_WIDTH + m.gap + 20.0, 480.0),
            v,
            &m,
            trade::TICKET_WIDTH,
        );
        assert_eq!(narrow.chart, None, "the chart gives way before the ladder");
        assert!(narrow.ladder.is_some());
        let wide = body(trade::CHART_WIDTH + trade::TICKET_WIDTH + m.gap, 480.0);
        let chart_only =
            split(wide, view(true, false, TradeWindowTicketPlace::Beside), &m, trade::TICKET_WIDTH);
        assert!(
            chart_only.chart.is_some_and(|c| c.width() >= trade::CHART_WIDTH),
            "the chart takes the rest"
        );
        assert_eq!(chart_only.ladder, None);
    }

    /// Under, the chart and the ladder share the top, half the width each, and the compact ticket
    /// is under both; with neither, the compact ticket stands alone.
    #[test]
    fn under_puts_the_chart_and_the_ladder_side_by_side_over_the_compact_ticket() {
        let m = ThemeDensity::Normal.metrics();
        let both = view(true, true, TradeWindowTicketPlace::Under);
        let r = split(body(601.0, 600.0), both, &m, trade::TICKET_WIDTH);
        let (chart, ladder) = (r.chart.expect("a chart"), r.ladder.expect("a ladder"));
        assert_eq!((chart.width(), ladder.width()), (300.0, 300.0));
        assert!(compact_ticket(both, &r));
        let alone = view(false, false, TradeWindowTicketPlace::Under);
        let r = split(body(301.0, 300.0), alone, &m, trade::TICKET_WIDTH);
        assert_eq!((r.chart, r.ladder, r.ticket), (None, None, body(301.0, 300.0)));
        assert!(compact_ticket(alone, &r), "the compact ticket's own column");
    }

    #[test]
    fn beside_puts_the_ladder_left_of_a_fixed_width_ticket() {
        let m = ThemeDensity::Normal.metrics();
        let r = split(
            body(600.0, 480.0),
            TradeWindowPanes { chart: false, ladder: true, panel: TradeWindowTicketPlace::Beside },
            &m,
            trade::TICKET_WIDTH,
        );
        let ladder = r.ladder.expect("a 600 pt body has room for a ladder");
        assert_eq!(r.ticket.width(), trade::TICKET_WIDTH);
        assert!(ladder.max.x <= r.ticket.min.x, "the ladder ends before the ticket starts");
        assert_eq!(ladder.height(), 480.0);
        assert!(!compact_ticket(
            TradeWindowPanes { chart: false, ladder: true, panel: TradeWindowTicketPlace::Beside },
            &r
        ));
    }

    #[test]
    fn under_puts_the_ladder_above_a_compact_ticket() {
        for d in ThemeDensity::ALL {
            let m = d.metrics();
            let v = TradeWindowPanes {
                chart: false,
                ladder: true,
                panel: TradeWindowTicketPlace::Under,
            };
            let r = split(body(320.0, 600.0), v, &m, trade::TICKET_WIDTH);
            let ladder = r.ladder.expect("a 600 pt tall body has room for a ladder");
            assert_eq!(r.ticket.height(), under_ticket_h(&m), "{d:?}");
            assert!(ladder.max.y <= r.ticket.min.y, "{d:?}: the ladder ends above the ticket");
            assert!(compact_ticket(v, &r), "{d:?}");
        }
    }

    #[test]
    fn a_hidden_or_squeezed_ladder_leaves_the_ticket_the_whole_body() {
        let m = ThemeDensity::Normal.metrics();
        let hidden = split(
            body(600.0, 480.0),
            TradeWindowPanes { chart: false, ladder: false, panel: TradeWindowTicketPlace::Beside },
            &m,
            trade::TICKET_WIDTH,
        );
        assert_eq!((hidden.ladder, hidden.ticket), (None, body(600.0, 480.0)));
        let squeezed = split(
            body(trade::TICKET_WIDTH + 50.0, 480.0),
            TradeWindowPanes { chart: false, ladder: true, panel: TradeWindowTicketPlace::Beside },
            &m,
            trade::TICKET_WIDTH,
        );
        assert_eq!(squeezed.ladder, None, "no ladder narrower than MIN_LADDER_WIDTH is drawn");
        assert_eq!(squeezed.ticket, body(trade::TICKET_WIDTH + 50.0, 480.0));
    }
}
