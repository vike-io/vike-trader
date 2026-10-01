use super::*;
use std::time::Instant;

/// Windows is a SUPPORTED graceful-stop platform now (split-plane B10): Ctrl-C must arm the
/// flag path rather than falling through to the OS default handler, which terminates the
/// process with ZERO Rust run — the incident shape the unix arm's SIGTERM fix closed.
/// (`ctrlc::set_handler` is once-per-process; this is deliberately the only test that
/// installs, and nextest's process-per-test isolation holds regardless.)
#[cfg(windows)]
#[test]
fn windows_install_is_installed_not_unsupported() {
    let flag = Arc::new(AtomicBool::new(false));
    let outcome = install_handlers(&flag);
    assert!(outcome.is_installed(), "got {outcome:?}");
}

#[test]
fn a_fresh_signal_is_not_requested_and_not_claimed() {
    let stop = StopSignal::new();
    assert!(!stop.is_requested(), "nothing has asked this process to stop yet");
    assert!(!stop.is_teardown_finished(), "…and nothing has finished one either");
    assert!(stop.begin_teardown(), "the first claim wins");
    assert!(
        !stop.is_teardown_finished(),
        "CLAIMING a teardown is not FINISHING it — a loser released by the claim alone would \
             exit the process while the winner was still running"
    );
}

/// The whole point of a FLAG rather than a channel: two triggers, one stop.
#[test]
fn a_second_request_is_a_no_op() {
    let stop = StopSignal::new();
    stop.request();
    assert!(stop.is_requested());
    stop.request();
    assert!(stop.is_requested(), "raising an already-raised flag changes nothing");
}

/// Every clone of [`StopSignal::flag`] is the same flag — the invariant that makes a signal
/// handler and a stdio thread two triggers of one stop rather than two stops.
#[test]
fn every_flag_handle_raises_the_same_stop() {
    let stop = StopSignal::new();
    let from_a_handler = stop.flag();
    let from_a_control_thread = stop.flag();
    assert!(!stop.is_requested());
    request_stop(&from_a_handler);
    assert!(stop.is_requested(), "the handler's flag is the daemon's flag");
    assert!(
        from_a_control_thread.load(Ordering::SeqCst),
        "…and so is the control thread's — one flag, many holders"
    );
}

