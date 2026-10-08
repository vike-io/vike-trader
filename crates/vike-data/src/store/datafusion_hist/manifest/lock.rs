//! The per-series write lock (an OS advisory lock on `_manifest.lock`) and the locked delete's sweep.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use crate::store::hist::DataError;

use super::super::io;
use super::MANIFEST_LOCK;

/// A per-series write lock. Serializes the manifest read-modify-write across `commit_rows` /
/// `compact_series` / `apply_retention` so a concurrent compaction + append can't lost-update.
///
/// **The lock is the OS advisory lock on `_manifest.lock` — NOT the existence of that file.** That
/// distinction is the whole design: the kernel releases an advisory lock when the holder's
/// descriptor closes, which it does on a clean drop, on a panic, on SIGKILL, on an OOM kill, and on
/// a reboot. A dead writer therefore cannot leave a lock behind, and no reader of the lock has to
/// guess whether an owner is still alive.
///
/// It used to be the file's existence (`create_new`), which has no such property. the CI box,
/// 2026-08-04: the recorder's compaction thread was OOM-killed by its 4 GB cgroup while holding
/// this lock. SIGKILL runs no `Drop`, so the zero-byte `_manifest.lock` survived its owner — and
/// carrying neither an owner id nor a timestamp, it was indistinguishable from a live holder.
/// Every subsequent start timed out in WAL recovery and exited 1; systemd restarted the daemon
/// **2,728 times over ~11 h**, recording nothing, until an operator deleted the file by hand. A
/// second series lost 104,650 rows the same way, silently: its flushes timed out and the sink
/// discards on failure.
///
/// The file is created-if-absent and **never unlinked**. Unlinking would reintroduce the same class
/// of bug in a subtler form: a second process can create a NEW inode at the same path and lock
/// that, so two writers would each hold a lock and each believe it was alone. Leaving one empty
/// file per series is the price of the guarantee. A leftover file from any older build is inert —
/// the first `acquire` simply locks it.
pub(crate) struct SeriesLock(File);

/// `2000 × 2 ms` — the ~4 s spin budget a CONTENDED acquire pays before giving up. Named because
/// two things read it: the callers whose retry logic is sized against it
/// (`crates/vike-data/src/rec/live_rec.rs`'s `RecorderSink` documents "the store already spun ~4s"),
/// and the tests, which pass a SMALLER budget so a deliberately-contended case costs milliseconds
/// instead of four seconds.
pub(crate) const SPIN_ATTEMPTS: u32 = 2000;

impl SeriesLock {
    pub(crate) fn acquire(series_dir: &Path) -> Result<Self, DataError> {
        Self::acquire_within(series_dir, SPIN_ATTEMPTS).map(|(lock, _attempts)| lock)
    }

