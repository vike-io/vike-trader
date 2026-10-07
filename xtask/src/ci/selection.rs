//! What a changed file selects beyond its owning crates: gate crates, lane readers, suites, escalation.

use std::collections::BTreeSet;
use std::path::Path;

#[cfg(doc)]
use super::git;
use super::graph::{Rdeps, affected_from};
use super::{set, tables};

/// The gate-owning crates a change must select REGARDLESS of which crate owns the file.
///
/// Every entry reads an input OUTSIDE its own crate's sources — the whole `.rs` tree, the markdown
/// tree, `deploy/`, a repo-root manifest, every crate's `Cargo.toml` — so the reverse-dep closure,
/// derived from changed CRATES, structurally cannot select them. A gate that cannot run on its own change class is not a gate:
/// measured, a docs-only PR selected zero crates and not one of the five prose gates ran, and a
/// `server.json`-only PR selected zero crates and did not run the manifest's drift gate.
pub fn gate_crates_for(files: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if files.iter().any(|f| f.ends_with(".rs")) {
        out.insert(tables::gate_crates::SETTINGS_GATE_CRATE.to_string());
    }
    let doc_input = |f: &String| {
        tables::gate_crates::DOC_GATE_INPUT_SUFFIXES.iter().any(|s| f.ends_with(s))
            || tables::gate_crates::DOC_GATE_INPUT_PREFIXES.iter().any(|p| f.starts_with(p))
    };
    if files.iter().any(doc_input) {
        out.insert(tables::gate_crates::DOC_GATE_CRATE.to_string());
    }
    // A crate's OWN manifest (the root one has no `/` and escalates on its own) — see
    // `tables::gate_crates::CRATE_MANIFEST_GATE_CRATE` for the gates that read every one.
    if files.iter().any(|f| f.ends_with("/Cargo.toml")) {
        out.insert(tables::gate_crates::CRATE_MANIFEST_GATE_CRATE.to_string());
    }
    // Exact paths rather than a prefix — see `tables::gate_crates::MANIFEST_GATE_INPUTS` for why the repository
    // root cannot be swept wholesale.
    if files.iter().any(|f| tables::gate_crates::MANIFEST_GATE_INPUTS.contains(&f.as_str())) {
        out.insert(tables::gate_crates::MANIFEST_GATE_CRATE.to_string());
    }
    // Exact paths again — the files the docs-data gate reads; see `tables::gate_crates::DOCS_DATA_GATE_INPUTS`.
    if files.iter().any(|f| tables::gate_crates::DOCS_DATA_GATE_INPUTS.contains(&f.as_str())) {
        out.insert(tables::gate_crates::DOCS_DATA_GATE_CRATE.to_string());
    }
    out
}

/// The crates a changed file must put in the test LANE because their tests READ it, though it
/// belongs to no crate — never `affected`, so no feature suite fires for them.
///
/// Two sources: any path [`tables::escalation::is_global_exempt`] answers for selects
/// [`tables::readers::EXEMPT_INPUT_GATE_CRATE`] (whose gates read or run all of them), and a path matching a
/// [`tables::readers::LANE_INPUT_READERS`] row selects that row's crates. Keyed on the FILES, like
/// [`gate_crates_for`]; it differs from that function only in where its answer goes, and the
/// difference is the point — `gate_crates_for` feeds `affected` (so its crates fire the suites
/// they trigger), this feeds only `ordered`.
///
/// It does not ask whether a path ESCALATES: an escalating change plans the whole roster anyway,
/// so a lane addition there is a no-op rather than an error.
pub fn lane_crates_for(files: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if files.iter().any(|f| tables::escalation::is_global_exempt(f)) {
        out.insert(tables::readers::EXEMPT_INPUT_GATE_CRATE.to_string());
    }
    for (prefix, suffix, readers) in tables::readers::LANE_INPUT_READERS {
        if files.iter().any(|f| f.starts_with(prefix) && f.ends_with(suffix)) {
            out.extend(readers.iter().map(|r| (*r).to_string()));
        }
    }
    out
}

/// The feature-suite keys a changed file must FIRE because the only build that compiles its
/// reader is that suite — [`tables::readers::SUITE_INPUT_READERS`]. Keyed on the FILES like
/// [`lane_crates_for`], and for the same reason: the file owns no crate, so no trigger set can see
/// it. It adds suite legs only; no crate joins `affected` for it.
pub fn suite_keys_for(files: &[String]) -> BTreeSet<&'static str> {
    tables::readers::SUITE_INPUT_READERS
        .iter()
        .filter(|(prefix, suffix, _)| {
            files.iter().any(|f| f.starts_with(prefix) && f.ends_with(suffix))
        })
        .map(|(_, _, key)| *key)
        .collect()
}

