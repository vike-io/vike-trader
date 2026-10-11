//! The workspace graph, read from `cargo metadata` — and the two closures over it.
//!
//! ⚠ **THE TWO REVERSE-ADJACENCIES ARE SEPARATE, AND THAT IS THE WHOLE DESIGN.** A dev-dependency is
//! a TEST edge, not a layering edge, and this workspace dev-depends UPWARD on purpose (`vike-mm`
//! \[dev\]-> `vike-core` for its white-box `LiveBroker` tests; `vike-bridge-core` \[dev\]-> nine venue
//! crates for the shared conformance harnesses; `crates/vike-ops` \[dev\]-> this crate). Feeding those
//! back into one graph makes it CYCLIC, and a transitive walk then escapes the layering entirely:
//! `oanda -> bridge-core =dev=> binance -> core =dev=> script -> indicators` is how one
//! indicator-registry edit reached `vike-oanda`, the DataFusion lane and every Polymarket suite —
//! 31 crates where 9 were affected.
//!
//! So [`Graph::radj`] carries NORMAL (+build) edges only and is walked TRANSITIVELY — that IS the
//! real layering, and it is acyclic — while [`Graph::radj_dev`] carries dev edges and is applied ONE
//! HOP (see [`affected_from`]): if a crate's TESTS use a changed crate it still gets tested, we just
//! never walk THROUGH that edge and inherit the changed crate's whole reverse-world.
//!
//! `crates/vike-ops/tests/architecture/layer_gate.rs` draws the same distinction over the manifests for the same
//! reason, and says so.
//!
//! A THIRD reverse map, [`Graph::rfeatures`], is not an adjacency and is never walked: it records
//! which features of a member each dependency entry turns on, read from the DECLARED dependency lists
//! (`cargo metadata`'s resolve graph carries no per-edge features). It answers one question,
//! [`Graph::feature_enablers`], for the one kind of file whose reach is decided by a feature: a module
//! compiled only under `#[cfg(any(test, feature = "F"))]` ([`feature_test_module`]). It leaves
//! [`Graph::radj`] and [`Graph::radj_dev`] exactly as they are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use super::tables;

/// Crate name -> the crates that depend on it.
pub type Rdeps = BTreeMap<String, BTreeSet<String>>;

/// The workspace, as the plan needs it.
pub struct Graph {
    /// Absolute, forward-slashed manifest DIRECTORY -> package name, workspace members only.
    pub dirs: BTreeMap<String, String>,
    /// Reverse NORMAL (+build) edges. Walked transitively.
    pub radj: Rdeps,
    /// Reverse DEV edges. Applied one hop.
    pub radj_dev: Rdeps,
    /// Reverse FEATURE edges: member -> every dependency entry of another member (or of itself)
    /// that names it. Never walked; read through [`Graph::feature_enablers`].
    pub rfeatures: BTreeMap<String, Vec<FeatureEdge>>,
    /// Every workspace member's package name.
    pub names: BTreeSet<String>,
    /// Member -> the NAMES of every package in its resolved dependency closure (normal, build and
    /// dev, transitively, members and third-party alike). Names only: two versions of one name are
    /// one entry, so a changed version of `x` selects every member that links ANY `x` — the larger set.
    pub closure: BTreeMap<String, BTreeSet<String>>,
}

/// One dependency entry INTO a workspace member, as far as that member's features go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureEdge {
    /// The member that declares the entry.
    pub dependent: String,
    /// A `[dependencies]` or `[build-dependencies]` entry (or one whose kind could not be read):
    /// what it turns on is compiled into the dependent's SHIPPED build. `false` for a dev entry.
    pub normal: bool,
    /// The depended-on member's features the entry turns on: its own `features` list, `default`
    /// unless `default-features = false`, every feature the DEPENDENT's `[features]` table forwards
    /// to it (`"dep/f"`, `"dep?/f"`; conditional on a feature of the dependent, counted as on), all
    /// closed over the depended-on member's own `[features]` implications. `None` when any of that
    /// could not be read, or when cargo resolved an edge no entry was read for: EVERY feature, so the
    /// reader fails closed.
    pub features: Option<BTreeSet<String>>,
}

