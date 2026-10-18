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
fn y_autofit_pads_visible_extents() {
    let mut b = bars(10);
    b[3].l = 0.4; // visible min
    b[7].h = 3.0; // visible max
    let (rlo, rhi) = y_raw_ext(&b).unwrap();
    let (lo, hi) = y_pad(rlo, rhi, 5.0, 5.0);
    let pad = ((3.0 - 0.4) * 0.05_f64).max(0.01);
    assert_eq!(lo, 0.4 - pad);
    assert_eq!(hi, 3.0 + pad);
    // degenerate flat slice still gets the minimum pad
    let flat = vec![Bar { t: 0.0, ot: 0, o: 5.0, h: 5.0, l: 5.0, c: 5.0, v: 0.0 }];
    let (rlo, rhi) = y_raw_ext(&flat).unwrap();
    let (lo, hi) = y_pad(rlo, rhi, 5.0, 5.0);
    assert_eq!(lo, 5.0 - 0.01);
    assert_eq!(hi, 5.0 + 0.01);
    assert!(y_raw_ext(&[]).is_none());
}

// --- Sync seam (task B7) behavior 2: visible_ts_bounds clamp math ---

#[test]
fn visible_ts_bounds_interior_window() {
    let b = bars(10);
    // ceil(2.3) = 3, floor(6.7) = 6
    assert_eq!(visible_ts_bounds(&b, 2.3, 6.7), Some((b[3].ot, b[6].ot)));
    // exact integers pass through unchanged
    assert_eq!(visible_ts_bounds(&b, 2.0, 6.0), Some((b[2].ot, b[6].ot)));
}

#[test]
fn visible_ts_bounds_clamps_both_edges() {
    let b = bars(10);
    assert_eq!(visible_ts_bounds(&b, -50.0, 500.0), Some((b[0].ot, b[9].ot)));
}

#[test]
fn visible_ts_bounds_empty_series_is_none() {
    assert_eq!(visible_ts_bounds(&[], 0.0, 5.0), None);
}

#[test]
fn visible_ts_bounds_sub_bar_window_is_none() {
    // ceil(5.1) = 6 > floor(5.4) = 5: a window narrower than one bar's
    // spacing, straddling no whole index — i0 > i1, not an empty series.
    let b = bars(10);
    assert_eq!(visible_ts_bounds(&b, 5.1, 5.4), None);
}

#[test]
fn visible_ts_bounds_entirely_out_of_range_is_none() {
    let b = bars(10);
    assert_eq!(visible_ts_bounds(&b, 500.0, 600.0), None); // scrolled past the end
    assert_eq!(visible_ts_bounds(&b, -600.0, -500.0), None); // scrolled past the start
}

#[test]
fn orderflow_index_bounds_normal_and_clamped_windows() {
    let b = bars(10);
    assert_eq!(orderflow_index_bounds(&b, 2.3, 6.7), Some((3, 6)));
    assert_eq!(orderflow_index_bounds(&b, 2.0, 6.0), Some((2, 6)));
    assert_eq!(orderflow_index_bounds(&b, -50.0, 500.0), Some((0, 9))); // clamped to [0, last]
}

#[test]
fn orderflow_index_bounds_degenerate_windows_are_none() {
    let b = bars(10);
    assert_eq!(orderflow_index_bounds(&[], 0.0, 5.0), None); // empty series
    assert_eq!(orderflow_index_bounds(&b, 5.1, 5.4), None); // narrower than one bar's spacing
    assert_eq!(orderflow_index_bounds(&b, 500.0, 600.0), None); // scrolled past the end
    assert_eq!(orderflow_index_bounds(&b, -600.0, -500.0), None); // scrolled past the start
}

#[test]
fn orderflow_tick_size_pinned_wins_over_derivation() {
    let b = bars(10); // fixture bars are flat h=2.0/l=0.5
    assert_eq!(orderflow_tick_size(&b, 0, 9, 0.25), 0.25);
    // a caller pin short-circuits BEFORE the bounds guard, even given an invalid window.
    assert_eq!(orderflow_tick_size(&b, 3, 2, 0.5), 0.5);
}

#[test]
fn orderflow_tick_size_derives_the_nice_step_the_profile_overlay_used_to_inline() {
    let b = bars(10);
    let derived = orderflow_tick_size(&b, 0, 9, 0.0);
    assert_eq!(derived, scale::nice_step_ceil((2.0 - 0.5) / 36.0));
    assert_eq!(derived, 0.05);
}

