use super::*;
use std::path::Path;
use std::sync::Arc;
use vike_study_runner::study_run::{StudyPersistError, StudyRunRequest, run_study_with};
use vike_user_research::{StudyContext, StudyError};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

/// A server's study folder: a Rhai study with two recipes, a Rust study with none.
fn studies() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a temp dir");
    write(&root.path().join("rhai/fixture/fixture.rhai"), "fn run(ctx, params) { outcome() }\n");
    write(&root.path().join("rhai/fixture/wide.toml"), "lookback = 50\n");
    write(&root.path().join("rhai/fixture/baseline.toml"), "lookback = 20\n");
    write(&root.path().join("rust/cohort/cohort.rs"), "//! A study.\n");
    root
}

/// `ListStudies`: each study by name and tier, with its recipe FILE NAMES in name order.
#[test]
fn the_study_list_names_each_study_its_tier_and_its_recipe_files() {
    let root = studies();
    let listing = study_listing(root.path());
    assert_eq!(listing.root, root.path().display().to_string());
    let rows: Vec<(&str, StudyTierWire, Vec<&str>)> = listing
        .studies
        .iter()
        .map(|s| (s.name.as_str(), s.tier, s.recipes.iter().map(String::as_str).collect()))
        .collect();
    assert_eq!(
        rows,
        [
            ("fixture", StudyTierWire::Rhai, vec!["baseline.toml", "wide.toml"]),
            ("cohort", StudyTierWire::Rust, vec![]),
        ]
    );
    assert!(listing.diagnostics.is_empty(), "{:?}", listing.diagnostics);
}

/// ⚠ A study is found only by THIS server's listing, in its own tier (no tier is the Rust tier), so a
/// name that is a path, or a study of the other tier, runs nothing.
#[test]
fn a_study_is_found_only_by_the_listing_and_in_its_own_tier() {
    let root = studies();
    let rhai = listed_study(root.path(), "fixture", Some(StudyTierWire::Rhai)).expect("listed");
    assert_eq!((rhai.tier, rhai.dir), (StudyTier::Rhai, root.path().join("rhai/fixture")));
    assert_eq!(listed_study(root.path(), "cohort", None).expect("listed").tier, StudyTier::Rust);
    for (name, tier) in [
        ("fixture", None),
        ("cohort", Some(StudyTierWire::Rhai)),
        ("../rust/cohort", Some(StudyTierWire::Rust)),
        ("", None),
    ] {
        let refused = listed_study(root.path(), name, tier).expect_err(name);
        assert!(refused.contains("nothing was run"), "{refused}");
    }
}

/// A recipe FILE is read only when the study's folder lists it, and never beside recipe text.
#[test]
fn a_recipe_file_is_read_only_when_the_folder_lists_it() {
    let root = studies();
    let study = listed_study(root.path(), "fixture", Some(StudyTierWire::Rhai)).expect("listed");
    let (params, config) = recipe_file(&study, "baseline.toml", "").expect("listed");
    assert_eq!(params.get("lookback").and_then(toml::Value::as_integer), Some(20));
    assert_eq!(config.name.as_deref(), Some("baseline"));
    let missing = recipe_file(&study, "missing.toml", "").expect_err("not listed");
    assert!(missing.contains("baseline.toml"), "it names the recipes there are: {missing}");
    assert!(recipe_file(&study, "../../rust/cohort/cohort.rs", "").is_err());
    let both = recipe_file(&study, "baseline.toml", "x = 1\n").expect_err("two recipes");
    assert!(both.contains("one run, one recipe"), "{both}");
}

/// An EMPTY bound is unbounded on that side; a window that ends before it starts is refused.
#[test]
fn an_empty_bound_is_unbounded_and_a_backwards_window_is_refused() {
    assert_eq!(study_window("", ""), Ok(TimestampRange::all()));
    assert_eq!(
        study_window("", "1785906000"),
        Ok(TimestampRange { start: None, end: Some(1_785_906_000_000) })
    );
    assert!(study_window("2026-01-02", "2026-01-01").expect_err("backwards").contains("after"));
    assert!(study_window("yesterday", "").expect_err("no grammar").contains("YYYY-MM-DD"));
}

/// A finished study run under `runs`, left by the one persistence path, with the artifact
/// `fold_scores.csv`; its id.
fn a_saved_study_run(runs: &Path, scratch: &Path) -> String {
    let params = toml::Value::Table(toml::Table::new());
    let entry = |_ctx: &StudyContext, _params: &toml::Value| -> Result<StudyOutcome, StudyError> {
        let mut outcome = StudyOutcome::new();
        outcome.metric("sharpe", 1.0)?;
        outcome.artifact("fold_scores.csv", "fold,score\n0,0.5\n")?;
        Ok(outcome)
    };
    let run = run_study_with(
        &entry,
        StudyRunRequest {
            study: "fixture",
            params: &params,
            store: Arc::new(vike_data::HistoryStoreMemory::new()),
            learner: None,
            sim: None,
            window: TimestampRange::all(),
            scratch: scratch.to_path_buf(),
            runs_root: runs,
            produced_by: "study_files_tests",
            git_sha: None,
            config: RunConfig { path: None, name: None },
            clock: &vike_model::ClockLive,
        },
    )
    .expect("the run is saved");
    run.run_id
}

