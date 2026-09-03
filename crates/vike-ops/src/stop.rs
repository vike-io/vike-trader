//! **One stop flag, many triggers** — the process-stop primitive both headless daemons share.
//!
//! `vike-tradehub` and `vike-recorder` each already had a cooperative stop: a control word on stdin
//! raises an `AtomicBool`, the daemon observes it, and the teardown written below runs. Under
//! systemd that channel does not exist — stdin is `/dev/null`, which reads EOF immediately, and both
//! daemons deliberately treat a non-tty EOF as "keep running headless" so they do not exit at
//! startup. So `systemctl stop` sent SIGTERM, the kernel took the default disposition, and **not one
//! line of Rust ran**: the recorder never flushed its buffers, and the trading daemon never reached
//! `crates/vike-core/src/runtime/watchdog.rs`'s `cancel_resting_on_shutdown` — the resting book was
//! abandoned at the venue rather than cancelled, on every restart of every box, while
//! `systemctl status` reported a clean stop. `docs/ops/graceful-stop.md` carries the the CI box
//! measurements, the rejected options and the decision; this module is the option it chose.
//!
//! # The contract
//!
//! **A signal is a TRIGGER, never a teardown.** [`install_handlers`] points SIGTERM and SIGINT at
//! the SAME flag the stdio word raises, and does nothing else. There is one stop flag and one
//! teardown path, because a second teardown route is how the two drift — and drift here reads as
//! "the interactive stop closes the book and the service stop does not", which is the defect being
//! fixed, wearing a different hat.
//!
//! # Why a handler may only store a bool
//!
//! The set of async-signal-safe operations is tiny. A handler runs on whatever thread the kernel
//! picked, at an arbitrary instruction, and a handler that allocates, takes a lock or logs can
//! deadlock against the very thread it interrupted (`malloc`'s arena lock and `tracing`'s subscriber
//! are both reachable from ordinary code). Storing into an already-allocated [`AtomicBool`] touches
//! neither. That is the whole reason this uses `signal-hook`'s FLAG registration rather than its
//! callback API: the flag API's handler is a lock-free store and nothing else, so it is safe by
//! construction rather than by review.
//!
//! # Windows
//!
//! There is no POSIX signal, but there ARE console control events (Ctrl-C, Ctrl-Break, window
//! close), and before split-plane B10 nothing caught them: the OS default handler terminated the
//! process with ZERO Rust run — the same incident shape the unix arm fixed for SIGTERM. The
//! Windows arm now bridges those events to the stop flag through `ctrlc`'s safe
//! `SetConsoleCtrlHandler` wrapper (the raw `windows-sys` route is `unsafe extern "system"`, and
//! this crate inherits the workspace `unsafe_code = "forbid"`). Same contract as unix: the handler
//! stores a bool and returns; [`StopSignal::wait`] notices within [`STOP_POLL`]. `signal-hook`
//! stays `cfg(unix)`-gated; `ctrlc` is `cfg(windows)`-gated — neither platform compiles the
//! other's bridge. Only a platform with NEITHER (no POSIX signals, no Windows console) returns
//! [`HandlerOutcome::Unsupported`] now.
//!
//! ## ⚠ …and a console event needs a CONSOLE, which a background daemon does not have
//!
//! That is the hole split-plane I13 closes, and it is not a small one: a console control event can
//! only be DELIVERED by the console the process is attached to. A daemon started detached — a
//! hidden `Start-Process`, a Task Scheduler job at boot, anything that is the Windows analogue of
//! `systemctl start` — has no console anybody can type into, so B10's handler is installed and
//! unreachable, the stdio word is unreachable for the same reason systemd made it unreachable
//! (stdin is not a TTY), and the ONLY remaining stop is `taskkill`, which is `SIGKILL` with a
//! different spelling: no teardown, no cancel sweep, the resting book abandoned at the venue. That
//! is byte-for-byte the incident this whole module exists to close, on the one platform where it
//! was still live.
//!
//! [`arm_stop_file`] is ANOTHER trigger of the SAME flag: a path the daemon polls, whose mere
//! EXISTENCE requests a stop. One flag, many triggers — unchanged. What is new is only how the
//! request arrives, and a file is the one channel a detached Windows process can be reached
//! through without an `unsafe extern "system"` `GenerateConsoleCtrlEvent` dance or a remote
//! shutdown verb on a daemon deliberately built without one.
//!
//! **It is `cfg(windows)` and stays that way.** `docs/ops/graceful-stop.md`'s option 3 — a sentinel
//! plus a blocking `ExecStop=` — was WEIGHED and REFUSED for unix, and every reason still holds
//! there: SIGTERM already covers every sender including a bare `kill`, and a second route would be
//! a second thing to keep in step. None of those reasons survives the move to Windows, because on
//! Windows the alternative is not a signal, it is nothing. So the platform that has a better answer
//! keeps it and does not compile this, exactly as it does not compile `ctrlc`.
//!
//! # Exactly once
//!
//! A stop word and a signal can arrive together, so both [`StopSignal::request`] and the handler's
//! store are idempotent (raising an already-raised flag is a no-op) and [`StopSignal::begin_teardown`]
//! is a compare-exchange that answers `true` to exactly one caller for the life of the process.
//! Today each daemon has one teardown site, so the claim is defence in depth; it exists so a future
//! second stop route cannot quietly become a second teardown. `tests/graceful_stop_pin.rs` and this
//! module's own tests prove it under concurrency rather than by inspection.
//!
//! # ⚠ The LOSER of that claim must WAIT — it must never simply carry on
//!
//! Losing the claim means "somebody else is tearing down"; it does **not** mean "there is nothing
//! left to do". In a daemon the loser is a thread that can END THE PROCESS — both daemons' claim
//! sites are in `main`/`run`, and returning from there terminates every other thread mid-instruction.
//! A guard whose stated job is to stop a second teardown would then have converted "two teardowns"
//! into "**the winner's teardown is killed in flight by the loser's process exit**": a truncated
//! cancel sweep with orders still resting at the venue, or a Parquet part torn between its footer
//! and its manifest entry. That is strictly worse than the duplicate it prevents.
//!
//! So the claim has a second half. The winner calls [`StopSignal::finish_teardown`] when its
//! teardown returns; every loser calls [`StopSignal::await_teardown`] and blocks until then. The wait
//! is BOUNDED by the caller's own stop budget, because a loser that waits forever on a wedged winner
//! is the hang the bounded teardown exists to prevent — and because that bound is part of the same
//! arithmetic the unit's `TimeoutStopSec=` is sized against. This module's own
//! `a_losing_claim_cannot_exit_while_the_winner_is_still_tearing_down` proves the ordering with real
//! threads, and `crates/vike-ops/tests/graceful_stop_pin.rs` holds both daemons to the shape.
//!
//! # Why this crate
//!
//! It sits beside [`crate::shutdown`], the bounded teardown orchestration the stop path ends in —
//! the two halves of one story, in the crate that is already the headless daemons' operations
//! layer. It is in the LIGHT half (no `full` feature): std plus one `cfg(unix)` package, naming no
//! vike type, so `vike-recorder` reaches it with `default-features = false` and does not grow a
//! `vike-core`/`vike-exec` edge for a stop flag.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How often [`StopSignal::wait`] looks at the flag.
///
/// A signal wakes nothing by itself here — the handler stores a bool and returns, so the waiting
/// thread finds out on its next look. This is therefore the added latency between `systemctl stop`
/// and the first line of teardown, and it is spent doing nothing on a thread that would otherwise be
/// parked forever. Small enough to be invisible next to the teardown it precedes (a venue feed
/// shutdown is a socket read timeout), large enough that an idle daemon is not a spin loop.
pub const STOP_POLL: Duration = Duration::from_millis(50);

