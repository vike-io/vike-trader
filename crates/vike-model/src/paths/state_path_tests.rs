use super::*;

/// A private scratch directory under the system temp dir — this crate has no `tempfile`
/// dev-dependency and Phase 2 adds no dependency, so the three filesystem tests below make
/// (and remove) their own unique directory.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-state-path-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `user_data/` is a SIBLING of `settings/`, and the whole design rests on it: a user copies
/// or commits `user_data/`, and `settings/` holds live venue keys (`settings/db/vike.db`, or
/// `settings/secrets.env` on a box that has not migrated) that must not ride along. Nesting it
/// would make that impossible to avoid.
#[test]
fn user_data_is_a_sibling_of_settings_never_a_child() {
    let scratch = Scratch::new("ud-sibling");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let settings = project_settings_dir(scratch.path()).unwrap();
    let user_data = project_user_data_dir(scratch.path()).unwrap();

    assert_eq!(settings.parent(), user_data.parent(), "same project root");
    assert!(
        !user_data.starts_with(&settings),
        "user_data ({}) must not live under settings ({})",
        user_data.display(),
        settings.display()
    );
    assert_eq!(user_data.file_name().unwrap(), PROJECT_USER_DATA_DIR);
}

/// Both directories must come from ONE walk. If they could disagree, a user would edit a
/// strategy in one project while the app read another's — and the walk's rules have already
/// been revised three times, so restating them separately would drift.
#[test]
fn user_data_and_settings_resolve_to_the_same_project() {
    let scratch = Scratch::new("ud-same-root");
    // A workspace root two levels above the starting directory: the interesting case, since a
    // naive "look next to me" resolver would answer with the wrong level.
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let deep = scratch.path().join("crates").join("thing");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join(CARGO_MANIFEST), "[package]\nname = \"thing\"\n").unwrap();

    let settings = project_settings_dir(&deep).unwrap();
    let user_data = project_user_data_dir(&deep).unwrap();

    assert_eq!(settings, scratch.path().join(PROJECT_SETTINGS_DIR));
    assert_eq!(user_data, scratch.path().join(PROJECT_USER_DATA_DIR));
}

/// Absence is the ordinary state of a fresh install, not a mis-resolution. `settings/` can
/// answer with a directory it FOUND because that directory is its own marker; `user_data/` is
/// not a marker, so it answers with where the directory BELONGS and lets the caller decide
/// between creating it and having no user content at all.
#[test]
fn user_data_resolves_even_when_the_directory_does_not_exist() {
    let scratch = Scratch::new("ud-absent");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let user_data = project_user_data_dir(scratch.path()).unwrap();
    assert!(!user_data.exists(), "precondition: nothing created it");
    assert_eq!(user_data, scratch.path().join(PROJECT_USER_DATA_DIR));
}

/// The override names the directory outright. A user pointing the app at a strategy library on
/// another disk is answering a different question from an operator relocating a deployment's
/// settings, which is why this is its own variable rather than a subdirectory of that one.
#[test]
fn the_override_wins_over_the_walk() {
    let scratch = Scratch::new("ud-override");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let elsewhere = scratch.path().join("some").join("other").join("place");
    let got = project_user_data_dir_from(Some(elsewhere.to_str().unwrap()), scratch.path());
    assert_eq!(got.unwrap(), elsewhere);
}

/// A blank value configured nothing. Honouring it would resolve user content to `""` and scan
/// the working directory for strategies — the same class of bug the store-path blank guard
/// exists for, and an empty `Environment=VIKE_USER_DATA_DIR=` line is easy to leave behind.
#[test]
fn a_blank_override_is_ignored_not_honoured() {
    let scratch = Scratch::new("ud-blank");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let walked = project_user_data_dir(scratch.path()).unwrap();

    for blank in ["", "   ", "\t"] {
        assert_eq!(
            project_user_data_dir_from(Some(blank), scratch.path()).unwrap(),
            walked,
            "blank override {blank:?} must fall through to the walk"
        );
    }
}

// ---- the SIBLING off an already-resolved settings directory ------------------------------

/// The ordinary case must be IDENTICAL to the walking twin, or a composition root switching to
/// the sibling form would silently relocate every user's strategy library.
#[test]
fn the_sibling_agrees_with_the_walk_when_nothing_overrides_it() {
    let scratch = Scratch::new("ud-beside-agrees");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let settings = project_settings_dir(scratch.path());

    assert_eq!(
        user_data_dir_beside(None, settings.as_deref()),
        project_user_data_dir_from(None, scratch.path()),
    );
}

