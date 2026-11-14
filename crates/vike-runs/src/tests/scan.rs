//! The run scan's rules (`crate::scan`), on real directories. Moved here with the rules from the two
//! twins that used to restate them (the Studio's listing, the CLI's `runs` scan).

use std::path::{Path, PathBuf};

use vike_model::DiagnosticSeverity;

use crate::{MANIFEST_FILE, REPORT_FILE, ScanDiagnostic, scan_runs};

fn write_file(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a file has a parent")).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A manifest exactly as `crate::write_run` lays one out, written as TEXT on purpose: these tests
/// pin the ON-DISK document a scan has to survive, not a round trip through the writer's own struct.
fn manifest_json(run_id: &str, kind: &str) -> String {
    format!(
        r#"{{
  "run_id": "{run_id}",
  "kind": "{kind}",
  "produced_by": "{kind}",
  "started_at": "2026-08-24T09:15:04Z",
  "finished_at": "2026-08-24T09:15:16Z",
  "git_sha": null,
  "config": {{ "path": "profiles/sma.toml", "name": "sma cross" }},
  "detail": {{ "anything": "the producer's own business" }}
}}
"#
    )
}

/// A finished run directory: the report first, the manifest last — the order the writer uses.
fn finished_run(root: &Path, run_id: &str, kind: &str) -> PathBuf {
    let dir = root.join(run_id);
    write_file(&dir.join(REPORT_FILE), "{ \"sharpe\": 1.25 }\n");
    write_file(&dir.join(MANIFEST_FILE), &manifest_json(run_id, kind));
    dir
}

/// A fresh install has no `user_data/` at all, so an absent runs root is the ORDINARY state and
/// must be silent.
#[test]
fn an_absent_runs_root_scans_nothing_and_says_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("user_data").join("runs");
    let scan = scan_runs(&root);
    assert!(scan.runs.is_empty());
    assert!(scan.diagnostics.is_empty(), "absence is not a failure");
}

/// A root that EXISTS and cannot be listed is a permissions bug wearing the "not configured yet"
/// answer, so it is its own named row.
#[test]
fn a_runs_root_that_exists_and_cannot_be_read_is_its_own_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("runs");
    // A FILE where the directory belongs: `read_dir` fails with something that is NOT `NotFound`.
    write_file(&root, "not a directory");

    let scan = scan_runs(&root);

    assert!(scan.runs.is_empty());
    match &scan.diagnostics[..] {
        [ScanDiagnostic::RootUnreadable { root: reported, error }] => {
            assert_eq!(reported, &root);
            assert!(!error.is_empty(), "the OS's own words must be carried through");
        }
        other => panic!("expected one RootUnreadable, got {other:?}"),
    }
    assert_eq!(scan.diagnostics[0].severity(), DiagnosticSeverity::Error);
}

/// THE property the common manifest exists for: one scan reads every KIND of run off the top-level
/// fields, with no parser per kind — including a kind this crate has never heard of.
#[test]
fn every_kind_of_run_scans_off_the_common_fields_with_no_per_kind_parser() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    finished_run(root, "1756000000-1-0", "backtest");
    finished_run(root, "1756000100-1-0", "a-kind-invented-next-year");

    let scan = scan_runs(root);

    assert!(scan.diagnostics.is_empty(), "unexpected: {:?}", scan.diagnostics);
    assert_eq!(scan.runs.len(), 2);
    let first = &scan.runs[0];
    assert_eq!(first.run_id, "1756000000-1-0");
    assert_eq!(first.manifest.kind, "backtest");
    assert_eq!(first.manifest.config.name.as_deref(), Some("sma cross"));
    assert_eq!(first.report, Some(root.join("1756000000-1-0").join(REPORT_FILE)));
    assert_eq!(scan.runs[1].manifest.kind, "a-kind-invented-next-year");
}

/// A corrupt manifest is a NAMED row, never a dropped run.
#[test]
fn a_run_whose_manifest_is_corrupt_is_reported_rather_than_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    finished_run(root, "1756000000-1-0", "backtest");
    write_file(&root.join("1756000100-1-0").join(MANIFEST_FILE), "{ not json");

    let scan = scan_runs(root);

    assert_eq!(scan.runs.len(), 1, "the good run still scans");
    match &scan.diagnostics[..] {
        [ScanDiagnostic::RunUnreadable { run_id, path, error }] => {
            assert_eq!(run_id, "1756000100-1-0");
            assert!(path.ends_with(MANIFEST_FILE), "{path:?}");
            assert!(!error.is_empty(), "the parser's own words must be carried through");
        }
        other => panic!("expected one RunUnreadable, got {other:?}"),
    }
    assert_eq!(scan.diagnostics[0].severity(), DiagnosticSeverity::Error);
}

