use super::*;
use vike_model::runs::{MANIFEST_FILE, REPORT_FILE, RunManifest};

/// THE gate for [`path_line`], and it asserts the rendered LINE rather than that the function
/// was called — so removing the normalisation reddens it.
///
/// The input is the exact shape the defect appears in: a forward-slash root, which is how every
/// `VIKE_SETTINGS_DIR` value arrives, with platform-joined children on top — precisely what
/// `crates/vike-desktop/src/main.rs`'s `runs_root: user_data.join(RUNS_SUBDIR)` produces.
#[test]
fn a_rendered_path_never_mixes_separators() {
    let p = std::path::Path::new("C:/Users/the operator/scratch/qa-root").join("user_data").join("runs");
    let line = path_line(&p);
    assert!(
        !(line.contains('/') && line.contains('\\')),
        "a path a human is meant to READ must not change separator halfway: {line}"
    );
    assert!(line.ends_with("runs"), "the path itself is unchanged: {line}");
    // The control, and it is why this test is not vacuous: the RAW rendering must actually
    // exhibit the defect. It can only assert that where the two spellings differ — on a
    // platform whose separator is already `/` this would be a test of the host OS, not of the
    // code, so it says nothing there rather than asserting something it cannot mean.
    if std::path::MAIN_SEPARATOR != '/' {
        let raw = p.display().to_string();
        assert!(
            raw.contains('/') && raw.contains('\\'),
            "the defect must be reproducible, or this gate proves nothing: {raw}"
        );
    }
}
use vike_studio_core::STUDY_RUN_KIND;

/// Write a run directory by hand, exactly as `crates/vike-model/src/runs.rs`'s `write_run`
/// lays one out, so [`run_rows`] is exercised over the real reader.
fn write_run(runs_root: &std::path::Path, manifest: &RunManifest) {
    let dir = runs_root.join(&manifest.run_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(REPORT_FILE), "{}\n").unwrap();
    std::fs::write(dir.join(MANIFEST_FILE), serde_json::to_string_pretty(manifest).unwrap())
        .unwrap();
}

fn manifest(run_id: &str, kind: &str, detail: serde_json::Value) -> RunManifest {
    RunManifest {
        // Spelled `1` rather than imported: this is a TEST FIXTURE standing in for a document
        // on disk, and pinning it to `MANIFEST_SCHEMA` would make it follow a future bump
        // silently — which is the one thing a fixture for old bytes must not do.
        schema: 1,
        run_id: run_id.to_string(),
        kind: kind.to_string(),
        produced_by: "vike-app".to_string(),
        started_at: "2026-08-24T09:15:04Z".to_string(),
        finished_at: "2026-08-24T09:15:16Z".to_string(),
        git_sha: None,
        fingerprint: None,
        config: RunConfig { path: None, name: None },
        detail,
    }
}

/// **The R6 gate this pane owes.** A study run's numbers are in its manifest — and none of them
/// reaches the row the SHARED list renders.
///
/// Asserted over the whole row rather than over a named field, so a future column that
/// reaches into `detail` fails here rather than shipping: the defect
/// `crates/vike-studio-core/src/study_run.rs` nests its metrics to prevent is a number appearing
/// in a column beside a backtest's, and the only structural defence a renderer has is having
/// no such column at all.
#[test]
fn a_study_runs_metrics_never_reach_the_shared_runs_list() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    write_run(
        &runs_root,
        &manifest(
            "1756000000-1-0",
            STUDY_RUN_KIND,
            serde_json::json!({
                "study": "vol",
                // A value chosen to be UNMISTAKABLE in a rendered row and to be no
                // mathematical constant: clippy's `approx_constant` rejects the obvious
                // memorable ones, and a plausible Sharpe would be too easy to match by
                // accident.
                "metrics": [{ "name": "sharpe", "value": 1234.5, "nonfinite": null }],
                "metrics_note": vike_studio_core::STUDY_METRICS_NOTE,
            }),
        ),
    );
    let rows = run_rows(&list_runs(&runs_root));
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let rendered = format!("{row:?}");
    for leak in ["1234", "sharpe", "metrics"] {
        assert!(!rendered.contains(leak), "{leak:?} reached the shared runs list: {rendered}");
    }
    assert_eq!(row.kind, STUDY_RUN_KIND, "...and the kind IS on the row, which is the point");
}

/// A study run and a backtest run sit in ONE list and are told apart by `kind` — the property
/// `crates/vike-studio-core/src/study_run.rs`'s own listing test asserts on disk, asserted here
/// on what the surface actually renders.
#[test]
fn one_list_carries_both_kinds_and_names_each() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    write_run(&runs_root, &manifest("1755000000-1-0", BACKTEST_RUN_KIND, serde_json::json!({})));
    write_run(
        &runs_root,
        &manifest("1756000000-1-0", STUDY_RUN_KIND, serde_json::json!({ "study": "vol" })),
    );
    let kinds: Vec<String> = run_rows(&list_runs(&runs_root)).into_iter().map(|r| r.kind).collect();
    assert_eq!(kinds, [BACKTEST_RUN_KIND, STUDY_RUN_KIND], "one list, two kinds, in id order");
}

/// The two known kinds are drawn apart, and an unrecognised one is neither of them — and is a
/// warning, which `crates/vike-model/src/runs.rs` relies on.
#[test]
fn the_two_known_kinds_are_coloured_apart() {
    let study = kind_status(STUDY_RUN_KIND);
    let backtest = kind_status(BACKTEST_RUN_KIND);
    let unknown = kind_status("sweep-of-the-future");
    assert_ne!(
        study.color(),
        backtest.color(),
        "a reader must not have to read the text to tell them apart"
    );
    assert_ne!(study, unknown);
    assert_ne!(backtest, unknown);
    assert_eq!(unknown, Status::Warning);
}

/// A run that is missing its report still lists, and says so — the row carries the difference
/// `crates/vike-studio-core/src/listing.rs` calls "the result survived" vs "nothing was kept".
#[test]
fn a_run_without_a_report_still_lists_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let m = manifest("1756000000-1-0", STUDY_RUN_KIND, serde_json::json!({}));
    write_run(&runs_root, &m);
    std::fs::remove_file(runs_root.join(&m.run_id).join(REPORT_FILE)).unwrap();
    let rows = run_rows(&list_runs(&runs_root));
    assert!(!rows[0].has_report);
}

/// Both tiers get a badge and the two badges differ — the ONE thing this pane renders about a
/// tier, because everything else about the difference is the runner's business.
#[test]
fn every_tier_has_its_own_badge_and_its_own_tooltip() {
    assert_ne!(tier_label(StudyTier::Rhai), tier_label(StudyTier::Rust));
    assert_ne!(tier_tip(StudyTier::Rhai), tier_tip(StudyTier::Rust));
}

/// With no host there is nothing to plan: the pane cannot invent a runs directory to mint into.
#[test]
fn a_pane_with_no_host_plans_nothing() {
    let pane = ResearchPane::default();
    let dir = tempfile::tempdir().unwrap();
    let store: StoreHandle =
        std::sync::Arc::new(vike_data::DataFusionHist::open(dir.path()).unwrap());
    assert!(pane.plan(store, TsRange::all()).is_none());
}
