//! The pure-Rust walker against a REAL LightGBM model and REAL LightGBM outputs — with no
//! LightGBM binary anywhere in sight.
//!
//! `train_infer_equality.rs` is the stronger test and runs only where the binary exists. This one
//! runs in the DEFAULT lane, on a machine with nothing installed, because the library's answers are
//! FROZEN in the repository instead of recomputed.
//!
//! Regenerate deliberately: see `train_infer_equality.rs`'s `capture_the_golden_fixture`, and
//! `crates/vike-ml-training/CLAUDE.md` for when a regeneration is legitimate.

use std::path::PathBuf;

use vike_ml_prediction::{GbdtModel, Objective, load_model_file, parse_model_text};

const N_FEATURES: usize = 4;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn expected_text(stem: &str) -> String {
    std::fs::read_to_string(fixture_dir().join(format!("{stem}_expected.tsv")))
        .expect("committed fixture")
}

fn bits(token: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(token, 16).unwrap_or_else(|e| panic!("`{token}`: {e}")))
}

/// `(row, expected_proba)` per line of the binary fixture.
fn expectations() -> Vec<(Vec<f64>, f64)> {
    expectations_of("real_binary_categorical", 1)
        .into_iter()
        .map(|(row, want)| (row, want[0]))
        .collect()
}

/// `(row, expected outputs)` per line of `<stem>_expected.tsv`: `width` outputs after the features.
fn expectations_of(stem: &str, width: usize) -> Vec<(Vec<f64>, Vec<f64>)> {
    expected_text(stem)
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let f: Vec<f64> = l.split('\t').filter(|t| !t.is_empty()).map(bits).collect();
            assert_eq!(f.len(), N_FEATURES + width, "malformed fixture line: {l}");
            (f[..N_FEATURES].to_vec(), f[N_FEATURES..].to_vec())
        })
        .collect()
}

/// A committed real model: parsed, with the objective it was trained for, a categorical split (or
/// it gates nothing that matters) and the expected width.
fn real_model(stem: &str, objective: Objective) -> GbdtModel {
    let m = load_model_file(&fixture_dir().join(format!("{stem}_v4.txt"))).expect("parse");
    assert_eq!(m.objective, objective);
    assert!(m.trees.len() > 5, "{} trees", m.trees.len());
    assert!(m.trees.iter().any(|t| !t.cat_threshold.is_empty()), "no categorical split");
    assert_eq!(m.n_features_expected(), N_FEATURES);
    m
}

// The fixture's PROVENANCE check (captured from the pinned LightGBM tag) lives beside the pin it
// compares against: `crates/vike-ml-training/src/pins.rs`'s
// `the_golden_fixture_records_which_lightgbm_produced_it_and_it_is_the_pinned_one`.

#[test]
fn the_committed_real_model_parses() {
    let m = load_model_file(&fixture_dir().join("real_binary_categorical_v4.txt")).expect("parse");
    assert!(m.trees.len() > 5, "{} trees", m.trees.len());
    assert!(
        m.trees.iter().any(|t| !t.cat_threshold.is_empty()),
        "the fixture must contain a categorical split or it gates nothing that matters — this is \
         the requirement every Rust binding failed"
    );
    assert_eq!(m.n_features_expected(), N_FEATURES);
}

#[test]
fn the_first_tree_carries_the_folded_init_score_rather_than_being_a_constant() {
    // LightGBM's GBDT::TrainOneIter folds the init score into the first tree's leaf values
    // (AddBias) when it splits; only a learner that could not split at all yields a constant tree.
    let m = load_model_file(&fixture_dir().join("real_binary_categorical_v4.txt")).unwrap();
    let first = &m.trees[0];
    assert!(first.num_leaves > 1, "a real first tree splits; got {} leaves", first.num_leaves);
    assert!(m.trees.len() > 1);
}

#[test]
fn the_walker_reproduces_lightgbms_own_probabilities_to_a_stated_epsilon() {
    let m = load_model_file(&fixture_dir().join("real_binary_categorical_v4.txt")).unwrap();
    let rows = expectations();
    assert!(rows.len() > 40, "{} rows", rows.len());
    for (i, (row, want)) in rows.iter().enumerate() {
        // LightGBM's own task=predict, frozen at ~17 significant digits; 1e-12 is the budget
        // train_infer_equality.rs states, for the same reason.
        let d = (m.predict_proba(row) - want).abs();
        assert!(d <= 1e-12, "row {i} {row:?}: differs by {d}");
    }
}

#[test]
fn the_walker_reproduces_lightgbms_own_regression_values_bit_for_bit() {
    let m = real_model("real_regression_categorical", Objective::Regression);
    let rows = expectations_of("real_regression_categorical", 1);
    assert!(rows.len() > 40, "{} rows", rows.len());
    let mut out = [0.0];
    for (i, (row, want)) in rows.iter().enumerate() {
        // No transcendental at all: the prediction IS the plain tree sum, so any difference is a
        // row reaching a different leaf.
        m.predict_into(row, &mut out).unwrap();
        assert_eq!(
            out[0].to_bits(),
            want[0].to_bits(),
            "row {i} {row:?}: {} vs {}",
            out[0],
            want[0]
        );
    }
}

#[test]
fn the_walker_reproduces_lightgbms_own_class_probabilities_to_a_stated_epsilon() {
    let m = real_model("real_multiclass_categorical", Objective::Multiclass { class_count: 3 });
    assert!(m.trees.len().is_multiple_of(3), "whole iterations of three trees");
    let rows = expectations_of("real_multiclass_categorical", 3);
    assert!(rows.len() > 40, "{} rows", rows.len());
    let mut out = [0.0; 3];
    for (i, (row, want)) in rows.iter().enumerate() {
        m.predict_into(row, &mut out).unwrap();
        // The softmax's `exp`s through two libms, the binary budget for the same reason.
        for (k, (got, want)) in out.iter().zip(want).enumerate() {
            let d = (got - want).abs();
            assert!(d <= 1e-12, "row {i} class {k} {row:?}: differs by {d}");
        }
        assert!((out.iter().sum::<f64>() - 1.0).abs() <= 1e-12, "row {i}: {out:?}");
    }
}

#[test]
fn parsing_the_text_and_loading_the_file_agree() {
    let path = fixture_dir().join("real_binary_categorical_v4.txt");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(load_model_file(&path).unwrap(), parse_model_text(&text).unwrap());
}
