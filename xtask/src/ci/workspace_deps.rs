//! A root-manifest change confined to `[workspace.dependencies]`, and the members it reaches.
//!
//! The root `Cargo.toml` escalates to the full roster by name ([`super::selection::escalates`]),
//! which is right for `[workspace] members`, `[profile]`, `[workspace.lints]`, `[patch]` and
//! `rust-toolchain` — each reaches every crate. A dependency entry does not: it reaches the crates
//! that link the package it names, and the lockfile says which packages moved.
//!
//! [`consumers`] answers `Some(members)` only when the WHOLE change to the root manifest is entries
//! of that one section; every other shape answers `None`, which the caller reads as "global".
//!
//! ⚠ **The unit is the package NAME, in the resolved closure — not the literal `workspace = true`
//! line.** A member that does not inherit the entry but links the package through another crate is
//! rebuilt all the same: bump `tokio` and a crate that only depends on `hyper` still compiles the
//! new `tokio`. So a member is selected when its [`super::graph::Graph::closure`] holds a changed
//! name, which includes every `workspace = true` consumer and the members behind them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::git;
use super::graph::Graph;
use super::lockfile;

/// The root manifest split at `[workspace.dependencies]`.
struct Split {
    /// Every meaningful line OUTSIDE the section (comments and blank lines dropped), in order.
    outside: Vec<String>,
    /// Entry key -> its meaningful lines. A `[workspace.dependencies.x]` table is entry `x`.
    entries: BTreeMap<String, Vec<String>>,
}

fn is_noise(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with('#')
}

/// The name inside a column-0 table header line (`[a.b]`, `[[bin]]`), comment stripped.
fn header_of(line: &str) -> Option<String> {
    if !line.starts_with('[') {
        return None;
    }
    let t = line.split('#').next().unwrap_or_default().trim();
    t.ends_with(']').then(|| t.trim_matches(|c| c == '[' || c == ']').trim().to_string())
}

fn unquote(key: &str) -> String {
    key.trim().trim_matches(|c| c == '"' || c == '\'').to_string()
}

/// `Some(key)` for a column-0 `key = ...` line.
fn entry_key(line: &str) -> Option<String> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    line.split_once('=').map(|(k, _)| unquote(k)).filter(|k| !k.is_empty())
}

/// `None` when a line inside the section belongs to no entry — a shape this reader does not know.
fn split(text: &str) -> Option<Split> {
    let mut s = Split { outside: Vec::new(), entries: BTreeMap::new() };
    let (mut in_deps, mut table) = (false, false);
    let mut cur: Option<String> = None;
    for line in text.lines() {
        if let Some(h) = header_of(line) {
            (in_deps, table, cur) = (false, false, None);
            if h == "workspace.dependencies" {
                in_deps = true;
            } else if let Some(k) = h.strip_prefix("workspace.dependencies.") {
                (in_deps, table) = (true, true);
                cur = Some(unquote(k));
                s.entries.entry(unquote(k)).or_default();
            } else {
                s.outside.push(line.trim_end().to_string());
            }
            continue;
        }
        if is_noise(line) {
            continue;
        }
        if !in_deps {
            s.outside.push(line.trim_end().to_string());
            continue;
        }
        if !table && let Some(k) = entry_key(line) {
            cur = Some(k);
        }
        s.entries.entry(cur.clone()?).or_default().push(line.trim().to_string());
    }
    Some(s)
}

/// The value of `package = "..."` in an entry's text, when the entry renames its package.
fn package_of(entry: &[String]) -> Option<String> {
    let text = entry.join(" ");
    let mut from = 0;
    while let Some(at) = text[from..].find("package") {
        let start = from + at;
        from = start + "package".len();
        let before_ok = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-');
        let rest = text[from..].trim_start();
        if before_ok && let Some(v) = rest.strip_prefix('=') {
            let v = v.trim_start().strip_prefix('"')?;
            return v.split_once('"').map(|(name, _)| name.to_string());
        }
    }
    None
}

/// The package names of the entries that differ between two root manifests, or `None` when the
/// two differ anywhere ELSE (or either does not read).
///
/// A changed entry contributes its key and, when it renames, its `package` — added, removed and
/// edited entries alike, because a removed or edited one changes what its consumers link.
pub fn changed_entries(old: &str, new: &str) -> Option<BTreeSet<String>> {
    let (old, new) = (split(old)?, split(new)?);
    if old.outside != new.outside {
        return None;
    }
    let keys: BTreeSet<&String> = old.entries.keys().chain(new.entries.keys()).collect();
    let mut names = BTreeSet::new();
    for k in keys {
        let (a, b) = (old.entries.get(k), new.entries.get(k));
        if a == b {
            continue;
        }
        names.insert(k.clone());
        names.extend([a, b].into_iter().flatten().filter_map(|e| package_of(e)));
    }
    Some(names)
}

/// The members a dependency-only change to the root manifest reaches, or `None` = a global edit.
///
/// `files` is the change's file list: the lockfile is read only when it is in it. Both texts come
/// from git (`base` and `HEAD`), never the working tree. Any text git cannot produce — a new or
/// deleted root manifest or lockfile — is `None`.
///
/// The packages are the changed ENTRIES (a feature edit moves no lockfile line) plus every package
/// whose resolved versions moved in `Cargo.lock` (a bump moves transitive ones too); a member is
/// reached when its resolved closure holds any of them.
pub fn consumers(files: &[String], base: &str, cwd: &Path, g: &Graph) -> Option<BTreeSet<String>> {
    if !files.iter().any(|f| f == "Cargo.toml") {
        return None;
    }
    let old = git::show_at(base, "Cargo.toml", cwd)?;
    let new = git::show_at("HEAD", "Cargo.toml", cwd)?;
    let mut packages = changed_entries(&old, &new)?;
    if files.iter().any(|f| f == "Cargo.lock") {
        let old_lock = git::show_at(base, "Cargo.lock", cwd)?;
        let new_lock = git::show_at("HEAD", "Cargo.lock", cwd)?;
        packages.extend(lockfile::changed_packages(&old_lock, &new_lock));
    }
    Some(
        g.closure
            .iter()
            .filter(|(_, deps)| deps.iter().any(|d| packages.contains(d)))
            .map(|(member, _)| member.clone())
            .collect(),
    )
}
