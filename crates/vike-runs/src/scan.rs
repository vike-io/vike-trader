//! The SCAN RULES of `<project>/user_data/runs` — every run of every kind, read through the COMMON
//! manifest. The ONE scan: `vike-cli`'s `runs` verbs call [`scan_runs`] directly, and the Studio's
//! results surface (`crates/vike-studio-core/src/listing.rs`'s `list_runs`, which decision 0031
//! pins the study runner beside) calls it too; each keeps only its own rendering. They used to
//! carry a twin each, the rules restated by hand. The owner's ruling of 2026-11-09 is the split:
//! the rules here, the listing there (decision 0031's amendment).
//!
//! # What a listing may assume about a run, and what it may not
//!
//! It reads the COMMON manifest and nothing else, so ONE row renders a backtest, a research run and
//! a kind invented next year — the kind-specific `detail` subtree is carried through untouched for
//! a caller that recognises it. Nothing here opens the report: its NAME is common and its CONTENT is
//! the producer's own business, so this reports whether it is THERE.
//!
//! # Absent is silent; unreadable is an ERROR with its own row
//!
//! A fresh install has no `user_data/` at all (`vike_paths::state_path::user_runs_dir` answers with
//! where the directory BELONGS rather than one it found), so "nothing there" is the ordinary state.
//! A root that EXISTS and cannot be listed is the opposite: a permissions bug wearing the "not
//! configured yet" answer looks exactly like a correct fresh install while every run silently
//! vanishes — the line the credential store draws too.
//!
//! # Nothing is skipped in silence
//!
//! Every failure is a named [`ScanDiagnostic`] carrying its path and its fix. A manifest that will
//! not parse, and a run directory with no manifest at all, are rows rather than absences. A loose
//! FILE in the root and a dot-entry (tool droppings) are not runs, and that is a rule rather than a
//! silent loss.
//!
//! # Determinism
//!
//! Runs are sorted by directory NAME, which for a minted id is chronological: only the
//! `<unix-seconds>` PREFIX of `<unix-seconds>-<address|pid>-<seq>` is load-bearing for the sort, and
//! unix seconds are ten digits until the year 2286 (`crate::run_id_at`).
//!
//! The root is a `&Path` PARAMETER and nothing here resolves one — the crate doc's rule.

use std::path::{Path, PathBuf};

use vike_model::DiagnosticSeverity;

use crate::{MANIFEST_FILE, REPORT_FILE, RunManifest, RunReadError, read_manifest};

/// One run directory whose COMMON manifest was read.
#[derive(Debug, Clone)]
pub struct ScannedRun {
    /// The DIRECTORY name, which is the address the run is found at.
    ///
    /// ⚠ Deliberately the directory rather than [`RunManifest::run_id`], which is duplicated into
    /// the file and can therefore disagree with it (a manifest copied out of its directory, a
    /// folder renamed by hand). The path is what a caller opens, so the path decides — and the
    /// disagreement is reported as [`ScanDiagnostic::RunIdMismatch`] rather than silently
    /// resolved.
    pub run_id: String,
    /// The run directory.
    pub dir: PathBuf,
    /// The common manifest, verbatim — including the kind-specific `detail` subtree, untouched.
    pub manifest: RunManifest,
    /// The run's report file when it is there. Nothing here opens it.
    pub report: Option<PathBuf>,
}

/// What one scan of `<project>/user_data/runs` found.
#[derive(Debug, Clone)]
pub struct RunScan {
    /// The root that was scanned — carried so a rendered list says WHICH tree it is about, which
    /// matters the moment `VIKE_USER_DATA_DIR` is in play.
    pub root: PathBuf,
    /// The runs, in directory-name order.
    pub runs: Vec<ScannedRun>,
    /// Everything the scan could not use, named.
    pub diagnostics: Vec<ScanDiagnostic>,
}

