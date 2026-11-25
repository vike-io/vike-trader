//! `set_aside`'s promises, pinned: the file moves beside itself under its dated name with its bytes,
//! a second set-aside on the same date never replaces the first, a directory moves too, and a
//! missing file is an error that creates nothing.

use super::*;
use std::time::{Duration, SystemTime};

/// 2026-11-23 10:15:00 UTC.
const MODIFIED: u64 = 1_795_428_900;

fn plant(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(MODIFIED)).unwrap();
}

#[test]
fn a_file_moves_beside_itself_under_its_dated_name_and_keeps_its_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.msgpack");
    plant(&path, b"\xc1 not one value");
    let aside = set_aside(&path).unwrap();
    assert_eq!(aside, dir.path().join("state.msgpack.unreadable-20261123T101500Z"));
    assert!(!path.exists(), "nothing is left at the old name for a save to write over");
    assert_eq!(std::fs::read(&aside).unwrap(), b"\xc1 not one value");
}

#[test]
fn a_second_set_aside_on_the_same_date_takes_the_next_free_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sends");
    plant(&path, b"first");
    let first = set_aside(&path).unwrap();
    plant(&path, b"second");
    let second = set_aside(&path).unwrap();
    assert_eq!(second, dir.path().join("sends.unreadable-20261123T101500Z-2"));
    assert_eq!(std::fs::read(&first).unwrap(), b"first", "the earlier copy is never replaced");
    assert_eq!(std::fs::read(&second).unwrap(), b"second");
}

#[test]
fn a_directory_at_the_path_is_set_aside_too() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime_mounts.json");
    std::fs::create_dir(&path).unwrap();
    let aside = set_aside(&path).unwrap();
    assert!(aside.is_dir());
    assert!(!path.exists());
}

#[test]
fn nothing_at_the_path_is_not_found_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let error = set_aside(&dir.path().join("absent")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
