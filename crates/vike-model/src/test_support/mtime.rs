//! Planting a tree and stamping its modification times, for tests that hold a build script's
//! rerun watch against a MODEL of cargo's rerun-if-changed rule: `vike-user-strategies`' and
//! `vike-user-research`'s generator tests, which carried these as identical private copies, and
//! `crates/vike-model/tests/host_build.rs`, which tests the shared watch itself. [`watched`] and
//! [`reruns`] read a directive set back; the two hosts had spelled `reruns` two ways.
//!
//! ⚠ [`age`] takes the instant it stamps as a PARAMETER. Both copies computed it from the clock
//! (two hours before now), and this module is read as production code by the clock ratchet, so the
//! caller supplies the instant and the read stays in the test.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Write the marker file `root/rel` — the bytes `"x\n"` — creating every missing parent first.
/// A write is what moves a watched tree's newest mtime past a previous build's start.
pub fn write_marker(root: &Path, rel: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, "x\n").unwrap();
}

/// Set every mtime at or under `path` to `then`, links excluded: the tree as it stood before the
/// previous run started. A path that cannot be stat'ed is left alone; children are stamped before
/// their directory, so the directory's own stamp is the last write and stays `then`.
///
/// ⚠ Unix-shaped: it opens each entry, directories included, to stamp it, and Windows refuses
/// `File::open` on a directory. Every caller's case is `#[cfg(unix)]`.
pub fn age(path: &Path, then: SystemTime) {
    let Ok(meta) = fs::symlink_metadata(path) else { return };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        for e in fs::read_dir(path).unwrap().flatten() {
            age(&e.path(), then);
        }
    }
    fs::File::open(path).and_then(|f| f.set_modified(then)).unwrap();
}

/// `cargo_util::paths::mtime_recursive`, MODELLED: the newest mtime at or under `path`, following
/// symlinks and skipping whatever cannot be stat'ed — `None` only when `path` itself cannot be, which
/// cargo reports as `MissingFile`. A model, not cargo: the lane check in
/// `crates/vike-model/src/host_build/watch.rs`'s module doc is what proves the real tool behaves this
/// way.
pub fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let meta = fs::metadata(path).ok()?;
    let mut newest = meta.modified().ok()?;
    if let Ok(link) = fs::symlink_metadata(path)
        && link.file_type().is_symlink()
        && let Ok(t) = link.modified()
    {
        newest = newest.max(t);
    }
    if meta.is_dir()
        && let Ok(entries) = fs::read_dir(path)
    {
        for e in entries.flatten() {
            if let Some(t) = newest_mtime(&e.path()) {
                newest = newest.max(t);
            }
        }
    }
    Some(newest)
}

/// The paths a set of `cargo:rerun-if-changed=` directives names, in order. Panics on a line that
/// is not one: every caller hands it `crate::host_build::watch::rerun_directives`' output.
pub fn watched(lines: &[String]) -> Vec<PathBuf> {
    lines
        .iter()
        .map(|l| PathBuf::from(l.strip_prefix("cargo:rerun-if-changed=").expect("a directive")))
        .collect()
}

/// cargo's verdict over a directive set, under the model: the build script re-runs when ANY
/// watched path is missing, or holds anything newer than `started`, the previous run's start. The
/// newest-mtime half is [`newest_mtime`], a MODEL of `cargo_util::paths::mtime_recursive`, not
/// cargo: the lane check in `crates/vike-model/src/host_build/watch.rs`'s module doc is what proves
/// the real tool behaves this way.
pub fn reruns(lines: &[String], started: SystemTime) -> bool {
    watched(lines).iter().any(|w| newest_mtime(w).is_none_or(|t| t > started))
}
