use super::*;

/// `user_data/` is a SIBLING of `settings/`, and the whole design rests on it: a user copies
/// or commits `user_data/`, and `settings/` holds live venue keys (`settings/db/vike.db`) that
/// must not ride along. Nesting it would make that impossible to avoid.
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
