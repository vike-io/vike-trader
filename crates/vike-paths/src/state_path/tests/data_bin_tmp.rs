use super::*;

// ---- the DATA directory: the same walk, the same override --------------------------------

/// `market_data/` is a SIBLING of `settings/` and of `user_data/`, all three off ONE project root.
/// The whole change rests on this: one folder to move, back up or delete.
#[test]
fn data_is_a_sibling_of_settings_and_user_data_off_one_root() {
    let scratch = Scratch::new("data-sibling");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let settings = project_settings_dir(scratch.path()).unwrap();
    let user_data = project_user_data_dir(scratch.path()).unwrap();
    let data = project_data_dir(scratch.path()).unwrap();

    assert_eq!(settings.parent(), data.parent(), "same project root as settings");
    assert_eq!(user_data.parent(), data.parent(), "same project root as user_data");
    assert!(
        !data.starts_with(&settings),
        "data ({}) must not live under settings ({}) — a tape is not a setting",
        data.display(),
        settings.display()
    );
    assert!(!data.starts_with(&user_data), "nor under user_data — a tape is not the user's work");
    assert_eq!(data.file_name().unwrap(), PROJECT_DATA_DIR);
}

/// The store sits one level DOWN, at `market_data/hist`, so `market_data/` can grow an export or a second
/// store later without a store's own `kind=`/`venue=` partition directories sitting loose in
/// the folder users are pointed at.
#[test]
fn the_hist_store_is_a_named_subdirectory_of_the_data_dir() {
    let scratch = Scratch::new("data-hist");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let data = project_data_dir(scratch.path()).unwrap();
    let hist = project_hist_store_dir(scratch.path()).unwrap();

    assert_eq!(hist, data.join(HIST_SUBDIR));
    assert_eq!(hist, scratch.path().join(PROJECT_DATA_DIR).join(HIST_SUBDIR));
}

/// The archive IMPORT root is `<project>/market_data/imports` — a SIBLING of the store, never
/// inside it (the store is the daemon's alone; this folder is filled by the operator's own tools),
/// and derived from the already-resolved settings directory, so it follows `$VIKE_SETTINGS_DIR`
/// wherever the boot's one walk put the project.
#[test]
fn the_imports_root_is_a_sibling_of_the_store_beside_the_resolved_settings() {
    let scratch = Scratch::new("data-imports");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let settings = project_settings_dir(scratch.path()).unwrap();

    let imports = imports_dir_beside(Some(&settings)).unwrap();
    assert_eq!(imports, scratch.path().join(PROJECT_DATA_DIR).join(IMPORTS_SUBDIR));
    let hist = project_hist_store_dir(scratch.path()).unwrap();
    assert_eq!(imports.parent(), hist.parent(), "imports/ and hist/ share market_data/");
    assert!(!imports.starts_with(&hist), "an import tree must not sit inside the store");
    assert_ne!(IMPORTS_SUBDIR, HIST_SUBDIR);
    assert!(!imports.exists(), "it resolves NOTHING and creates nothing");

    // A relocated project: the root moves with the settings directory it is derived from.
    let elsewhere = scratch.path().join("deployed").join(PROJECT_SETTINGS_DIR);
    assert_eq!(
        imports_dir_beside(Some(&elsewhere)).unwrap(),
        scratch.path().join("deployed").join(PROJECT_DATA_DIR).join(IMPORTS_SUBDIR)
    );
}

/// No settings directory — a daemon with no project above it — has NO imports root, and neither
/// does a bare relative `settings` whose `""` parent would resolve against the working directory.
/// The import lane is then not mounted at all (design §2.3), which is what this `None` drives.
#[test]
fn no_settings_dir_means_no_imports_root() {
    assert_eq!(imports_dir_beside(None), None);
    assert_eq!(imports_dir_beside(Some(Path::new(PROJECT_SETTINGS_DIR))), None);
}