/// `ListRuns`: a saved study run is a row of the common fields.
#[test]
fn the_runs_page_lists_a_saved_study_run() {
    let (runs, scratch) = (tempfile::tempdir().expect("dir"), tempfile::tempdir().expect("dir"));
    let id = a_saved_study_run(runs.path(), scratch.path());
    let page = runs_page(runs.path(), 0, 0);
    assert_eq!((page.total, page.root), (1, runs.path().display().to_string()));
    let row = &page.runs[0];
    assert_eq!(
        (row.run_id.as_str(), row.kind.as_str(), row.has_report),
        (id.as_str(), "study", true)
    );
}

/// `ReadRunArtifact`: a declared artifact reads in pages; the run's own documents, a path out of the run
/// and a run id that is a path are refused.
#[test]
fn a_study_runs_artifact_reads_in_pages_and_nothing_else_does() {
    let (runs, scratch) = (tempfile::tempdir().expect("dir"), tempfile::tempdir().expect("dir"));
    let id = a_saved_study_run(runs.path(), scratch.path());
    let whole = run_artifact(runs.path(), &id, "fold_scores.csv", 0, 0).expect("declared");
    assert_eq!((whole.total, whole.data().expect("base64")), (17, b"fold,score\n0,0.5\n".to_vec()));
    let part = run_artifact(runs.path(), &id, "fold_scores.csv", 11, 3).expect("declared");
    assert_eq!((part.offset, part.data().expect("base64")), (11, b"0,0".to_vec()));
    for (run, file) in [
        (id.as_str(), vike_runs::MANIFEST_FILE),
        (id.as_str(), "../fold_scores.csv"),
        ("..", "fold_scores.csv"),
        ("no-such-run", "fold_scores.csv"),
    ] {
        assert!(run_artifact(runs.path(), run, file, 0, 0).is_err(), "{run} {file}");
    }
}

/// ⚠ A declared artifact that is a LINK is never followed out of the run.
#[cfg(unix)]
#[test]
fn an_artifact_that_is_a_link_is_never_read() {
    let (runs, scratch) = (tempfile::tempdir().expect("dir"), tempfile::tempdir().expect("dir"));
    let id = a_saved_study_run(runs.path(), scratch.path());
    let artifact = runs.path().join(&id).join("fold_scores.csv");
    let outside = scratch.path().join("secret.txt");
    std::fs::write(&outside, "secret").expect("write");
    std::fs::remove_file(&artifact).expect("remove");
    std::os::unix::fs::symlink(&outside, &artifact).expect("link");
    let refused = run_artifact(runs.path(), &id, "fold_scores.csv", 0, 0).expect_err("a link");
    assert!(refused.contains("not a regular file"), "{refused}");
}

/// A result the runner could not save: a NaN metric, an ordinary one and an artifact.
fn not_persisted() -> StudyRunError {
    let mut outcome = StudyOutcome::new();
    outcome.metric("sharpe", f64::NAN).expect("a metric");
    outcome.metric("hit_rate", 0.5).expect("a metric");
    outcome.artifact("fold_scores.csv", "fold,score\n0,0.5\n").expect("an artifact");
    StudyRunError::NotPersisted {
        outcome: Box::new(outcome),
        why: StudyPersistError::Artifact {
            path: "/srv/runs/1-1/fold_scores.csv".into(),
            why: "No space left on device".to_string(),
        },
    }
}

fn request(report_unsaved_result: bool) -> StudyRunRequestWire {
    StudyRunRequestWire {
        study: "cohort".to_string(),
        report_unsaved_result,
        ..StudyRunRequestWire::default()
    }
}

/// ⚠ A client that ASKED gets the numbers of a run that could not be saved, with the reason and no run.
#[test]
fn an_unsaved_result_answers_its_numbers_to_a_client_that_asked() {
    let report = study_report(&request(true), Err(not_persisted())).expect("a report");
    assert_eq!((report.run_id, report.dir, report.manifest), (None, None, None));
    let why = report.not_saved.expect("the reason");
    assert!(why.contains("No space left on device"), "{why}");
    assert_eq!(report.study, "cohort");
    let metrics: Vec<(&str, Option<f64>, Option<&str>, &str)> = report
        .metrics
        .iter()
        .map(|m| (m.name.as_str(), m.value, m.nonfinite.as_deref(), m.text.as_str()))
        .collect();
    assert_eq!(
        metrics,
        [("sharpe", None, Some("NaN"), "NaN"), ("hit_rate", Some(0.5), None, "0.500000")]
    );
    assert_eq!(
        report.artifacts,
        [WireStudyArtifact { name: "fold_scores.csv".to_string(), bytes: 17 }]
    );
}

/// ⚠ …and every OTHER client (an older `vike-cli`) gets today's error, the runner's own sentence, so it
/// can never read an unsaved result as a saved run.
#[test]
fn an_unsaved_result_is_an_error_to_a_client_that_did_not_ask() {
    let sentence = not_persisted().to_string();
    let refused = study_report(&request(false), Err(not_persisted())).expect_err("an error");
    assert_eq!(refused, sentence);
    assert!(refused.contains("NOT saved (2 metric(s) and 1 artifact(s) are still in hand)"));
}

/// Any other refusal is the runner's sentence, asked or not.
#[test]
fn a_refusal_is_the_runners_sentence_whatever_was_asked() {
    let refused = StudyRunError::NoSandbox { name: "cohort".to_string() };
    let sentence = refused.to_string();
    assert_eq!(study_report(&request(true), Err(refused)), Err(sentence));
}
