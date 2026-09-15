//! **Every advertised settings LAYER must be reachable** — the layer-level twin of
//! `settings_are_consumed.rs`.
//!
//! # The defect this gates
//!
//! `crates/vike-config/tests/settings_are_consumed.rs` proves every declared KEY has a reader. It
//! cannot see one level up, where the same failure has a bigger blast radius: a LAYER that is
//! implemented, validated, unit-tested and **named in an operator-facing precedence header**, while
//! no composition root ever applies it.
//!
//! That is not hypothetical either. `<project>/vike.toml`, a per-project override layer, shipped
//! with a loader arm, an authority check (`[policy]`/`[flags]` refused BY NAME), a `project`
//! provenance variant, and ~16 tests. Every one of them passed. And all four composition roots —
//! `vike-app`'s `resolve_settings`, `vike-tradehub`'s `resolve_settings`, `vike-cli`'s
//! `resolve_policy` and `vike-cli config show`'s own `execute` — passed `None` for the project
//! directory, so a `vike.toml` setting `preferences.log_file_level` was read by nothing, warned
//! about by nothing, and reported as applicable by `config show`'s header. The authority check
//! never fired either: a `[policy]` table in it should be a hard error, and `config show` exited 0
//! with empty stderr.
//!
//! The consumption gate's own words apply verbatim: positive confirmation of something false is
//! worse than an unimplemented feature, because an unimplemented feature has no output claiming it
//! works. The file layer defaults to `trace`, and `vike_log::file_level_directive`'s doc records
//! what that once cost — 341 GB, on the disk hosting a live trading node.
//!
//! # ⚠ The layer was then REMOVED, and this gate is why that cost one line
//!
//! Wiring it made the header true. **Deleting it** — the decision that followed, because a fifth
//! settings file sitting one level ABOVE `<project>/settings/` and overriding two of the four files
//! inside it puts "which file won?" back into every investigation — made the header true a second
//! way, and the SAME derivation carried both: `PRECEDENCE` is the header, so a removed layer
//! disappears from the operator's screen in the commit that removes it, with nothing to remember.
//! A hand-typed header would have been false in one direction, then false in the other.
//!
//! Direction 3 changed shape with it, and honestly. Its old form said "no production file may call
//! the bare, project-forgetting `load`/`load_with_cli`/`describe`" — but the forgettable `project`
//! parameter is GONE from those signatures, so there is no bare form left to ban and the compiler
//! settles it. What replaced it is the property the removal has to keep true from here on:
//! [`no_production_source_reads_the_removed_project_file`] — nobody re-grows a private read of that
//! file in a root, where it would be a settings layer outside the chain, invisible to directions
//! 1–2 AND to `config show`. Same walk, same file-count floor (a gate that walks nothing passes
//! everything), aimed at what can still go wrong.
//!
//! # The three directions
//!
//! 1. [`the_precedence_table_is_exactly_the_origin_variants`] — [`vike_config::PRECEDENCE`] and
//!    [`Origin`] describe the same set of layers. A new `Origin` variant with no row (or a row with
//!    no variant) fails here, so the header cannot name a layer the resolver does not have, nor
//!    stay silent about one it does.
//! 2. [`the_declared_precedence_is_the_resolved_precedence`] — the ORDER is measured, not asserted
//!    from the source: one scenario per layer, each arming that layer and every layer below it, and
//!    the winner must be the declared one. A layer added to `PRECEDENCE` without a scenario fails
//!    on the length mismatch rather than being silently skipped.
//! 3. [`no_production_source_reads_the_removed_project_file`] — the source walk (see above).
//!
//! Direction 3 is a source walk and 1–2 are behavioural, deliberately: a source walk alone proves
//! only that the code LOOKS right, and `crates/vike-cli/tests/settings_layers_reachable.rs` closes
//! the loop from the other end by driving the SHIPPED binary and watching each layer take effect —
//! plus, now, watching a present `vike.toml` be refused rather than ignored.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_config::{Origin, PRECEDENCE, REMOVED_PROJECT_FILE, describe};

/// The `crates/` directory, from `CARGO_MANIFEST_DIR` (never CWD) — the same idiom every other
/// source-walking gate in this workspace uses.
///
/// ⚠ `parent()`, not `join("..")`. The `..` form yields `crates/vike-config/../..`, whose every
/// descendant path then contains the literal `vike-config` — which silently swallowed the entire
/// walk through the [`SELF_CRATE`] filter below. The file-count floor in [`production_sources`] is
/// what caught it; a gate that walks nothing passes everything.
fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("vike-config sits inside crates/").into()
}