impl Graph {
    /// The direct dependents whose entry for `owner` turns its feature `feature` on, as
    /// `(dev, normal)`. A dependent with ANY normal entry doing so is in `normal` alone: that entry
    /// links the feature's code into its shipped build, so for that dependent the module is
    /// production. One hop by construction: what the dependents' own dependents enable is not read.
    pub fn feature_enablers(
        &self,
        owner: &str,
        feature: &str,
    ) -> (BTreeSet<String>, BTreeSet<String>) {
        let (mut dev, mut normal) = (BTreeSet::new(), BTreeSet::new());
        for e in self.rfeatures.get(owner).into_iter().flatten() {
            if e.features.as_ref().is_none_or(|on| on.contains(feature)) {
                let side = if e.normal { &mut normal } else { &mut dev };
                side.insert(e.dependent.clone());
            }
        }
        dev.retain(|d| !normal.contains(d));
        (dev, normal)
    }
}

/// Repo paths and cargo's `manifest_path` are compared as strings, so both sides are normalised to
/// forward slashes first — cargo reports `\` on the Windows dev box while `git diff --name-only`
/// reports `/` on both platforms, and an unnormalised comparison would match nothing there.
fn slashes(s: &str) -> String {
    s.replace('\\', "/")
}

/// `path`, made absolute against `cwd` and LEXICALLY normalised.
///
/// ⚠ Lexical on purpose: it must NOT resolve symlinks, and must not start to. The manifest dirs come
/// from cargo and the diff paths from git, and on a checkout reached THROUGH a symlink the two spell
/// the same directory differently — canonicalising one side and not the other is how every path
/// stops comparing equal and every crate stops being selected.
fn abspath(path: &str, cwd: &Path) -> String {
    let joined = if Path::new(path).is_absolute() {
        slashes(path)
    } else {
        format!("{}/{}", slashes(&cwd.to_string_lossy()), slashes(path))
    };
    let mut out: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "." => {}
            ".." => {
                out.pop();
            }
            "" if !out.is_empty() => {}
            s => out.push(s),
        }
    }
    out.join("/")
}

/// The crate a repo path belongs to — the member whose manifest directory is its LONGEST matching
/// prefix — and the path RELATIVE to that directory. `None` for a path no member owns (`docs/`,
/// `content/`, `fixtures/`, the workflows).
///
/// It answers BOTH halves because the relative path is only meaningful against the directory that
/// decided the name: `super::plan::compute` needs the name to seed the closure and the remainder to ask
/// [`is_test_target_source`], and a second longest-prefix walk for the second half would be a copy
/// able to rot away from this one. There is deliberately no name-only wrapper — this workspace does
/// not keep a second spelling of an answer alive once the call sites have moved.
pub fn owner_of(
    path: &str,
    dirs: &BTreeMap<String, String>,
    cwd: &Path,
) -> Option<(String, String)> {
    let ap = abspath(path, cwd);
    let mut best: Option<(&str, &str)> = None;
    for (d, name) in dirs {
        let hit = ap == *d || ap.starts_with(&format!("{d}/"));
        if hit && best.map(|(bd, _)| d.len() > bd.len()).unwrap_or(true) {
            best = Some((d.as_str(), name.as_str()));
        }
    }
    best.map(|(d, name)| {
        let rel = ap.strip_prefix(d).unwrap_or("").trim_start_matches('/').to_string();
        (name.to_string(), rel)
    })
}

/// True for a changed file that is a crate-owned **document**: a file inside the owning crate's
/// directory whose suffix is in [`tables::roster::UNLINKED_DOC_SUFFIXES`] (`.md`: `CLAUDE.md`, a README,
/// a `PROVENANCE.md`, anywhere under the crate), named relative to that crate.
///
/// Like [`is_test_target_source`] this is the claim "nothing outside this crate can link this file",
/// and what it does with it is the same: the crate stays in `changed` (its own tests run, and may read
/// the page at run time — aster's `testnet_claim_gate.rs` reads its own `CLAUDE.md`) but never reaches
/// `linkable`, so the edit seeds no reverse-dependency walk, no API-reference build, no dependency-only
/// suite trigger, no `vike-ops` gate link. Unlike a test-target source it is ALSO no input of the latency
/// gate: a test-target source can be the harness, a document cannot be any part of the measured binary.
///
/// The claim rests on a property of the CODE, not of the layout: no compiled source `include_str!`s,
/// `include_bytes!`s or `include!`s a crate's `.md`. `.rs`-only [`is_test_target_source`] avoids such a
/// dependence by leaving data files conservative; a document has no conservative form that still
/// narrows anything, so the dependence is real and is held by
/// `xtask/tests/ci_plan_gate/test_target_source.rs`'s `no_compiled_source_includes_a_crate_markdown_file`
/// (it fails when one appears), not by this function. Only a file a crate OWNS is a crate document: a
/// `.md` that [`owner_of`] maps to no crate (`docs/`, the repository root, `content/`) never reaches
/// this predicate, and the planner treats it as before — it selects the prose-gate crates through
/// `super::selection::gate_crates_for` and nothing else.
pub fn is_crate_document(rel: &str) -> bool {
    tables::roster::UNLINKED_DOC_SUFFIXES.iter().any(|s| rel.ends_with(s))
}

