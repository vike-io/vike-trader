//! `write_atomic`'s policy, pinned: the bytes land verbatim, an existing file is replaced, no
//! temporary file outlives a call that returned, a crash's leftover never blocks the next write, and
//! two writers in one process never collide.
//!
//! ⚠ NOT tested, because no honest test can: "the destination is never absent during a write". The
//! window is between two syscalls inside one call, and a reader thread racing it proves nothing when
//! it wins. The property is carried by the code shape instead — nothing in `write_atomic` removes
//! the destination; only `rename` touches it.

use super::*;

/// Every entry of `dir`, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_new_file_holds_exactly_the_bytes_and_nothing_is_left_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.json");
    write_atomic(&p, b"{\n  \"k\": 1\n}\n").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"{\n  \"k\": 1\n}\n");
    assert_eq!(listing(dir.path()), vec!["state.json".to_string()], "no temporary file left");
}

#[test]
fn an_existing_file_is_replaced_whole() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.json");
    std::fs::write(&p, b"the old, LONGER contents of this file").unwrap();
    write_atomic(&p, b"new").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"new", "replaced, not overwritten in place");
    write_atomic(&p, b"").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"", "an empty payload is a valid file");
    assert_eq!(listing(dir.path()), vec!["state.json".to_string()]);
}

/// A crash between the temp write and the rename leaves a temporary file behind — at the fixed name
/// every copy this replaced used (`<name>.tmp`, `<stem>.json.tmp`), and at this module's own
/// spelling. Neither may break the next write, and the next write leaves the leftovers alone (it
/// does not sweep; see the module doc).
#[test]
fn a_leftover_temporary_file_does_not_break_the_next_write() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("m.json");
    let old_spelling = dir.path().join("m.json.tmp");
    let own_spelling = dir.path().join(format!("m.json.{}.0.tmp", std::process::id()));
    std::fs::write(&old_spelling, b"torn").unwrap();
    std::fs::write(&own_spelling, b"torn").unwrap();
    write_atomic(&p, b"whole").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"whole");
    let left = listing(dir.path());
    assert!(left.contains(&"m.json".to_string()), "{left:?}");
    assert!(left.len() <= 3, "the call added no temporary file of its own: {left:?}");
}

#[test]
fn a_missing_parent_is_not_found_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("absent").join("m.json");
    let e = write_atomic(&p, b"x").unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
    assert!(listing(dir.path()).is_empty(), "the parent is the caller's to create");
}

#[test]
fn a_path_with_no_file_name_is_refused() {
    for p in ["", "..", "/"] {
        let e = write_atomic(Path::new(p), b"x").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{p:?}: {e}");
    }
}

/// A failed rename (here: the destination is a non-empty directory, which `rename` refuses on every
/// platform) returns the error, leaves the destination exactly as it was and removes the temporary
/// file.
#[test]
fn a_refused_rename_leaves_the_destination_and_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("taken");
    std::fs::create_dir(&p).unwrap();
    std::fs::write(p.join("inside"), b"kept").unwrap();
    write_atomic(&p, b"x").unwrap_err();
    assert_eq!(std::fs::read(p.join("inside")).unwrap(), b"kept");
    assert_eq!(listing(dir.path()), vec!["taken".to_string()], "the temporary file was removed");
}

/// Two writers in one process at once: both succeed, and the file is one of the two payloads
/// whole — never a mix, never a torn one. Repeated so the two calls genuinely overlap.
#[test]
fn concurrent_writers_both_succeed_and_one_payload_wins_whole() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("shared.json");
    let a = vec![b'a'; 64 * 1024];
    let b = vec![b'b'; 32 * 1024];
    for _ in 0..50 {
        std::thread::scope(|s| {
            let ha = s.spawn(|| write_atomic(&p, &a));
            let hb = s.spawn(|| write_atomic(&p, &b));
            ha.join().unwrap().unwrap();
            hb.join().unwrap().unwrap();
        });
        let got = std::fs::read(&p).unwrap();
        assert!(got == a || got == b, "a torn or mixed file: {} bytes", got.len());
    }
    assert_eq!(listing(dir.path()), vec!["shared.json".to_string()]);
}

/// The temporary name: beside the destination, carrying its file name, the pid and a fresh number
/// per call, ending in `.tmp`.
#[test]
fn temporary_names_are_siblings_and_unique_per_call() {
    let dir = Path::new("some").join("dir");
    let one = temp_path(&dir, std::ffi::OsStr::new("m.json"));
    let two = temp_path(&dir, std::ffi::OsStr::new("m.json"));
    assert_ne!(one, two);
    for t in [&one, &two] {
        assert_eq!(t.parent(), Some(dir.as_path()));
        let name = t.file_name().unwrap().to_str().unwrap();
        let prefix = format!("m.json.{}.", std::process::id());
        assert!(name.starts_with(&prefix) && name.ends_with(".tmp"), "{name}");
    }
}