// -------------------------------------------------------------------------------------------
// 1. The table and the resolver describe the same layers
// -------------------------------------------------------------------------------------------

#[test]
fn the_precedence_table_is_exactly_the_origin_variants() {
    // Every variant, spelled out. A new one added to `Origin` fails to compile here first, which is
    // the earliest possible place to be asked "and which layer is that, in what order?".
    let variants = [Origin::Env("VIKE_LOG_DIR"), Origin::File("config.toml"), Origin::Default];

    let mut declared: Vec<&str> = PRECEDENCE.iter().map(|l| l.kind).collect();
    let mut observed: Vec<&str> = variants.iter().map(Origin::kind).collect();
    declared.sort_unstable();
    observed.sort_unstable();
    assert_eq!(
        declared, observed,
        "vike_config::PRECEDENCE and Origin must describe the same layers — a header that names a \
         layer the resolver lacks (or omits one it has) is the defect this file exists for"
    );

    let mut unique = declared.clone();
    unique.dedup();
    assert_eq!(declared, unique, "a layer kind appears twice in PRECEDENCE: {declared:?}");

    // The rendered header is derived from the same table, so it can only name what is in it.
    let line = vike_config::precedence_line();
    for layer in PRECEDENCE {
        assert!(line.contains(layer.label), "precedence_line() omits {:?}: {line}", layer.label);
    }
    // ...and it must not name the layer that was REMOVED. A header naming a file nothing reads is
    // the original defect; naming one that is actively REFUSED would be a stranger version of it.
    assert!(
        !line.contains(REMOVED_PROJECT_FILE),
        "the header names {REMOVED_PROJECT_FILE}, a layer this crate refuses: {line}"
    );
}

// -------------------------------------------------------------------------------------------
// 2. The declared order is the order the resolver really applies
// -------------------------------------------------------------------------------------------

/// Resolve `config.log_dir` with the named layers armed, and return the origin KIND that won.
///
/// One key is enough: it is settable from both non-default layers, which is exactly the property
/// being measured. Everything runs against a `tempfile::TempDir`, so no test here can reach a real
/// settings directory.
fn winning_origin(env_layer: bool, file_layer: bool) -> String {
    let project = tempfile::tempdir().unwrap();
    let settings = project.path().join("settings");
    std::fs::create_dir(&settings).unwrap();

    if file_layer {
        std::fs::write(settings.join("config.toml"), "log_dir = \"/from/file\"\n").unwrap();
    }
    let mut env = HashMap::new();
    if env_layer {
        env.insert("VIKE_LOG_DIR".to_string(), "/from/env".to_string());
    }

    let d = describe(Some(&settings), &env).unwrap();
    let row = d.rows.iter().find(|r| r.key == "config.log_dir").expect("config.log_dir has a row");
    row.origin.kind().to_string()
}

#[test]
fn the_declared_precedence_is_the_resolved_precedence() {
    // One scenario per layer, highest first: arm that layer and everything below it, and it must
    // win. Peeling one layer off the top each time is what makes this an ORDER measurement rather
    // than three independent "this layer works" checks.
    let observed = vec![
        winning_origin(true, true),   // env over everything
        winning_origin(false, true),  // the settings file over the default
        winning_origin(false, false), // nothing set
    ];
    let declared: Vec<String> = PRECEDENCE.iter().map(|l| l.kind.to_string()).collect();

    assert_eq!(
        observed, declared,
        "the printed precedence header must describe reality: PRECEDENCE says {declared:?} but the \
         resolver applies {observed:?}. If a layer was ADDED, add its scenario above — a layer with \
         no scenario is a layer nothing proves the order of."
    );
}

// -------------------------------------------------------------------------------------------
// 3. The removed layer stays removed — no root re-grows a private read of it
// -------------------------------------------------------------------------------------------

/// The declaring crate. It owns the tombstone, the refusal and the message, and names the file in
/// all three — which is exactly why the walk below skips it.
const SELF_CRATE: &str = "vike-config";

/// The vendored IBKR SDK copy: not our code, and never a settings consumer.
const VENDORED: &str = "ibapi";

