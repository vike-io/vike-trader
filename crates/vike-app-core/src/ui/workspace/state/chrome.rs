//! The per-window chrome loop: the body wrapper `BodyBounds`, `show_window` and its edge bands.

use egui::{Id, LayerId, Order, Rect, Vec2};
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::workspace;

use super::{CEILING_SLACK, WinKind, WinState};

/// The body wrapper [`show_window`] hands to a window's draw closure: call [`BodyBounds::show`]
/// around the window's BODY (below its custom title bar) and the window is bounded correctly in
/// both of its states.
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
/// ⚠ For a NON-`fills` kind the container is created UNCONDITIONALLY and only its SIZING is
/// toggled — that is the whole
/// point, not a simplification. A `ScrollArea` inserted when the latch flips would change `ui.id`
/// for the entire body, and egui discards auto-id-keyed widget state on an id change: the Studio
/// code editor's cursor, selection and UNDO STACK, every inner scroll offset, every
/// `CollapsingState`, a half-typed field in the Connections editor — all thrown away the first
/// time a user dragged an edge. VERIFIED in `egui-0.37.0/src/containers/scroll_area.rs`: with both
/// directions disabled and `auto_shrink` on, `ScrollArea::begin` computes `current_bar_use` from
/// `show_bars` (false, since `Prepared::end`'s `content_is_too_large` ANDs in `direction_enabled`)
/// so no space is reserved, `content_clip_rect` takes the `else` arm for every disabled direction
/// and inherits the parent clip verbatim, `state.offset` is clamped to
/// `scroll_bounds(content_size - inner_rect.size())` = `0..=0`, and `Prepared::end`'s
/// `(false, true) => content_size[d]` arm makes the reserved `outer_rect` exactly the content size
/// — "follow the content", the same rect an unwrapped body would have advanced the cursor past.
pub struct BodyBounds {
    /// Whether this window's kind FILLS (`WinKind::Chart | Trade | Polymarket`). Such a body takes
    /// no container at all — see this type's doc.
    fills: bool,
    /// Whether this window is app-owned — by the user's own drag ([`WinState::user_sized`]) or by
    /// the ARENA CEILING ([`WinState::arena_bounded`]) — and so will NOT grow for its body any
    /// more, which is what makes the body bound itself. Always false for a `fills` kind.
    scrolls: bool,
}

impl BodyBounds {
    /// Lay out a window's body inside the bounding container this window needs.
    ///
    /// The `ScrollArea` goes HERE — around the BODY, below the window's custom title bar — and
    /// never via `Window::scroll(true)`: egui's `Window::show_dyn` calls `scroll.show(ui,
    /// add_contents)` around the ENTIRE closure, which would scroll vike's own title bar (its
    /// move-drag and its close button) out of reach on an overflowing window.
    ///
    /// `auto_shrink(!scrolls)` is what makes the latched half work, and it is not decoration:
    /// `scroll_area.rs` resolves the `(scroll_enabled, auto_shrink)` pair `(true, false)` to
    /// `inner_size[d]` — "let scroll area be larger than content; fill with blank space" — so the
    /// body reports the WINDOW's size rather than its content's, and `Resize::begin`'s
    /// `desired_size.max(last_content_size)` content floor never engages.
    ///
    /// ⚠ A `fills` kind returns BEFORE any of that: `body(ui)` on the caller's own `Ui`, no
    /// container, no extra id level — byte-identical to the unwrapped spelling the DOM (now Trade) and
    /// Polymarket bodies were dispatched under before this wrapper existed.
    pub fn show<R>(&self, ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
        if self.fills {
            return body(ui);
        }
        egui::ScrollArea::new([self.scrolls, self.scrolls])
            .auto_shrink(!self.scrolls)
            .show(ui, body)
            .inner
    }

    /// Whether this window's body is being bounded rather than grown for. Tests assert on it; no
    /// production caller needs to branch on it (that is [`Self::show`]'s job).
    pub fn scrolls(&self) -> bool {
        self.scrolls
    }
}

