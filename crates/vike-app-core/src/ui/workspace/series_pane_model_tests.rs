use super::*;
use std::assert_matches;
use vike_chart::panes::PaneKey;

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
    use vike_chart::scale::ScaleAssign;
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

// C2b Task 7: a fresh window pins nothing to a secondary axis — every symbol
// resolves to the `Percent` default (the shared-% overlay behavior), which is
// what makes the default render byte-identical to C2a.
#[test]
fn fresh_window_series_scale_defaults_to_percent() {
    use vike_chart::scale::ScaleAssign;
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
