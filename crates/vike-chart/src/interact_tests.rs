use super::*;

fn bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            t: i as f64,
            ot: 1_700_000_000_000 + i as i64 * 60_000,
            o: 1.0,
            h: 2.0,
            l: 0.5,
            c: 1.5,
            v: 10.0,
        })
        .collect()
}

#[test]
fn on_series_change_arms_seed_refit_and_keeps_maximize() {
    // Simulate a chart the user zoomed on (auto_y off) with a settled bar count and the
    // price pane maximized. A symbol swap must re-arm the seed/reset path (last_len == 0),
    // re-engage follow + auto_y, clear the scale-fallback latch, and preserve maximize.
    let mut f =
        FollowLive { on: false, auto_y: false, price_maximized: true, ..Default::default() };
    f.last_len = 500;
    f.fallback_latched = true;
    f.pending_seed = false;

    f.on_series_change();

    assert!(f.on, "follow-live re-engaged");
    assert!(f.auto_y, "y-autofit re-engaged");
    assert_eq!(f.last_len, 0, "last_len zeroed → chart::draw seed guard fires next frame");
    assert!(!f.fallback_latched, "stale scale-fallback latch cleared for the new series");
    assert!(f.price_maximized, "price-pane maximize preserved (view chrome, not per-series)");
}

#[test]
fn visible_slice_maps_x_range_to_indices() {
    let b = bars(100);
    // interior window: ±1 bar margin like the old filter
    let s = visible_slice(&b, 10.0, 20.0);
    assert_eq!(s.first().unwrap().t, 9.0);
    assert_eq!(s.last().unwrap().t, 21.0);
    // clamped at both ends
    let s = visible_slice(&b, -50.0, 5.0);
    assert_eq!(s.first().unwrap().t, 0.0);
    assert_eq!(s.last().unwrap().t, 6.0);
    let s = visible_slice(&b, 95.0, 500.0);
    assert_eq!(s.first().unwrap().t, 94.0);
    assert_eq!(s.last().unwrap().t, 99.0);
    // fully outside → empty; empty input → empty
    assert!(visible_slice(&b, 200.0, 300.0).is_empty());
    assert!(visible_slice(&b, -30.0, -10.0).is_empty());
    assert!(visible_slice(&[], 0.0, 10.0).is_empty());
}

#[test]
fn visible_slice_equals_old_linear_filter() {
    let b = bars(64);
    for (x0, x1) in [(0.0, 63.0), (5.3, 9.9), (-2.0, 1.0), (62.5, 70.0), (31.0, 31.0)] {
        let new: Vec<f64> = visible_slice(&b, x0, x1).iter().map(|bb| bb.t).collect();
        let old: Vec<f64> =
            b.iter().filter(|bb| bb.t >= x0 - 1.0 && bb.t <= x1 + 1.0).map(|bb| bb.t).collect();
        assert_eq!(new, old, "mismatch at ({x0},{x1})");
    }
}

#[test]
fn follow_shift_pins_right_edge_keeping_width() {
    let (nx0, nx1) = follow_shift(10.0, 60.0, 99.0);
    assert_eq!(nx1, 100.0); // last bar + 1 margin, same as Reset's fx_max
    assert_eq!(nx1 - nx0, 50.0); // width preserved
}

#[test]
fn follow_engaged_hysteresis() {
    // at/near the live edge → engaged; panned away → disengaged
    assert!(follow_engaged(100.0, 99.0));
    assert!(follow_engaged(98.6, 99.0));
    assert!(!follow_engaged(90.0, 99.0));
}

// --- T2 review fixes: the scale fallback must be STICKY (spec §1: "stays
// Linear with a status hint until the user re-toggles — never a per-frame
// flip"), not recomputed per frame from the visible slice. ---

