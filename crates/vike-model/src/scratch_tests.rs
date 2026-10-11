use super::*;

/// A throwaway scratch ROOT for one test, standing in for `<project>/tmp`.
///
/// Built with [`ScratchDir`] itself, which is dogfooding rather than cuteness: the root is then
/// unique per process, self-deleting on the panic path, and needs no `tempfile` dev-dependency
/// in the crate every binary in this workspace links. The system temp directory is legitimate
/// HERE — this is test code, `crates/vike-ops/tests/hygiene/system_temp_gate.rs` scopes itself to
/// production, and its sibling `temp_path_gate.rs` is satisfied because the name is not fixed.
fn root() -> ScratchDir {
    ScratchDir::create_in(&std::env::temp_dir(), "vike-scratch-selftest").expect("temp root")
}

/// The whole point: the directory and its contents are gone once the guard drops.
#[test]
fn drop_removes_the_directory_and_its_contents() {
    let r = root();
    let (dir, inner) = {
        let s = ScratchDir::create_in(r.path(), "export").expect("create");
        std::fs::write(s.join("stage.parquet"), b"a fully written export").expect("write");
        (s.path().to_path_buf(), s.join("stage.parquet"))
    };
    assert!(!inner.exists(), "the staged file is gone");
    assert!(!dir.exists(), "and so is the directory holding it");
    assert!(r.path().exists(), "…but the scratch ROOT is left for the next caller");
}

/// Unwinding runs destructors, so a caller that PANICS still cleans up. This is the property
/// that makes the leak unable to recur on the failure path — where it is least likely to be
/// noticed and therefore most likely to accumulate.
#[test]
fn a_panicking_caller_still_cleans_up() {
    let r = root();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(PathBuf::new()));
    let sink = std::sync::Arc::clone(&seen);
    let at = r.path().to_path_buf();

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {})); // keep the deliberate panic off the test log
    let outcome = std::panic::catch_unwind(move || {
        let s = ScratchDir::create_in(&at, "export").expect("create");
        std::fs::write(s.join("stage.parquet"), b"half an export").expect("write");
        *sink.lock().unwrap() = s.path().to_path_buf();
        panic!("the staging step fails here");
    });
    std::panic::set_hook(hook);

    assert!(outcome.is_err(), "the observed closure really did panic");
    let path = seen.lock().unwrap().clone();
    assert!(path.components().count() > 1, "the closure ran far enough to allocate");
    assert!(!path.exists(), "unwinding dropped the guard and removed the directory");
}

/// Two allocations never share a path, so one process staging two exports concurrently cannot
/// have them overwrite each other.
#[test]
fn two_allocations_are_two_directories() {
    let r = root();
    let a = ScratchDir::create_in(r.path(), "export").expect("create");
    let b = ScratchDir::create_in(r.path(), "export").expect("create");
    assert_ne!(a.path(), b.path(), "the per-process counter separates them");
    assert!(a.exists() && b.exists(), "both exist independently");
}

/// The tag is in the name, so a directory observed mid-run names the tool that owns it — the
/// property that made `vike_ch_backtest_bridge_*` diagnosable on a shared box.
#[test]
fn the_path_carries_the_tag() {
    let r = root();
    let s = ScratchDir::create_in(r.path(), "pmxt").expect("create");
    let name = s.file_name().expect("a file name").to_string_lossy().into_owned();
    assert!(name.starts_with("pmxt-"), "got {name}");
}

/// The root is created on first use, because `tmp/` is not a marker and a fresh install has
/// none. Without this the first tool to run on a new deployment fails on a missing directory.
#[test]
fn the_scratch_root_is_created_on_first_use() {
    let r = root();
    let never_created = r.path().join("tmp");
    assert!(!never_created.exists(), "precondition");
    let s = ScratchDir::create_in(&never_created, "first").expect("create");
    assert!(s.exists() && never_created.exists());
}

/// A directory left by a previous run at the SAME path — pid reuse after an abort — is cleared
/// rather than inherited, so an aborted run cannot poison its successor with half-written files.
///
/// Driven through `create_at` at an explicit path rather than through `create_in`, because
/// predicting the next name means reading the shared [`SEQ`] and that races the rest of this
/// binary — a correctness pin turned into a flake. `create_in` reaches this by a one-line
/// delegation and adds only the NAME, which [`the_path_carries_the_tag`] and
/// [`two_allocations_are_two_directories`] cover between them.
#[test]
fn a_stale_directory_at_the_same_path_is_cleared() {
    let r = root();
    let path = r.path().join("reuse-1234-0");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("half.parquet"), b"junk from an aborted run").unwrap();

    let fresh = ScratchDir::create_at(&path).expect("create");
    assert_eq!(fresh.path(), path, "precondition: the same path was re-minted");
    assert!(!fresh.join("half.parquet").exists(), "the stale contents are gone");
    assert!(path.exists(), "…and the directory itself is back, empty");
}

