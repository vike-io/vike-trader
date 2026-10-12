use super::*;

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
