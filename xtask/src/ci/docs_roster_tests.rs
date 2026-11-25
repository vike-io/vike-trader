//! The documented roster and the rendered plan, held to the derivations they are read from.

use std::collections::BTreeSet;

use super::plan::Plan;
use super::roster::{ci_crates, doc_crates};
use super::set;
use super::tables::halves::Half;

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

fn plan(ordered: &[&str], roster: &[&str]) -> Plan {
    let owned = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    Plan {
        any: !ordered.is_empty(),
        ordered: owned(ordered),
        covered: owned(ordered),
        rerun: BTreeSet::new(),
        half_of: [("vike-ops", Half::A), ("vike-data", Half::A)]
            .into_iter()
            .map(|(c, h)| (c.to_string(), h))
            .collect(),
        summary: String::new(),
        strategy_lab: String::new(),
        ops_gates: None,
        xtask_narrowed: false,
        roster: owned(roster),
        suites: vec![],
        datahub: false,
        tradehub: false,
        core: false,
        latency_reuse: String::new(),
        app: false,
        lightgbm: false,
        diagnostic: String::new(),
    }
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
    // A gate that is a MODULE of a group binary (`binary::gate`) is that binary's tests under its prefix.
    p.ops_gates = Some(vec!["docs::citation_gate".to_string(), "run_checks_selftest".to_string()]);
    assert!(
        p.render().contains(
            "\ntests=package(=vike-core) | (package(=vike-ops) & ((binary(=docs) & \
             test(/^citation_gate::/)) | binary(=run_checks_selftest))) | package(=xtask)\n"
        ),
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
    for bad in ["docs::a::b", "docs::", "::a", "docs::a/) | all() | (/"] {
        p.ops_gates = Some(vec![bad.to_string()]);
        assert!(
            p.render().contains("\ntests=package(=vike-core) | package(=vike-ops)\n"),
            "{bad}: {}",
            p.render()
        );
    }
}

/// The `test` job's matrix, `halves=`: one row per half with a test to run, each that half's slice of `roster=` to
/// build and of `tests=` to run, on the half's runner. A red run's failed test rides the half of its binary's crate,
/// a half with nothing to run has no row, and `tests=` still names everything.
#[test]
fn the_halves_split_the_roster_and_the_filter_and_drop_an_idle_half() {
    let rows = |p: &Plan| -> Vec<serde_json::Value> {
        let text = p.render();
        let line = text.lines().find_map(|l| l.strip_prefix("halves=")).expect("a halves= line");
        let parsed: serde_json::Value = serde_json::from_str(line).expect("one line of JSON");
        parsed.as_array().expect("an array").clone()
    };
    let roster = ["vike-core", "vike-data", "vike-ops"];
    let both = plan(&["vike-core", "vike-ops"], &roster);
    assert_eq!(
        rows(&both),
        vec![
            serde_json::json!({"half": "a", "runner": "ci-half-a", "roster": "-p vike-data -p vike-ops",
                "tests": "package(=vike-ops)"}),
            serde_json::json!({"half": "b", "runner": "ci-half-b", "roster": "-p vike-core",
                "tests": "package(=vike-core)"}),
        ]
    );

    let mut one = plan(&["vike-core"], &roster);
    assert_eq!(rows(&one).len(), 1, "half A has nothing to run: {}", one.render());
    assert_eq!(rows(&one)[0]["half"], "b");

    one.rerun = [("vike-data::props".to_string(), "a::b".to_string())].into_iter().collect();
    let r = rows(&one);
    assert_eq!(r.len(), 2, "a failed test of a half-A crate gives half A a leg: {r:?}");
    assert_eq!(r[0]["tests"], "(binary_id(=vike-data::props) & test(=a::b))");
    assert_eq!(r[1]["tests"], "package(=vike-core)");
    assert!(
        one.render().contains(
            "\ntests=package(=vike-core) | (binary_id(=vike-data::props) & test(=a::b))\n"
        ),
        "{}",
        one.render()
    );

    assert!(plan(&[], &roster).render().contains("\nhalves=[]\n"));
}
