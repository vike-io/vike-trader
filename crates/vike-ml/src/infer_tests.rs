use crate::error::MlError;
use crate::parse::parse_model_text;

/// A one-split tree over one feature. `decision_type` and `threshold` are the two knobs each
/// test varies; going LEFT reaches leaf 0 (`!(-1) == 0`, value 7) and RIGHT leaf 1 (value -7).
///
/// ⚠ The trailing `end of trees` is not decoration: the parser requires it, because a model
/// text without one is a TRUNCATED file — see `parse_model_text`.
fn one_split(decision_type: u8, threshold: &str) -> String {
    format!(
        "tree\n\
             version=v4\n\
             num_class=1\n\
             num_tree_per_iteration=1\n\
             max_feature_idx=0\n\
             objective=binary sigmoid:1\n\
             feature_names=Column_0\n\
             \n\
             Tree=0\n\
             num_leaves=2\n\
             num_cat=0\n\
             split_feature=0\n\
             threshold={threshold}\n\
             decision_type={decision_type}\n\
             left_child=-1\n\
             right_child=-2\n\
             leaf_value=7 -7\n\
             is_linear=0\n\
             shrinkage=1\n\
             \n\
             end of trees\n"
    )
}

/// Route one value and report the leaf value it reached.
fn route(decision_type: u8, threshold: &str, fval: f64) -> f64 {
    let m = parse_model_text(&one_split(decision_type, threshold)).expect("test model");
    m.trees[0].leaf_value_for(&[fval])
}

#[test]
fn a_leaf_index_is_the_ones_complement_of_the_node_not_its_negation() {
    // left_child = -1 encodes leaf `!(-1) == 0`. A naive `-node` maps it to leaf 1 — and
    // worse, maps leaf 0 onto node 0, the root. Going left must reach 7.0.
    assert_eq!(route(0, "1", 0.0), 7.0);
    assert_eq!(route(0, "1", 9.0), -7.0);
}

#[test]
fn the_threshold_comparison_is_less_than_or_equal_not_less_than() {
    // Exactly AT the threshold goes LEFT. `<` would send it right.
    assert_eq!(route(0, "1", 1.0), 7.0);
}

#[test]
fn a_non_negative_child_recurses_into_the_next_internal_node() {
    let text = "tree\n\
                    version=v4\n\
                    num_class=1\n\
                    num_tree_per_iteration=1\n\
                    max_feature_idx=1\n\
                    objective=binary sigmoid:1\n\
                    feature_names=Column_0 Column_1\n\
                    \n\
                    Tree=0\n\
                    num_leaves=3\n\
                    num_cat=0\n\
                    split_feature=0 1\n\
                    threshold=1 5\n\
                    decision_type=0 0\n\
                    left_child=1 -1\n\
                    right_child=-3 -2\n\
                    leaf_value=1 2 3\n\
                    is_linear=0\n\
                    shrinkage=1\n\
                    \n\
                    end of trees\n";
    let m = parse_model_text(text).unwrap();
    let t = &m.trees[0];
    assert_eq!(t.leaf_value_for(&[0.0, 0.0]), 1.0, "left, then left -> leaf 0");
    assert_eq!(t.leaf_value_for(&[0.0, 9.0]), 2.0, "left, then right -> leaf 1");
    assert_eq!(t.leaf_value_for(&[9.0, 0.0]), 3.0, "right at the root -> leaf 2");
}

#[test]
fn a_nan_is_remapped_to_zero_when_the_node_has_no_missing_type() {
    // missing_type == NoMissing: LightGBM sets fval = 0.0 and then compares. It does NOT
    // consult default_left. Two thresholds either side of 0.0 prove the remap happened —
    // a walker that special-cased "NaN -> default side" would answer the same both times.
    assert_eq!(route(0, "1", f64::NAN), 7.0, "0.0 <= 1 -> left");
    assert_eq!(route(0, "-1", f64::NAN), -7.0, "0.0 > -1 -> right");
}

#[test]
fn missing_type_nan_sends_a_nan_to_the_default_side() {
    let nan_default_left = (2u8 << 2) | 2; // missing_type = NaN, default_left set
    let nan_default_right = 2u8 << 2; //      missing_type = NaN, default_left clear
    assert_eq!(route(nan_default_left, "-1", f64::NAN), 7.0);
    assert_eq!(route(nan_default_right, "1", f64::NAN), -7.0);
}