/// True when a change to `f` invalidates the narrow selection and plans the FULL roster.
///
/// [`tables::escalation::GLOBAL_PREFIXES`] (and the root `Cargo.toml`), minus what [`tables::escalation::is_global_exempt`]
/// exempts — and an exempted path is exempt only while it still EXISTS under `cwd`. A deleted or
/// renamed-away file is the one change to an exempted input that can break a test no
/// [`tables::readers::LANE_INPUT_READERS`] row selects: every crate's citations and existence checks name
/// these paths, and an edit cannot move an existence check while a deletion always does. That
/// rule needs the OLD path of a rename in the file list, which is why [`git::diff_names`] passes
/// `--no-renames`.
///
/// ⚠ The exemption is tested FIRST, and it is a per-FILE decision rather than a per-commit one: a
/// change touching `scripts/qa_shots.sh` AND `scripts/ci_feature_suite.sh` still escalates,
/// because the second is global on its own.
///
/// ⚠ Those two examples are REAL files on purpose. `crates/vike-ops/tests/docs/citation_gate.rs` reads a
/// BACKTICKED path as a citation and requires it to resolve, so an invented placeholder name under
/// a real directory reddens CI — which is exactly what an earlier draft of the comment this
/// replaced did. An illustrative path must either name a file that exists or carry no backticks.
pub fn escalates(f: &str, cwd: &Path) -> bool {
    if tables::escalation::is_global_exempt(f) && cwd.join(f).exists() {
        return false;
    }
    f == "Cargo.toml" || tables::escalation::GLOBAL_PREFIXES.iter().any(|p| f.starts_with(p))
}

/// True iff this change can move the p99 the the latency box latency gate measures.
///
/// ⚠ Deliberately NOT `"vike-core" in affected`, which is what it used to be. That expression READS
/// as "could this touch the hot fold?" and it does not, because `affected` is widened by two
/// mechanisms that have nothing to do with the measured binary: one hop of DEV-dependents (vike-core
/// dev-depends upward on vike-mm/vike-strategy/vike-script, none of which is in its compiled
/// library), and ESCALATION (any workspace-global file made `affected` every crate, so `deny.toml`,
/// the `justfile` or `.github/CODEOWNERS` each bought an ~11-minute microsecond measurement on
/// reserved cores).
///
/// So the question is asked directly instead: does the change touch a file that compiles INTO the
/// measured binary, or the harness that measures it? The crate half walks NORMAL edges only (an
/// empty `radj_dev` is passed), which drops the dev-only false positives while keeping every real
/// one — a `vike-model` change still reaches `vike-core` through `radj`. It also self-maintains
/// DOWNWARD: a crate inserted below any [`tables::roster::LATENCY_CRATES`] member fires the gate with no
/// edit to that table.
pub fn latency_affected(files: &[String], changed_crates: &BTreeSet<String>, radj: &Rdeps) -> bool {
    let is_latency_input = |f: &String| {
        f.as_str() == "Cargo.toml"
            || tables::escalation::LATENCY_GLOBAL_PREFIXES.iter().any(|p| f.starts_with(p))
    };
    if files.iter().any(is_latency_input) {
        return true;
    }
    let latency = set(tables::roster::LATENCY_CRATES);
    affected_from(changed_crates, radj, &Rdeps::new()).intersection(&latency).next().is_some()
}

/// Pack the affected suite keys into `features` matrix legs.
///
/// Input: the affected [`tables::feature_suites::FEATURE_SUITES`] keys, in declaration order. Output: one entry
/// per matrix leg — a key on its own, or every affected member of one [`tables::suite_rules::SUITE_GROUPS`]
/// set joined by single spaces (in input order), emitted at the position of the set's FIRST
/// affected member. A set with one affected member emits that bare key, so a narrow PR's plan is
/// byte-identical to the pre-packing one; packing can only ever MERGE jobs, never add work to one.
///
/// The space join is the contract with ci.yml's `features` job: `${{ matrix.suite }}` is passed
/// to `scripts/ci_feature_suite.sh` unquoted, the shell word-splits it, and the script runs each
/// argument's `case` arm in order — the same invocations the keys ran as separate jobs.
pub fn pack_suites(keys: &[String]) -> Vec<String> {
    let mut emitted: Vec<usize> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        match tables::suite_rules::SUITE_GROUPS.iter().position(|g| g.contains(&k.as_str())) {
            None => out.push(k.clone()),
            Some(gi) => {
                if emitted.contains(&gi) {
                    continue;
                }
                emitted.push(gi);
                let leg: Vec<&str> = keys
                    .iter()
                    .map(String::as_str)
                    .filter(|key| tables::suite_rules::SUITE_GROUPS[gi].contains(key))
                    .collect();
                out.push(leg.join(" "));
            }
        }
    }
    out
}

/// True iff the the CI box LightGBM job should run.
///
/// Deliberately the same shape as the `hist` trigger — an intersection with the reverse-dep closure —
/// and NOT [`latency_affected`]'s narrower question. This job asks "could this change break the
/// suites that drive the trainer?", which the closure genuinely answers; the latency gate asks "did
/// the MEASURED binary move?", which it does not.
pub fn lightgbm_affected(affected: &BTreeSet<String>) -> bool {
    affected.intersection(&set(tables::roster::LIGHTGBM_CRATES)).next().is_some()
}
