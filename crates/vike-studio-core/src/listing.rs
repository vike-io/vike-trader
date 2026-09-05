//! **What is in `<project>/user_data/`, as a list**: the RUNS that exist, and the STUDIES that
//! produce them.
//!
//! # Why a listing is the thing that was missing
//!
//! A result that is not listed does not exist to the person who produced it. Research results went
//! to a ClickHouse an end user does not have, and a strategy backtest printed to stdout and was
//! gone with the scrollback — two producers, two places, neither of them anywhere a user could
//! point at. `crates/vike-backtest/src/runs.rs` is the half that made a run leave something behind;
//! this is the half that finds it again.
//!
//! # What a listing may assume about a run, and what it may not
//!
//! It reads the COMMON manifest and nothing else. Every top-level field of
//! [`vike_backtest::runs::RunManifest`] is identical for every producer, so ONE row renders a
//! backtest, a research run and a kind invented next year — and the kind-specific `detail` subtree
//! is carried through untouched for a caller that recognises it. Nothing here opens the report:
//! its NAME is common and its CONTENT is the producer's own business, so this reports whether it
//! is THERE and leaves reading it to whoever knows what it holds.
//!
//! # Two decisions this module shares with the strategy loader, and takes for its reasons
//!
//! Modelled on `crates/vike-studio-core/src/user_strategies/load.rs`, which states both at length.
//!
//! **An ABSENT root is silent; an UNREADABLE one is an ERROR with its own row.** A fresh install
//! has no `user_data/` at all — `crates/vike-model/src/state_path.rs`'s `user_runs_dir` answers
//! with where the directory BELONGS rather than one it found — so "nothing there" is the ordinary
//! state. A root that EXISTS and cannot be listed is the opposite: a permissions bug wearing the
//! "not configured yet" answer looks exactly like a correct fresh install while every run silently
//! vanishes. That is the same line the credential store draws
//! (`crates/vike-secrets/src/dotenv.rs`), for the same reason.
//!
//! **The root is a `&Path` PARAMETER and nothing here resolves one.** The BINARY calls
//! `vike_model::state_path::user_runs_dir` / `user_studies_dir` and hands the answer down: a
//! library that resolves its own project root reads global state its caller can neither see nor
//! override, which is what `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchets
//! down and why ONE walk decides which project a process is in.
//!
//! # Nothing is skipped in silence, and nothing is COMPILED either
//!
//! Every failure is a named [`ListingDiagnostic`] carrying its path and its fix — a run that
//! vanishes from a list is worse than one that shows as broken, because the first is unanswerable
//! and the second names what to do. A manifest that will not parse, and a run directory with no
//! manifest at all, are therefore rows rather than absences.
//!
//! ⚠ **A study is LISTED, never compiled or run**, and that is a deliberate boundary rather than
//! an omission. The strategy loader compiles every script during its scan because a strategy that
//! cannot build must not be offered as runnable — it has a `build_strategy` to call. So a folder is
//! a study because it is a FOLDER: no entry-file rule, no name registry, no verdict about whether
//! it would run.
//!
//! ⚠ **This paragraph used to say a study has no execution contract in this workspace AT ALL, and
//! that is no longer true** — `crates/vike-user-research/src/contract.rs` is the contract and
//! `crates/vike-studio-core/src/study_run.rs`'s `run_study` is the caller. The boundary above
//! survives the change unaltered and its reason gets STRONGER rather than weaker: a compiled study
//! is resolved through a registry generated at BUILD time, so "would this folder run" is not a
//! question a directory scan can answer at all — a folder added since the binary was compiled is
//! real, listable, and not in the registry. Listing it is the honest answer; refusing to list it
//! would hide a folder the user can see with their own eyes.
//!
//! # Determinism
//!
//! Directory order is whatever the OS feels like. Everything here is sorted before it is returned —
//! runs by directory NAME (which for a minted id is chronological: `runs.rs`'s `RunManifest`
//! documents the `<unix-seconds>-<pid>-<seq>` shape, and a plain name sort is what makes a
//! hand-made directory sort predictably too), studies by tier and then by name. A results surface
//! whose rows move between two calls is one nobody can read.

use std::path::{Path, PathBuf};

use vike_backtest::runs::{MANIFEST_FILE, REPORT_FILE, RunManifest, RunReadError, read_manifest};
use vike_model::state_path::{RHAI_SUBDIR, RUST_SUBDIR};