/// True for a changed file that is a **test-target source**: a `.rs` file under the owning crate's
/// `tests/`, `benches/` or `examples/` directory, named relative to that crate.
///
/// ⚠ **This is, with [`is_crate_document`] (and the `cfg(test)` classifiers below), what lets a change select
/// its crate WITHOUT its reverse-dep world** (`super::plan::compute`), so the claim it makes has to be exact: *nothing outside this crate can link
/// this file.* Cargo auto-discovers each of those directories into its OWN compilation unit — an
/// integration-test binary, a bench harness, an example — none of which is a library target, so no
/// `[dependencies]` edge can reach one. A dependent crate compiling this crate's lib does not
/// compile these files at all, which is exactly why their reverse-dep closure buys nothing.
///
/// Three deliberate NARROWINGS, each measured on the real tree rather than assumed:
///
/// * **`.rs` only — a DATA file under `tests/` stays a full change.** `src/` really does reach into
///   `tests/fixtures/`: `crates/bridges/ig/src/rest.rs`, `crates/vike-ml/src/model/infer.rs` and
///   `crates/vike-mount/src/server_time.rs` all
///   `include_str!` a fixture from there. Every one of those sites is under `#[cfg(test)]` today, so
///   a dependent's build genuinely does not see them — but that is a property of the CODE, and this
///   predicate is about the LAYOUT. A rule that had to re-verify a `cfg` attribute to stay correct
///   would be one refactor away from silently under-selecting, so fixtures simply keep the
///   conservative answer.
/// * **`src/bin/` is NOT covered**, though a binary is equally unlinkable. A test in another crate
///   can still SPAWN one by path, and cargo models no edge for that — so the reverse-dep closure is
///   the only thing standing behind it. No measured win asked for the risk.
/// * **The latency gate reads the same classification, with the harness carved out.** `super::plan::compute`
///   seeds `latency_affected` and `latency_direct` from the crates owning a file that is NOT test-only, via
///   `super::selection::latency_compiled`, and keeps every file of the `runtime_latency` binary
///   (`tables::roster::LATENCY_HARNESS_FILES`) in even though the harness,
///   `crates/vike-core/tests/runtime_latency.rs`, is itself a test-target source.
///
/// `xtask/tests/ci_plan_gate.rs` holds all four claims, including a structural sweep of
/// the real tree asserting no `src/` file, `build.rs` or `build/` module `include!`s a `.rs` path
/// under these directories — the one way the first bullet's reasoning could stop holding.
pub fn is_test_target_source(rel: &str) -> bool {
    rel.ends_with(".rs")
        && (rel.starts_with("tests/")
            || rel.starts_with("benches/")
            || rel.starts_with("examples/"))
}

