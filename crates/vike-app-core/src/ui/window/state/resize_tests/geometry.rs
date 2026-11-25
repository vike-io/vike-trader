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
    // ⚠ Since 2026-11-16 a tool body is bounded by its window from open (`WindowBodyBounds` fills it),
    // so the two rects AGREE for a tool window and this fixture no longer diverges; it holds the
    // bands inside the painted window of a body far shorter than its box.
    let mut h = Harness::tool(vec2(520.0, 100.0)); // a body FAR shorter than the box
    let mut w = settled_tool(&mut h, WindowKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

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
    // The arena is NOT the problem and the theory it suggests is wrong: `app_ui/mod.rs` adds
    // `egui::Panel::bottom("statusbar")` BEFORE the `CentralPanel`, and a bottom panel sets
    // `cursor.max[axis] = visible_outer_rect.min[axis]` on its parent
    // (`egui-0.37.0/src/containers/panel.rs`), so `app.desktop` genuinely excludes the strip —
    // see [`arena`], which is shaped the same way.
    //
    // What is true is the WINDOW. Until it is app-owned a tool window is painted at its
    // CONTENT, whatever its declared size says: `Window::new` builds its `Resize` with
    // `.with_stroke(false)` and `Window::show_dyn` forces `resize.resizable(false)`, so
    // `Resize::end` always takes
    // `else { size[d] = state.last_content_size[d]; }`. A body taller than the arena therefore
    // paints a window whose bottom edge is past `bounds` — under the status bar, exactly as
    // reported — and `window_resize_handles` then allocates the bottom band and both bottom
    // corners out there, where `Ui::interact`'s `interact_rect: self.clip_rect().intersect(
    // rect)` is NEGATIVE (the area's clip rect is its `constrain_rect`) and
    // `egui-0.37.0/src/hit_test.rs` drops the widget before measuring anything. The band
    // exists, `read_response` answers for it, and it can never be hit.
    //
    // MEASURED under the egui-sized spelling tool windows had until 2026-11-16: the window was
    // painted to `..1476` against an arena ending at `978`, its bottom band allocated 489pt
    // outside the clip, and a full press-move-release on it moved the window `1436 -> 1436`.
    //
    // MUTATION that must redden this: build a tool window `.default_size(w.size)` and read its
    // size back from egui (the window then grows to its 1400pt body, past the status bar).
    const BODY_H: f32 = 1400.0;
    let mut h = Harness::tool(vec2(520.0, BODY_H)).in_arena();
    let arena = arena();
    // `WindowKind::Data` here is an arbitrary tool-window fixture (`WindowKind::Connections`, the kind
    // the bug was originally reported on, is deleted 2026-10-05) — this test's vertical-geometry
    // assertions are independent of `carries_title_tabs()`, which only affects the bar's
    // HORIZONTAL layout.
    let mut w = settled_tool(&mut h, WindowKind::Data, pos2(120.0, 60.0), vec2(560.0, 400.0));

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

    // ...and the bottom edge WORKS, the half the owner could not reach at all. The window keeps
    // the size it opened at, whatever its body wants.
    let h0 = w.size.y;
    assert!((h0 - 400.0).abs() < 1.0, "the window keeps its spawn height: {h0}");
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

    // ...and back DOWN again.
    drag_band(&mut h, &mut w, 8, vec2(0.0, 60.0));
    h.frames(&mut w, 4);
    assert!(
        (w.size.y - (h0 - 60.0)).abs() < 2.0,
        "the bottom edge must drag back down too: {}",
        w.size.y
    );

    assert!(h.scrolled.get(), "the body bounds itself");
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
    // MUTATION that must redden this: as above, the egui-sized spelling — it reddens on the
    // painted WIDTH.
    const BODY_W: f32 = 1400.0;
    let mut h = Harness::tool(vec2(BODY_W, 300.0)).in_arena();
    let arena = arena();
    let mut w = settled_tool(&mut h, WindowKind::Studio, pos2(60.0, 80.0), vec2(560.0, 400.0));

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
    let right0 = w.screen_position.x + w.size.x;
    let x0 = w.screen_position.x;
    drag_band(&mut h, &mut w, 1, vec2(90.0, 0.0));
    h.frames(&mut w, 4);
    assert!(
        (w.screen_position.x - (x0 + 90.0)).abs() < 2.0,
        "the left edge must follow the drag: {x0} -> {}",
        w.screen_position.x
    );
    assert!(
        (w.screen_position.x + w.size.x - right0).abs() < 2.0,
        "...with the RIGHT edge pinned: {right0} -> {}",
        w.screen_position.x + w.size.x
    );

    assert!(
        h.body_max.get().width() < BODY_W,
        "...which is what BOUNDS it: offered {} of {BODY_W}",
        h.body_max.get().width()
    );
}

