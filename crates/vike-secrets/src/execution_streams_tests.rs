//! `execution_stream` and `execution_stream_gap` against a real store in a temp directory: the cursor's two numbers
//! move as a window is begun and ended, a gap is kept once, and nothing else in the file moves.

use std::path::Path;

use super::*;
use crate::store_locator::db_path_in;

fn store() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp");
    crate::create_empty_store_for_test(&db_path_in(dir.path())).expect("a store");
    let conn = rusqlite::Connection::open(db_path_in(dir.path())).expect("open");
    conn.execute_batch("INSERT INTO hub_key (name, value) VALUES ('A_KEY', 'a-fake-hub-key');")
        .expect("seed");
    dir
}

fn stream(journal_id: &str) -> ExecutionStreamIdentity {
    ExecutionStreamIdentity {
        journal_id: journal_id.to_string(),
        tradehub: "the CI box live".to_string(),
        key_id: Some("k-1".to_string()),
    }
}

/// The hub key row, which no write here may move.
fn hub_keys(dir: &Path) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(db_path_in(dir)).expect("open");
    conn.prepare("SELECT name, value FROM hub_key ORDER BY name")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("read")
}

/// A journal nothing was stored of has no row until its first open creates one; a window begun moves
/// `writing_through` alone, and a window ended moves `stored_through` with its record's time. A crash between the two
/// leaves `writing_through` above `stored_through`: the window the next open must rewrite.
#[test]
fn a_window_begun_and_ended_moves_the_two_numbers() {
    let dir = store();
    let before = hub_keys(dir.path());
    let id = "0123456789abcdef0123456789abcdef";
    assert_eq!(read_execution_stream(dir.path(), id).expect("read"), None, "never opened");
    assert!(
        begin_execution_window(dir.path(), &stream(id), 1).is_err(),
        "no row: refused, not created"
    );

    let opened = open_execution_stream_row(dir.path(), &stream(id), 500, 1_000).expect("open");
    assert_eq!(
        (opened.stored_through, opened.writing_through, opened.first_ms),
        (None, None, 1_000),
        "nothing stored yet"
    );
    begin_execution_window(dir.path(), &stream(id), 12).expect("begin");
    let row = read_execution_stream(dir.path(), id).expect("read").expect("a row");
    assert_eq!(
        (row.stored_through, row.writing_through, row.first_contact_through),
        (None, Some(12), 500)
    );
    assert_eq!((row.stored_through_recorded_ms, row.last_stored_ms), (None, None));

    end_execution_window(dir.path(), &stream(id), 12, Some(900), 1_100).expect("end");
    begin_execution_window(dir.path(), &stream(id), 30).expect("the next window");
    let row = read_execution_stream(dir.path(), id).expect("read").expect("a row");
    assert_eq!(
        (row.stored_through, row.stored_through_recorded_ms, row.writing_through),
        (Some(12), Some(900), Some(30)),
        "a crash here leaves the window (12, 30] to rewrite"
    );
    assert_eq!((row.first_ms, row.last_stored_ms), (1_000, Some(1_100)));
    assert_eq!((row.tradehub.as_str(), row.key_id.as_deref()), ("the CI box live", Some("k-1")));
    assert_eq!(hub_keys(dir.path()), before, "no other table moved");
}

/// **Record 0 stored is not "nothing stored"**: a fresh journal's first window may end at its sequence 0, and the row
/// then says `Some(0)`, so a reopen chains from record 0 instead of starting the journal over.
#[test]
fn a_window_ending_at_record_zero_is_stored_through_zero() {
    let dir = store();
    let id = "00112233445566778899aabbccddeeff";
    open_execution_stream_row(dir.path(), &stream(id), 0, 1).expect("open");
    begin_execution_window(dir.path(), &stream(id), 0).expect("begin");
    let begun = read_execution_stream(dir.path(), id).expect("read").expect("a row");
    assert_eq!(
        (begun.stored_through, begun.writing_through),
        (None, Some(0)),
        "the window (start, 0] in flight"
    );
    end_execution_window(dir.path(), &stream(id), 0, Some(7), 2).expect("end");
    let row = open_execution_stream_row(dir.path(), &stream(id), 0, 3).expect("reopen");
    assert_eq!(
        (row.stored_through, row.stored_through_recorded_ms, row.writing_through),
        (Some(0), Some(7), Some(0))
    );
}

/// A gap is kept once, however often its window is resent, and is read with its journal.
#[test]
fn a_gap_is_kept_once_and_read_with_its_journal() {
    let dir = store();
    let id = "fedcba9876543210fedcba9876543210";
    open_execution_stream_row(dir.path(), &stream(id), 0, 100).expect("open");
    let gap = ExecutionStreamGapRow {
        journal_id: id.to_string(),
        kind: GAP_PRUNED.to_string(),
        after_sequence: 40,
        resumes_at_sequence: 90,
        lost_from_ms: Some(5),
        lost_to_ms: Some(9),
        noted_ms: 100,
        reason: Some("a snapshot-only restore".into()),
        order_client_ids: vec!["c-1".into(), "c-2".into()],
    };
    record_execution_gap(dir.path(), &gap).expect("gap");
    record_execution_gap(dir.path(), &ExecutionStreamGapRow { noted_ms: 200, ..gap.clone() })
        .expect("the resend");
    end_execution_window(dir.path(), &stream(id), 89, None, 100).expect("past the gap");
    let all = read_execution_streams(dir.path()).expect("read");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0.stored_through, Some(89));
    assert_eq!(
        all[0].1,
        vec![gap],
        "one row, the first one written, its reason and orders read back"
    );
}

