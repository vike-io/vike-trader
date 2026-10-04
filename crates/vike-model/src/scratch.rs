//! **The retention rule for `<project>/tmp`** — an OWNED scratch directory, plus the bounded sweep
//! that catches what ownership structurally cannot.
//!
//! Ported from nothing: net-new Rust surface, the second half of
//! [`crate::paths::state_path::PROJECT_TMP_DIR`]. Read that constant first for WHERE scratch goes and why;
//! this module is WHY IT DOES NOT ACCUMULATE, which is the half that must not be skipped.
//!
//! # Why this module is not optional
//!
//! The system temp directory has exactly one property the project folder does not: **something else
//! empties it.** Moving scratch into the project without replacing that is not a lateral move — it
//! takes a leak that filled a machine's root filesystem and re-points it at the volume an operator
//! mounts, backs up and pays for.
//!
//! The measurement, from the CI box on 2026-08-23: **26,851 leaked scratch directories totalling
//! 215 GB — about 70% of that filesystem.** The size came from the journal's own design rather than
//! from many files (`crates/vike-journal/src/segment.rs`'s `reserve_blocks` calls
//! `posix_fallocate`, so a 64 MiB segment is fully allocated the moment it is created), but the
//! COUNT came from nothing removing a directory, ever.
//!
//! # Two mechanisms, because one of them provably cannot finish the job
//!
//! **1. Ownership.** [`ScratchDir`] removes its directory and everything under it when the guard
//! drops. Unwinding runs destructors, so a caller that PANICS cleans up exactly like one that
//! returns — the property `crates/vike-ops/tests/temp_path_gate.rs` already names as what makes
//! `tempfile` "the better of the two" test spellings, and the one a keep-on-failure design gets
//! wrong precisely where a leak is least likely to be noticed.
//!
//! **2. A bounded sweep.** [`sweep`] is not belt-and-braces; it closes a residual ownership cannot
//! reach, and the residual is exactly the population that matters:
//!
//!   * `Drop` does not run on `SIGKILL`, on an OOM kill, under `panic = "abort"`, or on power loss.
//!     A long-running daemon on a memory-limited box is not an exotic case here — it is the normal
//!     one.
//!   * A guard only ever cleans what THIS process created. After an abort, nothing looks at that
//!     directory again for the rest of the machine's life. That is not a hypothesis about the
//!     future: the leaked directories above were minted by a helper that DID clean up — it removed
//!     a stale directory at the START of a run — and it still leaked, because the name it cleaned
//!     was derived from a pid that never repeated.
//!
//! So ownership bounds the ordinary path and the sweep bounds the abort path, and neither
//! substitutes for the other.
//!
//! # The sweep's policy, and why it is a COUNT rather than an age
//!
//! Keep the newest `max_entries` direct children of the scratch root; remove the rest, oldest
//! first. That is `vike_log`'s `file_max_files` verbatim in shape — a default in code
//! ([`DEFAULT_MAX_SCRATCH_ENTRIES`]) rather than in prose, `None` meaning keep everything — and
//! reusing a policy this workspace already operates beats inventing a second one.
//!
//! ⚠ It is a count rather than an age for a reason that is not aesthetic: an age needs `now`, and
//! `vike-model` is in `crates/vike-ops/tests/clock_pin.rs`'s `DETERMINISM_CRITICAL_CRATES`, where
//! `SystemTime::now()` is a ratchet that may shrink and never grow. Ordering by modification time
//! needs no clock — it compares stored stamps to each other — so the policy that avoids the clock is
//! also the policy with a precedent.
//!
//! ⚠ **The residual, stated rather than assumed away:** a sweep cannot tell an ABANDONED directory
//! from one a concurrent sibling process is still using, and there is no portable way to ask. So it
//! is bounded by keeping a generous newest-N (a live directory's mtime advances every time an entry
//! is created or removed in it, which is what puts a working directory at the newest end), and it is
//! called ONCE at startup rather than on a timer. The same residual is `file_max_files`'s and it has
//! not bitten there.
//!
//! # Purity, and who reads the environment
//!
//! Same contract as [`crate::paths::state_path`]: no environment read, no walk. The caller — a composition
//! root — resolves `<project>/tmp` through
//! [`crate::paths::state_path::project_tmp_dir_from`] and passes the path in. Filesystem contact is the
//! `create_dir_all` in [`ScratchDir::create_in`], the `read_dir`/`metadata` in [`sweep`], and the
//! removals both perform.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How many scratch entries [`sweep`] keeps when the caller has no opinion.
///
/// The number lives HERE rather than in prose, exactly as `vike_log::DEFAULT_MAX_LOG_FILES` does:
/// this repository has watched every hand-copied constant rot. Sized so that a handful of concurrent
/// tools plus a run or two of history all survive while an abandoned population cannot grow without
/// bound — the failure being bounded, not zero.
pub const DEFAULT_MAX_SCRATCH_ENTRIES: usize = 16;

