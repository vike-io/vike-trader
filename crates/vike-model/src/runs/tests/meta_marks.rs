//! The tag sidecar and the mark store: dedupe, append-only notes, re-marking and name refusal.

use std::path::{Path, PathBuf};

use super::*;

// ─── the tag sidecar and the mark store ─────────────────────────────────────────────────────

/// A FINISHED run directory: a manifest, which is the completion marker this module's doc
/// describes. Written as TEXT rather than through `write_run`, so these cases pin the on-disk
/// document a later reader has to survive rather than a round trip of our own struct.
fn plant(runs: &Path, run_id: &str, kind: &str) -> PathBuf {
    let dir = runs.join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        format!(
            r#"{{"run_id":"{run_id}","kind":"{kind}","produced_by":"backtest",
                     "started_at":"2025-08-24T01:46:40Z","finished_at":"2025-08-24T01:46:41Z",
                     "git_sha":null,"config":{{"path":"p.toml","name":null}},"detail":null}}"#
        ),
    )
    .unwrap();
    dir
}

/// Tags are a SET with a stable order: adding one twice does not duplicate it, and the order is
/// FIRST-INSERT so a rendered row does not shuffle between calls.
#[test]
fn tags_dedupe_and_keep_first_insert_order() {
    let root = tempfile::tempdir().unwrap();
    let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");

    let m = add_tags(&dir, &["ci".into(), "fee-fix".into()], None, 1_756_000_000).unwrap();
    assert_eq!(m.tags, vec!["ci".to_string(), "fee-fix".to_string()]);
    let m = add_tags(&dir, &["fee-fix".into(), "green".into()], None, 1_756_000_001).unwrap();
    assert_eq!(m.tags, vec!["ci".to_string(), "fee-fix".to_string(), "green".to_string()]);
    assert_eq!(m.schema, META_SCHEMA, "the schema key ships WITH the document, never after it");
    // …and it is on DISK with the schema, not merely in the returned value.
    let on_disk = read_meta(&dir).unwrap();
    assert_eq!(on_disk.tags, m.tags);
    assert_eq!(on_disk.schema, META_SCHEMA);
}

/// Notes APPEND and never overwrite. A note is evidence — §7.2's "re-marking is explicit and
/// recorded" is the same instinct — so the second one must not delete the first.
#[test]
fn notes_append_with_their_own_timestamps() {
    let root = tempfile::tempdir().unwrap();
    let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");

    add_tags(&dir, &[], Some("first look"), 1_756_000_000).unwrap();
    let m = add_tags(&dir, &[], Some("after the fee fix"), 1_756_000_060).unwrap();
    assert_eq!(m.notes.len(), 2);
    assert_eq!(m.notes[0].text, "first look");
    assert_eq!(m.notes[1].at, utc_rfc3339(1_756_000_060));
}

/// An ABSENT sidecar is an EMPTY one, never an error: a run minted before tagging existed, or
/// one nobody has tagged, is the ordinary case and not a broken run.
#[test]
fn a_run_with_no_sidecar_reads_as_empty() {
    let root = tempfile::tempdir().unwrap();
    let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");
    let m = read_meta(&dir).unwrap();
    assert!(m.tags.is_empty() && m.notes.is_empty());
    assert!(!dir.join(META_FILE).exists(), "reading must not create one");
}

/// ⚠ A sidecar that EXISTS and will not parse is a REFUSAL, not a re-mint. The file is the only
/// copy of whatever somebody wrote in it, and overwriting it from an empty document would
/// delete their notes silently — the one outcome a metadata write may not have.
#[test]
fn an_unparseable_sidecar_is_refused_rather_than_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");
    std::fs::write(dir.join(META_FILE), "not json at all").unwrap();

    let err = add_tags(&dir, &["ci".into()], None, 1_756_000_000).unwrap_err();
    assert!(err.to_string().contains(META_FILE), "the message names the file: {err}");
    assert_eq!(
        std::fs::read_to_string(dir.join(META_FILE)).unwrap(),
        "not json at all",
        "and the bytes on disk are untouched"
    );
}

/// A mark is a POINTER, and moving it RECORDS where it pointed — §7.2: "Re-marking is explicit
/// and recorded", which is only true if the move is kept where somebody can see it.
#[test]
fn re_marking_records_what_moved() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");

    let first =
        write_mark(&marks, "baseline/momentum", "1756000000-1-0", Some("v1"), 1_756_000_000)
            .unwrap();
    assert!(first.history.is_empty(), "the first mark has nothing to record");
    assert_eq!(first.schema, META_SCHEMA);

    let second =
        write_mark(&marks, "baseline/momentum", "1799999999-2-0", None, 1_799_999_999).unwrap();
    assert_eq!(second.run_id, "1799999999-2-0");
    assert_eq!(second.history.len(), 1, "the previous pointer is kept");
    assert_eq!(second.history[0].run_id, "1756000000-1-0");
    assert_eq!(second.history[0].note.as_deref(), Some("v1"));

    assert_eq!(read_mark(&marks, "baseline/momentum").unwrap().run_id, "1799999999-2-0");
    assert!(
        marks.join("baseline").join("momentum.json").is_file(),
        "a mark is a file NAMED for the mark"
    );
}