/// Everything a scan could not use, NAMED — one variant per thing that actually goes wrong, each
/// carrying the path it is about and enough to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanDiagnostic {
    /// The root exists and could not be listed. Deliberately distinct from "no root", which is
    /// silent — see this module's doc.
    RootUnreadable {
        /// The directory that would not open.
        root: PathBuf,
        /// The operating system's own words.
        error: String,
    },

    /// A run's manifest is there and cannot be turned into a row: unreadable, not JSON, or JSON
    /// missing a common field. One variant on purpose: to the user they are one event, and `error`
    /// carries which one it was in the reader's own words.
    RunUnreadable {
        /// The run directory's name.
        run_id: String,
        /// The manifest that could not be used.
        path: PathBuf,
        /// [`RunReadError`]'s own words.
        error: String,
    },

    /// A run directory holding no manifest at all.
    ///
    /// The manifest is written LAST (the crate doc), so this is a run being written RIGHT NOW or one
    /// that stopped between its two writes — not a corrupt one. A [`DiagnosticSeverity::Warning`] for that
    /// reason: the report is the irreplaceable half and is written FIRST, so it may well be there.
    RunUnfinished {
        /// The run directory's name.
        run_id: String,
        /// The directory itself.
        dir: PathBuf,
        /// Whether the report file is there — "the result survived" versus "nothing was kept".
        report: bool,
    },

    /// A manifest whose `run_id` is not the directory it sits in. The run still lists, addressed by
    /// its DIRECTORY — see [`ScannedRun::run_id`].
    RunIdMismatch {
        /// The directory name, which is what the listing uses.
        run_id: String,
        /// What the manifest calls itself.
        declared: String,
        /// The manifest that disagrees.
        path: PathBuf,
    },
}

impl ScanDiagnostic {
    /// Did this cost the user a row they should have had? The two [`DiagnosticSeverity::Warning`]s both leave
    /// something listed or recoverable: an unfinished run still has its report on disk, and a
    /// mismatched id still lists under its directory.
    pub fn severity(&self) -> DiagnosticSeverity {
        match self {
            ScanDiagnostic::RunUnfinished { .. } | ScanDiagnostic::RunIdMismatch { .. } => {
                DiagnosticSeverity::Warning
            }
            ScanDiagnostic::RootUnreadable { .. } | ScanDiagnostic::RunUnreadable { .. } => {
                DiagnosticSeverity::Error
            }
        }
    }
}

impl std::fmt::Display for ScanDiagnostic {
    /// ONE line each, and every line names the fix.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanDiagnostic::RootUnreadable { root, error } => write!(
                f,
                "{}: the runs directory exists but cannot be read ({error}) — nothing under it was \
                 listed; fix its permissions",
                root.display()
            ),
            ScanDiagnostic::RunUnreadable { run_id, path, error } => write!(
                f,
                "{run_id}: {} cannot be used — {error}. The run is on disk and cannot be listed; \
                 open the file, or delete the run directory if it is junk.",
                path.display()
            ),
            ScanDiagnostic::RunUnfinished { run_id, dir, report } => {
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
            ScanDiagnostic::RunIdMismatch { run_id, declared, path } => write!(
                f,
                "{run_id}: {} calls the run '{declared}' — listed under its DIRECTORY, which is \
                 where it is actually found. Fix the manifest's run_id, or rename the directory \
                 back.",
                path.display()
            ),
        }
    }
}

/// Scan `runs_root` — every run of every kind, read through the COMMON manifest.
///
/// `runs_root` comes from `vike_paths::state_path::user_runs_dir` (or a sibling of an already
/// resolved settings directory), resolved by the BINARY.
///
/// Never fails. An absent root is an empty scan with no diagnostic; everything else that goes
/// wrong is a named row.
pub fn scan_runs(runs_root: &Path) -> RunScan {
    let mut scan =
        RunScan { root: runs_root.to_path_buf(), runs: Vec::new(), diagnostics: Vec::new() };
    let entries = match std::fs::read_dir(runs_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return scan,
        Err(e) => {
            scan.diagnostics.push(ScanDiagnostic::RootUnreadable {
                root: runs_root.to_path_buf(),
                error: e.to_string(),
            });
            return scan;
        }
    };

    // A per-ENTRY error is skipped: the entry failed to materialise, so there is no name to attach a
    // diagnostic to — unlike the root failure above, which names the directory.
    let mut folders: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            (!name.starts_with('.')).then_some((name, path))
        })
        .collect();
    folders.sort();

    for (run_id, dir) in folders {
        match read_manifest(&dir) {
            Ok(manifest) => {
                if manifest.run_id != run_id {
                    scan.diagnostics.push(ScanDiagnostic::RunIdMismatch {
                        run_id: run_id.clone(),
                        declared: manifest.run_id.clone(),
                        path: dir.join(MANIFEST_FILE),
                    });
                }
                let report = dir.join(REPORT_FILE);
                let report = report.is_file().then_some(report);
                scan.runs.push(ScannedRun { run_id, dir, manifest, report });
            }
            // Written LAST, so its absence is an unfinished run rather than a broken one.
            Err(RunReadError::Missing { .. }) => {
                let report = dir.join(REPORT_FILE).is_file();
                scan.diagnostics.push(ScanDiagnostic::RunUnfinished { run_id, dir, report });
            }
            Err(RunReadError::Read { path, why } | RunReadError::Parse { path, why }) => {
                scan.diagnostics.push(ScanDiagnostic::RunUnreadable { run_id, path, error: why });
            }
        }
    }
    scan
}
