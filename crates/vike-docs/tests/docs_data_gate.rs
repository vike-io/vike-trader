//! The docs-data export's gate: the rendered assets are roster-complete, schema-complete, and the
//! one constant this export duplicates cannot drift from its authority.
//!
//! Runs the EXACT rendering the `docs_data` bin writes (`vike_docs::rendered_files`),
//! in-process — no subprocess, no filesystem — and asserts over the parsed output, so what is
//! gated here is byte-for-byte what `.github/workflows/release.yml` attaches to a Release.
//!
//! ⚠ It also READS FOUR repo files, and three of them cannot select this crate through the
//! reverse-dependency closure, so `xtask/src/ci/tables/gate_crates.rs`'s `DOCS_DATA_GATE_INPUTS`
//! force-adds it (why: `crates/vike-docs/CLAUDE.md`). Every read is ONE repo-root-relative literal
//! passed to `repo_file` or `read_repo` IN THIS FILE, the only one `xtask/tests/ci_plan_gate.rs`
//! scans for them; the `WITHHELD` needle stays here too, because
//! `xtask/tests/ci_plan_gate/exempt_inputs.rs`'s `NOT_A_READ` keys its rows on this path. So a test
//! that reads a repo file lives in this root, and the per-asset children render in-process only.

use std::path::{Path, PathBuf};

use serde_json::Value;
use vike_docs::{DEFAULT_GENERATED_FROM, EVENTS, P99_BUDGET_NS, rendered_files};

#[path = "docs_data_gate/indicators.rs"]
mod indicators;
#[path = "docs_data_gate/rosters.rs"]
mod rosters;
#[path = "docs_data_gate/stats.rs"]
mod stats;
#[path = "docs_data_gate/templates.rs"]
mod templates;
#[path = "docs_data_gate/venues.rs"]
mod venues;

fn object_keys(value: &Value, what: &str) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("{what} is not a JSON object: {value}"))
        .keys()
        .cloned()
        .collect();
    keys.sort_unstable();
    keys
}

/// The string `field` of every row of the JSON array `rows`, in row order.
fn ids<'a>(rows: &'a Value, field: &str) -> Vec<&'a str> {
    rows.as_array()
        .unwrap_or_else(|| panic!("rows carrying {field:?} are not a JSON array: {rows}"))
        .iter()
        .map(|r| r[field].as_str().unwrap_or_else(|| panic!("a row has no string {field:?}: {r}")))
        .collect()
}

/// The exported latency budget equals `crates/vike-core/tests/runtime_latency.rs`'s
/// `P99_BUDGET_NS` — parsed out of the source, because a test-file const cannot be imported. This
/// is the pin that lets `vike_docs` carry a copy at all: the copy cannot drift silently.
#[test]
fn latency_budget_matches_the_runtime_latency_gate() {
    let src = read_repo("crates/vike-core/tests/runtime_latency.rs");
    let needle = "const P99_BUDGET_NS: u64 =";
    let line = src
        .lines()
        .find(|l| l.trim_start().starts_with(needle))
        .unwrap_or_else(|| panic!("{needle:?} not found in the latency gate's source"));
    let digits: String = line[line.find('=').expect("declaration has an `=`") + 1..]
        .chars()
        .take_while(|c| *c != ';')
        .filter(char::is_ascii_digit)
        .collect();
    let authority: u64 = digits.parse().unwrap_or_else(|e| {
        panic!("could not parse the budget out of {line:?} in the latency gate's source: {e}")
    });
    assert_eq!(
        P99_BUDGET_NS, authority,
        "vike_docs::P99_BUDGET_NS drifted from the latency gate's constant — \
         update the copy in crates/vike-docs/src/stats.rs"
    );
}

