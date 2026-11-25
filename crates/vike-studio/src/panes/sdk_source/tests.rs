use super::*;
use egui_kittest::kittest::Queryable as _;

fn sdk(number: Option<u32>, usable: bool) -> WireSdk {
    WireSdk {
        name: number.map_or_else(|| "sdk-dev".to_string(), |n| format!("sdk-{n}")),
        number,
        commit: "a3ca741107".to_string(),
        abi_version: Some(14),
        usable,
        why_unusable: if usable { String::new() } else { "SDK 1 is retired".to_string() },
    }
}

/// A server holding SDK 3 (usable) and SDK 1 (retired).
fn server() -> WireSdkListing {
    WireSdkListing {
        root: "/srv/vike/vendor/vike-strategy-source".to_string(),
        sdks: vec![sdk(Some(3), true), sdk(Some(1), false)],
    }
}

fn folder(sdk: &str, path: &str, entries: &[(&str, bool)]) -> WireSdkFile {
    WireSdkFile {
        sdk: sdk.to_string(),
        path: path.to_string(),
        folder: true,
        text: String::new(),
        entries: entries
            .iter()
            .map(|(name, folder)| WireSdkEntry { name: name.to_string(), folder: *folder })
            .collect(),
    }
}

/// A strategy file whose header pins `sdk`.
fn pinned(sdk: u32) -> String {
    format!(
        "//! A strategy.\n//!\n//! ```toml\n//! [strategy]\n//! sdk = {sdk}\n//! ```\npub fn build() {{}}\n"
    )
}

/// The pin is read through the one header parser: a pinned file names its SDK; an unpinned one, a Rhai
/// buffer and a header that does not parse name none.
#[test]
fn the_pin_is_the_headers_sdk() {
    assert_eq!(strategy_pin(&pinned(3)), Some(3));
    assert_eq!(strategy_pin("//! ```toml\n//! [strategy]\n//! ```\n"), None);
    assert_eq!(strategy_pin("fn on_bar(strategy) {}\n"), None);
    assert_eq!(strategy_pin("//! ```toml\n//! [strategy]\n//! sdk = \"three\"\n//! ```\n"), None);
}

/// The toolbar's closed SDK pick names the SDK by its folder and says where the choice came from: the
/// pick, the file's pin, the newest (a file pinning none), or the file it cannot read here.
#[test]
fn the_sdk_pick_names_the_sdk_and_where_the_choice_came_from() {
    let listed = server();
    assert_eq!(sdk_pick_text(None, Some(Some(1)), None), "SDK: sdk-1 (from the file)");
    assert_eq!(sdk_pick_text(None, Some(Some(1)), Some(&listed)), "SDK: sdk-1 (from the file)");
    assert_eq!(sdk_pick_text(None, Some(None), Some(&listed)), "SDK: sdk-3 (the newest)");
    assert_eq!(sdk_pick_text(None, Some(None), None), "SDK: the newest on the server");
    assert_eq!(sdk_pick_text(None, None, Some(&listed)), "SDK: as the file says");
    assert_eq!(sdk_pick_text(Some(3), Some(Some(1)), Some(&listed)), "SDK: sdk-3 (picked)");
    let dev = WireSdkListing { root: String::new(), sdks: vec![sdk(None, true)] };
    assert_eq!(sdk_pick_text(None, Some(Some(1)), Some(&dev)), "SDK: sdk-dev (from the file)");
}

/// What the section says the strategy builds against, in each case the server can answer.
#[test]
fn the_resolution_says_which_sdk_the_strategy_builds_against() {
    let listed = server();
    assert_eq!(resolution_line(&listed, Some(1)), "Your strategy pins SDK 1: sdk-1.");
    assert_eq!(
        resolution_line(&listed, None),
        "Your strategy pins no SDK: it builds against the newest usable one, sdk-3."
    );
    assert!(resolution_line(&listed, Some(9)).contains("does not hold"));
    let dev = WireSdkListing { root: "/home/dev/vike".to_string(), sdks: vec![sdk(None, true)] };
    assert!(resolution_line(&dev, Some(3)).contains("dev checkout"));
    let empty = WireSdkListing { root: String::new(), sdks: Vec::new() };
    assert!(resolution_line(&empty, None).contains("no usable SDK"));
}

