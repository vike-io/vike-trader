//! TreeSHAP against hand-computed values and against an INDEPENDENT oracle: the Shapley values
//! computed from their definition, by enumerating every coalition, over the same path-dependent
//! value function (Lundberg et al. 2018, Algorithm 1, `EXPVALUE`). The oracle shares nothing with
//! the polynomial algorithm but the walker's routing, so agreement proves the path bookkeeping
//! (`extend_path`, `unwind_path`, `unwound_path_sum`), not just additivity, which a wrong split of
//! the same total would also pass.

use super::*;
use crate::model::Objective;
use crate::model::parse::parse_model_text;

/// A model text with one tree, `max_feature_idx = features - 1`, and the given tree body lines.
fn one_tree(features: usize, body: &str) -> String {
    let names: Vec<String> = (0..features).map(|i| format!("Column_{i}")).collect();
    format!(
        "tree\nversion=v4\nnum_class=1\nnum_tree_per_iteration=1\nmax_feature_idx={}\n\
         objective=regression\nfeature_names={}\n\nTree=0\n{body}\nis_linear=0\nshrinkage=1\n\n\
         end of trees\n",
        features - 1,
        names.join(" ")
    )
}

/// One numerical split on feature 0 at 0.5: left leaf 8 (3 rows), right leaf -4 (1 row).
fn one_split(covers: &str) -> String {
    one_tree(
        1,
        &format!(
            "num_leaves=2\nnum_cat=0\nsplit_feature=0\nthreshold=0.5\ndecision_type=0\n\
             left_child=-1\nright_child=-2\nleaf_value=8 -4\n{covers}"
        ),
    )
}

fn explain(model: &GbdtModel, row: &[f64]) -> Result<Vec<f64>, MlError> {
    let mut out =
        vec![f64::NAN; model.objective.output_count() * (model.n_features_expected() + 1)];
    model.shap_values_into(row, &mut out).map(|()| out)
}

/// Hand-computed: the expected value is `(3·8 + 1·(-4)) / 4 = 5`, and a row going left scores 8,
/// so feature 0 contributes `8 - 5 = 3`. Every intermediate is exact in binary, so this is `==`.
#[test]
fn a_single_split_is_attributed_by_hand() {
    let m = parse_model_text(&one_split("leaf_count=3 1\ninternal_count=4")).unwrap();
    assert_eq!(explain(&m, &[0.0]).unwrap(), vec![3.0, 5.0]);
    // Going right: -4 - 5 = -9.
    assert_eq!(explain(&m, &[1.0]).unwrap(), vec![-9.0, 5.0]);
}

/// A constant tree has nothing to attribute: every contribution is zero and the expected value is
/// the leaf, covers or none (LightGBM's `ExpectedValue` returns the leaf before reading a count).
#[test]
fn a_constant_tree_contributes_only_its_expected_value() {
    let m = parse_model_text(&one_tree(2, "num_leaves=1\nnum_cat=0\nleaf_value=0.25")).unwrap();
    assert_eq!(explain(&m, &[1.0, 2.0]).unwrap(), vec![0.0, 0.0, 0.25]);
}

/// Covers that are absent, malformed, zero or do not add up refuse the EXPLANATION and nothing
/// else: the same text still loads and predicts, because inference never reads them.
#[test]
fn unusable_covers_refuse_the_explanation_but_not_the_prediction() {
    for (covers, says) in [
        ("", "missing or malformed"),
        ("leaf_count=3 x\ninternal_count=4", "missing or malformed"),
        ("leaf_count=3 1 7\ninternal_count=4", "missing or malformed"),
        ("leaf_count=3 0\ninternal_count=3", "zero cover"),
        ("leaf_count=3 1\ninternal_count=5", "do not add up"),
    ] {
        let m = parse_model_text(&one_split(covers)).expect("a cover line never refuses a load");
        assert_eq!(m.raw_score(&[0.0]), 8.0, "{covers:?}");
        let refusal = explain(&m, &[0.0]).unwrap_err();
        assert!(
            matches!(&refusal, MlError::Unsupported(text) if text.contains(says)
                && text.contains("tree 0")),
            "{covers:?}: {refusal}"
        );
    }
}