/// What [`install_handlers`] actually did — returned as DATA so the binary logs it through the
/// subscriber it owns.
///
/// The alternative (log from in here) is the shape this workspace keeps removing: a library that
/// writes to its caller's stderr on its own initiative cannot be used by a binary whose stdout is a
/// protocol, and both daemons' stdout is one. It is an enum rather than a `Result<(), E>` because
/// "there are no signals on this platform" is not a failure and must not be reported as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerOutcome {
    /// SIGTERM and SIGINT now raise the flag. The graceful stop is armed.
    Installed,
    /// A platform with neither POSIX signals nor a Windows console (unix and windows both have
    /// real arms now): nothing was installed, and nothing could have been. The stdio stop word is
    /// the only stop, which is what it was on every platform before this module.
    Unsupported,
    /// Registration failed. The daemon still runs — it simply stops the way it did before, and the
    /// caller must say so loudly, because "the graceful stop is armed" is now a false belief an
    /// operator would otherwise hold.
    Failed(String),
}

impl HandlerOutcome {
    /// Is the graceful stop actually armed on this process? Handy for a call site that wants to log
    /// at `warn` for anything else without matching every variant.
    pub fn is_installed(&self) -> bool {
        matches!(self, HandlerOutcome::Installed)
    }
}

/// The one stop flag, plus the one-shot teardown claim and the completion it publishes.
///
/// `requested` is shared (an `Arc`, because that is what a signal handler and a control thread can
/// hold); `claimed` and `finished` are plain flags on the value itself, because every party to the
/// claim already holds the same `StopSignal` (by reference, or behind an `Arc`).
#[derive(Debug, Default)]
pub struct StopSignal {
    requested: Arc<AtomicBool>,
    claimed: AtomicBool,
    /// Raised by [`StopSignal::finish_teardown`] — the winner's "you may exit now" to every loser
    /// parked in [`StopSignal::await_teardown`]. Separate from `claimed` because the whole point is
    /// to distinguish "a teardown is RUNNING" from "the teardown is DONE"; one flag cannot say both,
    /// and reading `claimed` as "done" is exactly the bug this pair exists to close.
    finished: AtomicBool,
}

