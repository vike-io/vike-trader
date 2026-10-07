//! The documented roster and the rendered plan, held to the derivations they are read from.

use std::collections::BTreeSet;

use super::plan::Plan;
use super::roster::{ci_crates, doc_crates, docs_affected};
use super::set;

fn names() -> BTreeSet<String> {
    set(&["vike-model", "vike-core", "vike-desktop", "vike-backfill", "xtask"])
}

/// The published reference documents the CI roster — asserted as an EQUALITY between the two
/// derivations rather than against a written-down list, so this test still holds the day a
/// crate is added and cannot be satisfied by pasting one in.
#[test]
fn the_documented_roster_is_the_ci_roster() {
    let n = names();
    assert_eq!(doc_crates(&n), ci_crates(&n));
    assert!(doc_crates(&n).contains(&"vike-model".to_string()));
    // ...and the exclusions really are excluded, so an equality that had degenerated to "every
    // member" would fail here rather than read green.
    for excluded in ["vike-desktop", "vike-backfill"] {
        assert!(
            !doc_crates(&n).contains(&excluded.to_string()),
            "{excluded} is in EXCLUDE_FROM_CI and must not reach the published reference"
        );
    }
}

/// The trigger fires on a documented crate's own source and on nothing else — the property that
/// separates it from `any` and keeps a markdown typo from rebuilding the whole reference.
#[test]
fn the_docs_trigger_reads_the_owning_crate_not_the_closure() {
    let n = names();
    assert!(docs_affected(&set(&["vike-model"]), &n), "a documented crate's own source");
    assert!(
        !docs_affected(&set(&["vike-desktop"]), &n),
        "vike-desktop is not documented, so a change confined to it cannot move a published page"
    );
    assert!(
        !docs_affected(&set(&[]), &n),
        "a change owning no crate at all — a markdown typo — must not fire the docs job"
    );
}

fn plan(ordered: &[&str], roster: &[&str]) -> Plan {
    let owned = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    Plan {
        any: !ordered.is_empty(),
        ordered: owned(ordered),
        roster: owned(roster),
        suites: vec![],
        hist: false,
        core: false,
        app: false,
        lightgbm: false,
        docs: true,
        diagnostic: String::new(),
    }
}

/// The `docs` line is EMITTED. `.github/workflows/ci.yml` reads `docs` off `$GITHUB_OUTPUT` by
/// name, and an output the plan never prints is permanently empty and silently falsy — the
/// exact defect the `shards` output sat in for months (see that file's `outputs:` block).
#[test]
fn render_emits_the_docs_key_in_both_states() {
    let mut plan = plan(&["vike-model"], &["vike-core", "vike-model"]);
    assert!(plan.render().contains("\ndocs=true\n"), "{}", plan.render());
    plan.docs = false;
    assert!(plan.render().contains("\ndocs=false\n"), "{}", plan.render());
}

/// The `test` job's two inputs, rendered: it BUILDS the roster and RUNS the affected set.
/// Pinned byte for byte, because ci.yml hands both strings to cargo/nextest unparsed.
#[test]
fn render_emits_the_build_roster_and_the_test_filter_separately() {
    let p = plan(&["vike-model"], &["vike-core", "vike-model"]);
    let text = p.render();
    assert!(text.contains("\ncrates=-p vike-model\n"), "{text}");
    assert!(text.contains("\nroster=-p vike-core -p vike-model\n"), "{text}");
    assert!(text.contains("\ntests=package(=vike-model)\n"), "{text}");

    let p = plan(&["vike-core", "vike-model"], &["vike-core", "vike-model"]);
    assert!(
        p.render().contains("\ntests=package(=vike-core) | package(=vike-model)\n"),
        "{}",
        p.render()
    );
}

/// An empty plan names no package — and emits no filter that could stand for one. `tests=`
/// empty is a nextest PARSE ERROR if anything ever runs on it (measured: exit 94), which is the
/// direction a mistake here must fail in. The roster is still the whole roster.
#[test]
fn an_empty_plan_renders_an_empty_filter_and_the_whole_roster() {
    let text = plan(&[], &["vike-core", "vike-model"]).render();
    assert!(text.starts_with("any=false\ncrates=\n"), "{text}");
    assert!(text.contains("\ntests=\n"), "{text}");
    assert!(text.contains("\nroster=-p vike-core -p vike-model\n"), "{text}");
}