/// `keep` is the opt-out, and it must actually opt out — a guard that still deleted would make
/// the escape hatch a trap.
#[test]
fn keep_gives_up_ownership() {
    let r = root();
    let kept = {
        let s = ScratchDir::create_in(r.path(), "output").expect("create");
        std::fs::write(s.join("result.json"), b"{}").expect("write");
        s.keep()
    };
    assert!(kept.join("result.json").exists(), "nothing removed the directory");
}

/// The sweep's contract: newest `max_entries` survive, oldest go first.
#[test]
fn sweep_keeps_the_newest_and_removes_the_oldest() {
    let r = root();
    let mut made = Vec::new();
    for i in 0..5 {
        let d = r.path().join(format!("entry-{i}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("payload"), b"x").unwrap();
        // Stamp mtimes explicitly: creating five directories in a millisecond leaves the
        // ordering to filesystem timestamp resolution, which is how this test would otherwise
        // pass or fail depending on the disk it ran on.
        let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i);
        filetime_set(&d, at);
        made.push(d);
    }

    let swept = sweep(r.path(), Some(2));
    assert_eq!(swept.found, 5, "all five were seen");
    assert_eq!(swept.removed, 3, "three oldest removed: {swept:?}");
    assert_eq!(swept.failed, 0);
    assert!(!made[0].exists() && !made[1].exists() && !made[2].exists(), "oldest three gone");
    assert!(made[3].exists() && made[4].exists(), "newest two kept");
    assert!(r.path().exists(), "the root itself is never removed");
}

/// Under the limit, the sweep removes NOTHING — the anti-vacuity twin of the test above. A
/// sweep that removed on every call would delete a concurrent sibling's working directory.
#[test]
fn sweep_below_the_limit_removes_nothing() {
    let r = root();
    for i in 0..3 {
        std::fs::create_dir_all(r.path().join(format!("entry-{i}"))).unwrap();
    }
    let swept = sweep(r.path(), Some(DEFAULT_MAX_SCRATCH_ENTRIES));
    assert_eq!(swept, Swept { found: 3, removed: 0, failed: 0 });
    assert!(r.path().join("entry-0").exists());
}

/// `None` is retention OFF, the `vike_log::LogConfig::file_max_files` spelling. Asserted
/// against a population that WOULD be pruned under the default, so this cannot pass by the
/// limit simply not being reached.
#[test]
fn sweep_with_no_limit_keeps_everything() {
    let r = root();
    for i in 0..(DEFAULT_MAX_SCRATCH_ENTRIES + 4) {
        std::fs::create_dir_all(r.path().join(format!("entry-{i:03}"))).unwrap();
    }
    assert_eq!(sweep(r.path(), None), Swept::default(), "no limit means no work at all");
    assert_eq!(
        std::fs::read_dir(r.path()).unwrap().count(),
        DEFAULT_MAX_SCRATCH_ENTRIES + 4,
        "…and nothing was removed"
    );
    // …and the same population under the DEFAULT limit really would have been pruned, so the
    // assertion above is about `None` rather than about a limit nobody reached.
    assert_eq!(sweep(r.path(), Some(DEFAULT_MAX_SCRATCH_ENTRIES)).removed, 4);
}

/// An absent root is the ordinary state of a fresh install, not an error — a startup must not
/// fail over housekeeping.
#[test]
fn sweep_of_an_absent_root_is_silent() {
    let r = root();
    assert_eq!(sweep(&r.path().join("never-created"), Some(1)), Swept::default());
}

/// A loose FILE in the scratch root is swept like a directory. Not hypothetical: the ClickHouse
/// export path this module replaces staged a bare `.parquet` file rather than a directory, so a
/// sweep that only understood directories would leave exactly the biggest entries behind.
#[test]
fn sweep_removes_loose_files_too() {
    let r = root();
    for i in 0..3 {
        let f = r.path().join(format!("stage-{i}.parquet"));
        std::fs::write(&f, b"an export").unwrap();
        let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i);
        filetime_set(&f, at);
    }
    let swept = sweep(r.path(), Some(1));
    assert_eq!((swept.found, swept.removed, swept.failed), (3, 2, 0));
    assert!(r.path().join("stage-2.parquet").exists(), "the newest file survives");
}

/// Set a path's modification time, so the sweep's ordering is driven by the test rather than by
/// the host filesystem's timestamp resolution.
///
/// Hand-rolled over `std::fs::File::set_times` rather than pulling the `filetime` crate: this is
/// the only caller, and `vike-model` adding a dependency for one test would be a dependency in
/// the crate every binary links.
fn filetime_set(path: &Path, at: std::time::SystemTime) {
    let times = std::fs::FileTimes::new().set_modified(at).set_accessed(at);
    let handle = if path.is_dir() {
        // A directory handle needs the "backup semantics" flag on Windows; on unix a plain
        // read-only open is enough.
        open_dir(path)
    } else {
        std::fs::OpenOptions::new().write(true).open(path)
    };
    handle.and_then(|f| f.set_times(times)).expect("stamp the mtime");
}

#[cfg(windows)]
fn open_dir(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(not(windows))]
fn open_dir(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}
