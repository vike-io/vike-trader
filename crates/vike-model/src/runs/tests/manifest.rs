//! The run id's collision rule, the manifest's common fields, and reading a manifest back.

use super::*;

/// The collision rule, stated as the property that matters: the SAME clock second, minted
/// twice, must not name one directory. The research producer's bare-seconds id failed exactly
/// this, which is why the rule is decided here rather than inherited.
#[test]
fn two_runs_starting_in_the_same_second_cannot_share_a_run_id() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");

    let first = create_run_dir(&runs, 1_756_000_000, None).unwrap();
    let second = create_run_dir(&runs, 1_756_000_000, None).unwrap();

    assert_ne!(first.run_id, second.run_id, "one clock second must not mint one id twice");
    assert_ne!(first.path, second.path);
    assert!(first.path.is_dir(), "minting a run id CREATES the directory — that is the check");
    assert!(second.path.is_dir());
}

/// Creation is the check, so an id whose directory already exists must be refused rather than
/// reused: handing one out twice lets the second run overwrite the first one's report. The
/// taken id is built with the SAME function the minter uses, so this test cannot drift away
/// from the id format.
#[test]
fn an_id_whose_directory_already_exists_is_never_handed_out() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    let taken = run_id_at(1_756_000_000, Some(FP), 0);
    std::fs::create_dir(runs.join(&taken)).unwrap();
    std::fs::write(runs.join(&taken).join(REPORT_FILE), b"the first run's result").unwrap();

    let minted = create_run_dir(&runs, 1_756_000_000, Some(FP)).unwrap();

    assert_ne!(minted.run_id, taken);
    assert!(
        minted.run_id.ends_with("-1"),
        "a taken id must advance to the NEXT seq, not to some unrelated name: {}",
        minted.run_id
    );
    assert_eq!(
        std::fs::read(runs.join(&taken).join(REPORT_FILE)).unwrap(),
        b"the first run's result",
        "the run already holding that id keeps its report"
    );
}

/// The id opens with the second the run started, so a plain directory listing sorts by when —
/// the one ordering a human scanning `user_data/runs/` actually wants. True in BOTH forms;
/// that is what makes a content address safe to put in the name at all.
#[test]
fn a_run_id_opens_with_the_second_its_run_started() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");

    for fp in [None, Some(FP)] {
        let minted = create_run_dir(&runs, 1_756_000_000, fp).unwrap();
        assert!(
            minted.run_id.starts_with("1756000000-"),
            "run id {} must open with its start second",
            minted.run_id
        );
    }
}

/// The whole reason this manifest is common: a listing renders a row off the TOP level, without
/// knowing what kind of run produced the file.
#[test]
fn every_common_field_sits_at_the_top_level_so_a_listing_needs_no_per_kind_parser() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();
    let obj = v.as_object().expect("a manifest is a JSON object");

    for key in [
        "schema",
        "run_id",
        "kind",
        "produced_by",
        "started_at",
        "finished_at",
        "git_sha",
        "fingerprint",
        "config",
    ] {
        assert!(obj.contains_key(key), "a listing row needs `{key}` at the top level");
    }
}

/// Kind-specific detail nests BELOW the common fields. Beside them is the defect the nesting
/// exists to refuse — it forces every reader to know the kind before it can read the first key.
#[test]
fn kind_specific_detail_nests_below_the_common_fields_rather_than_beside_them() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();
    let obj = v.as_object().unwrap();

    assert_eq!(obj["detail"]["strategy"], json!("sma_cross"));
    assert!(
        !obj.contains_key("strategy"),
        "`strategy` is a backtest's business and must not sit beside the common fields"
    );
}

/// `git_sha` is written even when the producer cannot name a build. A missing key would be
/// indistinguishable from a manifest predating the field; `null` says "this producer does not
/// know", which is a different and reportable answer.
#[test]
fn a_producer_that_cannot_name_a_build_still_writes_the_git_sha_key() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();

    assert_eq!(v.as_object().unwrap().get("git_sha"), Some(&serde_json::Value::Null));
}

/// A run directory holds BOTH documents, and the report is stored verbatim — a report's JSON is
/// a machine contract, and persisting it must not reshape it.
#[test]
fn a_written_run_holds_the_report_verbatim_beside_its_manifest() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    let report = json!({ "name": "sma cross", "sharpe": 1.25, "trades": 42 });

    write_run(&run.path, &a_manifest(&run.run_id), &report).unwrap();

    let back: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run.path.join(REPORT_FILE)).unwrap())
            .unwrap();
    assert_eq!(back, report);
    let m: RunManifest =
        serde_json::from_str(&std::fs::read_to_string(run.path.join(MANIFEST_FILE)).unwrap())
            .unwrap();
    assert_eq!(m.run_id, run.run_id);
    assert_eq!(m.kind, "backtest");
}

