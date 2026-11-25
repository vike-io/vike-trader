use super::*;

const MIN: f32 = 44.0;

#[test]
fn a_study_pane_reads_only_its_own_word() {
    // The stored word is "Study"; the pre-C1 "Osc" is no longer an alias, so it is an unknown
    // variant (a workspace that holds it is from before version 3 and is refused whole).
    let (k, frac): (ChartPaneKey, f32) = serde_json::from_str(r#"[{"Study":3},0.16]"#).unwrap();
    assert_eq!(k, ChartPaneKey::Study(3));
    approx(frac, 0.16);
    assert_eq!(serde_json::to_string(&ChartPaneKey::Study(7)).unwrap(), r#"{"Study":7}"#);
    let old = serde_json::from_str::<ChartPaneKey>(r#"{"Osc":7}"#).expect_err("Osc is not read");
    assert!(old.to_string().contains("unknown variant `Osc`"), "{old}");
}

#[test]
fn resolve_add_target_default_is_byte_identical() {
    use vike_indicators::RenderKind;
    // `Auto` never relocates, whatever the kind — the add-with-default guarantee.
    assert_eq!(resolve_add_target(RenderKind::Overlay, ChartPaneAddTarget::Auto), None);
    assert_eq!(resolve_add_target(RenderKind::Oscillator, ChartPaneAddTarget::Auto), None);
}

#[test]
fn resolve_add_target_overlay_never_relocates() {
    use vike_indicators::RenderKind;
    // An overlay is restricted to the price pane: EVERY target is a no-op.
    for t in [
        ChartPaneAddTarget::Auto,
        ChartPaneAddTarget::Price,
        ChartPaneAddTarget::NewPane,
        ChartPaneAddTarget::Existing(ChartPaneKey::Study(3)),
    ] {
        assert_eq!(resolve_add_target(RenderKind::Overlay, t), None, "overlay+{t:?}");
    }
}

#[test]
fn resolve_add_target_oscillator_mapping() {
    use vike_indicators::RenderKind;
    // NewPane / Price both resolve to the fresh pane `add_indicator` already made.
    assert_eq!(resolve_add_target(RenderKind::Oscillator, ChartPaneAddTarget::NewPane), None);
    assert_eq!(resolve_add_target(RenderKind::Oscillator, ChartPaneAddTarget::Price), None);
    // Only Existing yields a real merge into that pane.
    assert_eq!(
        resolve_add_target(
            RenderKind::Oscillator,
            ChartPaneAddTarget::Existing(ChartPaneKey::Study(2))
        ),
        Some(ChartPaneMoveTarget::Into(ChartPaneKey::Study(2))),
    );
}

#[test]
fn present_panes_prepends_price_then_sub_then_series() {
    use ChartPaneKey::*;
    // Price is always first and always present, whatever the sub-panes are.
    assert_eq!(present_panes(&[], &[]), vec![Price]);
    assert_eq!(present_panes(&[Volume], &[]), vec![Price, Volume]);
    let cvd = CumulativeVolumeDelta;
    assert_eq!(present_panes(&[cvd], &[]), vec![Price, cvd]);
    assert_eq!(present_panes(&[Volume, cvd], &[]), vec![Price, Volume, cvd]);
    // Chart single-max default: Volume/CVD/Study are PEERS — the unified
    // sub-order is emitted VERBATIM (no forced Volume→Cvd→studies sequence),
    // so any user arrangement survives to the render.
    let sub = [Study(3), Volume, Study(1), cvd, Study(2)];
    assert_eq!(present_panes(&sub, &[]), vec![Price, Study(3), Volume, Study(1), cvd, Study(2)],);
    // C2b: authored series panes follow verbatim, AFTER the unified sub-panes.
    let series = [Series(5), Series(2)];
    assert_eq!(
        present_panes(&[Volume, cvd, Study(3)], &series),
        vec![Price, Volume, cvd, Study(3), Series(5), Series(2)],
    );
    // Series panes alone (no sub-panes): Price then series directly.
    assert_eq!(present_panes(&[], &series), vec![Price, Series(5), Series(2)]);
}

fn approx(a: f32, b: f32) {
    assert!((a - b).abs() < 0.01, "{a} !~= {b}");
}

#[test]
fn layout_normalizes_to_avail_at_defaults() {
    let mut pf = ChartPaneFractions::default();
    let present = [ChartPaneKey::Price, ChartPaneKey::Volume, ChartPaneKey::Study(1)];
    let heights = pf.layout(&present, 600.0, MIN);
    assert_eq!(heights.len(), 3);
    approx(heights.iter().sum(), 600.0);
    // price is the clear majority share
    assert!(heights[0] > heights[1]);
    assert!(heights[0] > heights[2]);
}

#[test]
fn new_osc_gets_default_share_existing_scale_proportionally() {
    let mut pf = ChartPaneFractions::default();
    let base = [ChartPaneKey::Price, ChartPaneKey::Volume];
    let before = pf.layout(&base, 600.0, MIN);
    let ratio_before = before[0] / before[1];

    let with_new = [ChartPaneKey::Price, ChartPaneKey::Volume, ChartPaneKey::Study(7)];
    let after = pf.layout(&with_new, 600.0, MIN);
    assert_eq!(after.len(), 3);
    approx(after.iter().sum(), 600.0);
    // the new osc pane got a real (non-zero, non-dominant) share
    assert!(after[2] > MIN - 1.0);
    assert!(after[2] < after[0]);
    // price:volume ratio is preserved (both shrank together to make room)
    let ratio_after = after[0] / after[1];
    approx(ratio_before, ratio_after);
    // and both shrank in absolute terms vs. the 2-pane layout
    assert!(after[0] < before[0]);
    assert!(after[1] < before[1]);
}

#[test]
fn hidden_pane_keeps_fraction_and_reslots() {
    let mut pf = ChartPaneFractions::default();
    let with_volume = [ChartPaneKey::Price, ChartPaneKey::Volume];
    let shown = pf.layout(&with_volume, 600.0, MIN);
    let volume_fraction_before = shown[1] / 600.0;

    // Volume toggled off: layout with just Price present.
    let price_only = [ChartPaneKey::Price];
    let hidden = pf.layout(&price_only, 600.0, MIN);
    assert_eq!(hidden.len(), 1);
    approx(hidden[0], 600.0); // price alone takes the whole budget

    // Volume toggled back on: same fraction as before it was hidden.
    let reshown = pf.layout(&with_volume, 600.0, MIN);
    approx(reshown[1] / 600.0, volume_fraction_before);
    approx(reshown[0], shown[0]); // price's share is unaffected by the round-trip
}

#[test]
fn drag_moves_boundary_by_delta_and_respects_min() {
    let mut pf = ChartPaneFractions::default();
    let present = [ChartPaneKey::Price, ChartPaneKey::Volume];
    let before = pf.layout(&present, 600.0, MIN);

    pf.drag(&present, 0, 20.0, 600.0, MIN);
    let after = pf.layout(&present, 600.0, MIN);
    approx(after[0], before[0] + 20.0);
    approx(after[1], before[1] - 20.0);
    approx(after.iter().sum(), 600.0);

    // an enormous drag clamps so pane 1 never crosses min
    pf.drag(&present, 0, 10_000.0, 600.0, MIN);
    let clamped = pf.layout(&present, 600.0, MIN);
    approx(clamped[1], MIN);
    assert!(clamped[0] >= MIN);
    approx(clamped[0] + clamped[1], 600.0);

    // and the opposite direction clamps pane 0
    pf.drag(&present, 0, -10_000.0, 600.0, MIN);
    let clamped2 = pf.layout(&present, 600.0, MIN);
    approx(clamped2[0], MIN);
    approx(clamped2[0] + clamped2[1], 600.0);
}

#[test]
fn drag_moves_only_the_two_adjacent_panes_in_a_3_pane_layout() {
    // T9 review fix: with 3+ panes a drag must move EXACTLY the two panes
    // adjacent to the separator; a non-adjacent pane's height is invariant.
    // (Pre-fix this leaked the delta onto the non-adjacent pane because
    // `drag` re-pinned only the pair, leaving the present-weight-sum ≠ 1.0
    // for the next layout's renormalization.)
    let present = [ChartPaneKey::Price, ChartPaneKey::Volume, ChartPaneKey::Study(1)];

    // separator 0 (Price↔Volume): Study(1) must not move.
    let mut pf = ChartPaneFractions::default();
    let before = pf.layout(&present, 600.0, MIN);
    pf.drag(&present, 0, 20.0, 600.0, MIN);
    let after = pf.layout(&present, 600.0, MIN);
    approx(after[0], before[0] + 20.0); // Price grew by exactly +20
    approx(after[1], before[1] - 20.0); // Volume shrank by exactly -20
    approx(after[2], before[2]); // Study(1): non-adjacent, invariant
    approx(after.iter().sum(), 600.0);

    // separator 1 (Volume↔Study): Price must not move.
    let mut pf = ChartPaneFractions::default();
    let before = pf.layout(&present, 600.0, MIN);
    pf.drag(&present, 1, 20.0, 600.0, MIN);
    let after = pf.layout(&present, 600.0, MIN);
    approx(after[0], before[0]); // Price: non-adjacent, invariant
    approx(after[1], before[1] + 20.0); // Volume grew by exactly +20
    approx(after[2], before[2] - 20.0); // Study shrank by exactly -20
    approx(after.iter().sum(), 600.0);

    // over-drag on separator 1 still clamps Study at min, and STILL leaves
    // the non-adjacent Price untouched.
    let mut pf = ChartPaneFractions::default();
    let before = pf.layout(&present, 600.0, MIN);
    pf.drag(&present, 1, 10_000.0, 600.0, MIN);
    let clamped = pf.layout(&present, 600.0, MIN);
    approx(clamped[2], MIN); // Study floored at min
    assert!(clamped[1] >= MIN);
    approx(clamped[0], before[0]); // Price still invariant under the over-drag
    approx(clamped.iter().sum(), 600.0);
}

#[test]
fn drag_out_of_range_is_a_no_op() {
    let mut pf = ChartPaneFractions::default();
    let present = [ChartPaneKey::Price];
    pf.drag(&present, 0, 50.0, 600.0, MIN); // single pane: no boundary 0
    assert!(pf.0.is_empty());

    let present2 = [ChartPaneKey::Price, ChartPaneKey::Volume];
    pf.drag(&present2, 5, 50.0, 600.0, MIN); // out-of-range i
    assert!(pf.0.is_empty());
}

#[test]
fn degenerate_avail_clamps_sanely() {
    let mut pf = ChartPaneFractions::default();
    let present =
        [ChartPaneKey::Price, ChartPaneKey::Volume, ChartPaneKey::Study(1), ChartPaneKey::Study(2)];
    // avail well under 4 * min — every pane floors to `min`, no NaN/negative/panic.
    let heights = pf.layout(&present, 100.0, MIN);
    assert_eq!(heights.len(), 4);
    for h in &heights {
        assert!(h.is_finite());
        approx(*h, MIN);
    }

    // zero and negative avail: still finite, non-negative, no panic.
    for bad in [0.0_f32, -50.0] {
        let heights = pf.layout(&present, bad, MIN);
        for h in &heights {
            assert!(h.is_finite());
            assert!(*h >= 0.0);
        }
    }

    // a drag in this fully-degenerate state is a safe no-op, not a panic.
    pf.drag(&present, 1, 500.0, 100.0, MIN);
    let still = pf.layout(&present, 100.0, MIN);
    for h in &still {
        approx(*h, MIN);
    }
}