    /// ONE attempt, no spin, and **no `create_dir_all`** — `Ok(None)` means another writer holds
    /// this series' lock RIGHT NOW, in this process or any other.
    ///
    /// # Why a caller would want the refusal instead of the spin
    ///
    /// [`acquire`](Self::acquire)'s ~4 s spin is right for a WRITER that must eventually write:
    /// spinning costs the holder nothing, and the loser simply waits. It is wrong for the manifest
    /// REPAIR ([`super::DataFusionHist::rebuild_series_manifest_if_uncontended`]), and the
    /// asymmetry is about the critical SECTION rather than about the wait: a rebuild reads every
    /// part footer in the series INSIDE the lock, so on a series with hundreds of parts the hold is
    /// long — and `crates/vike-data/src/rec/live_rec.rs`'s `RecorderSink` discards its buffer (up to
    /// 5,000 rows) when its own flush spins that budget out. Winning a contended lock is therefore
    /// the outcome to avoid, not the one to wait for.
    ///
    /// ⚠ **The attempt IS the probe, deliberately.** A "check then acquire" pair is a TOCTOU gap a
    /// writer arrives in, so this returns the GUARD it took rather than an answer about whether one
    /// could be taken. What it cannot do is prove the series is idle: a recorder holds this lock
    /// only while committing, so an absent lock is an absent COMMIT, not an absent writer. The
    /// caller owes that sentence to its operator; this function owes only the honest snapshot.
    ///
    /// No `create_dir_all` (unlike [`acquire_within`](Self::acquire_within)) because the one caller
    /// repairs a series that EXISTS: creating the leaf here would mint a phantom series at a
    /// mistyped selector and then publish an empty manifest into it, which `list_series` would
    /// enumerate forever. An absent directory is `NotFound` — an error the caller reports rather
    /// than a leaf it invents.
    pub(crate) fn try_acquire(series_dir: &Path) -> Result<Option<Self>, DataError> {
        let path = series_dir.join(MANIFEST_LOCK);
        let file =
            OpenOptions::new().write(true).create(true).truncate(false).open(&path).map_err(io)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(SeriesLock(file))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(io(e)),
        }
    }

    /// [`SeriesLock::acquire`], with the spin budget as a parameter and the number of `try_lock`
    /// ATTEMPTS spent reported back.
    ///
    /// The attempt count is the honest measure of "how contended was this lock", and it is what the
    /// leftover-lock-file test asserts on. That test used to assert a WALL CLOCK (`elapsed() < 1s`)
    /// — and a wall clock here measures the wrong thing entirely. Only the LAST of this function's
    /// syscalls is the lock: `create_dir_all` issues `mkdirat` (which takes the PARENT directory's
    /// inode lock exclusively before it can even discover the directory already exists) plus a
    /// `stat`, then `open` walks the same parent — and in a test that parent is the system temp
    /// dir every other process on the box is also churning. Measured on the CI box (btrfs `/tmp`): in
    /// ONE 1,024-run soak of this exact uncontended acquire, p50 was 42 µs and max was 301 ms — a
    /// 7,000x spread within a single shape, living entirely in `create_dir_all`, while `attempts`
    /// stayed 1 in all 1,536 runs measured. The lock behaviour was never what varied.
    pub(crate) fn acquire_within(series_dir: &Path, spins: u32) -> Result<(Self, u32), DataError> {
        std::fs::create_dir_all(series_dir).map_err(io)?;
        let path = series_dir.join(MANIFEST_LOCK);
        // `create(true).truncate(false)`: open-or-create, and never disturb a file another process
        // may already hold — the bytes are irrelevant, the lock lives in the kernel.
        let file =
            OpenOptions::new().write(true).create(true).truncate(false).open(&path).map_err(io)?;
        for attempt in 1..=spins {
            match file.try_lock() {
                Ok(()) => return Ok((SeriesLock(file), attempt)),
                // Another writer holds it — in this process or any other. Spin: the background
                // MaintenanceScheduler churns this lock far harder than one-off manual compaction
                // did, and every holder's critical section is a manifest read-modify-write.
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(2)),
                Err(TryLockError::Error(e)) => return Err(io(e)),
            }
        }
        Err(DataError::Io(format!(
            "timeout acquiring series lock at {} after {spins} attempts",
            path.display()
        )))
    }
}

/// Remove EVERYTHING inside a series leaf except the lock file — the first half of a locked delete
/// (`DataFusionHist::delete_series_checked`, whose doc carries the whole argument).
///
/// The exclusion is the point, and it is stated as "everything but the lock" rather than as a list
/// of what a series holds: the caller is running INSIDE the guard on `_manifest.lock`, so removing
/// that file would delete the inode the kernel is holding its lock on — and on Windows the open
/// descriptor makes the removal fail outright. Every other entry goes: the `date=` partitions and
/// their parts, `_manifest.json`, a half-written `_manifest.json.tmp`, the `_wal.arrow` if a crash
/// left one, and any `_tmp-merge-…` an interrupted compaction abandoned. Spelled as a NEGATIVE
/// filter so a sibling file added later is removed by construction rather than left behind by an
/// enumeration nobody remembered to extend — which is exactly what happened when v3 added
/// `_manifest.delta` and [`MANIFEST_V2_BACKUP`]: both are swept with no edit here, and a positive
/// list would have left a delta log behind to replay over the next series created at this path.
///
/// A leaf that vanishes under us is `Ok(())`: this is a delete, and something else having already
/// done the work is the outcome asked for.
pub(crate) fn remove_series_contents(dir: &Path) -> Result<(), DataError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(io)?;
        if entry.file_name() == MANIFEST_LOCK {
            continue;
        }
        let path = entry.path();
        let removed = if entry.file_type().map_err(io)?.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match removed {
            Ok(()) => {}
            Err(ref e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => {
                return Err(DataError::Query(format!(
                    "delete series {}: removing {}: {e}",
                    dir.display(),
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

impl Drop for SeriesLock {
    fn drop(&mut self) {
        // Closing the descriptor would release the lock on its own; unlocking first makes the
        // release explicit and ordered. The FILE stays on disk — see the type doc.
        let _ = self.0.unlock();
    }
}
