//! Which `vike-ops` gates a change can affect: the planner half of [`tables::gate_triggers`].
//!
//! `vike-ops` holds one nextest binary per gate. The planner selects the CRATE whole (it must: the crate
//! stays in `crates=` and in the roster build), and this module answers the finer question the `test`
//! job's filter is built from ([`super::roster::nextest_filter`]): WHICH of its binaries run. The answer
//! is `None` ("every gate", today's behaviour) or `Some(gates)`.
//!
//! # `None` — every doubt runs everything
//!
//! [`select`] answers `None`, and so [`super::plan::Plan::ops_gates`] is `None` and the filter names the
//! whole package, when ANY of these holds:
//!
//!   * there is no file list: `CI_FULL=1` (every tag, release and manual full matrix) or a diff base that
//!     does not resolve — decided by [`super::plan::compute`], which never asks;
//!   * the plan escalated to the full roster (a workspace-global file), for the same reason;
//!   * a root-manifest dependency edit reaches `vike-ops` (the gates link third-party crates, which no
//!     `links` list names), decided by [`super::plan::compute`], which never asks;
//!   * a file the crate itself compiles changed: anything under the crate that is not a gate's own
//!     sources and not prose — `src/`, the manifest, a build script — or a file under `tests/common/`
//!     (the helpers most gates `#[path]`-include), or a file under `tests/` that belongs to no gate
//!     (a fixture, a regression seed, a helper nobody declared);
//!   * the crate's manifest cannot be read as a list of `[[test]]` rows, or lists none;
//!   * a row needs the diff text (a `tokens` list) and [`super::git::diff_patch`] could not produce it.
//!
//! # `Some(gates)` — a gate runs when
//!
//!   * it has NO row (the safe default), or its row is [`Trigger::Always`];
//!   * one of its own source files changed: the gate's root file, or any file in the directory beside it
//!     that carries its name (its `#[path]` children). Derived from the manifest, so a folder move changes
//!     nothing here, and the drift gate holds that no gate includes another gate's files;
//!   * its row's `paths`, `suffixes` or `tokens` match ([`Trigger::On`]): a path PREFIX or suffix of a
//!     changed file, or a substring of an ADDED or REMOVED diff line ([`changed_lines`]);
//!   * it LINKS a crate the change reached through the GRAPH: the gates are test binaries that link the
//!     crates their code names (`STORE_KINDS`, the planner itself), so a row keyed on file names cannot see
//!     what an edit of such a crate changes. `reached` is every crate with an edit that can be linked plus
//!     everything depending on one of them normally, and a gate runs when its row's `links`
//!     ([`GateTrigger::links`], derived from the gate's sources and held equal to them by
//!     `xtask/tests/gate_triggers_gate.rs`) or a [`tables::gate_triggers::LinkKeep`] meets it. A gate whose
//!     `links` are empty is never selected this way: for an edit of any crate its row decides, as for a
//!     crate `vike-ops` does not link at all. A crate the gate does not name cannot reach it: rustc does not
//!     load an unreferenced crate.
//!
//! A test binary that cargo auto-discovers (`tests/*.rs`, `tests/*/main.rs`) and the manifest does not
//! list is still a gate here, with no row, so it always runs. Dropping it from the filter would be a
//! silent skip of a binary nobody wrote a trigger for.
//!
//! What this module does NOT cover is stated where it matters: the crate's own library unit tests and
//! doctests are not gates, and only a `src/` change can move them (which runs everything).

use std::collections::BTreeSet;
use std::path::Path;

#[cfg(doc)]
use super::tables;
use super::tables::gate_triggers::{GateTrigger, LINK_KEEPS, Trigger};

/// The crate whose gates are narrowed. A name, because the planner's roster is names.
pub const OPS_CRATE: &str = "vike-ops";

/// One test binary of [`OPS_CRATE`]: its nextest binary name and its root source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// The `[[test]] name`, which is the binary name nextest's `binary(=NAME)` matches.
    pub name: String,
    /// The root source file, relative to the crate directory (`tests/<folder>/<name>.rs`).
    pub root: String,
}