#[test]
fn scale_fallback_latches_and_stays_latched_across_pans() {
    let mut f = FollowLive::default();
    // supported Log frames → Log, no latch
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
    assert_eq!(f.resolve_scale(ScaleMode::Log, 50.0, 0.0), ScaleMode::Log);
    // pan into a y<=0 region → falls back to Linear AND latches
    assert_eq!(f.resolve_scale(ScaleMode::Log, -5.0, 0.0), ScaleMode::Linear);
    // pan BACK to a supported region → STAYS Linear (sticky — this is the
    // per-frame-flip hazard the spec forbids; a per-frame recompute would
    // return Log here)
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
}

#[test]
fn scale_fallback_unlatches_only_on_requested_change() {
    let mut f = FollowLive::default();
    // latch via an unsupported Log view
    assert_eq!(f.resolve_scale(ScaleMode::Log, -1.0, 0.0), ScaleMode::Linear);
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
    // user re-toggles to Linear → latch clears
    assert_eq!(f.resolve_scale(ScaleMode::Linear, 100.0, 0.0), ScaleMode::Linear);
    // user re-toggles back to Log over a NOW-supported view → Log again
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
    // and an unsupported re-toggle re-latches immediately (Percent, anchor 0)
    assert_eq!(f.resolve_scale(ScaleMode::Percent, 100.0, 0.0), ScaleMode::Linear);
    assert_eq!(f.resolve_scale(ScaleMode::Percent, 100.0, 63_000.0), ScaleMode::Linear);
}

// --- T3 carry-over fix: the latch must NOT engage on a data-absent frame
// (empty visible slice for Log, no closed bar yet for Percent) — only a
// genuine unsupported value FROM real data may latch. Callers signal
// "no data" with NaN (see `scale::has_data`). ---

#[test]
fn scale_fallback_does_not_latch_on_empty_visible_slice_log() {
    let mut f = FollowLive::default();
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
    // this frame's visible slice is empty -> raw_lo is NaN (no data): degrade
    // for the frame but must NOT latch.
    assert_eq!(f.resolve_scale(ScaleMode::Log, f64::NAN, 0.0), ScaleMode::Linear);
    // a later frame with real supported data resumes Log — proves no latch.
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
}

#[test]
fn scale_fallback_latches_on_genuine_nonpositive_log_data() {
    let mut f = FollowLive::default();
    // real (finite) non-positive visible low -> genuine unsupported DATA -> latches.
    assert_eq!(f.resolve_scale(ScaleMode::Log, -5.0, 0.0), ScaleMode::Linear);
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
    // stays latched
}

#[test]
fn scale_fallback_does_not_latch_on_no_closed_bar_percent() {
    let mut f = FollowLive::default();
    // no closed bar yet -> anchor is NaN (no data): degrade without latching.
    assert_eq!(f.resolve_scale(ScaleMode::Percent, 100.0, f64::NAN), ScaleMode::Linear);
    // a later frame with a real closed-bar anchor resumes Percent — proves no latch.
    assert_eq!(f.resolve_scale(ScaleMode::Percent, 100.0, 63_000.0), ScaleMode::Percent);
}

#[test]
fn scale_fallback_peek_is_read_only() {
    let mut f = FollowLive::default();
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
    // peek over an unsupported view reports Linear but does NOT latch...
    assert_eq!(f.peek_scale(ScaleMode::Log, -1.0, 0.0), ScaleMode::Linear);
    assert_eq!(f.resolve_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Log);
    // ...while peek after a real latch reports the latched Linear even
    // over a supported view
    assert_eq!(f.resolve_scale(ScaleMode::Log, -1.0, 0.0), ScaleMode::Linear);
    assert_eq!(f.peek_scale(ScaleMode::Log, 100.0, 0.0), ScaleMode::Linear);
}