/// True for a `.rs` file under the owning crate's `src/` that is compiled ONLY under `cfg(test)` —
/// the second kind of file whose change cannot reach a dependent, and the one
/// [`is_test_target_source`] cannot see because it is a property of the CODE, not of the layout.
/// [`feature_test_module`] is the third kind, read by the same walk.
///
/// A file qualifies when it carries a file-level `#![cfg(test)]`, or when a `mod name;` declaration
/// in a sibling or ancestor file RESOLVES TO IT and either carries `#[cfg(test)]` itself or sits in
/// a file that is test-only in turn. Resolution follows the compiler's own rule — `a/b.rs` declares
/// `a/b/name.rs` or `a/b/name/mod.rs`, a `lib.rs`/`main.rs`/`mod.rs` declares beside itself, and
/// `#[path = "p"]` is relative to the declaring file's directory — because the repo's dominant shape
/// is `#[path = "x_tests.rs"] #[cfg(test)] mod x_tests;` (618 of them), which a conventional-location
/// rule cannot see at all.
///
/// ⚠ This is the CONVERSE of `vike_model::libm_walk::cfg_test_module_rel_files`, which the
/// source-walking gates use to answer "which files does this `#[cfg(test)] mod` declaration pull
/// IN"; this asks "is THIS file pulled in only under `cfg(test)`". It is not a second copy of that
/// function, and it cannot call it either: this crate takes no `vike-*` dependency (a planner that
/// had to be compiled with the workspace it plans could not plan a workspace that does not build).
///
/// Every shape this does not understand answers `false`, the conservative answer that keeps the
/// reverse-dep walk: a file that cannot be read (a deletion), a declaration indented inside an inline
/// module, `cfg` forms other than the bare `#[cfg(test)]`, `src/bin/`, the crate root, and the
/// declaring file itself (a production file, which walks on its own).
///
/// ⚠ Why this reads files when [`is_test_target_source`] is deliberately a pure predicate over the
/// path: that function's doc refuses a rule that "had to re-verify a `cfg` attribute to stay
/// correct". This one DOES re-verify it, on every call, from the declaring file as it stands in the
/// checkout being planned, so it cannot go stale the way a name pattern (`tests/`, `*_tests.rs`)
/// would the day a production module is named `tests`.
pub fn is_cfg_test_module(crate_dir: &Path, rel: &str) -> bool {
    src_gate(crate_dir, rel) == Gate::CfgTest
}

/// `Some(F)` for a `.rs` file under the owning crate's `src/` that is compiled ONLY under `cfg(test)`
/// or with the crate's own feature `F` on: a `test-support` module. Such a file reaches exactly the
/// builds that turn `F` on, and `super::plan::compute` asks [`Graph::feature_enablers`] which those
/// are instead of walking every dependent.
///
/// The declaration chain is resolved exactly as for [`is_cfg_test_module`] (`#[path]`, ancestor
/// declarers, a plain `mod x;` child inheriting its parent's gate), and every declaration that
/// RESOLVES to the file must gate it on the same `F`: each link is either a plain `mod` or carries
/// `#[cfg(any(test, feature = "F"))]`, written on one line, in that order, with any whitespace the
/// compiler accepts. A `#[cfg(test)]` link anywhere in the chain makes the file
/// [`is_cfg_test_module`] instead (narrower: no dependent compiles it at all).
///
/// Everything else answers `None` and keeps the walk, as before this rule existed: another `cfg`
/// shape (`any(test, debug_assertions)`, `feature = "F"` alone, `all(test, …)`, the arms reversed or
/// a third arm), an attribute split over lines, a file-level `#![cfg(any(…))]`, a chain that gates on
/// two DIFFERENT features, and a file one declaration reaches gated and another reaches plainly.
///
/// ⚠ What this answer is worth rests on an edge property it does not check: that no crate names the
/// module in code without turning `F` on itself. The roster build unifies features, so such a crate
/// would COMPILE in CI on another crate's dev edge while its tests were never planned for the
/// module's edit (they would still BUILD: `test` builds the whole roster).
/// `crates/vike-ops/tests/architecture/test_surface_gate.rs` keeps every edge enabling a test feature a dev
/// edge, and `xtask/tests/ci_plan_gate/feature_test_module.rs` holds the real
/// consumers of `vike_model::libm_walk` to the property.
pub fn feature_test_module(crate_dir: &Path, rel: &str) -> Option<String> {
    match src_gate(crate_dir, rel) {
        Gate::Feature(f) => Some(f),
        Gate::Production | Gate::CfgTest => None,
    }
}

/// When a `src/` file is compiled, as far as the declarations that pull it in say.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Gate {
    /// In an ordinary build of the crate, or not provably otherwise.
    Production,
    /// Only under `cfg(test)`.
    CfgTest,
    /// Only under `cfg(test)` or with the crate's own feature of this name on.
    Feature(String),
}

fn src_gate(crate_dir: &Path, rel: &str) -> Gate {
    if rel.starts_with("src/") && rel.ends_with(".rs") && !rel.starts_with("src/bin/") {
        gate_of(crate_dir, rel, 0)
    } else {
        Gate::Production
    }
}