impl Gate {
    /// The directory holding this gate's `#[path]` children, relative to the crate directory: the file's
    /// path without `.rs` (`tests/x/g.rs` -> `tests/x/g`), or its own directory for a `main.rs`.
    fn own_dir(&self) -> &str {
        match self.root.strip_suffix("/main.rs") {
            Some(dir) => dir,
            None => self.root.strip_suffix(".rs").unwrap_or(&self.root),
        }
    }

    /// True when `rel` (relative to the crate directory) is this gate's root file or lies in its directory.
    pub fn owns(&self, rel: &str) -> bool {
        rel == self.root || rel.strip_prefix(self.own_dir()).is_some_and(|r| r.starts_with('/'))
    }
}

/// The `[[test]]` rows of a manifest, in file order. `None` when a row has no `name` (a manifest this
/// reader does not understand, which the caller answers by running everything).
///
/// A line reader, not a TOML parser: this crate takes ONE normal dependency, and a `[[test]]` row is
/// `name = "…"` and `path = "…"` on their own lines (the drift gate parses the same manifest with a real
/// TOML parser and holds the two readers to the same list). A row without a `path` is cargo's default,
/// `tests/<name>.rs`.
pub fn manifest_gates(manifest: &str) -> Option<Vec<Gate>> {
    fn string_value(line: &str, key: &str) -> Option<String> {
        let rest = line
            .trim()
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?
            .trim_start()
            .strip_prefix('"')?;
        Some(rest[..rest.find('"')?].to_string())
    }
    fn flush(row: Option<(Option<String>, Option<String>)>, out: &mut Vec<Gate>) -> Option<()> {
        if let Some((name, path)) = row {
            let name = name?;
            let root = path.unwrap_or_else(|| format!("tests/{name}.rs"));
            out.push(Gate { name, root });
        }
        Some(())
    }

    let mut out = Vec::new();
    let mut row: Option<(Option<String>, Option<String>)> = None;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            flush(row.take(), &mut out)?;
            if t.split('#').next().unwrap_or("").trim() == "[[test]]" {
                row = Some((None, None));
            }
        } else if let Some((name, path)) = row.as_mut() {
            if let Some(v) = string_value(t, "name") {
                *name = Some(v);
            } else if let Some(v) = string_value(t, "path") {
                *path = Some(v);
            }
        }
    }
    flush(row.take(), &mut out)?;
    Some(out)
}

/// The test binaries cargo auto-discovers under `crate_dir/tests` — `tests/*.rs` and `tests/*/main.rs` —
/// that the manifest does not already name. Sorted, so the filter is deterministic.
fn autodiscovered(crate_dir: &Path, known: &[Gate]) -> Vec<Gate> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(crate_dir.join("tests")) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        let Some(file) = p.file_name().and_then(|n| n.to_str()) else { continue };
        let found = if p.is_dir() {
            p.join("main.rs").is_file().then(|| (file.to_string(), format!("tests/{file}/main.rs")))
        } else {
            file.strip_suffix(".rs").map(|stem| (stem.to_string(), format!("tests/{file}")))
        };
        if let Some((name, root)) = found
            && !known.iter().any(|g| g.name == name || g.root == root)
        {
            out.push(Gate { name, root });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Every test binary of the crate at `crate_dir`: the manifest's `[[test]]` rows, then the
/// auto-discovered rest. `None` when the manifest cannot be read, is not understood, or lists no row —
/// the caller runs every gate.
pub fn crate_gates(crate_dir: &Path) -> Option<Vec<Gate>> {
    let text = std::fs::read_to_string(crate_dir.join("Cargo.toml")).ok()?;
    let mut gates = manifest_gates(&text)?;
    if gates.is_empty() {
        return None;
    }
    let extra = autodiscovered(crate_dir, &gates);
    gates.extend(extra);
    Some(gates)
}

/// The text of the lines a `git diff -U0` ADDS or REMOVES, without their `+`/`-` marker. File headers
/// (`--- a/x`, `+++ b/x`) are not content: they are skipped by position — everything between a
/// `diff --git` line and the first `@@` — not by looking for `+++`, which would also drop a removed line
/// that begins with `--`.
pub fn changed_lines(patch: &str) -> Vec<&str> {
    let mut in_hunk = false;
    let mut out = Vec::new();
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            in_hunk = false;
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk && (line.starts_with('+') || line.starts_with('-')) {
            out.push(&line[1..]);
        }
    }
    out
}

