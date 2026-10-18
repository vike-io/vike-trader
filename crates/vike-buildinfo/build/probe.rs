//! The git and `rustc` probes, each degrading to `UNKNOWN`; "the module doc" below is `build.rs`'s.

use std::path::Path;
use std::process::Command;

use super::UNKNOWN;

/// The two facts git answers — `(sha, dirty)` — for a build that is NOT frozen. `main` reads the
/// build time beside them, because this module reads no clock (`build.rs`'s `mod` lines say why).
pub(super) fn probe_git(manifest_dir: &Path) -> (String, Option<bool>) {
    let sha = git(manifest_dir, &["rev-parse", "--short", "HEAD"])
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| UNKNOWN.to_string());

    // `--no-optional-locks`: see the module doc. Empty output ⇒ clean; a failed probe ⇒ we do not
    // know, which is a THIRD answer and is reported as one rather than rounded to "clean".
    //
    // ⚠ UNTRACKED files count as dirty (plain `--porcelain`, not `--untracked-files=no`). An
    // untracked-but-unignored `.rs` file IS part of the build, and over-reporting dirty is the safe
    // direction here for the same reason over-redaction is elsewhere: a false `dirty` costs one
    // `git status`, a false `clean` costs an unidentifiable binary on a trading box.
    let dirty = git(manifest_dir, &["--no-optional-locks", "status", "--porcelain"])
        .map(|out| !out.trim().is_empty());

    (sha, dirty)
}

/// `rustc --version` of the compiler cargo is invoking, trimmed, or [`UNKNOWN`] when it would not
/// run, failed or printed nothing. `rustc` is the `$RUSTC` value `main` read.
pub(super) fn rustc_version(rustc: &str) -> String {
    Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| UNKNOWN.to_string())
}

/// Run `git` from the package directory and return its trimmed stdout, or `None` for any failure —
/// git absent, not a repository, a non-zero status, non-UTF-8 output.
///
/// The working directory is named EXPLICITLY rather than inherited. A build script's cwd is the
/// package root today, and a probe whose answer depends on an unstated working directory is the
/// exact shape of the incident in the module doc.
pub(super) fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
