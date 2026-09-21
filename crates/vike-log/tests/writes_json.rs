//! `init` with a temp dir must produce a rolling JSON file containing an emitted event.
use std::fs;

#[test]
fn init_writes_json_line_to_dir() {
    // ⚠ A bound `TempDir`, not a pid-keyed path. This used to clean up with a `remove_dir_all` at
    // the END and say so ("a failing assert keeps the dir for post-mortem") — which leaks on
    // exactly the runs nobody is watching, and on a box where CI and the verification lanes run as
    // DIFFERENT users a stale pid-keyed directory is one the other user cannot write into.
    let tmp = tempfile::tempdir().expect("temp log dir");
    let dir = tmp.path().to_path_buf();
    let cfg = vike_log::LogConfig {
        file_prefix: "test".to_string(),
        dir: Some(dir.clone()),
        ..Default::default()
    };
    let guards = vike_log::init(cfg);
    tracing::info!(marker = "hello_json", "integration event");
    drop(guards); // flush the non-blocking writer

    // find the rolling file (daily suffix) and assert our marker landed as JSON
    let mut found = false;
    for entry in fs::read_dir(&dir).expect("log dir exists") {
        let contents = fs::read_to_string(entry.unwrap().path()).unwrap_or_default();
        if contents.contains("hello_json") && contents.trim_start().starts_with('{') {
            found = true;
        }
    }
    assert!(found, "expected a JSON log line containing the marker in {dir:?}");
}
