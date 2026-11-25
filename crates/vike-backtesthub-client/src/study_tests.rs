use super::*;

/// An OLDER server's saved report (no `study`, no `artifacts`) still reads, so a new desktop shows an old
/// server's run.
#[test]
fn an_older_servers_report_still_reads() {
    let old = r#"{"run_id":"1763424000-1","dir":"/srv/runs/1763424000-1","manifest":{"kind":"study"},"metrics":[{"name":"sharpe","value":null,"nonfinite":"NaN","text":"NaN"}]}"#;
    let report: WireStudyReport = serde_json::from_str(old).expect("an old report reads");
    assert_eq!(report.run_id.as_deref(), Some("1763424000-1"));
    assert!(report.not_saved.is_none() && report.artifacts.is_empty());
    assert_eq!(report.metrics[0].text, "NaN");
}

fn row(i: usize) -> WireRunRow {
    WireRunRow {
        run_id: format!("{i}-1"),
        kind: "study".to_string(),
        produced_by: "vike-backend backtesthub".to_string(),
        started_at: "2026-11-19T00:00:00Z".to_string(),
        has_report: true,
    }
}

/// Page 0 is the NEWEST runs, page 1 the ones before them, each in the scan's order; a page past the
/// oldest run is empty, and the size is held at the maximum.
#[test]
fn a_runs_page_counts_back_from_the_newest() {
    let runs: Vec<WireRunRow> = (0..5).map(row).collect();
    let ids = |page: &WireRunPage| page.runs.iter().map(|r| r.run_id.clone()).collect::<Vec<_>>();
    let newest = WireRunPage::of("/srv/runs".to_string(), &runs, &[], 0, 2);
    assert_eq!((newest.total, newest.page_size), (5, 2));
    assert_eq!(ids(&newest), ["3-1", "4-1"]);
    assert_eq!(ids(&WireRunPage::of(String::new(), &runs, &[], 2, 2)), ["0-1"]);
    assert!(WireRunPage::of(String::new(), &runs, &[], 3, 2).runs.is_empty());
    assert_eq!(WireRunPage::of(String::new(), &runs, &[], 0, 0).page_size, RUNS_PAGE_SIZE);
    assert_eq!(WireRunPage::of(String::new(), &runs, &[], 0, 99_999).page_size, RUNS_PAGE_SIZE_MAX);
}

/// The diagnostics are cut to [`RUN_DIAGNOSTICS_MAX`] and counted whole.
#[test]
fn a_runs_page_cuts_its_diagnostics_and_counts_them() {
    let diagnostics: Vec<String> =
        (0..RUN_DIAGNOSTICS_MAX + 7).map(|i| format!("bad {i}")).collect();
    let page = WireRunPage::of(String::new(), &[], &diagnostics, 0, 0);
    assert_eq!(page.diagnostics.len(), RUN_DIAGNOSTICS_MAX);
    assert_eq!(page.diagnostic_total, RUN_DIAGNOSTICS_MAX + 7);
}

/// A binary file crosses as base64 and decodes to its bytes.
#[test]
fn a_run_file_crosses_as_base64_and_decodes_to_its_bytes() {
    let bytes = [0u8, 159, 146, 150, b'\n'];
    let artifact = WireRunArtifact::of("1-1", "model.onnx", 0, 5, &bytes);
    assert_eq!(artifact.data().expect("decodes"), bytes);
}

/// An unsaved report writes no `run_id`, `dir` or `manifest` key at all, so no reader can take it for a
/// saved run.
#[test]
fn an_unsaved_report_carries_no_run() {
    let report = WireStudyReport {
        not_saved: Some("disk full".to_string()),
        study: "cohort".to_string(),
        ..WireStudyReport::default()
    };
    let json = serde_json::to_string(&report).expect("encodes");
    assert_eq!(json, r#"{"not_saved":"disk full","study":"cohort","metrics":[],"artifacts":[]}"#);
}
