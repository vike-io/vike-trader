//! The Research pane over the shell: a study runs on the server only over the Source backend, a
//! finished study asks for the newest runs again, and the study result screen — a saved run's and
//! an unsaved one's — is the central panel, with a study's own numbers and never a backtest's columns.

use super::super::*;
use super::support::{ready, seeded_store, state_new};
use vike_backtesthub_client::study::{
    StudyTierWire, WireListedStudy, WireStudyArtifact, WireStudyMetric,
};
use vike_studio_core::STUDY_METRICS_NOTE;

/// One Rhai study with no recipe.
fn one_study() -> WireStudyListing {
    WireStudyListing {
        root: "/srv/vike/user_data/research/studies".to_string(),
        studies: vec![WireListedStudy {
            name: "counting".to_string(),
            tier: StudyTierWire::Rhai,
            recipes: Vec::new(),
        }],
        diagnostics: Vec::new(),
    }
}

/// A study report as the server answers it: a finite metric, a `NaN` one (printed as the server's
/// `text`, never rounded into a number) and one artifact; saved under `dir`, or not saved and why.
fn report(not_saved: Option<&str>) -> WireStudyReport {
    let saved = not_saved.is_none();
    WireStudyReport {
        run_id: saved.then(|| "1756000000-1-0".to_string()),
        dir: saved.then(|| "/srv/vike/user_data/runs/1756000000-1-0".to_string()),
        manifest: None,
        not_saved: not_saved.map(str::to_string),
        study: "counting".to_string(),
        metrics: vec![
            WireStudyMetric {
                name: "n_bars".to_string(),
                value: Some(400.0),
                nonfinite: None,
                text: "400.000000".to_string(),
            },
            WireStudyMetric {
                name: "oos_sharpe".to_string(),
                value: None,
                nonfinite: Some("NaN".to_string()),
                text: "NaN".to_string(),
            },
        ],
        artifacts: vec![WireStudyArtifact { name: "notes.txt".to_string(), bytes: 5 }],
    }
}

/// Every text of the shell's frame with `report` as the last study run on the study surface.
fn study_screen(report: WireStudyReport) -> String {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.center = StudioCenterView::Study;
    st.study_last = Some(Ok(report));
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1440.0, 960.0))
        .with_max_steps(64)
        .build_ui_state(
            |ui, st: &mut StudioState| {
                // The shell draws icons; their family is bound only by the app's type.
                if vike_ui_theme::harness::type_ready(ui.ctx()) {
                    st.ui(ui);
                }
            },
            st,
        );
    h.run();
    vike_ui_theme::harness::tree_text(&h)
}

/// **The study result surface** and the R6 claim asserted where a reader meets it: the study's own
/// numbers under a heading that names them as a study's, `STUDY_METRICS_NOTE` VERBATIM beside them,
/// the server's path in "Saved to", each artifact with its size — and not one cell of the backtest
/// results surface on screen at the same time.
#[test]
fn a_study_result_renders_its_own_surface_and_never_a_backtests_columns() {
    let text = study_screen(report(None));
    for want in [
        crate::STUDY_RESULT_TITLE,
        "n_bars",
        "400.000000",
        "NaN",
        STUDY_METRICS_NOTE,
        "/srv/vike/user_data/runs/1756000000-1-0",
        "notes.txt  (5 bytes)",
    ] {
        assert!(text.contains(want), "{want:?} must be on the study surface; rendered: {text}");
    }
    for backtest_cell in ["profit_factor", "max_drawdown", "final_equity", "win_rate"] {
        assert!(
            !text.contains(backtest_cell),
            "{backtest_cell:?} is a backtest column and must not share the frame with a study's \
             numbers; rendered: {text}"
        );
    }
}

/// **A run the server could not save is the same screen**: its reason in red, its numbers, and
/// "Saved to" saying it was not — never a run id that does not exist.
#[test]
fn an_unsaved_study_result_shows_its_numbers_and_says_it_was_not_saved() {
    let text = study_screen(report(Some("the disk is full")));
    for want in ["Not saved: the disk is full", "NOT saved — the disk is full", "n_bars", "NaN"] {
        assert!(text.contains(want), "{want:?} must be on the study surface; rendered: {text}");
    }
    assert!(!text.contains("1756000000-1-0"), "an unsaved run has no id; rendered: {text}");
}

/// A study runs on the server only over the Source backend's Write dial: the Named backend lists
/// studies and dispatches nothing, the Source one dispatches the selected study and shows the
/// study surface.
#[test]
fn a_study_runs_only_over_the_source_backend() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.backend = StudioRunBackend::Named { address: "127.0.0.1:1".to_string() };
    st.research.begin_listing("127.0.0.1:1", false);
    st.research.apply_studies(Ok(one_study()));
    st.start_study();
    assert!(st.study_rx.is_none(), "the Named backend has no Write dial to run a study on");

    // `state_new`'s backend: port 1, so the dispatched dial is refused at once.
    st.backend = StudioRunBackend::Source { address: "127.0.0.1:1".to_string() };
    st.research.begin_listing("127.0.0.1:1", true);
    st.research.apply_studies(Ok(one_study()));
    st.start_study();
    assert!(st.study_rx.is_some(), "the Source backend runs the selected study on the server");
    assert_eq!(st.center, StudioCenterView::Study);
}

/// A finished study's run belongs in the list it was saved into: `poll` asks for the newest page of
/// runs again when the report lands.
#[test]
fn a_finished_study_asks_for_the_newest_runs_again() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.study_rx = Some(ready(Ok(report(None))));
    st.poll();
    assert!(matches!(st.study_last, Some(Ok(_))), "the report is the last study run");
    assert!(st.research_runs_rx.is_some(), "...and page 0 of the runs is asked again");
}
