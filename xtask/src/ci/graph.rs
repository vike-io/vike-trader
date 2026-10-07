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
//! `crates/vike-ops/tests/arch/layer_gate.rs` draws the same distinction over the manifests for the same
//! reason, and says so.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

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
    /// Every workspace member's package name.
    pub names: BTreeSet<String>,
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

/// True for a changed file that is a **test-target source**: a `.rs` file under the owning crate's
/// `tests/`, `benches/` or `examples/` directory, named relative to that crate.
///
/// ⚠ **This is the one predicate that lets a change select its crate WITHOUT its reverse-dep world**
/// (`super::plan::compute`), so the claim it makes has to be exact: *nothing outside this crate can link
/// this file.* Cargo auto-discovers each of those directories into its OWN compilation unit — an
/// integration-test binary, a bench harness, an example — none of which is a library target, so no
/// `[dependencies]` edge can reach one. A dependent crate compiling this crate's lib does not
/// compile these files at all, which is exactly why their reverse-dep closure buys nothing.
///
/// Three deliberate NARROWINGS, each measured on the real tree rather than assumed:
///
/// * **`.rs` only — a DATA file under `tests/` stays a full change.** `src/` really does reach into
///   `tests/fixtures/`: `crates/bridges/ig/src/rest.rs`, `crates/vike-ml/src/infer.rs` and
///   `crates/vike-mount/src/server_time.rs` all
///   `include_str!` a fixture from there. Every one of those sites is under `#[cfg(test)]` today, so
///   a dependent's build genuinely does not see them — but that is a property of the CODE, and this
///   predicate is about the LAYOUT. A rule that had to re-verify a `cfg` attribute to stay correct
///   would be one refactor away from silently under-selecting, so fixtures simply keep the
///   conservative answer.
/// * **`src/bin/` is NOT covered**, though a binary is equally unlinkable. A test in another crate
///   can still SPAWN one by path, and cargo models no edge for that — so the reverse-dep closure is
///   the only thing standing behind it. No measured win asked for the risk.
/// * **The latency gate is untouched.** `super::plan::compute` keeps handing `latency_affected` the FULL
///   changed set, so this rule can never make that job fire LESS — which matters because the gate's
///   own harness, `crates/vike-core/tests/runtime_latency.rs`, is itself a test-target source.
///
/// `crates/vike-ops/tests/ci/ci_plan_gate.rs` holds all four claims, including a structural sweep of
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
    rel.starts_with("src/")
        && rel.ends_with(".rs")
        && !rel.starts_with("src/bin/")
        && cfg_test_file(crate_dir, rel, 0)
}

fn cfg_test_file(crate_dir: &Path, rel: &str, depth: usize) -> bool {
    if depth > 8 || rel == "src/lib.rs" || rel == "src/main.rs" {
        return false;
    }
    let read = |p: &str| std::fs::read_to_string(crate_dir.join(p)).ok();
    if read(rel).is_some_and(|s| has_inner_cfg_test(&s)) {
        return true;
    }
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
                if d.cfg_test || cfg_test_file(crate_dir, decl_file, depth + 1) {
                    return true;
                }
            }
        }
        if dir == "src" || !dir.contains('/') {
            return false;
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
        out.push(ModDecl {
            name: name.to_string(),
            cfg_test: attrs.iter().any(|a| a == "#[cfg(test)]"),
            path,
        });
    }
    out
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
/// ⚠ Deliberately WITHOUT `--locked`, and `crates/vike-ops/tests/ci/local_gate_mirrors_ci/release_tag.rs`'s
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
    let nodes = meta["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no resolve.nodes array (was --no-deps passed?)")?;
    for node in nodes {
        let Some(id) = node["id"].as_str() else { continue };
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

    Ok(Graph { dirs, radj: reverse(&fwd), radj_dev: reverse(&fwd_dev), names })
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