#[test]
fn missing_type_zero_sends_a_zero_to_the_default_side() {
    let zero_default_left = (1u8 << 2) | 2;
    // 0.0 > -5, so the ordinary comparison would go RIGHT; the missing rule takes it left.
    assert_eq!(route(zero_default_left, "-5", 0.0), 7.0);
    // ...and a value that is NOT zero still takes the ordinary comparison.
    assert_eq!(route(zero_default_left, "-5", 1.0), -7.0);
}

#[test]
fn a_nan_at_a_zero_missing_node_is_remapped_then_still_counts_as_the_zero_missing_value() {
    // Rule 1 remaps NaN -> 0.0 first (mt != Nan). Rule 2 then asks "IS 0.0 this node's
    // missing value" — mt == Zero and is_zero(0.0) — which is true, so it takes the
    // default_left side, NOT the ordinary `0.0 > -5` comparison that would send it right.
    let zero_default_left = (1u8 << 2) | 2;
    assert_eq!(route(zero_default_left, "-5", f64::NAN), 7.0);
}

#[test]
fn the_default_left_bit_is_ignored_when_the_value_is_present() {
    // decision_type = 2: default_left set, missing_type NoMissing. A present value above the
    // threshold still goes right — default_left is about MISSING, not about ties.
    assert_eq!(route(2, "1", 9.0), -7.0);
}

#[test]
fn a_row_too_short_for_the_split_feature_routes_as_missing() {
    let m = parse_model_text(&one_split(0, "1")).unwrap();
    // No feature 0 at all -> NaN -> remapped to 0.0 -> 0.0 <= 1 -> left.
    assert_eq!(m.trees[0].leaf_value_for(&[]), 7.0);
}

#[test]
fn a_constant_tree_returns_its_only_leaf_without_walking() {
    let text = "tree\n\
                    version=v4\n\
                    num_class=1\n\
                    num_tree_per_iteration=1\n\
                    max_feature_idx=0\n\
                    objective=binary sigmoid:1\n\
                    feature_names=Column_0\n\
                    \n\
                    Tree=0\n\
                    num_leaves=1\n\
                    num_cat=0\n\
                    leaf_value=0.25\n\
                    is_linear=0\n\
                    shrinkage=1\n\
                    \n\
                    end of trees\n";
    let m = parse_model_text(text).unwrap();
    assert_eq!(m.trees[0].leaf_value_for(&[f64::NAN]), 0.25);
}

/// A one-split CATEGORICAL tree over one feature. `decision_type = 3` is the categorical bit
/// PLUS default_left — set deliberately, so every test below also proves default_left is
/// ignored on this path. `threshold` is a GROUP INDEX into `cat_boundaries`, not a value.
/// Left reaches leaf 0 (value 7), right leaf 1 (value -7).
fn one_categorical(cat_boundaries: &str, cat_threshold: &str, num_cat: u32) -> String {
    format!(
        "tree\n\
             version=v4\n\
             num_class=1\n\
             num_tree_per_iteration=1\n\
             max_feature_idx=0\n\
             objective=binary sigmoid:1\n\
             feature_names=Column_0\n\
             \n\
             Tree=0\n\
             num_leaves=2\n\
             num_cat={num_cat}\n\
             split_feature=0\n\
             threshold=0\n\
             decision_type=3\n\
             left_child=-1\n\
             right_child=-2\n\
             leaf_value=7 -7\n\
             cat_boundaries={cat_boundaries}\n\
             cat_threshold={cat_threshold}\n\
             is_linear=0\n\
             shrinkage=1\n\
             \n\
             end of trees\n"
    )
}

/// Categories 0 and 2 are in the left set: bitset word 0 = 0b101 = 5.
fn route_cat(fval: f64) -> f64 {
    let m = parse_model_text(&one_categorical("0 1", "5", 1)).expect("test model");
    m.trees[0].leaf_value_for(&[fval])
}

#[test]
fn a_category_in_the_bitset_goes_left_and_one_outside_it_goes_right() {
    assert_eq!(route_cat(0.0), 7.0);
    assert_eq!(route_cat(1.0), -7.0);
    assert_eq!(route_cat(2.0), 7.0);
    assert_eq!(route_cat(3.0), -7.0);
}

#[test]
fn a_nan_category_goes_right_unconditionally_even_with_default_left_set() {
    // The fixture's decision_type is 3 — categorical AND default_left. The categorical path
    // never reads that bit; a walker that shared the numerical path's missing handling would
    // send this LEFT.
    assert_eq!(route_cat(f64::NAN), -7.0);
}

