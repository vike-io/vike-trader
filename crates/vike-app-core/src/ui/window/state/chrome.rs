//! The per-window chrome loop: the body wrapper `WindowBodyBounds`, `show_window` and its edge bands.

use egui::{Id, LayerId, Order, Rect, Vec2};
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::workspace;

use super::{WindowKind, WindowState};

/// The body wrapper [`show_window`] hands to a window's draw closure: call [`WindowBodyBounds::show`]
/// around the window's BODY (below its custom title bar) and the body is bounded by the window —
/// it wraps or scrolls inside it and never widens or lengthens it (every window is app-owned
/// from open; see [`show_window`]).
///
/// ⚠ This exists so the wrap lives in the crate the gate compiles. It was spelled at the
/// vike-desktop call site as a bare `if scrolls { ScrollArea… } else { body(ui) }`, and
/// `vike-desktop` is in `xtask::ci::tables::roster`'s `EXCLUDE_FROM_CI` while `app-check` runs only
/// `cargo nextest run -p vike-desktop` (which reaches `chart_gpu.rs` and nothing else) — so every
/// mutation the tests below claim for the scroll half was a mutation of the TEST HARNESS, and
/// deleting `auto_shrink(false)` from production left all of them green.
///
/// ⚠ **A `fills` kind takes NO container at all** — [`Self::show`] hands the body straight
/// through — and that is the same id-chain argument one step further out. Only THREE kinds fill
/// (Chart, Trade, Polymarket) and only ONE of them is drawn at the chart call site that passes the
/// body no wrapper; Trade and Polymarket are dispatched from the TOOL call site, which does call
/// [`Self::show`]. Wrapping them would put an extra `Ui` level into the id chain of the Trade window
/// and the Polymarket cockpit — discarding every egui-persisted widget state inside them ONCE, on
/// upgrade, which is the exact failure this type exists to avoid. A `fills` body also needs no
/// container: it sizes itself to the available rect, so there is nothing to bound. Gated by
/// `the_tool_sites_fills_body_is_handed_through_unwrapped`.
///
/// ⚠ For a NON-`fills` kind the container is ALWAYS the same `ScrollArea`, at the same place in the
/// id chain, from the window's first frame: egui discards auto-id-keyed widget state on an id
/// change (the Studio code editor's cursor, selection and UNDO STACK, every inner scroll offset,
/// every `CollapsingState`, a half-typed field in the Connections editor), so a container that came
/// and went would throw all of that away. Gated by `the_bodys_id_is_stable_across_a_resize`.
pub struct WindowBodyBounds {
    /// Whether the body is wrapped in its bounding `ScrollArea`: every kind but the three that
    /// FILL (`WindowKind::Chart | Trade | Polymarket`), which take no container at all — see this
    /// type's doc.
    scrolls: bool,
}

impl WindowBodyBounds {
    /// Lay out a window's body inside the bounding container this window needs.
    ///
    /// The `ScrollArea` goes HERE — around the BODY, below the window's custom title bar — and
    /// never via `Window::scroll(true)`: egui's `Window::show_dyn` calls `scroll.show(ui,
    /// add_contents)` around the ENTIRE closure, which would scroll vike's own title bar (its
    /// move-drag and its close button) out of reach on an overflowing window.
    ///
    /// `auto_shrink(false)` is load-bearing: `scroll_area.rs` resolves the `(scroll_enabled,
    /// auto_shrink)` pair `(true, false)` to `inner_size[d]` — "let scroll area be larger than
    /// content; fill with blank space" — so the body reports the WINDOW's size rather than its
    /// content's (a `Window` is painted at its content, `Resize::end`'s `size[d] =
    /// state.last_content_size[d]`): a short body does not shrink the window and a wide one does
    /// not widen it. Text in the body wraps at the window's width, because a `ScrollArea` offers
    /// its content the inner rect to fit before it scrolls.
    ///
    /// ⚠ A `fills` kind returns BEFORE any of that: `body(ui)` on the caller's own `Ui`, no
    /// container, no extra id level — byte-identical to the unwrapped spelling the DOM (now Trade) and
    /// Polymarket bodies were dispatched under before this wrapper existed.
    pub fn show<R>(&self, ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
        if !self.scrolls {
            return body(ui);
        }
        egui::ScrollArea::both().auto_shrink(false).show(ui, body).inner
    }

