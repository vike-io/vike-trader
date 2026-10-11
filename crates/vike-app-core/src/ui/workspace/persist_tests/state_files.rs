//! The state directory and the files under it — relocation, disk round trips, named layouts.

use super::*;

/// ⚠⚠ **THE MIGRATION QUESTION, ANSWERED OUT LOUD.** When the root's declaration moves the
/// family, an operator who was relying on the working-directory walk now reads a DIFFERENT
/// `workspace.json`, `layouts/`, `last_layout.txt` and `backends.json` — a saved layout and a
/// whole backend registry appearing to vanish. Nothing here copies, moves or deletes a file
/// the operator wrote, so the ONLY thing standing between them and "my backends are gone" is
/// this sentence. Reddens on a relocation that goes quiet.
///
/// Pure inputs — planted directories, no declaration, no environment — so this runs beside the
/// process-wide declaration sequence in
/// `crates/vike-app-core/tests/workspace_state_dir_declaration.rs` without racing it.
#[test]
fn a_relocation_that_strands_a_family_member_says_so_and_names_both_directories() {
    let root = std::env::temp_dir().join(format!("vike-reloc-{}", std::process::id()));
    let old = root.join("old");
    let new = root.join("new");
    std::fs::create_dir_all(&old).expect("plant the old state dir");

    // Nothing at the old location: a move that strands nothing needs no warning.
    assert_eq!(
        relocation_notice(Some(&old), Some(&new)),
        None,
        "an empty old directory loses the operator nothing"
    );

    // …now plant a registry there. THIS is the measured failure's own file.
    std::fs::write(old.join(crate::backend::backend_registry::BACKENDS_FILE), "{}")
        .expect("plant a backends.json");
    let notice =
        relocation_notice(Some(&old), Some(&new)).expect("a stranded family member is reported");
    assert!(notice.contains(&old.display().to_string()), "names the OLD directory: {notice}");
    assert!(notice.contains(&new.display().to_string()), "names the NEW directory: {notice}");
    assert!(
        notice.contains(crate::backend::backend_registry::BACKENDS_FILE),
        "…and names the member that was left behind: {notice}"
    );
    assert!(
        notice.contains("copy them across"),
        "…and says the copy is the operator's to make — no variable keeps the old answer: {notice}"
    );
    // ⚠ The file is still there afterwards. This workspace never relocates a file the operator
    // wrote — the notice is a report, not a migration.
    assert!(
        old.join(crate::backend::backend_registry::BACKENDS_FILE).exists(),
        "reporting a relocation must never move or delete anything"
    );

    // An answer that did not change says nothing, whatever sits in the directory.
    assert_eq!(relocation_notice(Some(&old), Some(&old)), None);
    assert_eq!(relocation_notice(None, None), None);
    let _ = std::fs::remove_dir_all(&root);
}

/// Every member of the family is in the list a relocation reports on — so a member added later
/// cannot be stranded silently.
#[test]
fn the_family_member_list_holds_every_file_this_module_resolves() {
    assert!(FAMILY_MEMBERS.contains(&WORKSPACE_FILE));
    assert!(FAMILY_MEMBERS.contains(&LAYOUTS_SUBDIR));
    assert!(FAMILY_MEMBERS.contains(&LAST_LAYOUT_FILE));
    assert!(
        FAMILY_MEMBERS.contains(&crate::backend::backend_registry::BACKENDS_FILE),
        "the registry reaches `base_dir` through this module, so it moves with the family"
    );
}

#[test]
fn disk_round_trip_and_corrupt_file() {
    // ⚠ The directory is UNIQUE per run, and that is a CI-reliability property rather than a
    // tidiness one. This test used a fixed `<temp>/vike-layout-test`; CI runs as one user and
    // agents as another ON THE SAME BOX, whichever creates the directory first OWNS it, and
    // every later `create_dir_all` under the other user then returns `PermissionDenied` —
    // forever, because nothing ever cleans `/tmp`. Reproduced on the CI box by pre-creating the
    // path as the other user. `crates/vike-ops/tests/hygiene/temp_path_gate.rs` is the gate.
    //
    // ⚠ The `TempDir` is BOUND, not discarded: `tempfile::tempdir().unwrap().path()` drops the
    // guard at the end of the statement and deletes the directory before the first write.
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path();
    let p = dir.join("workspace.json");
    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    save_to(&wins, DisplayTz::Local, true, &[], &p).unwrap();
    let ws = load_from(&p).expect("file written above");
    assert_eq!(ws, capture(&wins, DisplayTz::Local, true));
    // corrupt file → None, never a panic (startup must survive it)
    std::fs::write(&p, "{not json").unwrap();
    assert!(load_from(&p).is_none());
    assert!(load_from(&dir.join("absent.json")).is_none());
    // No `remove_dir_all`: dropping `tmp` removes the tree, on the panic path too.
}

