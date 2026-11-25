use super::*;
use std::assert_matches;
use vike_chart::chart::pane::{ChartPaneKey, ChartPaneMoveTarget};

// helper: a WindowState with N oscillator studies added in order, returning their uids
fn win_with_oscillators(n: usize) -> (WindowState, Vec<u64>) {
    let mut w = WindowState::default();
    let mut uids = Vec::new();
    for i in 0..n {
        // add a known oscillator-kind indicator (RSI is Oscillator); use the same path add_indicator uses
        let uid = w.add_test_oscillator(&format!("rsi_{i}"));
        uids.push(uid);
    }
    (w, uids)
}

#[test]
fn each_oscillator_gets_its_own_pane_in_add_order() {
    let (w, uids) = win_with_oscillators(3);
    assert_eq!(w.present_study_panes().len(), 3);
    // each uid maps to a distinct pane, ordered as added
    let keys: Vec<ChartPaneKey> = uids.iter().map(|u| w.study_pane[u]).collect();
    assert_eq!(keys, w.present_study_panes());
    assert!(keys.iter().all(|k| matches!(k, ChartPaneKey::Study(_))));
}

#[test]
fn add_indicator_to_auto_matches_add_indicator() {
    use vike_chart::chart::pane::ChartPaneAddTarget;
    // Part (a) byte-identical guarantee: adding with `Auto` produces the exact
    // same pane arrangement as the plain `add_indicator` path.
    let mut a = WindowState::default();
    a.add_indicator("rsi", &[]);
    a.add_indicator("macd", &[]);
    let mut b = WindowState::default();
    b.add_indicator_to("rsi", &[], ChartPaneAddTarget::Auto);
    b.add_indicator_to("macd", &[], ChartPaneAddTarget::Auto);
    assert_eq!(a.present_study_panes(), b.present_study_panes());
    assert_eq!(a.study_pane.len(), b.study_pane.len());
    assert_eq!(a.indicators.len(), b.indicators.len());
}

#[test]
fn add_indicator_to_existing_merges_into_that_pane() {
    use vike_chart::chart::pane::{ChartPaneAddTarget, ChartPaneKey};
    let mut w = WindowState::default();
    w.add_indicator("rsi", &[]);
    let first_uid = w.indicators[0].uid;
    let target = w.study_pane[&first_uid];
    // Add a SECOND oscillator straight into the first one's pane.
    w.add_indicator_to("macd", &[], ChartPaneAddTarget::Existing(target));
    let second_uid = w.indicators[1].uid;
    assert_eq!(w.study_pane[&second_uid], target, "second study merged into the target pane");
    assert_eq!(w.present_study_panes(), vec![target], "still one pane (the fresh one was dropped)");
    assert_matches!(target, ChartPaneKey::Study(_));
}

#[test]
fn add_indicator_to_overlay_is_never_placed_in_a_study_pane() {
    use vike_chart::chart::pane::{ChartPaneAddTarget, ChartPaneKey};
    // An OVERLAY (SMA) is restricted to the price pane: even asking for a study
    // pane leaves it off `study_pane`/`pane_order` entirely.
    let mut w = WindowState::default();
    w.add_indicator("rsi", &[]); // one real study pane exists
    let osc_pane = w.study_pane[&w.indicators[0].uid];
    w.add_indicator_to("sma", &[], ChartPaneAddTarget::Existing(osc_pane));
    let sma_uid = w.indicators[1].uid;
    assert!(w.indicators[1].is_overlay(), "sma must be overlay-kind (registry drift?)");
    assert!(!w.study_pane.contains_key(&sma_uid), "overlay never enters study_pane");
    assert_eq!(w.present_study_panes(), vec![osc_pane], "overlay add didn't touch panes");
    assert_matches!(osc_pane, ChartPaneKey::Study(_));
}

#[test]
fn move_into_merges_two_studies_into_one_pane() {
    let (mut w, uids) = win_with_oscillators(2);
    let target = w.study_pane[&uids[0]];
    w.move_study(uids[1], ChartPaneMoveTarget::Into(target));
    assert_eq!(w.study_pane[&uids[0]], target);
    assert_eq!(w.study_pane[&uids[1]], target);
    assert_eq!(w.present_study_panes(), vec![target]); // one pane now, the emptied one dropped
}

#[test]
fn move_into_a_stale_pane_key_is_ignored_not_orphaning() {
    // Review Important: Into() must not point a study at a dropped/foreign pane.
    let (mut w, uids) = win_with_oscillators(2);
    let p0 = w.study_pane[&uids[0]];
    let p1_stale = w.study_pane[&uids[1]];
    w.move_study(uids[1], ChartPaneMoveTarget::Into(p0)); // merges; p1_stale is now dropped
    assert!(!w.present_study_panes().contains(&p1_stale));
    // moving uid[0] into the now-stale key must be a no-op, NOT orphan uid[0]
    w.move_study(uids[0], ChartPaneMoveTarget::Into(p1_stale));
    assert_eq!(w.study_pane[&uids[0]], p0); // still in a LIVE pane
    // every tracked study still resolves to a present pane (nothing rendered in zero panes)
    let present = w.present_study_panes();
    assert!(w.study_pane.values().all(|k| present.contains(k)));
}

