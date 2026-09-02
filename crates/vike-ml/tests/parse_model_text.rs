//! The v4 text parser, against a committed fixture — so no parser test needs LightGBM installed.

use std::path::PathBuf;

// The crate-root API, not the module-qualified path: as the crate's only EXTERNAL consumer, this
// file is what proves the documented public surface (lib.rs's `pub use`) actually exists.
use vike_ml::{load_model_file, parse_model_text, MlError, Objective};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/two_trees_binary_v4.txt")
}

fn fixture() -> String {
    std::fs::read_to_string(fixture_path()).expect("committed fixture must be readable")
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
    // A refusal an operator cannot act on gets widened by the next person who hits it. This one
    // has to name all four moving parts: what was seen, what is implemented, the upstream tag
    // this crate is pinned to, and the fact that the EQUALITY GATE — not this check — is the
    // proof a bump is safe. See crates/vike-ml/CLAUDE.md, written in Task 17.
    let text = fixture().replace("version=v4", "version=v5");
    let msg = parse_model_text(&text).unwrap_err().to_string();
    assert!(msg.contains("v5"), "{msg}");
    assert!(msg.contains(vike_ml::MODEL_VERSION), "{msg}");
    assert!(msg.contains(vike_ml::PINNED_LIGHTGBM_TAG), "the pinned upstream tag: {msg}");
    assert!(msg.contains("train_infer_equality"), "the gate that proves a bump: {msg}");
}

#[test]
fn a_non_binary_objective_is_refused() {
    let text = fixture().replace("objective=binary sigmoid:1", "objective=regression");
    assert!(matches!(parse_model_text(&text), Err(MlError::Unsupported(_))));
}

#[test]
fn a_multiclass_model_is_refused() {
    let text = fixture().replace("num_class=1", "num_class=3");
    assert!(matches!(parse_model_text(&text), Err(MlError::Unsupported(_))));
}

#[test]
fn a_non_default_sigmoid_is_honoured_not_assumed() {
    let text = fixture().replace("objective=binary sigmoid:1", "objective=binary sigmoid:2.5");
    let m = parse_model_text(&text).unwrap();
    assert_eq!(m.objective, Objective::Binary { sigmoid: 2.5 });
}

#[test]
fn an_objective_line_with_no_sigmoid_defaults_to_one() {
    let text = fixture().replace("objective=binary sigmoid:1", "objective=binary");
    let m = parse_model_text(&text).unwrap();
    assert_eq!(m.objective, Objective::Binary { sigmoid: 1.0 });
}

#[test]
fn a_non_positive_sigmoid_is_refused_like_lightgbms_own_constructor() {
    let text = fixture().replace("objective=binary sigmoid:1", "objective=binary sigmoid:0");
    assert!(matches!(parse_model_text(&text), Err(MlError::Unsupported(_))));
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
    // 3 leaves means internal nodes 0 and 1. A child of 2 is off the end of every per-node array,
    // and the walk that follows it indexes `decision_type[2]` — a PANIC inside a live prediction.
    // Nothing downstream re-checks this, so it is refused where a caller can still act on it.
    match parse_model_text(&with_left_child("2 -1")) {
        Err(MlError::Parse { what, .. }) => assert!(what.contains("left_child"), "{what}"),
        other => panic!("expected a Parse error naming left_child, got {other:?}"),
    }
}

#[test]
fn a_leaf_index_past_the_end_of_leaf_value_is_refused_at_load_time() {
    // -9 encodes leaf `!(-9) == 8`, and the tree has 3 leaves. The walk terminates and then
    // indexes `leaf_value[8]` — again a panic, and again only once a row happens to route there,
    // which is what makes it a production failure rather than a startup one.
    match parse_model_text(&with_left_child("1 -9")) {
        Err(MlError::Parse { what, .. }) => {
            assert!(what.contains("leaf") && what.contains("-9"), "{what}")
        }
        other => panic!("expected a Parse error naming the leaf, got {other:?}"),
    }
}

#[test]
fn a_child_pointing_back_at_its_own_node_is_refused_rather_than_looping_in_a_prediction() {
    // In range, and still fatal: node 0's left child is node 0, so a row routed left there walks
    // forever. Range alone cannot see this — the rule that does is LightGBM's own numbering, where
    // `Tree::Split` appends each child ABOVE its parent, which is what makes the walk terminate.
    match parse_model_text(&with_left_child("0 -1")) {
        Err(MlError::Parse { what, .. }) => assert!(what.contains("left_child"), "{what}"),
        other => panic!("expected a Parse error naming left_child, got {other:?}"),
    }
}

#[test]
fn a_model_truncated_at_a_tree_boundary_is_refused_rather_than_loading_under_boosted() {
    // The whole file is a two-tree model...
    assert_eq!(parse_model_text(&fixture()).unwrap().trees.len(), 2);
    // ...and this is a byte-exact PREFIX of it, cut where the second tree starts: what an
    // interrupted copy, a full disk or a partial upload leaves behind. Every remaining line is
    // valid, so it used to load as a one-tree model and score a plausible wrong number forever.
    let text = fixture();
    let truncated = &text[..text.find("Tree=1").expect("the fixture has a second tree")];
    let err = parse_model_text(truncated).unwrap_err().to_string();
    assert!(err.contains("end of trees"), "name the terminator that is missing: {err}");
    assert!(err.to_uppercase().contains("TRUNCATED"), "and say what that means: {err}");
}

#[test]
fn a_model_missing_only_its_terminator_is_refused() {
    // The minimal cut: both trees present, `end of trees` and the tail gone. Nothing else in the
    // document distinguishes it from a complete model — which is exactly why the terminator has to
    // be REQUIRED rather than merely honoured when it turns up.
    let text = fixture();
    let truncated = &text[..text.find("end of trees").unwrap()];
    assert_eq!(truncated.matches("Tree=").count(), 2, "both trees are still there");
    let err = parse_model_text(truncated).unwrap_err().to_string();
    assert!(err.contains("end of trees"), "{err}");
}

#[test]
fn a_tree_deleted_from_the_middle_is_caught_by_the_headers_own_tree_count() {
    // The terminator cannot see this one: the file still ENDS correctly. `tree_sizes=` carries one
    // entry per tree, so the header itself states how many there should be — the only check that
    // catches a model doctored anywhere other than its end.
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
    // ⚠ ONE line, no embedded `\n`. A two-line needle silently matches NOTHING on a CRLF
    // checkout — which is this repo's default on Windows — and the test then asserts that an
    // UNMODIFIED fixture is refused, i.e. it passes for the wrong reason or fails for a
    // mysterious one. `replace_all` on the single token is immune to line endings.
    let text = fixture().replace("is_linear=0", "is_linear=1");
    assert!(matches!(parse_model_text(&text), Err(MlError::Unsupported(_))));
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
    // Every header line, no `Tree=` block at all — a well-formed opening that never gets to the
    // thing this crate exists to walk.
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
    // `boosting=rf` writes a bare `average_output` line (no `=`) and LightGBM's own predictor
    // divides the summed score by num_iterations for it. This walker only ever sums, so letting
    // it fall through to the generic "not a key=value line" refusal would be an ACCIDENTAL
    // correctness property — the very next robustness edit (tolerate an unrecognised line) would
    // turn it into a score silently wrong by a factor of num_iterations. Inserted just ahead of
    // `end of trees`: parsing stops there, so a line placed after it would never be seen.
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