#[test]
fn indicator_favs_round_trip_and_old_file_defaults_empty() {
    // Feature part (b): favourites written by `save_to` survive a disk round-trip,
    // and a file with NO `indicator_favs` key at all (pre-feature) loads empty.
    let dir = std::env::temp_dir().join(format!("vike-favs-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("workspace.json");
    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    let favs = vec!["rsi".to_string(), "macd".to_string()];

    save_to(&wins, DisplayTz::Local, false, &favs, &p).unwrap();
    let ws = load_from(&p).expect("file written above");
    assert_eq!(ws.indicator_favs, favs, "favourites round-trip verbatim, in order");

    // An OLD file with no `indicator_favs` key still parses, defaulting to empty.
    let raw = std::fs::read_to_string(&p).unwrap();
    assert!(raw.contains("indicator_favs"));
    let stripped: serde_json::Value = {
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v.as_object_mut().unwrap().remove("indicator_favs");
        v
    };
    let back: Workspace = serde_json::from_value(stripped).unwrap();
    assert!(back.indicator_favs.is_empty(), "pre-feature file ⇒ empty favourites");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn asset_class_round_trips_and_old_file_defaults_none() {
    // Feed-routing slice 1: `WinSnap::asset_class` is the persisted twin of
    // `WinState::asset_class`. A window with a derivative asset class must survive a
    // disk round-trip, AND an old file with no `asset_class` key at all (pre-feature)
    // must still load, defaulting to `None` (spot/legacy — byte-identical behavior).
    let dir = std::env::temp_dir().join(format!("vike-asset-class-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("workspace.json");

    let mut wins = vec![chart_win("BTC-USDT-SWAP", ChartStyle::Candles)];
    wins[0].venue = "okx".into();
    wins[0].asset_class = Some(AssetClass::CryptoPerp);
    save_to(&wins, DisplayTz::Local, false, &[], &p).unwrap();

    let ws = load_from(&p).expect("file written above");
    assert_eq!(ws.wins[0].asset_class, Some(AssetClass::CryptoPerp));
    let rebuilt = apply(&ws);
    assert_eq!(rebuilt[0].asset_class, Some(AssetClass::CryptoPerp));

    // An OLD file with no `asset_class` key at all still parses, defaulting to None.
    let raw = std::fs::read_to_string(&p).unwrap();
    assert!(raw.contains("asset_class"));
    let stripped: serde_json::Value = {
        let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["wins"][0].as_object_mut().unwrap().remove("asset_class");
        v
    };
    let back: Workspace = serde_json::from_value(stripped).unwrap();
    assert_eq!(back.wins[0].asset_class, None, "pre-feature file ⇒ None (spot/legacy)");

    // An ordinary spot window (asset_class: None) never writes the key at all
    // (`skip_serializing_if`), so an existing all-spot workspace file is untouched.
    let spot = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    let spot_json = serde_json::to_string(&capture(&spot, DisplayTz::Local, false)).unwrap();
    assert!(!spot_json.contains("asset_class"));

    let _ = std::fs::remove_dir_all(&dir);
}

// --- named / multiple saved layouts -----------------------------------

/// (m) filename sanitization: safe stems pass through; path separators,
/// dots, control/exotic chars become `_`; whitespace collapses + trims;
/// empty/whitespace/all-punctuation ⇒ `""` (the "no valid name" signal); the
/// result is bounded in length and never contains a path separator.
#[test]
fn sanitize_layout_name_is_path_injection_proof() {
    assert_eq!(sanitize_layout_name("My Scalping Layout"), "My Scalping Layout");
    assert_eq!(sanitize_layout_name("  trimmed  "), "trimmed");
    assert_eq!(sanitize_layout_name("a\t b   c"), "a b c"); // whitespace runs collapse
    assert_eq!(sanitize_layout_name("keep-dash_underscore"), "keep-dash_underscore");
    // path-traversal / separators / dots are neutralized, then edge-trimmed
    assert_eq!(sanitize_layout_name("../../etc/passwd"), "etc_passwd");
    assert_eq!(sanitize_layout_name("a/b\\c"), "a_b_c");
    assert_eq!(sanitize_layout_name(".hidden."), "hidden");
    // empty / whitespace / all-punctuation collapse to the "no valid name" signal
    assert_eq!(sanitize_layout_name(""), "");
    assert_eq!(sanitize_layout_name("   "), "");
    assert_eq!(sanitize_layout_name("...///"), "");
    // never yields a path separator, and stays bounded
    let s = sanitize_layout_name(&"x/".repeat(100));
    assert!(!s.contains('/') && !s.contains('\\'));
    assert!(s.chars().count() <= 64);
    // an invalid name has no layout_path; a valid one does
    assert!(layout_path("   ").is_none());
    assert!(layout_path("Valid Name").is_some());
}

/// (n) listing + save/load/delete round-trip through a temp `layouts/` dir,
/// independent of the process's real workspace dir (uses the `*_in`/`*_to`/
/// `load_from` seams the public helpers are built on, so no env juggling).
#[test]
fn named_layout_save_list_load_delete_round_trip() {
    let dir = std::env::temp_dir().join(format!("vike-layouts-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // empty dir ⇒ no layouts
    assert!(list_layouts_in(&dir).is_empty());

    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    let expected = capture(&wins, DisplayTz::Utc, true);

    // save two named layouts (via the same schema the public save_layout uses)
    for name in ["Alpha", "beta view"] {
        let p = dir.join(format!("{}.json", sanitize_layout_name(name)));
        save_to(&wins, DisplayTz::Utc, true, &[], &p).unwrap();
    }
    // listing is sorted case-insensitively and stem-named
    assert_eq!(list_layouts_in(&dir), vec!["Alpha".to_string(), "beta view".to_string()]);

    // load one back — full-fidelity round-trip through the shared load_from seam
    let loaded = load_from(&dir.join("Alpha.json")).expect("saved above");
    assert_eq!(loaded, expected);
    let rebuilt = apply(&loaded);
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(rebuilt[0].symbol, "BTCUSDT");

    // delete one → gone from the listing, the other survives
    std::fs::remove_file(dir.join("Alpha.json")).unwrap();
    assert_eq!(list_layouts_in(&dir), vec!["beta view".to_string()]);
    // deleting an absent file is not fatal (idempotent contract)
    assert_matches!(
        std::fs::remove_file(dir.join("Alpha.json")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    // a missing layout file loads as None (never a panic)
    assert!(load_from(&dir.join("no-such.json")).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `layouts/` DIRECTORY holds every named layout, and it is the ONLY directory any of
/// them is looked for in: listing it lists all of them, and a name resolves to exactly one
/// file inside it whether or not that file exists yet.
///
/// A layout file sitting in a SIBLING directory is invisible — the property that says the
/// listing is a plain read of one directory rather than a merge of several. (The behavioural
/// twin, over the directory a second rung would actually have used, is
/// [`nothing_in_the_state_family_is_read_from_beside_the_executable`].)
///
/// Env-free by construction — driven through the same `list_layouts_in`/`load_from` seams the
/// public helpers compose, over temp directories, so no ambient `$VIKE_WORKSPACE` can move the
/// answer.
#[test]
fn the_layout_directory_is_the_only_place_a_named_layout_is_looked_for() {
    let root =
        std::env::temp_dir().join(format!("vike-lay-one-{}-{}", std::process::id(), line!()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("settings/state").join(LAYOUTS_SUBDIR);
    let sibling = root.join("elsewhere").join(LAYOUTS_SUBDIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();

    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    for name in ["Fresh", "Shared"] {
        save_to(&wins, DisplayTz::Utc, true, &[], &dir.join(format!("{name}.json"))).unwrap();
    }
    // Same stem, different directory, and a different value so a mix-up would be visible.
    for name in ["Elsewhere", "Shared"] {
        save_to(&wins, DisplayTz::Utc, false, &[], &sibling.join(format!("{name}.json"))).unwrap();
    }

    // The listing is that ONE directory: "Elsewhere" is not in it, "Shared" appears once.
    assert_eq!(
        list_layouts_in(&dir),
        vec!["Fresh".to_string(), "Shared".to_string()],
        "a layout outside the layouts directory is not a layout"
    );
    // …and the file a name resolves to is the one inside it (the GPU-on copy, not GPU-off).
    assert!(load_from(&dir.join("Shared.json")).unwrap().gpu_render);
    // A name with no file resolves to a path inside the same directory — reported, not read.
    assert!(!dir.join("Nowhere.json").exists());
    assert!(load_from(&dir.join("Nowhere.json")).is_none());

    // Deleting the one copy takes the layout out of the listing for good.
    std::fs::remove_file(dir.join("Shared.json")).unwrap();
    assert_eq!(list_layouts_in(&dir), vec!["Fresh".to_string()]);

    let _ = std::fs::remove_dir_all(&root);
}

/// `workspace.json` has ONE path, and the reader and the writer agree on it — a typo on
/// either side would silently write a file the loader never opens.
///
/// Also pinned: the write CREATES its directory on the way (a first save on a fresh checkout
/// must not fail for want of `settings/state`), and an absent file resolves to that same path
/// rather than to a second candidate.
#[test]
fn the_workspace_file_has_one_path_and_the_reader_and_the_writer_agree() {
    assert_eq!(WORKSPACE_FILE, "workspace.json");

    // The composition, over a temp directory: no real `$HOME` is touched and no ambient variable
    // can move the answer.
    let root =
        std::env::temp_dir().join(format!("vike-ws-state-{}-{}", std::process::id(), line!()));
    let _ = std::fs::remove_dir_all(&root);
    let state = root.join("state");
    assert!(!state.exists(), "precondition: nothing has been written yet");
    let written =
        vike_model::paths::state_path::write_path(Some(state.as_path()), WORKSPACE_FILE).unwrap();
    assert_eq!(written, state.join(WORKSPACE_FILE));
    assert!(state.is_dir(), "the directory is created on the way");
    assert!(!written.exists(), "…and only the directory: the file is the caller's to write");
    let _ = std::fs::remove_dir_all(&root);

    // …and this module's own two entry points land on that same single join.
    let read = path().expect("these tests run from inside the checkout");
    let write = save_path().expect("…whose state directory is creatable");
    assert_eq!(read, write, "a load and a save must never name different files");
    if std::env::var("VIKE_WORKSPACE").is_err() {
        let dir = state_dir().expect("these tests run from inside the checkout");
        assert_eq!(read, dir.join(WORKSPACE_FILE));
    }
}

/// **The state family is read from ONE directory, and it is not the executable's.**
///
/// Structural half: `layouts/` and `last_layout.txt` share ONE base with `workspace.json`, so
/// the family can never half-move, and that base is the project's `settings/state` (or, under
/// `$VIKE_WORKSPACE`, the directory that override names — one variable moves all of it).
///
/// Behavioural half, and the reason this test writes files: it plants a complete set —
/// `workspace.json`, `layouts/<name>.json`, `last_layout.txt` — in the test binary's own
/// directory, and asserts every reader ignores all three. That is the assertion a second
/// read rung anywhere in this module would fail.
#[test]
fn nothing_in_the_state_family_is_read_from_beside_the_executable() {
    let base = base_dir().expect("these tests run from inside the checkout");
    assert_eq!(layouts_dir(), Some(base.join(LAYOUTS_SUBDIR)));
    assert_eq!(last_layout_path(), Some(base.join(LAST_LAYOUT_FILE)));
    assert_eq!(layout_path("Ghost Layout"), Some(base.join(LAYOUTS_SUBDIR).join(GHOST_FILE)));

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .expect("a test binary has a directory");
    assert_ne!(base, exe_dir, "the family's base is the state directory, not the binary's own");

    // Plant the whole family exactly where it must no longer be looked for…
    let ghost_dir = exe_dir.join(LAYOUTS_SUBDIR);
    let ghost_layout = ghost_dir.join(GHOST_FILE);
    let ghost_workspace = exe_dir.join(WORKSPACE_FILE);
    let ghost_pointer = exe_dir.join(LAST_LAYOUT_FILE);
    let wins = vec![chart_win("BTCUSDT", ChartStyle::Candles)];
    std::fs::create_dir_all(&ghost_dir).unwrap();
    save_to(&wins, DisplayTz::Utc, false, &[], &ghost_layout).unwrap();
    save_to(&wins, DisplayTz::Utc, false, &[], &ghost_workspace).unwrap();
    std::fs::write(&ghost_pointer, "Ghost Layout").unwrap();

    // …ask every reader, then clean up BEFORE asserting so a failure leaves nothing behind.
    let listed = list_layouts();
    let loaded = load_layout("Ghost Layout");
    let pointed = last_layout();
    let workspace = path();
    for f in [&ghost_layout, &ghost_workspace, &ghost_pointer] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir(&ghost_dir);

    assert!(!listed.iter().any(|n| n == "Ghost Layout"), "listed: {listed:?}");
    assert!(loaded.is_none(), "a layout beside the executable is not a layout");
    assert_ne!(pointed.as_deref(), Some("Ghost Layout"), "nor is a pointer at one");
    assert_ne!(workspace, Some(ghost_workspace), "nor is a workspace file");
}

/// The layout file the test above plants, named once so its two spellings cannot drift.
const GHOST_FILE: &str = "Ghost Layout.json";