/// A mark NAME becomes a PATH, so traversal, invisibility and reserved device names are refused
/// at the door rather than left to the filesystem.
///
/// ⚠ The Windows device names matter here even though NO test in this workspace runs on Windows:
/// a mark called `con` would be written and resolved here and be unopenable there, and nothing
/// downstream would ever discover it.
#[test]
fn a_mark_name_that_would_escape_or_break_a_path_is_refused() {
    for ok in ["baseline/momentum", "prod", "v1.2_rc-3", "nightly/eu/open"] {
        assert!(valid_mark_name(ok).is_ok(), "`{ok}` must be accepted");
    }
    for bad in [
        "",
        "/leading",
        "trailing/",
        "a//b",
        "../escape",
        "a/../b",
        ".hidden",
        "with space",
        "with\\backslash",
        "with:colon",
        "a/b/c/d/e",
        "con",
        "COM1",
        "nul",
        "nul.baseline",
    ] {
        assert!(valid_mark_name(bad).is_err(), "`{bad}` must be refused");
    }
}

/// ⚠ A refused name never reaches the filesystem, in EITHER direction. `valid_mark_name` being
/// right is only half of it — the check has to be the first thing both doors do, or a traversal
/// typed on a command line becomes a write outside the marks root.
#[test]
fn a_refused_name_writes_nothing_and_reads_nothing() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");
    let outside = root.path().join("escaped.json");

    let err = write_mark(&marks, "../escaped", "1756000000-1-0", None, 1_756_000_000)
        .expect_err("a traversal is refused");
    assert!(matches!(&err, MarkError::BadName { .. }), "got {err:?}");
    assert!(!outside.exists(), "nothing was written outside the marks root");
    assert!(!marks.exists(), "…and the marks root was not even created");

    assert!(matches!(
        read_mark(&marks, "../escaped").expect_err("and the read door refuses too"),
        MarkError::BadName { .. }
    ));
}

/// A mark pointing at a run that is gone is DANGLING, which is a different answer from "no such
/// mark": the first says a prune or an `rm` took the run, the second says the name was never
/// set. They have different fixes, so they are different errors — and the SELECTOR layer that
/// distinguishes them lives in `crates/vike-cli/src/cmd/runs/selector.rs`, which is why this
/// case asserts only the two halves this module owns.
#[test]
fn a_missing_mark_and_a_bad_name_are_different_answers() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");
    write_mark(&marks, "baseline", "1799999999-2-0", None, 1_799_999_999).unwrap();

    assert!(matches!(read_mark(&marks, "nope").expect_err("never set"), MarkError::Missing { .. }));
    let err = read_mark(&marks, "nope").unwrap_err();
    assert!(err.to_string().contains("tag"), "it names the verb that sets one: {err}");
    assert_eq!(read_mark(&marks, "baseline").unwrap().run_id, "1799999999-2-0");
}

/// The history is BOUNDED. A mark moved on every CI run would otherwise grow one file forever,
/// which is the failure the log retention in this workspace already exists for.
#[test]
fn the_mark_history_is_capped() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");
    for i in 0..(MARK_HISTORY_MAX + 5) {
        write_mark(
            &marks,
            "baseline",
            &format!("1756000000-1-{i}"),
            None,
            1_756_000_000 + i as i64,
        )
        .unwrap();
    }
    let m = read_mark(&marks, "baseline").unwrap();
    assert_eq!(m.history.len(), MARK_HISTORY_MAX);
    assert_eq!(
        m.history[0].run_id,
        format!("1756000000-1-{}", MARK_HISTORY_MAX + 3),
        "most recent first — the oldest moves fall off the end"
    );
    assert_eq!(
        m.run_id,
        format!("1756000000-1-{}", MARK_HISTORY_MAX + 4),
        "…and the pointer itself is the last write, not a history row"
    );
}

/// The write is ATOMIC and leaves no droppings: an interrupted CI job must leave the OLD mark
/// rather than a truncated file, and a successful one must not leave the temp file behind for a
/// listing to trip over.
#[test]
fn a_written_mark_leaves_no_temporary_file_behind() {
    let root = tempfile::tempdir().unwrap();
    let marks = root.path().join("marks");
    write_mark(&marks, "baseline/m", "1756000000-1-0", None, 1_756_000_000).unwrap();
    write_mark(&marks, "baseline/m", "1756000001-1-0", None, 1_756_000_001).unwrap();

    let kept: Vec<String> = std::fs::read_dir(marks.join("baseline"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(kept, vec!["m.json".to_string()], "one file, and no `.tmp` beside it");
}
