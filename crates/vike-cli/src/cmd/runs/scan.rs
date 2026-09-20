//! The runs directory, as a list — `<project>/user_data/runs`, read through the COMMON manifest.
//!
//! # Why this is a SECOND scan, and what keeps it honest
//!
//! `crates/vike-studio-core/src/listing.rs`'s `list_runs` is the twin and it got here first. It
//! cannot be shared: it sits at layer 55 behind `vike-backtest[hist-replay]`, `vike-ml` and `rhai`,
//! and this crate's whole identity is being light and DataFusion-free. What IS shared is the part
//! that matters — the manifest itself, `vike_model::runs`, which came down to the bottom of the
//! graph precisely so both readers parse one schema.
//!
//! So the rules are restated here deliberately, and the tests below pin the three that two scans
//! could disagree about:
//!
//! * an **ABSENT** root is an empty scan with no complaint — a project that has run nothing is the
//!   ordinary state, not a fault;
//! * a root that EXISTS and cannot be listed, a manifest that will not parse, and a run directory
//!   with no manifest at all are each a NAMED problem rather than an absence — a run that vanishes
//!   from a list is unanswerable, one that shows as broken names its own fix;
//! * the **DIRECTORY name is the address**. `vike_model::runs::RunManifest::run_id` is duplicated
//!   into the file and can disagree with it; the path is what a caller opens, so the path decides
//!   and the disagreement is reported.
//!
//! # What this scan does NOT do
//!
//! It does not open `report.json` — the NAME is common and the CONTENT is the producer's own
//! business — and it carries no study half, no tier vocabulary and no severity ranking. It reports
//! strictly less than its twin, which is what stops it drifting by inventing a rule the twin does
//! not have.
//!
//! # The root is a PARAMETER
//!
//! Nothing here resolves a path. `crate::dispatch` owns the one walk and hands the answer down,
//! because a library that resolves its own project root reads global state its caller can neither
//! see nor override — the rule `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`
//! ratchets down. One consequence, stated rather than left implicit: a CLI invoked from a different
//! project than the engine ran in answers about its OWN project, which is correct.

use std::path::{Path, PathBuf};

use vike_model::runs::{MANIFEST_FILE, REPORT_FILE, RunManifest, RunReadError, read_manifest};

/// One run directory whose COMMON manifest was read.
///
/// `Debug` because a test asserting a REFUSAL over a collection of these needs it (`expect_err`
/// requires the Ok side to be `Debug`), and because a `{:?}` on a problems row is what a failing
/// assertion prints.
#[derive(Debug)]
pub(crate) struct ScannedRun {
    /// The DIRECTORY name, which is the address the run is found at — see this module's doc.
    pub(crate) run_id: String,
    /// The run directory itself.
    pub(crate) dir: PathBuf,
    /// The common manifest, verbatim, including the kind-specific `detail` subtree.
    pub(crate) manifest: RunManifest,
    /// The report file when it is there. Nothing here opens it.
    pub(crate) report: Option<PathBuf>,
}

/// What one scan found.
#[derive(Debug)]
pub(crate) struct RunScan {
    /// The root that was scanned — carried so a rendered list says WHICH tree it is about, which
    /// matters the moment `VIKE_USER_DATA_DIR` is in play.
    pub(crate) root: PathBuf,
    /// The runs, in directory-name order — which for a minted id is chronological, because
    /// `vike_model::runs::RunManifest::run_id` documents the `<unix-seconds>-<pid>-<seq>` shape.
    pub(crate) runs: Vec<ScannedRun>,
    /// Everything the scan could not use, one rendered sentence each. These go to STDERR, never to
    /// the `--json` document — see `crate::cmd::runs`'s doc for the rule.
    pub(crate) problems: Vec<String>,
}