#[test]
fn orderflow_tick_size_degenerate_inputs_yield_zero() {
    let b = bars(10);
    assert_eq!(orderflow_tick_size(&b, 3, 2, 0.0), 0.0); // i0 > i1
    assert_eq!(orderflow_tick_size(&b, 0, 20, 0.0), 0.0); // i1 out of bounds
    assert_eq!(orderflow_tick_size(&[], 0, 0, 0.0), 0.0); // empty bars
}

#[test]
fn cell_px_for_uses_transform_pixels_per_unit() {
    // 100 plot-y-units mapped onto a 400px-tall frame → 4 px/unit (sign-independent, abs'd
    // inside `cell_px_for` — `dpos_dvalue_y()` is negative because screen y grows downward
    // while plot y grows upward).
    let frame = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 400.0));
    let bounds = egui_plot::PlotBounds::from_min_max([0.0, 0.0], [10.0, 100.0]);
    let t = egui_plot::PlotTransform::new(frame, bounds, false);
    assert_eq!(cell_px_for(&t, 2.0), 8.0);
    assert_eq!(cell_px_for(&t, 0.0), 0.0);
}

// --- chart refactor PR-2 (Block C): default_bounds ---

#[test]
fn default_bounds_pads_the_series_extent_linear() {
    // Fixture bars: t = 0..9, l = 0.5, h = 2.0, c = 1.5. `owned_is_none = false`
    // takes the plain series-fold branch (no ChartState cache needed), and a
    // default `FollowLive` peeks Linear ⇒ map_pad == y_pad (Linear map is identity).
    let b = bars(10);
    let st = ChartState::default();
    let follow = FollowLive::default();
    let bnds = default_bounds(&b, false, &st, &follow, ScaleMode::Linear, false, 5.0, 5.0);
    assert!(bnds.apply);
    assert_eq!(bnds.fx_min, -1.0);
    assert_eq!(bnds.fx_max, b.last().unwrap().t + 1.0); // 9.0 + 1.0
    assert_eq!(bnds.y_lo, 0.5); // unpadded min low = the fill floor
    let (elo, ehi) = y_pad(0.5, 2.0, 5.0, 5.0);
    assert_eq!(bnds.fy_min, elo);
    assert_eq!(bnds.fy_max, ehi);
}

#[test]
fn y_pad_applies_asymmetric_top_bottom_margins() {
    // 20% top / 10% bottom of a span of 10 (lo=0, hi=10): top pad 2.0, bottom 1.0.
    let (lo, hi) = y_pad(0.0, 10.0, 20.0, 10.0);
    assert_eq!(lo, -1.0);
    assert_eq!(hi, 12.0);
    // 5.0/5.0 reproduces the former fixed 5% pad byte-for-byte.
    let (lo5, hi5) = y_pad(0.4, 3.0, 5.0, 5.0);
    let pad = ((3.0 - 0.4) * 0.05_f64).max(0.01);
    assert_eq!((lo5, hi5), (0.4 - pad, 3.0 + pad));
}

#[test]
fn default_bounds_empty_series_keeps_defaults_and_skips_apply() {
    let st = ChartState::default();
    let follow = FollowLive::default();
    let bnds = default_bounds(&[], true, &st, &follow, ScaleMode::Linear, false, 5.0, 5.0);
    assert!(!bnds.apply, "empty series ⇒ builder default-bounds chain is skipped");
    assert_eq!((bnds.fx_min, bnds.fx_max, bnds.fy_min, bnds.fy_max), (-1.0, 1.0, 0.0, 1.0));
    assert_eq!(bnds.y_lo, 0.0);
}

#[test]
fn default_bounds_uses_cached_y_ext_fast_path() {
    // owned_is_none = true AND a seeded ChartState ⇒ the `y_ext` fast-path branch
    // of `y_extents` (cached closed-prefix extent) is taken; it must agree with the
    // raw series fold for an all-closed series.
    let b = bars(12);
    let mut st = ChartState::default();
    st.bars = b.clone();
    st.closed_len = b.len();
    st.refresh_caches();
    let follow = FollowLive::default();
    let via_cache = default_bounds(&b, true, &st, &follow, ScaleMode::Linear, false, 5.0, 5.0);
    let via_fold = default_bounds(&b, false, &st, &follow, ScaleMode::Linear, false, 5.0, 5.0);
    assert_eq!(via_cache.y_lo, via_fold.y_lo);
    assert_eq!((via_cache.fy_min, via_cache.fy_max), (via_fold.fy_min, via_fold.fy_max));
    assert_eq!(via_cache.y_lo, 0.5);
}
