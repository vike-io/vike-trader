//! The central panel DISCLOSES a refused scan and a depth-only store instead of rendering either as
//! an empty store: the second surface of a defect the picker's toolbar already gates.

use std::sync::Arc;

use vike_data::{DataFusionHist, HistStore};
use vike_model::{BookLevel, BookUpdate, BookUpdateKind};
use vike_studio::{DEPTH_NOT_REPLAYABLE, RightTab, SERIES_SCAN_ADVICE, StoreHandle};
use vike_ui_theme::icons;

use crate::common::{REASON, RefusingCatalogStore, empty_store};
use crate::support::{all_text, has_button, run_button, settle, shell};

/// **The second surface of the disclosure defect.** A store that REFUSED to enumerate must not
/// render as an empty one in the CENTRAL PANEL either.
///
/// `crates/vike-studio/tests/picker_disclosure.rs` covers `SlicePicker::ui` — the toolbar — and
/// cannot reach `crates/vike-studio/src/studio/center.rs`'s `empty_state`, whose own doc calls itself
/// "the second surface that rendered that lie". Swapping its two arms sends an operator whose
/// datahub went away to a backfill they do not need while leaving that entire file green.
///
/// The store's OWN reason is asserted, not just the advice: the advice is a constant this file
/// could match against a hard-coded string, the reason can only have come through the error.
#[test]
fn a_refused_series_scan_is_disclosed_by_the_shell_not_rendered_as_an_empty_store() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store: StoreHandle = Arc::new(RefusingCatalogStore);
    let mut h = shell(&store, &dir, RightTab::Sweep);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains(SERIES_SCAN_ADVICE)),
        "the central panel must say where to look; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t.contains(REASON)),
        "...and carry the STORE's own words, not a message the shell invented; rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.contains("The data store is empty")),
        "a refused scan must not wear the empty-store costume; rendered: {text:?}"
    );

    let (empty_dir, empty) = empty_store();
    let mut h = shell(&empty, &empty_dir, RightTab::Sweep);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains("The data store is empty")),
        "the control: a store that ANSWERED and holds nothing keeps its own copy; \
         rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.contains(SERIES_SCAN_ADVICE)),
        "nothing failed here — the disclosure must not fire; rendered: {text:?}"
    );
    assert!(!text.iter().any(|t| t.contains(REASON)), "no reason exists; rendered: {text:?}");
}

/// **The MIDDLE arm of the same placeholder — the one the pair above cannot reach.**
///
/// `empty_state` picks between FOUR copies in a fixed order (refused scan, depth-only store, empty
/// store, pickable slice) and the test above pins only the FIRST against the THIRD, so a swap of
/// the middle two leaves it green. That swap is the same lie in the same panel wearing the other
/// costume: an operator who DID record this instrument — as the conflating `kind=depth` lane, which
/// no slice can replay — is told their store is empty and sent to re-fetch history they already
/// hold. `crates/vike-studio/tests/picker_disclosure.rs`'s
/// `a_depth_only_instrument_is_named_on_screen_instead_of_vanishing` is the TOOLBAR half of exactly
/// this claim and, being a `SlicePicker::ui` harness, can no more see the central panel here than
/// it could see the refusal.
///
/// The isolation is the refusal's, for the same reason: the toolbar's own `⚠ depth-only` chip
/// carries `DEPTH_NOT_REPLAYABLE` in HOVER text and its dropdown rows live in a CLOSED popup, so
/// neither is in an un-hovered frame's tree — the string asserted here can only have come from
/// `empty_state`. The two negative legs are what stop it passing for the wrong reason: the
/// empty-store copy must be absent (that is the swapped arm), and no dispatch may be offered over
/// a store holding nothing runnable.
#[test]
fn a_depth_only_store_is_disclosed_by_the_shell_not_rendered_as_an_empty_store() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = DataFusionHist::open(dir.path()).expect("open hist store");
    store
        .append_depth(
            "binance",
            "BTCUSDT",
            &[BookUpdate {
                ts: 1,
                local_ts: 1,
                seq: 0,
                kind: BookUpdateKind::Snapshot,
                tick_size: 0.01,
                bids: vec![BookLevel::new(100.0, 1.0)],
                asks: vec![BookLevel::new(100.5, 1.0)],
                symbol: "BTCUSDT".to_string(),
            }],
            None,
        )
        .expect("append depth");
    let store: StoreHandle = Arc::new(store);

    let mut h = shell(&store, &dir, RightTab::Sweep);
    settle(&mut h);
    let text = all_text(&h);
    assert!(
        text.iter().any(|t| t.contains(DEPTH_NOT_REPLAYABLE)),
        "a recorded-but-unreplayable instrument must be named by the central panel, not silently \
         counted as nothing; rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.contains("The data store is empty")),
        "this store is NOT empty — that copy sends the operator to a backfill they already ran; \
         rendered: {text:?}"
    );
    assert!(
        !has_button(&h, &run_button("Run backtest")),
        "nothing here is replayable, so the getting-started dispatch must not be offered"
    );
}

/// Spec §4.2: loading, empty and unreachable are three renderings. A store that REFUSED its
/// catalog must not wear the empty store's picture — which it did for as long as `empty_state`
/// drew one Studio mark and one title above both, and only the sentence under them differed.
#[test]
fn a_refused_store_and_an_empty_store_render_differently() {
    let cloud = icons::UNREACHABLE.accessible_label("");
    let tray = icons::EMPTY.accessible_label("");

    let dir = tempfile::tempdir().expect("temp dir");
    let refusing: StoreHandle = Arc::new(RefusingCatalogStore);
    let mut h = shell(&refusing, &dir, RightTab::Sweep);
    settle(&mut h);
    let refused = all_text(&h);
    assert!(refused.contains(&cloud), "a refused store shows the unreachable mark: {refused:?}");
    assert!(!refused.contains(&tray), "...and not the empty one: {refused:?}");

    let (empty_dir, empty) = empty_store();
    let mut h = shell(&empty, &empty_dir, RightTab::Sweep);
    settle(&mut h);
    let blank = all_text(&h);
    assert!(blank.contains(&tray), "an empty store shows the empty mark: {blank:?}");
    assert!(!blank.contains(&cloud), "...and not the unreachable one: {blank:?}");
}