/// **The whole reason this function exists.** `project_user_data_dir_from`'s fallback WALKS,
/// and that walk does not honour `$VIKE_SETTINGS_DIR` — so a root that resolved its settings
/// through the override and then asked for the sibling got a different project back. Here the
/// working directory sits under one project and the override names another; the sibling must
/// follow the OVERRIDE, and the walking twin demonstrably does not.
#[test]
fn the_sibling_follows_the_settings_override_where_the_walk_cannot() {
    let scratch = Scratch::new("ud-beside-override");
    let walked_project = scratch.path().join("checkout");
    std::fs::create_dir_all(&walked_project).unwrap();
    std::fs::write(walked_project.join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let named_project = scratch.path().join("deployment");
    let named_settings = named_project.join(PROJECT_SETTINGS_DIR);
    std::fs::create_dir_all(&named_settings).unwrap();

    let settings =
        project_settings_dir_from(Some(named_settings.to_str().unwrap()), &walked_project);
    assert_eq!(settings.as_deref(), Some(named_settings.as_path()), "precondition");

    assert_eq!(
        user_data_dir_beside(None, settings.as_deref()).unwrap(),
        named_project.join(PROJECT_USER_DATA_DIR),
        "the sibling hangs off the directory that ACTUALLY answered"
    );
    assert_eq!(
        project_user_data_dir_from(None, &walked_project).unwrap(),
        walked_project.join(PROJECT_USER_DATA_DIR),
        "…and the walking twin still answers with the CWD's project — the defect, pinned"
    );
}

/// `VIKE_USER_DATA_DIR` still wins outright, and a blank one still falls through — the same
/// contract as the walking twin, because a caller swapping one for the other must not have to
/// re-read the blank-value rule.
#[test]
fn the_sibling_keeps_the_user_data_override_and_its_blank_guard() {
    let settings = PathBuf::from("/srv/proj").join(PROJECT_SETTINGS_DIR);
    assert_eq!(
        user_data_dir_beside(Some("/elsewhere/lib"), Some(&settings)).unwrap(),
        PathBuf::from("/elsewhere/lib")
    );
    for blank in ["", "   ", "\t"] {
        assert_eq!(
            user_data_dir_beside(Some(blank), Some(&settings)).unwrap(),
            PathBuf::from("/srv/proj").join(PROJECT_USER_DATA_DIR),
            "a blank {blank:?} must fall through to the sibling, not resolve to \"\""
        );
    }
}

/// No settings directory ⇒ no sibling. A root with no project above it has no user content, and
/// inventing one from the working directory is the CWD-relative default this whole module
/// exists to eliminate. An EMPTY parent (a bare relative `VIKE_SETTINGS_DIR=settings`) is
/// refused for the same reason — see `project_root_from`.
#[test]
fn the_sibling_refuses_an_absent_or_rootless_settings_dir() {
    assert_eq!(user_data_dir_beside(None, None), None);
    assert_eq!(user_data_dir_beside(None, Some(Path::new(PROJECT_SETTINGS_DIR))), None);
}

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

/// The two strategy roots differ by LANGUAGE under one artifact folder — `strategies/rhai` and
/// `strategies/rust`, not `rhai/strategies`. Artifact first keeps "where are my strategies" a
/// single answer and lets a third language add a sibling instead of a new root.
#[test]
fn strategy_roots_are_artifact_major_then_language() {
    let scratch = Scratch::new("ud-strat");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let root = scratch.path().join(PROJECT_USER_DATA_DIR).join(STRATEGIES_SUBDIR);

    assert_eq!(user_rhai_strategies_dir(scratch.path()).unwrap(), root.join(RHAI_SUBDIR));
    assert_eq!(user_rust_strategies_dir(scratch.path()).unwrap(), root.join(RUST_SUBDIR));
}

/// Indicators are a SIBLING of `strategies/`, not a third tier under it — and flat, so the
/// path ends at the directory rather than at a per-indicator folder.
#[test]
fn indicators_sit_beside_strategies_and_are_flat() {
    let scratch = Scratch::new("ind-sibling");
    std::fs::write(
        scratch.path().join(CARGO_MANIFEST),
        "[workspace]
",
    )
    .unwrap();
    let ud = scratch.path().join(PROJECT_USER_DATA_DIR);

    assert_eq!(user_indicators_dir(scratch.path()).unwrap(), ud.join(INDICATORS_SUBDIR));
    assert_ne!(
        user_indicators_dir(scratch.path()).unwrap(),
        ud.join(STRATEGIES_SUBDIR).join(INDICATORS_SUBDIR),
        "indicators are not filed under strategies/"
    );
}

/// No project above the start path means `None`, matching every sibling resolver: an
/// unconfigured tree is ordinary, not an error.
#[test]
fn no_project_means_no_indicators_dir() {
    let scratch = Scratch::new("ind-none");
    assert!(user_indicators_dir(&scratch.path().join("nowhere")).is_none());
}

/// ONE runs directory for every kind of run, and it sits at the `user_data/` ROOT rather than
/// under a `research/` parent. A strategy backtest is not research, and filing it under one
/// would hide the pairing this directory exists to keep visible: the same idea carried from a
/// research run into a strategy run.
#[test]
fn runs_sit_at_the_user_data_root_rather_than_under_research() {
    let scratch = Scratch::new("ud-runs-root");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let ud = scratch.path().join(PROJECT_USER_DATA_DIR);

    assert_eq!(user_runs_dir(scratch.path()).unwrap(), ud.join(RUNS_SUBDIR));
    assert_ne!(
        user_runs_dir(scratch.path()).unwrap(),
        ud.join("research").join(RUNS_SUBDIR),
        "a strategy backtest is not research, so runs/ is not filed under it"
    );
}

/// The SAME walk as every sibling resolver, answered from two levels down. A second walk could
/// disagree, and then a run would land in one project while the strategy that produced it was
/// read from another.
#[test]
fn runs_and_settings_resolve_to_the_same_project() {
    let scratch = Scratch::new("ud-runs-same-root");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let deep = scratch.path().join("crates").join("thing");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join(CARGO_MANIFEST), "[package]\nname = \"thing\"\n").unwrap();

    assert_eq!(project_settings_dir(&deep).unwrap(), scratch.path().join(PROJECT_SETTINGS_DIR));
    assert_eq!(
        user_runs_dir(&deep).unwrap(),
        scratch.path().join(PROJECT_USER_DATA_DIR).join(RUNS_SUBDIR)
    );
}

