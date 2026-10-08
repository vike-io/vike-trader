//! The v4 text parser, against a committed fixture — so no parser test needs LightGBM installed.

use std::path::PathBuf;

// The crate-root API, not the module-qualified path: as the crate's only EXTERNAL consumer, this
// file is what proves the documented public surface (lib.rs's `pub use`) actually exists.
use vike_ml::{MlError, Objective, load_model_file, parse_model_text};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/two_trees_binary_v4.txt")
}

fn fixture() -> String {
    std::fs::read_to_string(fixture_path()).expect("committed fixture must be readable")
}

/// The fixture with every `from` rewritten to `to`, refusing a `from` that matches NOTHING, which
/// would leave the UNMODIFIED fixture under test (the trap `a_linear_tree_is_refused` records).
#[track_caller]
fn fixture_with(from: &str, to: &str) -> String {
    let text = fixture();
    assert!(text.contains(from), "the needle {from:?} matched nothing in the fixture");
    text.replace(from, to)
}

/// The fixture's own objective line, the one the objective tests rewrite.
const OBJECTIVE: &str = "objective=binary sigmoid:1";

/// Rewriting `from` to `to` turns the fixture into a model the walker refuses as `Unsupported`.
#[track_caller]
fn refused_as_unsupported(from: &str, to: &str) {
    let got = parse_model_text(&fixture_with(from, to));
    assert!(matches!(got, Err(MlError::Unsupported(_))), "expected Unsupported, got {got:?}");
}

/// The objective the walker reads once the fixture's objective line says `line`.
#[track_caller]
fn objective_of(line: &str) -> Objective {
    parse_model_text(&fixture_with(OBJECTIVE, line)).expect("the rewritten model parses").objective
}

/// The needle check itself: a missed needle panics rather than handing back the fixture untouched.
#[test]
#[should_panic(expected = "matched nothing")]
fn fixture_with_refuses_a_needle_that_matches_nothing() {
    fixture_with("a line no model file carries", "x");
}

#[test]
fn the_header_carries_the_facts_inference_needs() {
    let m = parse_model_text(&fixture()).expect("fixture must parse");
    assert_eq!(m.max_feature_idx, 2);
    assert_eq!(m.n_features_expected(), 3);
    assert_eq!(m.objective, Objective::Binary { sigmoid: 1.0 });
    assert_eq!(m.feature_names, vec!["Column_0", "Column_1", "Column_2"]);
    assert_eq!(m.trees.len(), 2);
}

#[test]
fn the_degenerate_constant_tree_has_one_leaf_and_no_split_arrays() {
    let m = parse_model_text(&fixture()).unwrap();
    let t = &m.trees[0];
    assert_eq!(t.num_leaves, 1);
    assert!(t.split_feature.is_empty());
    assert!(t.decision_type.is_empty());
    assert_eq!(t.leaf_value, vec![-0.405_465_108_108_164_4]);
}

#[test]
fn a_split_tree_keeps_every_array_at_its_declared_length() {
    let m = parse_model_text(&fixture()).unwrap();
    let t = &m.trees[1];
    assert_eq!(t.num_leaves, 3);
    assert_eq!(t.split_feature, vec![1, 2]);
    assert_eq!(t.threshold, vec![4.5, 0.0]);
    assert_eq!(t.decision_type, vec![2, 1]);
    assert_eq!(t.left_child, vec![1, -1]);
    assert_eq!(t.right_child, vec![-3, -2]);
    assert_eq!(t.leaf_value, vec![0.1, -0.2, 0.3]);
    assert_eq!(t.cat_boundaries, vec![0, 1]);
    assert_eq!(t.cat_threshold, vec![5]);
}

#[test]
fn everything_after_end_of_trees_is_ignored() {
    // The fixture's tail carries `[boosting: gbdt]`, which is not a `key=value` line at all.
    // Parsing at all is the assertion; this test names why the tail is in the fixture.
    assert!(fixture().contains("[boosting: gbdt]"));
    assert!(parse_model_text(&fixture()).is_ok());
}

