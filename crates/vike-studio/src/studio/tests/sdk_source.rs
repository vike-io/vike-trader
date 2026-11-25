//! The SDK source view over the shell (decision 0296, verdict 3): the Rust pane's "SDK source" section,
//! the SDK the editor's strategy resolves to as the server's answer lands, a folder answer folded into
//! the tree, and a file in the central panel's read-only code view that "Back to results" leaves.

use super::super::*;
use super::support::{ready, seeded_store, state_new};
use egui_kittest::kittest::Queryable as _;
use vike_backtesthub_client::sdk_source::{WireSdk, WireSdkEntry};

/// A served SDK file's path, spelled in two literals: xtask's scan for another crate's `.rs` read as text
/// (`xtask/src/ci/tables/readers.rs`'s `SOURCES_READ_AS_TEXT`) would take a whole one for a read of the real
/// file, and the path-key gate (`crates/vike-ops/tests/hygiene/path_key_gate.rs`) for a key git must track.
const TACTICS_LIB: &str = concat!("crates", "/vike-tactics/src/lib.rs");

/// A server holding SDK 4 (usable) and SDK 2 (retired).
fn server() -> WireSdkListing {
    let sdk = |number: u32, usable: bool| WireSdk {
        name: format!("sdk-{number}"),
        number: Some(number),
        commit: "a3ca741107".to_string(),
        abi_version: Some(14),
        usable,
        why_unusable: if usable { String::new() } else { "retired".to_string() },
    };
    WireSdkListing {
        root: "/srv/vike/vendor/vike-strategy-source".to_string(),
        sdks: vec![sdk(4, true), sdk(2, false)],
    }
}

/// A Rust strategy in the editor pinning `pin`.
fn rust_state(pin: u32) -> (tempfile::TempDir, StudioState) {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StudioStrategySource::Rust;
    st.editor.source = format!(
        "//! A strategy.\n//!\n//! ```toml\n//! [strategy]\n//! sdk = {pin}\n//! ```\npub fn build() {{}}\n"
    );
    (dir, st)
}

/// The listing and a folder answer, delivered: the tree shows the SDK the editor's PIN names (not the
/// newest), and a folder of it is folded into that SDK's tree.
#[test]
fn the_tree_follows_the_editors_pin_and_folds_a_folder_in() {
    let (_dir, mut st) = rust_state(2);
    st.sdk_listing_rx = Some(ready(Ok(server())));
    st.poll();
    assert_eq!(st.sdk_source.shown(), Some("sdk-2"), "the pin, not the newest usable SDK");

    let crates = WireSdkFile {
        sdk: "sdk-2".to_string(),
        path: "crates".to_string(),
        folder: true,
        text: String::new(),
        entries: vec![WireSdkEntry { name: "vike-tactics".to_string(), folder: true }],
    };
    st.sdk_folder_rx = Some((("sdk-2".to_string(), "crates".to_string()), ready(Ok(crates))));
    st.poll();
    assert!(st.sdk_folder_rx.is_none(), "the answer was taken");

    // An edited pin moves the tree while nothing was picked by hand.
    st.editor.source = st.editor.source.replace("sdk = 2", "sdk = 4");
    let pin = st.strategy_sdk_pin();
    st.sdk_source.follow(pin);
    assert_eq!(st.sdk_source.shown(), Some("sdk-4"));
}

/// A file click is a read for the code view and puts the central panel on it.
#[test]
fn opening_a_file_puts_the_central_panel_on_the_code_view() {
    let (_dir, mut st) = rust_state(4);
    st.sdk_listing_rx = Some(ready(Ok(server())));
    st.poll();
    st.on_sdk_source_action(StudioSdkSourceAction::OpenFile(TACTICS_LIB.to_string()));
    assert_eq!(st.center, StudioCenterView::SdkFile);
    assert!(st.sdk_file_rx.is_some(), "the file is asked of the server");
}