/// **Nothing outside `vike-config` may name the removed project file in shipped code.**
///
/// The failure this forbids is not hypothetical hand-waving — it is the concrete way the removal
/// could be undone one root at a time. A binary that read `<project>/vike.toml` itself would have a
/// settings layer OUTSIDE the chain: absent from [`PRECEDENCE`] (so directions 1–2 could not see
/// it), absent from `Description::files` (so `config show` could not disclose it), and therefore
/// exactly the "which file won?" question that removing the layer was meant to end — with the extra
/// twist that the loader would be refusing the same file the root had just applied.
///
/// A literal, not a call: there is no single function to key on, and every plausible re-growth
/// (a `Path::join`, a `read_to_string`, a `const`) has to spell the name somewhere.
#[test]
fn no_production_source_reads_the_removed_project_file() {
    let mut offenders = Vec::new();
    for (rel, file) in production_sources() {
        let text = std::fs::read_to_string(&file).unwrap();
        if production_half(&text).contains(REMOVED_PROJECT_FILE) {
            offenders.push(format!("crates/{rel}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "these shipped files name `{REMOVED_PROJECT_FILE}`, the per-project override layer that was \
         REMOVED. A root that reads it privately has a settings layer outside the chain: not in \
         `vike_config::PRECEDENCE`, not in `config show`'s file list, and refused by the loader it \
         sits beside. Settings come from `<project>/settings/*.toml` through `vike_config::load`, \
         and a file that is gone stays gone.\n  {}",
        offenders.join("\n  ")
    );
}

/// Every `src/**/*.rs` under `crates/`, minus this crate and the vendored SDK, as
/// `(path relative to `crates/`, absolute path)`.
///
/// The filters match the RELATIVE path: an absolute one carries the checkout's own directory names,
/// which is how a `vike-config` in the prefix once matched every file and emptied the walk.
///
/// `src/` only: a `tests/` file may legitimately write a `vike.toml` to prove it is refused, which
/// is the opposite of the defect above.
fn production_sources() -> Vec<(String, PathBuf)> {
    let root = crates_dir();
    let mut found = Vec::new();
    walk(&root, &mut found);

    let mut out = Vec::new();
    for path in found {
        let rel = display(path.strip_prefix(&root).expect("walked from the crates directory"));
        let first = rel.split('/').next().unwrap_or_default();
        if rel.contains("/src/") && first != SELF_CRATE && !rel.contains(VENDORED) {
            out.push((rel, path));
        }
    }
    assert!(
        out.len() > 100,
        "the walk found only {} source files — a gate that walks nothing passes everything, which \
         is exactly what a `..` in the root path caused once",
        out.len()
    );
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The half of a file that ships: everything before the first `#[cfg(test)]`, with `//` comments
/// stripped so a doc comment DESCRIBING the removed file is never mistaken for a read of it.
///
/// ⚠ Two admitted blind spots, both of which can only produce a false NEGATIVE (a missed offender),
/// never a false alarm: a `/* */` block comment is not stripped, and a line whose text after a `//`
/// happens to be code is dropped with the comment. Neither shape appears in this workspace; the
/// convention is `#[cfg(test)] mod tests` at the END of a file, which is what makes the split above
/// exact rather than approximate.
fn production_half(text: &str) -> String {
    let head = text.split("#[cfg(test)]").next().unwrap_or_default();
    head.lines().map(|l| l.split("//").next().unwrap_or_default()).collect::<Vec<_>>().join("\n")
}

/// Repo-relative-ish, with forward slashes, so the two `contains` filters above mean the same thing
/// on Windows and Linux.
fn display(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// The walk and the comment stripper are the gate's INPUT, and an input that silently degrades
/// makes direction 3 vacuous — the same "mutation-test every gate" rule that
/// `the_completeness_gate_has_a_non_empty_input` applies next door. `production_sources` carries
/// its own floor; this pins the stripper, which is the half a floor cannot see.
#[test]
fn the_source_walk_reads_code_and_not_comments() {
    let text = "// a comment naming vike.toml\nlet x = \"vike.toml\";\n#[cfg(test)]\nmod t { \
                let y = \"vike.toml\"; }";
    let code = production_half(text);
    assert!(code.contains("let x = \"vike.toml\""), "the shipped line survives: {code:?}");
    assert!(!code.contains("a comment naming"), "comments are stripped: {code:?}");
    assert!(!code.contains("let y"), "the test half is dropped: {code:?}");
}