/// ⚠ THE idempotence proof, under real concurrency: many threads request a stop and every one of
/// them then tries to claim the teardown. Exactly one may win. This is the scenario the design
/// note names — "the stdio stop word and a signal can arrive together" — with the arrival window
/// widened to every interleaving the scheduler will produce.
#[test]
fn concurrent_triggers_claim_the_teardown_exactly_once() {
    use std::sync::atomic::AtomicUsize;

    let stop = Arc::new(StopSignal::new());
    let wins = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let stop = Arc::clone(&stop);
            let wins = Arc::clone(&wins);
            std::thread::spawn(move || {
                stop.request();
                if stop.begin_teardown() {
                    wins.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("a trigger thread panicked");
    }

    assert!(stop.is_requested(), "16 requests still mean one stop");
    assert_eq!(
        wins.load(Ordering::SeqCst),
        1,
        "the teardown must be claimed EXACTLY once — a second claim is a second teardown, i.e. \
             a double cancel sweep, a double flush, and a join of an already-joined thread"
    );
    assert!(
        !stop.begin_teardown(),
        "…and it stays claimed afterwards: a late trigger must never re-arm the teardown"
    );
}

/// ⚠ **THE race the first cut of this module got wrong.** A loser of the claim must not carry
/// on, because in both daemons "carry on" means RETURN FROM `main`, and that terminates the
/// process — killing the winner's teardown at an arbitrary instruction. The claim would then
/// have converted "two teardowns" into "a truncated one", which is strictly worse.
///
/// `exited` stands in for that process exit: only a loser sets it, and the winner asserts at
/// every step of its teardown that it has not happened. Eight losers, so the scheduler gets many
/// chances to let one through.
///
/// MUTATION PROOF: replace the losers' `await_teardown` with a bare `return`/no-op and this goes
/// red on the winner's first step ("a losing claim exited the process while the winner was still
/// tearing down"), which is the defect wearing its real consequence.
#[test]
fn a_losing_claim_cannot_exit_while_the_winner_is_still_tearing_down() {
    use std::sync::atomic::AtomicUsize;

    let stop = Arc::new(StopSignal::new());
    // The stand-in for `return ExitCode::SUCCESS` out of a daemon's `main`.
    let exited = Arc::new(AtomicBool::new(false));
    let losers_seen = Arc::new(AtomicUsize::new(0));
    // Gate the losers behind the winner's claim so every one of them is a genuine loser and the
    // test cannot pass by having them all run before the race exists.
    let claimed = Arc::new(AtomicBool::new(false));

    let winner = {
        let stop = Arc::clone(&stop);
        let exited = Arc::clone(&exited);
        let claimed = Arc::clone(&claimed);
        std::thread::spawn(move || {
            stop.request();
            assert!(stop.begin_teardown(), "the first claimant wins");
            claimed.store(true, Ordering::SeqCst);
            // A teardown is a SEQUENCE — a cancel sweep, a flush, a join. Each step re-checks
            // that the process is still alive, which is the property under test.
            for step in 0..5 {
                std::thread::sleep(Duration::from_millis(20));
                assert!(
                    !exited.load(Ordering::SeqCst),
                    "a losing claim exited the process at teardown step {step} — the winner's \
                         sweep/flush would have been cut off mid-write"
                );
            }
            stop.finish_teardown();
        })
    };

    let losers: Vec<_> = (0..8)
        .map(|_| {
            let stop = Arc::clone(&stop);
            let exited = Arc::clone(&exited);
            let claimed = Arc::clone(&claimed);
            let losers_seen = Arc::clone(&losers_seen);
            std::thread::spawn(move || {
                while !claimed.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                stop.request(); // a second `systemctl stop`, a typed word, a second signal
                if stop.begin_teardown() {
                    panic!("the teardown was claimed twice");
                }
                losers_seen.fetch_add(1, Ordering::SeqCst);
                assert!(
                    stop.await_teardown(Duration::from_secs(30)),
                    "the loser must be released by the winner's finish, not by its own bound"
                );
                // …and ONLY now is exiting allowed.
                exited.store(true, Ordering::SeqCst);
            })
        })
        .collect();

    winner.join().expect("the winning teardown panicked");
    for l in losers {
        l.join().expect("a losing claimant panicked");
    }
    assert_eq!(losers_seen.load(Ordering::SeqCst), 8, "every loser must have reached the wait");
    assert!(stop.is_teardown_finished(), "the winner announced completion");
    assert!(exited.load(Ordering::SeqCst), "…and the losers were then released to exit");
}

/// The other half of the same contract: the wait is BOUNDED. A winner that wedges must not hold
/// the process open past the unit's `TimeoutStopSec=`, where SIGKILL is waiting regardless — so
/// an unreleased loser returns `false` (which its caller reports) rather than blocking forever.
#[test]
fn the_losers_wait_is_bounded_when_the_winner_never_finishes() {
    let stop = StopSignal::new();
    stop.request();
    assert!(stop.begin_teardown(), "somebody else owns the teardown");
    // …and then never finishes it.
    let started = Instant::now();
    let released = stop.await_teardown(Duration::from_millis(200));
    let waited = started.elapsed();

    assert!(!released, "an unfinished teardown must report the bound, not a completion");
    assert!(waited >= Duration::from_millis(150), "it must actually have waited: {waited:?}");
    assert!(waited < Duration::from_secs(10), "…and must not block forever: {waited:?}");
}

/// A wait that starts AFTER the winner already finished returns at once — the ordinary case
/// once a teardown is over, and the reason a loser cannot be parked by arriving late.
#[test]
fn awaiting_an_already_finished_teardown_returns_immediately() {
    let stop = StopSignal::new();
    assert!(stop.begin_teardown());
    stop.finish_teardown();
    let started = Instant::now();
    assert!(stop.await_teardown(Duration::from_secs(30)));
    assert!(started.elapsed() < Duration::from_secs(5), "a finished teardown must not park");
}

/// A stop that arrives WHILE the teardown runs must not start a second one. This is the ordering
/// the previous test cannot show: claim first, then keep requesting.
#[test]
fn a_request_during_teardown_does_not_re_arm_it() {
    let stop = StopSignal::new();
    stop.request();
    assert!(stop.begin_teardown(), "the stop path claims the teardown");
    stop.request(); // e.g. an impatient operator's second `systemctl stop`
    assert!(!stop.begin_teardown(), "a second stop request cannot buy a second teardown");
}

#[test]
fn wait_returns_once_another_thread_requests() {
    let stop = Arc::new(StopSignal::new());
    let raiser = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            stop.request();
        })
    };
    let started = Instant::now();
    stop.wait();
    assert!(stop.is_requested());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "wait must return on the request, not on a timeout"
    );
    raiser.join().expect("the raiser thread panicked");
}