/// No settings database is REFUSED, both ways, and none is created: a cursor that silently stopped being kept would
/// make every reconnect first contact.
#[test]
fn no_settings_database_is_refused_and_not_created() {
    let dir = tempfile::tempdir().expect("temp");
    let id = "0123456789abcdef0123456789abcdef";
    let refused = |e: StoreError| matches!(e.kind, StoreErrorKind::NoSettingsDatabase);
    assert!(read_execution_stream(dir.path(), id).is_err_and(refused));
    assert!(read_execution_streams(dir.path()).is_err_and(refused));
    assert!(open_execution_stream_row(dir.path(), &stream(id), 1, 1).is_err_and(refused));
    assert!(begin_execution_window(dir.path(), &stream(id), 1).is_err_and(refused));
    assert!(!db_path_in(dir.path()).exists(), "no database was created");
}

/// **The first-contact boundary is the FIRST open's head, kept whatever opens follow**: a reconnect in the middle
/// of a catch-up states a newer head and changes nothing but the identity line.
#[test]
fn the_first_contact_boundary_is_the_first_opens_head() {
    let dir = store();
    let id = "0123456789abcdef0123456789abcdef";
    open_execution_stream_row(dir.path(), &stream(id), 700, 1).expect("first open");
    begin_execution_window(dir.path(), &stream(id), 40).expect("begin");
    end_execution_window(dir.path(), &stream(id), 40, Some(9), 2).expect("end");
    let again = ExecutionStreamIdentity { tradehub: "after a restart".into(), ..stream(id) };
    let row = open_execution_stream_row(dir.path(), &again, 900, 3).expect("reconnect");
    assert_eq!((row.first_contact_through, row.stored_through, row.first_ms), (700, Some(40), 1));
    assert_eq!(row.generation, 1, "a reconnect keeps the journal's generation");
    let other =
        open_execution_stream_row(dir.path(), &stream("fedcba9876543210fedcba9876543210"), 3, 4)
            .expect("a second journal");
    assert_eq!(other.generation, 2, "each journal first heard from later comes after");
    assert_eq!(row.tradehub, "after a restart");
}

/// A store whose tables were never created holds no stream, and that is not an error.
#[test]
fn a_store_with_no_stream_table_holds_no_stream() {
    let dir = store();
    assert_eq!(read_execution_stream(dir.path(), "x").expect("read"), None);
    assert!(read_execution_streams(dir.path()).expect("read").is_empty());
    assert!(read_account_positions(dir.path(), i64::MAX).expect("read").is_empty());
}

/// **A position snapshot is kept once, and the latest at or before a time answers, per account**: a resend of its
/// window changes nothing, a journal re-identified after a copy resending what another id stored changes nothing,
/// an account with no snapshot that early is absent, and no other row of the file moves.
#[test]
fn an_account_snapshot_is_kept_once_and_the_latest_at_or_before_answers() {
    let dir = store();
    let before = hub_keys(dir.path());
    let id = "0123456789abcdef0123456789abcdef";
    let snapshot = |account: &str, sequence: u64, as_of_ms: i64| ExecutionAccountPositionsRow {
        journal_id: id.into(),
        generation: 1,
        sequence,
        as_of_ms,
        venue: "binance".into(),
        account: account.into(),
        unknown: None,
        positions: vec![vike_model::AccountPosition {
            symbol: "BTCUSDT".into(),
            position_side: vike_model::events::PositionSide::Both,
            size: 1.5,
            average_price: 100.0,
        }],
    };
    let first = snapshot("MAIN", 5, 1_000);
    record_account_positions(dir.path(), &first).expect("write");
    // The stored cell is the bytes the string-sided row wrote: typing the side moved none.
    let cell: String = rusqlite::Connection::open(db_path_in(dir.path()))
        .expect("open")
        .query_row("SELECT positions FROM execution_account_positions", [], |r| r.get(0))
        .expect("the one stored row");
    assert_eq!(
        cell,
        r#"[{"symbol":"BTCUSDT","position_side":"BOTH","size":1.5,"average_price":100.0}]"#
    );
    record_account_positions(dir.path(), &first).expect("a resend");
    let copy = ExecutionAccountPositionsRow {
        journal_id: "fedcba9876543210fedcba9876543210".into(),
        generation: 2,
        ..first.clone()
    };
    record_account_positions(dir.path(), &copy).expect("a re-identified copy");
    let later = ExecutionAccountPositionsRow {
        unknown: Some("the boot restore did not fold the journal's tail".into()),
        positions: Vec::new(),
        ..snapshot("MAIN", 50, 2_000)
    };
    record_account_positions(dir.path(), &later).expect("write");
    let alt = snapshot("ALT", 20, 1_500);
    record_account_positions(dir.path(), &alt).expect("write");

    assert!(read_account_positions(dir.path(), 999).expect("read").is_empty(), "none that early");
    assert_eq!(read_account_positions(dir.path(), 1_000).expect("read"), vec![first.clone()]);
    assert_eq!(
        read_account_positions(dir.path(), 1_999).expect("read"),
        vec![alt.clone(), first],
        "one per account, the copy never stored"
    );
    assert_eq!(read_account_positions(dir.path(), 5_000).expect("read"), vec![alt, later]);
    assert_eq!(hub_keys(dir.path()), before, "no other row moved");
}
