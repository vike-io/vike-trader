use super::*;
use std::path::{Path, PathBuf};

/// A manifest exactly as `crates/vike-model/src/runs.rs`'s `write_run` lays one out, written
/// as TEXT on purpose: these tests pin the ON-DISK document a listing has to survive, not a
/// round trip through the writer's own struct.
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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A finished run directory: the report first, the manifest last — the order the writer uses.
fn finished_run(root: &Path, run_id: &str, kind: &str) -> PathBuf {
    let dir = root.join(run_id);
    write(&dir.join("report.json"), "{ \"sharpe\": 1.25 }\n");
    write(&dir.join("manifest.json"), &manifest_json(run_id, kind));
    dir
}

/// A fresh install has no `user_data/` at all, so an absent runs root is the ORDINARY state and
/// must be silent. Greeting a new user with an error about a directory they have never heard of
/// is the alternative.
#[test]
fn an_absent_runs_root_lists_nothing_and_says_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("user_data").join("runs");
    assert!(!root.exists(), "precondition");

    let listing = list_runs(&root);

    assert!(listing.runs.is_empty());
    assert!(listing.diagnostics.is_empty(), "absence is not a failure");
}

/// The opposite ruling for the opposite fact: a root that EXISTS and cannot be listed is a
/// permissions bug wearing the "not configured yet" answer, and it looks exactly like a correct
/// fresh install while every run silently vanishes.
#[test]
fn a_runs_root_that_exists_and_cannot_be_read_is_its_own_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("runs");
    // A FILE where the directory belongs: `read_dir` fails with something that is NOT
    // `NotFound`, on every platform this ships to, without a test changing permissions.
    write(&root, "not a directory");

    let listing = list_runs(&root);

    assert!(listing.runs.is_empty());
    match &listing.diagnostics[..] {
        [ListingDiagnostic::RootUnreadable { root: reported, error }] => {
            assert_eq!(reported, &root);
            assert!(!error.is_empty(), "the OS's own words must be carried through");
        }
        other => panic!("expected one RootUnreadable, got {other:?}"),
    }
    assert_eq!(listing.diagnostics[0].severity(), Severity::Error);
}

/// THE property the common manifest exists for: one listing renders every KIND of run off the
/// top-level fields, with no parser per kind — including a kind this crate has never heard of.
#[test]
fn every_kind_of_run_lists_off_the_common_fields_with_no_per_kind_parser() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    finished_run(root, "1756000000-1-0", "backtest");
    finished_run(root, "1756000100-1-0", "a-kind-invented-next-year");

    let listing = list_runs(root);

    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
    assert_eq!(listing.runs.len(), 2);
    let first = &listing.runs[0];
    assert_eq!(first.run_id, "1756000000-1-0");
    assert_eq!(first.manifest.kind, "backtest");
    assert_eq!(first.manifest.produced_by, "backtest");
    assert_eq!(first.manifest.started_at, "2026-08-24T09:15:04Z");
    assert_eq!(first.manifest.finished_at, "2026-08-24T09:15:16Z");
    assert_eq!(first.manifest.git_sha, None);
    assert_eq!(first.manifest.config.name.as_deref(), Some("sma cross"));
    assert_eq!(first.report, Some(root.join("1756000000-1-0").join("report.json")));
    assert_eq!(
        listing.runs[1].manifest.kind, "a-kind-invented-next-year",
        "an unrecognised kind still renders a complete row"
    );
}

