//! Schema back-compat, the content-addressed run id, and the chrono-free timestamp.

use super::*;

/// ⚠ **THE BACK-COMPATIBILITY PROOF, and the reason both new fields carry
/// `#[serde(default)]`.** Every field but `detail` is required at deserialize time, so a
/// document written before these fields existed becomes `RunReadError::Parse` the moment one is
/// required — which `crates/vike-studio-core/src/listing.rs`'s `list_runs` renders as
/// `RunUnreadable` and DROPS. Written as TEXT rather than through the writer, deliberately: the
/// bytes on somebody's disk are what this test is about, and a round trip through the current
/// struct could never see the loss.
#[test]
fn a_manifest_written_before_the_schema_field_existed_still_reads() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    std::fs::write(
        run.path.join(MANIFEST_FILE),
        br#"{
  "run_id": "1756000000-4242-0",
  "kind": "backtest",
  "produced_by": "backtest",
  "started_at": "2026-08-24T09:15:04Z",
  "finished_at": "2026-08-24T09:15:16Z",
  "git_sha": null,
  "config": { "path": "profiles/sma.toml", "name": "sma cross" },
  "detail": { "strategy": "sma_cross" }
}
"#,
    )
    .unwrap();

    let back = read_manifest(&run.path).unwrap();

    assert_eq!(back.schema, 0, "absence means PRE-SCHEMA, which is a real and reportable answer");
    assert_eq!(back.fingerprint, None, "a manifest predating the address names none");
    assert_eq!(back.kind, "backtest", "and every field it DID carry is untouched");
}

/// A manifest this build writes states its own version, so a decoder never has to guess.
#[test]
fn a_freshly_written_manifest_states_the_schema_this_build_writes() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();

    assert_eq!(v["schema"], json!(MANIFEST_SCHEMA));
}

/// `fingerprint` follows `git_sha`'s rule exactly: written even when the producer cannot name
/// one, because "does not know" and "wrote no such field" are different answers to a reader.
#[test]
fn a_producer_that_cannot_address_its_inputs_still_writes_the_fingerprint_key() {
    let v = serde_json::to_value(a_manifest("r-1")).unwrap();

    assert_eq!(v.as_object().unwrap().get("fingerprint"), Some(&serde_json::Value::Null));
}

/// The kind a backtest writes is EXPORTED now, so the two places that spell it cannot drift —
/// `crates/vike-studio/src/panes/research.rs` carried its own copy and said in a comment that this
/// is what should replace it.
#[test]
fn the_backtest_kind_is_exported_and_is_what_this_producer_writes() {
    assert_eq!(BACKTEST_RUN_KIND, "backtest");
    assert_eq!(a_manifest("r-1").kind, BACKTEST_RUN_KIND);
}

/// The content-addressing property, stated as what it is FOR: two runs over the same inputs
/// carry the same address in their names, so a human scanning `user_data/runs/` sees at a
/// glance which runs are comparable. The full digest lives in the manifest; this is the
/// legible prefix of it.
#[test]
fn two_runs_with_the_same_inputs_carry_the_same_address_in_their_ids() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");

    let first = create_run_dir(&runs, 1_756_000_000, Some(FP)).unwrap();
    let second = create_run_dir(&runs, 1_756_000_000, Some(FP)).unwrap();

    assert_ne!(first.run_id, second.run_id, "two runs are still two directories");
    assert!(first.run_id.starts_with("1756000000-9f86d081884c7d65-"), "{}", first.run_id);
    assert!(second.run_id.starts_with("1756000000-9f86d081884c7d65-"), "{}", second.run_id);
    assert!(second.run_id.ends_with("-1"), "the second takes the next seq: {}", second.run_id);
}