// The layout vocabulary is the strategy loader's, shared rather than re-spelled: a Rhai file is
// named the same thing in both trees, the case-insensitive comparison carries the same argument (a
// Windows editor that saved `VOL.RHAI` wrote a perfectly good script), and `stray_target` states
// the same rule about where a loose file belongs. Two copies could disagree; there is one.
use crate::user_strategies::Severity;
use crate::user_strategies::load::{RHAI_EXT, RUST_EXT, has_ext, stray_target};

/// Which tier a study was found in — the same split a strategy has
/// (`vike_model::state_path::STUDIES_SUBDIR`), carried on the row because the difference is one a
/// user acts on: an interpreted study runs on a shipped binary, a compiled one needs a checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudyTier {
    /// `research/studies/rhai` — interpreted.
    Rhai,
    /// `research/studies/rust` — compiled, meaningful only in a source checkout.
    Rust,
}

impl StudyTier {
    /// The tier's directory name under `research/studies`.
    fn subdir(self) -> &'static str {
        match self {
            StudyTier::Rhai => RHAI_SUBDIR,
            StudyTier::Rust => RUST_SUBDIR,
        }
    }

    /// The extension a STRAY file loose in this tier's root would have — the only file-level fact
    /// this module knows, and it is about the LAYOUT (a study is a folder), never about contents.
    fn ext(self) -> &'static str {
        match self {
            StudyTier::Rhai => RHAI_EXT,
            StudyTier::Rust => RUST_EXT,
        }
    }
}

/// One study folder, as found. No verdict about whether it would run — see this module's doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedStudy {
    /// The folder name, verbatim.
    pub name: String,
    /// The folder itself, so a caller can open it, watch it, or delete the whole study.
    pub dir: PathBuf,
    /// Which tier it was found in.
    pub tier: StudyTier,
}

/// One run directory whose COMMON manifest was read.
#[derive(Debug, Clone)]
pub struct ListedRun {
    /// The DIRECTORY name, which is the address the run is found at.
    ///
    /// ⚠ Deliberately the directory rather than [`RunManifest::run_id`], which is duplicated into
    /// the file and can therefore disagree with it (a manifest copied out of its directory, a
    /// folder renamed by hand). The path is what a caller opens, so the path decides — and the
    /// disagreement is reported as [`ListingDiagnostic::RunIdMismatch`] rather than silently
    /// resolved.
    pub run_id: String,
    /// The run directory.
    pub dir: PathBuf,
    /// The common manifest, verbatim — including the kind-specific `detail` subtree, untouched.
    pub manifest: RunManifest,
    /// The run's report file when it is there. Its CONTENT is the producer's own business and
    /// nothing here opens it; that it EXISTS is what a results surface needs in order to offer it.
    pub report: Option<PathBuf>,
}

/// What one scan of `<project>/user_data/runs` found.
#[derive(Debug, Clone)]
pub struct RunListing {
    /// The root that was scanned — carried so a rendered list says WHICH tree it is about, which
    /// matters the moment `VIKE_USER_DATA_DIR` is in play.
    pub root: PathBuf,
    /// The runs, in directory-name order.
    pub runs: Vec<ListedRun>,
    /// Everything the scan could not use, named.
    pub diagnostics: Vec<ListingDiagnostic>,
}

/// What one scan of `<project>/user_data/research/studies` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudyListing {
    /// The `studies/` root that was scanned, not either tier under it.
    pub root: PathBuf,
    /// The studies: the `rhai` tier in name order, then the `rust` one.
    pub studies: Vec<ListedStudy>,
    /// Everything the scan could not use, named.
    pub diagnostics: Vec<ListingDiagnostic>,
}

