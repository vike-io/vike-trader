//! **What is in `<project>/user_data/`, as a list**: the RUNS that exist, and the STUDIES that
//! produce them.
//!
//! # Why a listing is the thing that was missing
//!
//! A result that is not listed does not exist to the person who produced it. Research results went
//! to a ClickHouse an end user does not have, and a strategy backtest printed to stdout and was
//! gone with the scrollback — two producers, two places, neither of them anywhere a user could
//! point at. `crates/vike-runs/src/lib.rs` is the half that made a run leave something behind;
//! this is the half that finds it again.
//!
//! # The runs half is a SURFACE; its rules live below
//!
//! [`list_runs`] is the results surface every run of every kind has to appear in — the listing
//! `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md` pins the study runner
//! beside, so the test that a study run lists beside a backtest run stays an in-crate test here.
//! The SCAN RULES it applies (read the COMMON manifest and nothing else, the directory is the
//! address, an unfinished run is a named row, directory-name order) are
//! `crates/vike-runs/src/scan.rs`'s `scan_runs`, which `vike-cli`'s `runs` verbs read through too:
//! the owner's ruling of 2026-11-09 split the two, recorded in 0031's amendment. The rules are not
//! restated here.
//!
//! # Two decisions, and their reasons
//!
//! **An ABSENT root is silent; an UNREADABLE one is an ERROR with its own row** — the run scan's
//! rule, applied here to the study tiers. A fresh install has no `user_data/` at all — `crates/vike-paths/src/state_path.rs`'s `user_runs_dir` answers
//! with where the directory BELONGS rather than one it found — so "nothing there" is the ordinary
//! state. A root that EXISTS and cannot be listed is the opposite: a permissions bug wearing the
//! "not configured yet" answer looks exactly like a correct fresh install while every run silently
//! vanishes. That is the same line the credential store draws
//! (`crates/vike-secrets/src/store_locator.rs`), for the same reason.
//!
//! **The root is a `&Path` PARAMETER and nothing here resolves one.** The BINARY calls
//! `vike_paths::state_path::user_runs_dir` / `user_studies_dir` and hands the answer down: a
//! library that resolves its own project root reads global state its caller can neither see nor
//! override, which is what `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` ratchets
//! down and why ONE walk decides which project a process is in.
//!
//! # Nothing is skipped in silence, and nothing is COMPILED either
//!
//! Every failure is a named row carrying its path and its fix ([`StudyDiagnostic`] here,
//! `vike_runs::ScanDiagnostic` for the runs) — an entry that vanishes from a list is worse than one
//! that shows as broken, because the first is unanswerable and the second names what to do.
//!
//! ⚠ **A study is LISTED, never compiled or run**, and that is a deliberate boundary rather than
//! an omission: a STRATEGY that cannot build must not be offered as runnable, because it has a
//! `build_strategy` to call, but a listing of studies offers nothing to run. So a folder is
//! a study because it is a FOLDER: no entry-file rule, no name registry, no verdict about whether
//! it would run.
//!
//! ⚠ **This paragraph used to say a study has no execution contract in this workspace AT ALL, and
//! that is no longer true** — `crates/vike-user-research/src/contract.rs` is the contract and
//! `crates/vike-study-runner/src/study_run.rs`'s `run_study_with` is the caller. The boundary above
//! survives the change unaltered and its reason gets STRONGER rather than weaker: a compiled study
//! is resolved through a registry generated at BUILD time, so "would this folder run" is not a
//! question a directory scan can answer at all — a folder added since the binary was compiled is
//! real, listable, and not in the registry. Listing it is the honest answer; refusing to list it
//! would hide a folder the user can see with their own eyes.
//!
//! # Determinism
//!
//! Directory order is whatever the OS feels like. Everything here is sorted before it is returned —
//! runs by directory NAME (the run scan's rule: chronological for a minted id, because only its
//! `<unix-seconds>` prefix is load-bearing), studies by tier and then by name. A results surface
//! whose rows move between two calls is one nobody can read.

use std::path::{Path, PathBuf};

use vike_model::DiagnosticSeverity;
use vike_paths::state_path::{RHAI_EXT, RHAI_SUBDIR, RUST_EXT, RUST_SUBDIR};
use vike_runs::{RunScan, scan_runs};

/// Case-insensitive extension test — see `vike_paths::state_path::RHAI_EXT`.
fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Where a stray tier-root script BELONGS: `<root>/<stem>/<stem>.<ext>`, keeping its own
/// extension, so the message states the fix as a path the user can copy.
fn stray_target(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = path.parent().unwrap_or(Path::new(""));
    parent.join(&stem).join(format!("{stem}.{ext}"))
}

/// Which tier a study was found in — the same split a strategy has
/// (`vike_paths::state_path::STUDIES_SUBDIR`), carried on the row because the difference is one a
/// user acts on: an interpreted study runs on a shipped binary, a compiled one needs a checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudyTier {
    /// `research/studies/rhai` — interpreted.
    Rhai,
    /// `research/studies/rust` — Rust study FILES, built and run sandboxed by a backtest server.
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

