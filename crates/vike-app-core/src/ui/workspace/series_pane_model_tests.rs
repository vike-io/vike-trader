use super::*;
use std::assert_matches;
use vike_chart::chart::{MoveTarget, PaneKey};

#[test]
fn compare_symbol_defaults_to_overlay() {
    // DEFAULT: a compare symbol lives in the price-pane %-overlay, NOT its
    // own pane — unlike C1 oscillators (which assign-on-add via
    // `assign_default_pane`), there is NO assign-on-add here; see
    // `move_series_to_new_pane`'s doc.
    let mut w = WinState::default();
    w.add_compare("ETHUSDT");
    assert!(!w.series_pane.contains_key("ETHUSDT"));
    assert!(w.present_series_panes().is_empty());
}

#[test]
fn remove_compare_cascades_into_series_pane() {
    use vike_chart::chart::ScaleAssign;
    // Review Important: removing an own-paned compare symbol must not leave an
    // orphaned series_pane entry / empty pane (mirrors remove_indicator).
    let mut w = WinState::default();
    w.add_compare("ETHUSDT");
    w.move_series_to_new_pane("ETHUSDT");
    w.series_scale.insert("ETHUSDT".to_string(), ScaleAssign::Right); // Task 7 review Minor-1
    assert_eq!(w.present_series_panes().len(), 1);
    w.remove_compare("ETHUSDT");
    assert!(!w.series_pane.contains_key("ETHUSDT"));
    assert!(w.present_series_panes().is_empty());
    // Task 7 review Minor-1: the scale pin is cleared too, so re-adding ETHUSDT
    // defaults back to Percent (no resurrected Right pin).
    assert_eq!(w.series_scale_of("ETHUSDT"), ScaleAssign::Percent);
}

#[test]
fn move_series_to_new_pane_gives_it_its_own_pane() {
    let mut w = WinState::default();
    w.add_compare("ETHUSDT");
    w.move_series_to_new_pane("ETHUSDT");
    assert_eq!(w.present_series_panes().len(), 1);
    let pane = w.series_pane["ETHUSDT"];
    assert_matches!(pane, PaneKey::Series(_));
    assert_eq!(w.present_series_panes(), vec![pane]);

    // Calling it again while already in its own pane is a no-op — same
    // pane, no second allocation.
    w.move_series_to_new_pane("ETHUSDT");
    assert_eq!(w.series_pane["ETHUSDT"], pane);
    assert_eq!(w.present_series_panes(), vec![pane]);
}

#[test]
fn overlay_series_returns_it_to_the_price_pane_and_drops_the_pane() {
    let mut w = WinState::default();
    w.add_compare("ETHUSDT");
    w.move_series_to_new_pane("ETHUSDT");
    assert_eq!(w.present_series_panes().len(), 1);

    w.overlay_series("ETHUSDT");
    assert!(!w.series_pane.contains_key("ETHUSDT"));
    assert!(w.present_series_panes().is_empty(), "the vacated pane must be dropped");
}

#[test]
fn move_into_a_stale_pane_key_is_ignored_not_orphaning() {
    // Review Important (mirrors C1's identically-purposed test): `Into()`
    // must not point a series at a dropped/foreign pane.
    let mut w = WinState::default();
    w.add_compare("ETHUSDT");
    w.add_compare("SOLUSDT");
    w.move_series_to_new_pane("ETHUSDT");
    w.move_series_to_new_pane("SOLUSDT");
    let p0 = w.series_pane["ETHUSDT"];
    let p1_stale = w.series_pane["SOLUSDT"];
    w.move_series("SOLUSDT", MoveTarget::Into(p0)); // merges; p1_stale is now dropped
    assert!(!w.present_series_panes().contains(&p1_stale));
    // moving ETHUSDT into the now-stale key must be a no-op, NOT orphan it
    w.move_series("ETHUSDT", MoveTarget::Into(p1_stale));
    assert_eq!(w.series_pane["ETHUSDT"], p0); // still in a LIVE pane
    let present = w.present_series_panes();
    assert!(w.series_pane.values().all(|k| present.contains(k)));
}

#[test]
fn move_series_on_a_still_overlaid_symbol_is_a_no_op() {
    // `move_series` (reorder/merge) only operates on a symbol ALREADY in
    // its own pane — an overlaid (or unknown) symbol must not be silently
    // pulled into a pane via the reorder path (use
    // `move_series_to_new_pane` for that transition).
    let mut w = WinState::default();
    w.add_compare("ETHUSDT");
    w.move_series_to_new_pane("ETHUSDT");
    let anchor = w.series_pane["ETHUSDT"];

    w.add_compare("SOLUSDT"); // stays overlaid — never entered series_pane
    w.move_series("SOLUSDT", MoveTarget::NewBelow(anchor));
    assert!(!w.series_pane.contains_key("SOLUSDT"), "still overlaid, not pulled in");
    assert_eq!(w.present_series_panes(), vec![anchor]);
}

#[test]
fn move_new_below_creates_and_orders_a_fresh_pane() {
    let mut w = WinState::default();
    w.add_compare("ETHUSDT");
    w.add_compare("SOLUSDT");
    w.move_series_to_new_pane("ETHUSDT");
    w.move_series_to_new_pane("SOLUSDT");
    let anchor = w.series_pane["ETHUSDT"];
    let before = w.present_series_panes();
    w.move_series("SOLUSDT", MoveTarget::NewBelow(anchor));
    let after = w.present_series_panes();
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], anchor); // anchor stays first
    assert_ne!(after[1], before[1]); // SOLUSDT's pane is a NEW key, below anchor
    assert_eq!(w.series_pane["SOLUSDT"], after[1]);
}

#[test]
fn moving_the_last_symbol_out_of_a_pane_drops_the_pane() {
    let mut w = WinState::default();
    w.add_compare("ETHUSDT");
    w.add_compare("SOLUSDT");
    w.move_series_to_new_pane("ETHUSDT");
    w.move_series_to_new_pane("SOLUSDT");
    let p0 = w.series_pane["ETHUSDT"];
    w.move_series("SOLUSDT", MoveTarget::Into(p0)); // merge into one pane
    assert_eq!(w.present_series_panes(), vec![p0]);

    w.overlay_series("ETHUSDT"); // one member leaves, one remains
    assert_eq!(w.present_series_panes(), vec![p0], "pane survives while SOLUSDT is still in it");

    w.overlay_series("SOLUSDT"); // last member leaves
    assert!(w.present_series_panes().is_empty(), "pane dropped once empty");
}

// C2b Task 7: a fresh window pins nothing to a secondary axis — every symbol
// resolves to the `Percent` default (the shared-% overlay behavior), which is
// what makes the default render byte-identical to C2a.
#[test]
fn fresh_window_series_scale_defaults_to_percent() {
    use vike_chart::chart::ScaleAssign;
    let w = WinState::default();
    assert!(w.series_scale.is_empty(), "no pins on a fresh window");
    // An unpinned (indeed unknown) symbol reads back the Percent default.
    assert_eq!(w.series_scale_of("ETHUSDT"), ScaleAssign::Percent);

    // And once pinned, the read seam reflects it (the menu wiring is Task 9).
    let mut w = w;
    w.series_scale.insert("ETHUSDT".to_string(), ScaleAssign::Right);
    assert_eq!(w.series_scale_of("ETHUSDT"), ScaleAssign::Right);
    assert_eq!(w.series_scale_of("SOLUSDT"), ScaleAssign::Percent, "others stay default");
}