    /// Whether this window's body is wrapped in its bounding `ScrollArea`. Tests assert on it; no
    /// production caller needs to branch on it (that is [`Self::show`]'s job).
    pub fn scrolls(&self) -> bool {
        self.scrolls
    }
}

/// The egui temp-data key, beside a chart window's id, under which [`show_window`] keeps the
/// `(venue, symbol)` the chart shows, for [`focused_chart`].
const CHART_SERIES: &str = "chart_series";

/// **The focused chart: the `(venue, symbol)` of the chart window highest in egui's window
/// order**, or `None` while no chart is on screen. A click or a drag moves a window to the top
/// (`egui::Context::move_to_top`), so this is the chart the operator last touched, the same order
/// the desktop's Alt+L reads; a window that is not a chart never counts, so focusing the News
/// window that follows it keeps the chart below. The News window's Follow chart reads it
/// (`crate::ui::windows::WindowToolContext::news_inputs`).
pub fn focused_chart(context: &egui::Context) -> Option<(String, String)> {
    context.memory(|m| {
        m.layer_ids()
            .filter(|l| l.order == Order::Middle && m.areas().is_visible(l))
            .filter_map(|l| m.data.get_temp(l.id.with(CHART_SERIES)))
            .last()
    })
}

/// Show one window with a CUSTOM single-row title bar (no egui title bar): consume any forced
/// geometry, draw, read the live rect back. `draw` renders the title bar (first row) + content and
/// RETURNS the title bar's drag delta in screen points, which we apply to the window position;
/// geometry is app-controlled via `current_pos` so the custom bar drives movement. Its second
/// argument is the [`WindowBodyBounds`] this window's BODY must be laid out through.
/// Returns `true` if the user dragged this window's title bar this frame (a manual move),
/// so the caller can drop any remembered tiling mode (don't re-tile a hand-placed layout).
///
/// ⚠ `bounds` is a CEILING as well as a clamp, and that is this function's job rather than
/// egui's: egui bounds what it PAINTS (an `Area`'s content `Ui` is clipped to its `constrain_rect`)
/// but it does not bound how large a `Window` is LAID OUT, so a window larger than the arena would
/// put its far edges — and therefore `window_resize_handles`' bands — outside that clip, where
/// `egui-0.37.0/src/hit_test.rs` drops them and no drag can reach them. Every window is pinned at
/// `w.size`, capped here at the arena, and a tool window is placed so it fits.
pub fn show_window(
    context: &egui::Context,
    w: &mut WindowState,
    bounds: Rect,
    draw: impl FnOnce(&mut egui::Ui, &WindowBodyBounds) -> egui::Vec2,
) -> bool {
    if w.kind == WindowKind::Chart {
        let series = (w.venue.clone(), w.symbol.clone());
        context.data_mut(|d| d.insert_temp(w.id.with(CHART_SERIES), series));
    }
    let forced = w.pending.take();
    if let Some(rect) = forced {
        w.screen_position = rect.min;
        w.size = rect.size();
        w.force_frames = 4; // the bands stay off while the arranged geometry settles
    }
    // ⚠ THE ARENA CEILING (size half). No window may be laid out LARGER than the arena it lives
    // in — `w.size` is authoritative for every window (each is pinned at it), and a band drag can
    // push it past `bounds` on either axis in one gesture.
    // egui clamps what it PAINTS regardless (`Window::show_dyn` does
    // `resize.max_size = resize.max_size.min(constrain_rect.size())` before the frame margin comes
    // off), so leaving the oversized value in `w.size` only makes the two DISAGREE: the bands
    // anchor on the painted rect while the next drag's `w.size.y + d.y` starts from a number
    // nobody can see, and the whole divergence is released in one jump — the same failure the
    // both-axes floor inside the resize block below exists to prevent, one bound over.
    //
    // Floored at `MIN_WIDTH`×`MIN_HEIGHT` so a degenerate arena (an app window dragged tiny, a frame taken
    // mid-restore) cannot squash every window to nothing IRREVERSIBLY, and skipped outright for a
    // non-positive `bounds` — `Rect::NOTHING`'s width is `-inf`, and `w.size` must not be built
    // out of that.
    if bounds.is_positive() {
        w.size = w.size.min(egui::vec2(
            bounds.width().max(workspace::MIN_WIDTH),
            bounds.height().max(workspace::MIN_HEIGHT),
        ));
    }
    // Keep the window inside the workspace: its top can't go above the main caption's bottom,
    // nor past the status bar / rail (ctx.content_rect doesn't exclude our sub-ui caption panel).
    w.screen_position.x = w
        .screen_position
        .x
        .clamp(bounds.min.x, (bounds.max.x - workspace::MIN_VISIBLE_WIDTH).max(bounds.min.x));
    w.screen_position.y = w
        .screen_position
        .y
        .clamp(bounds.min.y, (bounds.max.y - workspace::MIN_VISIBLE_HEIGHT).max(bounds.min.y));
    // A MAXIMIZED window is the full bounds every frame, until restored.
    if w.maximized {
        w.screen_position = bounds.min;
        w.size = bounds.size();
    }
    // FILL-window kinds (Chart plot, Trade window, Polymarket cockpit) size their body to exactly
    // the rect they are offered (the chart's `leftover` fill; the Trade window allocates its whole
    // rect once; the cockpit's ladder sizes its region to `available_rect_before_wrap`), so they take
    // NO body container — see [`WindowBodyBounds`].
    let fills = matches!(w.kind, WindowKind::Chart | WindowKind::Trade | WindowKind::Polymarket);
    // ⚠ EVERY window is app-owned from the frame it opens (the owner's ruling of 2026-11-16, "a
    // window keeps its own size"): `current_pos` + `fixed_size` from `w.screen_position`/`w.size`, which only
    // the spawn (the kind's default size), arrange/maximize, the title-bar move and the edge bands
    // below ever write. A tool window's body therefore BOUNDS itself (`scrolls`): it wraps or
    // scrolls inside the window and never widens or lengthens it. Until then an unlatched tool
    // window was egui-sized — `default_size`, its geometry read back from egui each frame, growing
    // to whatever its body laid out — which is how News grew to the whole screen on one-line
    // headlines, the Calendar crept right on open, and the Tearsheet shifted when a picker changed
    // its content's width.
    //
    // Placed so it FITS, too, beside the overhang the clamp above allows: a pinned window is
    // `current_pos`, so `Context::constrain_window_rect_to_area` would move the PAINTED window back
    // inside while `w.screen_position` went on describing a window nobody sees, and the bands (anchored on the
    // painted rect) would answer for a different window than the drag arithmetic. The size half of
    // that cap is the ARENA CEILING at the top of this function.
    if !fills && !w.maximized && bounds.is_positive() {
        w.screen_position = w.screen_position.min(bounds.max - w.size).max(bounds.min);
    }
    let body_bounds = WindowBodyBounds { scrolls: !fills };
    let win = egui::Window::new("")
        .id(w.id)
        .title_bar(false) // custom title bar lives inside the content
        .movable(false) // we move the window via the custom bar's drag
        // ⚠ NOT a sizing change. egui 0.37's `Window::show_dyn` does `let resize =
        // resize.resizable(false); // We resize it manually` UNCONDITIONALLY, on the line right
        // after `PossibleInteractions::new(&area, &resize, is_collapsed)` reads it — so the builder
        // flag feeds ONLY whether egui registers its own edge widgets. Turning it off removes the
        // mechanism that would fight our own bands below — and egui could never have served the
        // LEFT edge here anyway: `PossibleInteractions::new` reads `resize_left: resizable.x &&
        // (movable || pivot.x() != Align::LEFT)`, and these windows are `.movable(false)` with the
        // default LEFT_TOP pivot.
        .resizable(false)
        .constrain_to(bounds)
        .current_pos(w.screen_position)
        .fixed_size(w.size);

    // Custom edge/corner resize for EVERY kind (egui's own native resize is off — see the
    // `.resizable(false)` note above, and note that egui structurally refuses the left edge for a
    // non-movable LEFT_TOP-pivot window, so the bands are the only thing that can serve it).
    // Disabled while maximized or during a forced-geometry pin (arrange/restore settle).
    let resizable_now = !w.maximized && w.force_frames == 0;
    let wid = w.id;
    let mut resize: Option<(u8, egui::Vec2)> = None;
    let mut drag = egui::Vec2::ZERO;
    let _ = win.show(context, |ui| {
        drag = draw(ui, &body_bounds);
        if resizable_now {
            resize = window_resize_handles(ui, wid);
        }
    });
    if let Some((mask, d)) = resize {
        if mask & 2 != 0 {
            w.size.x = (w.size.x + d.x).max(workspace::MIN_WIDTH);
        }
        if mask & 8 != 0 {
            w.size.y = (w.size.y + d.y).max(workspace::MIN_HEIGHT);
        }
        if mask & 1 != 0 {
            let nx = (w.size.x - d.x).max(workspace::MIN_WIDTH);
            w.screen_position.x += w.size.x - nx;
            w.size.x = nx;
        }
        // ⚠ The AXIS the pressed band actually consumes, not the whole delta. Each arm above reads
        // exactly one component — the side bands `d.x`, the bottom band `d.y` — so a press on a
        // side band that jitters only VERTICALLY (or on the bottom band, only horizontally)
        // resizes NOTHING. Testing `d != Vec2::ZERO` latched the window on precisely that motion:
        // the same irreversible conversion the zero-delta guard below exists to prevent, one axis
        // over, and a pointer that moves perfectly straight for one frame is the exception rather
        // than the rule. Gated by `cross_axis_jitter_on_a_band_does_not_latch_the_window`.
        let moved_the_edge = (mask & (1 | 2) != 0 && d.x != 0.0) || (mask & 8 != 0 && d.y != 0.0);
        if !fills && moved_the_edge {
            // RECORD (see `WindowState::user_sized`): the user chose this tool window's size. Guarded
            // on `!fills`, which keeps its old meaning: a `fills` kind never takes the record.
            //
            // ⚠ `moved_the_edge` subsumes the NON-ZERO guard this started as (a zero delta moves no
            // component): `window_resize_handles` returns `Some` on every `resp.dragged()` frame,
            // and egui marks a `Sense::drag()` widget dragged on the PRESS frame, where
            // `drag_delta()` is zero, so one accidental CLICK on an 8pt edge must record nothing.
            //
            // BOTH axes are floored here, not just the one the pressed band consumed (each arm above
            // floors only its own), so a side drag never leaves the other axis under `MIN_HEIGHT`/`MIN_WIDTH`.
            w.size = w.size.max(egui::vec2(workspace::MIN_WIDTH, workspace::MIN_HEIGHT));
            w.user_sized = true;
        }
        // ⚠ There is deliberately NO `force_frames` re-pin here: `resize` is `Some` only when
        // `resizable_now`, which requires `force_frames == 0`, and the window is pinned at `w.size`
        // every frame anyway, so the new size commits on the next frame by itself. The move path's
        // own `.max(1)` below is a different case: it keeps the bands off while a dragged window
        // settles.
    }
    if w.force_frames > 0 {
        w.force_frames -= 1;
    }
    let moved = drag != egui::Vec2::ZERO;
    if moved {
        w.screen_position += drag;
        w.force_frames = w.force_frames.max(1); // re-pin so the dragged pos is applied next frame
    }
    if forced.is_some() {
        context.move_to_top(LayerId::new(Order::Middle, w.id));
    }
    moved
}