/// How one changed file under the crate bears on its gates.
enum Bearing<'a> {
    /// It can change any gate: run them all.
    Everything,
    /// It is one gate's own source.
    Gate(&'a str),
    /// Prose or a file no gate compiles; only the rows can say who reads it.
    RowsOnly,
}

fn bearing<'a>(rel: &str, gates: &'a [Gate]) -> Bearing<'a> {
    if let Some(under_tests) = rel.strip_prefix("tests/") {
        if under_tests.starts_with("common/") {
            return Bearing::Everything;
        }
        return match gates.iter().find(|g| g.owns(rel)) {
            Some(g) => Bearing::Gate(&g.name),
            None => Bearing::Everything,
        };
    }
    if rel.ends_with(".md") { Bearing::RowsOnly } else { Bearing::Everything }
}

/// True when the change reached a crate this row's gate links, or one a
/// [`tables::gate_triggers::LinkKeep`] names for it.
fn links_reached(row: &GateTrigger, reached: &BTreeSet<String>) -> bool {
    row.links.iter().any(|c| reached.contains(*c))
        || LINK_KEEPS.iter().any(|k| k.gate == row.gate && reached.contains(k.edit_of))
}

/// The gates that must run for this change, or `None` for "every gate".
///
/// `files` is the whole changed-file list (repo-relative, what the rows are matched against);
/// `ops_rel` is the subset the crate owns, relative to the crate directory; `reached` is the crates the
/// change's linkable edits reach through NORMAL dependency edges, the edited crates included (empty for a
/// change that edits no crate); `read_patch` produces the diff text and is called AT MOST ONCE, and only
/// when a row not already selected has tokens. The module doc is the rule list; this function is its only
/// implementation.
pub fn select(
    files: &[String],
    ops_rel: &[String],
    gates: &[Gate],
    rows: &[GateTrigger],
    reached: &BTreeSet<String>,
    read_patch: impl FnOnce() -> Option<String>,
) -> Option<BTreeSet<String>> {
    let mut run: BTreeSet<String> = BTreeSet::new();
    for rel in ops_rel {
        match bearing(rel, gates) {
            Bearing::Everything => return None,
            Bearing::Gate(name) => {
                run.insert(name.to_string());
            }
            Bearing::RowsOnly => {}
        }
    }

    let mut token_rows: Vec<(&str, &[&str])> = Vec::new();
    for gate in gates {
        if run.contains(&gate.name) {
            continue;
        }
        let mine: Vec<&GateTrigger> = rows.iter().filter(|r| r.gate == gate.name).collect();
        if mine.is_empty() || mine.iter().any(|r| links_reached(r, reached)) {
            run.insert(gate.name.clone());
            continue;
        }
        for row in mine {
            match row.trigger {
                Trigger::Always => {
                    run.insert(gate.name.clone());
                }
                Trigger::On { paths, suffixes, tokens } => {
                    let by_file = files.iter().any(|f| {
                        paths.iter().any(|p| f.starts_with(p))
                            || suffixes.iter().any(|s| f.ends_with(s))
                    });
                    if by_file {
                        run.insert(gate.name.clone());
                    } else if !tokens.is_empty() {
                        token_rows.push((gate.name.as_str(), tokens));
                    }
                }
            }
        }
    }

    // Only a gate not already selected by a path or suffix is worth the diff.
    token_rows.retain(|(name, _)| !run.contains(*name));
    if !token_rows.is_empty() {
        let patch = read_patch()?;
        let lines = changed_lines(&patch);
        for (name, tokens) in token_rows {
            if lines.iter().any(|l| tokens.iter().any(|t| l.contains(t))) {
                run.insert(name.to_string());
            }
        }
    }
    Some(run)
}
