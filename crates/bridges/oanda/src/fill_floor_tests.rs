use super::*;
use vike_paths::scratch::ScratchDir;

fn scratch() -> ScratchDir {
    ScratchDir::create_in(&std::env::temp_dir(), "oanda-fill-floor").expect("a scratch directory")
}

/// The module doc's list: a usable persisted floor is the start; everything else is the account's latest.
#[test]
fn a_usable_persisted_floor_is_the_start_and_anything_else_is_the_latest_transaction() {
    // First start: nothing persisted is today's behaviour.
    assert_eq!(start_floor(None, 100), 100);
    // The floor the last run left, behind the latest: the gap is read.
    assert_eq!(start_floor(Some(90), 100), 90);
    assert_eq!(start_floor(Some(100), 100), 100, "nothing happened while down");
    // An account that was reset (its ids start over): a floor above the latest is no floor.
    assert_eq!(start_floor(Some(101), 100), 100);
    // The bound is inclusive: exactly MAX_LOOKBACK behind is read, one more is not.
    assert_eq!(start_floor(Some(1_000), 1_000 + MAX_LOOKBACK), 1_000);
    assert_eq!(start_floor(Some(999), 1_000 + MAX_LOOKBACK), 1_000 + MAX_LOOKBACK);
    // A latest of zero (a summary that states none) has no gap to read and no floor above it to trust.
    assert_eq!(start_floor(Some(5), 0), 0);
}

#[test]
fn a_floor_round_trips_and_an_absent_one_is_nothing() {
    let dir = scratch();
    let file = FloorFile::new(&dir, "101-001-1234567-001");
    assert_eq!(file.read(), None, "a first start has no file");
    file.write(84);
    assert_eq!(file.read(), Some(84));
    file.write(90);
    assert_eq!(file.read(), Some(90), "a write replaces the floor whole");
    // A second handle on the same account (the next run) reads what the last one left.
    assert_eq!(FloorFile::new(&dir, "101-001-1234567-001").read(), Some(90));
}

/// One file per account, and its name carries a hash, never the account id.
#[test]
fn an_account_has_its_own_file_and_the_file_names_no_account_id() {
    let dir = scratch();
    let (a, b) =
        (FloorFile::new(&dir, "101-001-1234567-001"), FloorFile::new(&dir, "101-001-7654321-001"));
    a.write(10);
    b.write(20);
    assert_eq!((a.read(), b.read()), (Some(10), Some(20)));
    let names: Vec<String> = std::fs::read_dir(&*dir)
        .expect("read the directory")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    for name in &names {
        assert!(name.starts_with("oanda-fill-floor-") && name.ends_with(".json"), "{name}");
        assert!(!name.contains("1234567") && !name.contains("7654321"), "{name}");
    }
}

/// A file that exists and cannot be read is not "nothing persisted": it reads as no floor (so the start is
/// today's), and it is never touched.
#[test]
fn an_unreadable_floor_reads_as_none_and_the_file_is_left_as_it_is() {
    let dir = scratch();
    let file = FloorFile::new(&dir, "acct");
    for garbage in ["not json", "{}", r#"{"floor":"84"}"#, r#"{"floor":-1}"#, ""] {
        std::fs::write(dir.join(&file.name), garbage).expect("plant the file");
        assert_eq!(file.read(), None, "{garbage:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join(&file.name)).expect("still there"),
            garbage,
            "the file is not ours to repair"
        );
    }
}

/// A place that cannot hold the file (a plain file where the directory should be) costs a warning, once, and
/// never a panic: the run goes on with its floor in memory.
#[test]
fn a_write_that_cannot_land_is_said_once_and_costs_nothing_else() {
    let dir = scratch();
    let in_the_way = dir.join("not-a-directory");
    std::fs::write(&in_the_way, b"x").expect("a plain file");
    let file = FloorFile::new(&in_the_way, "acct");
    assert!(!file.warned.load(Ordering::Relaxed));
    file.write(1);
    assert!(file.warned.load(Ordering::Relaxed), "said");
    file.write(2);
    assert_eq!(file.read(), None);
}