/// Every declaration that resolves to `rel` compiles it, so the answer is their JOIN (one plain
/// production path makes the file production) — except that ONE `#[cfg(test)]` link decides on its
/// own, which is the rule [`is_cfg_test_module`] had before features were read, kept as it was.
fn gate_of(crate_dir: &Path, rel: &str, depth: usize) -> Gate {
    if depth > 8 || rel == "src/lib.rs" || rel == "src/main.rs" {
        return Gate::Production;
    }
    let read = |p: &str| std::fs::read_to_string(crate_dir.join(p)).ok();
    if read(rel).is_some_and(|s| has_inner_cfg_test(&s)) {
        return Gate::CfgTest;
    }
    let mut reached: Option<Gate> = None;
    // A declaring file is a sibling of `rel` or lives in one of its ancestor directories, up to `src/`.
    let mut dir = parent_dir(rel).to_string();
    loop {
        let mut entries: Vec<String> = std::fs::read_dir(crate_dir.join(&dir))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".rs"))
            .map(|n| format!("{dir}/{n}"))
            .collect();
        entries.sort();
        for decl_file in entries.iter().filter(|f| f.as_str() != rel) {
            let Some(src) = read(decl_file) else { continue };
            for d in module_declarations(&src) {
                if !declaration_targets(decl_file, &d, rel) {
                    continue;
                }
                if d.cfg_test {
                    return Gate::CfgTest;
                }
                // The declaration's own gate AND the gate of the file that declares it.
                let here = match (d.features.as_slice(), gate_of(crate_dir, decl_file, depth + 1)) {
                    (_, Gate::CfgTest) => return Gate::CfgTest,
                    ([], outer) => outer,
                    ([f], Gate::Production) => Gate::Feature(f.clone()),
                    ([f], Gate::Feature(outer)) if *f == outer => Gate::Feature(outer),
                    // Two different features: compiled when test, or BOTH on. Not this rule's shape.
                    _ => Gate::Production,
                };
                reached = Some(match reached {
                    Some(seen) if seen != here => Gate::Production,
                    _ => here,
                });
            }
        }
        if dir == "src" || !dir.contains('/') {
            return reached.unwrap_or(Gate::Production);
        }
        dir = parent_dir(&dir).to_string();
    }
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// One `mod name;` declaration found at column 0 of a file.
struct ModDecl {
    name: String,
    cfg_test: bool,
    /// The `F` of every `#[cfg(any(test, feature = "F"))]` on it, sorted and distinct.
    features: Vec<String>,
    path: Option<String>,
}

/// Whether `decl`, written in `decl_file`, names the file `target`.
fn declaration_targets(decl_file: &str, decl: &ModDecl, target: &str) -> bool {
    let dir = parent_dir(decl_file);
    if let Some(p) = &decl.path {
        return !p.contains("..") && format!("{dir}/{}", p.trim_start_matches("./")) == target;
    }
    let stem = decl_file.rsplit('/').next().unwrap_or("").trim_end_matches(".rs");
    let base = if matches!(stem, "mod" | "lib" | "main") {
        dir.to_string()
    } else {
        format!("{dir}/{stem}")
    };
    target == format!("{base}/{}.rs", decl.name) || target == format!("{base}/{}/mod.rs", decl.name)
}

/// Every `mod name;` that starts at column 0, with the attributes written on its own line or on the
/// attribute lines directly above it. An indented one is inside an inline module, where `#[path]`
/// and the file layout mean something else, so it is not reported.
fn module_declarations(src: &str) -> Vec<ModDecl> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        if raw.starts_with(char::is_whitespace) {
            continue;
        }
        let mut attrs: Vec<String> = Vec::new();
        let mut rest = raw.trim();
        while rest.starts_with("#[") {
            let Some(end) = rest.find(']') else { break };
            attrs.push(rest[..=end].to_string());
            rest = rest[end + 1..].trim();
        }
        let rest = rest.split("//").next().unwrap_or("").trim();
        let rest = if let Some(r) = rest.strip_prefix("pub(") {
            r.split_once(')').map_or("", |(_, tail)| tail.trim())
        } else {
            rest.strip_prefix("pub ").unwrap_or(rest).trim()
        };
        let Some(name) = rest.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let mut j = i;
        while j > 0 {
            let a = lines[j - 1].trim();
            if a.starts_with("#[") && a.ends_with(']') {
                attrs.push(a.to_string());
            } else if !a.starts_with("//") {
                break;
            }
            j -= 1;
        }
        let path = attrs.iter().find_map(|a| {
            a.strip_prefix("#[path = \"").and_then(|r| r.strip_suffix("\"]")).map(str::to_string)
        });
        let mut features: Vec<String> = attrs.iter().filter_map(|a| feature_test_cfg(a)).collect();
        features.sort();
        features.dedup();
        out.push(ModDecl {
            name: name.to_string(),
            cfg_test: attrs.iter().any(|a| a == "#[cfg(test)]"),
            features,
            path,
        });
    }
    out
}

