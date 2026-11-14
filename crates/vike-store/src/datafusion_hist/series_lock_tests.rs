use super::*;

/// The property that makes this a LOCK: while one holder has it, nobody else gets it. `flock` is
/// per open-file-description, so two `acquire`s inside one process contend exactly as two
/// processes do — which is what lets the property be tested without spawning one.
#[test]
fn a_second_acquire_is_excluded_until_the_holder_drops() {
    let dir = tempfile::tempdir().unwrap();
    let held = SeriesLock::acquire(dir.path()).expect("first acquire");

    // `let Err(..) else` rather than `expect_err`: that would need `SeriesLock: Debug`, and a
    // lock guard has no business growing a public trait impl to satisfy a test.
    let Err(err) = SeriesLock::acquire(dir.path()) else {
        panic!("two holders had the same series lock at once");
    };
    assert!(
        format!("{err:?}").contains("timeout acquiring series lock"),
        "wrong failure while contended: {err:?}"
    );

    drop(held);
    SeriesLock::acquire(dir.path()).expect("lock must be free once its holder drops");
}

/// A lock file with no live holder — what a SIGKILL'd writer leaves — must be takeable at once.
/// Under the old existence-is-the-lock scheme this spun 4 s and then failed, forever.
///
/// "At once" is asserted as **one `try_lock` attempt**, not as a wall clock. The clock this
/// test used to read (`elapsed() < 1 s`) spans `create_dir_all` + `open` as well, and those are
/// filesystem metadata syscalls on the shared system temp dir — bounded by the box, not by the
/// lock. It failed 2 of 248 runs in #1125's soak for exactly that reason. Reproduced on the CI box
/// under CPU starvation, the old assertion failed with `spun for 36.9s` and `spun for 76.2s`
/// — and the phase timing of those very runs puts 36.900997766 s of the 36.9012802 s inside
/// `create_dir_all`, with ONE `try_lock` attempt. It never spun at all; its own failure message
/// was wrong about what it had measured. Attempts are what the #1024 fix changed, so attempts
/// are what this asserts — and it is the STRICTER bound: a 999 ms spin of ~500 attempts passed
/// the old assertion and fails this one.
#[test]
fn a_leftover_lock_file_with_no_holder_is_taken_immediately() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(MANIFEST_LOCK), b"").unwrap();

    let (_held, attempts) = SeriesLock::acquire_within(dir.path(), SPIN_ATTEMPTS)
        .expect("a dead writer's file must not wedge us");
    assert_eq!(attempts, 1, "a holderless lock file cost {attempts} attempts, not 1");
}

/// The negative control for the test above: `attempts == 1` only carries information because a
/// loop that genuinely iterates can report more than 1. An `acquire_within` that returned on
/// its first pass whatever the lock's state, or one whose spin never slept, would satisfy the
/// leftover-file test and be worthless. Here the holder never lets go, so the acquire must
/// spend its WHOLE budget, and both halves of that are asserted: the error names the budget,
/// and the call takes at least the sleeping that many attempts implies.
///
/// The elapsed assertion is a LOWER bound, deliberately — that is the difference between this
/// and the bound it replaces. Load can only push a lower bound further into the passing side,
/// where an upper bound is exactly what a loaded box breaks. Budget 8 rather than
/// [`SPIN_ATTEMPTS`] keeps the control at ~16 ms instead of four seconds; a cheap control is
/// one that keeps getting run.
#[test]
fn a_lock_with_a_live_holder_spends_every_attempt_in_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let _held = SeriesLock::acquire(dir.path()).expect("first acquire");

    let t0 = std::time::Instant::now();
    let Err(err) = SeriesLock::acquire_within(dir.path(), 8) else {
        panic!("a live holder must not yield the series lock");
    };
    let spent = t0.elapsed();
    assert!(
        format!("{err:?}").contains("after 8 attempts"),
        "a contended acquire must report the budget it spent: {err:?}"
    );
    // 8 attempts sleep 2 ms each; `thread::sleep` sleeps AT LEAST that long, so 7 completed
    // sleeps is a floor no scheduler can undercut.
    assert!(spent >= Duration::from_millis(14), "8 spin attempts cannot take only {spent:?}");
}
