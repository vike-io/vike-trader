use super::*;
use vike_runs::RunConfig;

/// A server's runs root, a path on ITS box: the pane shows it verbatim.
const SERVER_RUNS: &str = "/srv/vike/user_data/runs";

fn row(run_id: &str, kind: &str) -> WireRunRow {
    WireRunRow {
        run_id: run_id.to_string(),
        kind: kind.to_string(),
        produced_by: "vike-backend".to_string(),
        started_at: "2026-08-24T09:15:04Z".to_string(),
        has_report: true,
    }
}

/// Page `page` of a server holding `total` runs, its rows in scan order (oldest first).
fn page(page: usize, total: usize, runs: Vec<WireRunRow>) -> WireRunPage {
    WireRunPage {
        root: SERVER_RUNS.to_string(),
        total,
        page,
        page_size: 2,
        runs,
        diagnostics: Vec::new(),
        diagnostic_total: 0,
    }
}

/// Two studies, one per tier; the first has two recipe files.
fn listing() -> WireStudyListing {
    WireStudyListing {
        root: "/srv/vike/user_data/research/studies".to_string(),
        studies: vec![
            WireListedStudy {
                name: "vol".to_string(),
                tier: StudyTierWire::Rhai,
                recipes: vec!["fast.toml".to_string(), "slow.toml".to_string()],
            },
            WireListedStudy {
                name: "regime".to_string(),
                tier: StudyTierWire::Rust,
                recipes: Vec::new(),
            },
        ],
        diagnostics: Vec::new(),
    }
}

/// A pane that asked a server (`compute`: over the Source backend) and holds [`listing`].
fn listed(compute: bool) -> StudioResearchPane {
    let mut pane = StudioResearchPane::default();
    pane.begin_listing("<host>:7880", compute);
    pane.apply_studies(Ok(listing()));
    pane
}

fn ids(pane: &StudioResearchPane) -> Vec<&str> {
    pane.runs().iter().map(|r| r.run_id.as_str()).collect()
}

/// **The R6 gate this pane owes.** A study run's numbers are in its manifest — and none of them
/// reaches the row the SHARED list renders, even when a server page carries them beside a row.
///
/// Asserted over the whole row rather than over a named field, so a future column that reaches into
/// a run's numbers fails here rather than shipping: the defect `crates/vike-studio-core/src/study_run.rs`
/// nests its metrics to prevent is a number appearing in a column beside a backtest's, and the only
/// structural defence a renderer has is a row type with no such column at all.
#[test]
fn a_study_runs_metrics_never_reach_the_shared_runs_list() {
    let page: WireRunPage = serde_json::from_value(serde_json::json!({
        "root": SERVER_RUNS,
        "total": 1,
        "page": 0,
        "page_size": 200,
        "runs": [{
            "run_id": "1756000000-1-0",
            "kind": STUDY_RUN_KIND,
            "produced_by": "vike-backend",
            "started_at": "2026-08-24T09:15:04Z",
            "has_report": true,
            // A value chosen to be UNMISTAKABLE in a rendered row and to be no mathematical
            // constant, smuggled in the two ways a server could: nested and top-level.
            "detail": { "metrics": [{ "name": "sharpe", "value": 1234.5, "nonfinite": null }] },
            "sharpe": 1234.5,
        }],
    }))
    .expect("a server page");
    let mut pane = listed(true);
    pane.fetching_runs(0);
    pane.apply_runs(Ok(page));
    let rows = pane.runs();
    assert_eq!(rows.len(), 1);
    let rendered = format!("{:?}", rows[0]);
    for leak in ["1234", "sharpe", "metrics"] {
        assert!(!rendered.contains(leak), "{leak:?} reached the shared runs list: {rendered}");
    }
    assert_eq!(rows[0].kind, STUDY_RUN_KIND, "...and the kind IS on the row, which is the point");
}

