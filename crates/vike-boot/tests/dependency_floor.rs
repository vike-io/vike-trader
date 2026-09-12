//! **This crate may not acquire a tree.** The whole point of hoisting the startup sequence is that
//! FIVE roots share one order; the whole risk is that they thereby share one another's
//! dependencies.
//!
//! # The two failures this gate is made of, both real
//!
//! 1. ONE normal dependency edge from `vike-tradehub` (the headless daemon on the box that signs
//!    real orders) to `vike-app-core` dragged the ENTIRE egui widget stack into that daemon, for
//!    THREE pure modules. `crates/vike-ops/Cargo.toml` exists because of it, and
//!    `crates/vike-ops/tests/layer_gate.rs` is the gate that came out of it. That gate checks
//!    DIRECTION; it cannot check WEIGHT — rank 25 permits an edge to rank 20 no matter what rank 20
//!    links.
//! 2. `vike-cli` is deliberately DataFusion-free AND transport-free and rides the FAST CI lane. The
//!    credential loader the roots call — `vike_bridge_core::credentials::load_workspace_secrets_from_env`
//!    — lives in the crate that OWNS the ureq/tungstenite/rustls stack, and the obvious way to write
//!    `vike-boot` puts that one edge between `vike-cli` and all of it. `crates/vike-secrets` exists
//!    (ZERO dependencies) precisely so the store can be reached without it, which is why
//!    `vike_boot::Credentials::LoadWith` takes the ROOT's own loader as a function instead.
//!
//! A third, measured rather than argued: a `vike-log` edge would add **43 packages** to `vike-cli`
//! (`tracing-subscriber`, `tracing-appender`, `time`, `regex-automata` and the whole ICU4X `idna`
//! tree). That is why this crate returns a log HOME and the binary calls `vike_log::init`.
//!
//! # The mechanism
//!
//! Text-only over the real `Cargo.toml` files — the same shape as
//! `crates/vike-ops/tests/layer_gate.rs`, `crates/vike-buildinfo/tests/identity_adoption.rs` and
//! `crates/vike-ops/tests/settings_registry.rs`, and the only kind of gate that has held in this
//! repo. No cargo invocation, no new dependency.
//!
//! Two assertions, and they are different questions:
//!
//! * [`the_vike_closure_is_exactly_the_pinned_set`] — the transitive `vike-*` NORMAL closure of this
//!   crate is EXACTLY [`VIKE_CLOSURE`]. An addition is the layering question; a removal is a win
//!   that has to be recorded so the pin keeps meaning what it says.
//! * [`no_crate_in_the_closure_declares_a_heavy_dependency`] — no crate in that closure declares an
//!   external dependency outside [`EXTERNAL_FLOOR`]. This is what makes the first assertion worth
//!   anything: a closure of four light crates is only light while all four stay light, and the way
//!   a transport stack actually arrives is `vike-config` growing a `ureq`.
//!
//! # The declared limitation
//!
//! It sees DECLARED dependencies, not the resolved graph: `serde` pulls `serde_core`, and this gate
//! will never know. That is fine for what it is for — every crate this exists to keep out
//! (`ureq`, `tungstenite`, `rustls`, `tracing-subscriber`, `datafusion`, `eframe`) has to be
//! DECLARED by somebody in the closure to arrive at all. The resolved-tree question is
//! `cargo tree -p vike-cli -e normal`, which is a CI-lane check in the `alerting-standalone` shape
//! and deliberately not re-implemented here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The transitive `vike-*` NORMAL dependency closure of `vike-boot`, this crate included.
///
/// Four crates, and each is in it for one reason: `vike-config` (the settings model and the three
/// refusals this crate sequences), `vike-secrets` (the project walk — ZERO dependencies),
/// `vike-model` (the `state/logs` sub-directory names, already present via vike-config) and
/// `vike-buildinfo` (the identity line — ZERO dependencies).
const VIKE_CLOSURE: &[&str] =
    &["vike-boot", "vike-buildinfo", "vike-config", "vike-model", "vike-secrets"];