/// Persisting is additive, never a new way to fail: a runs root that cannot be created comes
/// back as a VALUE naming the path, so the caller can print the result it already computed and
/// report the failure beside it.
#[test]
fn a_runs_root_that_cannot_be_created_is_reported_rather_than_panicking() {
    let root = tempfile::tempdir().unwrap();
    let blocked = root.path().join("blocked");
    std::fs::write(&blocked, b"a file where the runs directory would go").unwrap();

    let err = create_run_dir(&blocked, 1_756_000_000, None).unwrap_err();

    assert!(
        matches!(err, RunPersistError::Dir { .. }),
        "expected a directory failure, got {err:?}"
    );
    assert!(err.to_string().contains("blocked"), "the message must name the path: {err}");
}

/// The round trip that makes a listing possible at all: what [`write_run`] wrote comes back as
/// the same COMMON fields, through the reader that lives beside the writer rather than through
/// a second spelling of the schema somewhere up the dependency graph.
#[test]
fn a_written_manifest_reads_back_with_every_common_field_intact() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    let written = a_manifest(&run.run_id);
    write_run(&run.path, &written, &json!({ "sharpe": 1.25 })).unwrap();

    let back = read_manifest(&run.path).unwrap();

    assert_eq!(back.run_id, written.run_id);
    assert_eq!(back.kind, "backtest");
    assert_eq!(back.produced_by, "backtest");
    assert_eq!(back.started_at, written.started_at);
    assert_eq!(back.finished_at, written.finished_at);
    assert_eq!(back.git_sha, None);
    assert_eq!(back.config.path.as_deref(), Some("profiles/sma.toml"));
    assert_eq!(back.detail["strategy"], json!("sma_cross"));
}

/// The manifest is written LAST, so its ABSENCE is a distinct answer rather than a corrupt
/// file: the run is being written right now, or a process died between the two writes. A reader
/// that collapsed this into "unreadable" would make a listing call a running backtest broken.
#[test]
fn a_directory_whose_manifest_is_absent_reads_as_missing_not_as_unreadable() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    // The half-written shape exactly: the report landed, the manifest has not.
    std::fs::write(run.path.join(REPORT_FILE), b"{}\n").unwrap();

    let err = read_manifest(&run.path).unwrap_err();

    assert!(matches!(err, RunReadError::Missing { .. }), "expected Missing, got {err:?}");
    assert!(
        err.to_string().contains(MANIFEST_FILE),
        "the message must name the file it looked for: {err}"
    );
}

/// A corrupt manifest is REPORTED, never treated as absent — a run that vanishes from a listing
/// is worse than one that shows as broken, and the two have different fixes.
#[test]
fn a_manifest_that_is_not_json_is_reported_with_the_parser_error_and_the_path() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    std::fs::write(run.path.join(MANIFEST_FILE), b"{ not json").unwrap();

    let err = read_manifest(&run.path).unwrap_err();

    match &err {
        RunReadError::Parse { path, why } => {
            assert!(path.ends_with(MANIFEST_FILE), "the path must be the manifest: {path:?}");
            assert!(!why.is_empty(), "serde_json's own words must be carried through");
        }
        other => panic!("expected Parse, got {other:?}"),
    }
}

/// A manifest that parses as JSON but is missing a COMMON field is a parse failure too: the
/// common fields are what a listing renders a row from, so a document without them is not a
/// manifest, whatever it is.
#[test]
fn a_json_document_missing_a_common_field_is_a_parse_failure() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    std::fs::write(run.path.join(MANIFEST_FILE), br#"{ "run_id": "r-1" }"#).unwrap();

    let err = read_manifest(&run.path).unwrap_err();

    assert!(matches!(err, RunReadError::Parse { .. }), "expected Parse, got {err:?}");
}

/// A manifest that exists and cannot be READ is its own answer — a permissions bug must not
/// wear the "not written yet" reply, for the same reason the credential store refuses to.
#[test]
fn a_manifest_that_cannot_be_read_is_distinct_from_one_that_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    // A DIRECTORY where the file belongs: unreadable as text on every platform this ships to,
    // and reached without asking a test to change file permissions.
    std::fs::create_dir(run.path.join(MANIFEST_FILE)).unwrap();

    let err = read_manifest(&run.path).unwrap_err();

    assert!(matches!(err, RunReadError::Read { .. }), "expected Read, got {err:?}");
    assert!(err.to_string().contains(MANIFEST_FILE), "the message must name the path: {err}");
}
