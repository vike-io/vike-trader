//! Whether a `Cargo.lock` change only ADDED packages, the one global change that may stay narrow.

use std::collections::BTreeSet;
use std::path::Path;

use super::git;

/// `(name, version)` pairs from a `Cargo.lock`'s `[[package]]` blocks.
fn lock_pkgs(text: &str) -> BTreeSet<(String, String)> {
    let mut pkgs = BTreeSet::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        let s = line.trim();
        if s == "[[package]]" {
            name = None;
        } else if let Some(rest) = s.strip_prefix("name = ") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = s.strip_prefix("version = ")
            && let Some(n) = &name
        {
            pkgs.insert((n.clone(), rest.trim().trim_matches('"').to_string()));
        }
    }
    pkgs
}

/// True iff every `(name, version)` in the OLD lock is still present in the NEW one — i.e. the change
/// only ADDED packages and moved or removed nothing.
///
/// Such a diff cannot change any already-resolved crate's version, so crates unrelated to the
/// accompanying source change are unaffected and the full-matrix escalation is not needed. An empty
/// old lock (parse failure, no base) is `false`, so the answer stays conservative.
pub fn lock_additive_only(old_text: &str, new_text: &str) -> bool {
    let old = lock_pkgs(old_text);
    !old.is_empty() && old.is_subset(&lock_pkgs(new_text))
}

/// Disk/git wrapper over [`lock_additive_only`]: the base commit's `Cargo.lock` against HEAD's on
/// disk. Any error (no base ref, unreadable file) is `false` — the conservative full matrix.
pub(super) fn cargo_lock_additive_only(base: &str, cwd: &Path) -> bool {
    let Some(old) = git::show_cargo_lock(base, cwd) else { return false };
    let Ok(new) = std::fs::read_to_string(cwd.join("Cargo.lock")) else { return false };
    lock_additive_only(&old, &new)
}