/// Every EXTERNAL crate any member of [`VIKE_CLOSURE`] is allowed to declare as a normal
/// dependency.
///
/// Deliberately a floor rather than a ban list. A ban list only stops the crates somebody thought
/// of; this stops the one nobody thought of, which is the shape the failure actually takes — the
/// egui stack arrived through a dependency nobody was looking at either.
///
/// Every entry is a pure-Rust, no-native-code, no-I/O crate. Adding one costs a line here and an
/// argument in the PR; adding `ureq`, `tungstenite`, `rustls`, `tracing-subscriber`, `datafusion`
/// or `eframe` is what this refuses.
const EXTERNAL_FLOOR: &[&str] = &[
    "compact_str", // vike-model: the small-string Event ids
    "indexmap",    // vike-model: insertion-ordered maps (f64 sum order is load-bearing)
    "libm",        // vike-model: erf/exp, no libc dependency
    "serde",       // the file model
    "serde_json",  // vike-model
    "toml",        // vike-config: the human-edited layer's format
    "toml_edit",   // vike-config: comment/format-PRESERVING settings writes (REQ-7 write half) —
    // pure in-memory TOML document editing, no I/O/net/TLS of its own; the same
    // parser family `toml` already rides on. The write path validates with the
    // loader before touching disk, so this crate never widens what a root ACCEPTS.
    "ustr", // vike-model: interned venue/symbol
];

/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the same idiom
/// `crates/vike-ops/tests/layer_gate.rs` and `crates/vike-buildinfo/tests/identity_adoption.rs`
/// use.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Every `[workspace].members` path, verbatim from the root manifest.
fn member_dirs() -> Vec<String> {
    let manifest = read("Cargo.toml");
    let block = manifest
        .split_once("members = [")
        .and_then(|(_, r)| r.split_once("\n]"))
        .expect("root Cargo.toml must have a members = [ ... ] block")
        .0;
    block
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') {
                return None;
            }
            let start = l.find('"')? + 1;
            let end = l[start..].find('"')? + start;
            Some(l[start..end].to_string())
        })
        .collect()
}

/// What a `[section]` header is, as far as normal dependencies are concerned.
///
/// ⚠ **The SUB-TABLE arm is the whole reason this is an enum and not a `bool`.** The first cut asked
/// `header.trim_matches(['[', ']']).rsplit('.').next() == Some("dependencies")`, which reads
/// `[dependencies.ureq]` as the section `"ureq"` — not a dependency table, so the entire sub-table
/// was SKIPPED and the dependency it declares was invisible. Both assertions in this file stayed
/// green with `[dependencies.ureq]` and again with `[dependencies.vike-log]` appended to
/// `crates/vike-boot/Cargo.toml` (measured, both directions), which is a gate that cannot see the
/// two things it exists to refuse. TOML treats `[dependencies] \n ureq = "2"` and
/// `[dependencies.ureq] \n version = "2"` as the same document, so a parser that reads only one of
/// them is not reading manifests.
#[derive(Debug, PartialEq, Eq)]
enum Section {
    /// `[dependencies]`, `[build-dependencies]`, `[target.'cfg(unix)'.dependencies]` — a
    /// platform-gated edge is a normal edge like any other, and a build-dependency is a real
    /// build-graph edge. Every column-0 key INSIDE is a dependency name.
    DepTable,
    /// `[dependencies.ureq]`, `[target.'cfg(windows)'.build-dependencies.cc]` — the dependency's
    /// name is in the HEADER and the lines inside are its fields (`version`, `path`, `features`),
    /// which are emphatically not dependencies.
    DepEntry(String),
    /// Anything else, `[dev-dependencies]` and `[dev-dependencies.tempfile]` included: a dev edge is
    /// deliberately NOT a normal edge (this crate's own `tempfile` is one, and the closure must stay
    /// blind to it).
    Other,
}

/// Classify a `[...]` header. The marker is matched as a whole dotted COMPONENT, so
/// `[target.'cfg(unix)'.dependencies]` and `[dependencies]` land in the same arm and a package
/// literally named `dependencies-foo` cannot masquerade as one.
fn classify(header: &str) -> Section {
    let Some(body) = header.trim().strip_prefix('[').and_then(|h| h.strip_suffix(']')) else {
        return Section::Other;
    };
    let parts: Vec<&str> = body.split('.').map(str::trim).collect();
    // The LAST marker component wins: `[target.'cfg(unix)'.dependencies.foo]` names `foo`.
    let Some(at) = parts.iter().rposition(|p| matches!(*p, "dependencies" | "build-dependencies"))
    else {
        return Section::Other;
    };
    match parts.get(at + 1) {
        None => Section::DepTable,
        Some(name) => Section::DepEntry(name.trim_matches(['"', '\'']).to_string()),
    }
}