#[test]
fn a_negative_category_goes_right_unconditionally() {
    // LightGBM's own `categorical_feature` docs: negative values are treated as missing. Same
    // default_left trap as above.
    assert_eq!(route_cat(-1.0), -7.0);
    // ⚠ ...but -0.5 is NOT negative once truncated: `static_cast<int>(-0.5) == 0`, the sign
    // test does not fire, and category 0 IS in the set. The truncation happens FIRST.
    assert_eq!(route_cat(-0.5), 7.0);
}

#[test]
fn a_category_id_truncates_toward_zero_and_is_not_rounded() {
    // 2.9 -> 2, which IS in {0, 2} -> left. Rounding would give 3 -> right.
    assert_eq!(route_cat(2.9), 7.0);
    // 1.9 -> 1, which is NOT in the set -> right. Rounding would give 2 -> left.
    assert_eq!(route_cat(1.9), -7.0);
}

#[test]
fn a_category_past_the_end_of_the_bitset_goes_right() {
    // One word covers categories 0..31. Category 64 needs word 2, which this group does not
    // have — the bit is implicitly 0, not an index panic.
    assert_eq!(route_cat(64.0), -7.0);
    assert_eq!(route_cat(1e18), -7.0, "and an absurd id must not panic either");
}

#[test]
fn the_bitset_spans_words_so_a_high_category_is_looked_up_in_the_right_one() {
    // Two words: word 0 = 0 (no category 0..31), word 1 = 2 (bit 1 -> category 33).
    let m = parse_model_text(&one_categorical("0 2", "0 2", 1)).unwrap();
    let t = &m.trees[0];
    assert_eq!(t.leaf_value_for(&[33.0]), 7.0);
    assert_eq!(t.leaf_value_for(&[1.0]), -7.0, "bit 1 of WORD 0 is clear");
    assert_eq!(t.leaf_value_for(&[32.0]), -7.0, "bit 0 of word 1 is clear");
}

#[test]
fn the_threshold_of_a_categorical_node_is_a_group_index_not_a_value() {
    // Two groups. Group 0 = word 0 = 1 (category 0 only); group 1 = word 1 = 2 (category 1
    // only). The node's threshold selects group 1, so category 1 goes left and category 0
    // does not — the exact inverse of reading `threshold` as a value.
    // Anchored on the surrounding newlines, not the bare token: `threshold=0` is also a
    // substring of `cat_threshold=0 2`, and an unanchored `.replace` would corrupt that
    // bitset instead of (or as well as) the split's own `threshold=` line.
    let text = one_categorical("0 1 2", "1 2", 2).replace("\nthreshold=0\n", "\nthreshold=1\n");
    let m = parse_model_text(&text).unwrap();
    let t = &m.trees[0];
    assert_eq!(t.leaf_value_for(&[1.0]), 7.0);
    assert_eq!(t.leaf_value_for(&[0.0]), -7.0);
}

const FIXTURE: &str = include_str!("../tests/fixtures/two_trees_binary_v4.txt");

#[test]
fn shrinkage_is_already_in_the_leaf_value_and_must_not_be_applied_again() {
    let text = "tree\n\
                    version=v4\n\
                    num_class=1\n\
                    num_tree_per_iteration=1\n\
                    max_feature_idx=0\n\
                    objective=binary sigmoid:1\n\
                    feature_names=Column_0\n\
                    \n\
                    Tree=0\n\
                    num_leaves=1\n\
                    num_cat=0\n\
                    leaf_value=2\n\
                    is_linear=0\n\
                    shrinkage=0.1\n\
                    \n\
                    end of trees\n";
    let m = parse_model_text(text).unwrap();
    // 2.0, not 0.2. LightGBM multiplies leaf_value by learning_rate IN PLACE at training
    // time; the `shrinkage=` field is bookkeeping for resumed boosting and the prediction
    // path never reads it. Re-applying it scales the score by learning_rate SQUARED.
    assert_eq!(m.raw_score(&[0.0]), 2.0);
}