/// The `F` of `#[cfg(any(test, feature = "F"))]`, and `None` for every other attribute.
///
/// Whitespace outside the string literal is ignored, as the compiler ignores it; nothing else is
/// normalised. The arms reversed, a third arm or a trailing comma mean the same to the compiler and
/// still answer `None`: an attribute this does not read keeps the walk, which is the safe side.
fn feature_test_cfg(attr: &str) -> Option<String> {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let (head, rest) = attr.split_once('"')?;
    let (feature, tail) = rest.split_once('"')?;
    let named = !feature.is_empty()
        && feature.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '+' | '.'));
    (named && squash(head) == "#[cfg(any(test,feature=" && squash(tail) == "))]")
        .then(|| feature.to_string())
}

/// A file-level `#![cfg(test)]`, which may follow blank lines, comments and other inner attributes
/// and nothing else.
fn has_inner_cfg_test(src: &str) -> bool {
    for line in src.lines() {
        let l = line.trim();
        if l == "#![cfg(test)]" {
            return true;
        }
        if !(l.is_empty() || l.starts_with("//") || l.starts_with("#![")) {
            return false;
        }
    }
    false
}

/// Read the workspace graph.
///
/// ⚠ Deliberately WITHOUT `--locked`, and `xtask/tests/local_gate_mirrors_ci/release_tag.rs`'s
/// `the_release_workflow_can_never_resolve_the_tagged_commit` exists because of it: this call can
/// refresh a stale `Cargo.lock` on disk, so `scripts/ci_lockfile_gate.sh` must run BEFORE it or it
/// would inspect a lockfile this step had already rewritten.
///
/// Why the flag stays off rather than making that ordering unnecessary: this function runs on EVERY
/// `just` invocation (the justfile's `ci_crates` calls it, and `just` evaluates assignments eagerly),
/// so `--locked` would turn a developer's half-finished dependency edit into a hard failure of every
/// recipe in the file — including the recipes that would fix it. The flip condition is a `just`
/// entry point that does not need the roster, not an argument about tidiness.
pub fn load_graph(cwd: &Path) -> Result<Graph, String> {
    parse_metadata(&load_metadata(cwd)?, cwd)
}

/// The raw `cargo metadata --format-version 1` document — the spawn half of [`load_graph`], split
/// out so a second renderer over the same document (`crate::docs_bins`, the `bins.json` docs-data
/// asset) shares one loader and one error path. Same `--locked` caveat as [`load_graph`]'s doc.
///
/// # Errors
/// Cargo could not be spawned, exited non-zero, or printed something that is not JSON.
pub fn load_metadata(cwd: &Path) -> Result<serde_json::Value, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let out = Command::new(&cargo)
        .args(["metadata", "--format-version", "1"])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("could not run `{cargo} metadata`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{cargo} metadata` failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata is not JSON: {e}"))
}