/// **The code view's layout**: the central panel names the SDK and the path, says it is read-only and
/// shows the file's text; "Back to results" returns to the backtest surface.
#[test]
fn the_code_view_shows_the_file_read_only_and_goes_back_to_results() {
    let (_dir, mut st) = rust_state(4);
    st.center = StudioCenterView::SdkFile;
    st.sdk_source.apply_file(Ok(WireSdkFile {
        sdk: "sdk-4".to_string(),
        path: TACTICS_LIB.to_string(),
        folder: false,
        text: "//! Tactics.\npub fn rule_defaults() {}\n".to_string(),
        entries: Vec::new(),
    }));
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1440.0, 960.0))
        .with_max_steps(64)
        .build_ui_state(
            |ui, st: &mut StudioState| {
                // The shell draws icons; their family is bound only by the app's type.
                if vike_ui_theme::harness::type_ready(ui.ctx()) {
                    st.ui(ui);
                }
            },
            st,
        );
    h.run_steps(3);
    let text = vike_ui_theme::harness::tree_text(&h);
    for want in [
        "SDK source · sdk-4 · crates/vike-tactics/src/lib.rs",
        "read-only",
        "pub fn rule_defaults() {}",
    ] {
        assert!(text.contains(want), "{want:?} must be on the code view; rendered: {text}");
    }
    h.get_by_label("Back to results").click();
    h.run_steps(3);
    assert_eq!(h.state().center, StudioCenterView::Backtest);
}

/// **The toolbar's SDK pick is a dropdown of the server's SDKs** (decision 0296), the list the SDK source
/// view reads: "As the file says" by default, each SDK by its number, one that does not build here with why
/// (and not pickable: `crate::panes::sdk_source::sdk_pick_label`), and a pick reaches the spec a Run sends.
#[test]
fn the_toolbar_sdk_pick_offers_the_servers_sdks_and_a_pick_reaches_the_run() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.sdk_listing_rx = Some(ready(Ok(server())));
    st.poll();
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1440.0, 960.0))
        .with_max_steps(64)
        .build_ui_state(
            |ui, st: &mut StudioState| {
                if vike_ui_theme::harness::type_ready(ui.ctx()) {
                    st.ui(ui);
                }
            },
            st,
        );
    h.run_steps(3);
    // A combo box's accessible name is empty and its VALUE is the text it shows: a server file run by its
    // path, whose header the editor does not hold, builds as the file says.
    h.get(
        egui_kittest::kittest::By::new()
            .role(egui::accesskit::Role::ComboBox)
            .value("SDK: as the file says"),
    )
    .click();
    h.run_steps(3);
    let text = vike_ui_theme::harness::tree_text(&h);
    for want in ["As the file says", "SDK 4", "SDK 2: does not build here: retired"] {
        assert!(text.contains(want), "{want:?} must be in the dropdown; rendered: {text}");
    }
    h.get_by_label("SDK 4").click();
    h.run_steps(3);
    assert_eq!(h.state().sdk_pick, Some(4));
    assert!(matches!(h.state().current_spec(), StrategySpec::File { sdk: Some(4), .. }));
}

/// The dropdown's words for each listed SDK: its number, why one does not build here, and `sdk-dev` said
/// to build every file against the live tree.
#[test]
fn the_sdk_pick_labels_say_the_number_and_why_one_does_not_build() {
    use crate::panes::sdk_source::sdk_pick_label;
    let listed = server();
    assert_eq!(sdk_pick_label(&listed.sdks[0]), "SDK 4");
    assert_eq!(sdk_pick_label(&listed.sdks[1]), "SDK 2: does not build here: retired");
    let dev = WireSdk { name: "sdk-dev".to_string(), number: None, ..listed.sdks[0].clone() };
    assert_eq!(
        sdk_pick_label(&dev),
        "sdk-dev: a dev checkout builds every file against its live tree"
    );
}
