//! **One stop flag, many triggers** — the process-stop primitive both headless daemons share.
//!
//! `vike-tradehub` and `vike-recorder` each already had a cooperative stop: a control word on stdin
//! raises an `AtomicBool`, the daemon observes it, and the teardown written below runs. Under
//! systemd that channel does not exist — stdin is `/dev/null`, which reads EOF immediately, and both
//! daemons deliberately treat a non-tty EOF as "keep running headless" so they do not exit at
//! startup. So `systemctl stop` sent SIGTERM, the kernel took the default disposition, and **not one
//! line of Rust ran**: the recorder never flushed its buffers, and the trading daemon never reached
//! `crates/vike-core/src/runtime/watchdog/orders.rs`'s `cancel_resting_on_shutdown` — the resting book was
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
//! `arm_stop_file` (Windows only) is ANOTHER trigger of the SAME flag: a path the daemon polls, whose mere
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
//! second stop route cannot quietly become a second teardown. `tests/container_deploy/graceful_stop_pin.rs` and this
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
//! threads, and `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs` holds both daemons to the shape.
//!
//! # Why this crate
//!
//! It sits beside [`crate::shutdown`], the bounded teardown orchestration the stop path ends in —
//! the two halves of one story, in the crate that is already the headless daemons' operations
//! layer. It is in the LIGHT half (no `full` feature): std plus one `cfg(unix)` package, naming no
//! vike type, so `vike-recorder` reaches it with `default-features = false` and does not grow a
//! `vike-core`/`vike-exec` edge for a stop flag.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
///   about the HALT sentinel's path, which is why both resolve to the same granted state directory.
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
            return StopFileOutcome::Failed(format!("{} has no parent directory", path.display()));
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

#[path = "stop_tests.rs"]
#[cfg(test)]
mod stop_tests;