/// The parse, split from the process spawn so a test can drive it over a planted document.
pub fn parse_metadata(meta: &serde_json::Value, cwd: &Path) -> Result<Graph, String> {
    let members: BTreeSet<&str> = meta["workspace_members"]
        .as_array()
        .ok_or("cargo metadata has no workspace_members array")?
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    let packages = meta["packages"].as_array().ok_or("cargo metadata has no packages array")?;
    let mut id_name: BTreeMap<&str, &str> = BTreeMap::new();
    let mut dirs: BTreeMap<String, String> = BTreeMap::new();
    for p in packages {
        let (Some(id), Some(name)) = (p["id"].as_str(), p["name"].as_str()) else { continue };
        id_name.insert(id, name);
        if !members.contains(id) {
            continue;
        }
        let manifest = p["manifest_path"].as_str().unwrap_or_default();
        let abs = abspath(manifest, cwd);
        let dir = abs.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or(abs);
        dirs.insert(dir, name.to_string());
    }

    let names: BTreeSet<String> =
        members.iter().filter_map(|m| id_name.get(*m)).map(|n| (*n).to_string()).collect();

    // Forward workspace-internal deps from the RESOLVED graph, split by kind. `node["deps"]` (not
    // `node["dependencies"]`) is what carries `dep_kinds`; a `kind` of null means a normal dep.
    let mut fwd: Rdeps = BTreeMap::new();
    let mut fwd_dev: Rdeps = BTreeMap::new();
    // Every resolve node's direct dependency ids, ALL kinds, for the closure below.
    let mut direct: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let nodes = meta["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no resolve.nodes array (was --no-deps passed?)")?;
    for node in nodes {
        let Some(id) = node["id"].as_str() else { continue };
        let deps = node["dependencies"].as_array().into_iter().flatten();
        direct.insert(id, deps.filter_map(|d| d.as_str()).collect());
        if !members.contains(id) {
            continue;
        }
        let me = (*id_name.get(id).ok_or("a resolve node names no package")?).to_string();
        fwd.entry(me.clone()).or_default();
        fwd_dev.entry(me.clone()).or_default();
        for d in node["deps"].as_array().into_iter().flatten() {
            let Some(pkg) = d["pkg"].as_str() else { continue };
            if !members.contains(pkg) {
                continue;
            }
            let dep = (*id_name.get(pkg).ok_or("a dep names no package")?).to_string();
            let kinds = d["dep_kinds"].as_array();
            // No `dep_kinds` at all (an older `cargo metadata` shape) reads as ONE normal edge —
            // the conservative fallback, since a normal edge is walked transitively while a dev edge
            // is applied one hop, so guessing "dev" here could DROP dependents.
            let (mut normal, mut dev) = (kinds.is_none(), false);
            for k in kinds.into_iter().flatten() {
                match k["kind"].as_str() {
                    None => normal = true, // null (or absent) == a normal dep
                    Some("build") => normal = true,
                    Some("dev") => dev = true,
                    Some(_) => {}
                }
            }
            // A dep can be BOTH (normal + dev-only extras): normal wins, it is a real edge.
            if normal {
                fwd.entry(me.clone()).or_default().insert(dep);
            } else if dev {
                fwd_dev.entry(me.clone()).or_default().insert(dep);
            }
        }
    }

    let rfeatures = feature_edges(packages, &members, &fwd, &fwd_dev);
    let closure = members
        .iter()
        .filter_map(|m| {
            id_name.get(m).map(|n| ((*n).to_string(), closure_names(m, &direct, &id_name)))
        })
        .collect();
    Ok(Graph { dirs, radj: reverse(&fwd), radj_dev: reverse(&fwd_dev), rfeatures, names, closure })
}

/// [`Graph::rfeatures`]: every member's DECLARED dependency entries on members, by package name (the
/// resolve graph carries no per-edge features), each with what [`entry_features`] reads off it. Then
/// every RESOLVED member edge (`fwd`, `fwd_dev`) that no entry was read for is added turning EVERY
/// feature on: a dependency list that could not be read, or an entry that did not match, fails
/// closed rather than vanishing.
fn feature_edges(
    packages: &[serde_json::Value],
    members: &BTreeSet<&str>,
    fwd: &Rdeps,
    fwd_dev: &Rdeps,
) -> BTreeMap<String, Vec<FeatureEdge>> {
    let tables: BTreeMap<&str, &serde_json::Value> = packages
        .iter()
        .filter(|p| p["id"].as_str().is_some_and(|id| members.contains(id)))
        .filter_map(|p| Some((p["name"].as_str()?, &p["features"])))
        .collect();
    let mut out: BTreeMap<String, Vec<FeatureEdge>> = BTreeMap::new();
    for p in packages {
        let (Some(id), Some(me)) = (p["id"].as_str(), p["name"].as_str()) else { continue };
        if !members.contains(id) {
            continue;
        }
        for d in p["dependencies"].as_array().into_iter().flatten() {
            let Some((owner, owner_features)) =
                d["name"].as_str().and_then(|n| tables.get_key_value(n))
            else {
                continue;
            };
            out.entry((*owner).to_string()).or_default().push(FeatureEdge {
                dependent: me.to_string(),
                normal: d["kind"].as_str() != Some("dev"),
                features: entry_features(d, &p["features"], owner_features),
            });
        }
    }
    for (normal, adjacency) in [(true, fwd), (false, fwd_dev)] {
        for (dependent, owners) in adjacency {
            for owner in owners {
                let edges = out.entry(owner.clone()).or_default();
                if !edges.iter().any(|e| &e.dependent == dependent) {
                    let unread =
                        FeatureEdge { dependent: dependent.clone(), normal, features: None };
                    edges.push(unread);
                }
            }
        }
    }
    out
}