#[test]
fn a_version_other_than_v4_is_refused_rather_than_parsed_anyway() {
    let text = fixture().replace("version=v4", "version=v3");
    match parse_model_text(&text) {
        Err(MlError::UnsupportedVersion(v)) => assert_eq!(v, "v3"),
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
}

#[test]
fn the_version_refusal_tells_the_operator_what_to_actually_do() {
    // A refusal an operator cannot act on gets widened by the next person who hits it, so it
    // names what was seen, what is implemented, the pinned upstream tag, and the EQUALITY GATE as
    // the proof a bump is safe.
    let text = fixture().replace("version=v4", "version=v5");
    let msg = parse_model_text(&text).unwrap_err().to_string();
    assert!(msg.contains("v5"), "{msg}");
    assert!(msg.contains(vike_ml::MODEL_VERSION), "{msg}");
    assert!(msg.contains(vike_ml::PINNED_LIGHTGBM_TAG), "the pinned upstream tag: {msg}");
    assert!(msg.contains("train_infer_equality"), "the gate that proves a bump: {msg}");
}

#[test]
fn a_non_binary_objective_is_refused() {
    refused_as_unsupported(OBJECTIVE, "objective=regression");
}

#[test]
fn a_multiclass_model_is_refused() {
    refused_as_unsupported("num_class=1", "num_class=3");
}

#[test]
fn a_non_default_sigmoid_is_honoured_not_assumed() {
    assert_eq!(objective_of("objective=binary sigmoid:2.5"), Objective::Binary { sigmoid: 2.5 });
}

#[test]
fn an_objective_line_with_no_sigmoid_defaults_to_one() {
    assert_eq!(objective_of("objective=binary"), Objective::Binary { sigmoid: 1.0 });
}

#[test]
fn a_non_positive_sigmoid_is_refused_like_lightgbms_own_constructor() {
    refused_as_unsupported(OBJECTIVE, "objective=binary sigmoid:0");
}

#[test]
fn an_array_of_the_wrong_length_is_a_parse_error() {
    // 3 leaves means 2 internal nodes; one child entry is not enough.
    let text = fixture().replace("left_child=1 -1", "left_child=1");
    match parse_model_text(&text) {
        Err(MlError::Parse { what, .. }) => assert!(what.contains("left_child"), "{what}"),
        other => panic!("expected a Parse error naming left_child, got {other:?}"),
    }
}

/// The fixture's second tree with one child entry rewritten. `left_child=1 -1` is unique to it —
/// `Tree=0` is constant and carries no child arrays at all.
fn with_left_child(spec: &str) -> String {
    let text = fixture().replace("left_child=1 -1", &format!("left_child={spec}"));
    assert!(text.contains(&format!("left_child={spec}")), "the needle must have matched");
    text
}

#[test]
fn an_internal_child_index_past_the_end_of_the_nodes_is_refused_at_load_time() {
    // 3 leaves means internal nodes 0 and 1. A child of 2 would make the walk index
    // `decision_type[2]` — a PANIC inside a live prediction.
    match parse_model_text(&with_left_child("2 -1")) {
        Err(MlError::Parse { what, .. }) => assert!(what.contains("left_child"), "{what}"),
        other => panic!("expected a Parse error naming left_child, got {other:?}"),
    }
}

#[test]
fn a_leaf_index_past_the_end_of_leaf_value_is_refused_at_load_time() {
    // -9 encodes leaf `!(-9) == 8` of 3: a panic, and only once a row happens to route there.
    match parse_model_text(&with_left_child("1 -9")) {
        Err(MlError::Parse { what, .. }) => {
            assert!(what.contains("leaf") && what.contains("-9"), "{what}")
        }
        other => panic!("expected a Parse error naming the leaf, got {other:?}"),
    }
}

#[test]
fn a_child_pointing_back_at_its_own_node_is_refused_rather_than_looping_in_a_prediction() {
    // In range, and still fatal: node 0's left child is node 0, so the walk loops forever. Only
    // LightGBM's own numbering rule (child ABOVE parent) sees it.
    match parse_model_text(&with_left_child("0 -1")) {
        Err(MlError::Parse { what, .. }) => assert!(what.contains("left_child"), "{what}"),
        other => panic!("expected a Parse error naming left_child, got {other:?}"),
    }
}

#[test]
fn a_model_truncated_at_a_tree_boundary_is_refused_rather_than_loading_under_boosted() {
    // The whole file is a two-tree model...
    assert_eq!(parse_model_text(&fixture()).unwrap().trees.len(), 2);
    // ...and this is a byte-exact PREFIX of it, cut where the second tree starts. Every remaining
    // line is valid; without the terminator check it loads as a one-tree model.
    let text = fixture();
    let truncated = &text[..text.find("Tree=1").expect("the fixture has a second tree")];
    let err = parse_model_text(truncated).unwrap_err().to_string();
    assert!(err.contains("end of trees"), "name the terminator that is missing: {err}");
    assert!(err.to_uppercase().contains("TRUNCATED"), "and say what that means: {err}");
}

#[test]
fn a_model_missing_only_its_terminator_is_refused() {
    // The minimal cut: both trees present, `end of trees` and the tail gone — why the terminator
    // is REQUIRED rather than merely honoured.
    let text = fixture();
    let truncated = &text[..text.find("end of trees").unwrap()];
    assert_eq!(truncated.matches("Tree=").count(), 2, "both trees are still there");
    let err = parse_model_text(truncated).unwrap_err().to_string();
    assert!(err.contains("end of trees"), "{err}");
}

#[test]
fn a_tree_deleted_from_the_middle_is_caught_by_the_headers_own_tree_count() {
    // The file still ENDS correctly, so only the header's `tree_sizes=` count can see this.
    let text = fixture();
    let start = text.find("Tree=1").unwrap();
    let end = text.find("end of trees").unwrap();
    let doctored = format!("{}{}", &text[..start], &text[end..]);
    assert!(
        doctored.contains("end of trees"),
        "the terminator survives, so only the count can tell"
    );
    assert!(doctored.contains("tree_sizes=0 0"), "and the header still declares two trees");
    let err = parse_model_text(&doctored).unwrap_err().to_string();
    assert!(err.contains("tree_sizes"), "{err}");
    assert!(err.contains('2') && err.contains('1'), "declared vs carried: {err}");
}

#[test]
fn a_linear_tree_is_refused() {
    // ⚠ ONE line, no embedded `\n`: a two-line needle matches NOTHING on a CRLF checkout, leaving
    // the UNMODIFIED fixture under test. `fixture_with` also refuses a needle that matches nothing.
    refused_as_unsupported("is_linear=0", "is_linear=1");
}

#[test]
fn a_file_that_is_not_a_model_is_an_error_naming_the_line() {
    match parse_model_text("hello\nworld\n") {
        Err(MlError::Parse { line, .. }) => assert_eq!(line, 1),
        other => panic!("expected a Parse error on line 1, got {other:?}"),
    }
}

#[test]
fn an_empty_model_text_is_a_parse_error() {
    assert!(matches!(parse_model_text(""), Err(MlError::Parse { .. })));
}

#[test]
fn a_header_with_no_tree_block_is_a_parse_error() {
    // Every header line, no `Tree=` block at all.
    let text = "tree\n\
                version=v4\n\
                num_class=1\n\
                num_tree_per_iteration=1\n\
                max_feature_idx=0\n\
                objective=binary sigmoid:1\n\
                feature_names=Column_0\n\
                end of trees\n";
    match parse_model_text(text) {
        Err(MlError::Parse { what, .. }) => assert!(what.contains("Tree"), "{what}"),
        other => panic!("expected a Parse error naming the missing Tree block, got {other:?}"),
    }
}

#[test]
fn an_averaged_model_is_refused_by_name_not_by_accident() {
    // Why by name: the comment above `average_output` in `parse_model_text`. Inserted just ahead
    // of `end of trees`, because parsing stops there.
    let text = fixture().replace("end of trees", "average_output\nend of trees");
    let msg = parse_model_text(&text).unwrap_err().to_string();
    assert!(msg.contains("average_output"), "{msg}");
}

#[test]
fn loading_a_path_that_does_not_exist_is_an_io_error_naming_it() {
    let missing = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nope.txt");
    match load_model_file(&missing) {
        Err(MlError::Io(what)) => assert!(what.contains("nope.txt"), "{what}"),
        other => panic!("expected an Io error naming the path, got {other:?}"),
    }
}

#[test]
fn loading_the_fixture_by_path_gives_the_same_model_as_parsing_its_text() {
    assert_eq!(load_model_file(&fixture_path()).unwrap(), parse_model_text(&fixture()).unwrap());
}