/// ⚠ **THE ORDERING CONTRACT.** `crates/vike-studio-core/src/listing.rs`'s `list_runs` sorts by
/// DIRECTORY NAME and its module doc says that is chronological because the id opens with unix
/// seconds; `crates/vike-studio/src/panes/research.rs` reverses that list to get "Newest first". A
/// bare content hash would make both arbitrary and silently wrong, which is why the address is
/// a SUFFIX.
#[test]
fn a_content_address_does_not_disturb_the_chronological_name_sort() {
    let mut ids = vec![
        run_id_at(1_756_000_200, Some("ffffffffffffffff"), 0),
        run_id_at(1_756_000_000, Some("0000000000000000"), 0),
        run_id_at(1_756_000_100, Some("aaaaaaaaaaaaaaaa"), 0),
    ];
    ids.sort();

    assert_eq!(
        ids,
        vec![
            "1756000000-0000000000000000-0".to_string(),
            "1756000100-aaaaaaaaaaaaaaaa-0".to_string(),
            "1756000200-ffffffffffffffff-0".to_string(),
        ],
        "a plain name sort must still be chronological whatever the addresses are"
    );
}

/// A producer that cannot address its inputs keeps the shape that was there before, pid and
/// all — so the study producer and every pre-address run sort and read exactly as they did.
#[test]
fn a_run_with_no_address_keeps_the_pid_form() {
    let pid = std::process::id();

    assert_eq!(run_id_at(1_756_000_000, None, 3), format!("1756000000-{pid}-3"));
}

/// The id is a DIRECTORY NAME, so an address is sanitized before it becomes one: a value
/// carrying separators would otherwise be a path traversal built out of something a producer
/// computed.
#[test]
fn an_address_that_is_not_bare_hex_cannot_escape_the_runs_root() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");

    let minted = create_run_dir(&runs, 1_756_000_000, Some("../../etc/passwd")).unwrap();

    assert_eq!(minted.path.parent(), Some(runs.as_path()), "minted outside the runs root");
    assert!(!minted.run_id.contains('/') && !minted.run_id.contains('\\'));
    assert!(!minted.run_id.contains(".."), "run id {}", minted.run_id);
}

/// An address with nothing usable in it is the same answer as no address at all, rather than
/// an empty segment that would make two ids collide on a name a human cannot read.
#[test]
fn an_address_with_no_usable_characters_falls_back_to_the_pid_form() {
    let pid = std::process::id();

    assert_eq!(run_id_at(1_756_000_000, Some("///"), 0), format!("1756000000-{pid}-0"));
}

/// ⚠ The formatter is no longer `chrono`'s — it is [`crate::time::civil_from_days`], because
/// this module moved into the crate at the BOTTOM of the graph and `chrono` may not follow it
/// there (`crates/vike-cli/Cargo.toml`'s whole identity is being light). These instants are the
/// pins: each was MEASURED against the chrono implementation this replaced, in the commit before
/// the move, so a divergence is a REGRESSION in a document already on people's disks rather than
/// a formatting preference.
#[test]
fn the_chrono_free_formatter_is_byte_identical_to_the_one_it_replaced() {
    for (secs, expect) in [
        (0_i64, "1970-01-01T00:00:00Z"),
        (1_756_000_000, "2025-08-24T01:46:40Z"),
        (1_756_000_012, "2025-08-24T01:46:52Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (-1, "1969-12-31T23:59:59Z"),
        (-86_400, "1969-12-31T00:00:00Z"),
        (253_402_300_799, "9999-12-31T23:59:59Z"),
    ] {
        assert_eq!(utc_rfc3339(secs), expect, "{secs}");
    }
}

/// The fallback the old doc promised: a second no calendar date can hold must not panic. With
/// integer-exact civil math there is no such second inside `i64::MIN/86_400`, so the guard is
/// the four-digit-year boundary instead — and it must still return a string rather than abort.
#[test]
fn an_unrepresentable_second_falls_back_to_the_raw_number_instead_of_panicking() {
    assert_eq!(utc_rfc3339(i64::MIN), i64::MIN.to_string());
    assert_eq!(utc_rfc3339(i64::MAX), i64::MAX.to_string());
}