impl StopSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// A handle on the flag, for a signal handler ([`install_handlers`]) or a control thread. Every
    /// clone raises the SAME flag — that is the invariant this whole module exists to hold.
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.requested)
    }

    /// Raise the stop request. Idempotent — see [`request_stop`].
    pub fn request(&self) {
        request_stop(&self.requested);
    }

    /// Has a stop been requested by anything?
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Block the calling thread until a stop is requested, checking every [`STOP_POLL`].
    ///
    /// This replaces `vike-tradehub`'s `loop { std::thread::park(); }`. A park is not merely
    /// equivalent-but-older: nothing in that daemon ever unparked it, and a POSIX signal does not —
    /// `signal-hook` installs its handler with `SA_RESTART` and Rust's std retries `EINTR` anyway,
    /// so a thread blocked in a park or in a `read` does not come back to look at anything. The
    /// thread that must observe the flag has to be a thread that periodically looks.
    pub fn wait(&self) {
        while !self.is_requested() {
            std::thread::sleep(STOP_POLL);
        }
    }

    /// Claim the teardown. Returns `true` for exactly one caller, ever, and `false` for every other.
    ///
    /// The caller that gets `true` runs the teardown and MUST call [`Self::finish_teardown`] when it
    /// returns. A caller that gets `false` must not run a teardown — running it twice means
    /// double-cancelling a book, double-flushing a writer, or joining a thread that has been joined
    /// — and must not simply CARRY ON either: see [`Self::await_teardown`] and the module doc.
    pub fn begin_teardown(&self) -> bool {
        self.claimed.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }

    /// The winner's announcement that its teardown has RETURNED — every loser parked in
    /// [`Self::await_teardown`] is released by this and by nothing else.
    ///
    /// Call it after the bounded teardown returns, on either outcome: a hard-capped teardown has
    /// finished *waiting*, which is the only thing a loser can act on, and leaving losers parked
    /// past the winner's own bound would add that bound to itself. Idempotent.
    pub fn finish_teardown(&self) {
        self.finished.store(true, Ordering::SeqCst);
    }

    /// Has the winner's teardown returned?
    pub fn is_teardown_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// **The loser's half of the claim.** Block until the winner calls [`Self::finish_teardown`],
    /// or until `bound` elapses; `true` means the winner finished, `false` means the bound won.
    ///
    /// A daemon calls this on the `begin_teardown() == false` arm INSTEAD of returning. Returning
    /// there is what a reviewer caught in the first cut of this module: both claim sites sit in
    /// `main`/`run`, so the loser's `return` is a PROCESS EXIT, and it would kill the winner's
    /// teardown at an arbitrary instruction — a truncated cancel sweep or a torn Parquet part
    /// instead of the duplicated one the claim was protecting against.
    ///
    /// `bound` is the caller's OWN total stop budget, not a number chosen here: the wait has to be
    /// bounded (a wedged winner must not hold the process open past the unit's `TimeoutStopSec=`,
    /// where SIGKILL is waiting anyway) and it has to be counted in the same arithmetic as the
    /// teardown it is waiting for, which only the caller knows. `false` is therefore not an error to
    /// swallow — it says the winner outran its own budget, and the caller should say so.
    pub fn await_teardown(&self, bound: Duration) -> bool {
        let deadline = std::time::Instant::now() + bound;
        while !self.is_teardown_finished() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(STOP_POLL.min(bound).max(Duration::from_millis(1)));
        }
        true
    }
}

