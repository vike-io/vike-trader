use super::*;

const MIN: f32 = 44.0;

#[test]
fn pre_rename_osc_tag_still_deserializes_as_study() {
    // Backward file-compat: workspaces saved before the C1 Osc->Study rename
    // store the variant's externally-tagged key as "Osc". The serde alias must
    // still accept it (else the whole saved layout silently resets on load).
    let v: PaneKey = serde_json::from_str(r#"{"Osc":7}"#).unwrap();
    assert_eq!(v, PaneKey::Study(7));
    // and a full (PaneKey, f32) pane entry, as actually stored in WinSnap::panes
    let (k, frac): (PaneKey, f32) = serde_json::from_str(r#"[{"Osc":3},0.16]"#).unwrap();
    assert_eq!(k, PaneKey::Study(3));
    approx(frac, 0.16);
    // serialization always emits the new tag going forward
    assert_eq!(serde_json::to_string(&PaneKey::Study(7)).unwrap(), r#"{"Study":7}"#);
}

#[test]
fn resolve_add_target_default_is_byte_identical() {
    use crate::indicators::RenderKind;
    // `Auto` never relocates, whatever the kind — the add-with-default guarantee.
    assert_eq!(resolve_add_target(RenderKind::Overlay, PaneTarget::Auto), None);
    assert_eq!(resolve_add_target(RenderKind::Oscillator, PaneTarget::Auto), None);
}

#[test]
fn resolve_add_target_overlay_never_relocates() {
    use crate::indicators::RenderKind;
    // An overlay is restricted to the price pane: EVERY target is a no-op.
    for t in [
        PaneTarget::Auto,
        PaneTarget::Price,
        PaneTarget::NewPane,
        PaneTarget::Existing(PaneKey::Study(3)),
    ] {
        assert_eq!(resolve_add_target(RenderKind::Overlay, t), None, "overlay+{t:?}");
    }
}

#[test]
fn resolve_add_target_oscillator_mapping() {
    use crate::indicators::RenderKind;
    // NewPane / Price both resolve to the fresh pane `add_indicator` already made.
    assert_eq!(resolve_add_target(RenderKind::Oscillator, PaneTarget::NewPane), None);
    assert_eq!(resolve_add_target(RenderKind::Oscillator, PaneTarget::Price), None);
    // Only Existing yields a real merge into that pane.
    assert_eq!(
        resolve_add_target(RenderKind::Oscillator, PaneTarget::Existing(PaneKey::Study(2))),
        Some(MoveTarget::Into(PaneKey::Study(2))),
    );
}

#[test]
fn present_panes_prepends_price_then_sub_then_series() {
    use PaneKey::*;
    // Price is always first and always present, whatever the sub-panes are.
    assert_eq!(present_panes(&[], &[]), vec![Price]);
    assert_eq!(present_panes(&[Volume], &[]), vec![Price, Volume]);
    assert_eq!(present_panes(&[Cvd], &[]), vec![Price, Cvd]);
    assert_eq!(present_panes(&[Volume, Cvd], &[]), vec![Price, Volume, Cvd]);
    // Chart single-max default: Volume/CVD/Study are PEERS — the unified
    // sub-order is emitted VERBATIM (no forced Volume→Cvd→studies sequence),
    // so any user arrangement survives to the render.
    let sub = [Study(3), Volume, Study(1), Cvd, Study(2)];
    assert_eq!(present_panes(&sub, &[]), vec![Price, Study(3), Volume, Study(1), Cvd, Study(2)],);
    // C2b: authored series panes follow verbatim, AFTER the unified sub-panes.
    let series = [Series(5), Series(2)];
    assert_eq!(
        present_panes(&[Volume, Cvd, Study(3)], &series),
        vec![Price, Volume, Cvd, Study(3), Series(5), Series(2)],
    );
    // Series panes alone (no sub-panes): Price then series directly.
    assert_eq!(present_panes(&[], &series), vec![Price, Series(5), Series(2)]);
}

fn approx(a: f32, b: f32) {
    assert!((a - b).abs() < 0.01, "{a} !~= {b}");
}

#[test]
fn layout_normalizes_to_avail_at_defaults() {
    let mut pf = PaneFractions::default();
    let present = [PaneKey::Price, PaneKey::Volume, PaneKey::Study(1)];
    let heights = pf.layout(&present, 600.0, MIN);
    assert_eq!(heights.len(), 3);
    approx(heights.iter().sum(), 600.0);
    // price is the clear majority share
    assert!(heights[0] > heights[1]);
    assert!(heights[0] > heights[2]);
}

#[test]
fn new_osc_gets_default_share_existing_scale_proportionally() {
    let mut pf = PaneFractions::default();
    let base = [PaneKey::Price, PaneKey::Volume];
    let before = pf.layout(&base, 600.0, MIN);
    let ratio_before = before[0] / before[1];

    let with_new = [PaneKey::Price, PaneKey::Volume, PaneKey::Study(7)];
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
    let mut pf = PaneFractions::default();
    let with_vol = [PaneKey::Price, PaneKey::Volume];
    let shown = pf.layout(&with_vol, 600.0, MIN);
    let vol_frac_before = shown[1] / 600.0;

    // Volume toggled off: layout with just Price present.
    let price_only = [PaneKey::Price];
    let hidden = pf.layout(&price_only, 600.0, MIN);
    assert_eq!(hidden.len(), 1);
    approx(hidden[0], 600.0); // price alone takes the whole budget

    // Volume toggled back on: same fraction as before it was hidden.
    let reshown = pf.layout(&with_vol, 600.0, MIN);
    approx(reshown[1] / 600.0, vol_frac_before);
    approx(reshown[0], shown[0]); // price's share is unaffected by the round-trip
}

#[test]
fn drag_moves_boundary_by_delta_and_respects_min() {
    let mut pf = PaneFractions::default();
    let present = [PaneKey::Price, PaneKey::Volume];
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
    let present = [PaneKey::Price, PaneKey::Volume, PaneKey::Study(1)];

    // separator 0 (Price↔Volume): Study(1) must not move.
    let mut pf = PaneFractions::default();
    let before = pf.layout(&present, 600.0, MIN);
    pf.drag(&present, 0, 20.0, 600.0, MIN);
    let after = pf.layout(&present, 600.0, MIN);
    approx(after[0], before[0] + 20.0); // Price grew by exactly +20
    approx(after[1], before[1] - 20.0); // Volume shrank by exactly -20
    approx(after[2], before[2]); // Study(1): non-adjacent, invariant
    approx(after.iter().sum(), 600.0);

    // separator 1 (Volume↔Study): Price must not move.
    let mut pf = PaneFractions::default();
    let before = pf.layout(&present, 600.0, MIN);
    pf.drag(&present, 1, 20.0, 600.0, MIN);
    let after = pf.layout(&present, 600.0, MIN);
    approx(after[0], before[0]); // Price: non-adjacent, invariant
    approx(after[1], before[1] + 20.0); // Volume grew by exactly +20
    approx(after[2], before[2] - 20.0); // Study shrank by exactly -20
    approx(after.iter().sum(), 600.0);

    // over-drag on separator 1 still clamps Study at min, and STILL leaves
    // the non-adjacent Price untouched.
    let mut pf = PaneFractions::default();
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
    let mut pf = PaneFractions::default();
    let present = [PaneKey::Price];
    pf.drag(&present, 0, 50.0, 600.0, MIN); // single pane: no boundary 0
    assert!(pf.0.is_empty());

    let present2 = [PaneKey::Price, PaneKey::Volume];
    pf.drag(&present2, 5, 50.0, 600.0, MIN); // out-of-range i
    assert!(pf.0.is_empty());
}

#[test]
fn degenerate_avail_clamps_sanely() {
    let mut pf = PaneFractions::default();
    let present = [PaneKey::Price, PaneKey::Volume, PaneKey::Study(1), PaneKey::Study(2)];
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