struct Manifest {
    name: String,
    /// Normal dependency keys, `vike-*` and external alike.
    normal: BTreeSet<String>,
}

fn parse(dir: &str) -> Manifest {
    let text = read(&format!("{dir}/Cargo.toml"));
    let (name, normal) = parse_text(&text);
    let name = name.unwrap_or_else(|| panic!("no package name in {dir}/Cargo.toml"));
    Manifest { name, normal }
}

/// [`parse`]'s whole body, over TEXT — so the two spelling tests below drive the real parser rather
/// than a copy of it that could drift into agreeing with itself.
fn parse_text(text: &str) -> (Option<String>, BTreeSet<String>) {
    let mut name = None;
    let mut normal = BTreeSet::new();
    let mut section = Section::Other;
    let mut in_package = false;
    for raw in text.lines() {
        // A trailing `# comment` on a header must not defeat the `]` suffix test below.
        let line = match raw.find(" #") {
            Some(i) => &raw[..i],
            None => raw,
        };
        if line.trim_start().starts_with('#') {
            continue;
        }
        // A HEADER is a line that starts at column 0 with `[` and ENDS with `]`. The suffix test
        // matters: a multi-line inline table closes with `] }` at column 0, and treating that as a
        // header would silently end the dependency section and hide every dependency after it.
        if line.starts_with('[') && line.trim_end().ends_with(']') {
            in_package = line.trim() == "[package]";
            section = classify(line);
            // `[dependencies.ureq]` DECLARES `ureq` — the name is the header, not a line inside.
            if let Section::DepEntry(dep) = &section {
                normal.insert(dep.clone());
            }
            continue;
        }
        let Some((key, _)) = line.split_once('=') else { continue };
        let key = key.trim();
        // Dependency keys sit at column 0; an indented `key =` continues an inline table or a
        // multi-line array and is never a dependency of its own.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        if in_package && key == "name" {
            name = Some(line.split('"').nth(1).expect("name = \"...\"").to_string());
        } else if section == Section::DepTable {
            normal.insert(key.to_string());
        }
    }
    (name, normal)
}

fn manifests() -> BTreeMap<String, Manifest> {
    member_dirs().into_iter().map(|d| parse(&d)).map(|m| (m.name.clone(), m)).collect()
}

/// The transitive `vike-*` normal closure of `vike-boot`, computed from the manifests.
fn vike_closure(all: &BTreeMap<String, Manifest>) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec!["vike-boot".to_string()];
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(m) = all.get(&name) else { continue };
        for dep in m.normal.iter().filter(|d| d.starts_with("vike-")) {
            // A crate that dev-depends on itself is filtered by construction (dev tables are not
            // read); a normal self-edge cannot exist.
            stack.push(dep.clone());
        }
    }
    seen
}

#[test]
fn the_vike_closure_is_exactly_the_pinned_set() {
    let all = manifests();
    let measured = vike_closure(&all);
    let pinned: BTreeSet<String> = VIKE_CLOSURE.iter().map(|s| (*s).to_string()).collect();
    let gained: Vec<&String> = measured.difference(&pinned).collect();
    let lost: Vec<&String> = pinned.difference(&measured).collect();
    assert!(
        gained.is_empty() && lost.is_empty(),
        "vike-boot's transitive vike-* normal closure changed.\n  \
         gained (a NEW crate every consumer of vike-boot now links — including vike-cli, which is \
         DataFusion-free and transport-free by design): {gained:?}\n  \
         lost (a WIN — record it by deleting the line): {lost:?}\n\n\
         Before widening VIKE_CLOSURE, check the three answers that are usually better:\n  \
         1. take the thing as a PARAMETER (that is what `Credentials::LoadWith` is — the root's own \
         credential loader, so this crate never links vike-bridge-core's ureq/tungstenite/rustls);\n  \
         2. return DATA the caller acts on (that is what `Booted::log_home` is — so this crate \
         never links vike-log, measured at +43 packages on vike-cli);\n  \
         3. move the shared code DOWN into a crate that is already in the closure.\n\
         measured: {measured:?}"
    );
}

