//! **Two PROCESSES appending to one journal file both land, and every line stays whole.**
//!
//! This is the requirement that decided the store. SQLite and redb were both considered and
//! rejected for the change journal, and multi-process append is why: the daemon and the GUI are
//! separate processes writing concurrently, and an embedded database holding an exclusive write
//! lock turns that into one writer plus a queue of failures — for a record whose whole job is to
//! exist when something went wrong. A claim like that is worth exactly as much as its test.
//!
//! # Why real processes, when the in-crate suite already spawns threads
//!
//! `vike_model::change_journal`'s own `concurrent_appenders_do_not_interleave` uses threads, and at
//! the kernel level that is nearly the same experiment — each `append` performs its own `open`, so
//! N threads produce N independent open file descriptions with `O_APPEND` exactly as N processes
//! would. **Nearly** is the problem. A thread test cannot distinguish "the kernel serializes
//! `O_APPEND` writes across file descriptions" from "this process happened to serialize them",
//! and it would keep passing if the implementation grew a process-local mutex, a shared handle or
//! any other coordination that silently makes the multi-process claim false. So this file spawns
//! real children and asserts, on an independent fact, that they really were separate processes:
//! several distinct pids appear in the file.
//!
//! # How a child is spawned without an environment variable
//!
//! The parent re-executes THIS TEST BINARY (`current_exe`) with `--ignored --exact` naming
//! [`child_appends`], and hands it the target directory as its **working directory**. No
//! environment variable is involved, deliberately: an `env::var` read — even in a test — needs a
//! row in `vike_ops::settings::SETTINGS` (`crates/vike-ops/tests/settings_registry.rs` walks
//! `tests/` too), and a whole registry entry to pass one path to a child is a poor trade.
//!
//! ⚠ [`child_appends`] is `#[ignore]`d, so an ordinary `cargo test` / `cargo nextest run` never
//! runs it. Its own guard keys on the WORKING DIRECTORY's name — an independent precondition, never
//! on the thing under test — so a stray `cargo test -- --ignored` in a checkout writes nothing into
//! the repository. And the guard cannot hide a failure: the parent asserts the children actually
//! wrote, so a guard that misfired reddens [`concurrent_processes_both_append_and_no_line_tears`]
//! rather than letting it pass on an empty file.

//! # What this file proves about the LOCK, and what it cannot
//!
//! Every append is serialised behind an exclusive advisory lock on
//! `<dir>/`[`vike_model::change_journal::CHANGES_LOCK_FILE`], because a host-passthrough filesystem
//! does not preserve `O_APPEND` atomicity and loses concurrent records silently — the measurement is
//! in `crates/vike-model/src/change_journal.rs`'s module doc and in `docs/ops/tradehub-container.md`.
//!
//! ⚠ **Reproducing that LOSS needs a filesystem that loses appends, and no box here has one** (there
//! is no container runtime on any CI runner — measured). So the Docker Desktop half rests on a hand
//! measurement, and this file proves the half that IS machine-checkable:
//! [`a_held_lock_makes_another_process_wait_rather_than_dropping_its_record`] shows that a SECOND
//! PROCESS's append genuinely blocks on the lock and completes when it is released, which is the
//! property the fix consists of. Read the two claims separately; the suite passing is not evidence
//! about Docker.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc, CHANGES_LOCK_FILE};
use vike_model::scratch::ScratchDir;

/// The directory-name marker the child checks before writing anything. Also the `ScratchDir` tag,
/// so the two cannot drift apart.
const CHILD_DIR_TAG: &str = "vike-cj-children";

/// 2026-08-21T00:00:00Z — one fixed instant, so parent and child agree on the month file without
/// passing anything.
const T: i64 = 1_787_356_800_000;

/// How many children, and how many records each writes.
const CHILDREN: usize = 4;
const PER_CHILD: usize = 50;

/// The month file every process in this test appends to.
fn journal_file(dir: &Path) -> std::path::PathBuf {
    ChangeJournal::new(dir.to_path_buf(), Proc::new("t", 0, "0")).file_for(T)
}

/// One record: distinguishable by the writing process's pid and its own index.
fn change(pid: u32, i: usize) -> Change {
    Change::set_setting(
        Outcome::Applied,
        Actor::Gui,
        "policy.toml",
        "policy.max_notional_per_order",
        None,
        &format!("{pid}-{i}"),
    )
}

