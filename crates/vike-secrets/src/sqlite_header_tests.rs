use super::*;

#[test]
fn a_text_credential_file_is_not_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("secrets.env");
    std::fs::write(&p, "BINANCE_LIVE_API_KEY=abc\n").unwrap();
    assert!(!is_sqlite_file(&p));
    // ...while the coarse probe cannot tell them apart, which is the whole reason both exist.
    assert!(database_present(&p));
}

#[test]
fn an_absent_or_empty_path_is_not_a_database() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!is_sqlite_file(&dir.path().join("nothing-here")));
    let empty = dir.path().join("empty");
    std::fs::write(&empty, b"").unwrap();
    assert!(!is_sqlite_file(&empty));
    // Shorter than the header: a truncated write must not read as a database either.
    let stub = dir.path().join("stub");
    std::fs::write(&stub, b"SQLite").unwrap();
    assert!(!is_sqlite_file(&stub));
}

#[test]
fn a_real_database_is_recognised_by_its_own_header() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db").join(crate::DB_FILE);
    // Through the crate's own creator, so this asserts about the artifact `migrate` produces
    // rather than about a hand-planted 16 bytes.
    let conn = open_for_write(&db).unwrap().0;
    drop(conn);
    assert!(is_sqlite_file(&db));
}

/// ⚠ The failure this probe exists to prevent, driven end to end: a REAL database read as a
/// credential file. The assertion is not that the parse errors — it is that it SUCCEEDS and
/// yields nothing, which is the shape that reads as an empty store.
#[test]
fn a_database_read_as_text_is_silent_rather_than_loud() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db").join(crate::DB_FILE);
    drop(open_for_write(&db).unwrap().0);
    let bytes = std::fs::read(&db).unwrap();
    if let Ok(text) = String::from_utf8(bytes) {
        assert!(
            crate::parse_dotenv(&text).is_empty(),
            "a database parsed as KEY=VALUE yielded assignments; the refusal's premise moved"
        );
    }
    assert!(is_sqlite_file(&db), "and this is what stops it");
}