/// Raise a stop request through a bare flag handle — what a control thread holds, and what the
/// signal handler does.
///
/// Idempotent by construction: storing `true` into an already-`true` flag is a no-op, which is the
/// property that makes "the stdio word and the signal arrived together" a non-event rather than a
/// race to be reasoned about.
pub fn request_stop(flag: &AtomicBool) {
    flag.store(true, Ordering::SeqCst);
}

/// Point SIGTERM and SIGINT at `flag`, so `systemctl stop`, a bare `kill`, a container stop and
/// Ctrl-C all drive the SAME stop path the stdio word drives.
///
/// Both signals, deliberately: SIGTERM is what a supervisor sends and SIGINT is what a human at a
/// terminal sends, and a daemon that flushed on one and not the other would teach an operator a
/// rule that is wrong half the time.
///
/// The handler `signal-hook` installs stores `true` into `flag` and returns — see the module doc for
/// why that is the only thing a handler is allowed to do. Registration is additive, so calling this
/// twice in one process is harmless (both flags are raised).
#[cfg(unix)]
pub fn install_handlers(flag: &Arc<AtomicBool>) -> HandlerOutcome {
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        if let Err(e) = signal_hook::flag::register(sig, Arc::clone(flag)) {
            return HandlerOutcome::Failed(format!("registering signal {sig}: {e}"));
        }
    }
    HandlerOutcome::Installed
}

/// Windows: bridge the console control events (Ctrl-C, Ctrl-Break, window close) to the stop flag
/// through `ctrlc`'s safe `SetConsoleCtrlHandler` wrapper — see the module doc's Windows section.
/// Same contract as the unix arm: the handler stores a bool and returns; [`StopSignal::wait`]
/// notices within [`STOP_POLL`]. `ctrlc::set_handler` registers ONCE per process — a second call
/// errs, which surfaces as [`HandlerOutcome::Failed`] exactly like a failed `signal-hook`
/// registration would, and the binary logs it loudly (a false "graceful stop is armed" belief is
/// the thing this enum exists to prevent).
#[cfg(windows)]
pub fn install_handlers(flag: &Arc<AtomicBool>) -> HandlerOutcome {
    let flag = Arc::clone(flag);
    match ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst)) {
        Ok(()) => HandlerOutcome::Installed,
        Err(e) => HandlerOutcome::Failed(format!("SetConsoleCtrlHandler: {e}")),
    }
}

/// Neither unix nor windows: nothing to install. The stdio stop word is the only stop.
#[cfg(not(any(unix, windows)))]
pub fn install_handlers(_flag: &Arc<AtomicBool>) -> HandlerOutcome {
    HandlerOutcome::Unsupported
}

/// The file name a daemon watches for under its state directory, and the name an operator creates.
///
/// It is spelled as the interactive stop WORD (`shutdown`/`quit` on stdin) in capitals, because it
/// means the same thing and an operator should not have to learn a second vocabulary — and it is
/// deliberately NOT `STOP`, which reads a hair away from the `HALT` sentinel sitting in the very
/// same directory and meaning something different. `HALT` refuses the next ORDER and leaves the
/// daemon running (`vike_bridge_core::halt`); this ends the PROCESS through the full teardown. Two
/// switches in one directory whose names differ by a synonym is an incident waiting for a bad night.
///
/// Declared unconditionally even though only the `cfg(windows)` arm watches for it, so the ops page,
/// the host script's gate and any platform's test can name the same constant instead of three
/// string literals that agree until one of them does not.
pub const STOP_FILE_NAME: &str = "SHUTDOWN";