/// Like `user_data/` and unlike `settings/`, `market_data/` is NOT a marker: a project that has
/// recorded nothing yet still resolves, and the store creates its own root on first write.
/// Probing for it would send every fresh install to the per-user directory on run one and to
/// the project on run two.
#[test]
fn the_data_dir_resolves_even_when_it_does_not_exist() {
    let scratch = Scratch::new("data-absent");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let data = project_data_dir(scratch.path()).unwrap();
    assert!(!data.exists(), "precondition: nothing created it");
    assert_eq!(data, scratch.path().join(PROJECT_DATA_DIR));
}

/// `bin/` is a SIBLING of the other three, not a child of any of them — the property
/// [`PROJECT_BIN_DIR`]'s doc argues from ownership, lifecycle and platform. Asserted against
/// all three siblings at once, because "sibling" is a claim about the whole set: a later edit
/// that nested it under `settings/` would still pass a test that only compared it to the root.
#[test]
fn the_tool_dir_is_a_sibling_of_settings_user_data_and_data() {
    let scratch = Scratch::new("bin-sibling");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let bin = project_bin_dir(scratch.path()).unwrap();

    assert_eq!(bin, scratch.path().join(PROJECT_BIN_DIR));
    assert_eq!(bin.parent(), project_settings_dir(scratch.path()).unwrap().parent());
    assert_eq!(bin.parent(), project_user_data_dir(scratch.path()).unwrap().parent());
    assert_eq!(bin.parent(), project_data_dir(scratch.path()).unwrap().parent());
}

/// Like `user_data/` and `market_data/` and unlike `settings/`, `bin/` is NOT a marker: a project that
/// has installed no tool yet still resolves. Probing for it would make "no tool installed" and
/// "wrong project" the same answer — which is the pair the caller most needs told apart, since
/// one is a fresh install and the other is a mis-resolution.
#[test]
fn the_tool_dir_resolves_even_when_it_does_not_exist() {
    let scratch = Scratch::new("bin-absent");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let bin = project_bin_dir(scratch.path()).unwrap();
    assert!(!bin.exists(), "precondition: nothing created it");
    assert_eq!(bin, scratch.path().join(PROJECT_BIN_DIR));
}

/// The tool twin of `the_settings_override_moves_the_data_root_with_it`, and the reason the
/// `_from` form ships in the same commit as the bare one: a unit whose service file relocates
/// the project must not read its settings from the new place and spawn a tool from the old
/// walk. That split is not hypothetical — it is what `project_log_dir_from` was added to close
/// after a the CI box daemon wrote its log beside the executable while reading relocated settings.
#[test]
fn the_settings_override_moves_the_tool_dir_with_it() {
    let scratch = Scratch::new("bin-override");
    // A resolvable walk underneath, so this asserts the override BEAT it rather than that the
    // walk found nothing.
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let walked = project_bin_dir(scratch.path()).unwrap();

    let elsewhere = scratch.path().join("relocated");
    let override_dir = elsewhere.join(PROJECT_SETTINGS_DIR);
    let override_str = override_dir.to_str().unwrap();

    let moved = project_bin_dir_from(Some(override_str), scratch.path()).unwrap();
    assert_eq!(moved, elsewhere.join(PROJECT_BIN_DIR), "tools hang off the OVERRIDE's root");
    assert_ne!(moved, walked, "the override must not agree with the walk by accident");

    // ...and a blank value is IGNORED rather than honoured, the same rule every `_from` in this
    // module follows: an empty `Environment=VIKE_SETTINGS_DIR=` line would otherwise resolve
    // the tool directory relative to the working directory.
    for blank in ["", "   ", "\t"] {
        assert_eq!(
            project_bin_dir_from(Some(blank), scratch.path()).unwrap(),
            walked,
            "a blank override must fall through to the walk"
        );
    }
}