/// [`FeatureEdge::features`] for one entry of `cargo metadata`'s `packages[].dependencies`, given the
/// dependent's `[features]` table and the depended-on member's. `None` = could not be read.
fn entry_features(
    entry: &serde_json::Value,
    dependent_features: &serde_json::Value,
    owner_features: &serde_json::Value,
) -> Option<BTreeSet<String>> {
    let mut on: BTreeSet<String> = entry["features"]
        .as_array()?
        .iter()
        .map(|f| f.as_str().map(str::to_string))
        .collect::<Option<_>>()?;
    if entry["uses_default_features"].as_bool()? {
        on.insert("default".to_string());
    }
    // The dependent's own features forward to this entry by its LOCAL name (`rename`, if any).
    let local = entry["rename"].as_str().or_else(|| entry["name"].as_str())?;
    for values in dependent_features.as_object()?.values() {
        for v in values.as_array()? {
            let forwarded = v.as_str()?.strip_prefix(local).and_then(|r| {
                r.strip_prefix('?').unwrap_or(r).strip_prefix('/').map(str::to_string)
            });
            on.extend(forwarded);
        }
    }
    // ...closed over the owner's implications. `dep:x` turns on a dependency, not a feature; any
    // other value names a feature of the owner (`x/y` and `x?/y` at least imply looking at `x`,
    // which can only add to the set).
    let table = owner_features.as_object()?;
    let mut stack: Vec<String> = on.iter().cloned().collect();
    while let Some(f) = stack.pop() {
        let Some(implied) = table.get(&f) else { continue };
        for v in implied.as_array()? {
            let v = v.as_str()?;
            if v.starts_with("dep:") {
                continue;
            }
            let name = v.split('/').next().unwrap_or(v).trim_end_matches('?').to_string();
            if on.insert(name.clone()) {
                stack.push(name);
            }
        }
    }
    Some(on)
}

/// The package names reachable from `root` through `direct` (`root` itself excluded unless a cycle
/// reaches it, which a dev-edge cycle can).
fn closure_names(
    root: &str,
    direct: &BTreeMap<&str, Vec<&str>>,
    id_name: &BTreeMap<&str, &str>,
) -> BTreeSet<String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = direct.get(root).cloned().unwrap_or_default();
    while let Some(id) = stack.pop() {
        if seen.insert(id) {
            stack.extend(direct.get(id).into_iter().flatten().copied());
        }
    }
    seen.into_iter().filter_map(|id| id_name.get(id).map(|n| (*n).to_string())).collect()
}

fn reverse(fwd: &Rdeps) -> Rdeps {
    let mut r: Rdeps = fwd.keys().map(|n| (n.clone(), BTreeSet::new())).collect();
    for (n, deps) in fwd {
        for d in deps {
            r.entry(d.clone()).or_default().insert(n.clone());
        }
    }
    r
}

/// `crate` plus every crate that (transitively) depends on it through `radj`.
pub fn transitive_rdeps(krate: &str, radj: &Rdeps) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = [krate.to_string()].into();
    let mut stack = vec![krate.to_string()];
    while let Some(cur) = stack.pop() {
        for parent in radj.get(&cur).into_iter().flatten() {
            if seen.insert(parent.clone()) {
                stack.push(parent.clone());
            }
        }
    }
    seen
}

/// Crates to test for a change to `seeds`: the transitive NORMAL reverse-dep closure, plus ONE HOP
/// of dev-dependents. A crate whose tests use an affected crate is tested; its own reverse-world is
/// not pulled in — that hop is where the cycle used to escape.
pub fn affected_from(seeds: &BTreeSet<String>, radj: &Rdeps, radj_dev: &Rdeps) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for s in seeds {
        out.extend(transitive_rdeps(s, radj));
    }
    let hop: BTreeSet<String> =
        out.iter().flat_map(|c| radj_dev.get(c).into_iter().flatten().cloned()).collect();
    out.extend(hop);
    out
}
