use super::*;

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