/// `tmp/` is a SIBLING of the other four, not a child of any of them — the property
/// [`PROJECT_TMP_DIR`]'s doc argues from ownership, lifecycle, size and disk. Asserted against
/// all four siblings at once, for the reason the `bin/` twin gives: "sibling" is a claim about
/// the whole set, and a later edit that nested scratch under `settings/` (installed `-m700` and
/// hand-edited) or under `market_data/` (where a half-written stage file is a reader's correctness
/// problem) would still pass a test that only compared it to the root.
#[test]
fn the_scratch_dir_is_a_sibling_of_settings_user_data_data_and_bin() {
    let scratch = Scratch::new("tmp-sibling");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let tmp = project_tmp_dir(scratch.path()).unwrap();

    assert_eq!(tmp, scratch.path().join(PROJECT_TMP_DIR));
    assert_eq!(tmp.parent(), project_settings_dir(scratch.path()).unwrap().parent());
    assert_eq!(tmp.parent(), project_user_data_dir(scratch.path()).unwrap().parent());
    assert_eq!(tmp.parent(), project_data_dir(scratch.path()).unwrap().parent());
    assert_eq!(tmp.parent(), project_bin_dir(scratch.path()).unwrap().parent());
}

/// Like every sibling except `settings/`, `tmp/` is NOT a marker: a project that has staged
/// nothing yet still resolves. Probing for it would make "nothing staged yet" and "wrong
/// project" the same answer, and the first is the ordinary state of every fresh install.
#[test]
fn the_scratch_dir_resolves_even_when_it_does_not_exist() {
    let scratch = Scratch::new("tmp-absent");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let tmp = project_tmp_dir(scratch.path()).unwrap();
    assert!(!tmp.exists(), "precondition: nothing created it");
    assert_eq!(tmp, scratch.path().join(PROJECT_TMP_DIR));
}

/// The scratch twin of `the_settings_override_moves_the_tool_dir_with_it`. It matters more here
/// than for any other sibling: `<project>/tmp` exists because the project folder is the MOUNTED
/// volume, so a unit that relocated its project while scratch kept resolving off the working
/// directory would stage gigabytes outside the mount — the exact failure the directory replaces,
/// reintroduced by the walk instead of by the system temp directory.
#[test]
fn the_settings_override_moves_the_scratch_dir_with_it() {
    let scratch = Scratch::new("tmp-override");
    // A resolvable walk underneath, so this asserts the override BEAT it rather than that the
    // walk found nothing.
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let walked = project_tmp_dir(scratch.path()).unwrap();

    let elsewhere = scratch.path().join("relocated");
    let override_dir = elsewhere.join(PROJECT_SETTINGS_DIR);
    let override_str = override_dir.to_str().unwrap();

    let moved = project_tmp_dir_from(Some(override_str), scratch.path()).unwrap();
    assert_eq!(moved, elsewhere.join(PROJECT_TMP_DIR), "scratch hangs off the OVERRIDE's root");
    assert_ne!(moved, walked, "the override must not agree with the walk by accident");

    // ...and a blank value is IGNORED rather than honoured, the same rule every `_from` in this
    // module follows.
    for blank in ["", "   ", "\t"] {
        assert_eq!(
            project_tmp_dir_from(Some(blank), scratch.path()).unwrap(),
            walked,
            "a blank override must fall through to the walk"
        );
    }
}