#[test]
fn no_crate_in_the_closure_declares_a_heavy_dependency() {
    let all = manifests();
    let floor: BTreeSet<&str> = EXTERNAL_FLOOR.iter().copied().collect();
    let mut bad: Vec<String> = Vec::new();
    for name in vike_closure(&all) {
        let Some(m) = all.get(&name) else { continue };
        for dep in &m.normal {
            if dep.starts_with("vike-") || floor.contains(dep.as_str()) {
                continue;
            }
            bad.push(format!("  {name} -> {dep}"));
        }
    }
    assert!(
        bad.is_empty(),
        "a crate in vike-boot's closure declares an external dependency outside the floor:\n{}\n\n\
         Every root — the GUI, the two daemons, the recorder and the DataFusion-free CLI — links \
         this closure at startup, so a crate that arrives here arrives in all five. If the \
         dependency is genuinely pure and light, add it to EXTERNAL_FLOOR with the reason; if it is \
         a transport, a TLS stack, a logging subscriber or a query engine, it does not belong \
         below a composition root at all.",
        bad.join("\n")
    );
}

/// A floor, not a count: this gate is textual, so a parser that quietly stopped matching anything
/// would pass both assertions above by seeing an empty tree.
#[test]
fn the_gate_actually_sees_the_manifests() {
    let all = manifests();
    assert!(all.len() >= 40, "only {} workspace members parsed — the parser is broken", all.len());
    let boot = all.get("vike-boot").expect("vike-boot must parse as a workspace member");
    assert!(
        boot.normal.contains("vike-config"),
        "vike-boot must parse as depending on vike-config — the dependency parser is broken"
    );
    let model = all.get("vike-model").expect("vike-model must parse");
    assert!(
        model.normal.contains("serde"),
        "vike-model must parse as depending on serde — the EXTERNAL half of the parser is broken"
    );
}

/// **The sub-table form is READ, and this is pinned directly rather than left to the manifests.**
///
/// No manifest in the closure uses `[dependencies.x]` today, so the floor above cannot notice a
/// parser that stops handling it — which is exactly the state this file shipped in: appending
/// `[dependencies.ureq]` (a transport stack) and `[dependencies.vike-log]` (a closure member) to
/// `crates/vike-boot/Cargo.toml` each left all three assertions GREEN. TOML says the two spellings
/// are one document; a gate that reads only the flat one is bypassed by writing the other.
#[test]
fn the_section_classifier_reads_both_spellings_of_a_dependency() {
    assert_eq!(classify("[dependencies]"), Section::DepTable);
    assert_eq!(classify("[build-dependencies]"), Section::DepTable);
    assert_eq!(classify("[target.'cfg(unix)'.dependencies]"), Section::DepTable);
    // The bypass, both ways it was demonstrated.
    assert_eq!(classify("[dependencies.ureq]"), Section::DepEntry("ureq".to_string()));
    assert_eq!(classify("[dependencies.vike-log]"), Section::DepEntry("vike-log".to_string()));
    assert_eq!(
        classify("[target.'cfg(windows)'.build-dependencies.cc]"),
        Section::DepEntry("cc".to_string())
    );
    // A DEV edge stays invisible in both spellings — this crate's own `tempfile` is one.
    assert_eq!(classify("[dev-dependencies]"), Section::Other);
    assert_eq!(classify("[dev-dependencies.tempfile]"), Section::Other);
    // …and neither a package whose NAME merely contains the marker nor a closing `] }` is a header.
    assert_eq!(classify("[package]"), Section::Other);
    assert_eq!(classify("] }"), Section::Other);
}

/// The parser must reach the same answer for a manifest written either way — a property no real
/// manifest in this workspace exercises, and the one the bypass rode.
#[test]
fn a_sub_table_dependency_parses_as_the_same_edge_as_a_flat_one() {
    let flat = "[package]\nname = \"x\"\n\n[dependencies]\nureq = \"2\"\n";
    let nested = "[package]\nname = \"x\"\n\n[dependencies.ureq]\nversion = \"2\"\n";
    assert_eq!(deps_of(flat), deps_of(nested));
    assert!(deps_of(nested).contains("ureq"));

    // …and a multi-line inline table must not end the section: everything after `] }` at column 0
    // is still inside `[dependencies]`.
    let wrapped = "[package]\nname = \"x\"\n\n[dependencies]\nserde = { version = \"1\", features = [\n    \"derive\",\n] }\nureq = \"2\"\n";
    assert!(deps_of(wrapped).contains("ureq"), "a wrapped inline table hid every later dependency");
}

/// The REAL parser over a literal manifest — never a second copy of it.
fn deps_of(manifest: &str) -> BTreeSet<String> {
    parse_text(manifest).1
}