/// The declared [`EVENTS`] table IS `vike_model::events::Event` — every variant, its payload type, and its
/// wire tag — parsed straight out of the enum's source.
///
/// This is the pin that lets `docs_data` declare the table at all. A wire tag is a
/// `#[serde(rename = "…")]` ATTRIBUTE: no runtime renderer can read one, and the enum has no value
/// to enumerate, so the table is written by hand and held equal to its authority here — the same
/// device as `latency_budget_matches_the_runtime_latency_gate`. A new variant, a renamed payload
/// or a changed wire tag reddens CI until the table is updated.
#[test]
fn event_table_matches_the_event_enum() {
    let src = read_repo("crates/vike-model/src/events.rs");
    let body = src
        .split_once("pub enum Event {")
        .unwrap_or_else(|| panic!("no `pub enum Event {{` in vike-model's events source"))
        .1
        .split_once("\n}")
        .expect("the enum body is closed by a line-start `}`")
        .0;

    // (variant, payload, wire tag) per variant: a `Variant(Payload),` line, with the wire tag
    // taken from the most recent `#[serde(rename = "…")]` when one sits directly above it.
    let mut parsed: Vec<(String, String, String)> = Vec::new();
    let mut pending_rename: Option<String> = None;
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("#[serde(rename = \"") {
            pending_rename = rest.split('"').next().map(str::to_string);
            continue;
        }
        if t.starts_with("//") || t.is_empty() {
            continue;
        }
        let Some((variant, rest)) = t.split_once('(') else {
            pending_rename = None;
            continue;
        };
        let Some(payload) = rest.split_once(')').map(|(p, _)| p) else {
            pending_rename = None;
            continue;
        };
        if !variant.chars().next().is_some_and(char::is_uppercase) {
            pending_rename = None;
            continue;
        }
        let wire = pending_rename.take().unwrap_or_else(|| variant.to_string());
        parsed.push((variant.to_string(), payload.to_string(), wire));
    }

    let declared: Vec<(String, String, String)> =
        EVENTS.iter().map(|&(v, p, w)| (v.to_string(), p.to_string(), w.to_string())).collect();
    assert_eq!(
        declared, parsed,
        "vike_docs::EVENTS drifted from `vike_model::events::Event` — update the table in \
         crates/vike-docs/src/rosters.rs to match crates/vike-model/src/events.rs"
    );
}

// ── rendered ⇒ attached ⇒ mirrored ───────────────────────────────────────────────────────────────
//
// Three files name the docs-data set and none of them can see the others: this module RENDERS it,
// `.github/workflows/release.yml` ATTACHES it to a private Release, and `scripts/publish_mirror.sh`
// re-publishes it onto the PUBLIC mirror, which is where the documentation site fetches from. The
// two tests below hold the second and third equal to the first.
//
// They exist because the three lists drifted once — `rosters.json` was rendered at every release
// and attached to nothing — and nothing failed.
//
// ⚠ `bins.json` is xtask's (`cargo run -p xtask -- docs-bins`), not this module's, so it is a
// literal below. A rename on that side is uncovered: the two crates cannot see each other, and a
// shared constant crate for one string would cost more than the hole it closes.

/// The docs-data set as every consumer must spell it: what this module renders, then xtask's.
fn expected_docs_data_assets() -> Vec<&'static str> {
    let mut names: Vec<&'static str> =
        rendered_files(DEFAULT_GENERATED_FROM).iter().map(|(name, _)| *name).collect();
    names.push("bins.json");
    // ⚠ Two more BARE LITERALS. `cli.json`'s renderer is `crates/vike-cli/src/surface.rs`'s
    // `rendered_files`, `profile.json`'s is `crates/vike-backtest/src/profile_surface/render.rs`'s
    // `rendered_files`; both crates declare layer 30 against this crate's 25, so
    // `crates/vike-ops/tests/architecture/layer_gate.rs` refuses a NORMAL edge. A DEV edge would
    // pass that gate but compile the whole crate into this test build to read one string: cost is
    // the argument. The release copies bytes derived from the type; only the NAME is duplicated.
    names.push("cli.json");
    names.push("profile.json");
    names
}

/// The repo-root-relative path `rel`, resolved from this crate's directory — the one join
/// [`read_repo`] and [`repo_file`] share.
fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join(rel)
}

/// Read a repo-root-relative file that EVERY checkout carries, the public mirror included — so an
/// absence is a broken tree and panics, where [`repo_file`] below skips a withheld one.
///
/// ⚠ Spelled as one literal at every call site, never assembled from `join` segments: that literal
/// is what `xtask/tests/ci_plan_gate.rs` reads to hold `DOCS_DATA_GATE_INPUTS` equal to
/// the files this gate actually reads.
fn read_repo(rel: &str) -> String {
    let p = repo_path(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Read a repo-root-relative file, or `None` where it is absent.
///
/// `.github/` and `scripts/` are both withheld from the public source mirror, so a checkout of the
/// mirror has neither file to read. These gates are for the private repository; skipping loudly
/// there beats failing a tree that was never meant to contain the thing being gated.
fn repo_file(rel: &str) -> Option<String> {
    let p = repo_path(rel);
    match std::fs::read_to_string(&p) {
        Ok(text) => Some(text),
        Err(_) => {
            eprintln!(
                "SKIPPED: {} is absent — this gate runs in the private repository only",
                p.display()
            );
            None
        }
    }
}

/// The one line in `file` whose trimmed form starts with `prefix` and mentions a `.json`, with that
/// prefix and the closing paren stripped — the shape both lists happen to share.
fn sole_asset_list<'a>(text: &'a str, prefix: &str, what: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(prefix) && l.contains(".json"))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "{what} carries exactly one `{prefix}…` list naming .json assets; found {}",
        lines.len()
    );
    lines[0].trim_start_matches(prefix).trim_end_matches(')').split_whitespace().collect()
}