// --- T4 gutter drag math: y_drag_zoom / x_drag_zoom (pure, TDD) ---
//
// The two tests below RE-DERIVE the expected factor by evaluating the same `exp(±0.005 * d)`
// the functions do, then compare within 1e-9. They call `libm::exp` for the same reason the
// production sites now do — and here it is not merely hygiene: an expected value computed with
// the `f64::exp` METHOD would come from the PLATFORM's libm while the value under test came
// from the `libm` crate, so the assertion would be measuring the gap between two
// implementations rather than checking the function. The 1e-9 tolerance is wide enough to
// absorb that today, which is precisely why it would hide the mismatch instead of reporting
// it; keeping both sides on one implementation removes the question.
//
// ⚠ These re-derivations also RESTATE the `0.005` rate as a literal rather than referencing
// `K_DRAG`, deliberately — a test that reads the constant it is checking cannot catch a change
// to it. That predates this conversion and is left as it stands.

#[test]
fn y_drag_zoom_dy_zero_is_identity() {
    let (n0, n1) = y_drag_zoom(3.0, 7.0, 0.0);
    assert_eq!(n0, 3.0);
    assert_eq!(n1, 7.0);
}

#[test]
fn y_drag_zoom_preserves_center_and_applies_exp_factor() {
    let (ty0, ty1) = (10.0, 20.0); // center 15.0, half-width 5.0
    let dy = 40.0_f32;
    let (n0, n1) = y_drag_zoom(ty0, ty1, dy);
    let factor = libm::exp(0.005_f64 * dy as f64);
    let center = (n0 + n1) / 2.0;
    assert!((center - 15.0).abs() < 1e-9, "center drifted: {center}");
    let new_half = (n1 - n0) / 2.0;
    assert!(
        (new_half - 5.0 * factor).abs() < 1e-9,
        "factor mismatch: {new_half} vs {}",
        5.0 * factor
    );
}

#[test]
fn y_drag_zoom_negative_dy_compresses() {
    // dy negative (drag up) -> factor < 1 -> narrower range, same center
    let (n0, n1) = y_drag_zoom(0.0, 10.0, -100.0);
    assert!(n1 - n0 < 10.0);
    assert!((n0 + n1 - 10.0).abs() < 1e-9);
}

#[test]
fn x_drag_zoom_dx_zero_is_identity() {
    let (n0, n1) = x_drag_zoom(10.0, 50.0, 0.0);
    assert_eq!(n0, 10.0);
    assert_eq!(n1, 50.0);
}

#[test]
fn x_drag_zoom_pins_right_edge_exactly_and_scales_width() {
    let (cx0, cx1) = (0.0, 100.0);
    let dx = 20.0_f32;
    let (n0, n1) = x_drag_zoom(cx0, cx1, dx);
    assert_eq!(n1, cx1, "right edge must be bit-equal to the input");
    let factor = libm::exp(-0.005_f64 * dx as f64);
    let new_w = n1 - n0;
    assert!((new_w - 100.0 * factor).abs() < 1e-9, "width mismatch: {new_w} vs {}", 100.0 * factor);
}

#[test]
fn x_drag_zoom_positive_dx_narrows_negative_dx_widens() {
    let (cx0, cx1) = (0.0, 100.0);
    let (n0_pos, n1_pos) = x_drag_zoom(cx0, cx1, 50.0);
    assert!(n1_pos - n0_pos < 100.0);
    assert_eq!(n1_pos, cx1);
    let (n0_neg, n1_neg) = x_drag_zoom(cx0, cx1, -50.0);
    assert!(n1_neg - n0_neg > 100.0);
    assert_eq!(n1_neg, cx1);
}

// --- T5: auto_y state-transition table + the seed/Reset y-write source ---