/// What one scan of `<project>/user_data/research/studies` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudyListing {
    /// The `studies/` root that was scanned, not either tier under it.
    pub root: PathBuf,
    /// The studies: the `rhai` tier in name order, then the `rust` one.
    pub studies: Vec<ListedStudy>,
    /// Everything the scan could not use, named.
    pub diagnostics: Vec<StudyDiagnostic>,
}

/// Everything the STUDY scan could not use, NAMED — one variant per thing that actually goes wrong,
/// each carrying the path it is about and enough to act on. (The runs' rows are
/// `vike_runs::ScanDiagnostic`, the run scan's own.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StudyDiagnostic {
    /// A study TIER exists and could not be listed. Deliberately distinct from "no tier", which is
    /// silent — see this module's doc.
    RootUnreadable {
        /// The directory that would not open.
        root: PathBuf,
        /// The operating system's own words.
        error: String,
    },

    /// A study script directly in a tier root instead of in a folder of its own — the first-time
    /// mistake of a tree whose unit is a FOLDER.
    StrayStudy {
        /// The file that is not a study.
        path: PathBuf,
    },
}

impl StudyDiagnostic {
    /// Did this cost the user a row they should have had? Both do: a tier that will not open hides
    /// every study in it, and a stray script is a study the user believes is there and is not.
    pub fn severity(&self) -> DiagnosticSeverity {
        DiagnosticSeverity::Error
    }
}

impl std::fmt::Display for StudyDiagnostic {
    /// ONE line each, and every line names the fix — these are read by a person who has already
    /// looked in the directory once and not understood what they saw.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StudyDiagnostic::RootUnreadable { root, error } => write!(
                f,
                "{}: the directory exists but cannot be read ({error}) — nothing under it was \
                 listed; fix its permissions",
                root.display()
            ),
            StudyDiagnostic::StrayStudy { path } => write!(
                f,
                "{}: a script directly in a study tier is NOT a study — a study is a FOLDER. Move \
                 it to {}",
                path.display(),
                stray_target(path).display()
            ),
        }
    }
}

/// THE RESULTS SURFACE: `<project>/user_data/runs`, every run of every kind — the listing decision
/// 0031 pins the study runner beside (`crate::study_run`'s tests list a study run beside a backtest
/// run through it).
///
/// The rules are `crates/vike-runs/src/scan.rs`'s `scan_runs` and this adds none: a second name for
/// one function only because the name records the decision the scan does not carry (0031's
/// 2026-11-09 amendment: "the listing" is this surface, and the scan rules it calls live below).
/// `runs_root` comes from `crates/vike-paths/src/state_path.rs`'s `user_runs_dir`, resolved by the
/// BINARY: this module resolves nothing (see the module doc).
pub fn list_runs(runs_root: &Path) -> RunScan {
    scan_runs(runs_root)
}

/// List `<project>/user_data/research/studies` — BOTH tiers, `rhai/` then `rust/`.
///
/// `studies_root` comes from `crates/vike-paths/src/state_path.rs`'s `user_studies_dir`, resolved
/// by the BINARY.
///
/// ⚠ **Enumeration only.** Nothing is compiled, nothing is run, and no entry-file rule is applied —
/// the module doc carries that argument. A folder is a study; that is the whole contract this
/// function asserts.
///
/// A name claimed in BOTH tiers lists TWICE, one row per tier, and produces no diagnostic. A
/// strategy name in both tiers is a collision because a strategy is ADDRESSED by name alone; a study
/// is addressed by its tier AND its name (the backtest server's `RunStudy` carries both), so the two
/// rows are two studies — and refusing one would silently hide a folder the user can see with their
/// own eyes.
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
            listing.diagnostics.push(StudyDiagnostic::StrayStudy { path });
        }
        for (name, dir) in folders {
            listing.studies.push(ListedStudy { name, dir, tier });
        }
    }
    listing
}

/// One directory's entries, split by kind and SORTED — what [`read_entries`] hands back.
///
/// A named struct rather than a tuple because the two halves mean opposite things: a study tier
/// cares about both, and `entries.files` says which is which at the use site where a `.1` would
/// not.
struct Entries {
    /// The sub-directories, as `(name, path)`, in name order.
    folders: Vec<(String, PathBuf)>,
    /// The files directly inside, in path order.
    files: Vec<PathBuf>,
}

/// One study tier's entries, split and SORTED, under the absent-vs-unreadable rule.
///
/// `None` means there is nothing to scan: either the directory does not exist (silent, the ordinary
/// state) or it could not be listed (a [`StudyDiagnostic::RootUnreadable`] was pushed).
///
/// Dot-entries are skipped: tool droppings are not user content, and skipping them is a rule rather
/// than a silent loss.
///
/// A per-ENTRY error is skipped: the directory entry itself failed to materialise, so there is no
/// name to attach a diagnostic to and no actionable text to put in one — unlike the root failure
/// above, which names the directory.
fn read_entries(root: &Path, diags: &mut Vec<StudyDiagnostic>) -> Option<Entries> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            diags.push(StudyDiagnostic::RootUnreadable {
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

#[path = "listing_tests.rs"]
#[cfg(test)]
mod listing_tests;