#[test]
fn the_constant_tree_sums_like_any_other_and_needs_no_base_score_step() {
    let m = parse_model_text(FIXTURE).unwrap();
    // Feature 1 = 3.0 <= 4.5 -> the categorical node; category 2 is in {0, 2} -> leaf 0 (0.1).
    // The score is Tree=0's constant init score PLUS that leaf, and nothing else: adding a
    // separate base score would double-count it.
    let expected = -0.405_465_108_108_164_4 + 0.1;
    assert_eq!(m.raw_score(&[0.0, 3.0, 2.0]), expected);
    // ...and a different route reaches a different leaf, so the sum is genuinely per-row.
    assert_eq!(m.raw_score(&[0.0, 9.0, 2.0]), -0.405_465_108_108_164_4 + 0.3);
    assert_eq!(m.raw_score(&[0.0, 3.0, 1.0]), -0.405_465_108_108_164_4 + -0.2);
}

#[test]
fn a_splitting_first_tree_is_summed_like_any_other_not_read_as_a_standalone_init_score() {
    // The exact misconception the plan itself was corrected for: treating `trees[0]`
    // specially — `acc = trees[0].leaf_value[0]; for t in &trees[1..] { acc +=
    // t.leaf_value_for(row) }` — is silently right on a CONSTANT first tree (this file's
    // `two_trees_binary_v4.txt` fixture) and silently WRONG the moment the first tree
    // actually splits, which `AddBias` makes the common shape on a real model. This fixture's
    // Tree=0 splits on feature 0, so a row that reaches its SECOND leaf must not fall back to
    // leaf 0's value.
    let text = "tree\n\
                    version=v4\n\
                    num_class=1\n\
                    num_tree_per_iteration=1\n\
                    max_feature_idx=0\n\
                    objective=binary sigmoid:1\n\
                    feature_names=Column_0\n\
                    \n\
                    Tree=0\n\
                    num_leaves=2\n\
                    num_cat=0\n\
                    split_feature=0\n\
                    threshold=0.5\n\
                    decision_type=0\n\
                    left_child=-1\n\
                    right_child=-2\n\
                    leaf_value=0.2 0.3\n\
                    is_linear=0\n\
                    shrinkage=1\n\
                    \n\
                    Tree=1\n\
                    num_leaves=1\n\
                    num_cat=0\n\
                    leaf_value=0.05\n\
                    is_linear=0\n\
                    shrinkage=0.1\n\
                    \n\
                    end of trees\n";
    let m = parse_model_text(text).unwrap();
    // Routes LEFT in Tree=0 (0.0 <= 0.5) -> leaf 0 (0.2). This row alone cannot catch the
    // bug: `trees[0].leaf_value[0]` also happens to be 0.2 here.
    assert_eq!(m.raw_score(&[0.0]), 0.2 + 0.05);
    // Routes RIGHT in Tree=0 (9.0 > 0.5) -> leaf 1 (0.3). The "tree 0 IS the init score"
    // implementation would still add leaf_value[0] (0.2), giving 0.25 instead of 0.35.
    assert_eq!(m.raw_score(&[9.0]), 0.3 + 0.05);
}

/// A model of CONSTANT trees, one per value — so `raw_score` is exactly the fold over `values`,
/// with the routing removed from the question entirely.
fn constant_trees(values: &[f64]) -> String {
    let mut text = String::from(
        "tree\n\
             version=v4\n\
             num_class=1\n\
             num_tree_per_iteration=1\n\
             max_feature_idx=0\n\
             objective=binary sigmoid:1\n\
             feature_names=Column_0\n\
             \n",
    );
    for (i, v) in values.iter().enumerate() {
        // `{v}` is Rust's shortest round-tripping decimal, so the parsed leaf value is the SAME
        // BITS as the one folded below — the comparison is about the fold, not the formatter.
        text.push_str(&format!(
            "Tree={i}\nnum_leaves=1\nnum_cat=0\nleaf_value={v}\nis_linear=0\nshrinkage=1\n\n"
        ));
    }
    text.push_str("end of trees\n");
    text
}

