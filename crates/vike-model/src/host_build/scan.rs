//! One TIER's listing, the half of a host's scan both hosts share: directory listing -> the folders
//! that will compile, plus every objection naming its path.
//!
//! ## The scanned convention
//!
//! ```text
//! <tier>/<name>/
//! ├─ <name>.rs        entry file — stem MUST equal the folder name (the rhai tier's rule)
//! └─ …                whatever else the host reads beside it (presets, recipes, a manifest)
//! ```
//!
//! An absent tier is the ordinary empty state (every CI checkout, every fresh clone): no folders,
//! no errors. Present-but-malformed content is an ERROR, never a skip — silence is how the
//! strategy tier spent months being mistaken for a working mechanism. Folders come back SORTED, so
//! the generated registry is deterministic regardless of filesystem order.
//!
//! ## The one place the hosts differ: a folder with no entry file
//!
//! [`EntryPolicy`] carries it. The strategy tier SKIPS such a folder: a presets-only folder for a
//! BUILT-IN strategy is legitimate there (the vike-studio-core `StrategyBody::Native` convention).
//! The research tier skips one only when it holds no `.rs` at all (a recipes-only folder); Rust
//! under the wrong file name is an error, because the operator's file is there and nothing will
//! ever compile it (`crates/vike-studio-core/src/user_strategies/load.rs`'s `MissingEntry` is the
//! same diagnostic).
//!
//! Host-specific per-folder work — the strategy tier's `strategy.toml`, the research tier's
//! both-tiers warning — runs in the host over [`TierScan::found`], after [`scan_tier`].

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// One folder that will compile: its entry file exists and its name is valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The folder name == registry name == entry-file stem. Validated by [`valid_name`].
    pub name: String,
    /// The folder itself, `<tier>/<name>`, as listed — where a host reads its per-folder extras.
    pub dir: PathBuf,
    /// `<dir>/<name>.rs`, exactly as found (rendered with forward slashes).
    pub entry: PathBuf,
}

/// One tier's scan: the folders found plus every objection, each naming its path.
#[derive(Debug, Default)]
pub struct TierScan {
    /// Sorted by folder path.
    pub found: Vec<Found>,
    /// One line per malformed folder, each starting with that folder's path.
    pub errors: Vec<String>,
}

/// What a folder WITHOUT `<name>.rs` means — the module doc's "The one place the hosts differ".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryPolicy {
    /// Skip it silently: a presets-only folder for a built-in (the strategy tier).
    SkipEntryless,
    /// An error when it holds ANY `.rs` file, a silent skip when it holds none (the research
    /// tier).
    ErrorIfRustWithoutEntry,
}

/// A registry name must be a lowercase Rust-identifier-safe token: the generated module is
/// `user_<name>` and the match arm quotes it, so the charset is the whole safety argument.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

/// Scan `tier`: every sub-directory, sorted, checked in this order — a non-UTF-8 name, a missing
/// `<name>.rs` (judged by `policy`), a name [`valid_name`] refuses. `noun` names the folder kind
/// in the bad-name error (`"strategy"`, `"study"`). An absent or unreadable tier is the empty
/// outcome.
pub fn scan_tier(tier: &Path, noun: &str, policy: EntryPolicy) -> TierScan {
    let mut out = TierScan::default();
    let entries = match fs::read_dir(tier) {
        Ok(e) => e,
        Err(_) => return out, // absent tier = empty registry, the CI/default state
    };
    let mut dirs: Vec<PathBuf> =
        entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    dirs.sort(); // deterministic generation regardless of filesystem order
    for dir in dirs {
        let name = match dir.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => {
                out.errors.push(format!("{}: folder name is not valid UTF-8", dir.display()));
                continue;
            }
        };
        let entry = dir.join(format!("{name}.rs"));
        if !entry.is_file() {
            if policy == EntryPolicy::ErrorIfRustWithoutEntry && holds_any_rust(&dir) {
                // The `MissingEntry` case: Rust IS here and none of it will ever be compiled.
                out.errors.push(format!(
                    "{}: holds Rust source but no `{name}.rs` — the entry file's stem must equal \
                     the folder name (the rhai tier's rule), or nothing in this folder is \
                     compiled",
                    dir.display()
                ));
            }
            // Otherwise: a presets-only or recipes-only folder — not ours, not an error.
            continue;
        }
        if !valid_name(&name) {
            out.errors.push(format!(
                "{}: {noun} folder name must match [a-z][a-z0-9_]* (it becomes the registry name \
                 and the generated module name)",
                dir.display()
            ));
            continue;
        }
        out.found.push(Found { name, dir, entry });
    }
    out
}

/// Does this folder hold ANY `.rs` file? The question that separates "half-written folder" (an
/// error under [`EntryPolicy::ErrorIfRustWithoutEntry`]) from "recipes only" (silent).
pub fn holds_any_rust(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return false };
    entries.filter_map(|e| e.ok()).any(|e| {
        let p = e.path();
        p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("rs")
    })
}

/// The sub-directory names directly under `dir`; empty when `dir` is absent or unreadable (the
/// ordinary nothing-there state, not a finding). The research host lists its interpreted tier
/// with it to notice a name present in both tiers.
pub fn folder_names(dir: &Path) -> BTreeSet<String> {
    let Ok(entries) = fs::read_dir(dir) else { return BTreeSet::new() };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect()
}
