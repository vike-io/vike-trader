use super::*;
use vike_model::runs::{MANIFEST_SCHEMA, RunConfig, RunManifest};

fn doc(sharpe: f64, profile: &str) -> Value {
    serde_json::json!({
        "sharpe": sharpe,
        "win_rate": 0.51,
        "config": { "path": profile },
        "run_id": "1756000000-1-0",
        "started_at": "2025-08-24T01:46:40Z",
    })
}

/// The lockstep walk classifies every leaf, on both sides, once.
#[test]
fn a_leaf_on_one_side_only_is_added_or_removed_and_never_changed() {
    let a = serde_json::json!({ "only_a": 1, "both": 2 });
    let b = serde_json::json!({ "both": 3, "only_b": 4 });
    let rows = diff_docs(&a, &b, &[]);
    let classes: Vec<(&str, &str)> = rows.iter().map(|r| (r.key.as_str(), r.class())).collect();
    assert_eq!(
        classes,
        vec![("both", "changed"), ("only_a", "removed"), ("only_b", "added")],
        "sorted, and each classified once"
    );
}

/// ⚠ `run_id`, `started_at` and `finished_at` differ between ANY two runs, so they are excluded
/// from the INPUT diff and rendered in the header instead. Otherwise three guaranteed rows drown
/// the real signal in a view whose whole job is removing noise.
#[test]
fn the_always_different_keys_are_skipped_in_the_input_diff() {
    let rows = diff_docs(&doc(1.8, "m.toml"), &doc(1.4, "m-v2.toml"), VOLATILE);
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert!(!keys.contains(&"run_id"), "{keys:?}");
    assert!(!keys.contains(&"started_at"), "{keys:?}");
    assert!(keys.contains(&"config.path"), "…and the real signal survives: {keys:?}");
    // The negative control: WITHOUT the skip list they are rows, so the filter is what removes
    // them rather than the fixture happening not to have them.
    let unskipped = diff_docs(&doc(1.8, "m.toml"), &doc(1.4, "m-v2.toml"), &[]);
    assert!(unskipped.iter().any(|r| r.key == "run_id"));
}

/// A percent move is computed against `|a|`, and is absent at a zero baseline rather than
/// infinite — "it went from 0 to 4" has no percentage, and printing one is a number nobody can
/// act on.
#[test]
fn a_percent_move_is_absent_at_a_zero_baseline() {
    let rows = diff_docs(
        &serde_json::json!({ "x": 0.0, "y": 2.0 }),
        &serde_json::json!({ "x": 4.0, "y": 1.0 }),
        &[],
    );
    let x = rows.iter().find(|r| r.key == "x").unwrap();
    let y = rows.iter().find(|r| r.key == "y").unwrap();
    assert_eq!(x.percent(), None);
    assert_eq!(y.percent(), Some(-50.0));
}

/// A NEGATIVE left-hand value still gives a percentage with the right SIGN: `|a|` in the
/// denominator, the same rule `failif`'s tolerance uses and for the same reason.
#[test]
fn a_percent_move_from_a_negative_value_keeps_its_direction() {
    let rows = diff_docs(&serde_json::json!({ "s": -0.5 }), &serde_json::json!({ "s": -0.6 }), &[]);
    let p = rows[0].percent().unwrap();
    assert!(p < 0.0, "a fall from -0.5 to -0.6 is a FALL, not a rise: {p}");
    assert!((p + 20.0).abs() < 1e-9, "{p}");
}

fn manifest(git: Option<&str>, fp: Option<&str>) -> RunManifest {
    RunManifest {
        schema: MANIFEST_SCHEMA,
        run_id: "1756000000-1-0".into(),
        kind: "backtest".into(),
        produced_by: "backtest".into(),
        started_at: "2025-08-24T01:46:40Z".into(),
        finished_at: "2025-08-24T01:46:41Z".into(),
        git_sha: git.map(str::to_string),
        fingerprint: fp.map(str::to_string),
        config: RunConfig { path: Some("m.toml".into()), name: None },
        detail: Value::Null,
    }
}

/// ⚠ **A diff that cannot attribute a move SAYS SO.** Without this, a diff showing only a config
/// change reads as "the config is why", when the truth is that the other two causes were never
/// recorded.
#[test]
fn a_diff_names_the_attributions_the_record_cannot_support() {
    let notes = unattributable(&manifest(None, None), &manifest(None, None));
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("build stamp")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("fingerprint")), "{notes:?}");
}

/// ⚠ **A MISSING fingerprint on ONE side is enough to warn, and the warning may not name ONE
/// cause.** A null has more than one this document cannot tell apart — a producer that computes
/// no address, and a store that could not be inventoried. The sentence named the second one
/// alone and was a fraction of the truth the day it was written.
///
/// ⚠ A SEARCH PARENT was a third cause until `vike_data::DataFusionHist::series_facts` let
/// `crates/vike-backtest/src/backtest_cli.rs` address a search for the price of the data
/// witness it was already paying. It is dropped from this list rather than kept "for old
/// documents": the note is advice to a reader about what a null might mean NOW, and listing a
/// cause the producer can no longer exhibit sends them looking for a sweep that is not there.
#[test]
fn one_missing_fingerprint_is_enough_and_the_note_names_every_cause() {
    let notes = unattributable(&manifest(Some("abc"), Some("dead")), &manifest(Some("abc"), None));
    assert_eq!(notes.len(), 1, "the build stamp is on both sides: {notes:?}");
    assert!(notes[0].contains("not a different one"), "it says what absent MEANS: {notes:?}");
    for cause in ["computes no address", "could not be inventoried"] {
        assert!(notes[0].contains(cause), "the note omits `{cause}`: {notes:?}");
    }
    assert!(
        !notes[0].contains("search parent"),
        "a search run addresses its inputs now — naming it as a cause of a null sends the \
             reader to a sweep that is not there: {notes:?}"
    );
}

/// ⚠ **A DIFFERING fingerprint is not proof the data moved either** — the second half of the
/// same honesty requirement. A GROUPED series contributes its WHOLE GROUP's coverage to the
/// address, so an unrelated symbol's rows move it while this run's inputs are byte-identical
/// (spec §18 row 10).
#[test]
fn two_different_fingerprints_are_a_reason_to_look_rather_than_a_finding() {
    let notes =
        unattributable(&manifest(Some("abc"), Some("dead")), &manifest(Some("abc"), Some("beef")));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("DIFFERENT"), "{notes:?}");
    assert!(notes[0].contains("grouped series"), "it says WHY it is not a finding: {notes:?}");
    assert!(
        !notes[0].contains("search parent"),
        "…and does not recite the ABSENT-address causes, which do not apply: {notes:?}"
    );
}

/// Both facts recorded and AGREEING on both sides: no notes at all. The negative control — every
/// test above would pass against a function that always warned.
#[test]
fn a_fully_recorded_agreeing_pair_carries_no_attribution_notes() {
    let notes =
        unattributable(&manifest(Some("abc"), Some("dead")), &manifest(Some("def"), Some("dead")));
    assert!(notes.is_empty(), "{notes:?}");
}