/// The list reads newest first; "Older runs" adds the next page BEHIND it, a refused older page
/// leaves the rows in place, and page 0 again (a finished study) replaces the list.
#[test]
fn pages_read_newest_first_and_an_older_page_joins_behind() {
    let mut pane = listed(true);
    pane.fetching_runs(0);
    pane.apply_runs(Ok(page(0, 4, vec![row("3", STUDY_RUN_KIND), row("4", BACKTEST_RUN_KIND)])));
    assert_eq!(ids(&pane), ["4", "3"], "a page arrives oldest first and reads newest first");
    assert_eq!(pane.next_runs_page(), 1);

    pane.fetching_runs(1);
    pane.apply_runs(Ok(page(1, 4, vec![row("1", STUDY_RUN_KIND), row("2", STUDY_RUN_KIND)])));
    assert_eq!(ids(&pane), ["4", "3", "2", "1"], "an older page joins behind the newer one");

    pane.fetching_runs(2);
    pane.apply_runs(Err("the backtest server went away".to_string()));
    assert_eq!(ids(&pane), ["4", "3", "2", "1"], "a refused older page keeps the rows");
    assert!(
        matches!(&pane.runs, Some(Ok(runs)) if runs.older_error.is_some()),
        "...and says why under them"
    );

    pane.fetching_runs(0);
    pane.apply_runs(Ok(page(0, 5, vec![row("4", STUDY_RUN_KIND), row("5", STUDY_RUN_KIND)])));
    assert_eq!(ids(&pane), ["5", "4"], "page 0 replaces the list");
}

/// The two known kinds are drawn apart, and an unrecognised one is neither of them — and is a
/// warning, which `crates/vike-runs/src/lib.rs` relies on.
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
    assert_eq!(unknown, ThemeStatus::Warning);
}

/// Both tiers get a badge and the two badges differ — the ONE thing this pane renders about a
/// tier, because everything else about the difference is the server's business.
#[test]
fn every_tier_has_its_own_badge_and_its_own_tooltip() {
    assert_ne!(tier_label(StudyTierWire::Rhai), tier_label(StudyTierWire::Rust));
    assert_ne!(tier_tip(StudyTierWire::Rhai), tier_tip(StudyTierWire::Rust));
}

/// The request names the selected study, its tier and the picked recipe by FILE NAME (the server
/// reads it; the PC sends no text), the window as unix SECONDS covering the picked milliseconds,
/// and asks for an unsaved result's numbers. Index 0 is no recipe; the whole store is two empty
/// bounds.
#[test]
fn the_request_names_the_study_its_tier_its_recipe_file_and_the_window_in_seconds() {
    let mut pane = listed(true);
    pane.recipe_index = 2;
    let window = TimestampRange { start: Some(1_767_225_600_000), end: Some(1_767_249_540_500) };
    let request = pane.request(window).expect("a study is selected");
    assert_eq!(request.study, "vol");
    assert_eq!(request.tier, Some(StudyTierWire::Rhai));
    assert_eq!(request.recipe.as_deref(), Some("slow.toml"));
    assert_eq!((request.from.as_str(), request.to.as_str()), ("1767225600", "1767249541"));
    assert!(request.report_unsaved_result, "an unsaved run must still answer its numbers");
    assert!(request.recipe_toml.is_empty(), "the server reads the recipe; the PC sends no text");

    pane.study_index = 1;
    pane.recipe_index = 0;
    let request = pane.request(TimestampRange::all()).expect("a study is selected");
    assert_eq!((request.study.as_str(), request.tier), ("regime", Some(StudyTierWire::Rust)));
    assert_eq!(request.recipe, None);
    assert_eq!((request.from.as_str(), request.to.as_str()), ("", ""));
}

/// Run study is armed only on the Source backend with a study selected and none running: the Named
/// backend says why in its own words, and a refused listing (an older server, an unreachable one)
/// leaves nothing to run.
#[test]
fn run_study_is_armed_only_with_a_write_dial_and_a_listed_study() {
    assert!(StudioResearchPane::default().request(TimestampRange::all()).is_none());
    assert_eq!(listed(true).run_disabled_reason(false), None);
    assert_eq!(listed(true).run_disabled_reason(true), Some(RUN_DISABLED_TIP));
    assert_eq!(listed(false).run_disabled_reason(false), Some(STUDY_NEEDS_COMPUTE));

    let mut refused = StudioResearchPane::default();
    refused.begin_listing("<host>:7880", true);
    refused.apply_studies(Err("does not advertise `study_files`: deploy the server".to_string()));
    assert!(refused.request(TimestampRange::all()).is_none());
    assert_eq!(refused.run_disabled_reason(false), Some(RUN_DISABLED_TIP));
}