/// Everything a scan could not use, NAMED — one variant per thing that actually goes wrong, each
/// carrying the path it is about and enough to act on.
///
/// Shared by both listings because the failures are the same failures: a directory that will not
/// open, and an entry inside it that will not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListingDiagnostic {
    /// A root (or a study TIER) exists and could not be listed. Deliberately distinct from "no
    /// root", which is silent — see this module's doc.
    RootUnreadable {
        /// The directory that would not open.
        root: PathBuf,
        /// The operating system's own words.
        error: String,
    },

    /// A run's manifest is there and cannot be turned into a row: unreadable, not JSON, or JSON
    /// missing a common field.
    ///
    /// The three collapse into one variant on purpose, the way
    /// `crates/vike-studio-core/src/user_strategies/load.rs`'s `read_preset` collapses its three:
    /// to the user they are one event — "this run cannot be listed, here is why" — and `error`
    /// carries which one it was in the reader's own words.
    RunUnreadable {
        /// The run directory's name.
        run_id: String,
        /// The manifest that could not be used.
        path: PathBuf,
        /// `vike_backtest::runs::RunReadError`'s own words.
        error: String,
    },

    /// A run directory holding no manifest at all.
    ///
    /// The manifest is written LAST (`crates/vike-backtest/src/runs.rs`'s module doc), so this is
    /// a run being written RIGHT NOW or one that stopped between its two writes — not a corrupt
    /// one. A [`Severity::Warning`] for that reason: the report is the irreplaceable half and is
    /// written FIRST, so it may well be sitting there, and painting a running backtest as an error
    /// would train a user to ignore the list.
    RunUnfinished {
        /// The run directory's name.
        run_id: String,
        /// The directory itself.
        dir: PathBuf,
        /// Whether the report file is there — the difference between "the result survived" and
        /// "nothing was kept", which is the whole question an operator has about this row.
        report: bool,
    },

    /// A manifest whose `run_id` is not the directory it sits in. The run still lists, addressed by
    /// its DIRECTORY — see [`ListedRun::run_id`].
    RunIdMismatch {
        /// The directory name, which is what the listing uses.
        run_id: String,
        /// What the manifest calls itself.
        declared: String,
        /// The manifest that disagrees.
        path: PathBuf,
    },

    /// A study script directly in a tier root instead of in a folder of its own — the same
    /// first-time mistake the strategy loader reports, in a tree with the same layout.
    StrayStudy {
        /// The file that is not a study.
        path: PathBuf,
    },
}

impl ListingDiagnostic {
    /// Did this cost the user a row they should have had?
    ///
    /// The two [`Severity::Warning`]s both leave something listed or recoverable: an unfinished run
    /// still has its report on disk, and a mismatched id still lists under its directory.
    /// Everything else means work the user believes is there and is not.
    pub fn severity(&self) -> Severity {
        match self {
            ListingDiagnostic::RunUnfinished { .. } | ListingDiagnostic::RunIdMismatch { .. } => {
                Severity::Warning
            }
            _ => Severity::Error,
        }
    }
}

impl std::fmt::Display for ListingDiagnostic {
    /// ONE line each, and every line names the fix — these are read by a person who has already
    /// looked in the directory once and not understood what they saw.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListingDiagnostic::RootUnreadable { root, error } => write!(
                f,
                "{}: the directory exists but cannot be read ({error}) — nothing under it was \
                 listed; fix its permissions",
                root.display()
            ),
            ListingDiagnostic::RunUnreadable { run_id, path, error } => write!(
                f,
                "{run_id}: {} cannot be used — {error}. The run is on disk and cannot be listed; \
                 open the file, or delete the run directory if it is junk.",
                path.display()
            ),
            ListingDiagnostic::RunUnfinished { run_id, dir, report } => {
                let kept = if *report {
                    format!("its {REPORT_FILE} IS there, so the result survived")
                } else {
                    format!("there is no {REPORT_FILE} either, so nothing was kept")
                };
                write!(
                    f,
                    "{run_id}: {} holds no {MANIFEST_FILE} — the run is still being written, or it \
                     stopped before finishing ({kept}).",
                    dir.display()
                )
            }
            ListingDiagnostic::RunIdMismatch { run_id, declared, path } => write!(
                f,
                "{run_id}: {} calls the run '{declared}' — listed under its DIRECTORY, which is \
                 where it is actually found. Fix the manifest's run_id, or rename the directory \
                 back.",
                path.display()
            ),
            ListingDiagnostic::StrayStudy { path } => write!(
                f,
                "{}: a script directly in a study tier is NOT a study — a study is a FOLDER. Move \
                 it to {}",
                path.display(),
                stray_target(path).display()
            ),
        }
    }
}