/// **B4: `VIKE_SETTINGS_DIR` moves the DATA root too.** An operator who relocates their project
/// with that one variable must not get their settings from the new place and their tape from
/// the old walk — that is one project's credentials paired with another project's data.
#[test]
fn the_settings_override_moves_the_data_root_with_it() {
    let scratch = Scratch::new("data-override");
    // A real project on the walk, so the assertion is "the override BEAT a resolvable walk"
    // rather than "the walk found nothing".
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let walked = project_hist_store_dir(scratch.path()).unwrap();

    let elsewhere = scratch.path().join("relocated");
    let override_dir = elsewhere.join(PROJECT_SETTINGS_DIR);
    let override_str = override_dir.to_str().unwrap();

    assert_eq!(
        project_data_dir_from(Some(override_str), scratch.path()).unwrap(),
        elsewhere.join(PROJECT_DATA_DIR),
        "data hangs off the OVERRIDE's project root"
    );
    assert_eq!(
        project_hist_store_dir_from(Some(override_str), scratch.path()).unwrap(),
        elsewhere.join(PROJECT_DATA_DIR).join(HIST_SUBDIR)
    );
    assert_ne!(
        project_hist_store_dir_from(Some(override_str), scratch.path()).unwrap(),
        walked,
        "the override must actually move it, not agree with the walk by accident"
    );
    // …and settings landed in that same project, which is the invariant being protected.
    assert_eq!(
        project_settings_dir_from(Some(override_str), scratch.path()).unwrap().parent(),
        project_data_dir_from(Some(override_str), scratch.path()).unwrap().parent(),
        "settings and data must hang off ONE root under the override"
    );
}

/// **The LOG twin, and the one that was missing.** `project_log_dir` had no `_from` sibling, so
/// a daemon whose unit sets `VIKE_SETTINGS_DIR` still resolved its rolling trace file purely
/// from the working directory — measured on the CI box: variable set, CWD under no project marker,
/// log written to `<exe_dir>/logs`, the last resort. A blank value still falls through to the
/// walk, like every other member of this family.
#[test]
fn the_settings_override_moves_the_log_directory_with_it() {
    let scratch = Scratch::new("log-override");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let walked = project_log_dir(scratch.path()).unwrap();

    let elsewhere = scratch.path().join("relocated");
    let override_dir = elsewhere.join(PROJECT_SETTINGS_DIR);
    let override_str = override_dir.to_str().unwrap();

    assert_eq!(
        project_log_dir_from(Some(override_str), scratch.path()).unwrap(),
        override_dir.join(STATE_SUBDIR).join(LOGS_SUBDIR),
        "the log hangs off the OVERRIDE's settings/state, beside the state it belongs to"
    );
    assert_ne!(
        project_log_dir_from(Some(override_str), scratch.path()).unwrap(),
        walked,
        "the override must actually move it, not agree with the walk by accident"
    );
    for blank in ["", "   ", "\t"] {
        assert_eq!(
            project_log_dir_from(Some(blank), scratch.path()).unwrap(),
            walked,
            "a blank `{blank:?}` configured nothing"
        );
    }
    assert_eq!(project_log_dir_from(None, scratch.path()).unwrap(), walked);
}

/// A blank override configured nothing, exactly as for settings and user content — it falls
/// through to the walk rather than resolving the store to `""`.
#[test]
fn a_blank_settings_override_leaves_the_data_root_on_the_walk() {
    let scratch = Scratch::new("data-blank");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let walked = project_hist_store_dir(scratch.path()).unwrap();

    for blank in ["", "   ", "\t"] {
        assert_eq!(
            project_hist_store_dir_from(Some(blank), scratch.path()).unwrap(),
            walked,
            "blank override {blank:?} must fall through to the walk"
        );
    }
}

/// ⚠ **The empty-parent refusal.** `VIKE_SETTINGS_DIR=settings` — a bare relative name, easy to
/// leave in a unit file — has `""` for a parent, and joining `market_data/hist` onto that resolves the
/// store against the WORKING DIRECTORY: a store created wherever the shell happened to stand,
/// which is the exact bug `store_path` exists to eliminate. `None` instead, so the caller falls
/// through to a rung that names a real place.
#[test]
fn a_relative_override_with_no_parent_refuses_rather_than_resolving_against_the_cwd() {
    let scratch = Scratch::new("data-rootless");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    for bare in [PROJECT_SETTINGS_DIR, "vike-settings"] {
        assert_eq!(
            project_data_dir_from(Some(bare), scratch.path()),
            None,
            "a parentless override ({bare:?}) must refuse, never answer with a CWD-relative path"
        );
        assert_eq!(project_hist_store_dir_from(Some(bare), scratch.path()), None);
    }
}
