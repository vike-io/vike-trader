//! The `user_sized` LATCH: drags, presses, jitter, coalesced frames, fills and maximized windows.

use super::*;

#[test]
fn a_left_drag_on_a_tool_window_moves_the_left_edge_and_pins_the_right() {
    // THE REPORTED BUG. A tool window's LEFT edge could not be dragged at all: egui refuses it
    // structurally — `PossibleInteractions::new` reads `resize_left: resizable.x && (movable
    // || pivot.x() != Align::LEFT)` and these windows are `.movable(false)` at the default
    // LEFT_TOP pivot — so the app's own bands are the only thing that can ever serve it.
    //
    // MUTATION that must redden this: drop `|| w.user_sized` from `show_window`'s
    // `pinning_now`. The band arithmetic still runs, but the end-of-frame read-back
    // (`if !pinning_now`) hands the geometry straight back to egui's stale rect and the left
    // edge snaps home.
    let mut h = Harness::tool(vec2(560.0, 520.0));
    let mut w = settled_tool(&mut h, WinKind::Greeks, pos2(240.0, 140.0), vec2(600.0, 420.0));
    assert!(!w.user_sized, "an untouched window has not latched");

    let before = w.pos.x + w.size.x; // the RIGHT edge — the invariant a left drag must pin
    let x0 = w.pos.x;
    let width0 = w.size.x;

    drag_band(&mut h, &mut w, 1, vec2(100.0, 0.0));

    assert!(w.user_sized, "moving a band latches the window as app-owned");
    assert!(
        (w.pos.x - (x0 + 100.0)).abs() < 2.0,
        "left edge must follow the drag: {x0} -> {} (wanted {})",
        w.pos.x,
        x0 + 100.0
    );
    assert!(
        (w.size.x - (width0 - 100.0)).abs() < 2.0,
        "width must shrink by the drag: {width0} -> {}",
        w.size.x
    );
    assert!(
        (w.pos.x + w.size.x - before).abs() < 2.0,
        "the RIGHT edge is pinned: {before} -> {}",
        w.pos.x + w.size.x
    );

    // ...and it STAYS moved. This half is what catches egui handing the geometry back a frame
    // later, which is exactly what the un-latched code did.
    h.frames(&mut w, 4);
    assert!(
        (w.pos.x - (x0 + 100.0)).abs() < 2.0,
        "the left edge must not snap home on a later frame: {}",
        w.pos.x
    );
    assert!(
        (w.pos.x + w.size.x - before).abs() < 2.0,
        "the right edge must stay pinned on later frames: {}",
        w.pos.x + w.size.x
    );

    // ⚠ ...and finally on what a USER sees. Every assertion above reads a value `show_window`
    // itself just wrote, so all of them would pass with the window painted somewhere else
    // entirely. The painted rect is egui's own answer.
    let painted = h.painted(&w);
    assert!(
        (painted.min.x - w.pos.x).abs() < 2.0,
        "the PAINTED left edge must be where `w.pos.x` says: {} vs {}",
        painted.min.x,
        w.pos.x
    );
    assert!(
        (painted.width() - w.size.x).abs() < 2.0,
        "the PAINTED width must be `w.size.x`: {} vs {}",
        painted.width(),
        w.size.x
    );
}

