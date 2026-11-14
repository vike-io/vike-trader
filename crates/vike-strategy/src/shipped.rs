//! The SHIPPED strategy files and the ONE function that seeds them into a project.
//!
//! A release carries its strategy files as one asset, unpacked whole to
//! `<project>/vendor/vike-strategies/` (read-only, replaced by every release). The files a user runs
//! live in `<project>/strategies/`, and a shipped file gets there by [`seed_shipped_strategies`]:
//! each file of the shipped tree is copied to the same relative path with `create_new`, so a file
//! that already exists, edited or not, is never touched. After the copy the file is the user's.
//!
//! Every installer calls this one function and no other copy exists: `vike-cli init` (and its
//! `--strategies-only`, which `deploy/docker/vikectl`, `deploy/sbin/vike-trader-ci-deploy` and
//! `deploy/docker/entrypoint.sh` run).
//!
//! [`differs_from_shipped`] is the lister's `edited` mark: a file whose shipped twin exists and
//! holds other bytes. It is information only; nothing acts on it.
//!
//! This module and the strategy-file scan are the crate's only filesystem I/O; the strategies
//! themselves stay pure.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// What one seed did, as paths RELATIVE to the strategies folder, sorted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Absent before; copied from the shipped tree (or, under a dry run, would be).
    pub copied: Vec<PathBuf>,
    /// Already present; left exactly as it is, whatever it holds.
    pub kept: Vec<PathBuf>,
}

/// Copy every shipped file under `shipped` into `strategies` at the same relative path, creating
/// only what is absent. `None` when this box has no shipped tree (nothing to seed).
///
/// A name starting with `.` is skipped (the deploy's `.release-asset` marker, a staging sibling),
/// and so is anything that is not a plain file or directory: a symbolic link is never followed,
/// so a planted link cannot copy a file from outside the tree. `dry_run` writes nothing and
/// reports what a real run would copy. A file this run created and could not finish writing is
/// removed, so a full disk never leaves a truncated strategy that a later seed would keep.
pub fn seed_shipped_strategies(
    shipped: &Path,
    strategies: &Path,
    dry_run: bool,
) -> io::Result<Option<SeedReport>> {
    if !shipped.is_dir() {
        return Ok(None);
    }
    let mut files = Vec::new();
    walk(shipped, Path::new(""), &mut files)?;
    files.sort();
    let mut report = SeedReport::default();
    for relative in files {
        let dest = strategies.join(&relative);
        if dry_run {
            if dest.exists() {
                report.kept.push(relative)
            } else {
                report.copied.push(relative)
            }
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&dest) {
            Ok(mut out) => {
                let written = std::fs::read(shipped.join(&relative))
                    .and_then(|bytes| out.write_all(&bytes))
                    .and_then(|()| out.sync_all());
                if let Err(e) = written {
                    drop(out);
                    let _ = std::fs::remove_file(&dest);
                    return Err(io::Error::new(e.kind(), format!("{}: {e}", dest.display())));
                }
                report.copied.push(relative);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => report.kept.push(relative),
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", dest.display()))),
        }
    }
    Ok(Some(report))
}

/// The lister's `edited` mark: `relative` (a path under `strategies`) has a shipped twin under
/// `shipped` and its bytes differ. A file with no twin, or one either side of which cannot be
/// read, is not marked: the mark is information, never a judgement.
pub fn differs_from_shipped(shipped: &Path, strategies: &Path, relative: &Path) -> bool {
    match (std::fs::read(shipped.join(relative)), std::fs::read(strategies.join(relative))) {
        (Ok(twin), Ok(mine)) => twin != mine,
        _ => false,
    }
}

/// Every plain file under `root/at`, as paths relative to `root`.
fn walk(root: &Path, at: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(root.join(at))? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        // `file_type` does not follow a symbolic link, so a link is neither a file nor a dir here.
        let kind = entry.file_type()?;
        let relative = at.join(entry.file_name());
        if kind.is_dir() {
            walk(root, &relative, out)?;
        } else if kind.is_file() {
            out.push(relative);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "shipped_tests.rs"]
mod shipped_tests;