/// A run that vanishes from a listing is worse than one that shows as broken: the first is
/// unanswerable, the second names its own fix. So a corrupt manifest is a NAMED row.
#[test]
fn a_run_whose_manifest_is_corrupt_is_reported_rather_than_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    finished_run(root, "1756000000-1-0", "backtest");
    write(&root.join("1756000100-1-0").join("manifest.json"), "{ not json");

    let listing = list_runs(root);

    assert_eq!(listing.runs.len(), 1, "the good run still lists");
    match &listing.diagnostics[..] {
        [ListingDiagnostic::RunUnreadable { run_id, path, error }] => {
            assert_eq!(run_id, "1756000100-1-0");
            assert!(path.ends_with("manifest.json"), "{path:?}");
            assert!(!error.is_empty(), "the parser's own words must be carried through");
        }
        other => panic!("expected one RunUnreadable, got {other:?}"),
    }
    assert_eq!(listing.diagnostics[0].severity(), Severity::Error);
}

/// The manifest is written LAST, so a directory without one is a run in flight or a run that
/// stopped mid-write — reported, never dropped, and as a WARNING because the report (the
/// irreplaceable half) may well be sitting right there.
#[test]
fn a_run_directory_with_no_manifest_is_reported_as_unfinished_not_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("1756000100-1-0").join("report.json"), "{}\n");
    std::fs::create_dir_all(root.join("1756000200-1-0")).unwrap();

    let listing = list_runs(root);

    assert!(listing.runs.is_empty(), "a run with no manifest produces no row");
    match &listing.diagnostics[..] {
        [
            ListingDiagnostic::RunUnfinished { run_id: a, report: true, .. },
            ListingDiagnostic::RunUnfinished { run_id: b, report: false, .. },
        ] => {
            assert_eq!(a, "1756000100-1-0");
            assert_eq!(b, "1756000200-1-0");
        }
        other => panic!("expected two RunUnfinished rows, got {other:?}"),
    }
    assert_eq!(listing.diagnostics[0].severity(), Severity::Warning);
    assert!(
        listing.diagnostics[0].to_string().contains("report.json"),
        "the row must say the result is there: {}",
        listing.diagnostics[0]
    );
}

/// The id is duplicated INTO the manifest on purpose, so the two CAN disagree — a manifest
/// copied out of its directory, or a hand-renamed folder. The DIRECTORY wins, because that is
/// the address the run is actually found at, and the disagreement is reported rather than
/// silently resolved.
#[test]
fn a_manifest_that_names_a_different_run_id_than_its_directory_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("1756000000-1-0").join("manifest.json"),
        &manifest_json("some-other-run", "backtest"),
    );

    let listing = list_runs(root);

    assert_eq!(listing.runs.len(), 1);
    assert_eq!(listing.runs[0].run_id, "1756000000-1-0", "the directory is the address");
    match &listing.diagnostics[..] {
        [ListingDiagnostic::RunIdMismatch { run_id, declared, .. }] => {
            assert_eq!(run_id, "1756000000-1-0");
            assert_eq!(declared, "some-other-run");
        }
        other => panic!("expected one RunIdMismatch, got {other:?}"),
    }
    assert_eq!(listing.diagnostics[0].severity(), Severity::Warning);
}

/// Directory order is whatever the OS feels like. A listing that reorders between two calls
/// makes a results surface unreadable, so the order is a RULE: by directory name.
#[test]
fn runs_are_listed_in_directory_name_order_whatever_the_filesystem_says() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for id in ["1756000200-1-0", "1756000000-1-0", "1756000100-1-0"] {
        finished_run(root, id, "backtest");
    }

    let ids: Vec<String> = list_runs(root).runs.into_iter().map(|r| r.run_id).collect();

    assert_eq!(ids, vec!["1756000000-1-0", "1756000100-1-0", "1756000200-1-0"]);
    assert_eq!(ids, list_runs(root).runs.into_iter().map(|r| r.run_id).collect::<Vec<_>>());
}

/// A run IS a directory. Dot-entries are tool droppings and a loose file was written by nothing
/// in this workspace — neither is a run that went missing, so neither earns a row.
#[test]
fn dot_entries_and_loose_files_in_the_runs_root_are_not_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    finished_run(root, "1756000000-1-0", "backtest");
    write(&root.join(".DS_Store"), "junk");
    write(&root.join("README.md"), "# my runs\n");
    std::fs::create_dir_all(root.join(".git")).unwrap();

    let listing = list_runs(root);

    assert_eq!(listing.runs.len(), 1);
    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
}