#[test]
fn a_resized_tool_window_stops_growing_to_its_content() {
    // THE OTHER REPORTED BUG: a tool window could not be dragged SMALLER, because
    // `Resize::begin` does `desired_size = desired_size.max(last_content_size)` on every
    // non-dragging frame — the window's floor is its content's minimum size.
    //
    // ⚠ The assertion is on the PAINTED rect, never on `w.size`: after the latch that value is
    // written by the band arithmetic itself, so asserting on it would pass with the whole fix
    // deleted.
    //
    // ⚠ It drags BOTH WAYS, and that is not thoroughness — it is what makes each half of the
    // fix load-bearing. The two mutations redden different halves, and a shrink-only test was
    // MEASURED green under the second one:
    //
    //   * `BodyBounds::show` -> call `body(ui)` directly (no container at all) and phase A
    //     reddens: the body reports its CONTENT size again, which re-arms `Resize`'s floor.
    //   * change only `auto_shrink(!scrolls)` to `auto_shrink(true)` and phase A stays GREEN —
    //     a scroll area is capped at the space available either way once its content
    //     OVERFLOWS, so shrinking alone cannot tell the two apart. Phase B is what reddens:
    //     with auto-shrink ON, a window dragged TALLER than its content collapses back to the
    //     content (`scroll_area.rs`'s `(true, true) => inner_size[d].min(content_size[d])`),
    //     so the painted window stops matching the size the user dragged and the bands end up
    //     outside it.
    let body = vec2(560.0, 400.0);
    let spawn = vec2(600.0, 400.0);
    let mut h = Harness::tool(body);
    let mut w = settled_tool(&mut h, WinKind::Studio, pos2(200.0, 120.0), spawn);
    // ⚠ Compared against the SPAWN height, not against `body.y`: the latter is true of any
    // window taller than its body and would hold with nothing grown at all. Against the spawn
    // it says the thing that matters — egui grew the window PAST what the app asked for.
    assert!(
        h.painted(&w).height() > spawn.y,
        "before the latch egui grows the window past its spawn size onto its content floor — \
             that is the bug ({} <= {})",
        h.painted(&w).height(),
        spawn.y
    );

    // PHASE A — drag the bottom edge UP, through the content floor.
    drag_band(&mut h, &mut w, 8, vec2(0.0, -180.0));
    h.frames(&mut w, 4);

    assert!(w.user_sized, "the bottom band latches the window too");
    assert!(h.scrolled.get(), "a latched tool body is told to bound itself");
    assert!(
        h.painted(&w).height() < body.y,
        "the PAINTED window must now be shorter than its content ({} >= {})",
        h.painted(&w).height(),
        body.y
    );

    // PHASE B — and back DOWN, past the content: the painted window must follow the drag
    // rather than collapsing onto the body it contains.
    let tall = w.size.y + 400.0;
    drag_band(&mut h, &mut w, 8, vec2(0.0, 400.0));
    h.frames(&mut w, 4);
    assert!(
        (h.painted(&w).height() - tall).abs() < 4.0,
        "the PAINTED window must match the dragged size, not the body's ({} vs {tall})",
        h.painted(&w).height()
    );
}

