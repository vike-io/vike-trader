use super::*;

/// Two records in the SAME millisecond are ordered by `seq`, which is what that field is for.
#[test]
fn two_records_in_one_millisecond_are_ordered_by_seq() {
    let r = root();
    let j = journal(r.path());
    let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, "1");
    let path = j.append(T, &c).expect("append");
    j.append(T, &c).expect("append");
    j.append(T, &c).expect("append");

    let lines = read_lines(&path);
    assert_eq!(lines.len(), 3, "three appends, three lines");
    let seqs: Vec<u64> = lines.iter().map(|v| v["seq"].as_u64().unwrap()).collect();
    assert!(seqs[0] < seqs[1] && seqs[1] < seqs[2], "strictly increasing: {seqs:?}");
    assert!(lines.iter().all(|v| v["ts_ms"] == T), "…within one millisecond");
}

/// Appends ACCUMULATE. A journal that truncated would look identical after one write, which is
/// exactly how an append-only store stops being one without anyone noticing.
#[test]
fn appends_accumulate_rather_than_replace() {
    let r = root();
    let j = journal(r.path());
    for i in 0..25 {
        let c = Change::set_setting(
            Outcome::Applied,
            Actor::Gui,
            "policy.toml",
            "policy.max_notional_per_order",
            None,
            &i.to_string(),
        );
        j.append(T, &c).expect("append");
    }
    let lines = read_lines(&j.file_for(T));
    assert_eq!(lines.len(), 25);
    assert_eq!(lines[0]["target"]["new"], "0", "the FIRST record still exists");
    assert_eq!(lines[24]["target"]["new"], "24");
}

/// Concurrent appenders both land, and every line stays whole — the multi-writer requirement
/// that ruled out an embedded database with an exclusive lock.
///
/// Threads rather than processes here (a test binary cannot portably re-exec itself); the
/// cross-PROCESS half is `crates/vike-model/tests/change_journal_concurrent.rs`, which spawns
/// real child processes. This one exists because it is the cheap version that runs everywhere.
#[test]
fn concurrent_appenders_do_not_interleave() {
    let r = root();
    let dir = r.path().to_path_buf();
    let writers = 8;
    let each = 40;
    let handles: Vec<_> = (0..writers)
        .map(|w| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let j = ChangeJournal::new(dir, Proc::new("vike-test", w as u32, "0.1.0"));
                for i in 0..each {
                    let c = Change::set_setting(
                        Outcome::Applied,
                        Actor::Gui,
                        "policy.toml",
                        "policy.max_notional_per_order",
                        None,
                        &format!("{w}-{i}"),
                    );
                    j.append(T, &c).expect("append");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("writer");
    }

    let lines = read_lines(&journal(r.path()).file_for(T));
    assert_eq!(lines.len(), writers * each, "every append landed");
    let mut seen: Vec<String> =
        lines.iter().map(|v| v["target"]["new"].as_str().unwrap().to_string()).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), writers * each, "…and no two records were merged or lost");
}

