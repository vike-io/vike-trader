use super::*;

fn fe(name: &str, date: &str, keys: &[&str]) -> FileEntry {
    FileEntry {
        name: name.to_string(),
        date: date.to_string(),
        ts_min: 0,
        ts_max: 1,
        rows: 1,
        commit_keys: keys.iter().map(|k| k.to_string()).collect(),
    }
}

/// Replay applies a frame's adds and removes, and takes the frame's version rather than
/// incrementing — which is what keeps the counter monotonic across a log whose frames a fold
/// may already have absorbed.
#[test]
fn a_frame_adds_removes_and_carries_its_own_version() {
    let mut m = Manifest {
        version: 7,
        files: vec![fe("part-00001.parquet", "2026-01-01", &["a"])],
        ..Manifest::empty()
    };
    m.apply(&DeltaFrame {
        version: 8,
        files_add: vec![fe("part-c00000008.parquet", "2026-01-01", &["a", "b"])],
        files_rm: vec![("part-00001.parquet".into(), "2026-01-01".into())],
        ..Default::default()
    });
    assert_eq!(m.version, 8);
    assert_eq!(m.files.len(), 1);
    assert_eq!(m.files[0].name, "part-c00000008.parquet");
    assert!(m.has_commit("a") && m.has_commit("b"), "the merge output carries both keys");
}

/// A `files_rm` naming a part that is not listed is ignored, not an error. Replay has to be
/// total over any prefix of the log a crash can leave.
#[test]
fn removing_an_absent_part_is_not_an_error() {
    let mut m = Manifest { version: 1, ..Manifest::empty() };
    m.apply(&DeltaFrame {
        version: 2,
        files_rm: vec![("gone.parquet".into(), "2026-01-01".into())],
        ..Default::default()
    });
    assert_eq!(m.version, 2);
    assert!(m.files.is_empty());
}

/// Removal matches on `(name, DATE)` and not on name alone — part names are unique within a
/// `date=` directory and NOT across a series, so `part-00001.parquet` exists under every date
/// and a name-only match would drop the wrong one.
#[test]
fn removal_matches_the_date_too() {
    let mut m = Manifest {
        version: 1,
        files: vec![
            fe("part-00001.parquet", "2026-01-01", &["a"]),
            fe("part-00001.parquet", "2026-01-02", &["b"]),
        ],
        ..Manifest::empty()
    };
    m.apply(&DeltaFrame {
        version: 2,
        files_rm: vec![("part-00001.parquet".into(), "2026-01-02".into())],
        ..Default::default()
    });
    assert_eq!(m.files.len(), 1);
    assert_eq!(m.files[0].date, "2026-01-01", "the wrong date's part was dropped");
}

/// **The Q2 hatch, exercised.** `docs/decisions/0060-…`'s Q2 — what the commit log retains, and
/// for how long — is the owner's to answer, and the whole point of these two fields is that an
/// answer lands as a FRAME rather than as a second format bump. `keys_add` has one producer since
/// 2026-10-02 (the empty-day markers, `DataFusionHist::spend_keys_without_rows`); nothing writes a
/// non-empty `keys_rm`, so without this test the replay honouring it would be dead code that
/// nobody had ever run — and "the format can express it" would be a claim rather than a fact.
///
/// `keys_rm` removes a key from BOTH homes: the orphan list and every `FileEntry` carrying it.
/// That is what an EXPIRY policy needs — a key leaving the log while its part stays — and it is
/// precisely the shape the current answer (a key lives exactly as long as its rows) does not
/// use.
#[test]
fn the_q2_hatch_can_add_and_remove_keys_without_touching_files() {
    let mut m = Manifest {
        version: 1,
        files: vec![fe("part-00001.parquet", "2026-01-01", &["live-x", "pmxt:keep"])],
        ..Manifest::empty()
    };
    // keys_add: a key joins the log carrying no file at all.
    m.apply(&DeltaFrame {
        version: 2,
        keys_add: vec!["orphaned-by-policy".into()],
        ..Default::default()
    });
    assert!(m.has_commit("orphaned-by-policy"));
    assert_eq!(m.files.len(), 1, "keys_add must not touch the file index");

    // keys_rm: an expiry policy drops a key whose PART is still live.
    m.apply(&DeltaFrame {
        version: 3,
        keys_rm: vec!["live-x".into(), "orphaned-by-policy".into()],
        ..Default::default()
    });
    assert!(!m.has_commit("live-x"), "an expired key must leave the file that carries it");
    assert!(!m.has_commit("orphaned-by-policy"), "...and the orphan list");
    assert!(m.has_commit("pmxt:keep"), "a key the policy did not name is untouched");
    assert_eq!(m.files.len(), 1, "the PART stays — only its key entry went");
    assert_eq!(m.version, 3);
}

/// A key on two parts (a batch straddling a UTC midnight seals one per date) is reported ONCE.
#[test]
fn commit_keys_deduplicates_a_day_straddling_batch() {
    let m = Manifest {
        version: 2,
        files: vec![
            fe("part-00001.parquet", "2026-01-01", &["spanning"]),
            fe("part-00001.parquet", "2026-01-02", &["spanning"]),
        ],
        ..Manifest::empty()
    };
    assert_eq!(m.commit_keys(), vec!["spanning".to_string()]);
}