#[test]
fn a_side_band_latch_floors_the_axis_its_band_never_touched() {
    // A SIDE-band drag latches the window, and the two resize arms above the latch floor only
    // the axis the pressed band consumes — `MINW` under `mask & (1 | 2)`, `MINH` under
    // `mask & 8`. So without the both-axes floor a side drag pinned the window with `w.size.y`
    // left at whatever the pre-latch read-back wrote: the PAINTED content height, which for a
    // short body (an empty list, a collapsed panel) is far under `MINH`.
    //
    // That value is then the window's DECLARED geometry (`pinning_now` builds it
    // `.fixed_size(w.size)`) while the window is PAINTED somewhere else entirely, because
    // `Resize::end` takes `size[d] = state.last_content_size[d]` for a `Window` and a latched
    // body is a `ScrollArea` whose `inner_size` is floored at `min_scrolled_size` — 64pt, BOTH
    // axes, unconditional for an enabled direction. `w.size` stops describing the window a
    // user sees, which is precisely what the read-back's own comment ("for pinned windows
    // `w.size`/`w.pos` are authoritative") promises it does not do, and the visible symptom is
    // the NEXT drag on that axis not tracking the pointer: the arm's own `.max(MIN*)` snaps
    // the stale value up first, so the edge moves by something other than the delta.
    //
    // ⚠ THIS IS NOT A STRANDING TEST, and a review round asked for it as one — the claim being
    // that `fixed_size` collapses `ui.max_rect()` onto `w.size` (true) and so puts BOTH of
    // `window_resize_handles`' anchor rects under its `fits` guard, leaving the window
    // unresizable by any means. It cannot: the same 64pt `min_scrolled_size` floor holds the
    // PAINTED rect — and therefore `ui.min_rect()` — well clear of that guard whatever
    // `w.size` says. The band-allocation assertions below are kept as the REFUTATION of that
    // claim and are GREEN under the mutation named here; the DRAG assertion is the gate.
    //
    // MUTATION that must redden this: delete `w.size = w.size.max(egui::vec2(MINW, MINH));`
    // from `show_window`'s latch block — the two `.max(MIN*)` calls in the arms above are the
    // per-axis floors, so deleting the line restores the per-axis-only spelling. RUN, and it
    // reddens HERE and nowhere else: "the bottom edge must follow the pointer after a
    // side-band latch: painted height 100 -> 250 for a 90pt drag (w.size.y = 250)". The jump
    // is the bottom drag's PRESS frame — a zero delta that still runs the `mask & 8` arm,
    // whose `.max(MINH)` snaps the stale value up before the move frame adds its 90.
    let mut h = Harness::tool(vec2(520.0, 6.0)); // the same short body as the test above
    let mut w = settled_tool(&mut h, WinKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

    // NON-VACUITY: the pre-latch read-back really does leave `w.size.y` under the `MINH` the
    // bottom arm would have applied, so the floor deleted by the mutation has work to do.
    assert!(
        w.size.y < 160.0,
        "the fixture must actually spawn a window shorter than MINH: w.size.y = {}",
        w.size.y
    );

    // THE SIDE BAND — mask 1, which consumes only `d.x`.
    drag_band(&mut h, &mut w, 1, vec2(60.0, 0.0));
    h.frames(&mut w, 4);
    assert!(w.user_sized, "a side-band drag latches the window");

    // THE REFUTATION (green under the mutation, and that is the point): a latched short window
    // is NOT stranded — `min_scrolled_size` keeps the painted rect band-sized either way.
    for mask in [1u8, 2, 8, 1 | 8, 2 | 8] {
        assert!(
            h.band(&w, mask).is_some(),
            "band {mask} must be allocated after a SIDE-band latch (w.size = {:?}, \
                 painted = {:?})",
            w.size,
            h.painted(&w)
        );
    }

    // THE GATE: `w.size` describes the window a user sees, so the bottom edge TRACKS the
    // pointer. Asserted on the PAINTED rect, never on `w.size` — after the latch that value is
    // written by the band arithmetic itself, so a `w.size`-only assertion passes with the
    // floor deleted and the window painted somewhere else.
    let before = h.painted(&w).height();
    drag_band(&mut h, &mut w, 8, vec2(0.0, 90.0));
    h.frames(&mut w, 4);
    let after = h.painted(&w).height();
    assert!(
        (after - before - 90.0).abs() < 2.0,
        "the bottom edge must follow the pointer after a side-band latch: painted height \
             {before} -> {after} for a 90pt drag (w.size.y = {})",
        w.size.y
    );
}

#[test]
fn cross_axis_jitter_on_a_band_does_not_latch_the_window() {
    // The latch guard tested the WHOLE delta while the resize arithmetic above it consumes
    // only the component the mask names: the side bands read `d.x`, the bottom band `d.y`. So
    // a press on the bottom band that jittered purely HORIZONTALLY (or on a side band, purely
    // vertically) resized NOTHING and latched the window anyway — the same permanent,
    // irreversible conversion to `fixed_size` + a self-bounding body that the zero-delta guard
    // exists to prevent, one axis over. A pointer that moves on exactly one axis for a frame
    // is the common case, not the exotic one.
    //
    // MUTATION that must redden this: restore `if !fills && d != egui::Vec2::ZERO` in place of
    // the `moved_the_edge` guard in `show_window`.
    // (band, the CROSS-axis jitter it consumes none of, an IN-axis drag it must still take)
    let rounds =
        [(8u8, vec2(20.0, 0.0), vec2(0.0, -40.0)), (1u8, vec2(0.0, 20.0), vec2(-40.0, 0.0))];
    for (mask, jitter, along) in rounds {
        let mut h = Harness::tool(vec2(560.0, 520.0));
        let mut w = settled_tool(&mut h, WinKind::Account, pos2(240.0, 140.0), vec2(600.0, 420.0));
        let from = h.band(&w, mask).expect("the band must be allocated").center();
        let to = from + jitter;
        let (x0, y0) = (w.size.x, w.size.y);

        h.move_to(from);
        h.frame(&mut w); // hover
        h.button(from, true);
        h.frame(&mut w); // press — zero delta
        h.move_to(to);
        h.frame(&mut w); // the CROSS-AXIS jitter: this band consumes none of it
        h.button(to, false);
        h.frame(&mut w); // release
        h.frames(&mut w, 4);

        assert!(
            !w.user_sized,
            "band {mask} consumes none of {jitter:?}, so it must not latch the window"
        );
        assert!(!h.scrolled.get(), "...and the body must not be told to bound itself");
        assert!(
            (w.size.x - x0).abs() < 2.0 && (w.size.y - y0).abs() < 2.0,
            "...and nothing was resized: {x0}x{y0} -> {}x{}",
            w.size.x,
            w.size.y
        );

        // NON-VACUITY: the SAME band on the SAME window latches on an in-axis move, so the
        // negative above is about the axis rather than about a band that never fired.
        drag_band(&mut h, &mut w, mask, along);
        h.frames(&mut w, 4);
        assert!(w.user_sized, "band {mask} must still latch on an in-axis drag of {along:?}");
    }
}

#[test]
fn an_untouched_tool_window_still_takes_its_size_from_egui() {
    // The latch's NO-OP half, and the reason it is a latch. Nine tool kinds open at 560x400
    // and several bodies are naturally bigger, so a window pinned from frame one would open
    // into scrollbars. Until a band is grabbed the window must behave exactly as it did before
    // this change: unpinned, sized by egui, growing to fit.
    //
    // MUTATION that must redden this: make `pinning`/`pinning_now` true for every tool window
    // from frame one (`|| !fills` in place of `|| w.user_sized`). `w.size` is then never read
    // back and stays at the spawn size.
    let mut h = Harness::tool(vec2(620.0, 440.0));
    let spawn = vec2(400.0, 300.0);
    let w = settled_tool(&mut h, WinKind::News, pos2(300.0, 200.0), spawn);

    assert!(!w.user_sized, "nothing has been dragged, so nothing has latched");
    assert!(!h.scrolled.get(), "an unlatched body is never told to bound itself");
    assert!(
        w.size.y > spawn.y,
        "egui must still grow the window to its taller content: {} <= {}",
        w.size.y,
        spawn.y
    );
    assert!(w.size.x > spawn.x, "...and to its wider content: {} <= {}", w.size.x, spawn.x);
}

#[test]
fn a_bare_click_on_a_band_does_not_latch_the_window() {
    // A `Sense::drag()` widget is `dragged()` from the PRESS frame, where `drag_delta()` is
    // ZERO (`egui-0.37.0/src/interaction.rs`: "This widget is just sensitive to drags, so we
    // can mark it as dragged right away"). Latching on `Some(..)` alone therefore turned ONE
    // accidental click on a tool window's 8pt edge into a permanent conversion to `fixed_size`
    // + scrolling for the rest of the session, with no way back. Every other test here DRAGS,
    // so none of them can tell a press from a press-and-move.
    //
    // MUTATION that must redden this: drop the per-axis NON-ZERO tests from the latch guard —
    // `let moved_the_edge = mask & (1 | 2 | 8) != 0;` — so the press frame's ZERO delta
    // latches. ⚠ This said "drop `&& d != egui::Vec2::ZERO` from the latch guard", which was
    // a leftover: that guard was rewritten to `moved_the_edge` and the string appears nowhere
    // in production, so the only instruction telling the next author how to prove this gate
    // was UNPERFORMABLE. Its sibling
    // (`cross_axis_jitter_on_a_band_does_not_latch_the_window`) names the same text as a
    // RESTORE, which is what it is — the old code. The re-worded mutation was RUN: it reddens
    // here with "a bare click must NOT latch the window" (and, expectedly, reddens that
    // sibling and `the_bottom_band_stays_inside_a_window_shorter_than_its_box` too).
    let mut h = Harness::tool(vec2(560.0, 520.0));
    let mut w = settled_tool(&mut h, WinKind::Account, pos2(240.0, 140.0), vec2(600.0, 420.0));
    let at = h.band(&w, 8).expect("the bottom band must be allocated").center();

    h.move_to(at);
    h.frame(&mut w); // hover
    h.button(at, true);
    h.frame(&mut w); // press — `dragged()`, zero delta
    h.button(at, false);
    h.frame(&mut w); // release, pointer never moved
    h.frames(&mut w, 4);

    assert!(!w.user_sized, "a bare click must NOT latch the window");
    assert!(!h.scrolled.get(), "...and the body must not be told to bound itself");
}

#[test]
fn a_press_and_a_move_coalesced_into_one_frame_keeps_the_delta_they_carried() {
    // `pinning` was captured BEFORE the resize block, so on the very frame a window latched
    // the end-of-frame read-back still ran and overwrote the delta just applied with egui's
    // pre-drag rect. With the latch moved onto MOVEMENT that frame ALWAYS carries a non-zero
    // delta, so this stopped being theoretical: a press and a move coalescing into one frame's
    // `RawInput` is routine with a high-polling mouse.
    //
    // ⚠ The move stays INSIDE the 8pt band on purpose: egui hit-tests with `interact_pos`,
    // which every pointer event in the frame updates, so a coalesced move that left the band
    // would never press the band at all.
    //
    // MUTATION that must redden this: use the pre-frame `pinning` for the read-back again
    // (`if !pinning` in place of `if !pinning_now`).
    let mut h = Harness::tool(vec2(560.0, 520.0));
    let mut w = settled_tool(&mut h, WinKind::Options, pos2(240.0, 140.0), vec2(600.0, 420.0));
    let band = h.band(&w, 8).expect("the bottom band must be allocated");
    let from = pos2(band.center().x, band.min.y + 1.0);
    let to = pos2(from.x, from.y + 6.0);

    h.move_to(from);
    h.frame(&mut w); // hover, so the press frame is the FIRST frame with a button down
    let h0 = w.size.y;
    h.press_and_move(from, to);
    h.frame(&mut w); // press AND move, one `RawInput`
    h.button(to, false);
    h.frame(&mut w); // release
    h.frames(&mut w, 4);

    assert!(w.user_sized, "a coalesced press+move is a movement, so it latches");
    assert!(
        (w.size.y - (h0 + 6.0)).abs() < 1.0,
        "the coalesced frame's delta must survive: {h0} -> {} (wanted {})",
        w.size.y,
        h0 + 6.0
    );
    assert!(
        (h.painted(&w).height() - w.size.y).abs() < 2.0,
        "...and the PAINTED window must agree: {} vs {}",
        h.painted(&w).height(),
        w.size.y
    );
}

#[test]
fn the_bodys_id_is_stable_across_the_latch() {
    // egui keys widget state on `Ui::id`, so a `ScrollArea` INSERTED when the latch flips
    // changes the id of the whole tool body and discards every auto-id-keyed piece of state
    // inside it the first time a user drags an edge: the Studio code editor's cursor,
    // selection and UNDO STACK, every inner scroll offset, every `CollapsingState`, a
    // half-typed field in the Connections editor. `BodyBounds::show` therefore builds the
    // container UNCONDITIONALLY and toggles only its sizing.
    //
    // MUTATION that must redden this: spell `BodyBounds::show` as
    // `if self.scrolls { ScrollArea::both().auto_shrink(false).show(ui, body).inner } else {
    // body(ui) }` — the pre-latch id is then the Frame's child, not the scroll area's.
    let mut h = Harness::tool(vec2(560.0, 520.0));
    let mut w = settled_tool(&mut h, WinKind::Studio, pos2(240.0, 140.0), vec2(600.0, 420.0));
    let before = h.body_id.get().expect("the body was drawn");
    assert!(!h.scrolled.get(), "the fixture must start UNLATCHED for this to mean anything");

    drag_band(&mut h, &mut w, 8, vec2(0.0, -120.0));
    h.frames(&mut w, 4);

    assert!(h.scrolled.get(), "the fixture must have latched for this to mean anything");
    assert_eq!(
        h.body_id.get().expect("the body was drawn"),
        before,
        "the body's `Ui::id` must not change when the window latches — every auto-id widget \
             inside it (text cursors, undo stacks, collapsing headers, inner scroll offsets) is \
             keyed on it"
    );
}

#[test]
fn a_fill_window_never_takes_the_latch() {
    // The safety half. A `fills` kind (Chart/Trade/Polymarket) is app-owned from frame one and
    // its body sizes itself to the available rect — handing it `scrolls == true` would wrap a
    // self-filling body in a ScrollArea and break it. Its band drags must therefore keep
    // working while the latch stays untaken.
    //
    // MUTATIONS that must redden this: drop the `!fills` guard around `w.user_sized = true`
    // (the first half), or drop the `!fills` from `BodyBounds { scrolls: !fills && … }` (the
    // second half — which no latch guard can catch, because `user_sized` is set BY HAND there).
    // Two of the three `fills` kinds, each at the call site it is really drawn at: the Chart at
    // the chart site, and the Trade window (which took the DOM's place) at the TOOL site through
    // the production wrapper. The Polymarket cockpit rides the same arm as the Trade window.
    let cases = [
        (
            "Chart",
            Harness::fill(),
            WinState::new(
                "chartfix",
                "BTCUSDT",
                "1m",
                WinKind::Chart,
                Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
            ),
        ),
        (
            "Trade",
            Harness::tool_fill(true),
            WinState::tool(
                "tradefix",
                WinKind::Trade,
                Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
            ),
        ),
    ];
    for (label, mut h, mut w) in cases {
        h.frames(&mut w, 14);

        let width0 = w.size.x;
        drag_band(&mut h, &mut w, 1, vec2(120.0, 0.0));

        // Non-vacuous: prove the band actually fired before believing what it did NOT set.
        assert!(
            w.size.x < width0 - 50.0,
            "{label}: the fill window must still resize from its left band: {width0} -> {}",
            w.size.x
        );
        assert!(!w.user_sized, "{label}: a fills kind must NEVER take the latch");
        assert!(!h.scrolled.get(), "{label}: ...and must never be told to scroll its body");

        // ⚠ The assertion above could not fail on its own: `scrolls` is `!fills && user_sized`
        // and the latch guard already holds `user_sized` false, so it restates the line before it.
        // Set the flag BY HAND and the `scrolls` expression is falsified independently of the
        // latch.
        w.user_sized = true;
        h.frame(&mut w);
        assert!(
            !h.scrolled.get(),
            "{label}: a `fills` kind must not scroll its body even with `user_sized` set by hand \
             — the `!fills` in `scrolls` is a second, independent guard"
        );
    }
}

#[test]
fn bands_do_not_register_while_maximized_or_pinned() {
    // A band must not fire while the window is maximized (its geometry is the full bounds
    // every frame) nor during the arrange/restore settle (`force_frames > 0`), where the app
    // is still committing a forced rect.
    //
    // MUTATIONS that must redden this: drop `!w.maximized` from `resizable_now` (the first
    // arm), or drop `w.force_frames == 0` (the second).
    //
    // ⚠ Each arm gets its OWN context, reads after several frames in the condition, and then
    // CLEARS the condition and asserts the band comes BACK. Without that second half,
    // `read_response(..).is_none()` is also the answer for a window that was never shown, an
    // id that changed, or a fixture too small for `window_resize_handles`' own size guard —
    // none of which is the thing under test. (`Context::read_response` also falls back to an
    // earlier pass, so a single frame could answer with a band allocated before the condition
    // was set.)
    fn left(h: &Harness, w: &WinState) -> Option<Rect> {
        h.band(w, 1u8)
    }
    fn fixture(id: &str) -> WinState {
        WinState::tool(
            id,
            WinKind::Data,
            Rect::from_min_size(pos2(200.0, 150.0), vec2(600.0, 420.0)),
        )
    }

    // CONTROL — the same fixture with neither guard active DOES allocate a band, so the two
    // negatives below cannot pass vacuously.
    let mut h = Harness::tool(vec2(560.0, 460.0));
    let w = settled_tool(&mut h, WinKind::Data, pos2(200.0, 150.0), vec2(600.0, 420.0));
    assert!(left(&h, &w).is_some(), "control: a settled tool window allocates its left band");

    // MAXIMIZED — and then un-maximized, which must bring the band back.
    let mut h = Harness::tool(vec2(560.0, 460.0));
    let mut w = fixture("maxed");
    w.maximized = true;
    h.frames(&mut w, 14);
    assert!(left(&h, &w).is_none(), "no band may be allocated while maximized");
    w.maximized = false;
    h.frames(&mut w, 14);
    assert!(left(&h, &w).is_some(), "...and the band must return once it is restored");

    // FORCE-PINNED: a freshly-opened window carries `OPEN_PIN_FRAMES`, so three frames in is
    // still mid-settle — and once the pin drains the band must appear.
    let mut h = Harness::tool(vec2(560.0, 460.0));
    let mut w = fixture("pinned");
    h.frames(&mut w, 3);
    assert!(
        w.force_frames > 0,
        "the fixture must still be mid-settle for this arm to mean anything"
    );
    assert!(left(&h, &w).is_none(), "no band may be allocated during a forced-geometry pin");
    h.frames(&mut w, 14);
    assert_eq!(w.force_frames, 0, "the pin must have drained");
    assert!(left(&h, &w).is_some(), "...and the band must appear once it has");
}
