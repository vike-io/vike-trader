//! What the slice picker SHOWS when the store could not be enumerated — asserted on the real
//! rendered widget tree, because this defect only ever existed in what a human sees.
//!
//! `SlicePicker::refresh` used to `unwrap_or_default()` both series lists, so a store that
//! REFUSED and a store that is genuinely EMPTY produced identical UI: an empty combo reading
//! *"no data in store"*, and a central panel telling the operator to go and backfill history. The
//! store they should have been looking at was a `RemoteHistStore` whose server had gone away.
//!
//! This file drives the pane the way `crates/vike-studio/tests/saved_pane_a11y.rs` drives the
//! Saved pane, and for the same reason: `SlicePicker` owns no store outside `refresh`, spawns
//! nothing and returns nothing, so the fixture below is small enough that what runs still
//! resembles the real call. A PURE assertion over `SlicePicker::error` could not have caught the
//! defect at all — that field is the FIX; the bug was that the RENDER of `None` and the render of
//! a failure were the same thing.
//!
//! ⚠ The store double refuses `list_series` and nothing else. That is the whole failure this path
//! models: `vike_studio_core::bar_series`, `vike_studio_core::tick_series` and
//! `vike_studio_core::depth_only_series` all fold that one catalog verb, which is why the picker
//! treats a refusal as one indivisible outcome rather than listing whichever part answered. It
//! lives in `crates/vike-studio/tests/common/mod.rs` now rather than in this file, because
//! `crates/vike-studio/tests/studio_shell_render.rs` drives the same refusal through the whole
//! shell; the two files split the SURFACE and share the fixture. THIS file asserts what the
//! picker's TOOLBAR renders; that one asserts the central-panel placeholder
//! `crates/vike-studio/src/studio.rs`'s `empty_state` draws, which is the second surface the same
//! lie was rendered on and which nothing here can see.
//!
//! The file also covers the SECOND thing this picker used to render as nothing: an instrument the
//! store holds only as `kind=depth`, which no slice can replay — see
//! `vike_studio_core::depth_only_series` for why that is a disclosure rather than a missing arm.

mod common;

use common::{RefusingCatalogStore, REASON};
use egui_kittest::kittest::NodeT;
use egui_kittest::Harness;
use vike_data::HistStore;
use vike_model::BookUpdate;
use vike_studio::SlicePicker;

/// Render the REAL `SlicePicker::ui` over a picker already refreshed against `store`, and return
/// every piece of text in the resulting accessibility tree — i.e. what a human (or a screen
/// reader) has in front of them.
///
/// ⚠ It reads accesskit `value()` as well as `label()`, and the `value()` half is the load-bearing
/// one. A version of this helper that read `label()` alone — the spelling
/// `crates/vike-studio/tests/saved_pane_a11y.rs` uses, correctly, for BUTTONS — returned an empty
/// list for every store and made all three render assertions fail identically, including the
/// control. egui puts a `Label`'s and a `ComboBox`'s text in `value`; only an interactive widget
/// carries its text as `label`. Reading both is what keeps this from silently asserting over
/// nothing.
///
/// ⚠ The ComboBox POPUP is closed on this frame, so its rows are not in the tree. That is
/// deliberate rather than a limitation: every assertion below is about what the toolbar shows
/// WITHOUT the user opening anything, which is precisely the standard a disclosure has to meet.
fn rendered_text(store: &dyn HistStore) -> Vec<String> {
    let mut picker = SlicePicker::default();
    picker.refresh(store);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(640.0, 120.0))
        .build_ui_state(|ui, p: &mut SlicePicker| p.ui(ui), picker);
    harness.run();
    let text: Vec<String> = harness
        .root()
        .children_recursive()
        .flat_map(|n| {
            let a = n.accesskit_node();
            [a.label(), a.value()].into_iter().flatten().map(|t| t.to_string()).collect::<Vec<_>>()
        })
        .collect();
    // The floor: `ui()` unconditionally draws the "Data" caption, so an empty harvest means the
    // helper stopped seeing the pane rather than the pane stopping saying something. Every
    // assertion in this file is a `contains` over this list, so without this guard a harvest that
    // silently broke would turn each negative assertion green and each positive one into an
    // identical-looking failure — which is exactly how the `label()`-only version presented.
    assert!(text.iter().any(|t| t == "Data"), "the harness saw no pane at all: {text:?}");
    text
}