#[test]
fn a_wrong_output_buffer_is_refused() {
    let m = parse_model_text(&one_split("leaf_count=3 1\ninternal_count=4")).unwrap();
    assert!(matches!(m.shap_values_into(&[0.0], &mut [0.0; 3]), Err(MlError::Shape(_))));
    assert!(matches!(m.shap_values_into(&[0.0], &mut [0.0; 1]), Err(MlError::Shape(_))));
}

#[test]
fn the_batch_is_the_single_row_call_per_row_and_checks_the_width() {
    let m = real("real_multiclass_categorical", Objective::Multiclass { class_count: 3 });
    let rows = golden_rows("real_multiclass_categorical");
    let flat: Vec<f64> = rows.iter().flatten().copied().collect();
    let batch = m.shap_values_batch(&flat, 4).unwrap();
    let per_row: Vec<f64> = rows.iter().flat_map(|row| explain(&m, row).unwrap()).collect();
    assert_eq!(batch.len(), rows.len() * 3 * 5);
    assert!(batch.iter().zip(&per_row).all(|(a, b)| a.to_bits() == b.to_bits()));
    assert!(matches!(m.shap_values_batch(&flat[..9], 3), Err(MlError::Shape(_))));
}

// ---------------------------------------------------------------------------------------------
// The oracle: Shapley values from their definition.
// ---------------------------------------------------------------------------------------------

/// Lundberg et al. Algorithm 1 (`EXPVALUE`): the tree's output when only the features in `known`
/// (a bit set) are known — a known feature follows the row, an unknown one weighs both branches by
/// their covers.
fn expectation(tree: &Tree, row: &[f64], known: u32, node: i32) -> f64 {
    if node < 0 {
        return tree.leaf_value[!node as usize];
    }
    let at = node as usize;
    if known & (1 << tree.split_feature[at]) != 0 {
        return expectation(tree, row, known, tree.decide(at, row));
    }
    let (left, right) = (tree.left_child[at], tree.right_child[at]);
    let weigh = |child: i32| f64::from(tree.cover(child)) * expectation(tree, row, known, child);
    (weigh(left) + weigh(right)) / f64::from(tree.cover(node))
}

fn factorial(n: u32) -> f64 {
    (1..=n).map(f64::from).product()
}

/// Every coalition of the other features, weighted `|S|! (n - |S| - 1)! / n!`, summed over trees
/// into `predict_contrib`'s layout.
fn shapley_by_enumeration(model: &GbdtModel, row: &[f64]) -> Vec<f64> {
    let n = model.n_features_expected() as u32;
    let outputs = model.objective.output_count();
    let width = n as usize + 1;
    let mut out = vec![0.0; outputs * width];
    for (index, tree) in model.trees.iter().enumerate() {
        let block = &mut out[(index % outputs) * width..][..width];
        let value = |known: u32| {
            if tree.num_leaves == 1 { tree.leaf_value[0] } else { expectation(tree, row, known, 0) }
        };
        block[n as usize] += value(0);
        for feature in 0..n {
            for known in 0..(1u32 << n) {
                if known & (1 << feature) != 0 {
                    continue;
                }
                let size = known.count_ones();
                let weight = factorial(size) * factorial(n - size - 1) / factorial(n);
                block[feature as usize] += weight * (value(known | 1 << feature) - value(known));
            }
        }
    }
    out
}

/// `1e-12` relative to the score's size: the two sides add the same tree terms in different
/// orders (per coalition vs per path), which is rounding, never a different attribution.
fn assert_close(got: &[f64], want: &[f64], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!((g - w).abs() <= 1e-12 * w.abs().max(1.0), "{what} [{i}]: got {g}, want {w}");
    }
}

