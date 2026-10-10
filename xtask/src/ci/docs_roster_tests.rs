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
        ops_gates: None,
        roster: owned(roster),
        suites: vec![],
        hist: false,
        core: false,
        core_direct: false,
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

/// A narrowed `vike-ops` renders as ONE intersected term in place of `package(=vike-ops)`, in its roster
/// position, with every other crate spelled as before — pinned byte for byte because `ci.yml` hands the
/// string to nextest unparsed. `crates=` still names the whole crate.
#[test]
fn a_narrowed_vike_ops_renders_as_an_intersected_term_and_nothing_else_moves() {
    let mut p = plan(&["vike-core", "vike-ops", "xtask"], &["vike-core", "vike-ops", "xtask"]);
    p.ops_gates = Some(vec!["citation_gate".to_string(), "layer_gate".to_string()]);
    let text = p.render();
    assert!(
        text.contains(
            "\ntests=package(=vike-core) | (package(=vike-ops) & (binary(=citation_gate) | \
             binary(=layer_gate))) | package(=xtask)\n"
        ),
        "{text}"
    );
    assert!(text.contains("\ncrates=-p vike-core -p vike-ops -p xtask\n"), "{text}");
    p.ops_gates = None;
    assert!(
        p.render().contains("\ntests=package(=vike-core) | package(=vike-ops) | package(=xtask)\n"),
        "{}",
        p.render()
    );
}

/// A narrowed crate with no gate left drops out of the filter, and a filter that would be EMPTY falls
/// back to the whole package: an empty `-E` is a nextest parse error, and a plan that names a crate must
/// run something. A gate name that is not a legal target name is never spliced into the filterset.
#[test]
fn a_narrowing_that_selects_nothing_or_is_malformed_never_empties_the_filter() {
    let mut p = plan(&["vike-core", "vike-ops"], &["vike-core", "vike-ops"]);
    p.ops_gates = Some(vec![]);
    assert!(p.render().contains("\ntests=package(=vike-core)\n"), "{}", p.render());

    let mut alone = plan(&["vike-ops"], &["vike-ops"]);
    alone.ops_gates = Some(vec![]);
    assert!(alone.render().contains("\ntests=package(=vike-ops)\n"), "{}", alone.render());

    p.ops_gates = Some(vec!["a) | all() | (b".to_string()]);
    assert!(
        p.render().contains("\ntests=package(=vike-core) | package(=vike-ops)\n"),
        "{}",
        p.render()
    );
}