/// What [`arm_stop_file`] did, as DATA — same discipline as [`HandlerOutcome`], and for the same
/// reason: this is armed before the log subscriber exists in one daemon and beside it in the other,
/// and a library that writes to its caller's stderr cannot be used by a binary whose stdout is a
/// protocol.
#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopFileOutcome {
    /// The watcher is running and this path is the one it watches. Log it: an operator who cannot
    /// see the path cannot use the switch, and guessing at it is how the HALT sentinel spent a
    /// release disarmed.
    Armed(std::path::PathBuf),
    /// Nothing is watching. The daemon still runs — it simply has no stop route that survives being
    /// detached from a console, and the caller must say so LOUDLY for exactly the reason
    /// [`HandlerOutcome::Failed`] must: "the graceful stop is armed" is otherwise a false belief an
    /// operator holds until the moment they need it not to be.
    Failed(String),
}

#[cfg(windows)]
impl StopFileOutcome {
    /// Is a stop file actually being watched?
    pub fn is_armed(&self) -> bool {
        matches!(self, StopFileOutcome::Armed(_))
    }
}

/// **Windows: watch `path` and raise `flag` the moment it exists** — another trigger of the one
/// stop flag, and the only one a DETACHED daemon can be reached through. See the module doc's
/// Windows section for why this exists here and nowhere near a unix build.
///
/// Two things happen before the watcher starts, and each is a failure mode somebody would otherwise
/// meet at the worst time:
///
/// * **the parent directory must already exist.** Nothing creates it lazily — a typo'd path would
///   otherwise arm a watcher over a file that can never appear, which looks exactly like a working
///   switch until an operator needs it. The same argument `deploy/vike-tradehub.service` makes
///   about `VIKE_HALT_FILE`, which is why both resolve to the same granted state directory.
/// * **a STALE file is REMOVED, not obeyed.** A stop request is an EVENT, and a daemon that was
///   killed between the request and its exit leaves the file behind; a fresh start that read it
///   would stop within [`STOP_POLL`] of coming up, and then do it again on every restart — a boot
///   loop whose cause is a file nobody remembers. If the removal fails, this returns
///   [`StopFileOutcome::Failed`] and arms NOTHING rather than starting a watcher that is guaranteed
///   to fire immediately.
///
/// The watcher runs on its own thread and ENDS when any trigger raises the flag — including the
/// console handler or the stdio word — so it costs one `stat` per [`STOP_POLL`] for the life of the
/// daemon and nothing after. `stat` was already the cost of `vike_bridge_core::halt`'s check on
/// every single order submit; this is that, once every fiftieth of a second, off the fold.
///
/// It deliberately does NOT delete the file after acting. The operator's own stop tooling removes it
/// once the process is gone (it is the tool that knows the process is gone), and until then the file
/// on disk is the evidence of what asked for the stop.
#[cfg(windows)]
pub fn arm_stop_file(path: &std::path::Path, flag: &Arc<AtomicBool>) -> StopFileOutcome {
    let parent = match path.parent() {
        Some(p) if p.as_os_str().is_empty() => std::path::Path::new("."),
        Some(p) => p,
        None => {
            return StopFileOutcome::Failed(format!("{} has no parent directory", path.display()))
        }
    };
    if !parent.is_dir() {
        return StopFileOutcome::Failed(format!(
            "{} is not a directory, so the stop file {} can never appear — nothing is watching",
            parent.display(),
            path.display()
        ));
    }
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return StopFileOutcome::Failed(format!(
                "a stale stop file {} could not be removed ({e}) — arming a watcher over it would \
                 stop this daemon within {STOP_POLL:?} of every start",
                path.display()
            ));
        }
    }
    let watched = path.to_path_buf();
    let flag = Arc::clone(flag);
    std::thread::Builder::new()
        .name("vike-stop-file".into())
        .spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                if watched.exists() {
                    request_stop(&flag);
                    return;
                }
                std::thread::sleep(STOP_POLL);
            }
        })
        .map(|_| StopFileOutcome::Armed(path.to_path_buf()))
        .unwrap_or_else(|e| StopFileOutcome::Failed(format!("spawning the stop-file watcher: {e}")))
}

#[cfg(test)]
mod tests {
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
}