/// Show one window with a CUSTOM single-row title bar (no egui title bar): consume any forced
/// geometry, draw, read the live rect back. `draw` renders the title bar (first row) + content and
/// RETURNS the title bar's drag delta in screen points, which we apply to the window position;
/// geometry is app-controlled via `current_pos` so the custom bar drives movement. Its second
/// argument is the [`BodyBounds`] this window's BODY must be laid out through.
/// Returns `true` if the user dragged this window's title bar this frame (a manual move),
/// so the caller can drop any remembered tiling mode (don't re-tile a hand-placed layout).
///
/// ⚠ `bounds` is a CEILING as well as a clamp, and that is this function's job rather than
/// egui's: egui bounds what it PAINTS (an `Area`'s content `Ui` is clipped to its `constrain_rect`)
/// but it does not bound how large a `Window` is LAID OUT, so a body bigger than the arena paints
/// a window whose far edges — and therefore [`window_resize_handles`]' bands — land outside that
/// clip, where `egui-0.37.0/src/hit_test.rs` drops them and no drag can reach them. See
/// [`WinState::arena_bounded`].
pub fn show_window(
    ctx: &egui::Context,
    w: &mut WinState,
    bounds: Rect,
    draw: impl FnOnce(&mut egui::Ui, &BodyBounds) -> egui::Vec2,
) -> bool {
    let forced = w.pending.take();
    if let Some(rect) = forced {
        w.pos = rect.min;
        w.size = rect.size();
        w.force_frames = 4; // pin the arranged geometry briefly so egui commits it, then release
    }
    // ⚠ THE ARENA CEILING (size half). No window may be laid out LARGER than the arena it lives
    // in — `w.size` is authoritative for every pinned window (maximized, `fills`, `user_sized`,
    // `arena_bounded`), and a band drag can push it past `bounds` on either axis in one gesture.
    // egui clamps what it PAINTS regardless (`Window::show_dyn` does
    // `resize.max_size = resize.max_size.min(constrain_rect.size())` before the frame margin comes
    // off), so leaving the oversized value in `w.size` only makes the two DISAGREE: the bands
    // anchor on the painted rect while the next drag's `w.size.y + d.y` starts from a number
    // nobody can see, and the whole divergence is released in one jump — the same failure the
    // both-axes floor inside the resize block below exists to prevent, one bound over.
    //
    // Floored at `MINW`×`MINH` so a degenerate arena (an app window dragged tiny, a frame taken
    // mid-restore) cannot squash every window to nothing IRREVERSIBLY, and skipped outright for a
    // non-positive `bounds` — `Rect::NOTHING`'s width is `-inf`, and `w.size` must not be built
    // out of that.
    if bounds.is_positive() {
        w.size = w.size.min(egui::vec2(
            bounds.width().max(workspace::MINW),
            bounds.height().max(workspace::MINH),
        ));
    }
    // Keep the window inside the workspace: its top can't go above the main caption's bottom,
    // nor past the status bar / rail (ctx.content_rect doesn't exclude our sub-ui caption panel).
    w.pos.x =
        w.pos.x.clamp(bounds.min.x, (bounds.max.x - workspace::MIN_VISIBLE_W).max(bounds.min.x));
    w.pos.y =
        w.pos.y.clamp(bounds.min.y, (bounds.max.y - workspace::MIN_VISIBLE_H).max(bounds.min.y));
    // ⚠ THE ARENA CEILING (position half), and it is deliberately NARROWER than the clamp above:
    // only a window the CEILING owns is placed so it FITS. The clamp above lets a window overhang
    // by design (80×40 of it must stay reachable), which is right for a window egui is still free
    // to pull back in — but an `arena_bounded` window is `current_pos` + `fixed_size` from here
    // on, so `Context::constrain_window_rect_to_area` would move the PAINTED window back inside
    // while `w.pos` went on describing a window nobody sees, and the bands (anchored on the
    // painted rect) would answer for a different window than the drag arithmetic.
    if w.arena_bounded && bounds.is_positive() {
        w.pos = w.pos.min(bounds.max - w.size).max(bounds.min);
    }

    // While pinning (just after arrange/move) force geometry so it commits; otherwise let egui
    // OWN pos+size and read it back — that's what makes resize work from EVERY edge (forcing
    // current_pos every frame would snap top/left-edge resizes back).
    // A MAXIMIZED window must stay pinned to the full bounds EVERY frame — otherwise egui's
    // auto-sizing shrinks it back to the content's natural size (the "maximize doesn't fill /
    // tiles don't resize" bug). force_frames pins briefly after arrange/move (then releases for
    // all-edge resize); maximized pins permanently until restored.
    if w.maximized {
        w.pos = bounds.min;
        w.size = bounds.size();
    }
    // FILL-window kinds (Chart plot, Trade window) are ALWAYS pinned to `w.size` (like a maximized
    // window), never left to egui's native resize. An egui `Window` auto-sizes to its content; a
    // fill-window's body is made to fill exactly `ui.max_rect()` (the chart's `leftover` fill; the
    // Trade window allocates its whole rect once), so a pinned body's content
    // height == the window height and egui keeps the fixed size. Leaving such a body to egui's
    // native auto-size is a run-away SHRINK loop: because the body reports whatever height egui
    // hands it, each frame's read-back is a hair shorter than the window, so `default_size` creeps
    // down toward the content floor every frame (the DOM's "resizes down at open" bug). We drive
    // resize ourselves via the edge handles below (the same pattern as the custom title-bar move).
    // Intrinsic-height tool windows (Options, News, …) don't fill, so they keep egui-native resize.
    // The Polymarket cockpit fills like the Trade window: its probability ladder sizes its region to
    // `available_rect_before_wrap`, so the same pinned-body treatment keeps the window from
    // auto-shrinking to the ladder's content floor.
    let fills = matches!(w.kind, WinKind::Chart | WinKind::Trade | WinKind::Polymarket);
    // A tool window whose user has grabbed an edge band (`user_sized`) is app-owned from then on,
    // exactly like a `fills` kind: pinned geometry, and a body that must BOUND itself because the
    // window will no longer grow for it. `scrolls` is that instruction, handed to `draw` so the
    // caller can wrap its body in a `ScrollArea` — a `fills` body already sizes itself to the
    // available rect, so it never scrolls.
    //
    // ⚠ `fills` is carried too, and it does more than zero `scrolls`: `BodyBounds::show` puts NO
    // container around a `fills` body, because Trade and Polymarket are dispatched from the TOOL
    // call site (which calls `show`) while only Chart uses the wrapper-less chart call site. See
    // [`BodyBounds`].
    //
    // ⚠ `arena_bounded` confers the SAME ownership without anybody grabbing anything: a body that
    // wants more room than the arena has gets the window capped at the arena and told to bound
    // itself, because the alternative is a window painted past `bounds` whose bottom band lands
    // outside `Area`'s clip rect where nothing can hit it. See that field's doc.
    let bounded = !fills && w.arena_bounded;
    let body_bounds = BodyBounds { fills, scrolls: !fills && (w.user_sized || bounded) };
    // ⚠ Recomputed after the resize block below (`pinning_now`) — see the note there. This copy
    // is only what the `Window` builder needs BEFORE the frame is drawn.
    let pinning = w.force_frames > 0 || w.maximized || fills || w.user_sized || bounded;
    let mut win = egui::Window::new("")
        .id(w.id)
        .title_bar(false) // custom title bar lives inside the content
        .movable(false) // we move the window via the custom bar's drag
        // ⚠ NOT a sizing change, and this flag is NOT what makes a window auto-grow. egui 0.37's
        // `Window::show_dyn` does `let resize = resize.resizable(false); // We resize it manually`
        // UNCONDITIONALLY, on the line right after `PossibleInteractions::new(&area, &resize,
        // is_collapsed)` reads it — so the builder flag feeds ONLY whether egui registers its own
        // edge widgets. `Resize::end`'s `if self.with_stroke || self.resizable[d]` loop is
        // unaffected, and an unlatched tool window still auto-grows to its content exactly as it
        // did. Turning it off removes the mechanism that would fight our own bands below — and
        // egui could never have served the LEFT edge here anyway: `PossibleInteractions::new`
        // reads `resize_left: resizable.x && (movable || pivot.x() != Align::LEFT)`, and these
        // windows are `.movable(false)` with the default LEFT_TOP pivot.
        .resizable(false)
        .constrain_to(bounds);
    win = if pinning {
        win.current_pos(w.pos).fixed_size(w.size)
    } else {
        win.default_pos(w.pos).default_size(w.size)
    };

    // Custom edge/corner resize for EVERY kind (egui's own native resize is off — see the
    // `.resizable(false)` note above, and note that egui structurally refuses the left edge for a
    // non-movable LEFT_TOP-pivot window, so the bands are the only thing that can serve it).
    // Disabled while maximized or during a forced-geometry pin (arrange/restore settle).
    let resizable_now = !w.maximized && w.force_frames == 0;
    let wid = w.id;
    let mut resize: Option<(u8, egui::Vec2)> = None;
    let mut drag = egui::Vec2::ZERO;
    let resp = win.show(ctx, |ui| {
        drag = draw(ui, &body_bounds);
        if resizable_now {
            resize = window_resize_handles(ui, wid);
        }
    });
    if let Some((mask, d)) = resize {
        if mask & 2 != 0 {
            w.size.x = (w.size.x + d.x).max(workspace::MINW);
        }
        if mask & 8 != 0 {
            w.size.y = (w.size.y + d.y).max(workspace::MINH);
        }
        if mask & 1 != 0 {
            let nx = (w.size.x - d.x).max(workspace::MINW);
            w.pos.x += w.size.x - nx;
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
            // LATCH (see `WinState::user_sized`): from the first band MOVEMENT this tool window is
            // app-owned — pinned geometry, self-bounding body. Guarded on `!fills` because a
            // `fills` kind is app-owned from frame one and must NEVER take the latch: its body
            // sizes itself to the available rect, so handing it `scrolls == true` would wrap a
            // self-filling body in a `ScrollArea` and break it.
            //
            // ⚠ `moved_the_edge` also subsumes the NON-ZERO guard this started as (a zero delta
            // moves no component), which matters because `window_resize_handles` returns `Some` on
            // every `resp.dragged()` frame, and egui marks a `Sense::drag()` widget dragged on the
            // PRESS frame, where `drag_delta()` is zero. Latching on `Some` alone turned one
            // accidental CLICK on a window's 8pt edge into a permanent, irreversible conversion to
            // `fixed_size` + scrolling for the rest of the session.
            //
            // ⚠ BOTH axes are floored HERE, not just the one the pressed band consumed. Each arm
            // above floors only its own axis (`MINW` under `mask & (1 | 2)`, `MINH` under
            // `mask & 8`), so a SIDE-band drag latched with `w.size.y` left at whatever the
            // pre-latch read-back wrote — the PAINTED content height, which for a short body (an
            // empty list, a collapsed panel) is well under `MINH`. From the next frame
            // `pinning_now` builds the window `.fixed_size(w.size)`, so that stale value becomes
            // the window's declared geometry while the window is PAINTED somewhere else entirely:
            // `Resize::end` takes `size[d] = state.last_content_size[d]` for a `Window`, and a
            // latched body is a `ScrollArea` whose `inner_size` is floored at
            // `min_scrolled_size` — 64pt, BOTH axes, unconditional for an enabled direction
            // (`egui-0.37.0/src/containers/scroll_area.rs`: `if direction_enabled[d] {
            // inner_size[d] = inner_size[d].max(min_scrolled_size[d]); }`). `w.size` then stops
            // describing the window a user sees, which is exactly what "for pinned windows
            // `w.size`/`w.pos` are authoritative" (the read-back's own comment below) promises it
            // does. The visible consequence is the NEXT drag on that axis not tracking the
            // pointer: that drag's PRESS frame carries a zero delta but still runs the arm above,
            // whose `.max(MIN*)` snaps the stale value up before the move frame adds anything. So
            // the whole accumulated divergence is released in one step — MEASURED by deleting this
            // line, a +90pt drag on the bottom band moved the painted edge from 100 to 250, a
            // 150pt jump. Gated by
            // `a_side_band_latch_floors_the_axis_its_band_never_touched`.
            //
            // ⚠ What this is NOT is a stranding fix, and a review round asked for it as one: the
            // claim was that `fixed_size` makes `ui.max_rect()` equal `w.size` exactly (true —
            // `Resize::fixed_size` sets `min_size = max_size = size` and `Resize::begin` clamps
            // `desired_size` into that pair), so a short stale value would put BOTH of
            // `window_resize_handles`' anchor rects under its size guard and leave the window
            // unresizable by any means. It cannot: the same 64pt `min_scrolled_size` floor holds
            // the PAINTED rect — and therefore `ui.min_rect()` — clear of that guard whatever
            // `w.size` says (MEASURED at 100pt tall on the fixture above, against a `fits` floor
            // of `workspace::TITLE_CLEAR + 2*BAND` = 52). Deleting this line leaves every band allocated and
            // reddens only the DRAG assertion. Do not restate the stranding story.
            w.size = w.size.max(egui::vec2(workspace::MINW, workspace::MINH));
            w.user_sized = true;
        }
        // ⚠ There is deliberately NO `force_frames` re-pin here. The line that used to sit on this
        // spot — `w.force_frames = w.force_frames.max(1); // commit the new size on the next
        // frame` — was DEAD as written: `resize` is `Some` only when `resizable_now`, which
        // requires `force_frames == 0`, so `.max(1)` raised it to 1 and the unconditional
        // decrement below put it straight back to 0 in the same call, leaving nothing for the next
        // frame to read. Its comment was therefore a promise the code did not keep, which is how
        // it survived: it reads like the thing that commits a resize, and the `user_sized` latch
        // reads like a duplicate of it.
        //
        // ⚠ It does NOT stay dead now that `pinning_now` is re-derived BELOW it, and that is why
        // it is deleted rather than moved: `force_frames > 0` is the first term of `pinning_now`,
        // so re-pinning here would suppress this frame's read-back on EVERY frame a band reported
        // a drag — including a bare press, which must change nothing (see the non-zero-delta guard
        // above) and must leave an unlatched window egui-owned. MEASURED by restoring the line:
        // `the_bottom_band_stays_inside_a_window_shorter_than_its_box` reddens with "the bottom
        // edge must drag: 135 -> 220", the drag applying more than it was given.
        //
        // What commits a new size is `pinning_now` below — through `fills`/`maximized` for those
        // kinds, and through the `user_sized` latch for a tool window from its first movement. The
        // move path's own `.max(1)` sits AFTER the decrement and is a different case: a move is
        // driven by the title bar's own response and genuinely needs the NEXT frame pinned.
    }

    // ⚠ RE-DERIVED, not the `pinning` computed before the frame. The resize block above can set
    // `user_sized` on THIS frame, and with movement-latching the first latching frame ALWAYS
    // carries a non-zero delta — so reading the stale flag here let the end-of-frame read-back
    // overwrite the delta just applied with egui's pre-drag rect. A press and a move coalescing
    // into one frame's `RawInput` is routine with a high-polling mouse.
    let pinning_now = w.force_frames > 0 || w.maximized || fills || w.user_sized || bounded;

    if let Some(r) = resp {
        let rect = r.response.rect;
        // Only sync geometry back from egui when it OWNS the geometry (non-pinned tool windows,
        // native resize). For pinned windows (maximized / force-pinned / chart) `w.size`/`w.pos`
        // are authoritative — set by maximize, arrange, or our own resize handles — and reading
        // back the fixed size here would clobber a just-applied resize.
        if !pinning_now {
            w.size = rect.size();
            w.pos = rect.min; // egui owns geometry on free frames — sync back (all-edge resize)
        }
        // ⚠ THE ARENA CEILING (the latch). `rect` is egui's OWN answer for where it just painted
        // this window, which is the only rect that can report the overflow: a `Window` is painted
        // at `Resize::end`'s `size[d] = state.last_content_size[d]` whatever `fixed_size` says, so
        // neither `w.size` nor the builder's inputs know the body overran. One frame of overflow
        // is unavoidable — the body has to lay out before anything can measure it — and one frame
        // is all it costs, because `force_frames` pins a freshly-opened window anyway, so a tool
        // window that cannot fit is bounded before an operator has seen it settle.
        //
        // NOT applied to a `fills` kind: its body sizes itself to the rect it is offered, so it
        // never overruns, and `scrolls` must stay false for it (`BodyBounds::show` hands a `fills`
        // body straight through — wrapping a self-filling body in a `ScrollArea` breaks it).
        // NOT applied while maximized: that path already writes `w.size = bounds.size()` every
        // frame, so any excess is rounding, which `CEILING_SLACK` covers.
        if !fills
            && !w.maximized
            && !w.arena_bounded
            && bounds.is_positive()
            && (rect.width() > bounds.width() + CEILING_SLACK
                || rect.height() > bounds.height() + CEILING_SLACK)
        {
            w.arena_bounded = true;
            // Take the size egui painted, capped at the arena, rather than whatever `w.size`
            // happens to hold: during the open pin `w.size` is still the SPAWN size (the read-back
            // above is skipped), and a body that grew past the arena on one axis has usually
            // settled on a perfectly good width on the other.
            w.size = rect.size().min(egui::vec2(
                bounds.width().max(workspace::MINW),
                bounds.height().max(workspace::MINH),
            ));
            w.pos = w.pos.min(bounds.max - w.size).max(bounds.min);
        }
    }
    if w.force_frames > 0 {
        w.force_frames -= 1;
    }
    let moved = drag != egui::Vec2::ZERO;
    if moved {
        w.pos += drag;
        w.force_frames = w.force_frames.max(1); // re-pin so the dragged pos is applied next frame
    }
    if forced.is_some() {
        ctx.move_to_top(LayerId::new(Order::Middle, w.id));
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
/// bit 1 = left, 2 = right, 8 = bottom. `show_window` applies the delta to `w.pos`/`w.size`.
///
/// ⚠ **The bands are anchored on `ui.min_rect()`, NOT `ui.max_rect()`, and that is load-bearing.**
/// This function runs after the body has been laid out, so `min_rect` is the content drawn this
/// frame — and for an egui `Window` that IS the painted rect. `Window::show_dyn` builds its
/// `Resize` with `.with_stroke(false)` and then forces `let resize = resize.resizable(false); //
/// We resize it manually` unconditionally, so `Resize::end`'s `if self.with_stroke ||
/// self.resizable[d]` loop always takes the `else { size[d] = state.last_content_size[d]; }` arm,
/// and `last_content_size` is `content_ui.min_size()` — this `min_rect`'s size. `max_rect` is
/// `Resize::begin`'s `state.desired_size`, a MONOTONE HIGH-WATER MARK
/// (`desired_size = desired_size.max(last_content_size)` on every non-dragging frame), so for an
/// UNLATCHED tool window whose body is shorter than its box the two diverge: the bottom band and
/// both bottom corners were allocated BELOW the visible window, where the bottom edge could not be
/// dragged at all and an invisible 8pt `Sense::drag` strip floated in dead space showing a resize
/// cursor and swallowing clicks on whatever was underneath. A REGRESSION, since that edge worked
/// through egui's native resize before `.resizable(false)`.
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
/// window allocates its whole rect once and does not). `vike_panels::cockpit::ladder::draw`
/// reads `ui.available_rect_before_wrap()` ONCE at the top and then lays out sibling
/// allocations SUMMING to that height — `ladder_header` the density's header height, 24
/// at Normal, and the region — so the
/// `item_spacing.y` egui inserts BETWEEN siblings was never subtracted and is pure overrun, by
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
/// box, which is the tool-window case above and which no `fills` kind can reach.
///
/// ⚠ **With ONE fallback, and it is a stranding fix rather than a softening of that rule.** A
/// window whose painted content is too short to carry a band set gets no bands from `min_rect` —
/// and because these bands are the only resize mechanism any window has, "no bands" means
/// "unresizable by any means, permanently", where before `.resizable(false)` egui's own right and
/// bottom edges still worked. So when `min_rect` cannot carry them and `max_rect` can, the bands
/// go on `max_rect`; the arm argues itself at the site.
///
/// ⚠ **A band that is ALLOCATED is not a band that can be HIT, and nothing in this function can
/// tell the difference.** `ui.interact` records `interact_rect: self.clip_rect().intersect(rect)`,
/// an `Area`'s content `Ui` is clipped to its `constrain_rect` (`area.rs`:
/// `ui.set_clip_rect(self.constrain_rect)` — "Don't paint outside our bounds"), and
/// `egui-0.37.0/src/hit_test.rs` drops any widget whose `interact_rect.is_negative()` before it
/// measures a single distance. So a band placed outside the workspace bounds is registered,
/// answers `Context::read_response`, sets a resize cursor on nothing, and can never be dragged.
/// That is not hypothetical geometry: an unlatched tool window is PAINTED at its content
/// (`Resize::end`'s `size[d] = state.last_content_size[d]`), so a body taller than the arena put
/// the bottom band and both bottom corners below it — the reported "why can't I resize it at the
/// bottom, the status bar is over the window". The cure is upstream of this function, in
/// [`WinState::arena_bounded`]: a window that cannot fit is CAPPED at the arena, so every band it
/// allocates is inside the clip. Gated by
/// `a_body_taller_than_the_arena_keeps_the_window_and_its_bands_inside_it`.
///
/// **The TOP edge is deliberately skipped, and there is no bit 4.** It overlaps the title bar's
/// move-drag and its control buttons — and the cost is not just the drag: a drag-only band
/// suppresses CLICKS as well, because `egui-0.37.0/src/hit_test.rs` returns only the drag-widget
/// when one is on top ("The drag-widget is separate from the click-widget, so return only the
/// drag-widget" → `click: None`). A top band would therefore kill double-click-to-maximize and
/// the top slice of every control button on all twelve window kinds. The side bands start below
/// the title bar (`workspace::TITLE_CLEAR`) for the same reason. This is an owner's ruling, not an
/// oversight: do not add a top band, and do not widen the side bands over the title bar.
pub(super) fn window_resize_handles(ui: &egui::Ui, wid: Id) -> Option<(u8, Vec2)> {
    use egui::{CursorIcon, Rect, Sense, pos2};
    const BAND: f32 = space::LG; // the side inset of `TOOL_BODY_MARGIN`, which the bands sit in
    let title_clear = workspace::TITLE_CLEAR; // keep side bands clear of the title-bar controls
    // Room for a non-overlapping band set: a bottom band between two corners, and side bands that
    // start below `workspace::TITLE_CLEAR` and are still at least `BAND` tall above the bottom one.
    let fits = |r: Rect| r.width() >= 3.0 * BAND && r.height() >= title_clear + 2.0 * BAND;
    let painted = ui.min_rect(); // the PAINTED content rect — see this function's doc
    let r = if fits(painted) {
        painted
    } else if fits(ui.max_rect()) {
        // ⚠ THE STRANDING FALLBACK, and it is not a softening of the anchor rule above — it is
        // the case where that rule has nothing to offer. These bands are the ONLY resize
        // mechanism any window has (`show_window` builds every `Window` `.resizable(false)`, and
        // egui structurally refuses the left edge for a non-movable LEFT_TOP-pivot window
        // anyway), so returning `None` here does not fall back to egui — it makes the window
        // UNRESIZABLE BY ANY MEANS, permanently. A tool window whose body paints shorter than
        // `workspace::TITLE_CLEAR + 2*BAND` is exactly that case: `egui-0.37.0/src/containers/resize.rs`'s
        // `Resize::end` takes `else { size[d] = state.last_content_size[d]; }` for a `Window`
        // (`with_stroke(false)` + the forced `resizable(false)`), so the window is PAINTED at its
        // content and `min_size` never floors it.
        //
        // `max_rect` is `Resize::begin`'s `state.desired_size`, a monotone high-water mark that
        // still remembers the spawn size, so it is the one rect that can carry a band set here.
        // The bands then sit partly below the painted window — the very shape the anchor rule
        // above exists to avoid — and that is ACCEPTED here because it is self-healing: one grab
        // latches the window (`user_sized`), `show_window` pins it to `w.size`, the body is
        // bounded to that, and `min_rect` clears `fits` from the next frame on, so the fallback
        // arm is never taken again for that window. Gated by
        // `a_window_too_short_for_its_bands_is_still_resizable`.
        //
        // ⚠ Self-healing WITHOUT help from the latch's `MINW`×`MINH` floor, and a review round
        // claimed otherwise — that a SIDE-band grab pins the window at the short painted height,
        // that a pinned window's `max_rect` IS `w.size`, and that this arm therefore has nothing
        // left to offer. The middle step is true (`Resize::fixed_size` sets
        // `min_size = max_size = size`) and the conclusion does not follow: a LATCHED body is a
        // `ScrollArea` with both directions enabled, and
        // `egui-0.37.0/src/containers/scroll_area.rs` floors its `inner_size` at
        // `min_scrolled_size` — 64pt, both axes, unconditional for an enabled direction — so the
        // PAINTED rect clears `fits` from the latching frame on however small `w.size` is, and it
        // is `painted` rather than this arm that serves the window. MEASURED by deleting that
        // floor: every band is still allocated — see
        // `a_side_band_latch_floors_the_axis_its_band_never_touched`.
        ui.max_rect()
    } else {
        return None; // too small to place non-overlapping bands on either rect
    };
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
