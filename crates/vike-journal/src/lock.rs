//! Duplicate-instance interlock for a write-ahead journal directory.
//!
//! CONTRACT: at most ONE writer may hold a journal directory at a time — one
//! [`crate::CommandJournal`], in one process. This is a desktop product; a double-launch
//! (second shortcut click, an operator restarting before the old process died, a stray
//! `cargo run`) is not hypothetical, and two writers on one WAL is silent corruption in four
//! distinct ways: interleaved frames break the replay determinism fence (`state_hash` replay
//! fails), the two `ClientOrderIdGenerator`s resume the SAME `coid_session` and mint duplicate
//! client-order-ids, two mounted makers quote against each other on ONE venue account, and both
//! reconcile drivers fold the same divergence twice.
//!
//! MECHANISM: an EXCLUSIVE advisory lock (`std::fs::File::try_lock`, which is `flock(LOCK_EX |
//! LOCK_NB)` on Unix and `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY)` on
//! Windows) taken on a sentinel file `<journal_dir>/`[`LOCK_FILE`]. On the two platforms this
//! ships on — Unix `flock` and Windows `LockFileEx` — the lock is keyed on the open file
//! description / file handle, NOT on the process, so a second `CommandJournal` in the SAME process
//! is refused exactly like a second process — which is what makes the interlock unit-testable at
//! all. (That is a property of those two primitives, not a universal one: a port to a target
//! whose `try_lock` is backed by POSIX `fcntl` record locks would key on the PROCESS instead, and
//! the same-process test would then pass the second acquire — such a port must re-verify this
//! paragraph, not inherit it.) Deliberately std-only: no new dependency, no `unsafe`, no `cfg` fork
//! (the crate carries a single audited `unsafe` carve-out for `MmapMut::map_mut` and this must not
//! widen it).
//!
//! FAILURE MODE: fail FAST and LOUD. A refused acquire returns an [`io::ErrorKind::AddrInUse`]
//! error whose message NAMES the directory and the lock file and says what to do about it — never
//! a silent second writer, never a bare panic.
//!
//! RELEASE: the guard is held for the writer's lifetime and releases when the [`File`] closes,
//! i.e. when the guard drops (documented `std::fs::File` lock behavior). That covers a hard crash
//! too: the OS closes the handle, so a leftover `LOCK` file is NEVER stale — the file persisting
//! on disk carries no state, only the live lock does. No cleanup/unlink is attempted (unlinking a
//! lock file is a race, not a tidy-up).
//!
//! SCOPE: only the WRITE path locks. The dir-scanning readers
//! ([`crate::CommandJournal::read_all`], `latest_segment_version`, `read_since`,
//! `prune_before_latest_snap`) take no lock, so offline inspection/replay tooling keeps working
//! against a directory a live core owns, exactly as it does today. The sentinel is invisible to
//! all of them: every one keys off the `journal-<idx>.vjl` name.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

/// The sentinel file name inside a journal directory. Deliberately NOT `*.vjl`: every reader on
/// [`crate::CommandJournal`] selects segments by the `journal-` prefix + `.vjl` suffix, so this file is
/// inert to `read_all` / `latest_segment_version` / `prune_before_latest_snap`.
pub const LOCK_FILE: &str = "LOCK";

/// An acquired exclusive lock on one journal directory. Hold it for as long as the journal is
/// open; dropping it releases the lock (see the module doc).
///
/// `Debug` is derived (and NOT hand-written): `File`'s own `Debug` prints only the handle/fd and
/// path, nothing sensitive, and the derive is what lets a caller `expect_err` on a refused
/// acquire — which the contention test does.
#[derive(Debug)]
pub struct JournalLock {
    /// The locked sentinel handle. Never read after construction — its VALUE is irrelevant, its
    /// LIFETIME is the lock. (The derived `Debug` above reads it, so this is not a dead field.)
    _file: File,
}

impl JournalLock {
    /// Take the exclusive lock on `dir`, creating `<dir>/`[`LOCK_FILE`] if absent. `dir` must
    /// already exist (the journal's `create_dir_all` runs first).
    ///
    /// Errors:
    /// - [`io::ErrorKind::AddrInUse`] — another live writer holds it. This is the double-launch
    ///   case and the message is written for the operator who sees it.
    /// - anything else — the sentinel could not be created/opened, or the platform refused the
    ///   lock; surfaced verbatim rather than degraded into "no lock".
    pub fn acquire(dir: &Path) -> io::Result<Self> {
        let path = dir.join(LOCK_FILE);
        // truncate(false): the sentinel's bytes are meaningless, but never rewrite a file another
        // process currently holds. read+write (not append) — Windows refuses to lock an
        // append-opened handle.
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        // Bind the attempt BEFORE the match so the `&file` autoref is unambiguously dead by the
        // time the success arm MOVES `file` into the guard.
        let attempt = file.try_lock();
        match attempt {
            Ok(()) => Ok(JournalLock { _file: file }),
            Err(TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "another vike instance is already writing the journal directory {} \
                     (lock file {}). Exactly one process may write a journal: two writers \
                     interleave WAL frames (replay's determinism fence then fails), re-mint the \
                     same client order ids, and double-fold reconciliation. Close the other \
                     instance, or point this one at a different journal directory.",
                    dir.display(),
                    path.display()
                ),
            )),
            Err(other) => Err(io::Error::other(format!(
                "could not acquire the journal directory lock at {}: {other:?}",
                path.display()
            ))),
        }
    }
}

#[path = "lock_tests.rs"]
#[cfg(test)]
mod lock_tests;