#[test]
fn move_new_below_creates_and_orders_a_fresh_pane() {
    let (mut w, uids) = win_with_oscillators(2);
    let anchor = w.study_pane[&uids[0]];
    let before = w.present_study_panes();
    w.move_study(uids[1], ChartPaneMoveTarget::NewBelow(anchor));
    let after = w.present_study_panes();
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], anchor); // anchor stays first
    assert_ne!(after[1], before[1]); // uid[1]'s pane is a NEW key, below anchor
    assert_eq!(w.study_pane[&uids[1]], after[1]);
}

#[test]
fn reorder_pane_swaps_adjacent_and_clamps_at_ends() {
    // Feature #1 (TradingView parity): the pane ↑/↓ controls swap a study
    // pane with its neighbor, and no-op at the group's ends / for unknowns.
    let (mut w, _uids) = win_with_oscillators(3);
    w.options.show_volume = false; // isolate the study group for this case
    let p = w.present_study_panes(); // [p0, p1, p2] in add-order
    assert_eq!(p.len(), 3);
    // middle pane UP → swaps with the first
    w.reorder_pane(p[1], true);
    assert_eq!(w.present_study_panes(), vec![p[1], p[0], p[2]]);
    // and back DOWN → original order
    w.reorder_pane(p[1], false);
    assert_eq!(w.present_study_panes(), vec![p[0], p[1], p[2]]);
    // UP at the top is a no-op
    w.reorder_pane(p[0], true);
    assert_eq!(w.present_study_panes(), vec![p[0], p[1], p[2]]);
    // DOWN at the bottom is a no-op
    w.reorder_pane(p[2], false);
    assert_eq!(w.present_study_panes(), vec![p[0], p[1], p[2]]);
    // an unknown pane is a no-op (never panics / reorders)
    w.reorder_pane(ChartPaneKey::Study(9999), true);
    assert_eq!(w.present_study_panes(), vec![p[0], p[1], p[2]]);
}

#[test]
fn reorder_pane_treats_volume_cvd_and_studies_as_peers() {
    // Chart single-max default: Volume and CVD are reorderable peers of study
    // panes. Turn both on, sync them into the authored order (Volume front,
    // CVD after), then reorder across the whole unified group.
    let (mut w, _uids) = win_with_oscillators(1);
    w.options.show_volume = true;
    w.cumulative_volume_delta_on = true;
    w.sync_sub_panes();
    let study = w.present_study_panes()[0];
    // Default authored order: Volume, Cvd, then the study pane.
    assert_eq!(
        w.present_sub_panes(),
        vec![ChartPaneKey::Volume, ChartPaneKey::CumulativeVolumeDelta, study]
    );
    // Move the study pane UP twice → it climbs above CVD, then above Volume.
    w.reorder_pane(study, true);
    assert_eq!(
        w.present_sub_panes(),
        vec![ChartPaneKey::Volume, study, ChartPaneKey::CumulativeVolumeDelta]
    );
    w.reorder_pane(study, true);
    assert_eq!(
        w.present_sub_panes(),
        vec![study, ChartPaneKey::Volume, ChartPaneKey::CumulativeVolumeDelta]
    );
    // UP again at the top is a no-op.
    w.reorder_pane(study, true);
    assert_eq!(
        w.present_sub_panes(),
        vec![study, ChartPaneKey::Volume, ChartPaneKey::CumulativeVolumeDelta]
    );
    // Move Volume DOWN → swaps with CVD (the bottom-most now).
    w.reorder_pane(ChartPaneKey::Volume, false);
    assert_eq!(
        w.present_sub_panes(),
        vec![study, ChartPaneKey::CumulativeVolumeDelta, ChartPaneKey::Volume]
    );
    // DOWN at the bottom (Volume) is a no-op.
    w.reorder_pane(ChartPaneKey::Volume, false);
    assert_eq!(
        w.present_sub_panes(),
        vec![study, ChartPaneKey::CumulativeVolumeDelta, ChartPaneKey::Volume]
    );
}

#[test]
fn sync_sub_panes_adds_and_removes_volume_cvd() {
    // Toggling show_volume / cumulative_volume_delta_on reconciles their membership in the
    // authored order (added at the default slot, removed cleanly).
    let mut w = WindowState::default();
    w.options.show_volume = false;
    w.cumulative_volume_delta_on = false;
    w.sync_sub_panes();
    assert!(w.present_sub_panes().is_empty());
    // Volume on → front.
    w.options.show_volume = true;
    w.sync_sub_panes();
    assert_eq!(w.present_sub_panes(), vec![ChartPaneKey::Volume]);
    // CVD on → directly after Volume.
    w.cumulative_volume_delta_on = true;
    w.sync_sub_panes();
    assert_eq!(
        w.present_sub_panes(),
        vec![ChartPaneKey::Volume, ChartPaneKey::CumulativeVolumeDelta]
    );
    // Volume off → removed, CVD stays.
    w.options.show_volume = false;
    w.sync_sub_panes();
    assert_eq!(w.present_sub_panes(), vec![ChartPaneKey::CumulativeVolumeDelta]);
    // idempotent — a second sync with no toggle change is a no-op.
    w.sync_sub_panes();
    assert_eq!(w.present_sub_panes(), vec![ChartPaneKey::CumulativeVolumeDelta]);
}

#[test]
fn removing_the_last_study_in_a_pane_drops_the_pane() {
    let (mut w, uids) = win_with_oscillators(2);
    w.remove_indicator(uids[1]);
    assert_eq!(w.present_study_panes().len(), 1);
    assert!(!w.study_pane.contains_key(&uids[1]));
}