#[test]
fn auto_y_state_transition_table() {
    // Every real call site in chart.rs funnels through `next_auto_y` (the
    // Nav::Reset arm, the pending_seed block, and the y-gutter drag/dbl-
    // click handlers) — this table doubles as the T5 audit: auto→manual
    // ONLY via Drag; manual→auto via DblClick, Reset (Auto button/⟳/
    // bottom-gutter dbl-click), or Seed (history reload).
    let cases = [
        (true, AutoYEvent::Drag, false),
        (false, AutoYEvent::Drag, false), // idempotent while already manual
        (false, AutoYEvent::DblClick, true),
        (true, AutoYEvent::DblClick, true), // idempotent while already auto
        (false, AutoYEvent::Reset, true),
        (true, AutoYEvent::Reset, true),
        (false, AutoYEvent::Seed, true),
        (true, AutoYEvent::Seed, true),
    ];
    for (cur, ev, expected) in cases {
        assert_eq!(next_auto_y(cur, ev), expected, "{cur} + {ev:?} -> expected {expected}");
    }
}

#[test]
fn resolve_ty_seed_or_reset_discards_stale_prev_entirely() {
    // Fabricate a "stale, wrong-space" prev the way a real frame could:
    // a manual range from BEFORE a symbol swap, migrated into Log space
    // by T2's own convert_bounds — not even same-space staleness, but
    // numerically meaningless for the NEW series.
    let stale_linear = (56_700.0, 69_300.0); // BTC-ish manual range
    let stale_in_log_space = crate::scale::convert_bounds(
        ScaleMode::Linear,
        ScaleMode::Log,
        0.0,
        0.0,
        stale_linear.0,
        stale_linear.1,
    );
    let fresh = (0.0001, 0.0005); // a totally different (e.g. altcoin) fresh fit
    assert_eq!(resolve_ty(true, stale_in_log_space, fresh), fresh);
}

#[test]
fn resolve_ty_non_seed_non_reset_preserves_prev_bit_exact() {
    let prev = (12.345, 67.891);
    let fresh = (0.0, 1.0);
    let (p0, p1) = resolve_ty(false, prev, fresh);
    // bit-exact, not just approximately equal — "the write must carry
    // (ty0, ty1) unchanged" (task brief).
    assert_eq!(p0.to_bits(), prev.0.to_bits());
    assert_eq!(p1.to_bits(), prev.1.to_bits());
}

#[test]
fn follow_shift_x_only_leaves_resolve_ty_prev_untouched() {
    // Behavior 3: follow-live's x-shift (bars appended, view pinned to
    // the live edge) is an x-only interaction — resolve_ty's non-seed/
    // non-reset branch must return the SAME (ty0, ty1) chart.rs read at
    // frame-start, bit-exact, regardless of how far follow_shift moves
    // (cx0, cx1).
    let (cx0, cx1) = follow_shift(10.0, 60.0, 99.0); // width-preserving x pin
    assert_ne!((cx0, cx1), (10.0, 60.0)); // sanity: x really did move
    let manual_ty = (1_234.5, 1_250.0);
    let fresh = (0.0, 1.0); // must be ignored entirely on this path
    let (ty0, ty1) = resolve_ty(false, manual_ty, fresh);
    assert_eq!((ty0.to_bits(), ty1.to_bits()), (manual_ty.0.to_bits(), manual_ty.1.to_bits()));
}

#[test]
fn resolve_ty_composes_with_queued_drag_using_fresh_never_stale() {
    // Edge case: a seed AND a queued gutter-drag delta landing on the
    // same frame (chart.rs consumes `pending_y_drag` AFTER this
    // resolution, see chart.rs ~702-706). The drag must zoom the FRESH
    // seeded range, never the stale prev — proven here by checking the
    // composed result only ever depends on `fresh`.
    let prev = (999_000.0, 999_999.0); // wildly stale, must be fully ignored
    let fresh = (10.0, 20.0);
    let dy = 40.0_f32;
    let (ty0, ty1) = resolve_ty(true, prev, fresh);
    let composed = y_drag_zoom(ty0, ty1, dy);
    let direct = y_drag_zoom(fresh.0, fresh.1, dy);
    assert_eq!(composed, direct);
}