/// **THE TEST.** Four processes append to one file at once; every record lands, every line parses,
/// and no two records were merged, torn or lost.
#[test]
fn concurrent_processes_both_append_and_no_line_tears() {
    let dir = ScratchDir::create_in(&std::env::temp_dir(), CHILD_DIR_TAG).expect("child dir");
    let exe = std::env::current_exe().expect("this test binary");

    let children: Vec<_> = (0..CHILDREN)
        .map(|_| {
            Command::new(&exe)
                .args(["--ignored", "--exact", "child_appends", "--test-threads", "1"])
                .current_dir(dir.path())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn a child appender")
        })
        .collect();

    for (n, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().expect("wait for a child appender");
        assert!(
            out.status.success(),
            "child {n} failed ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    let path = journal_file(dir.path());
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "no journal at {} ({e}). The children ran and exited 0, so either the working-directory \
             guard in `child_appends` misfired or nothing was appended",
            path.display()
        )
    });

    // Every line is a whole, parseable record — the property a torn append destroys.
    let lines: Vec<serde_json::Value> = raw
        .lines()
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("line {i} is not one whole JSON record ({e}): {l:?}"))
        })
        .collect();
    assert_eq!(
        lines.len(),
        CHILDREN * PER_CHILD,
        "every append from every process must land — got {} of {}",
        lines.len(),
        CHILDREN * PER_CHILD
    );
    assert!(raw.ends_with('\n'), "the file ends on a record boundary");

    // …and nothing was merged or duplicated: each record's payload is unique by construction.
    let mut payloads: Vec<&str> =
        lines.iter().map(|v| v["target"]["new"].as_str().expect("a new value")).collect();
    payloads.sort_unstable();
    payloads.dedup();
    assert_eq!(payloads.len(), CHILDREN * PER_CHILD, "no two records were merged or lost");

    // THE independent fact that makes this a multi-PROCESS test rather than a second thread test:
    // the writers really were distinct processes. Without it the whole file could pass with the
    // children never having spawned at all.
    let mut pids: Vec<u64> =
        lines.iter().map(|v| v["proc"]["pid"].as_u64().expect("a pid")).collect();
    pids.sort_unstable();
    pids.dedup();
    assert_eq!(
        pids.len(),
        CHILDREN,
        "expected {CHILDREN} distinct writer pids in the file, saw {pids:?} — the children are not \
         separate processes, so this proves nothing about multi-process append"
    );
    let parent = u64::from(std::process::id());
    assert!(!pids.contains(&parent), "the parent wrote nothing itself: {pids:?}");
}

/// The marker a locked-out child writes the instant BEFORE it calls `append`, and the one it writes
/// after. Two files rather than one because the pair is what separates "blocked on the lock" from
/// "has not got there yet" — the vacuous pass this test would otherwise be.
const REACHED_MARKER: &str = "reached-append";
const APPENDED_MARKER: &str = "appended";

/// How long the parent will wait for a spawned child to reach its append. Generous: it covers
/// process spawn plus the test harness's own startup on a loaded box, and it is a TIMEOUT rather
/// than a sleep, so a fast box pays nothing.
const REACH_TIMEOUT: Duration = Duration::from_secs(60);

/// The window in which a locked-out child must NOT complete. Without the lock its append is a
/// create, one `write` and an `fsync`.
const BLOCKED_WINDOW: Duration = Duration::from_millis(750);