/// `user_data/` is no marker and neither is `runs/`: a project that has run nothing is a fresh
/// install, not a mis-resolution. So the resolver answers with where the directory BELONGS and
/// leaves creating it to whoever writes the first run.
#[test]
fn runs_resolve_even_when_the_directory_does_not_exist() {
    let scratch = Scratch::new("ud-runs-absent");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let runs = user_runs_dir(scratch.path()).unwrap();
    assert!(!runs.exists(), "precondition: nothing created it");
    assert_eq!(runs, scratch.path().join(PROJECT_USER_DATA_DIR).join(RUNS_SUBDIR));
}

/// The override has to move the RUNS directory, not just the indicators one. It did not once,
/// and `crates/vike-backtest/src/backtest_cli.rs`'s `persist_run` declared that as a residual in
/// its own doc: an operator who redirected `user_data` got their indicators from the override
/// and their runs from the project walk, in one process, with nothing reporting the split.
#[test]
fn the_user_data_override_moves_the_runs_directory_too() {
    let scratch = Scratch::new("ud-runs-override");
    let elsewhere = scratch.path().join("mnt-big-user-data");

    let moved = user_runs_dir_from(Some(elsewhere.to_str().unwrap()), scratch.path())
        .expect("an override always resolves — it needs no project above the start path");

    assert_eq!(moved, elsewhere.join(RUNS_SUBDIR));
}

/// ⚠ **The marks directory is a SIBLING of the runs directory, never a child**, and this is the
/// assertion that holds it there. A `marks/` folder under the runs root is, to every scan of
/// that tree, a run directory holding no manifest — i.e. a permanent "this run never finished
/// writing" diagnostic row in every listing on every box. [`MARKS_SUBDIR`] carries the argument;
/// this pins the shape the argument is about.
#[test]
fn marks_sit_beside_the_runs_directory_rather_than_inside_it() {
    let scratch = Scratch::new("ud-marks-sibling");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let user_data = project_user_data_dir(scratch.path()).unwrap();
    let runs = user_runs_dir(scratch.path()).unwrap();
    let marks = user_data.join(MARKS_SUBDIR);

    assert_eq!(marks.parent(), runs.parent(), "the same parent — siblings, not nested");
    assert!(!marks.starts_with(&runs), "a marks directory under runs/ reads as a broken run");
    assert_ne!(MARKS_SUBDIR, RUNS_SUBDIR);
}

/// A blank value is IGNORED rather than honoured — the same rule every `_from` resolver in this
/// file applies, so an `Environment=VIKE_USER_DATA_DIR=` line cannot silently point a daemon at
/// the filesystem root. `None` is the same case by construction.
#[test]
fn a_blank_runs_override_falls_through_to_the_walk() {
    let scratch = Scratch::new("ud-runs-blank-override");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    assert_eq!(user_runs_dir_from(Some("   "), scratch.path()), user_runs_dir(scratch.path()));
    assert_eq!(user_runs_dir_from(None, scratch.path()), user_runs_dir(scratch.path()));
}

/// No project above the start path means `None`, matching every sibling resolver: an
/// unconfigured tree is ordinary, not an error.
#[test]
fn no_project_means_no_runs_dir() {
    let scratch = Scratch::new("runs-none");
    assert!(user_runs_dir(&scratch.path().join("nowhere")).is_none());
}

/// A study is written in the SAME two tiers a strategy is — `research/studies/rhai` and
/// `research/studies/rust` — and the tier constants are the strategy tree's own, not a second
/// pair spelled the same. A study is a strategy-shaped artifact: interpreted work runs on a
/// binary install, compiled work needs a checkout, and that difference is the same difference.
#[test]
fn study_roots_mirror_the_strategy_tiers_exactly() {
    let scratch = Scratch::new("ud-study-tiers");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let studies =
        scratch.path().join(PROJECT_USER_DATA_DIR).join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR);

    assert_eq!(user_studies_dir(scratch.path()).unwrap(), studies);
    assert_eq!(user_rhai_studies_dir(scratch.path()).unwrap(), studies.join(RHAI_SUBDIR));
    assert_eq!(user_rust_studies_dir(scratch.path()).unwrap(), studies.join(RUST_SUBDIR));
}

/// Studies are filed under `research/`, and RUNS deliberately are not — [`RUNS_SUBDIR`] carries
/// that argument. The two live at different depths on purpose, so this pins the pair together:
/// a later tidy-up that moved runs under `research/` would break exactly this.
#[test]
fn studies_are_research_but_runs_are_not() {
    let scratch = Scratch::new("ud-study-vs-runs");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let ud = scratch.path().join(PROJECT_USER_DATA_DIR);

    assert_eq!(
        user_studies_dir(scratch.path()).unwrap(),
        ud.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR)
    );
    assert_eq!(user_runs_dir(scratch.path()).unwrap(), ud.join(RUNS_SUBDIR));
    assert_ne!(
        user_runs_dir(scratch.path()).unwrap(),
        ud.join(RESEARCH_SUBDIR).join(RUNS_SUBDIR),
        "a strategy backtest is not research — runs stay at the user_data root"
    );
}

