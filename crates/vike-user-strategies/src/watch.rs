// How `build.rs` WATCHES what its scan reads — only that, and without re-running on every build
// while it is absent.
//
// `build.rs` `include!`s this file inside a `mod watch { … }`, and `lib.rs` compiles it as
// `pub mod watch` — the `codegen.rs` precedent: the tests drive exactly the code the build runs.
// It is byte-identical in `crates/vike-user-strategies/src/watch.rs` and
// `crates/vike-user-research/src/watch.rs`, and
// `crates/vike-user-research/tests/gen_unit.rs`'s `the_watch_module_is_byte_identical_in_both_hosts`
// holds the two equal.
//
// ## The two defects it cures
//
// Both build scripts printed ONE `cargo:rerun-if-changed=<user_data>` line, for the whole tree.
//
// * **An absent user_data re-ran them on EVERY build.** A path cargo cannot stat is not "unchanged"
//   to cargo, it is STALE: the build-script fingerprint check (`find_stale_file`, cargo's
//   `core/compiler/fingerprint` module) answers `StaleItem::MissingFile` whenever
//   `cargo_util::paths::mtime_recursive` fails, and that fails on its very first `metadata` call
//   for a path that does not exist. So in every checkout without `user_data/` — every fresh clone,
//   most CI runners — both scripts re-ran on every cargo invocation, rewrote their OUT_DIR, and
//   recompiled every crate built on them (eight, plus ~120 test binaries relinked) to produce output
//   that never changed.
// * **A PRESENT user_data re-ran them after every write ANYWHERE in it.** Cargo watches a directory
//   recursively, and a scan reads one or two tiers of a tree that also holds `runs/`, `logs/`,
//   `backtest_results/` and notebooks — all written by the program and by tests. MEASURED: the CI
//   runner checkouts and the verification lanes that carry a `user_data/` got it from tests writing
//   `user_data/runs/`, so on exactly those boxes the same eight-crate recompile came back after any
//   such write.
//
// ## The cure, in two halves
//
// **Watch exactly the directories the scan READS** — each host's `codegen::scanned_dirs` names
// them beside the `scan` that reads them — one `rerun-if-changed` line each, never the user_data
// root.
//
// **Watch an ABSENT one through a symlink in OUT_DIR.** `rerun_path` answers the directory itself
// while it exists, and otherwise a directory under OUT_DIR — one per watched directory, mirroring
// its path below user_data — holding one symlink that points at it. That directory EXISTS, so
// cargo never reports it missing, and two properties of `mtime_recursive` — both stated in its own
// source comments, and both proven by the lane check in `crates/vike-user-strategies/build.rs`'s
// module doc — do the rest:
//
// * it walks a directory FOLLOWING symlinks and SKIPS an entry it cannot stat ("Ignore errors
//   while walking"), so a dangling link contributes nothing and the watch stays fresh for as long
//   as the directory stays absent — whether user_data itself is absent or only the tier is;
// * once the directory appears the link resolves, the walk descends into it, and its newer mtime
//   re-runs the script — exactly when a direct watch re-runs on appearance. So the first-run path a
//   new user actually takes keeps working with no step added: `vike-cli init` creates `user_data/`
//   AFTER the build that compiled it, and `crates/vike-cli/src/cmd/init/content.rs`'s
//   `RUST_README` promises a strategy resolves "after the next `cargo build`".
//
// Nothing outside OUT_DIR is written. The cargo book's build-script chapter says a script "should
// not modify any files outside of that directory", which is why creating the absent directories
// from here was rejected even though user_data is gitignored.
//
// ## Why a PRESENT directory is still watched directly
//
// The generated registry `#[path]`-includes every entry file the scan found. Watched through the
// link, a DELETED tier would leave a dangling link the walk skips: the script would not re-run, and
// the library would then fail to compile against `#[path]`s that no longer exist. Watched directly,
// a deleted tier is `MissingFile` — one re-run, which regenerates the empty registry and moves the
// watch onto the link.
//
// ## Why the watch directory's mtime is set back
//
// Cargo compares a watched path against the time the script STARTED (it stamps the run's output
// file with the invocation time, not the finish time), so a link created DURING a run would make
// the NEXT build re-run once more. `rerun_path` therefore sets the directory's mtime to its
// `not_after` argument, and `build.rs` passes its own executable's mtime, which necessarily
// predates the run. Best-effort: a filesystem that refuses costs that one extra re-run, nothing
// else.
//
// ## A non-Unix host keeps the direct watch
//
// The link is made only on a Unix host; elsewhere `rerun_path` answers the directory itself — slow
// while it is absent, never stale. No Windows test runs anywhere in this workspace, and an untested
// branch in a build script is a worse bet than a slow one; every CI runner and every verification
// lane is Linux, which is where the cost was. The NARROWING is not Unix-only: a Windows host
// watches the same scanned directories, so a write to `runs/` re-runs nothing there either.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// The directory under OUT_DIR that holds one stand-in per ABSENT watched directory, at that
/// directory's own path below user_data (`user_data_watch/strategies/rust/`, …).
pub const WATCH_DIR: &str = "user_data_watch";

/// The symlink inside each stand-in that points at the absent directory.
pub const WATCH_LINK: &str = "root";

/// The `cargo:rerun-if-changed=` lines `build.rs` prints — one per directory in `scanned`, each a
/// [`rerun_path`] answer, verbatim, so the tests hold the emitted DIRECTIVES rather than values one
/// step before them.
///
/// `root` is the user_data directory and `scanned` the directories the scan reads, each under
/// `root`; all of them must be ABSOLUTE, because a symlink's relative target resolves against the
/// link's own directory, which is OUT_DIR, not the package.
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
///   holding a symlink to `dir` whose own mtime is set to `not_after`. The answer always EXISTS.
/// * `dir` is absent and that cannot be arranged (a non-Unix host, a `dir` that is not strictly
///   below `root`, or a filesystem that refused) — `dir` itself: re-runs on every build while
///   absent, but is never stale.
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
/// through plain components, so no answer can ever escape OUT_DIR (an absolute or `..` remainder
/// joined onto OUT_DIR would point somewhere else entirely).
fn stand_in(root: &Path, dir: &Path, out_dir: &Path) -> Option<PathBuf> {
    let below = dir.strip_prefix(root).ok()?;
    let plain = below.components().all(|c| matches!(c, Component::Normal(_)));
    (plain && below.components().next().is_some()).then(|| out_dir.join(WATCH_DIR).join(below))
}

#[cfg(unix)]
fn make_link(target: &Path, stand_in: &Path, link: &Path, not_after: SystemTime) -> io::Result<()> {
    // OUT_DIR persists between runs, so a link already pointing at `target` is left alone; it is
    // re-pointed only when the target itself moved (the build-time override changed).
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
