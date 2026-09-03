//! The pure-Rust walker against a REAL LightGBM model and REAL LightGBM outputs — with no
//! LightGBM binary anywhere in sight.
//!
//! `train_infer_equality.rs` is the stronger test and it runs only where the binary exists, by
//! hand, on the CI box. This one runs in the DEFAULT lane, on every CI run, on a machine with nothing
//! installed, because the library's answers are FROZEN in the repository instead of recomputed.
//! That is the same trade `fixtures/r0..r6` makes for the Python oracle, one layer down.
//!
//! Regenerate deliberately: see `train_infer_equality.rs`'s `capture_the_golden_fixture`, and
//! `crates/vike-ml/CLAUDE.md` for when a regeneration is legitimate.

use std::path::PathBuf;

use vike_ml::{load_model_file, parse_model_text};

const N_FEATURES: usize = 4;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn expected_text() -> String {
    std::fs::read_to_string(fixture_dir().join("real_binary_categorical_expected.tsv"))
        .expect("committed fixture")
}

fn bits(tok: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(tok, 16).unwrap_or_else(|e| panic!("`{tok}`: {e}")))
}

/// `(row, expected_proba)` per line.
fn expectations() -> Vec<(Vec<f64>, f64)> {
    expected_text()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let f: Vec<f64> = l.split('\t').filter(|t| !t.is_empty()).map(bits).collect();
            assert_eq!(f.len(), N_FEATURES + 1, "malformed fixture line: {l}");
            (f[..N_FEATURES].to_vec(), f[N_FEATURES])
        })
        .collect()
}

fn provenance(key: &str) -> Option<String> {
    expected_text()
        .lines()
        .find_map(|l| l.trim_start_matches('#').trim().strip_prefix(&format!("{key}=")))
        .map(str::to_string)
}

#[test]
fn the_fixture_records_which_lightgbm_produced_it_and_it_is_the_pinned_one() {
    // The cheapest possible guard against the most expensive possible mistake: a golden file
    // captured from a DIFFERENT LightGBM than the one this crate is pinned to. It costs one
    // string compare, runs in the default lane on every CI run, and needs no toolchain.
    //
    // ⚠ What it does NOT do is regenerate and byte-compare. LightGBM's own docs caveat that
    // versions, compilers and systems may produce different models even under deterministic=true,
    // so a byte gate would be a cross-box flake generator — the same reason the JForex jar gate
    // compares a name/CRC32/size manifest rather than raw bytes. Pin the generator beside the
    // artifact; that is the leg that transfers.
    let tag = provenance("pinned_tag").expect("the fixture must record its pinned tag");
    assert_eq!(
        tag,
        vike_ml::PINNED_LIGHTGBM_TAG,
        "this fixture was captured from LightGBM {tag}, but the crate is now pinned to {}. \
         Either the pin moved without the fixture being regenerated, or the fixture came from the \
         wrong box. Regenerate on the CI box with train_infer_equality.rs's capture test — and run the \
         equality gate there first, because THAT is what proves the bump did not change the \
         numbers. See crates/vike-ml/CLAUDE.md.",
        vike_ml::PINNED_LIGHTGBM_TAG
    );
    assert!(provenance("lightgbm_sha256").is_some(), "the binary's sha256 must be recorded");
    assert!(provenance("lightgbm_cmake_flags").unwrap_or_default().contains("USE_OPENMP=OFF"));
}

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
    // Pins the CORRECTED understanding, so nobody reintroduces the old assertion. LightGBM's
    // GBDT::TrainOneIter calls AddBias(init_score) on the first tree when it splits — folding the
    // init score into that tree's leaf values — and AddBias also forces shrinkage_ = 1.0. Only a
    // learner that could not split at all yields a constant tree.
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
        // The frozen value came from LightGBM's own task=predict on the capture box, printed at
        // ~17 significant digits and re-parsed exactly here. 1e-12 is the same budget
        // train_infer_equality.rs states, for the same reason.
        let d = (m.predict_proba(row) - want).abs();
        assert!(d <= 1e-12, "row {i} {row:?}: differs by {d}");
    }
}

#[test]
fn parsing_the_text_and_loading_the_file_agree() {
    let path = fixture_dir().join("real_binary_categorical_v4.txt");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(load_model_file(&path).unwrap(), parse_model_text(&text).unwrap());
}
