use super::*;

/// A two-tree binary model in the shape `crates/vike-ml/src/model/parse.rs`'s `parse_model_text`
/// accepts, small enough that every routing decision below is arithmetic done by hand.
///
/// Tree=0 splits feature 0 at 0.5 then feature 1 at 0.25; Tree=1 splits feature 0 at 0.75.
/// Feature 2 is declared and NEVER split on, which is the zero row the report must still carry.
fn two_tree_model() -> String {
    "tree\n\
         version=v4\n\
         num_class=1\n\
         num_tree_per_iteration=1\n\
         label_index=0\n\
         max_feature_idx=2\n\
         objective=binary sigmoid:1\n\
         feature_names=alpha beta gamma\n\
         tree_sizes=0 0\n\
         \n\
         Tree=0\n\
         num_leaves=3\n\
         num_cat=0\n\
         split_feature=0 1\n\
         split_gain=8 2\n\
         threshold=0.5 0.25\n\
         decision_type=2 2\n\
         left_child=1 -1\n\
         right_child=-3 -2\n\
         leaf_value=0.4 -0.6 0.2\n\
         is_linear=0\n\
         shrinkage=1\n\
         \n\
         Tree=1\n\
         num_leaves=2\n\
         num_cat=0\n\
         split_feature=0\n\
         split_gain=3\n\
         threshold=0.75\n\
         decision_type=2\n\
         left_child=-1\n\
         right_child=-2\n\
         leaf_value=0.05 -0.05\n\
         is_linear=0\n\
         shrinkage=0.1\n\
         \n\
         end of trees\n"
        .to_string()
}

/// `1 / (1 + exp(-raw))` for `sigmoid:1` — spelled as the arithmetic rather than as a decimal
/// literal, so the assertion pins the transform and not a number somebody typed.
fn sigmoid(raw: f64) -> f64 {
    1.0 / (1.0 + (-raw).exp())
}

#[test]
fn a_model_text_loads_and_scores_rows_into_a_probability_series() {
    let m = ScoringModel::from_text(&two_tree_model()).unwrap();
    assert_eq!(m.n_features(), 3);
    // Row A: 0.0 <= 0.5 -> node 1; 0.0 <= 0.25 -> leaf 0 (0.4). Tree=1: 0.0 <= 0.75 -> 0.05.
    // Row B: 1.0 > 0.5 -> leaf 2 (0.2).            Tree=1: 1.0 > 0.75 -> -0.05.
    let series = m.score_rows(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0], 3).unwrap();
    assert_eq!(series.len(), 2);
    assert!((series[0] - sigmoid(0.45)).abs() < 1e-12, "{series:?}");
    assert!((series[1] - sigmoid(0.15)).abs() < 1e-12, "{series:?}");
    // ...and the single-row doors agree with the batch, through the trait and through the
    // checked call alike.
    assert_eq!(m.score_row(&[0.0, 0.0, 0.0]).unwrap().to_bits(), series[0].to_bits());
    assert_eq!(ProbaModel::predict_proba(&m, &[1.0, 0.0, 0.0]).to_bits(), series[1].to_bits());
}

#[test]
fn the_importance_report_carries_every_feature_including_the_ones_no_tree_split_on() {
    let m = ScoringModel::from_text(&two_tree_model()).unwrap();
    let imp = m.importance();
    assert_eq!(imp.n_features, 3);
    // Feature 0: split in both trees (8 + 3). Feature 1: once (2). Feature 2: never.
    assert!((imp.gain[0] - 11.0).abs() < 1e-12);
    assert_eq!(imp.splits, vec![2, 1, 0]);

    let ranked = m.importance_ranked();
    assert_eq!(ranked.len(), 3, "the never-split feature must still have a row: {ranked:?}");
    assert_eq!(ranked.iter().map(|r| r.index).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(
        ranked.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
        vec![Some("alpha".into()), Some("beta".into()), Some("gamma".into())]
    );
    assert_eq!(
        ranked[2],
        FeatureWeight { index: 2, name: Some("gamma".into()), gain: 0.0, splits: 0 }
    );
}

#[test]
fn a_tie_in_gain_is_broken_by_column_index_so_a_panel_cannot_reshuffle_its_own_rows() {
    // Features 1 and 2 carry the same gain; feature 0 is never split on.
    let text = "tree\nversion=v4\nnum_class=1\nnum_tree_per_iteration=1\nlabel_index=0\n\
                    max_feature_idx=2\nobjective=binary sigmoid:1\n\
                    feature_names=alpha beta gamma\ntree_sizes=0\n\
                    Tree=0\nnum_leaves=3\nnum_cat=0\nsplit_feature=2 1\nsplit_gain=4 4\n\
                    threshold=0.5 0.5\ndecision_type=2 2\nleft_child=1 -1\nright_child=-3 -2\n\
                    leaf_value=0.1 0.2 0.3\nis_linear=0\nshrinkage=1\nend of trees\n";
    let ranked = ScoringModel::from_text(text).unwrap().importance_ranked();
    assert_eq!(ranked.iter().map(|r| r.index).collect::<Vec<_>>(), vec![1, 2, 0]);
    // ...and the order is the SAME every time it is asked, not merely once.
    let again = ScoringModel::from_text(text).unwrap().importance_ranked();
    assert_eq!(ranked, again);
}

#[test]
fn a_truncated_model_text_comes_back_as_a_named_error_rather_than_a_panic() {
    // The half-written export: a byte-exact PREFIX of a real model, cut before its terminator.
    let full = two_tree_model();
    let cut = full.split("end of trees").next().unwrap().to_string();
    let e = ScoringModel::from_text(&cut).unwrap_err();
    assert!(matches!(e, ModelError::Text(MlError::Parse { .. })), "{e:?}");
    assert!(e.to_string().contains("TRUNCATED"), "{e}");
}

#[test]
fn a_text_that_was_never_a_model_comes_back_as_a_named_error() {
    for junk in ["", "not a model at all", "{\"json\": true}"] {
        let e = ScoringModel::from_text(junk).unwrap_err();
        assert!(matches!(e, ModelError::Text(_)), "{junk:?} -> {e:?}");
        assert!(!e.to_string().is_empty());
    }
}

#[test]
fn a_model_file_that_cannot_be_read_names_the_path_it_tried() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-model.txt");
    let e = ScoringModel::from_path(&missing).unwrap_err();
    match &e {
        ModelError::Read { path, .. } => assert!(path.contains("no-such-model.txt"), "{path}"),
        other => panic!("expected a Read refusal, got {other:?}"),
    }
    // ...and the happy path through the same door, so the test above cannot pass vacuously.
    let good = dir.path().join("model.txt");
    std::fs::write(&good, two_tree_model()).unwrap();
    assert_eq!(ScoringModel::from_path(&good).unwrap().n_features(), 3);
}