/// `.github/workflows/release.yml` attaches exactly what is rendered, plus xtask's `bins.json`.
///
/// Rendering an asset and shipping it are two different lists, and until this gate they were free
/// to disagree.
#[test]
fn the_release_attaches_every_rendered_asset() {
    let Some(text) = repo_file(".github/workflows/release.yml") else { return };
    let attached = sole_asset_list(&text, "ASSETS+=(", "release.yml");
    assert_eq!(
        attached,
        expected_docs_data_assets(),
        "the docs-data assets release.yml attaches must be exactly what `rendered_files` renders, \
         plus xtask's bins.json, in that order. Add the missing name to the `ASSETS+=(` line that \
         carries the .json set — an asset rendered but not attached reaches no reader."
    );
}

/// `scripts/publish_mirror.sh` re-publishes exactly the same set onto the public mirror.
///
/// The private Release is not what the documentation site reads: it reads the MIRROR's latest
/// release. An asset that stops at the private release is as invisible to a reader as one that was
/// never rendered, and just as silent — the site falls back to its committed snapshot without
/// erroring.
///
/// ⚠ That script ALSO copies every binary the private release's `SHA256SUMS` names — DERIVED from
/// the manifest, never listed — so `RELEASE_ASSETS` is not "what the mirror release carries". It
/// is the subset the documentation site depends on BY NAME, whose absence must be a refusal
/// rather than whatever the manifest happened to hold that day: a manifest-derived set cannot
/// notice that an asset is missing from it. That is why the list survives the derivation, and why
/// this gate still holds it equal to the renderer's.
#[test]
fn the_mirror_publishes_every_released_asset() {
    let Some(text) = repo_file("scripts/publish_mirror.sh") else { return };
    let published = sole_asset_list(&text, "readonly RELEASE_ASSETS=(", "publish_mirror.sh");
    assert_eq!(
        published,
        expected_docs_data_assets(),
        "publish_mirror.sh's RELEASE_ASSETS must name exactly what release.yml attaches. An asset \
         missing here never reaches the public mirror, so the documentation site keeps rendering \
         from its committed snapshot — with no error anywhere, indefinitely."
    );
}

/// No rendered asset may cite a path the public source mirror withholds.
///
/// These files are not internal notes: every string in them is rendered onto a page at
/// `vike.io/docs/trader/`, for a reader whose only view of this workspace is the mirror. The mirror
/// publishes `crates/`, `xtask/`, `fixtures/`, `assets/`, `deploy/docker/` and the root manifests —
/// so citing those by path and symbol is CORRECT and is what makes a claim checkable. It publishes
/// no `CLAUDE.md`, no `docs/`, no `scripts/`, no `.github/` and no `justfile`, so a citation to one
/// of those is a dead link on a public page.
///
/// One had already shipped, through `SCRIPT_ONLY`'s rhai gloss. A doc COMMENT may cite a decision
/// record — that is how this workspace argues with itself, and the comment stays. A string that is
/// EXPORTED may not.
///
/// ⚠ This gate reads only what this module renders. It cannot see prose the site's own generators
/// add around these values, and it is not a substitute for the citation discipline in the docs
/// repository — it closes the one path that leads from a Rust string onto a public page unread.
#[test]
fn no_rendered_asset_cites_a_path_the_mirror_withholds() {
    const WITHHELD: &[&str] = &["CLAUDE.md", "justfile", ".github/", "scripts/", "docs/"];

    let mut offenders: Vec<String> = Vec::new();
    for (name, contents) in rendered_files(DEFAULT_GENERATED_FROM).iter() {
        for needle in WITHHELD {
            let mut from = 0;
            while let Some(at) = contents[from..].find(needle) {
                let start = from + at;
                let end = (start + 80).min(contents.len());
                let excerpt = contents[start..end].replace('\n', " ");
                offenders.push(format!("{name}: …{excerpt}…"));
                from = start + needle.len();
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these exported strings cite paths the public source mirror does not publish, so they \
         render as dead links on vike.io/docs/. Rewrite the exported string — the fact usually \
         stands without the citation — and keep the reasoning in a doc comment, which stays \
         internal:\n{}",
        offenders.join("\n")
    );
}