#[test]
fn the_tree_sum_is_the_naive_fold_lightgbm_uses_and_not_the_workspaces_compensated_one() {
    // ⚠ THE test that carries `raw_score`'s "a better sum would fail that gate, correctly" into
    // the DEFAULT lane. Before it existed the rule was enforced by `train_infer_equality.rs`'s
    // `the_raw_scores_match_bit_for_bit` alone — which is `#[ignore]`d and needs the LightGBM
    // binary, so it runs on the CI box by hand and nowhere else. MEASURED, not assumed: replacing
    // this function's body with `vike_model::py_sum(...)` passed the entire default lane
    // (120/120, golden fixture included) on 2026-08-09.
    //
    // The construction is metrics.rs's `py_sum_recovers_what_a_naive_fold_would_silently_drop`
    // turned inside out. One tree's leaf value large enough that the ulp beside it (2.0 at
    // 1e16) swallows each of eight 1.0s individually — 1.0 is half a ulp, and ties-to-even
    // rounds it back down every single time — but not their SUM, which is 8. LightGBM
    // accumulates `score += leaf_value_[~node]` in a plain loop over trees, so 1e16 IS the
    // right answer here and 1e16 + 8 is the wrong one, however much better a number it is.
    let mut values = vec![1e16];
    values.extend_from_slice(&[1.0; 8]);

    let naive = values.iter().fold(0.0f64, |acc, v| acc + v);
    let compensated = vike_model::py_sum(values.iter().copied());
    assert_ne!(
        naive.to_bits(),
        compensated.to_bits(),
        "this input must actually discriminate the two folds, or the test below proves nothing"
    );

    let m = parse_model_text(&constant_trees(&values)).unwrap();
    let got = m.raw_score(&[0.0]);
    assert_eq!(
        got.to_bits(),
        naive.to_bits(),
        "raw_score must reproduce LightGBM's plain accumulation, bit for bit: got {got}, \
             LightGBM's fold gives {naive}"
    );
    assert_ne!(
        got.to_bits(),
        compensated.to_bits(),
        "raw_score returned the COMPENSATED sum ({compensated}). It is the more accurate \
             number and it is the wrong one: the walker's whole contract is that it reproduces \
             LightGBM's own accumulation, and train_infer_equality.rs compares the two BIT FOR BIT"
    );
}

#[test]
fn the_probability_is_the_sigmoid_of_the_raw_score() {
    let m = parse_model_text(FIXTURE).unwrap();
    let row = [0.0, 3.0, 2.0];
    let raw = m.raw_score(&row);
    // ⚠ `libm::exp`, matching `predict_proba`'s own call — and this is an EXACT `assert_eq!` on
    // f64, so the two spellings are not interchangeable here. `(-raw).exp()` reaches the
    // platform's libm while production reaches the crate; they differ in the last bit for many
    // inputs, which would redden this on whichever box happened to be one of them. That is not
    // a tolerance to add: the assertion's job is to pin that the transform IS the logistic of
    // the raw score, so it must spell the transform the way production spells it.
    assert_eq!(m.predict_proba(&row), 1.0 / (1.0 + libm::exp(-raw)));
    assert!(m.predict_proba(&row) < 0.5, "a negative raw score is a probability below a half");
}

#[test]
fn a_non_default_sigmoid_from_the_header_scales_the_raw_score() {
    let m = parse_model_text(&FIXTURE.replace("sigmoid:1", "sigmoid:2")).unwrap();
    let row = [0.0, 3.0, 2.0];
    let raw = m.raw_score(&row);
    // The SECOND exact-f64 assert over this transform, and it is not the same expression as the
    // one above — the header's `sigmoid` scales the argument, so it is `exp(-2 * raw)` here
    // against `exp(-raw)` there. Both convert with production for the reason spelled out above;
    // a sweep that converted only the first spelling would leave this one comparing two
    // different libms and failing intermittently by box.
    assert_eq!(m.predict_proba(&row), 1.0 / (1.0 + libm::exp(-2.0 * raw)));
}

#[test]
fn the_batch_api_refuses_a_row_width_the_model_cannot_use() {
    let m = parse_model_text(FIXTURE).unwrap();
    // max_feature_idx = 2, so a row must be at least 3 wide.
    assert!(matches!(m.predict_proba_batch(&[1.0, 2.0], 2), Err(MlError::Shape(_))));
    assert!(matches!(m.predict_proba_batch(&[1.0, 2.0, 3.0, 4.0], 3), Err(MlError::Shape(_))));
    assert!(matches!(m.predict_proba_batch(&[1.0, 2.0, 3.0], 0), Err(MlError::Shape(_))));
}

#[test]
fn the_batch_api_returns_one_value_per_row_in_row_order() {
    let m = parse_model_text(FIXTURE).unwrap();
    let flat = [0.0, 3.0, 2.0, 0.0, 9.0, 2.0];
    let got = m.predict_proba_batch(&flat, 3).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0], m.predict_proba(&flat[0..3]));
    assert_eq!(got[1], m.predict_proba(&flat[3..6]));
    let raws = m.raw_scores_batch(&flat, 3).unwrap();
    assert_eq!(raws, vec![m.raw_score(&flat[0..3]), m.raw_score(&flat[3..6])]);
}