#[test]
fn a_row_narrower_than_the_model_is_refused_by_the_checked_entry_points() {
    let m = ScoringModel::from_text(&two_tree_model()).unwrap();
    let e = m.score_row(&[0.0, 0.0]).unwrap_err();
    assert!(e.to_string().contains('3'), "the refusal must name the width needed: {e}");
    assert!(m.score_rows(&[0.0, 0.0], 2).is_err());
    // A WIDER row is fine — the trailing columns are simply not split on.
    assert_eq!(
        m.score_row(&[0.0, 0.0, 0.0, 99.0]).unwrap().to_bits(),
        m.score_row(&[0.0, 0.0, 0.0]).unwrap().to_bits()
    );
}

#[test]
fn a_matrix_that_is_not_a_whole_number_of_rows_is_refused() {
    let m = ScoringModel::from_text(&two_tree_model()).unwrap();
    let e = m.score_rows(&[0.0, 0.0, 0.0, 1.0], 3).unwrap_err();
    assert!(matches!(e, ModelError::Rows(MlError::Shape(_))), "{e:?}");
}

#[test]
fn a_text_whose_two_halves_declare_different_widths_is_refused() {
    // The walker takes the FIRST `max_feature_idx=`, the importance parser the LAST. One text
    // carrying both is the only way they can disagree, and reconciling them would pick a width
    // at random.
    let text =
        two_tree_model().replace("max_feature_idx=2\n", "max_feature_idx=2\nmax_feature_idx=4\n");
    let e = ScoringModel::from_text(&text).unwrap_err();
    assert_eq!(e, ModelError::Width { walker: 3, importance: 5 });
    assert!(e.to_string().contains("wrong feature"), "{e}");
}

#[test]
fn a_model_that_names_some_of_its_columns_but_not_all_is_refused() {
    let text =
        two_tree_model().replace("feature_names=alpha beta gamma", "feature_names=alpha beta");
    let e = ScoringModel::from_text(&text).unwrap_err();
    assert_eq!(e, ModelError::FeatureNames { declared: 2, width: 3 });
}

#[test]
fn a_model_that_names_no_column_at_all_still_loads_and_reports_unnamed_features() {
    let text = two_tree_model().replace("feature_names=alpha beta gamma\n", "");
    let m = ScoringModel::from_text(&text).unwrap();
    assert!(m.feature_names().is_empty());
    let ranked = m.importance_ranked();
    assert!(ranked.iter().all(|r| r.name.is_none()), "{ranked:?}");
    // The report is otherwise unchanged — the names were never what carried the attribution.
    assert_eq!(ranked.iter().map(|r| r.index).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(m.score_rows(&[0.0, 0.0, 0.0], 3).unwrap().len(), 1);
}

#[test]
fn a_text_the_walker_accepts_but_whose_split_arrays_cannot_be_paired_is_refused_at_load() {
    // ⚠ The DECLARED narrowing, pinned so it is a decision on record rather than an accident:
    // `crates/vike-ml/src/model/parse.rs`'s `parse_tree` never reads `split_gain`, so this text walks
    // — and this surface still refuses it, because it promises both answers or neither.
    let text = "tree\nversion=v4\nnum_class=1\nnum_tree_per_iteration=1\nlabel_index=0\n\
                    max_feature_idx=1\nobjective=binary sigmoid:1\n\
                    Tree=0\nnum_leaves=2\nnum_cat=0\nsplit_feature=0\nthreshold=0.5\n\
                    decision_type=2\nleft_child=-1\nright_child=-2\nleaf_value=0.1 0.2\n\
                    is_linear=0\nshrinkage=1\nend of trees\n";
    assert!(vike_ml::parse_model_text(text).is_ok(), "the walker must still accept it");
    let e = ScoringModel::from_text(text).unwrap_err();
    assert!(matches!(e, ModelError::Importance(_)), "{e:?}");
    assert!(e.to_string().contains("Tree=0"), "the refusal must name the tree: {e}");
}

/// Compile-time, and the point is the same one `crates/vike-ml/src/seam/learner.rs`'s
/// `the_seam_carries_the_bounds_a_parallel_search_needs` makes: a caller scoring a series from
/// a pool needs these bounds, and it lives in another crate that would fail with a `rayon`
/// error instead of naming the cause.
#[test]
fn the_scoring_model_carries_the_bounds_a_parallel_scorer_needs() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<ScoringModel>();
    send_sync::<ModelError>();
}
