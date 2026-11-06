//! What a `Cargo.lock` change moved: whether it only ADDED packages (the one global change that may
//! stay narrow), which package names' resolved versions moved, and which package ENTRIES changed at
//! all ([`changed_lock_entries`], the latency gate's pull-request question).
//!
//! Two readers of one file, on purpose. `lock_pkgs` is lenient (it keeps every `name`/`version`
//! pair it finds and ignores everything else), which is what the additive-only exemption and
//! [`changed_packages`] were written against. [`changed_lock_entries`] is STRICT: it compares every
//! field of every `[[package]]` entry and answers `None` for any text it cannot account for, because
//! its caller fires a measurement on `None` and narrows one on `Some`.

use std::collections::{BTreeMap, BTreeSet};
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

/// The package NAMES whose set of resolved versions differs between two lockfiles: added, removed
/// or moved. A name appears once however many of its versions changed.
///
/// A lockfile that parses to nothing (a failed read) yields every name of the other side, so the
/// answer errs toward the larger set — and the caller refuses an empty `old` outright
/// ([`lock_additive_only`]'s own rule).
pub fn changed_packages(old_text: &str, new_text: &str) -> BTreeSet<String> {
    let (old, new) = (lock_pkgs(old_text), lock_pkgs(new_text));
    old.symmetric_difference(&new).map(|(name, _)| name.clone()).collect()
}

/// A `[[package]]` entry's identity: `(name, version, source)`, the source empty for a path package.
/// Two versions of one name are two entries.
type EntryId = (String, String, String);

/// A lockfile as [`changed_lock_entries`] compares it: every `[[package]]` entry's fields (key -> the
/// value's text, a multi-line array's items joined by newlines in the order written) and, in order,
/// every line that belongs to no entry — the format `version`, every comment, every other table
/// (`[[patch.unused]]`, `[metadata]`) and its lines.
struct LockEntries {
    entries: BTreeMap<EntryId, BTreeMap<String, String>>,
    other: Vec<String>,
}

/// [`LockEntries`] of `text`, or `None` for a text this reader cannot account for line by line: a
/// line inside an entry that is not `key = value` (or an array item), a key written twice in one
/// entry, an unterminated array, an entry with no `name` or `version`, or two entries with one
/// identity. Blank lines are dropped; every other line lands somewhere.
fn lock_entries(text: &str) -> Option<LockEntries> {
    let mut out = LockEntries { entries: BTreeMap::new(), other: Vec::new() };
    // The entry being read (None = outside every entry) and the array key it has open, if any.
    let mut cur: Option<BTreeMap<String, String>> = None;
    let mut open: Option<(String, Vec<String>)> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, mut items)) = open.take() {
            if line == "]" {
                put(cur.as_mut()?, key, items.join("\n"))?;
            } else {
                items.push(line.trim_end_matches(',').trim().to_string());
                open = Some((key, items));
            }
            continue;
        }
        if line.starts_with('#') {
            out.other.push(line.to_string());
            continue;
        }
        if line.starts_with('[') && !line.contains('=') {
            if let Some(e) = cur.take() {
                finish_entry(e, &mut out)?;
            }
            if line == "[[package]]" {
                cur = Some(BTreeMap::new());
            } else {
                out.other.push(line.to_string());
            }
            continue;
        }
        let Some(e) = cur.as_mut() else {
            out.other.push(line.to_string());
            continue;
        };
        let (key, value) = line.split_once('=')?;
        let (key, value) = (key.trim().to_string(), value.trim());
        if value == "[" {
            open = Some((key, Vec::new()));
        } else {
            put(e, key, value.trim_matches('"').to_string())?;
        }
    }
    if open.is_some() {
        return None;
    }
    if let Some(e) = cur.take() {
        finish_entry(e, &mut out)?;
    }
    Some(out)
}

/// `key = value` into an entry, `None` when the entry already holds `key`.
fn put(entry: &mut BTreeMap<String, String>, key: String, value: String) -> Option<()> {
    if entry.contains_key(&key) {
        return None;
    }
    entry.insert(key, value);
    Some(())
}

/// A finished entry into `out` under its [`EntryId`]; `None` when it has no `name` or `version`, or
/// another entry already has its identity.
fn finish_entry(entry: BTreeMap<String, String>, out: &mut LockEntries) -> Option<()> {
    let source = entry.get("source").cloned().unwrap_or_default();
    let id = (entry.get("name")?.clone(), entry.get("version")?.clone(), source);
    if out.entries.contains_key(&id) {
        return None;
    }
    out.entries.insert(id, entry);
    Some(())
}

/// The NAMES of the `[[package]]` entries that differ between two lockfiles: an entry added, removed,
/// or with any field changed (version, source, checksum, dependency list, any key cargo writes), so a
/// version bump names the package, and a new or dropped dependency edge names the package that
/// declares it. `None` = the two cannot be compared: either text does not read (`lock_entries`), or
/// the lines outside the entries differ (the lock format version, a comment, `[[patch.unused]]`),
/// which no package name can stand for.
///
/// Only the answer `Some` may narrow anything. A name stands for every version of it: two versions of
/// `x`, one of them moved, name `x` once.
pub fn changed_lock_entries(old_text: &str, new_text: &str) -> Option<BTreeSet<String>> {
    let (old, new) = (lock_entries(old_text)?, lock_entries(new_text)?);
    if old.other != new.other {
        return None;
    }
    let ids: BTreeSet<&EntryId> = old.entries.keys().chain(new.entries.keys()).collect();
    Some(
        ids.into_iter()
            .filter(|id| old.entries.get(*id) != new.entries.get(*id))
            .map(|(name, _, _)| name.clone())
            .collect(),
    )
}
