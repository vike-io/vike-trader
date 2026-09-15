//! **The retention rule for `<project>/tmp`** — an OWNED scratch directory, plus the bounded sweep
//! that catches what ownership structurally cannot.
//!
//! Ported from nothing: net-new Rust surface, the second half of
//! [`crate::state_path::PROJECT_TMP_DIR`]. Read that constant first for WHERE scratch goes and why;
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
//! from many files (`crates/vike-core/src/journal/segment.rs`'s `reserve_blocks` calls
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
//! Same contract as [`crate::state_path`]: no environment read, no walk. The caller — a composition
//! root — resolves `<project>/tmp` through
//! [`crate::state_path::project_tmp_dir_from`] and passes the path in. Filesystem contact is the
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
    /// `root` is `<project>/tmp` — [`crate::state_path::project_tmp_dir_from`]'s answer — and is
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway scratch ROOT for one test, standing in for `<project>/tmp`.
    ///
    /// Built with [`ScratchDir`] itself, which is dogfooding rather than cuteness: the root is then
    /// unique per process, self-deleting on the panic path, and needs no `tempfile` dev-dependency
    /// in the crate every binary in this workspace links. The system temp directory is legitimate
    /// HERE — this is test code, `crates/vike-ops/tests/system_temp_gate.rs` scopes itself to
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
}