/// Distinguishes two scratch directories minted by ONE process. See [`ScratchDir::create_in`] for
/// why this plus the process id is the whole uniqueness story and why no clock appears in it.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// A scratch directory owned by whoever created it: dropped, and it is gone.
///
/// Deliberately NOT `Clone` — two owners would each try to delete, and the second would report a
/// failure for work the first already did.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// Create a fresh directory under `root`, tagged with `tag` so a directory seen mid-run names
    /// the tool that owns it.
    ///
    /// `root` is `<project>/tmp` — [`crate::paths::state_path::project_tmp_dir_from`]'s answer — and is
    /// created if it does not exist yet, because `tmp/` is not a marker and a fresh install has
    /// none.
    ///
    /// # Uniqueness, without a clock
    ///
    /// `<tag>-<pid>-<n>`: the process id separates concurrent processes (an id is unique among LIVE
    /// processes, which is exactly the population that could collide), and [`SEQ`] separates two
    /// allocations inside one. No nanosecond stamp, because `vike-model` is in
    /// `crates/vike-ops/tests/clock_pin.rs`'s scope and a wall-clock read here would need a
    /// `CLOCK_PIN` row on a ratchet that may only shrink.
    ///
    /// ⚠ A pre-existing directory at the chosen path is REMOVED first, and that is safe rather than
    /// reckless: the only way it can exist is pid REUSE after a previous run aborted, since no live
    /// process shares this one's id. It is also the behaviour that keeps an aborted run from
    /// poisoning its successor with half-written files — the hand-rolled helpers this replaces all
    /// did the same thing, and it is the one part of them that was right.
    pub fn create_in(root: &Path, tag: &str) -> io::Result<Self> {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        Self::create_at(&root.join(format!("{tag}-{}-{n}", std::process::id())))
    }

    /// [`ScratchDir::create_in`] with the name already chosen — the clear-then-create half, split
    /// out so the stale-directory behaviour can be driven at an EXPLICIT path.
    ///
    /// The alternative was a test that reached into [`SEQ`] to predict the next name, which would
    /// have raced every other test in this binary against a shared counter and turned a
    /// correctness pin into a flake.
    fn create_at(path: &Path) -> io::Result<Self> {
        if path.exists() {
            std::fs::remove_dir_all(path)?;
        }
        std::fs::create_dir_all(path)?;
        Ok(Self { path: path.to_path_buf() })
    }

    /// The guarded path. [`Deref`](std::ops::Deref) covers most uses; this is for call sites that
    /// read better named, and for `impl Into<PathBuf>` parameters that get no deref coercion.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Give up ownership, returning the path and leaving the directory on disk.
    ///
    /// The escape hatch for the one honest case — a tool whose OUTPUT is the directory — and named
    /// so it reads as a decision at the call site rather than as a missing `drop`. Everything else
    /// should hold the guard.
    #[must_use = "the directory is no longer owned; nothing will remove it"]
    pub fn keep(self) -> PathBuf {
        let path = self.path.clone();
        std::mem::forget(self);
        path
    }
}

impl Drop for ScratchDir {
    /// Best-effort by construction: a failed removal must not panic, because a `Drop` panic during
    /// an unwind aborts the process — turning a reported error into a crash, and taking the
    /// unwinding path that makes the guard worth having in the first place.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl std::ops::Deref for ScratchDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// What one [`sweep`] did. Returned as DATA rather than logged, because `vike-model` carries no
/// logging dependency — the binary that called it owns the `tracing` line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Swept {
    /// Direct children of the scratch root before the sweep.
    pub found: usize,
    /// How many were removed.
    pub removed: usize,
    /// How many removals FAILED. Non-zero is not fatal — a scratch entry another process holds open
    /// on Windows is the ordinary cause — but it is the number that says a sweep is not keeping up.
    pub failed: usize,
}

/// Prune `root` to the newest `max_entries` direct children, oldest first.
///
/// The bounded sweep the module doc argues for: it exists to bound the population no [`ScratchDir`]
/// can reach, the one left by a `SIGKILL`, an OOM kill or a power loss. Call it ONCE at startup,
/// before allocating this run's own scratch.
///
/// `max_entries` of `None` keeps everything — the `file_max_files` spelling for "retention off".
/// An absent or unreadable root is not an error: a project that has staged nothing has no `tmp/`,
/// and a startup must not fail over housekeeping. Every removal is independently best-effort and
/// counted in [`Swept::failed`], so one undeletable entry cannot stop the rest.
///
/// ⚠ Only DIRECT children are considered, and each is removed whole. The root itself is never
/// removed — the caller is about to create a directory inside it.
pub fn sweep(root: &Path, max_entries: Option<usize>) -> Swept {
    let Some(max_entries) = max_entries else { return Swept::default() };
    let Ok(entries) = std::fs::read_dir(root) else { return Swept::default() };

    // `(modified, path)`, oldest first. An entry whose mtime cannot be read sorts OLDEST
    // (`UNIX_EPOCH`), so a metadata failure makes it a removal candidate rather than a permanent
    // resident — the direction that keeps the population bounded.
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|e| {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            (mtime, e.path())
        })
        .collect();
    found.sort();

    let mut out = Swept { found: found.len(), ..Swept::default() };
    let excess = found.len().saturating_sub(max_entries);
    for (_, path) in found.into_iter().take(excess) {
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match removed {
            Ok(()) => out.removed += 1,
            Err(_) => out.failed += 1,
        }
    }
    out
}

#[path = "scratch_tests.rs"]
#[cfg(test)]
mod scratch_tests;