/// A platform with NEITHER POSIX signals NOR a Windows console has nothing to install, and that
/// is an OUTCOME, not a failure — such a daemon must be able to say "the graceful stop is not
/// armed on this platform" rather than believing it is.
///
/// ⚠ **The gate is `not(any(unix, windows))`, and it used to be `not(unix)`.** B10 gave Windows
/// a real `install_handlers` arm and left this test's cfg alone, so on Windows the file carried
/// two tests asserting opposite things about the same call. It was not a latent contradiction:
/// `cargo test -p vike-ops` was RED on the dev box from the day B10 merged, and stayed red
/// because **no CI job runs a test on Windows at all** — and none compiles this crate's TEST
/// targets for that platform either. ⚠ Split-plane I12 does NOT close that: adding
/// vike-tradehub to `windows-cross` compiles this crate's LIB for Windows through the
/// dependency edge, and `--all-targets` applies to the NAMED packages only, so these tests stay
/// the dev box's job. What I12 buys is that the daemon's own `#[cfg(windows)]` code — including
/// the `#![cfg(windows)]` test binary that drives this module — is type-checked by CI instead of
/// by whoever remembers to run `just windows-check`. (Under plain `cargo test` the two tests
/// also raced for `ctrlc::set_handler`'s once-per-process registration, so the failure arrived
/// as `Failed("… already registered")` rather than as the `Installed` the arm returns; under
/// nextest's process isolation it is `Installed`. Both are the same bug.)
#[cfg(not(any(unix, windows)))]
#[test]
fn a_platform_with_no_stop_mechanism_reports_unsupported_rather_than_pretending() {
    let stop = StopSignal::new();
    assert_eq!(install_handlers(&stop.flag()), HandlerOutcome::Unsupported);
    assert!(!install_handlers(&stop.flag()).is_installed());
    assert!(!stop.is_requested(), "and installing nothing must not request a stop");
}

/// A scratch directory of this test's own, cleaned up by the caller.
#[cfg(windows)]
fn scratch(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("vike_stopfile_{tag}_{nanos}"));
    std::fs::create_dir_all(&dir).expect("create the scratch dir");
    dir
}

/// ⚠ THE Windows background-hosting stop, as a mechanism: a file appears, the flag comes up.
///
/// This is the whole of I13 at the primitive level. A daemon detached from a console cannot be
/// reached by B10's handler or by the stdio word, so without this its only stop is `taskkill`,
/// which runs no teardown at all.
#[cfg(windows)]
#[test]
fn creating_the_stop_file_raises_the_flag() {
    let dir = scratch("raises");
    let path = dir.join(STOP_FILE_NAME);
    let stop = StopSignal::new();

    let outcome = arm_stop_file(&path, &stop.flag());
    assert_eq!(outcome, StopFileOutcome::Armed(path.clone()), "the watcher must arm");
    assert!(!stop.is_requested(), "arming a watcher must not itself request a stop");

    std::fs::write(&path, "").expect("an operator creates the stop file");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !stop.is_requested() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        stop.is_requested(),
        "the stop file must raise the SAME flag a signal raises — it is a trigger, not a second \
             teardown"
    );
    assert!(stop.begin_teardown(), "…and the file-driven stop claims the teardown");
    let _ = std::fs::remove_dir_all(&dir);
}