/// The manifest is written LAST, so a directory without one is a run in flight or one that stopped
/// mid-write: a WARNING, and the row says whether the report survived.
#[test]
fn a_run_directory_with_no_manifest_is_reported_as_unfinished_not_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(&root.join("1756000100-1-0").join(REPORT_FILE), "{}\n");
    std::fs::create_dir_all(root.join("1756000200-1-0")).unwrap();

    let scan = scan_runs(root);

    assert!(scan.runs.is_empty(), "a run with no manifest produces no row");
    match &scan.diagnostics[..] {
        [
            ScanDiagnostic::RunUnfinished { run_id: a, report: true, .. },
            ScanDiagnostic::RunUnfinished { run_id: b, report: false, .. },
        ] => {
            assert_eq!(a, "1756000100-1-0");
            assert_eq!(b, "1756000200-1-0");
        }
        other => panic!("expected two RunUnfinished rows, got {other:?}"),
    }
    assert_eq!(scan.diagnostics[0].severity(), DiagnosticSeverity::Warning);
    assert!(scan.diagnostics[0].to_string().contains(REPORT_FILE), "{}", scan.diagnostics[0]);
}

/// The DIRECTORY is the address: a manifest naming another id still scans under its directory,
/// with the disagreement reported.
#[test]
fn a_manifest_that_names_a_different_run_id_than_its_directory_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_file(
        &root.join("1756000000-1-0").join(MANIFEST_FILE),
        &manifest_json("some-other-run", "backtest"),
    );

    let scan = scan_runs(root);

    assert_eq!(scan.runs.len(), 1);
    assert_eq!(scan.runs[0].run_id, "1756000000-1-0", "the directory is the address");
    match &scan.diagnostics[..] {
        [ScanDiagnostic::RunIdMismatch { run_id, declared, .. }] => {
            assert_eq!(run_id, "1756000000-1-0");
            assert_eq!(declared, "some-other-run");
        }
        other => panic!("expected one RunIdMismatch, got {other:?}"),
    }
    assert_eq!(scan.diagnostics[0].severity(), DiagnosticSeverity::Warning);
}

/// Directory order is whatever the OS feels like; the scan's order is a RULE, by directory name.
#[test]
fn runs_are_scanned_in_directory_name_order_whatever_the_filesystem_says() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for id in ["1756000200-1-0", "1756000000-1-0", "1756000100-1-0"] {
        finished_run(root, id, "backtest");
    }
    let ids: Vec<String> = scan_runs(root).runs.into_iter().map(|r| r.run_id).collect();
    assert_eq!(ids, vec!["1756000000-1-0", "1756000100-1-0", "1756000200-1-0"]);
}

/// A run IS a directory. Dot-entries are tool droppings and a loose file was written by nothing in
/// this workspace — neither is a run that went missing, so neither earns a row.
#[test]
fn dot_entries_and_loose_files_in_the_runs_root_are_not_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    finished_run(root, "1756000000-1-0", "backtest");
    write_file(&root.join(".DS_Store"), "junk");
    write_file(&root.join("README.md"), "# my runs\n");
    std::fs::create_dir_all(root.join(".git")).unwrap();

    let scan = scan_runs(root);

    assert_eq!(scan.runs.len(), 1);
    assert!(scan.diagnostics.is_empty(), "unexpected: {:?}", scan.diagnostics);
}

/// ⚠ THE SEARCH-PARENT LAYOUT DECISION: a parameter search keeps its trials as LINES in
/// `trials.jsonl` inside its own run directory, so three trials plus the search's extra documents
/// are ONE row and ZERO diagnostics (`crates/vike-backtest/src/trial_ledger.rs`'s `TRIALS_FILE`
/// carries the argument).
#[test]
fn a_search_parent_lists_as_one_row_whatever_it_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let runs = tmp.path().join("runs");
    let dir = runs.join("1756000000-77-0");
    write_file(&dir.join(MANIFEST_FILE), &manifest_json("1756000000-77-0", "search"));
    write_file(&dir.join(REPORT_FILE), "{}\n");
    write_file(&dir.join("search.json"), "{}\n");
    write_file(&dir.join("trials.jsonl"), "{\"n\":0}\n{\"n\":1}\n{\"n\":2}\n");
    let scan = scan_runs(&runs);
    assert_eq!(scan.runs.len(), 1, "one row, not one per trial: {:?}", scan.runs);
    assert!(scan.diagnostics.is_empty(), "{:?}", scan.diagnostics);
}

/// …and the measurement that makes the row above worth having: sibling directories named
/// `<id>#<n>` — the spelling the search layout rejected — DO become rows, one per trial.
#[test]
fn sibling_child_directories_would_each_become_a_row() {
    let tmp = tempfile::tempdir().unwrap();
    let siblings = tmp.path().join("siblings");
    for name in ["1756000000-77-0", "1756000000-77-0#0", "1756000000-77-0#1"] {
        write_file(&siblings.join(name).join(MANIFEST_FILE), &manifest_json(name, "search"));
    }
    assert_eq!(scan_runs(&siblings).runs.len(), 3, "the cost the file-per-search layout avoids");
}