/// ⚠ **THE APPEND PATH TAKES THE LOCK — and a contended append WAITS rather than losing its
/// record.** This is the machine-checked half of the module doc's serialisation argument.
///
/// What it can and cannot prove is worth stating plainly, because the two are easy to conflate.
/// It PROVES that `append` blocks on `<dir>/`[`CHANGES_LOCK_FILE`] and completes once that lock
/// is free — remove the lock and the writer finishes immediately, so this test goes red. It does
/// NOT prove the Docker Desktop case: reproducing lost appends needs a filesystem that loses
/// them, no CI box has one **because no CI box is Windows or macOS**, and that half rests
/// on the hand measurement recorded in the module doc and in
/// `docs/ops/tradehub-container.md`.
/// ⚠ The reason used to be given as "there is no container runtime on any of them", which is
/// false — both self-hosted boxes run Docker (measured 2026-09-25; the CI box built and published
/// every release image). A Linux runner's runtime cannot produce a Docker Desktop HOST
/// PASSTHROUGH mount, so the conclusion is unchanged and only its reason moves.
///
/// The lock is held from an INDEPENDENT descriptor in this same process, which works because
/// `flock`/`LockFileEx` key on the open file description rather than on the process — see
/// [`AppendLock`].
#[test]
fn an_append_waits_for_a_held_lock_instead_of_writing_beside_it() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    let r = root();
    let dir = r.path().to_path_buf();
    std::fs::create_dir_all(&dir).expect("journal dir");

    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(CHANGES_LOCK_FILE))
        .expect("open the sentinel");
    held.try_lock().expect("this test takes the append lock first");

    let started = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let (s, d, wdir) = (started.clone(), done.clone(), dir.clone());
    let writer = std::thread::spawn(move || {
        let j = ChangeJournal::new(wdir, Proc::new("vike-test", 1, "0.1.0"));
        let c = Change::set_setting(
            Outcome::Applied,
            Actor::Gui,
            "policy.toml",
            "policy.max_notional_per_order",
            None,
            "250",
        );
        s.store(true, Ordering::SeqCst);
        j.append(T, &c).expect("the append completes once the lock is released");
        d.store(true, Ordering::SeqCst);
    });

    // ⚠ ANTI-VACUITY, half one: wait until the writer has genuinely REACHED the append. A
    // "still running" assertion against a thread that has not started yet measures nothing —
    // it is the shape of contention test that never contends.
    while !started.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(1));
    }
    // …then a window in which it must NOT get through. Without the lock this append is a
    // create + one write + an fsync, so half a second is orders of magnitude of slack.
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(10));
        assert!(
            !done.load(Ordering::SeqCst),
            "the append completed while another descriptor held {CHANGES_LOCK_FILE} — the \
                 append path is NOT taking the lock"
        );
    }
    assert!(
        !dir.join(month_file_name(T)).exists(),
        "a blocked append must not have created the month file either"
    );

    // ⚠ ANTI-VACUITY, half two: release, and the SAME writer finishes. Without this the test
    // would pass just as well against a writer that had crashed, hung or never spawned.
    held.unlock().expect("release");
    drop(held);
    writer.join().expect("the writer finishes once the lock is free");
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(read_lines(&journal(r.path()).file_for(T)).len(), 1, "…and its record landed");
}

/// A lock that cannot be TAKEN AT ALL refuses the record loudly, and writes nothing — the
/// module doc's second half. A filesystem this journal cannot serialise on is one whose
/// completeness nobody can claim, so the caller gets an error it can log rather than a silence
/// it cannot.
///
/// Driven by putting a DIRECTORY where the sentinel belongs: `OpenOptions::open` cannot hand
/// back a file handle for one on either platform this ships on, which is a portable way to
/// reach a branch whose real-world cause (a filesystem whose locking errors) no box here has.
#[test]
fn a_lock_that_cannot_be_taken_refuses_the_record_and_writes_nothing() {
    let r = root();
    let dir = r.path().to_path_buf();
    std::fs::create_dir_all(dir.join(CHANGES_LOCK_FILE)).expect("a directory in the way");
    let j = journal(&dir);
    let c = Change::set_setting(Outcome::Applied, Actor::Gui, "policy.toml", "policy.k", None, "1");
    match j.append(T, &c) {
        Err(ChangeJournalError::Lock(e)) => {
            let msg = ChangeJournalError::Lock(e).to_string();
            assert!(msg.contains(CHANGES_LOCK_FILE), "the error names the sentinel: {msg}");
            assert!(msg.contains("REFUSED"), "…and says the record was not written: {msg}");
        }
        other => panic!("an unusable lock must refuse the record, got {other:?}"),
    }
    assert!(!j.file_for(T).exists(), "nothing was written unserialised");
}