/// List `<project>/user_data/runs` — every run of every kind, read through the COMMON manifest.
///
/// `runs_root` comes from `crates/vike-model/src/state_path.rs`'s `user_runs_dir`, resolved by the
/// BINARY: this module resolves nothing (see the module doc).
///
/// Never fails. An absent root is an empty listing with no diagnostic; everything else that goes
/// wrong is a named row.
pub fn list_runs(runs_root: &Path) -> RunListing {
    let mut listing =
        RunListing { root: runs_root.to_path_buf(), runs: Vec::new(), diagnostics: Vec::new() };
    let Some(Entries { folders, .. }) = read_entries(runs_root, &mut listing.diagnostics) else {
        return listing;
    };

    for (run_id, dir) in folders {
        match read_manifest(&dir) {
            Ok(manifest) => {
                if manifest.run_id != run_id {
                    listing.diagnostics.push(ListingDiagnostic::RunIdMismatch {
                        run_id: run_id.clone(),
                        declared: manifest.run_id.clone(),
                        path: dir.join(MANIFEST_FILE),
                    });
                }
                let report = dir.join(REPORT_FILE);
                let report = report.is_file().then_some(report);
                listing.runs.push(ListedRun { run_id, dir, manifest, report });
            }
            // Written LAST, so its absence is an unfinished run rather than a broken one.
            Err(RunReadError::Missing { .. }) => {
                let report = dir.join(REPORT_FILE).is_file();
                listing.diagnostics.push(ListingDiagnostic::RunUnfinished { run_id, dir, report });
            }
            Err(RunReadError::Read { path, why } | RunReadError::Parse { path, why }) => {
                listing.diagnostics.push(ListingDiagnostic::RunUnreadable {
                    run_id,
                    path,
                    error: why,
                });
            }
        }
    }
    listing
}

/// List `<project>/user_data/research/studies` — BOTH tiers, `rhai/` then `rust/`.
///
/// `studies_root` comes from `crates/vike-model/src/state_path.rs`'s `user_studies_dir`, resolved
/// by the BINARY.
///
/// ⚠ **Enumeration only.** Nothing is compiled, nothing is run, and no entry-file rule is applied —
/// the module doc carries that argument. A folder is a study; that is the whole contract this
/// function asserts.
///
/// A name claimed in BOTH tiers lists TWICE, one row per tier, and produces no diagnostic. The
/// strategy loader treats that as a collision because a strategy is ADDRESSED by name (its
/// `resolve_preset` looks one up); nothing addresses a study by name yet, so refusing one here
/// would be a namespace invented ahead of the thing that needs it — and it would silently hide a
/// folder the user can see with their own eyes.
///
/// Never fails: an absent root, and an absent TIER, are empty and silent.
pub fn list_studies(studies_root: &Path) -> StudyListing {
    let mut listing = StudyListing {
        root: studies_root.to_path_buf(),
        studies: Vec::new(),
        diagnostics: Vec::new(),
    };
    for tier in [StudyTier::Rhai, StudyTier::Rust] {
        let root = studies_root.join(tier.subdir());
        let Some(Entries { folders, files }) = read_entries(&root, &mut listing.diagnostics) else {
            continue;
        };
        for path in files.into_iter().filter(|p| has_ext(p, tier.ext())) {
            listing.diagnostics.push(ListingDiagnostic::StrayStudy { path });
        }
        for (name, dir) in folders {
            listing.studies.push(ListedStudy { name, dir, tier });
        }
    }
    listing
}

/// One directory's entries, split by kind and SORTED — what [`read_entries`] hands back.
///
/// A named struct rather than a tuple because the two halves mean opposite things in the two
/// scans: a runs root cares about the folders and a study tier cares about both, and
/// `entries.files` says which is which at the use site where a `.1` would not.
struct Entries {
    /// The sub-directories, as `(name, path)`, in name order.
    folders: Vec<(String, PathBuf)>,
    /// The files directly inside, in path order.
    files: Vec<PathBuf>,
}

/// One directory's entries, split and SORTED — the shape both listings scan, and the one place the
/// absent-vs-unreadable ruling is made.
///
/// `None` means there is nothing to scan: either the directory does not exist (silent, the ordinary
/// state) or it could not be listed (a [`ListingDiagnostic::RootUnreadable`] was pushed).
///
/// Dot-entries are skipped: tool droppings are not user content, and skipping them is a rule rather
/// than a silent loss.
///
/// A per-ENTRY error is skipped for the reason the strategy loader gives: the directory entry
/// itself failed to materialise, so there is no name to attach a diagnostic to and no actionable
/// text to put in one — unlike the root failure above, which names the directory.
fn read_entries(root: &Path, diags: &mut Vec<ListingDiagnostic>) -> Option<Entries> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            diags.push(ListingDiagnostic::RootUnreadable {
                root: root.to_path_buf(),
                error: e.to_string(),
            });
            return None;
        }
    };

    let mut folders: Vec<(String, PathBuf)> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            folders.push((name, path));
        } else {
            files.push(path);
        }
    }
    folders.sort();
    files.sort();
    Some(Entries { folders, files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A manifest exactly as `crates/vike-backtest/src/runs.rs`'s `write_run` lays one out, written
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
        write(
            &root.join("rhai").join("vol-clustering").join("vol-clustering.rhai"),
            "fn go() {}\n",
        );
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
}