/// Both tiers, one listing, each row saying which tier it came from — the tiers exist because
/// a compiled study needs a checkout and an interpreted one does not, and that is exactly the
/// difference a user needs to see in the list.
#[test]
fn both_study_tiers_are_listed_with_the_tier_each_was_found_in() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("rhai").join("vol-clustering").join("vol-clustering.rhai"), "fn go() {}\n");
    write(&root.join("rust").join("cohort-decay").join("cohort-decay.rs"), "fn main() {}\n");

    let listing = list_studies(root);

    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
    assert_eq!(
        listing.studies.iter().map(|s| (s.name.as_str(), s.tier)).collect::<Vec<_>>(),
        vec![("vol-clustering", StudyTier::Rhai), ("cohort-decay", StudyTier::Rust)],
        "the rhai tier first, each tier in name order"
    );
    assert_eq!(listing.studies[0].dir, root.join("rhai").join("vol-clustering"));
}

/// Listing is LISTING. A study whose script could not compile still appears, because nothing
/// here compiles or runs anything — a Study execution contract does not exist yet, and a
/// listing that quietly depended on one would hide every study the day it did.
#[test]
fn a_study_that_could_never_compile_still_lists_because_nothing_here_runs_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("rhai").join("broken").join("broken.rhai"), "fn on_bar( {\n");

    let listing = list_studies(root);

    assert_eq!(listing.studies.len(), 1);
    assert_eq!(listing.studies[0].name, "broken");
    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
}

/// A folder with no script at all is still a study folder. The entry-file rule belongs to
/// whatever eventually RUNS a study; asserting one here would be inventing that contract, and
/// the folder would be reported as broken for failing a rule nobody has written.
#[test]
fn a_study_folder_holding_no_script_is_listed_rather_than_called_broken() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("rhai").join("just-started")).unwrap();

    let listing = list_studies(root);

    assert_eq!(listing.studies.len(), 1);
    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
}

/// One name in BOTH tiers lists TWICE, each with its tier. Strategies share one namespace
/// because something resolves a strategy BY NAME; nothing resolves a study by name yet, so a
/// duplicate rule here would be a namespace invented ahead of the thing that needs it.
#[test]
fn a_name_used_in_both_tiers_lists_twice_because_nothing_resolves_a_study_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("rhai").join("decay").join("decay.rhai"), "fn go() {}\n");
    write(&root.join("rust").join("decay").join("decay.rs"), "fn main() {}\n");

    let listing = list_studies(root);

    assert_eq!(listing.studies.len(), 2, "both rows survive");
    assert_eq!(listing.studies[0].tier, StudyTier::Rhai);
    assert_eq!(listing.studies[1].tier, StudyTier::Rust);
    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
}

/// The same two rulings as the runs root, over the studies tree: absent is silent, and an
/// absent TIER is absent too — a user who writes only Rhai studies has no `rust/` directory.
#[test]
fn an_absent_studies_root_or_tier_lists_nothing_and_says_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("user_data").join("research").join("studies");
    assert!(!root.exists(), "precondition");

    let listing = list_studies(&root);
    assert!(listing.studies.is_empty());
    assert!(listing.diagnostics.is_empty(), "absence is not a failure");

    write(&root.join("rhai").join("only-rhai").join("only-rhai.rhai"), "fn go() {}\n");
    let listing = list_studies(&root);
    assert_eq!(listing.studies.len(), 1);
    assert!(listing.diagnostics.is_empty(), "a missing rust/ tier is not a failure");
}