/// Custom edge/corner resize handles for EVERY window kind (native egui resize is off).
///
/// ⚠ This used to say "for a pinned chart window" and served only the `fills` kinds. It is now
/// the ONLY resize mechanism any window has, tool windows included: egui 0.37's
/// `PossibleInteractions::new` reads `resize_left: resizable.x && (movable || pivot.x() !=
/// Align::LEFT)`, and every window here is built `.movable(false)` with the default LEFT_TOP
/// pivot, so egui structurally refuses the LEFT edge no matter how the builder is spelled.
///
/// Allocates invisible drag-sensing bands along the LEFT, RIGHT and BOTTOM edges + the two
/// BOTTOM corners of the content rect, drawn AFTER the body so they win the interaction over the
/// content underneath. Returns `(edge_mask, drag_delta)` for the active handle this frame:
/// bit 1 = left, 2 = right, 8 = bottom. `show_window` applies the delta to `w.screen_position`/`w.size`.
///
/// ⚠ **The bands are anchored on `ui.min_rect()`, NOT `ui.max_rect()`, and that is load-bearing.**
/// This function runs after the body has been laid out, so `min_rect` is the content drawn this
/// frame — and for an egui `Window` that IS the painted rect. `Window::show_dyn` builds its
/// `Resize` with `.with_stroke(false)` and then forces `let resize = resize.resizable(false); //
/// We resize it manually` unconditionally, so `Resize::end`'s `if self.with_stroke ||
/// self.resizable[d]` loop always takes the `else { size[d] = state.last_content_size[d]; }` arm,
/// and `last_content_size` is `content_ui.min_size()` — this `min_rect`'s size. `max_rect` is
/// `Resize::begin`'s `state.desired_size`, so wherever a body is shorter than its box the two
/// diverge, and a `max_rect` bottom band and both bottom corners would sit BELOW the visible
/// window: an invisible 8pt `Sense::drag` strip floating in dead space, showing a resize cursor and
/// swallowing clicks on whatever was underneath. (A tool body bounded by [`WindowBodyBounds`] fills its
/// window, so for it the two agree.)
///
/// ⚠ **Chart, Trade and Polymarket are all band-identical to the `max_rect` spelling, but only the
/// CHART is so for the reason this doc used to give.** It said "a `fills` body covers `max_rect`
/// exactly", which is true of the chart alone: `vike_chart::chart::draw` ends with
/// `add_space(ui.clip_rect().bottom().min(ui.max_rect().bottom()) - ui.cursor().top())`, and
/// `cursor().top()` is already past every `item_spacing.y` gap it laid, so its `min_rect` lands
/// exactly on `max_rect` (gated by
/// `a_chart_windows_band_anchor_is_byte_identical_to_the_old_max_rect_one`).
///
/// The Polymarket cockpit's LADDER body OVERRUNS instead (so did the DOM's, which is gone; the Trade
/// window allocates its whole rect once and does not).
/// `vike_order_entry::cockpit::probability_ladder::draw_probability_ladder` reads
/// `ui.available_rect_before_wrap()` ONCE at the top and then lays out sibling allocations SUMMING
/// to that height — `ladder_header` the density's header height, 24 at Normal, and the region — so
/// the `item_spacing.y` egui inserts BETWEEN siblings was never subtracted and is pure overrun, by
/// `(siblings - 1) * item_spacing.y`: at the app's `item_spacing` (`(6, 4)`, set by
/// `vike_ui_theme::appearance::install` at the default density) that is 4pt
/// for the Polymarket cockpit. **Its bands do not move for it**, because
/// `egui-0.37.0/src/layout.rs`'s
/// `Region::expand_to_include_rect` unions each child's rect into `min_rect` AND `max_rect` alike
/// — "`max_rect` will always be at least the size of `min_rect`", per `Region::max_rect`'s own doc
/// — so by the time this function runs the overrun is in both and the two anchors agree. Gated by
/// `a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical`, which asserts the
/// overrun against the rect the body was OFFERED and then asserts the bands anyway.
///
/// ⚠ That union is also why the anchor swap is a one-directional fix, and a review round read it
/// the other way round (that the ladders' `max_rect` bands sat 12pt INBOARD of the painted edge —
/// they did not, and nothing moved). `max_rect ⊇ min_rect` always, so a `max_rect` band can only
/// ever sit at-or-OUTSIDE the painted edge. The two can diverge only where a body UNDER-fills its
/// box, which no `fills` kind can reach.
///
/// ⚠ **No window paints too short to carry its bands**, and that is why there is no fallback onto
/// `max_rect` any more: these bands are the only resize mechanism any window has, so "no bands"
/// would mean "unresizable by any means". A `fills` body covers at least its `max_rect`, and a tool
/// body bounded by [`WindowBodyBounds`] paints at least its scroll area's 64pt `min_scrolled_size` below
/// the title bar, so the painted rect is never smaller than `max_rect` and always clears the guard
/// below for a window `show_window` can lay out. Gated by
/// `a_window_too_short_for_its_bands_is_still_resizable`.
///
/// ⚠ **A band that is ALLOCATED is not a band that can be HIT, and nothing in this function can
/// tell the difference.** `ui.interact` records `interact_rect: self.clip_rect().intersect(rect)`,
/// an `Area`'s content `Ui` is clipped to its `constrain_rect` (`area.rs`:
/// `ui.set_clip_rect(self.constrain_rect)` — "Don't paint outside our bounds"), and
/// `egui-0.37.0/src/hit_test.rs` drops any widget whose `interact_rect.is_negative()` before it
/// measures a single distance. So a band placed outside the workspace bounds is registered,
/// answers `Context::read_response`, sets a resize cursor on nothing, and can never be dragged.
/// That was the reported "why can't I resize it at the bottom, the status bar is over the window":
/// a tool window then grew to a body taller than the arena. The cure is upstream of this function,
/// in [`show_window`]: every window is pinned at a size capped at the arena and a tool window is
/// placed so it fits, so every band it allocates is inside the clip. Gated by
/// `a_body_taller_than_the_arena_keeps_the_window_and_its_bands_inside_it`.
///
/// **The TOP edge is deliberately skipped, and there is no bit 4.** It overlaps the title bar's
/// move-drag and its control buttons — and the cost is not just the drag: a drag-only band
/// suppresses CLICKS as well, because `egui-0.37.0/src/hit_test.rs` returns only the drag-widget
/// when one is on top ("The drag-widget is separate from the click-widget, so return only the
/// drag-widget" → `click: None`). A top band would therefore kill double-click-to-maximize and
/// the top slice of every control button on every window kind. The side bands start below
/// the title bar (`workspace::TITLE_CLEAR`) for the same reason. This is an owner's ruling, not an
/// oversight: do not add a top band, and do not widen the side bands over the title bar.
pub(super) fn window_resize_handles(ui: &egui::Ui, wid: Id) -> Option<(u8, Vec2)> {
    use egui::{CursorIcon, Rect, Sense, pos2};
    const BAND: f32 = space::LG; // the side inset of `TOOL_BODY_MARGIN`, which the bands sit in
    let title_clear = workspace::TITLE_CLEAR; // keep side bands clear of the title-bar controls
    // Room for a non-overlapping band set: a bottom band between two corners, and side bands that
    // start below `workspace::TITLE_CLEAR` and are still at least `BAND` tall above the bottom one.
    let fits = |r: Rect| r.width() >= 3.0 * BAND && r.height() >= title_clear + 2.0 * BAND;
    let r = ui.min_rect(); // the PAINTED content rect — see this function's doc
    if !fits(r) {
        return None; // too small to place non-overlapping bands
    }
    let bl = Rect::from_min_max(pos2(r.min.x, r.max.y - BAND), pos2(r.min.x + BAND, r.max.y));
    let br = Rect::from_min_max(pos2(r.max.x - BAND, r.max.y - BAND), pos2(r.max.x, r.max.y));
    let left = Rect::from_min_max(
        pos2(r.min.x, r.min.y + title_clear),
        pos2(r.min.x + BAND, r.max.y - BAND),
    );
    let right = Rect::from_min_max(
        pos2(r.max.x - BAND, r.min.y + title_clear),
        pos2(r.max.x, r.max.y - BAND),
    );
    let bottom =
        Rect::from_min_max(pos2(r.min.x + BAND, r.max.y - BAND), pos2(r.max.x - BAND, r.max.y));
    // Corners FIRST so a corner press is not stolen by an edge band.
    for (mask, rect) in [(1u8 | 8, bl), (2u8 | 8, br), (1u8, left), (2u8, right), (8u8, bottom)] {
        let resp = ui.interact(rect, wid.with(("winresize", mask)), Sense::drag());
        if resp.hovered() || resp.dragged() {
            let cursor = match mask {
                m if m == 1 | 8 => CursorIcon::ResizeNeSw, // bottom-left
                m if m == 2 | 8 => CursorIcon::ResizeNwSe, // bottom-right
                m if m & (1 | 2) != 0 => CursorIcon::ResizeHorizontal,
                _ => CursorIcon::ResizeVertical,
            };
            ui.ctx().set_cursor_icon(cursor);
        }
        if resp.dragged() {
            return Some((mask, resp.drag_delta()));
        }
    }
    None
}