/// **The defect, at the surface it existed on.** A refused enumeration must not render as an empty
/// store: the toolbar says the scan FAILED, and the empty-store copy — which sends the operator to
/// a backfill they do not need — must be nowhere on screen.
#[test]
fn a_failed_series_scan_is_disclosed_in_the_toolbar_not_rendered_as_an_empty_store() {
    let labels = rendered_text(&RefusingCatalogStore);
    let all = labels.join(" | ");

    assert!(
        labels.iter().any(|l| l.contains("scan failed")),
        "a refused scan must SAY so in the toolbar; rendered: {all}"
    );
    assert!(
        !labels.iter().any(|l| l.contains("no data in store")),
        "a refused scan must not wear the empty-store costume; rendered: {all}"
    );
}

/// The control the assertion above needs in order to mean anything: a store that ANSWERED and
/// holds nothing still reads as an empty store and says nothing about a failure. Without this, a
/// picker that shouted "scan failed" unconditionally would pass the test above.
#[test]
fn an_empty_store_that_answered_still_reads_as_empty_and_not_as_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let store = vike_data::DataFusionHist::open(dir.path()).unwrap();
    let labels = rendered_text(&store);
    let all = labels.join(" | ");

    assert!(
        labels.iter().any(|l| l.contains("no data in store")),
        "an empty store keeps its own copy; rendered: {all}"
    );
    assert!(
        !labels.iter().any(|l| l.contains("scan failed")),
        "nothing failed here — the disclosure must not fire; rendered: {all}"
    );
}

/// The state behind the render: the STORE's own words are what get carried, not a message the
/// picker invented, and the list is cleared rather than left stale.
#[test]
fn a_failed_series_scan_is_recorded_as_a_reason_not_an_empty_list() {
    let mut picker = SlicePicker::default();
    picker.refresh(&RefusingCatalogStore);

    let err = picker.error().expect("a refused scan is an error, never an empty list");
    assert!(err.contains(REASON), "the store's own reason reaches the operator: {err}");
    assert!(picker.available().is_empty());
    assert!(picker.selected().is_none(), "nothing can be picked out of a list that never loaded");
}

/// A store that answers records NO error — so `error()` cannot be read as "was there a problem at
/// some point", and the ⟳ Refresh that works retires a previous failure's disclosure.
#[test]
fn a_successful_refresh_clears_a_previous_failure() {
    let dir = tempfile::tempdir().unwrap();
    let store = vike_data::DataFusionHist::open(dir.path()).unwrap();
    let mut picker = SlicePicker::default();

    picker.refresh(&RefusingCatalogStore);
    assert!(picker.error().is_some());
    picker.refresh(&store);
    assert_eq!(picker.error(), None, "the ⟳ Refresh that worked must retire the disclosure");
}

/// **The second defect, at the same surface.** An instrument recorded through `subscribe_depth`
/// alone is a real series on disk that no slice can replay. It appeared in NO list, so the picker
/// rendered it exactly as it renders an instrument that was never recorded — and told the operator
/// their store was empty while `kind=depth` parts sat in it.
#[test]
fn a_depth_only_instrument_is_named_on_screen_instead_of_vanishing() {
    let dir = tempfile::tempdir().unwrap();
    let store = vike_data::DataFusionHist::open(dir.path()).unwrap();
    store
        .append_depth(
            "binance",
            "BTCUSDT",
            &[BookUpdate {
                ts: 1,
                local_ts: 1,
                seq: 0,
                kind: vike_model::BookUpdateKind::Snapshot,
                tick_size: 0.01,
                bids: vec![(100.0, 1.0)],
                asks: vec![(100.5, 1.0)],
                symbol: "BTCUSDT".to_string(),
            }],
            None,
        )
        .unwrap();

    let labels = rendered_text(&store);
    let all = labels.join(" | ");

    assert!(
        labels.iter().any(|l| l.contains("depth-only")),
        "an unreplayable-but-recorded instrument must be visible; rendered: {all}"
    );
    assert!(
        !labels.iter().any(|l| l.contains("no data in store")),
        "the store is NOT empty — that copy sends the operator to a backfill they do not need; \
         rendered: {all}"
    );
}
