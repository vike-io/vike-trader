//! How a host's build script WATCHES what its scan reads — only that, and without re-running on
//! every build while it is absent.
//!
//! ## The two defects it cures
//!
//! One `cargo:rerun-if-changed=<user_data>` line used to re-run both build scripts (recompiling
//! eight crates and relinking ~120 test binaries):
//!
//! * on EVERY build while user_data was ABSENT (every fresh clone, most CI runners): cargo's
//!   `find_stale_file` answers `StaleItem::MissingFile` when `cargo_util::paths::mtime_recursive`
//!   cannot stat the path;
//! * after every write ANYWHERE in a PRESENT user_data, watched recursively — MEASURED on CI
//!   runners and lanes, from tests writing `user_data/runs/`.
//!
//! ## The cure, in two halves
//!
//! **Watch exactly the directories the scan READS** (the host's `scanned_dirs`, handed to
//! [`crate::host_build::driver::Host`]), never the root.
//!
//! **Watch an ABSENT one through a symlink in OUT_DIR**: a stand-in directory (mirroring its path
//! below user_data) holding one link to it. The stand-in EXISTS, and `mtime_recursive` FOLLOWS
//! links and SKIPS what it cannot stat, so a dangling link stays fresh while the directory is
//! absent and its appearance re-runs the script: `vike-cli init` creating `user_data/` after the
//! build still works (`crates/vike-cli/src/cmd/init/content/readmes.rs`'s `RUST_README` promises
//! "after the next `cargo build`"). The lane check below proves both on the real cargo. Nothing
//! outside OUT_DIR is written (the cargo book: a build script "should not modify any files outside
//! of that directory").
//!
//! ## Why a PRESENT directory is still watched directly
//!
//! Through the link, a DELETED tier would be skipped: no re-run, and the registry's `#[path]`s
//! would fail to compile. Watched directly, deletion is `MissingFile`: one re-run, which
//! regenerates the empty registry and moves the watch onto the link.
//!
//! ## Why the watch directory's mtime is set back
//!
//! Cargo compares against the time the script STARTED, so a link created DURING a run would re-run
//! the next build once more. [`rerun_path`] sets the stand-in's mtime to `not_after` (the driver
//! passes the build script's own executable's mtime). Best-effort: a refusal costs one extra re-run.
//!
//! ## A non-Unix host keeps the direct watch
//!
//! Off Unix [`rerun_path`] answers the directory itself: slow while absent, never stale. No Windows
//! test runs anywhere, an untested build-script branch is a worse bet than a slow one, and every CI
//! runner and lane is Linux. The NARROWING holds there too.
//!
//! ## ⚠ The lane check that proves it on the REAL cargo
//!
//! `crates/vike-model/tests/host_build.rs` holds a MODEL of cargo's rule
//! (`crate::test_support::mtime::newest_mtime`), not cargo. In a Linux checkout, build twice in a
//! row with cargo's fingerprint log on, in each of two states: no `user_data/` at all (or the
//! build-time override naming a path that does not exist), and a `user_data/` holding only `runs/`,
//! with a new file written under `runs/` between the two builds — the CI-runner case:
//!
//! ```text
//! CARGO_LOG=cargo::core::compiler::fingerprint=info cargo build -v -p vike-user-strategies -p vike-user-research
//! ```
//!
//! The SECOND build must report `Fresh` for both crates and carry no `dirty` line for either build
//! script; before this mechanism existed, every build did.
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// The directory under OUT_DIR that holds one stand-in per ABSENT watched directory, at that
/// directory's own path below user_data (`user_data_watch/strategies/rust/`, …).
pub const WATCH_DIR: &str = "user_data_watch";

/// The symlink inside each stand-in that points at the absent directory.
pub const WATCH_LINK: &str = "root";

/// The `cargo:rerun-if-changed=` lines `build.rs` prints, one [`rerun_path`] answer per
/// directory in `scanned`, so the tests hold the emitted DIRECTIVES themselves.
///
/// `root` (the user_data directory) and every `scanned` directory under it must be ABSOLUTE: a
/// symlink's relative target resolves against the link's own directory, OUT_DIR, not the package.
pub fn rerun_directives(
    root: &Path,
    scanned: &[PathBuf],
    out_dir: &Path,
    not_after: SystemTime,
) -> Vec<String> {
    scanned
        .iter()
        .map(|dir| {
            let watched = rerun_path(root, dir, out_dir, not_after);
            format!("cargo:rerun-if-changed={}", watched.display())
        })
        .collect()
}

/// The path cargo is told to watch for one scanned directory `dir` under user_data `root`.
///
/// * `dir` exists — `dir` itself: cargo scans it recursively.
/// * `dir` is absent on a Unix host — `<out_dir>/`[`WATCH_DIR`]`/<dir below root>`, a directory
///   holding a symlink to `dir`, its mtime set to `not_after`. The answer always EXISTS.
/// * otherwise (a non-Unix host, a `dir` not strictly below `root`, a refusing filesystem) — `dir`
///   itself: re-runs every build while absent, never stale.
pub fn rerun_path(root: &Path, dir: &Path, out_dir: &Path, not_after: SystemTime) -> PathBuf {
    if dir.exists() {
        return dir.to_path_buf();
    }
    let Some(stand_in) = stand_in(root, dir, out_dir) else { return dir.to_path_buf() };
    match make_link(dir, &stand_in, &stand_in.join(WATCH_LINK), not_after) {
        Ok(()) => stand_in,
        Err(_) => dir.to_path_buf(),
    }
}

/// `<out_dir>/user_data_watch/<dir below root>` — `None` unless `dir` is STRICTLY below `root`
/// through plain components, so no answer can escape OUT_DIR (an absolute or `..` remainder would).
fn stand_in(root: &Path, dir: &Path, out_dir: &Path) -> Option<PathBuf> {
    let below = dir.strip_prefix(root).ok()?;
    let plain = below.components().all(|c| matches!(c, Component::Normal(_)));
    (plain && below.components().next().is_some()).then(|| out_dir.join(WATCH_DIR).join(below))
}

#[cfg(unix)]
fn make_link(target: &Path, stand_in: &Path, link: &Path, not_after: SystemTime) -> io::Result<()> {
    // OUT_DIR persists between runs: a link already pointing at `target` is left alone, re-pointed
    // only when the target moved (the build-time override changed).
    if std::fs::read_link(link).ok().as_deref() != Some(target) {
        std::fs::create_dir_all(stand_in)?;
        if let Err(e) = std::fs::remove_file(link)
            && e.kind() != io::ErrorKind::NotFound
        {
            return Err(e);
        }
        std::os::unix::fs::symlink(target, link)?;
    }
    // Best-effort, deliberately: see "Why the watch directory's mtime is set back" above.
    let _ = std::fs::File::open(stand_in).and_then(|d| d.set_modified(not_after));
    Ok(())
}

#[cfg(not(unix))]
fn make_link(_: &Path, _: &Path, _: &Path, _not_after: SystemTime) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}