/// **THE LOCK, ACROSS PROCESSES.** A record is never dropped because another process holds the
/// append lock — it WAITS, and lands when the lock frees.
///
/// This is the machine-checked half of the serialisation fix (see this file's module doc for the
/// half that is not). The parent takes the lock the way `AppendLock` does, from a plain descriptor
/// on the sentinel, so nothing about the assertion depends on re-implementing the journal.
///
/// Both anti-vacuity halves are load-bearing:
///   * the child announces it has REACHED the append before making it, so "still running" cannot
///     pass against a child that had not started;
///   * the lock is then released and the child must finish and its record must be in the file, so
///     the test cannot pass against a child that crashed, hung or never spawned.
#[test]
fn a_held_lock_makes_another_process_wait_rather_than_dropping_its_record() {
    let dir = ScratchDir::create_in(&std::env::temp_dir(), CHILD_DIR_TAG).expect("child dir");
    let exe = std::env::current_exe().expect("this test binary");

    // Hold the sentinel exactly as the journal's own guard opens it: read+write (Windows refuses to
    // lock an append-opened handle), create-if-absent, never truncate.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join(CHANGES_LOCK_FILE))
        .expect("open the append-lock sentinel");
    held.try_lock().expect("the parent takes the append lock first");

    let mut child = Command::new(&exe)
        .args(["--ignored", "--exact", "child_appends_once", "--test-threads", "1"])
        .current_dir(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the locked-out child");

    let reached = dir.path().join(REACHED_MARKER);
    let appended = dir.path().join(APPENDED_MARKER);
    let deadline = Instant::now() + REACH_TIMEOUT;
    while !reached.exists() {
        assert!(
            Instant::now() < deadline,
            "the child never reached its append within {REACH_TIMEOUT:?} — it did not run, so this \
             test would prove nothing about the lock"
        );
        assert!(
            child.try_wait().expect("poll the child").is_none(),
            "the child exited before reaching its append"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let until = Instant::now() + BLOCKED_WINDOW;
    while Instant::now() < until {
        assert!(
            !appended.exists(),
            "the child's append COMPLETED while this process held {CHANGES_LOCK_FILE} — the append \
             path is not taking the lock, so concurrent writers are not serialised"
        );
        assert!(
            child.try_wait().expect("poll the child").is_none(),
            "the child exited while the lock was held — a blocked append must WAIT, never fail or \
             drop its record"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    held.unlock().expect("release the append lock");
    drop(held);

    let out = child.wait_with_output().expect("wait for the locked-out child");
    assert!(
        out.status.success(),
        "the child must SUCCEED once the lock frees ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(appended.exists(), "the child got through only after the release");

    let raw = std::fs::read_to_string(journal_file(dir.path())).expect("the journal file");
    let lines: Vec<serde_json::Value> =
        raw.lines().map(|l| serde_json::from_str(l).expect("one whole record")).collect();
    assert_eq!(lines.len(), 1, "the waiting record landed, exactly once: {raw:?}");
    assert_ne!(
        lines[0]["proc"]["pid"].as_u64().expect("a pid"),
        u64::from(std::process::id()),
        "the record was written by the CHILD process, not by this one"
    );
}

/// The waiting child — **never run by an ordinary test invocation**, same working-directory guard
/// as [`child_appends`].
#[test]
#[ignore = "spawned by a_held_lock_makes_another_process_wait_rather_than_dropping_its_record"]
fn child_appends_once() {
    let cwd = std::env::current_dir().expect("a working directory");
    if !is_spawned_child(&cwd) {
        eprintln!(
            "not a spawned child (cwd {}) — writing nothing. This test is driven by \
             `a_held_lock_makes_another_process_wait_rather_than_dropping_its_record`.",
            cwd.display()
        );
        return;
    }

    let journal = ChangeJournal::new(cwd.clone(), Proc::current("change-journal-test"));
    let c = change(std::process::id(), 0);
    // ⚠ The marker goes down BEFORE the append and is what the parent waits on. Written first so
    // that "the child is still running" can only mean "blocked in `append`".
    std::fs::write(cwd.join(REACHED_MARKER), b"reached\n").expect("marker");
    journal.append(T, &c).expect("a blocked append must WAIT and then succeed, never fail");
    std::fs::write(cwd.join(APPENDED_MARKER), b"appended\n").expect("marker");
}

/// Is this process one the parent spawned? Keyed on the WORKING DIRECTORY's name — an independent
/// precondition, never on the thing under test — so a stray `cargo test -- --ignored` in a checkout
/// writes nothing.
fn is_spawned_child(cwd: &Path) -> bool {
    cwd.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(&format!("{CHILD_DIR_TAG}-")))
}

/// The child half — **never run by an ordinary test invocation.**
///
/// `#[ignore]`d, and additionally guarded on the WORKING DIRECTORY's name, which is an independent
/// precondition rather than anything about the journal. A `cargo test -- --ignored` in a checkout
/// therefore writes nothing: the guard sees a working directory that is not a spawned child's and
/// returns. That cannot mask a real failure, because the parent asserts the file it expects.
#[test]
#[ignore = "spawned by concurrent_processes_both_append_and_no_line_tears; writes nothing otherwise"]
fn child_appends() {
    let cwd = std::env::current_dir().expect("a working directory");
    if !is_spawned_child(&cwd) {
        eprintln!(
            "not a spawned child (cwd {}) — writing nothing. This test is driven by \
             `concurrent_processes_both_append_and_no_line_tears`.",
            cwd.display()
        );
        return;
    }

    let pid = std::process::id();
    let journal = ChangeJournal::new(cwd, Proc::current("change-journal-test"));
    for i in 0..PER_CHILD {
        journal.append(T, &change(pid, i)).expect("a child append must succeed");
    }
}
