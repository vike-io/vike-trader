//! Band GEOMETRY: where the bands anchor, the arena ceiling, and the scrollbar clearances.

use super::*;

#[test]
fn the_bottom_band_stays_inside_a_window_shorter_than_its_box() {
    // THE ANCHOR. `window_resize_handles` read `ui.max_rect()`, which for an UNLATCHED tool
    // window is `Resize::begin`'s `desired_size` — a MONOTONE HIGH-WATER MARK — while the
    // window is PAINTED at `last_content_size` (`Resize::end` takes `else { size[d] =
    // state.last_content_size[d]; }`, since `Window::show_dyn` forces `resizable(false)` and
    // builds its `Resize` `.with_stroke(false)`). A body SHORTER than the spawn box makes the
    // two diverge, and the bottom band plus both bottom corners were then allocated BELOW the
    // visible window: the bottom edge could not be grabbed at all, and an invisible 8pt
    // `Sense::drag` strip floated in dead space showing a resize cursor and swallowing clicks
    // on whatever sat underneath. A REGRESSION — that edge worked through egui's native
    // resize until `.resizable(false)`.
    //
    // MUTATION that must redden this: in `window_resize_handles`, replace the whole
    // `let r = if fits(painted) { … }` selection with `let r = ui.max_rect();`.
    let mut h = Harness::tool(vec2(520.0, 100.0)); // a body FAR shorter than the box
    let mut w = settled_tool(&mut h, WinKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

    // NON-VACUITY: the two rects really do diverge here, so "inside the painted rect" is a
    // claim about the fix rather than about a degenerate fixture.
    let (min_r, max_r) = (h.content_min.get(), h.content_max.get());
    assert!(
        max_r.height() > min_r.height() + 100.0,
        "the fixture must actually diverge: max_rect {} vs min_rect {}",
        max_r.height(),
        min_r.height()
    );

    let painted = h.painted(&w);
    for mask in [8u8, 1 | 8, 2 | 8] {
        let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert!(
            painted.contains_rect(band),
            "band {mask} at {band:?} must lie INSIDE the painted window {painted:?}"
        );
    }

    // ...and the edge is not merely inside, it WORKS: grabbing it resizes the window.
    let h0 = w.size.y;
    drag_band(&mut h, &mut w, 8, vec2(0.0, 60.0));
    h.frames(&mut w, 4);
    assert!(
        (w.size.y - (h0 + 60.0)).abs() < 2.0,
        "the bottom edge must drag: {h0} -> {}",
        w.size.y
    );
}

#[test]
fn a_body_taller_than_the_arena_keeps_the_window_and_its_bands_inside_it() {
    // THE REPORTED BUG, in the owner's words: "WHY I CANT RESIZE IT AT BOTTOM?? IT SEEMS LIKE
    // WINDOW IS OVERLAYED BY BOTTOM STATUS BAR?" — on the Connections tool window.
    //
    // The arena is NOT the problem and the theory it suggests is wrong: `app_ui.rs` adds
    // `egui::Panel::bottom("statusbar")` BEFORE the `CentralPanel`, and a bottom panel sets
    // `cursor.max[axis] = visible_outer_rect.min[axis]` on its parent
    // (`egui-0.36.1/src/containers/panel.rs`), so `app.desktop` genuinely excludes the strip —
    // see [`arena`], which is shaped the same way.
    //
    // What is true is the WINDOW. Until it is app-owned a tool window is painted at its
    // CONTENT, whatever its declared size says: `Window::show_dyn` forces
    // `resize.resizable(false)` and `.with_stroke(false)`, so `Resize::end` always takes
    // `else { size[d] = state.last_content_size[d]; }`. A body taller than the arena therefore
    // paints a window whose bottom edge is past `bounds` — under the status bar, exactly as
    // reported — and `window_resize_handles` then allocates the bottom band and both bottom
    // corners out there, where `Ui::interact`'s `interact_rect: self.clip_rect().intersect(
    // rect)` is NEGATIVE (the area's clip rect is its `constrain_rect`) and
    // `egui-0.36.1/src/hit_test.rs` drops the widget before measuring anything. The band
    // exists, `read_response` answers for it, and it can never be hit.
    //
    // MUTATION that must redden this: delete the `w.arena_bounded = true;` arm from
    // `show_window`'s read-back block (the ARENA CEILING latch). PERFORMED — it reddens here
    // and on the width twin, and leaves `a_window_that_fits_its_arena_never_takes_the_ceiling`
    // green. ⚠ MEASURED under it, by stripping this test's earlier assertions one layer at a
    // time so each later claim was actually reached — because a test that only ever reddens on
    // its first assertion has proved nothing about the rest:
    //
    //   * painted rect  `[..1476]` against an arena ending at `978` — the window is drawn
    //     498pt past the status bar's top edge, which is the report verbatim;
    //   * band 8        `[[129 1467] - [671 1475]]` against a clip rect of
    //     `[[0 40] - [1200 978]]` — allocated, answering `read_response`, 489pt outside;
    //   * the DRAG      a full press-move-release on it moves the window `1436 -> 1436`.
    //
    // That last line is the whole bug in one number, and it is why this test drags rather than
    // stopping at geometry.
    //
    // ⚠ The assertion ORDER is deliberate, because every band is still ALLOCATED under that
    // mutation and `h.band(..)` still answers for it. The geometric claims come FIRST so the
    // mutation's first red is the symptom a user sees — a window painted past the arena — and
    // only then the bookkeeping. Asserting `w.arena_bounded` first would have reddened on a
    // flag while proving nothing about the window.
    const BODY_H: f32 = 1400.0;
    let mut h = Harness::tool(vec2(520.0, BODY_H)).in_arena();
    let arena = arena();
    // `WinKind::Data` here is an arbitrary tool-window fixture (`WinKind::Connections`, the kind
    // the bug was originally reported on, is deleted 2026-10-05) — this test's vertical-geometry
    // assertions are independent of `carries_title_tabs()`, which only affects the bar's
    // HORIZONTAL layout.
    let mut w = settled_tool(&mut h, WinKind::Data, pos2(120.0, 60.0), vec2(560.0, 400.0));

    // NON-VACUITY, from the fixture's own arithmetic rather than from anything the fix wrote:
    // this body genuinely cannot fit, so the containment below is a claim about the ceiling.
    assert!(
        BODY_H > arena.height(),
        "the fixture must overflow: body {BODY_H} vs arena {}",
        arena.height()
    );

    assert!(!w.user_sized, "nothing was grabbed, so the USER latch must stay clear");

    // THE SYMPTOM: the window is no longer painted past the arena, i.e. under the status bar.
    let painted = h.painted(&w);
    assert!(
        painted.max.y <= arena.max.y + 1.0,
        "the painted window must not reach past the arena bottom ({}) into the status bar: {}",
        arena.max.y,
        painted.max.y
    );
    assert!(
        arena.contains_rect(painted.shrink(1.0)),
        "the whole window must sit inside the arena {arena:?}: {painted:?}"
    );

    // THE MECHANISM: every band is inside the clip rect, which for this `Area` IS the arena.
    for mask in [8u8, 1 | 8, 2 | 8, 1, 2] {
        let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert!(
            arena.intersect(band).is_positive(),
            "band {mask} at {band:?} must intersect the clip rect {arena:?} — a band outside \
                 it is registered, answers `read_response`, and can never be hit"
        );
        assert!(
            painted.contains_rect(band),
            "band {mask} at {band:?} must lie inside the painted window {painted:?}"
        );
    }

    // ...and the bottom edge WORKS. It can only be dragged UP from here — the window is at the
    // ceiling — which is the half the owner could not reach at all.
    let h0 = w.size.y;
    assert!(
        (h0 - arena.height()).abs() < 1.0,
        "the ceiling must cap the window at the arena height {}: {h0}",
        arena.height()
    );
    drag_band(&mut h, &mut w, 8, vec2(0.0, -120.0));
    h.frames(&mut w, 4);
    assert!(
        (w.size.y - (h0 - 120.0)).abs() < 2.0,
        "the bottom edge must drag the window SMALLER: {h0} -> {}",
        w.size.y
    );
    let painted = h.painted(&w);
    assert!(
        (painted.height() - (h0 - 120.0)).abs() < 2.0,
        "...and the PAINTED window must follow it: {}",
        painted.height()
    );

    // ...and back DOWN again, so the ceiling is a cap rather than a one-way freeze.
    drag_band(&mut h, &mut w, 8, vec2(0.0, 60.0));
    h.frames(&mut w, 4);
    assert!(
        (w.size.y - (h0 - 60.0)).abs() < 2.0,
        "the bottom edge must drag back down too: {}",
        w.size.y
    );

    // ...and only now the bookkeeping, which is the MEANS rather than the end.
    assert!(w.arena_bounded, "a body the arena cannot hold must latch the ceiling");
    assert!(h.scrolled.get(), "...and the body must be told to bound itself");
    assert!(
        h.body_max.get().height() < BODY_H,
        "...which is what BOUNDS it: offered {} of {BODY_H}",
        h.body_max.get().height()
    );
}

#[test]
fn a_body_wider_than_the_arena_keeps_both_side_bands_inside_it() {
    // The SAME mechanism on the other axis, and WHICH band it costs depends on where the
    // window sits. `Context::constrain_window_rect_to_area` clamps an oversized window's
    // position into `[area.left - margin, area.left]`, where
    // `margin = window.width() - area.width()` — so the overflow hangs off whichever side the
    // pivot did not pin. At this fixture's position the left edge lands exactly ON the arena's
    // and the RIGHT band is the one outside the clip; a window whose pivot had been dragged
    // further left loses the LEFT band instead. Either way at least one side band is gone,
    // which is how a wide body can reproduce the ORIGINAL report's symptom ("resize does not
    // work from the left edge") on a build where #1773 already fixed its cause — a different
    // defect wearing the same complaint.
    //
    // MUTATION that must redden this: as above, delete the `w.arena_bounded = true;` arm —
    // PERFORMED, and it reddens here first on the painted WIDTH, not on a flag (same
    // assertion-order argument as the height twin).
    const BODY_W: f32 = 1400.0;
    let mut h = Harness::tool(vec2(BODY_W, 300.0)).in_arena();
    let arena = arena();
    let mut w = settled_tool(&mut h, WinKind::Studio, pos2(60.0, 80.0), vec2(560.0, 400.0));

    assert!(
        BODY_W > arena.width(),
        "the fixture must overflow: body {BODY_W} vs arena {}",
        arena.width()
    );

    let painted = h.painted(&w);
    assert!(
        painted.width() <= arena.width() + 1.0,
        "the window must be capped at the arena width {}: {}",
        arena.width(),
        painted.width()
    );
    for mask in [1u8, 2, 1 | 8, 2 | 8] {
        let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert!(
            arena.intersect(band).is_positive(),
            "band {mask} at {band:?} must intersect the clip rect {arena:?}"
        );
    }

    // ...and the LEFT edge drags, pinning the right — the original report's exact claim, now
    // asserted on a window that could not previously offer that band at all.
    let right0 = w.pos.x + w.size.x;
    let x0 = w.pos.x;
    drag_band(&mut h, &mut w, 1, vec2(90.0, 0.0));
    h.frames(&mut w, 4);
    assert!(
        (w.pos.x - (x0 + 90.0)).abs() < 2.0,
        "the left edge must follow the drag: {x0} -> {}",
        w.pos.x
    );
    assert!(
        (w.pos.x + w.size.x - right0).abs() < 2.0,
        "...with the RIGHT edge pinned: {right0} -> {}",
        w.pos.x + w.size.x
    );

    // ...and only now the bookkeeping.
    assert!(w.arena_bounded, "a body the arena cannot hold must latch the ceiling");
    assert!(
        h.body_max.get().width() < BODY_W,
        "...which is what BOUNDS it: offered {} of {BODY_W}",
        h.body_max.get().width()
    );
}

#[test]
fn a_window_that_fits_its_arena_never_takes_the_ceiling() {
    // The ceiling's NO-OP half, and the reason it is a latch on OVERFLOW rather than a clamp
    // on every window. A tool window that fits must stay exactly what it was before this
    // change: unpinned, sized by egui, growing to its content — nine tool kinds open at
    // 560×400 and several bodies are naturally bigger, so bounding one that fits would open it
    // into scrollbars for nothing.
    //
    // ⚠ It runs in the NARROW [`arena`], not in [`screen`]: run against bounds nothing can
    // reach, this test would pass with the ceiling wired to fire unconditionally.
    //
    // MUTATION that must redden this: drop the `rect.width() > … || rect.height() > …`
    // condition from the ARENA CEILING latch (latch every window), or drop `CEILING_SLACK`
    // from both comparisons and hand the fixture a body that lands exactly on the arena.
    let mut h = Harness::tool(vec2(620.0, 500.0)).in_arena();
    let arena = arena();
    let w = settled_tool(&mut h, WinKind::News, pos2(100.0, 120.0), vec2(560.0, 400.0));

    assert!(!w.arena_bounded, "a window that fits must NOT take the ceiling");
    assert!(!h.scrolled.get(), "...and its body is never told to bound itself");
    assert!(!w.user_sized, "...nor does the user latch fire on its own");
    let painted = h.painted(&w);
    assert!(
        painted.height() < arena.height(),
        "non-vacuity: the fixture must actually fit ({} vs {})",
        painted.height(),
        arena.height()
    );
    // The pre-existing promise this must not break: egui still owns the geometry, so the
    // window has GROWN to its taller and wider content rather than staying at the 560×400 it
    // opened with.
    assert!(w.size.y > 400.0, "an unbounded window must still grow to its content: {}", w.size.y);
    assert!(w.size.x > 560.0, "...on the other axis too: {}", w.size.x);
}

/// `window_resize_handles`' own `BAND` and `workspace::TITLE_CLEAR` (a row of `ui-theme.toml`),
/// restated here so [`expected_bands`] can rebuild the band set from OUTSIDE the function under
/// test. `BAND` is private to it and the row is the production's own, so this is a deliberate
/// second copy: a production change to either constant moves every band and must redden the
/// byte-identity tests below, which is exactly what a shared constant would hide.
const BAND: f32 = 8.0;
const TITLE_CLEAR: f32 = 36.0;

/// The five band rects `window_resize_handles` places on an anchor rect, keyed by mask, spelled
/// as origin/main spelled them (`bl`, `br`, `left`, `right`, `bottom`).
fn expected_bands(r: Rect) -> [(u8, Rect); 5] {
    [
        (1 | 8, Rect::from_min_max(pos2(r.min.x, r.max.y - BAND), pos2(r.min.x + BAND, r.max.y))),
        (2 | 8, Rect::from_min_max(pos2(r.max.x - BAND, r.max.y - BAND), pos2(r.max.x, r.max.y))),
        (
            1,
            Rect::from_min_max(
                pos2(r.min.x, r.min.y + TITLE_CLEAR),
                pos2(r.min.x + BAND, r.max.y - BAND),
            ),
        ),
        (
            2,
            Rect::from_min_max(
                pos2(r.max.x - BAND, r.min.y + TITLE_CLEAR),
                pos2(r.max.x, r.max.y - BAND),
            ),
        ),
        (
            8,
            Rect::from_min_max(pos2(r.min.x + BAND, r.max.y - BAND), pos2(r.max.x - BAND, r.max.y)),
        ),
    ]
}

#[test]
fn a_chart_windows_band_anchor_is_byte_identical_to_the_old_max_rect_one() {
    // THE CHART must not move by ONE POINT, and it reaches that by FILLING rather than by
    // overrunning: `vike_chart::chart::draw` ends with
    // `add_space(min(clip.bottom, max_rect.bottom) - ui.cursor().top())`, and the cursor is
    // already PAST every gap it laid — so it lands exactly on `max_rect.bottom` however many
    // siblings it drew, and `min_rect == max_rect` with nothing unioned in.
    //
    // ⚠ This test ran over `[Site::Chart, Site::ToolFills]` and claimed the property for
    // Chart, DOM and Polymarket at once (the Trade window took the DOM's place). The claim
    // holds for all three, but that fixture could not show it: its body allocates exactly the
    // rect it is offered, so its `min_rect == max_rect` is a property of the FIXTURE rather
    // than of any shipped body, and the ladders (the DOM's, and the cockpit's still) in fact
    // OVERRUN. They keep the property for a different reason —
    // `Region::expand_to_include_rect` grows `max_rect` with them — which is why they have
    // their own test, `a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical`,
    // rather than riding this one.
    //
    // ⚠ It asserted `min_rect == max_rect` and NOTHING else at first, and that gated nothing:
    // every change to where the bands actually land left it green. What is asserted now is
    // each of the five BAND RECTS, in absolute coordinates, against the arithmetic origin/main
    // performed on `ui.max_rect()`. The equality survives as the PREMISE.
    //
    // MUTATION that must redden this: `workspace::TITLE_CLEAR` = 40.0 in
    // `ui-theme.toml` (or any change to `BAND` or to a band rect's arithmetic) — the
    // chart's bands move, which is precisely what "must not move by one point" forbids.
    // ⚠ The anchor swap itself CANNOT redden THIS test and that is the claim, not a gap: the
    // chart's body reaches `ui.max_rect()`'s bottom, so `content_ui.min_rect()` IS that rect
    // (`egui-0.36.1/src/containers/resize.rs`, `Resize::begin`'s `inner_rect =
    // Rect::from_min_size(position, state.desired_size)`). The anchor is gated where the two
    // rects CAN differ, which is where a body UNDER-fills its box —
    // `the_bottom_band_stays_inside_a_window_shorter_than_its_box`. No `fills` kind can reach
    // that state, so no `fills` test gates the anchor and none pretends to.
    let mut h = Harness::fill();
    let mut w = WinState::new(
        "chartfix",
        "BTCUSDT",
        "1m",
        WinKind::Chart,
        Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
    );
    h.frames(&mut w, 14);

    let (min_r, max_r) = (h.content_min.get(), h.content_max.get());
    assert_eq!(
        min_r, max_r,
        "PREMISE: the chart body fills to `max_rect`'s bottom, so the bands anchor on the \
             same rect either way"
    );
    // THE CLAIM: every band is exactly where the old `max_rect` spelling put it.
    for (mask, want) in expected_bands(max_r) {
        let got = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert_eq!(got, want, "band {mask} must not move by one point");
    }
    // ...and that rect is the window a user sees, which is the property the doc claims.
    let painted = h.painted(&w);
    assert!(
        painted.contains_rect(min_r),
        "the anchor rect {min_r:?} must lie inside the painted window {painted:?}"
    );
}

#[test]
fn a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical() {
    // THE LADDER SHAPE, and the two wrong stories told about it.
    //
    // The Polymarket cockpit reads `ui.available_rect_before_wrap()` ONCE and then lays out
    // siblings SUMMING to that height, so the `item_spacing.y` egui inserts BETWEEN them was
    // never subtracted and is pure overflow: the body ends
    // `(siblings - 1) * shipped_item_spacing().y` past the rect it was OFFERED — 4pt for the
    // cockpit ladder's two allocations ([`LADDER_SIBLINGS`]). That much is real
    // and is asserted below against [`Harness::offered_max`].
    //
    // ⚠ STORY ONE, the doc's original: "a `fills` body covers `max_rect` exactly, so
    // `min_rect == max_rect` and the bands are byte-identical". The CONCLUSION is right and
    // the REASON is not — these bodies overrun `max_rect` rather than covering it — and it was
    // gated by a fixture (`Site::ToolFills`) whose body allocates exactly the rect it is
    // offered, so the claim was unfalsifiable by construction.
    //
    // ⚠ STORY TWO, a review round's correction: "the bands therefore MOVE DOWN by the
    // overflow, and on the `max_rect` spelling the DOM's bottom band sat 12pt INBOARD of the
    // painted edge" (the DOM's overflow, finding F7, went with the DOM). Also false, and in the
    // more dangerous direction — it reads as a bug fixed
    // when nothing moved. `egui-0.36.1/src/layout.rs`'s `Region::expand_to_include_rect`
    // unions every child's rect into `min_rect` AND `max_rect` alike ("`max_rect` will always
    // be at least the size of `min_rect`", `Region::max_rect`'s own doc), so by the time
    // `window_resize_handles` runs the overflow is in BOTH rects and the anchors AGREE. A
    // `max_rect` band can only ever sit at-or-OUTSIDE the painted edge, never inboard.
    // MEASURED: under the anchor mutation named below this test stays GREEN, band rects and
    // all, while `the_bottom_band_stays_inside_a_window_shorter_than_its_box` reddens.
    //
    // What this test therefore gates is the CONCLUSION with its real mechanism: a body that
    // overruns its offered rect by a measured amount still gets byte-identical bands, because
    // `max_rect` grew with it.
    //
    // MUTATION that must redden this: `workspace::TITLE_CLEAR` = 40.0 in
    // `ui-theme.toml` (or any change to `BAND` or to a band rect's arithmetic) — the
    // ladders' bands move, which is what "byte-identical" forbids. RUN: it reddens here with
    // "<label>: band 1 must not move by one point" (and the chart's twin above with the same
    // wording). ⚠ The ANCHOR SWAP (replace the whole `let r = if fits(painted) { … }`
    // selection with `let r = ui.max_rect();`) deliberately does NOT redden it: that is the
    // claim, and it was RUN too — 16 tests, one failure, and it is the under-fill one.
    for (label, siblings) in LADDER_SIBLINGS {
        let mut h = Harness::ladder(siblings);
        let mut w = WinState::new(
            "ladderfix",
            "BTCUSDT",
            "1m",
            WinKind::Polymarket,
            Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
        );
        h.frames(&mut w, 14);

        let (min_r, max_r, offered) =
            (h.content_min.get(), h.content_max.get(), h.offered_max.get());
        let shift = (siblings - 1) as f32 * shipped_item_spacing().y;
        assert!(shift > 0.0, "{label}: a ladder has at least two siblings");

        // NON-VACUITY: the fixture really does model an OVERRUNNING body, by exactly the gaps
        // the shipped one leaves. Without this the byte-identity below is a claim about a body
        // that fits, which is the hole `Site::ToolFills` left.
        assert!(
            (min_r.max.y - offered.max.y - shift).abs() < 0.01,
            "{label}: the body must over-allocate by its {} item-spacing gaps — content \
                 bottom {} vs the {} it was offered (wanted a {shift}pt overrun)",
            siblings - 1,
            min_r.max.y,
            offered.max.y
        );

        // THE MECHANISM: `max_rect` absorbed that overrun, so the two anchors AGREE and the
        // `max_rect` spelling could not have placed a band inboard of the painted edge.
        assert_eq!(
            min_r, max_r,
            "{label}: `Region::expand_to_include_rect` unions the overrun into `max_rect` too, \
                 so the anchor the bands use and the one origin/main used are the same rect"
        );

        // ...and that rect IS the window a user sees. ⚠ Offset by the window frame's stroke,
        // which egui counts as part of the frame's margin, so the area rect sits that far
        // outside the content rect on every side.
        let painted = h.painted(&w);
        let edge = h.window_stroke();
        assert!(
            (painted.max.y - min_r.max.y - edge).abs() < 0.01,
            "{label}: the painted bottom edge IS the anchor's, plus the {edge}pt frame stroke \
                 ({} vs {})",
            painted.max.y,
            min_r.max.y
        );

        // THE CLAIM: every band, byte-identical to the arithmetic origin/main performed.
        for (mask, want) in expected_bands(min_r) {
            let got = h
                .band(&w, mask)
                .unwrap_or_else(|| panic!("{label}: band {mask} must be allocated"));
            assert_eq!(got, want, "{label}: band {mask} must not move by one point");
        }
    }
}

#[test]
fn the_tool_sites_fills_body_is_handed_through_unwrapped() {
    // G1's regression, and the one the layout assertions above can NEVER see. Trade and
    // Polymarket are `fills` kinds dispatched from the TOOL call site, so they go through
    // `BodyBounds::show` — and a `ScrollArea` there is LAYOUT-identical (`[false, false]` +
    // `auto_shrink(true)` reserves no space and follows the content) while putting an extra
    // `Ui` level into the body's ID CHAIN. egui keys widget state on `Ui::id`, so that is one
    // silent discard of every egui-persisted state inside the Trade window and the Polymarket
    // cockpit — scroll offsets, collapsing headers, text cursors — on upgrade.
    //
    // The reference is the SAME fixture with the wrapper bypassed, which is origin/main's
    // spelling: same window id, same kind, same geometry, same body.
    //
    // MUTATION that must redden this: delete `if self.fills { return body(ui); }` from
    // `BodyBounds::show`.
    let rect = Rect::from_min_size(pos2(180.0, 120.0), vec2(640.0, 460.0));
    let (mut hw, mut hb) = (Harness::tool_fill(true), Harness::tool_fill(false));
    let mut ww = WinState::tool("tradefix", WinKind::Trade, rect);
    let mut wb = WinState::tool("tradefix", WinKind::Trade, rect);
    hw.frames(&mut ww, 14);
    hb.frames(&mut wb, 14);

    assert!(!hw.scrolled.get(), "a `fills` kind is never told to bound its body");
    let wrapped_id = hw.body_id.get().expect("the wrapped body was drawn");
    assert_eq!(
        wrapped_id,
        hb.body_id.get().expect("the bare body was drawn"),
        "the tool site's `fills` body must get the SAME `Ui::id` it had unwrapped — every \
             auto-id widget inside the Trade window and the Polymarket cockpit is keyed on it"
    );

    // ...and the GEOMETRY is unchanged too: same painted window, same bands, same body rect.
    assert_eq!(hw.painted(&ww), hb.painted(&wb), "the painted window must be unchanged");
    assert_eq!(hw.body_max.get(), hb.body_max.get(), "the body's own rect must be unchanged");
    for (mask, _) in expected_bands(hw.content_min.get()) {
        let got = hw.band(&ww, mask).unwrap_or_else(|| panic!("band {mask} must exist"));
        let want = hb.band(&wb, mask).expect("the bare fixture's band");
        assert_eq!(got, want, "band {mask} must be unchanged by the wrapper");
    }
}

#[test]
fn a_window_too_short_for_its_bands_is_still_resizable() {
    // The bands are the ONLY resize mechanism any window has — `show_window` builds every
    // `Window` `.resizable(false)`, and egui structurally refuses the left edge for a
    // non-movable LEFT_TOP-pivot window regardless. So `window_resize_handles`' too-small
    // guard is not "skip a nicety": keyed on the PAINTED rect alone it made a tool window
    // whose body paints shorter than `workspace::TITLE_CLEAR + 2*BAND` unresizable BY ANY MEANS, for the
    // life of the window — where before `.resizable(false)` egui's own right and bottom edges
    // worked. A short body is not exotic: an empty list or a collapsed panel is one.
    //
    // MUTATION that must redden this: drop the `else if fits(ui.max_rect())` arm from
    // `window_resize_handles` (i.e. `return None` as soon as the painted rect is too small).
    let mut h = Harness::tool(vec2(520.0, 6.0)); // a body only a few points tall
    let mut w = settled_tool(&mut h, WinKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

    // NON-VACUITY: the painted content really is too short to carry a band set, and the
    // monotone high-water rect really is not — so this fixture takes the fallback arm.
    let (min_r, max_r) = (h.content_min.get(), h.content_max.get());
    assert!(
        min_r.height() < TITLE_CLEAR + 2.0 * BAND,
        "the fixture must actually be too short: painted height {}",
        min_r.height()
    );
    assert!(
        max_r.height() >= TITLE_CLEAR + 2.0 * BAND,
        "...and the fallback rect must be able to carry the bands: {}",
        max_r.height()
    );

    for mask in [1u8, 2, 8] {
        assert!(
            h.band(&w, mask).is_some(),
            "band {mask} must be allocated — without it this window can never be resized again"
        );
    }

    // ...and the bottom edge WORKS: grabbing it resizes the window.
    let h0 = w.size.y;
    drag_band(&mut h, &mut w, 8, vec2(0.0, 200.0));
    h.frames(&mut w, 4);
    assert!(w.user_sized, "the drag must latch the window as app-owned");
    assert!(w.size.y > h0 + 100.0, "the bottom edge must have dragged: {h0} -> {}", w.size.y);

    // SELF-HEALING, which is what makes the fallback's out-of-window bands acceptable: the
    // latched window is pinned to `w.size` and its body is bounded to that, so the painted
    // rect now carries the bands itself and the fallback arm is never taken again.
    let painted = h.painted(&w);
    assert!(
        (painted.height() - w.size.y).abs() < 4.0,
        "the latched window must paint at the dragged size: {} vs {}",
        painted.height(),
        w.size.y
    );
    for mask in [8u8, 1 | 8, 2 | 8] {
        let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert!(
            painted.contains_rect(band),
            "band {mask} at {band:?} must now lie INSIDE the painted window {painted:?}"
        );
    }
}

#[test]
fn the_side_bands_clear_the_scrollbars_and_the_bottom_band_overlaps_by_the_measured_2pt() {
    // The band-vs-scrollbar geometry, on a LATCHED window whose body overflows BOTH ways so
    // both floating bars are live. This is also the only test that drags the RIGHT band
    // (mask 2).
    //
    // ⚠ It models the PRODUCTION inset (`TOOL_BODY_MARGIN`), because a harness with no margin
    // is exactly the configuration this geometry says must never ship.
    //
    // MUTATION that must redden this: set `TOOL_BODY_MARGIN`'s `right` to 0 — the vertical
    // bar's strip then lands ON the right band.
    let mut h = Harness::tool(vec2(900.0, 700.0));
    // `WinKind::Data` here is an arbitrary tool-window fixture, same substitution and reason as
    // `a_body_taller_than_the_arena_keeps_the_window_and_its_bands_inside_it` above
    // (`WinKind::Connections` is deleted 2026-10-05).
    let mut w = settled_tool(&mut h, WinKind::Data, pos2(120.0, 100.0), vec2(600.0, 420.0));

    // Latch and shrink in BOTH axes, so the 900x700 body overflows the window both ways.
    drag_band(&mut h, &mut w, 8, vec2(0.0, -260.0));
    h.frames(&mut w, 4);
    drag_band(&mut h, &mut w, 1, vec2(200.0, 0.0));
    h.frames(&mut w, 4);
    assert!(h.scrolled.get(), "the window must be latched for the bars to exist");

    // THE RIGHT BAND. It must serve the drag even with the vertical bar alongside it.
    let left0 = w.pos.x;
    let right0 = w.pos.x + w.size.x;
    drag_band(&mut h, &mut w, 2, vec2(-90.0, 0.0));
    h.frames(&mut w, 4);
    assert!(
        (w.pos.x + w.size.x - (right0 - 90.0)).abs() < 2.0,
        "the right edge must follow the drag: {right0} -> {}",
        w.pos.x + w.size.x
    );
    assert!((w.pos.x - left0).abs() < 2.0, "a right drag pins the LEFT edge: {}", w.pos.x);

    // THE GEOMETRY. `body_max` is the `ScrollArea`'s `inner_rect`; a floating bar senses over
    // `max_bar_rect`, the outermost `BAR_W` points of it.
    let inner = h.body_max.get();
    let right_band = h.band(&w, 2).expect("the right band must be allocated");
    let bottom_band = h.band(&w, 8).expect("the bottom band must be allocated");

    assert!(
        inner.max.x <= right_band.min.x + 0.01,
        "the vertical bar's strip (out to x={}) must stay INBOARD of the right band (from \
             x={}) — that is what the 8pt side inset buys",
        inner.max.x,
        right_band.min.x
    );
    assert!(
        inner.max.x - BAR_W < right_band.min.x,
        "non-vacuous: the bar strip must actually sit against the band, not off in the middle \
             of the window ({} vs {})",
        inner.max.x - BAR_W,
        right_band.min.x
    );

    // ...and the ACCEPTED 2pt overlap at the bottom, where the inset is 6 rather than 8. The
    // bands are allocated LAST and `hit_test.rs`'s `find_closest_within` breaks a distance tie
    // by taking "the last one = the one on top", so the BAND wins those points and the
    // horizontal bar's grab area is 2pt thinner — never the other way round.
    let overlap = inner.max.y - bottom_band.min.y;
    assert!(
        (overlap - 2.0).abs() < 0.5,
        "the bottom band must overlap the horizontal bar's strip by the measured 2pt (inset 6 \
             vs band 8), not {overlap}"
    );
}
