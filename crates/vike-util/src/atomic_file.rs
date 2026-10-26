//! Replace a whole file atomically, under ONE durability policy for every writer in the workspace
//! (decision 0141: a filesystem write whose POLICY is the point of sharing it).
//!
//! # The policy
//!
//! [`write_atomic`] does exactly this, in this order, and every whole-file replace that used to
//! carry its own copy now calls it:
//!
//! 1. write the bytes to a TEMPORARY file in the destination's own directory (the same filesystem,
//!    so the rename below can never become a cross-device copy), named
//!    `<file name>.<pid>.<n>.tmp` — unique per call, so two writers in one process, or two
//!    processes, never share a temporary file;
//! 2. `sync_all` the temporary file, so the bytes are on disk before any name points at them;
//! 3. `std::fs::rename` it over the destination;
//! 4. fsync the destination's DIRECTORY, so the new name survives a power loss too (unix only; see
//!    below).
//!
//! ⚠ **The destination is NEVER removed first.** Until the rename it holds its old bytes; after it,
//! the new ones. A reader sees one or the other, never a truncated file and never no file.
//! `docs/decisions/0113-order-ownership-survives-a-restart-in-a-state-dir-file.md` rejected the
//! remove-then-retry fallback several copies carried because it can leave no file at all.
//!
//! On ANY error the temporary file is removed (best effort) and the destination is exactly what it
//! was. One exception, stated so nobody reads more into an `Err` than it says: an error from the
//! DIRECTORY fsync (step 4) comes after the rename, so the new bytes are already in place and only
//! the durability of their name is in doubt.
//!
//! # What it does not do
//!
//! - **It does not create the parent directory.** The writers disagreed (some created it, some
//!   resolved a path whose directory already exists), so the caller that wants a missing parent
//!   created does it before calling. A missing parent is the temp file's `NotFound`.
//! - **It does not serialize.** Bytes in; each caller keeps its own format.
//! - **It does not sweep temporary files a crash left behind.** A leftover `<name>.<pid>.<n>.tmp`
//!   never blocks a later write (a later call picks a fresh name, and one that happened to reuse the
//!   name would truncate it), but it stays on disk until something removes it. Every name ends in
//!   `.tmp`, so a reader that lists `*.json` never sees one.
//!
//! # Platforms: what this relies on
//!
//! - **`rename` over an existing file.** `std::fs::rename` is documented to replace `to` "if `to`
//!   already exists", on every platform: `rename(2)` on unix, and on Windows `MoveFileExW` with
//!   `MOVEFILE_REPLACE_EXISTING` (or `SetFileInformationByHandle` with replace semantics). What
//!   Windows can still REFUSE is a destination that is a directory, or one held open by a handle
//!   that did not grant delete sharing. Then the error is returned, the temporary file removed and
//!   the destination left as it was: the write FAILS rather than removing the destination to get
//!   through. No test in this workspace runs on Windows, so that arm is written from the documented
//!   rule, not from a measurement.
//! - **The directory fsync is `cfg(unix)`.** On POSIX a `rename` is durable only once the
//!   containing directory is fsynced. Windows exposes no directory flush through `std::fs` (opening
//!   a directory as a `File` fails outright), so there it is a deliberate no-op — the handling
//!   `crates/vike-store/src/datafusion_hist/parquet_io.rs`'s `fsync_dir` has, copied exactly.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-process counter that makes each call's temporary name unique; the pid makes it unique
/// across processes.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Replace the file at `path` with `bytes`, atomically and durably — the module doc's policy.
///
/// `path`'s parent directory must exist. `path` must name a file (an empty path, `/` or `..` has no
/// file name and is refused with [`io::ErrorKind::InvalidInput`]).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("atomic write: {} names no file", path.display()),
        )
    })?;
    // `Path::new("x.json").parent()` is `Some("")`: a bare name lives in the working directory.
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let tmp = temp_path(dir, file_name);
    let replaced = write_synced(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = replaced {
        // Best effort: the write or rename error is the one worth returning.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(dir)
}

/// `<dir>/<file name>.<pid>.<n>.tmp`, `n` from [`TMP_SEQ`].
fn temp_path(dir: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut name = file_name.to_owned();
    name.push(format!(".{}.{n}.tmp", std::process::id()));
    dir.join(name)
}

/// Create (or truncate) `path` holding exactly `bytes`, synced to disk before it is closed.
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// fsync a directory so a name created or renamed inside it survives a power loss.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// Windows: no directory flush through `std::fs`; a deliberate no-op (module doc).
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "atomic_file_tests.rs"]
mod tests;