/// ⚠ A STALE file must be REMOVED at arm, never obeyed. A daemon killed between the stop
/// request and its exit leaves one behind; obeying it would stop the next start within
/// [`STOP_POLL`] of coming up, and the one after that, and the one after that — a boot loop
/// whose cause is a file nobody remembers creating.
#[cfg(windows)]
#[test]
fn a_stale_stop_file_is_cleared_at_arm_instead_of_stopping_the_next_start() {
    let dir = scratch("stale");
    let path = dir.join(STOP_FILE_NAME);
    std::fs::write(&path, "").expect("the previous run's leftover");

    let stop = StopSignal::new();
    assert!(arm_stop_file(&path, &stop.flag()).is_armed());
    assert!(!path.exists(), "the stale file must be gone, not merely ignored");

    // Give the watcher several polls to get it wrong.
    std::thread::sleep(STOP_POLL * 4);
    assert!(
        !stop.is_requested(),
        "a leftover file from a previous run must NOT stop this one — that is a boot loop"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A path whose parent does not exist arms NOTHING and says so. Silently watching a file that
/// can never appear is the disarmed-kill-switch failure that cost this repo a release once, and
/// it looks identical to a working switch right up to the moment it is needed.
#[cfg(windows)]
#[test]
fn a_stop_file_under_a_missing_directory_is_a_loud_failure_not_a_silent_watcher() {
    let dir = scratch("missing");
    let path = dir.join("no-such-subdir").join(STOP_FILE_NAME);
    let stop = StopSignal::new();
    match arm_stop_file(&path, &stop.flag()) {
        StopFileOutcome::Failed(e) => {
            assert!(e.contains("no-such-subdir"), "the failure must name the path: {e}")
        }
        other => panic!("a missing parent must not arm a watcher, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The watcher ENDS when some OTHER trigger stops the daemon, so it costs nothing after a stop
/// and cannot hold a handle on the state directory the teardown is still writing into.
#[cfg(windows)]
#[test]
fn the_watcher_exits_when_another_trigger_raises_the_flag() {
    let dir = scratch("other");
    let path = dir.join(STOP_FILE_NAME);
    let stop = StopSignal::new();
    assert!(arm_stop_file(&path, &stop.flag()).is_armed());

    stop.request(); // the console handler, or the stdio `shutdown` word
    std::thread::sleep(STOP_POLL * 4);
    assert!(!path.exists(), "…and the watcher must not have created anything");
    // The scratch directory deletes only if the watcher released it, which is the observable
    // half of "the thread returned".
    std::fs::remove_dir_all(&dir).expect("the watcher must not still hold the directory");
}

/// ⚠ The real thing, end to end: install the handlers, send this process a REAL SIGTERM, and
/// watch the flag come up. Everything else in this file is about a bool; this is the only test
/// that proves the bool is wired to the kernel.
///
/// Raising SIGTERM at ourselves is safe here precisely BECAUSE the handler is installed first —
/// the default disposition (terminate) has been replaced by the time the signal is delivered.
/// `signal_hook::low_level::raise` is a safe wrapper, so this needs no `unsafe` and no direct
/// `libc` edge. Under nextest each test is its own process; under plain `cargo test` the
/// registration is process-wide and additive, so a sibling test is unaffected either way.
#[cfg(unix)]
#[test]
fn a_real_sigterm_raises_the_flag_and_the_teardown_is_claimed_once() {
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        let stop = StopSignal::new();
        assert_eq!(
            install_handlers(&stop.flag()),
            HandlerOutcome::Installed,
            "the handlers must install on a POSIX box"
        );
        assert!(!stop.is_requested(), "installing a handler must not itself request a stop");

        signal_hook::low_level::raise(sig).expect("raise a signal at this process");

        // Delivery is asynchronous — the handler runs on whichever thread the kernel picks.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stop.is_requested() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            stop.is_requested(),
            "signal {sig} must raise the stop flag — this is the whole graceful stop"
        );
        assert!(stop.begin_teardown(), "the signal-driven stop claims the teardown");
        assert!(!stop.begin_teardown(), "…exactly once");
    }
}