/// The SAME walk as every sibling resolver, answered from two levels down — the property that
/// keeps "which project am I in" a single answer for a study and for the run it produced.
#[test]
fn studies_and_runs_resolve_to_the_same_project() {
    let scratch = Scratch::new("ud-study-same-root");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
    let deep = scratch.path().join("crates").join("thing");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join(CARGO_MANIFEST), "[package]\nname = \"thing\"\n").unwrap();
    let ud = scratch.path().join(PROJECT_USER_DATA_DIR);

    assert_eq!(
        user_rhai_studies_dir(&deep).unwrap(),
        ud.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR).join(RHAI_SUBDIR)
    );
    assert_eq!(user_runs_dir(&deep).unwrap(), ud.join(RUNS_SUBDIR));
}

/// Like every `user_data/` resolver: the answer is where the directory BELONGS, whether or not
/// it exists. A project that has studied nothing is a fresh install, not a mis-resolution.
#[test]
fn studies_resolve_even_when_the_directory_does_not_exist() {
    let scratch = Scratch::new("ud-study-absent");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let studies = user_studies_dir(scratch.path()).unwrap();
    assert!(!studies.exists(), "precondition: nothing created it");
    assert!(!user_rhai_studies_dir(scratch.path()).unwrap().exists());
}

/// No project above the start path means `None`, matching every sibling resolver: an
/// unconfigured tree is ordinary, not an error.
#[test]
fn no_project_means_no_studies_dir() {
    let scratch = Scratch::new("study-none");
    let nowhere = scratch.path().join("nowhere");
    assert!(user_studies_dir(&nowhere).is_none());
    assert!(user_rhai_studies_dir(&nowhere).is_none());
    assert!(user_rust_studies_dir(&nowhere).is_none());
}

/// User logs are the user's; the app's runtime log stays under `settings/state`. A daemon on a
/// server has no `user_data/` at all, and a user must be able to delete their backtest traces
/// without touching anything the program relies on.
#[test]
fn user_logs_are_separate_from_the_app_runtime_log() {
    let scratch = Scratch::new("ud-logs");
    std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

    let user = user_logs_dir(scratch.path()).unwrap();
    let app = project_log_dir(scratch.path()).unwrap();

    assert_ne!(user, app);
    assert!(user.starts_with(scratch.path().join(PROJECT_USER_DATA_DIR)));
    assert!(app.starts_with(scratch.path().join(PROJECT_SETTINGS_DIR)));
}

/// The dual read: the NEW path wins the moment the file exists there.
#[test]
fn read_prefers_the_new_path_when_the_file_exists() {
    let scratch = Scratch::new("read-new");
    let state = scratch.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("thing.json"), "{}").unwrap();
    let legacy = scratch.path().join("legacy").join("thing.json");

    let got = read_path(Some(state.as_path()), "thing.json", legacy);
    assert_eq!(got, state.join("thing.json"));
}

/// …and falls back to the OLD path when it does not — including when the state dir EXISTS but
/// holds only some OTHER already-migrated file.
#[test]
fn read_falls_back_to_the_legacy_path() {
    let scratch = Scratch::new("read-old");
    let state = scratch.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("other.json"), "{}").unwrap();
    let legacy_dir = scratch.path().join("legacy");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    let legacy = legacy_dir.join("thing.json");
    std::fs::write(&legacy, "{}").unwrap();

    assert_eq!(read_path(Some(state.as_path()), "thing.json", legacy.clone()), legacy);
    assert_eq!(read_path(None, "thing.json", legacy.clone()), legacy, "no state dir ⇒ legacy");
}

/// A write always lands on the new path, and creates the directory on the way — lazily, only
/// when something is actually written.
#[test]
fn write_lands_on_the_new_path_and_creates_the_directory() {
    let scratch = Scratch::new("write");
    let state = scratch.path().join("deep").join("state");
    assert!(!state.exists(), "precondition: the directory does not exist yet");

    let p = write_path(Some(state.as_path()), "thing.json").expect("creatable");
    assert_eq!(p, state.join("thing.json"));
    assert!(state.is_dir(), "the state directory is created lazily on first write");
}

/// **A state FILE that is a symlink is refused.**
///
/// Everything under `settings/state` is program-written: the caller takes this path and
/// `fs::write`s it, which TRUNCATES whatever the link points at and replaces it with content
/// the program chose. Nothing here has a reason to be redirected file-by-file — these files
/// are re-derivable, their names are constants, and the operator-facing way to relocate them
/// is `VIKE_STATE_ROOT` / `VIKE_SETTINGS_DIR`, which relocates the whole directory.
///
/// The assertion that matters is the last one: the decoy's CONTENT is intact. A refusal that
/// still truncated the target would pass an `is_err()` check.
#[cfg(unix)]
#[test]
fn a_symlinked_state_file_is_refused_and_its_target_is_untouched() {
    let scratch = Scratch::new("write-symlink-file");
    let state = scratch.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let decoy = scratch.path().join("important.conf");
    std::fs::write(&decoy, "DO NOT TRUNCATE ME").unwrap();
    std::os::unix::fs::symlink(&decoy, state.join("thing.json")).unwrap();

    let err = write_path(Some(state.as_path()), "thing.json")
        .expect_err("a symlinked state file must be refused, not written through");
    assert!(err.to_string().to_lowercase().contains("symlink"), "the refusal must say why: {err}");
    assert_eq!(
        std::fs::read_to_string(&decoy).unwrap(),
        "DO NOT TRUNCATE ME",
        "the link's target must be untouched"
    );
}

