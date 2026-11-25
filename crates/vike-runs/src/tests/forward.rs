//! A forward run (book chapter 22): written when its mount opens, a finished run at once, its end a document of
//! its own added later.

use super::*;
use std::assert_matches;

fn a_forward_run() -> ForwardRun {
    ForwardRun {
        schema: FORWARD_RUN_SCHEMA,
        mount_id: "binance__BTCUSDT__1m".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        account: "binance/PAPER-1".to_string(),
        strategy_file: "trend/ema.rhai".to_string(),
        strategy_sha256: "ab".repeat(32),
        params: serde_json::from_value(json!({ "fast": 12, "slow": 26.5 })).unwrap(),
        reference_run: Some("1767225600-ab12cd34ef56ab78-0".to_string()),
        started_ms: 1_767_225_600_123,
    }
}

#[test]
fn a_forward_run_is_a_finished_run_at_once_and_its_end_is_added_later() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let run = a_forward_run();
    let dir = write_forward_run(&runs, &run, Some("abc1234")).unwrap();
    assert!(dir.run_id.starts_with("1767225600-"), "minted at the mount's second: {}", dir.run_id);

    let manifest = read_manifest(&dir.path).unwrap();
    assert_eq!(manifest.kind, FORWARD_RUN_KIND);
    assert_eq!(manifest.produced_by, "vike-tradehub");
    assert_eq!(manifest.started_at, utc_rfc3339(1_767_225_600));
    assert_eq!(manifest.git_sha.as_deref(), Some("abc1234"));
    assert_eq!(manifest.config.path.as_deref(), Some("trend/ema.rhai"));
    assert_eq!(manifest.config.name.as_deref(), Some("binance__BTCUSDT__1m"));
    // The scan lists it as any finished run, and a run id finds it.
    assert_eq!(scan_runs(&runs).runs.len(), 1);
    let found = finished_run_dir(&runs, &dir.run_id).unwrap();

    assert_eq!(read_forward_run(&found).unwrap(), (run.clone(), None), "still running");
    end_forward_run(&found, 1_767_312_000_000).unwrap();
    assert_eq!(read_forward_run(&found).unwrap(), (run, Some(1_767_312_000_000)));
    assert!(RESERVED_FILES.contains(&FORWARD_END_FILE));
}

#[test]
fn a_run_of_another_kind_is_not_read_as_a_forward_run() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    write_run(&run.path, &a_manifest(&run.run_id), &json!({})).unwrap();
    let err = read_forward_run(&run.path).unwrap_err();
    assert_matches!(err, RunReadError::Parse { .. }, "expected Parse, got {err:?}");
    assert!(err.to_string().contains("not a `forward` run"), "{err}");
}