/// A listing shows the strategy's SDK; an SDK the user picked survives a new listing that still holds
/// it; a different SDK forgets the folders read, and a folder answer for an SDK no longer shown is
/// dropped.
#[test]
fn a_listing_shows_the_strategys_sdk_until_the_user_picks_another() {
    let mut pane = StudioSdkSourcePane::default();
    pane.apply_listing(Ok(server()), None);
    assert_eq!(pane.shown(), Some("sdk-3"));
    pane.apply_folder("sdk-3", "crates".to_string(), Ok(folder("sdk-3", "crates", &[])));
    assert!(pane.folders.contains_key("crates"));

    pane.show(Some("sdk-1".to_string()), true);
    assert!(pane.folders.is_empty(), "another SDK's tree starts unread");
    pane.apply_folder("sdk-3", "crates".to_string(), Ok(folder("sdk-3", "crates", &[])));
    assert!(pane.folders.is_empty(), "an answer for the SDK no longer shown is dropped");
    pane.apply_listing(Ok(server()), None);
    assert_eq!(pane.shown(), Some("sdk-1"), "a picked SDK the server still holds stays");
    pane.apply_listing(Err("the server went away".to_string()), Some(3));
    assert_eq!(pane.shown(), None);
}

/// Render [`StudioSdkSourcePane::tree_ui`] once over `pane` with the strategy pinning `pin`, and return
/// the harness (its state is the pane and the last action).
fn render_tree(
    pane: StudioSdkSourcePane,
    pin: Option<u32>,
    reading: bool,
) -> egui_kittest::Harness<'static, (StudioSdkSourcePane, Option<StudioSdkSourceAction>)> {
    let mut h =
        egui_kittest::Harness::builder().with_size(egui::vec2(340.0, 600.0)).build_ui_state(
            move |ui, (pane, action): &mut (StudioSdkSourcePane, Option<StudioSdkSourceAction>)| {
                if let Some(asked) = pane.tree_ui(ui, pin, false, reading) {
                    *action = Some(asked);
                }
            },
            (pane, None),
        );
    // Steps, not `run`: an unread folder's spinner asks for frames forever.
    h.run_steps(2);
    h
}

/// **The tree's layout**: the resolution, the shown SDK's facts and its `crates/` entries, a folder as a
/// collapsible row and a file as a row that opens it. Clicking the file asks for it by its path under
/// the SDK; an unread `crates/` asks to be read, and nothing is asked while a folder is on its way.
#[test]
fn the_tree_shows_the_sdk_its_crates_and_opens_a_file_by_its_path() {
    let mut pane = StudioSdkSourcePane::default();
    pane.apply_listing(Ok(server()), Some(3));
    let unread = render_tree(pane, Some(3), false);
    assert_eq!(
        unread.state().1,
        Some(StudioSdkSourceAction::ReadFolder("crates".to_string())),
        "the shown SDK's crates/ is read first"
    );

    let mut pane = StudioSdkSourcePane::default();
    pane.apply_listing(Ok(server()), Some(3));
    let unread = render_tree(pane, Some(3), true);
    assert_eq!(unread.state().1, None, "one folder read at a time");

    let mut pane = StudioSdkSourcePane::default();
    pane.apply_listing(Ok(server()), Some(3));
    let crates = folder("sdk-3", "crates", &[("vike-tactics", true), ("README.md", false)]);
    pane.apply_folder("sdk-3", "crates".to_string(), Ok(crates));
    let mut h = render_tree(pane, Some(3), false);
    let text = vike_ui_theme::harness::tree_text(&h);
    for want in [
        "Your strategy pins SDK 3: sdk-3.",
        "sdk-3 · a3ca741107 · interface 14 · builds here",
        "vike-tactics",
        "README.md",
    ] {
        assert!(text.contains(want), "{want:?} must be in the tree; rendered: {text}");
    }
    assert_eq!(h.state().1, None, "a closed folder asks for nothing");
    h.get_by_label("README.md").click();
    h.run_steps(2);
    assert_eq!(h.state().1, Some(StudioSdkSourceAction::OpenFile("crates/README.md".to_string())));
}

/// An SDK that does not build says why, beside its name.
#[test]
fn an_sdk_that_does_not_build_says_why() {
    let mut pane = StudioSdkSourcePane::default();
    pane.apply_listing(Ok(server()), Some(1));
    pane.apply_folder("sdk-1", "crates".to_string(), Ok(folder("sdk-1", "crates", &[])));
    let h = render_tree(pane, Some(1), false);
    let text = vike_ui_theme::harness::tree_text(&h);
    assert!(text.contains("does not build here: SDK 1 is retired"), "rendered: {text}");
}