/// A tree that tests feature 0 twice on one path (the `unwind_path` branch), with a categorical
/// node and a NaN-as-missing node, so every routing rule the hot branch takes is exercised.
fn twisted_tree() -> GbdtModel {
    parse_model_text(&one_tree(
        3,
        "num_leaves=5\nnum_cat=1\nsplit_feature=0 1 0 2\nthreshold=0.5 0 1.5 2.5\n\
         decision_type=0 1 10 0\nleft_child=1 2 -1 -4\nright_child=3 -3 -2 -5\n\
         leaf_value=1 -2 3.5 0.25 -1.75\nleaf_count=4 6 5 7 3\ninternal_count=25 15 10 10\n\
         cat_boundaries=0 1\ncat_threshold=5",
    ))
    .unwrap()
}

#[test]
fn treeshap_equals_the_enumerated_shapley_values_on_a_tree_that_repeats_a_feature() {
    let m = twisted_tree();
    for row in [
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 9.0],
        [0.2, 2.0, 1.0],
        [f64::NAN, 0.0, 3.0],
        [1.0, 0.0, 2.0],
        [7.0, 2.0, f64::NAN],
        [0.4, -1.0, 2.5],
    ] {
        let got = explain(&m, &row).unwrap();
        assert_close(&got, &shapley_by_enumeration(&m, &row), &format!("row {row:?}"));
        let total = got.iter().sum::<f64>();
        assert!((total - m.raw_score(&row)).abs() <= 1e-12, "row {row:?}: additivity");
    }
}

// ---------------------------------------------------------------------------------------------
// The committed golden models (real LightGBM v4.7.0 output, 4 features, a categorical column).
// ---------------------------------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn real(stem: &str, objective: Objective) -> GbdtModel {
    let m = parse_model_text(&fixture(&format!("{stem}_v4.txt"))).unwrap();
    assert_eq!(m.objective, objective);
    m
}

/// The feature rows of `<stem>_expected.tsv`: its first four fields, IEEE-754 bits in hex.
fn golden_rows(stem: &str) -> Vec<Vec<f64>> {
    let rows: Vec<Vec<f64>> = fixture(&format!("{stem}_expected.tsv"))
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.split('\t')
                .take(4)
                .map(|t| f64::from_bits(u64::from_str_radix(t, 16).unwrap()))
                .collect()
        })
        .collect();
    assert!(rows.len() >= 32, "{stem}: {} rows", rows.len());
    rows
}

/// The brief's acceptance test: on every golden model and row, each output's contributions sum to
/// its raw score minus its expected value — and they are the enumerated Shapley values.
#[test]
fn on_the_golden_models_the_values_sum_to_the_raw_score_and_are_the_shapley_values() {
    for (stem, objective) in [
        ("real_binary_categorical", Objective::Binary { sigmoid: 1.0 }),
        ("real_regression_categorical", Objective::Regression),
        ("real_multiclass_categorical", Objective::Multiclass { class_count: 3 }),
    ] {
        let m = real(stem, objective);
        let outputs = objective.output_count();
        let mut raw = vec![0.0; outputs];
        let mut expected_values: Option<Vec<f64>> = None;
        for row in golden_rows(stem) {
            let got = explain(&m, &row).unwrap();
            m.raw_scores_into(&row, &mut raw).unwrap();
            for (k, block) in got.as_chunks::<5>().0.iter().enumerate() {
                let contributions = block[..4].iter().sum::<f64>();
                let (expected, score) = (block[4], raw[k]);
                assert!(
                    (contributions - (score - expected)).abs() <= 1e-12 * score.abs().max(1.0),
                    "{stem} row {row:?} output {k}: contributions {contributions}, raw {score}, \
                     expected value {expected}"
                );
            }
            // The expected value is the model's, not the row's.
            let bias: Vec<f64> = got.as_chunks::<5>().0.iter().map(|block| block[4]).collect();
            assert_eq!(expected_values.get_or_insert_with(|| bias.clone()), &bias, "{stem}");
            assert_close(&got, &shapley_by_enumeration(&m, &row), &format!("{stem} {row:?}"));
        }
    }
}