/// Scan `runs_root`. **Never fails**: an absent root is an empty scan, and everything else that goes
/// wrong is a named `problems` row.
pub(crate) fn scan_runs(runs_root: &Path) -> RunScan {
    let mut scan =
        RunScan { root: runs_root.to_path_buf(), runs: Vec::new(), problems: Vec::new() };

    let entries = match std::fs::read_dir(runs_root) {
        Ok(e) => e,
        // ABSENT is silent; anything else is named. The two are different problems with different
        // fixes, and a permissions bug wearing the "not configured yet" answer looks exactly like a
        // correct fresh install.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return scan,
        Err(e) => {
            scan.problems.push(format!(
                "{}: the runs directory exists but cannot be read ({e}) — nothing under it was \
                 listed; fix its permissions",
                runs_root.display()
            ));
            return scan;
        }
    };

    let mut folders: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // A run is a DIRECTORY. A loose file beside them is somebody's notes, not a broken run.
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        folders.push((name.to_string(), path));
    }
    // Directory order is whatever the OS feels like; a list whose rows move between two calls is one
    // nobody can read.
    folders.sort_by(|a, b| a.0.cmp(&b.0));

    for (run_id, dir) in folders {
        match read_manifest(&dir) {
            Ok(manifest) => {
                if manifest.run_id != run_id {
                    scan.problems.push(format!(
                        "{run_id}: {} calls the run '{}' — listed under its DIRECTORY, which is \
                         where it is actually found. Fix the manifest's run_id, or rename the \
                         directory back.",
                        dir.join(MANIFEST_FILE).display(),
                        manifest.run_id
                    ));
                }
                let report = dir.join(REPORT_FILE);
                let report = report.is_file().then_some(report);
                scan.runs.push(ScannedRun { run_id, dir, manifest, report });
            }
            // The manifest is written LAST, so its absence is an unfinished run rather than a broken
            // one — and whether the REPORT survived is the whole question an operator has about it.
            Err(RunReadError::Missing { .. }) => {
                let kept = if dir.join(REPORT_FILE).is_file() {
                    format!("its {REPORT_FILE} IS there, so the result survived")
                } else {
                    format!("there is no {REPORT_FILE} either, so nothing was kept")
                };
                scan.problems.push(format!(
                    "{run_id}: {} holds no {MANIFEST_FILE} — the run is still being written, or it \
                     stopped before finishing ({kept}).",
                    dir.display()
                ));
            }
            Err(e @ (RunReadError::Read { .. } | RunReadError::Parse { .. })) => {
                scan.problems.push(format!(
                    "{run_id}: {e}. The run is on disk and cannot be listed; open the file, or \
                     delete the run directory if it is junk."
                ));
            }
        }
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

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
  "detail": {{ "strategy": "sma_cross" }}
}}
"#
        )
    }

    /// Build a FINISHED run: report first, manifest last, the order the writer uses.
    fn finished_run(root: &Path, run_id: &str, kind: &str) {
        let dir = root.join(run_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(REPORT_FILE), "{\"sharpe\":1.25}\n").unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), manifest_json(run_id, kind)).unwrap();
    }

    /// ONE row per run, off the COMMON manifest, with no parser per kind — the property the shared
    /// manifest exists for, and the one this scan must not lose.
    #[test]
    fn every_kind_of_run_scans_off_the_common_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        finished_run(root, "1756000000-1-0", "backtest");
        finished_run(root, "1756000100-1-0", "a-kind-invented-next-year");

        let scan = scan_runs(root);

        assert!(scan.problems.is_empty(), "unexpected: {:?}", scan.problems);
        assert_eq!(scan.runs.len(), 2);
        assert_eq!(scan.runs[0].run_id, "1756000000-1-0", "directory-name order");
        assert_eq!(scan.runs[1].manifest.kind, "a-kind-invented-next-year");
        assert_eq!(scan.runs[0].report, Some(root.join("1756000000-1-0").join(REPORT_FILE)));
        assert_eq!(scan.runs[0].manifest.detail["strategy"], serde_json::json!("sma_cross"));
    }

    /// An ABSENT root is the ordinary state of a project that has run nothing — empty and SILENT.
    /// The twin (`crates/vike-studio-core/src/listing.rs`'s `list_runs`) draws the same line and
    /// this test is what holds the two together.
    #[test]
    fn an_absent_root_is_empty_and_silent() {
        let tmp = tempfile::tempdir().unwrap();
        let scan = scan_runs(&tmp.path().join("never-created"));
        assert!(scan.runs.is_empty());
        assert!(
            scan.problems.is_empty(),
            "an unconfigured project is not a fault: {:?}",
            scan.problems
        );
    }

    /// A run being WRITTEN right now has a report and no manifest — the writer's order — so it is a
    /// named problem rather than a silent absence, and it is not a corrupt run.
    #[test]
    fn a_run_with_no_manifest_is_named_not_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("1756000000-1-0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(REPORT_FILE), "{}\n").unwrap();

        let scan = scan_runs(tmp.path());

        assert!(scan.runs.is_empty());
        assert_eq!(scan.problems.len(), 1);
        let p = &scan.problems[0];
        assert!(p.contains("1756000000-1-0"), "{p}");
        assert!(p.contains("still being written") || p.contains("stopped"), "{p}");
    }

    /// A manifest that will not parse is a row, never an absence: a run that vanishes from a list is
    /// unanswerable, one that shows as broken names its own fix.
    #[test]
    fn an_unparseable_manifest_is_named_not_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("1756000000-1-0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), "not json at all").unwrap();

        let scan = scan_runs(tmp.path());

        assert!(scan.runs.is_empty());
        assert_eq!(scan.problems.len(), 1, "{:?}", scan.problems);
        assert!(scan.problems[0].contains("1756000000-1-0"), "{:?}", scan.problems);
    }

    /// The DIRECTORY is the address. A manifest calling itself something else still scans, under the
    /// name it is actually found at, with the disagreement reported.
    #[test]
    fn the_directory_name_is_the_address_and_a_mismatch_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("1756000000-1-0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), manifest_json("somebody-elses-id", "backtest"))
            .unwrap();

        let scan = scan_runs(tmp.path());

        assert_eq!(scan.runs.len(), 1);
        assert_eq!(scan.runs[0].run_id, "1756000000-1-0", "the DIRECTORY decides");
        assert_eq!(scan.runs[0].manifest.run_id, "somebody-elses-id");
        assert_eq!(scan.problems.len(), 1);
        assert!(scan.problems[0].contains("somebody-elses-id"), "{:?}", scan.problems);
    }

    /// A loose FILE in the runs root is not a run and is not a fault either — a run is a DIRECTORY.
    #[test]
    fn a_loose_file_in_the_runs_root_is_not_a_run() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "hello").unwrap();
        let scan = scan_runs(tmp.path());
        assert!(scan.runs.is_empty());
        assert!(scan.problems.is_empty(), "{:?}", scan.problems);
    }
}