/// The lists are asked once, on the first open, and again after a switch between Source and Named.
#[test]
fn the_lists_are_asked_on_first_open_and_after_a_backend_switch() {
    let mut pane = StudioResearchPane::default();
    assert!(pane.needs_listing(true) && pane.needs_listing(false), "nothing asked yet");
    pane.begin_listing("<host>:7880", true);
    assert!(!pane.needs_listing(true), "asked: the next frame dials nothing");
    assert!(pane.needs_listing(false), "Named may not read what Source listed");
}

/// A saved run's facts, off its manifest: the server's path in "Saved to".
#[test]
fn a_saved_runs_facts_come_from_its_manifest() {
    let manifest = RunManifest {
        // Spelled `1` rather than imported: a fixture for a document on disk must not follow a
        // future schema bump silently.
        schema: 1,
        run_id: "1756000000-1-0".to_string(),
        kind: STUDY_RUN_KIND.to_string(),
        produced_by: "vike-backend".to_string(),
        started_at: "2026-08-24T09:15:04Z".to_string(),
        finished_at: "2026-08-24T09:15:16Z".to_string(),
        git_sha: None,
        strategy_sdk: None,
        fingerprint: None,
        config: RunConfig { path: None, name: Some("vol".to_string()) },
        detail: serde_json::json!({
            "study": "vol",
            "window": { "start": 1_767_225_600_000_i64, "end": null },
            "learner": false,
            "metrics": [],
            "artifacts": [],
            "metrics_note": STUDY_METRICS_NOTE,
        }),
        kept_rows: Vec::new(),
    };
    let report = WireStudyReport {
        run_id: Some("1756000000-1-0".to_string()),
        dir: Some(format!("{SERVER_RUNS}/1756000000-1-0")),
        manifest: Some(serde_json::to_value(&manifest).expect("a manifest")),
        study: "vol".to_string(),
        ..WireStudyReport::default()
    };
    let rows = fact_rows(&report, Some(&manifest));
    let fact = |k: &str| rows.iter().find(|(name, _)| *name == k).map(|(_, v)| v.as_str());
    assert_eq!(fact("Study"), Some("vol"));
    assert_eq!(fact("Produced by"), Some("vike-backend"));
    assert_eq!(fact("Window"), Some("1767225600000 … unbounded"));
    assert_eq!(fact("Learner"), Some("none"));
    assert_eq!(fact("Saved to"), Some("/srv/vike/user_data/runs/1756000000-1-0"));
}

/// A run the server could not save has no manifest: its facts are its study and why "Saved to" is
/// empty, never a blank row or a path that does not exist.
#[test]
fn an_unsaved_run_says_it_was_not_saved_and_why() {
    let report = WireStudyReport {
        not_saved: Some("the disk is full".to_string()),
        study: "vol".to_string(),
        ..WireStudyReport::default()
    };
    assert_eq!(
        fact_rows(&report, None),
        [("Study", "vol".to_string()), ("Saved to", "NOT saved — the disk is full".to_string())]
    );
}

/// An artifact's first page is shown as text when it is UTF-8 (a page cut inside a character is
/// still text, up to it), as `binary` otherwise, and says when it is not the whole file.
#[test]
fn an_artifact_page_reads_as_text_or_binary() {
    let whole = WireRunArtifact::of("1756000000-1-0", "notes.txt", 0, 5, b"hello");
    assert_eq!(artifact_text(&whole), Ok("hello".to_string()));

    let cut = WireRunArtifact::of("1756000000-1-0", "notes.txt", 0, 3, &"aé".as_bytes()[..2]);
    assert_eq!(artifact_text(&cut), Ok("a\n… the first 2 of 3 bytes".to_string()));

    let binary = WireRunArtifact::of("1756000000-1-0", "model.onnx", 0, 3, &[0xff, 0x00, 0x01]);
    assert_eq!(artifact_text(&binary), Ok("binary (3 bytes)".to_string()));
}
