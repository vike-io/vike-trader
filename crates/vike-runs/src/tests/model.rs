//! `read_model_text`: a finished run's model text, by run id, and nothing outside the runs root.

use std::path::Path;

use crate::{
    MANIFEST_FILE, MODEL_ARTIFACT, MODEL_ONNX_ARTIFACT, RESERVED_FILES, RunReadError,
    read_model_text,
};

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a file has a parent")).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A finished study run holding `model`: its artifact first, its manifest last.
fn study_run(root: &Path, run_id: &str, model: &str) {
    write(&root.join(run_id).join(MODEL_ARTIFACT), model);
    let manifest = serde_json::to_string(&super::a_manifest(run_id)).unwrap();
    write(&root.join(run_id).join(MANIFEST_FILE), &manifest);
}

#[test]
fn a_finished_runs_model_is_read_byte_for_byte() {
    let root = tempfile::tempdir().unwrap();
    let text = "tree\nversion=v4\r\nend of trees\n";
    study_run(root.path(), "1756000000-abc-0", text);
    assert_eq!(read_model_text(root.path(), "1756000000-abc-0").unwrap(), text);
}

#[test]
fn an_id_that_is_not_one_run_directory_name_is_refused_before_any_read() {
    let root = tempfile::tempdir().unwrap();
    study_run(root.path(), "inner", "secret");
    for id in ["", "..", "../inner", "inner/../inner", "/etc", "a b", "inner\\x"] {
        match read_model_text(root.path(), id) {
            Err(RunReadError::Read { why, .. }) => {
                assert!(why.contains("is not a run id"), "{why}")
            }
            other => panic!("{id:?} must be refused as no run id: {other:?}"),
        }
    }
}

#[test]
fn an_absent_run_an_unfinished_run_and_a_run_with_no_model_each_say_so() {
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        read_model_text(root.path(), "1-none-0"),
        Err(RunReadError::Missing { path }) if path.ends_with("1-none-0")
    ));
    write(&root.path().join("2-unfinished-0").join(MODEL_ARTIFACT), "tree\n");
    assert!(matches!(
        read_model_text(root.path(), "2-unfinished-0"),
        Err(RunReadError::Missing { path }) if path.ends_with(MANIFEST_FILE)
    ));
    let manifest = serde_json::to_string(&super::a_manifest("3-backtest-0")).unwrap();
    write(&root.path().join("3-backtest-0").join(MANIFEST_FILE), &manifest);
    assert!(matches!(
        read_model_text(root.path(), "3-backtest-0"),
        Err(RunReadError::Missing { path }) if path.ends_with(MODEL_ARTIFACT)
    ));
}

#[test]
fn the_model_artifact_is_a_name_a_study_may_write() {
    assert!(!RESERVED_FILES.contains(&MODEL_ARTIFACT));
    assert!(!RESERVED_FILES.contains(&MODEL_ONNX_ARTIFACT));
}

/// A run whose model is a network: `model.onnx`'s BYTES (not UTF-8 here) cross as their STANDARD base64;
/// a run holding both model files is refused, never resolved by a preference.
#[test]
fn a_runs_onnx_model_crosses_as_the_base64_of_its_bytes_and_two_models_are_refused() {
    use base64::Engine as _;

    let root = tempfile::tempdir().unwrap();
    let bytes: &[u8] = &[0x08, 0x08, 0xff, 0x00, 0x12, 0x04, b'v', b'i', b'k', b'e'];
    let manifest = serde_json::to_string(&super::a_manifest("1-onnx-0")).unwrap();
    write(&root.path().join("1-onnx-0").join(MANIFEST_FILE), &manifest);
    std::fs::write(root.path().join("1-onnx-0").join(MODEL_ONNX_ARTIFACT), bytes).unwrap();
    let text = read_model_text(root.path(), "1-onnx-0").unwrap();
    assert_eq!(text, "CAj/ABIEdmlrZQ==");
    assert_eq!(base64::engine::general_purpose::STANDARD.decode(&text).unwrap(), bytes);

    write(&root.path().join("1-onnx-0").join(MODEL_ARTIFACT), "tree\n");
    match read_model_text(root.path(), "1-onnx-0") {
        Err(RunReadError::Read { why, .. }) => assert!(why.contains("one run, one model"), "{why}"),
        other => panic!("a run with two models must be refused: {other:?}"),
    }
}