/// **…but a symlinked state DIRECTORY is fine.**
///
/// Pointing `settings/` or `settings/state` at shared or larger storage is a legitimate
/// operator setup — it is the filesystem's spelling of the same intent `VIKE_STATE_ROOT`
/// serves — and the write still lands inside a directory the operator chose, under that
/// directory's own permissions. Refusing it would break real deployments to buy nothing:
/// the danger is the WRITE TARGET, and the target here is a plain file.
#[cfg(unix)]
#[test]
fn a_symlinked_state_directory_is_accepted() {
    let scratch = Scratch::new("write-symlink-dir");
    let real = scratch.path().join("shared-storage");
    std::fs::create_dir_all(&real).unwrap();
    let state = scratch.path().join("state");
    std::os::unix::fs::symlink(&real, &state).unwrap();

    let p = write_path(Some(state.as_path()), "thing.json")
        .expect("a symlinked state DIRECTORY is a legitimate operator setup");
    assert_eq!(p, state.join("thing.json"));
    std::fs::write(&p, "{}").unwrap();
    assert!(real.join("thing.json").is_file(), "the write lands in the linked directory");
}

// ---- the settings-directory walk: the two markers ----------------------------------------

/// **The DEV shape, unchanged.** A source checkout resolves to the OUTERMOST `Cargo.toml`'s
/// `settings/` from any depth — the #1089 regression guard, restated here because
/// `state_path.rs` never had one of its own (only `vike-secrets`' copy did) and the two-marker
/// change is exactly the kind that could quietly reintroduce the nearest-crate answer.
#[test]
fn a_source_checkout_resolves_to_the_outermost_cargo_toml() {
    let scratch = Scratch::new("dev-shape");
    let root = scratch.path();
    let crate_dir = root.join("crates").join("bridges").join("aster");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();

    let want = root.join(PROJECT_SETTINGS_DIR);
    for from in [root, &crate_dir, &crate_dir.join("src")] {
        assert_eq!(project_settings_dir(from).as_deref(), Some(want.as_path()));
    }
    assert_eq!(project_state_dir(&crate_dir).as_deref(), Some(want.join(STATE_SUBDIR).as_path()));
}

/// …and a `settings/` directory sitting at a CRATE level does not capture it either. This is
/// the shape the two-marker walk could have broken and the reason `Cargo.toml` outranks the
/// deployment marker instead of competing with it level by level.
#[test]
fn a_settings_dir_inside_a_checkout_never_beats_the_workspace_root() {
    let scratch = Scratch::new("dev-inner-settings");
    let root = scratch.path();
    let crate_dir = root.join("crates").join("bridges").join("aster");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
    // A real `settings/` directory at the crate level — the nearest deployment marker there is.
    std::fs::create_dir_all(crate_dir.join(PROJECT_SETTINGS_DIR)).unwrap();

    assert_eq!(
        project_settings_dir(&crate_dir.join("src")).as_deref(),
        Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
        "the workspace root must still win: a stray settings/ must not capture the project"
    );

    // …including a `settings/` at a level with NO manifest of its own — deployment-SHAPED, but
    // inside a checkout. **This is the one accepted residual, pinned rather than left to
    // drift**: it is byte-identical to the #1089 tree (`crates/bridges/aster/settings/` is also
    // a settings/ below the workspace root with no [workspace] table at that level), so no
    // marker rule can serve both, and a declared workspace root must keep winning. An operator
    // who genuinely deploys inside a checkout names the directory with `VIKE_SETTINGS_DIR`.
    let inside = root.join("deploy").join("vike");
    std::fs::create_dir_all(inside.join(PROJECT_SETTINGS_DIR)).unwrap();
    assert_eq!(
        project_settings_dir(&inside).as_deref(),
        Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
        "a declared [workspace] root decides alone — relaxing that is #1089"
    );
}

/// ⚠ **THE HIJACK.** One unrelated `Cargo.toml` a single level above a project used to capture
/// it, because the walk kept the OUTERMOST manifest unconditionally — a `cargo new` at the
/// wrong level, a parent monorepo or a vendored crate directory was enough. With no outer
/// `settings/` the project's own populated store simply vanished; with one, the project
/// silently read the OTHER tree's credentials.
///
/// Reproduced end-to-end on the CI box before the fix, in exactly this shape: `vike-cli secrets
/// list` printed `no store found — every venue stays paper` without the outer `settings/`, and
/// listed the OUTER key and never the inner one with it.
#[test]
fn an_unrelated_outer_manifest_cannot_capture_the_project() {
    let scratch = Scratch::new("hijack");
    let outer = scratch.path();
    // The stray: a plain PACKAGE manifest, plus a settings/ to be stolen from.
    std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    std::fs::create_dir_all(outer.join(PROJECT_SETTINGS_DIR)).unwrap();
    // The project: a real cargo WORKSPACE with its own settings/.
    let proj = outer.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::create_dir_all(proj.join(PROJECT_SETTINGS_DIR)).unwrap();

    let want = proj.join(PROJECT_SETTINGS_DIR);
    for from in [&proj, &proj.join("src")] {
        assert_eq!(
            project_settings_dir(from).as_deref(),
            Some(want.as_path()),
            "a stranger's manifest above the project must not capture its settings"
        );
    }
    assert_eq!(project_state_dir(&proj).as_deref(), Some(want.join(STATE_SUBDIR).as_path()));
}