#[test]
fn a_window_spawned_larger_than_the_arena_is_capped_at_it() {
    // THE ARENA CEILING: a size larger than the arena (a saved size from a bigger screen, a
    // default on a small one) is capped at it, and the window is placed so it fits, so every
    // band it allocates is inside the clip.
    //
    // MUTATION that must redden this: delete the `w.size = w.size.min(..)` cap at the top of
    // `show_window`, or the placement that makes a tool window FIT below it.
    let mut h = Harness::tool(vec2(300.0, 200.0)).in_arena();
    let arena = arena();
    let w = settled_tool(&mut h, WindowKind::News, pos2(400.0, 300.0), vec2(1500.0, 1300.0));

    assert!(
        (w.size.x - arena.width()).abs() < 1.0 && (w.size.y - arena.height()).abs() < 1.0,
        "the size is capped at the arena {:?}: {:?}",
        arena.size(),
        w.size
    );
    let painted = h.painted(&w);
    assert!(arena.contains_rect(painted.shrink(1.0)), "inside the arena {arena:?}: {painted:?}");
    for mask in [8u8, 1 | 8, 2 | 8, 1, 2] {
        let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert!(arena.contains_rect(band), "band {mask} at {band:?} inside the arena");
    }
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
    // (`egui-0.37.0/src/containers/resize.rs`, `Resize::begin`'s `inner_rect =
    // Rect::from_min_size(position, state.desired_size)`). The anchor is gated where the two
    // rects CAN differ, which is where a body UNDER-fills its box —
    // `the_bottom_band_stays_inside_a_window_shorter_than_its_box`. No `fills` kind can reach
    // that state, so no `fills` test gates the anchor and none pretends to.
    let mut h = Harness::fill();
    let mut w = WindowState::new(
        "chartfix",
        "BTCUSDT",
        "1m",
        WindowKind::Chart,
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
    // when nothing moved. `egui-0.37.0/src/layout.rs`'s `Region::expand_to_include_rect`
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
        let mut w = WindowState::new(
            "ladderfix",
            "BTCUSDT",
            "1m",
            WindowKind::Polymarket,
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
    // `WindowBodyBounds::show` — and a `ScrollArea` there is LAYOUT-identical (`[false, false]` +
    // `auto_shrink(true)` reserves no space and follows the content) while putting an extra
    // `Ui` level into the body's ID CHAIN. egui keys widget state on `Ui::id`, so that is one
    // silent discard of every egui-persisted state inside the Trade window and the Polymarket
    // cockpit — scroll offsets, collapsing headers, text cursors — on upgrade.
    //
    // The reference is the SAME fixture with the wrapper bypassed, which is origin/main's
    // spelling: same window id, same kind, same geometry, same body.
    //
    // MUTATION that must redden this: delete `if self.fills { return body(ui); }` from
    // `WindowBodyBounds::show`.
    let rect = Rect::from_min_size(pos2(180.0, 120.0), vec2(640.0, 460.0));
    let (mut hw, mut harness_bare) = (Harness::tool_fill(true), Harness::tool_fill(false));
    let mut ww = WindowState::tool("tradefix", WindowKind::Trade, rect);
    let mut wb = WindowState::tool("tradefix", WindowKind::Trade, rect);
    hw.frames(&mut ww, 14);
    harness_bare.frames(&mut wb, 14);

    assert!(!hw.scrolled.get(), "a `fills` kind is never told to bound its body");
    let wrapped_id = hw.body_id.get().expect("the wrapped body was drawn");
    assert_eq!(
        wrapped_id,
        harness_bare.body_id.get().expect("the bare body was drawn"),
        "the tool site's `fills` body must get the SAME `Ui::id` it had unwrapped — every \
             auto-id widget inside the Trade window and the Polymarket cockpit is keyed on it"
    );

    // ...and the GEOMETRY is unchanged too: same painted window, same bands, same body rect.
    assert_eq!(hw.painted(&ww), harness_bare.painted(&wb), "the painted window must be unchanged");
    assert_eq!(
        hw.body_max.get(),
        harness_bare.body_max.get(),
        "the body's own rect must be unchanged"
    );
    for (mask, _) in expected_bands(hw.content_min.get()) {
        let got = hw.band(&ww, mask).unwrap_or_else(|| panic!("band {mask} must exist"));
        let want = harness_bare.band(&wb, mask).expect("the bare fixture's band");
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
    // Since 2026-11-16 a tool body is bounded from open and paints at least its scroll area's 64pt
    // floor, so a tool window SPAWNED shorter than `TITLE_CLEAR + 2*BAND` still paints a rect that
    // carries the bands (the `max_rect` fallback that used to serve it is deleted).
    //
    // MUTATION that must redden this: in `WindowBodyBounds::show`, hand the body straight through for a
    // tool kind too (no `ScrollArea`, so no 64pt floor): the painted rect then cannot carry bands.
    let mut h = Harness::tool(vec2(520.0, 6.0)); // a body only a few points tall
    let mut w = settled_tool(&mut h, WindowKind::News, pos2(300.0, 160.0), vec2(600.0, 20.0));

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
    assert!(w.user_sized, "the drag records a chosen size");
    assert!(w.size.y > h0 + 100.0, "the bottom edge must have dragged: {h0} -> {}", w.size.y);

    // The window paints at the dragged size, and its bands sit inside it.
    let painted = h.painted(&w);
    assert!(
        (painted.height() - w.size.y).abs() < 4.0,
        "the window must paint at the dragged size: {} vs {}",
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
    // `WindowKind::Data` here is an arbitrary tool-window fixture, same substitution and reason as
    // `a_body_taller_than_the_arena_keeps_the_window_and_its_bands_inside_it` above
    // (`WindowKind::Connections` is deleted 2026-10-05).
    let mut w = settled_tool(&mut h, WindowKind::Data, pos2(120.0, 100.0), vec2(600.0, 420.0));

    // Latch and shrink in BOTH axes, so the 900x700 body overflows the window both ways.
    drag_band(&mut h, &mut w, 8, vec2(0.0, -260.0));
    h.frames(&mut w, 4);
    drag_band(&mut h, &mut w, 1, vec2(200.0, 0.0));
    h.frames(&mut w, 4);
    assert!(h.scrolled.get(), "the window must be latched for the bars to exist");

    // THE RIGHT BAND. It must serve the drag even with the vertical bar alongside it.
    let left0 = w.screen_position.x;
    let right0 = w.screen_position.x + w.size.x;
    drag_band(&mut h, &mut w, 2, vec2(-90.0, 0.0));
    h.frames(&mut w, 4);
    assert!(
        (w.screen_position.x + w.size.x - (right0 - 90.0)).abs() < 2.0,
        "the right edge must follow the drag: {right0} -> {}",
        w.screen_position.x + w.size.x
    );
    assert!(
        (w.screen_position.x - left0).abs() < 2.0,
        "a right drag pins the LEFT edge: {}",
        w.screen_position.x
    );

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

#[test]
fn every_tool_kind_keeps_the_rect_it_opened_at_whatever_its_content() {
    // THE SWEEP for the owner's ruling of 2026-11-16, "a window keeps its own size": News grew to
    // the whole screen on one-line headlines (its close button pushed off-screen), the Calendar
    // crept right on open, the Tearsheet shifted when a picker changed its content's width. Every
    // tool kind, at the size it opens at (`window::spawn::tool_window_size`), in a NARROW and a WIDE
    // arena, with a body far larger than any window plus a 300-character line (wrapping, and held
    // to one line), and with a body of a few points: the painted rect after 30 frames is the rect
    // of the first frame, and it is the opening size capped at the arena.
    //
    // MUTATION that must redden this: build a tool window `.default_pos(..).default_size(..)` and
    // read its rect back from egui each frame (the spelling until 2026-11-16).
    let narrow = Rect::from_min_size(pos2(0.0, 40.0), vec2(700.0, 560.0));
    let fills = |k: &WindowKind| {
        matches!(k, WindowKind::Chart | WindowKind::Trade | WindowKind::Polymarket)
    };
    for kind in WindowKind::ALL.into_iter().filter(|k| !fills(k)) {
        let spawn = crate::ui::window::spawn::tool_window_size(kind);
        for bounds in [narrow, arena()] {
            for (body, long_text) in [(vec2(4000.0, 3000.0), true), (vec2(40.0, 10.0), false)] {
                let mut h = Harness::tool(body);
                h.bounds = bounds;
                h.long_text = long_text;
                let at = Rect::from_min_size(bounds.min + vec2(20.0, 20.0), spawn);
                let mut w = WindowState::tool("sweep", kind, at);
                h.frame(&mut w);
                let open = h.painted(&w);
                h.frames(&mut w, 30);
                let case = format!("{kind:?}, body {body:?}, arena {bounds:?}");
                assert_eq!(h.painted(&w), open, "{case}: the window moved or resized itself");
                let size = spawn.min(bounds.size());
                assert!(
                    (open.width() - size.x).abs() < 2.0 && (open.height() - size.y).abs() < 2.0,
                    "{case}: painted {open:?}, wanted the opening size {size:?}"
                );
                assert!(
                    bounds.contains_rect(open.shrink(1.0)),
                    "{case}: {open:?} inside the arena"
                );
            }
        }
    }
}