/// …and the other half: a TIER that exists and cannot be read is a named row, because a
/// permissions bug there hides every study in it while looking like a user who writes only
/// Rhai.
#[test]
fn a_study_tier_that_exists_and_cannot_be_read_is_its_own_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("rhai").join("ok").join("ok.rhai"), "fn go() {}\n");
    write(&root.join("rust"), "a file where the rust tier belongs");

    let listing = list_studies(root);

    assert_eq!(listing.studies.len(), 1, "the readable tier still lists");
    match &listing.diagnostics[..] {
        [ListingDiagnostic::RootUnreadable { root: reported, .. }] => {
            assert_eq!(reported, &root.join("rust"));
        }
        other => panic!("expected one RootUnreadable, got {other:?}"),
    }
}

/// The first-time mistake, mirrored from the strategy loader because the layout is mirrored: a
/// script dropped straight into a tier root. Reported with the folder it belongs in — never
/// silently ignored, which is what makes "I dropped a file in and nothing happened" answerable.
#[test]
fn a_study_script_loose_in_a_tier_root_is_reported_with_where_it_belongs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("rhai").join("mystudy.rhai"), "fn go() {}\n");
    // Anything else loose in the tier is NOT a stray study: a README is not a study that went
    // missing, and reporting it would be noise that trains a user to ignore the list.
    write(&root.join("rhai").join("README.md"), "# my studies\n");

    let listing = list_studies(root);

    assert!(listing.studies.is_empty());
    match &listing.diagnostics[..] {
        [ListingDiagnostic::StrayStudy { path }] => {
            assert_eq!(path, &root.join("rhai").join("mystudy.rhai"));
        }
        other => panic!("expected one StrayStudy, got {other:?}"),
    }
    let msg = listing.diagnostics[0].to_string();
    assert!(msg.contains("mystudy"), "the message must name the fix as a path: {msg}");
    assert_eq!(listing.diagnostics[0].severity(), Severity::Error);
}

/// ⚠ THE SEARCH-PARENT LAYOUT DECISION, pinned where it can actually be broken. A parameter
/// search keeps its trials as LINES in `trials.jsonl` inside its own run directory — never as
/// sibling run directories named `<id>#<n>` — and this is what that buys: three trials, plus
/// the two extra documents a search writes, is still ONE listing row and ZERO diagnostics.
///
/// `crates/vike-backtest/src/trial_ledger.rs`'s `TRIALS_FILE` carries the argument. The half
/// below measures what the rejected spelling would have cost.
#[test]
fn a_search_parent_lists_as_one_row_whatever_it_holds() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let dir = runs.join("1756000000-77-0");
    write(&dir.join(MANIFEST_FILE), &manifest_json("1756000000-77-0", "search"));
    write(&dir.join(REPORT_FILE), "{}\n");
    write(&dir.join("search.json"), "{}\n");
    write(&dir.join("trials.jsonl"), "{\"n\":0}\n{\"n\":1}\n{\"n\":2}\n");

    let listing = list_runs(&runs);
    assert_eq!(listing.runs.len(), 1, "one row, not one per trial: {:?}", listing.runs);
    assert_eq!(listing.runs[0].manifest.kind, "search");
    assert!(
        listing.diagnostics.is_empty(),
        "and the two extra FILES are not mistaken for anything: {:?}",
        listing.diagnostics
    );
}

/// …and the measurement that makes the row above worth having: sibling directories named
/// `<id>#<n>` — the spelling stage 5 rejected — DO become rows, one per trial. `read_entries`
/// enumerates folders and [`list_runs`] knows nothing about parenthood, so a 512-point sweep in
/// that layout is 512 Research-tab rows.
#[test]
fn sibling_child_directories_would_each_become_a_row() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    for name in ["1756000000-77-0", "1756000000-77-0#0", "1756000000-77-0#1"] {
        write(&runs.join(name).join(MANIFEST_FILE), &manifest_json(name, "search"));
    }
    let listing = list_runs(&runs);
    assert_eq!(
        listing.runs.len(),
        3,
        "this is the cost the file-per-search layout avoids, measured rather than asserted"
    );
}