/// The same escape with **no `[workspace]` table anywhere** — a single-crate project under a
/// stray manifest. Nothing on the chain claims to be a workspace root, so the NEAREST manifest
/// is the project and the outermost is just the nearest stranger.
///
/// ⚠ This arm cannot reopen #1089: that bug needs `cargo test -p <crate>`, which needs a
/// workspace, which needs a `[workspace]` table — and a chain that has one never reaches here.
#[test]
fn a_plain_package_project_under_a_stray_manifest_keeps_its_own_settings() {
    let scratch = Scratch::new("hijack-plain");
    let outer = scratch.path();
    std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

    assert_eq!(
        project_settings_dir(&proj.join("src")).as_deref(),
        Some(proj.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// A COMMENTED-OUT `[workspace]` is not a workspace table — the marker is a table HEADER, and
/// `# [workspace]` is prose. Without this the stray above would capture the project again.
#[test]
fn a_commented_out_workspace_table_is_not_a_workspace_root() {
    let scratch = Scratch::new("hijack-comment");
    let outer = scratch.path();
    std::fs::write(outer.join("Cargo.toml"), "# [workspace]\n[package]\nname=\"x\"\n").unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[workspace]\n").unwrap();

    assert_eq!(
        project_settings_dir(&proj).as_deref(),
        Some(proj.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// A NESTED workspace resolves to the OUTERMOST one, not the nearest — this repo HAS one
/// (`crates/bridges/ctrader/protogen` carries its own `[workspace]` table so the drift-gate
/// codegen stays out of the build), and a tool run from inside it must still find the project's
/// settings. This is the guard on choosing "outermost `[workspace]`" over "nearest".
#[test]
fn a_nested_workspace_still_resolves_to_the_outermost_one() {
    let scratch = Scratch::new("nested-ws");
    let root = scratch.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let inner = root.join("crates").join("ctrader").join("protogen");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("Cargo.toml"), "[package]\nname=\"p\"\n[workspace]\n").unwrap();

    assert_eq!(
        project_settings_dir(&inner).as_deref(),
        Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
        "a nested workspace must not become the project"
    );
}

/// An UNREADABLE manifest is evidence of NOTHING, so the walk degrades to the rule that
/// shipped — the outermost manifest — rather than to a new answer. Guessing the other way
/// would mean an unreadable ROOT manifest resolves to the nearest crate, which is #1089
/// (credentials silently vanish, every venue on paper) — the worse of the two failures.
///
/// Invalid UTF-8 is the portable stand-in for "cannot be read"; a mode-000 file is not.
#[test]
fn an_unreadable_manifest_degrades_to_the_shipped_outermost_rule() {
    let scratch = Scratch::new("unreadable");
    let outer = scratch.path();
    std::fs::write(outer.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

    assert_eq!(
        project_settings_dir(&proj).as_deref(),
        Some(outer.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// **The DEPLOYMENT shape — the bug this walk was extended for.** A project root holds a binary,
/// a profile and `settings/`, and NO `Cargo.toml` anywhere above it. Before the second marker
/// this returned `None` and a production daemon ran with no settings and no credentials.
#[test]
fn a_deployment_with_no_cargo_toml_resolves_through_its_settings_dir() {
    let scratch = Scratch::new("deploy-shape");
    let opt_vike = scratch.path().join("opt").join("vike");
    std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

    let want = opt_vike.join(PROJECT_SETTINGS_DIR);
    // From the unit's WorkingDirectory…
    assert_eq!(project_settings_dir(&opt_vike).as_deref(), Some(want.as_path()));
    // …and from anywhere below it.
    assert_eq!(project_settings_dir(&opt_vike.join("bin")).as_deref(), Some(want.as_path()));
    assert_eq!(project_state_dir(&opt_vike).as_deref(), Some(want.join(STATE_SUBDIR).as_path()));
}

/// ⚠ **THE DEPLOYMENT HIJACK — the half #1101 left behind.** A deployment is a `settings/`
/// directory beside a binary with **no `Cargo.toml` at that level**, and ONE unrelated
/// `[package]` manifest anywhere above it used to take it: with no `[workspace]` table on the
/// chain the manifest arm falls back to the NEAREST manifest, which is still above the
/// deployment, and the `settings/` arm was never reached at all because a manifest existed.
///
/// Reproduced end-to-end on the CI box with a real `vike-cli` before the fix, in exactly this shape:
/// from a deployment holding `settings/policy.toml` (ceiling 22.0) and `settings/secrets.env`,
/// `secrets list` printed `no store found — every venue stays paper` and `config show` reported
/// `policy.max_notional_per_order  -  default` — a daemon with **no ceiling and no
/// credentials**, silently, because one stranger's manifest sat above the install directory.
#[test]
fn a_stray_manifest_above_a_deployment_cannot_capture_it() {
    let scratch = Scratch::new("deploy-hijack");
    let parent = scratch.path();
    std::fs::write(parent.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    let opt_vike = parent.join("opt-vike");
    std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

    let want = opt_vike.join(PROJECT_SETTINGS_DIR);
    for from in [&opt_vike, &opt_vike.join("bin")] {
        assert_eq!(
            project_settings_dir(from).as_deref(),
            Some(want.as_path()),
            "a stranger's manifest above a deployment must not capture its settings"
        );
    }
    assert_eq!(project_state_dir(&opt_vike).as_deref(), Some(want.join(STATE_SUBDIR).as_path()));
}

/// …and when the stranger has a `settings/` of its own, the deployment must still take ITS OWN
/// — the NEAREST one, per the blast-radius reasoning the deployment marker was chosen for.
/// Measured on the CI box before the fix: the stranger's 999.0 ceiling won over the deployment's
/// 22.0, and `secrets list` printed the stranger's key and never the deployment's.
#[test]
fn a_deployment_under_a_stray_manifest_takes_the_nearest_settings_dir() {
    let scratch = Scratch::new("deploy-hijack-both");
    let parent = scratch.path();
    std::fs::write(parent.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    std::fs::create_dir_all(parent.join(PROJECT_SETTINGS_DIR)).unwrap();
    let opt_vike = parent.join("opt-vike");
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

    assert_eq!(
        project_settings_dir(&opt_vike).as_deref(),
        Some(opt_vike.join(PROJECT_SETTINGS_DIR).as_path()),
        "the deployment's own settings/ is nearer than the stranger's — it must win"
    );
}

/// **The cure must not overreach into #1101's bug.** A stray `settings/` ABOVE a plain-package
/// project must not capture it: the project's own manifest is NEARER, and it is the nearest
/// marker OF EITHER KIND that answers — not "any `settings/` outranks any workspace-less
/// manifest", which would hand back exactly the credential theft #1101 fixed.
#[test]
fn a_stray_settings_dir_above_a_plain_package_project_cannot_capture_it() {
    let scratch = Scratch::new("no-overreach");
    let outer = scratch.path();
    std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
    std::fs::create_dir_all(outer.join(PROJECT_SETTINGS_DIR)).unwrap();
    let proj = outer.join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();
    // …and the project has NOT created its settings/ yet: the stray's is the only one that
    // EXISTS, which is precisely when a settings-first rule would reach for it.

    let want = proj.join(PROJECT_SETTINGS_DIR);
    for from in [&proj, &proj.join("src")] {
        assert_eq!(
            project_settings_dir(from).as_deref(),
            Some(want.as_path()),
            "the project's own manifest is nearer than the stray settings/ — it must win"
        );
    }
}

/// **An UNREADABLE manifest still decides ALONE**, so a crate-level `settings/` cannot answer
/// for a checkout whose root manifest could not be read. This is the guard on NOT applying the
/// nearest-marker rule to every case: the aster crate directory holds BOTH markers, so a walk
/// that let `settings/` compete here would resolve `crates/bridges/aster/settings` — #1089
/// exactly, credentials silently gone and every venue on paper.
#[test]
fn an_unreadable_root_manifest_still_outranks_a_crate_level_settings_dir() {
    let scratch = Scratch::new("unreadable-vs-settings");
    let root = scratch.path();
    std::fs::write(root.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
    let crate_dir = root.join("crates").join("bridges").join("aster");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();
    std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
    std::fs::create_dir_all(crate_dir.join(PROJECT_SETTINGS_DIR)).unwrap();

    assert_eq!(
        project_settings_dir(&crate_dir.join("src")).as_deref(),
        Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
        "an unreadable manifest is evidence of nothing — it must not let a crate-level \
             settings/ answer, which is #1089"
    );
}

/// The deployment marker is the NEAREST one, so a stray `settings/` higher up loses to the
/// deployment's own.
#[test]
fn the_deployment_marker_takes_the_nearest_settings_dir() {
    let scratch = Scratch::new("deploy-nearest");
    let stray = scratch.path().join("srv");
    let opt_vike = stray.join("opt").join("vike");
    std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();
    std::fs::create_dir_all(stray.join(PROJECT_SETTINGS_DIR)).unwrap();

    assert_eq!(
        project_settings_dir(&opt_vike).as_deref(),
        Some(opt_vike.join(PROJECT_SETTINGS_DIR).as_path()),
    );
}

/// A `settings` FILE is not a settings directory — the probe is `is_dir`, not `exists`.
#[test]
fn a_settings_file_is_not_the_marker() {
    let scratch = Scratch::new("settings-file");
    let dir = scratch.path().join("deploy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(PROJECT_SETTINGS_DIR), "not a directory").unwrap();
    assert_eq!(project_settings_dir(&dir), None);
}

/// Neither marker anywhere ⇒ `None`. Nothing is invented from the CWD, and a deployment that
/// has not created `settings/` yet is told so by the caller rather than guessed at.
///
/// ⚠ Assumes the system temp directory's own ancestry holds neither marker — the same
/// assumption `vike-secrets`' sibling test already makes for `Cargo.toml`. A failure here means
/// a stray `settings/` or `Cargo.toml` was created above `TMPDIR`, not that the walk regressed.
#[test]
fn no_marker_anywhere_still_refuses_to_guess() {
    let scratch = Scratch::new("no-marker");
    let deep = scratch.path().join("a").join("b").join("c");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(project_settings_dir(&deep), None, "no marker above TMPDIR: see the doc note");
    assert_eq!(project_state_dir(&deep), None);
}

/// The LOG directory is the state directory plus one named level — never a second walk, never
/// a second root. Pinned because the whole point of the repoint is that logs stop having a
/// location of their own (`<exe_dir>/logs`) and share the one the app already owns.
#[test]
fn the_log_dir_is_the_state_dir_plus_one_level() {
    let scratch = Scratch::new("logdir");
    let root = scratch.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

    let state = project_state_dir(root).expect("a workspace root resolves a state dir");
    assert_eq!(project_log_dir(root), Some(state.join(LOGS_SUBDIR)));
    assert_eq!(
        project_log_dir(root),
        Some(root.join(PROJECT_SETTINGS_DIR).join(STATE_SUBDIR).join(LOGS_SUBDIR)),
        "spelled out, so a change to any of the three names is visible here"
    );

    // And from DEEPER in the same project it is still the project's one directory, not a
    // CWD-relative one — a daemon started from a sub-directory must not scatter log files.
    let deep = root.join("a").join("b");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(project_log_dir(&deep), Some(state.join(LOGS_SUBDIR)));
}

/// No project above the working directory ⇒ **no answer**, exactly like every other resolver
/// here. The caller then keeps whatever it used before (for logs, vike-log's `<exe_dir>/logs`)
/// rather than this inventing a location from the CWD.
///
/// ⚠ Same TMPDIR-ancestry assumption as `no_marker_anywhere_still_refuses_to_guess` above.
#[test]
fn no_project_means_no_log_dir_rather_than_a_guess() {
    let scratch = Scratch::new("logdir-none");
    let deep = scratch.path().join("a").join("b").join("c");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(project_log_dir(&deep), None, "no marker above TMPDIR: see the doc note");
}

/// **The override wins over both markers**, and a blank one is ignored rather than resolving
/// settings to `""`.
#[test]
fn the_settings_dir_override_beats_the_walk() {
    let scratch = Scratch::new("override");
    let root = scratch.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let elsewhere = root.join("elsewhere");

    assert_eq!(
        project_settings_dir_from(elsewhere.to_str(), root),
        Some(elsewhere.clone()),
        "an explicit VIKE_SETTINGS_DIR must not be second-guessed by the walk"
    );
    assert_eq!(
        project_state_dir_from(elsewhere.to_str(), root),
        Some(elsewhere.join(STATE_SUBDIR)),
    );
    // …and it need not exist yet: it names a location, it is not a probe.
    assert!(!elsewhere.exists());

    // Blank/whitespace falls through to the walk.
    let want = root.join(PROJECT_SETTINGS_DIR);
    for blank in [None, Some(""), Some("   ")] {
        assert_eq!(project_settings_dir_from(blank, root).as_deref(), Some(want.as_path()));
    }
}

/// **The sweep-taking twin answers exactly what the override-taking one does** — the override out
/// of the map when the map carries it (blank ignored, the same rule), the walk otherwise — and it
/// looks up [`SETTINGS_DIR_ENV`] and no other name.
#[test]
fn the_sweep_taking_state_dir_takes_the_override_out_of_the_map() {
    let scratch = Scratch::new("sweep");
    let root = scratch.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    let elsewhere = root.join("elsewhere");
    let sweep = |value: &str| {
        std::collections::HashMap::from([(SETTINGS_DIR_ENV.to_string(), value.to_string())])
    };

    assert_eq!(
        project_state_dir_from_env(&sweep(elsewhere.to_str().unwrap()), root),
        Some(elsewhere.join(STATE_SUBDIR)),
    );
    let walked = Some(root.join(PROJECT_SETTINGS_DIR).join(STATE_SUBDIR));
    assert_eq!(project_state_dir_from_env(&sweep("  "), root), walked, "blank is unset");
    assert_eq!(project_state_dir_from_env(&std::collections::HashMap::new(), root), walked);
    // Another variable naming a directory is not the override.
    let other = std::collections::HashMap::from([(
        USER_DATA_DIR_ENV.to_string(),
        elsewhere.to_str().unwrap().to_string(),
    )]);
    assert_eq!(project_state_dir_from_env(&other, root), walked);
}

/// An unusable state root is an `Err` the caller logs, NEVER a panic: here a plain FILE sits
/// where the directory should be (the portable stand-in for a read-only `HOME`).
#[test]
fn an_uncreatable_state_dir_errors_instead_of_panicking() {
    let scratch = Scratch::new("blocked");
    let blocker = scratch.path().join("blocker");
    std::fs::write(&blocker, "not a directory").unwrap();

    let blocked = blocker.join("state");
    assert!(write_path(Some(blocked.as_path()), "thing.json").is_err());
    assert!(write_path(None, "thing.json").is_err(), "no state dir ⇒ Err, not a panic");
}
