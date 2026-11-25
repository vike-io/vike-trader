//! The SHIPPED strategy files and the ONE function that seeds them into a project.
//!
//! A release carries its strategy files as one asset, unpacked whole to
//! `<project>/vendor/vike-strategies/` (read-only, replaced by every release). The files a user runs
//! live in `<project>/strategies/`, and a shipped file gets there by [`seed_shipped_strategies`]. It
//! judges each shipped file against its REFERENCE, the same path under
//! `<project>/vendor/vike-strategies-seeded/`: the shipped files as this box last seeded them (the
//! owner's ruling of 2026-11-16, decision 0281's amendment):
//!
//! | the file in `strategies/` | the seed |
//! |---|---|
//! | absent | copies the shipped file in, with `create_new` |
//! | byte-identical to its reference (the operator never edited it) | replaces it with the shipped file |
//! | anything else (edited, or this box keeps no reference for it) | leaves it as it is and writes the shipped file beside it as `<name>.new` ([`NEW_VERSION_SUFFIX`]); nothing when the shipped file IS its reference (no new version came) or the file already equals it |
//! | not a shipped path (a file the operator added) | never read, never touched |
//!
//! The reference then takes the shipped file, so the next release is judged against what this one
//! left. A replace, a `.new` and a reference write each go through ONE atomic replace
//! (`vike_utility::atomic_file::write_atomic`): a reader sees the old bytes or the new ones. The seed
//! deletes nothing. Every way the reference can be missing or wrong (never laid down, half written)
//! lands in the `.new` row, never in a replace: no file is overwritten blind.
//!
//! Every installer calls this one function and no other copy exists: `vike-cli init` (and its
//! `--strategies-only`, which `deploy/docker/vikectl` and `deploy/docker/entrypoint.sh` run).
//!
//! [`differs_from_shipped`] is the lister's `edited` mark: a file whose shipped twin exists and
//! holds other bytes. It is information only; nothing acts on it.
//!
//! This module and the strategy-file scan are the crate's only filesystem I/O; the strategies
//! themselves stay pure.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The suffix of the shipped version written BESIDE a file the seed may not replace:
/// `trend/momentum.rs` gets `trend/momentum.rs.new`. No strategy reader loads it: a file's language
/// is its extension (`crate::file::scan::language_of`), and `new` is neither `rs` nor `rhai`.
pub const NEW_VERSION_SUFFIX: &str = ".new";

/// What one seed did, as paths RELATIVE to the strategies folder, sorted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Absent before; copied from the shipped tree (or, under a dry run, would be).
    pub copied: Vec<PathBuf>,
    /// Byte-identical to its reference, so never edited here: replaced with the shipped file.
    pub replaced: Vec<PathBuf>,
    /// Neither the shipped file nor its reference (edited, or no reference is kept): left as it is,
    /// with the shipped file written beside it as `<name>.new`.
    pub new_version_beside: Vec<PathBuf>,
    /// Present and left exactly as it is: as shipped, edited with no new version shipped, or not
    /// a plain file (a folder or a link is never written through).
    pub kept: Vec<PathBuf>,
}

/// Seed every shipped file under `shipped` into `strategies` at the same relative path by the
/// module doc's table, judging each against its twin under `reference` and then updating that twin
/// to the shipped file. `None` when this box has no shipped tree (nothing to seed).
///
/// A name starting with `.` is skipped (the deploy's `.release-asset` marker, a staging sibling),
/// and so is anything that is not a plain file or directory: a symbolic link is never followed,
/// so a planted link cannot copy a file from outside the tree. `dry_run` writes nothing, the
/// reference included, and reports what a real run would do. A file this run created and could not
/// finish writing is removed, so a full disk never leaves a truncated strategy that a later seed
/// would keep.
pub fn seed_shipped_strategies(
    shipped: &Path,
    reference: &Path,
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
        let source = shipped.join(&relative);
        let new = std::fs::read(&source).map_err(with_path(&source))?;
        let twin = reference.join(&relative);
        let previous = match on_disk(&twin)? {
            OnDisk::File(bytes) => Some(bytes),
            OnDisk::Absent | OnDisk::Other => None,
        };
        let dest = strategies.join(&relative);
        let list = match on_disk(&dest)? {
            OnDisk::Absent => {
                // `false`: something appeared there first, and it is left as it is.
                if dry_run || create(&dest, &new)? { &mut report.copied } else { &mut report.kept }
            }
            OnDisk::File(mine) if mine != new && previous.as_ref() == Some(&mine) => {
                if !dry_run {
                    vike_utility::atomic_file::write_atomic(&dest, &new)
                        .map_err(with_path(&dest))?;
                }
                &mut report.replaced
            }
            OnDisk::File(mine) if mine != new && previous.as_ref() != Some(&new) => {
                if !dry_run {
                    let beside = new_version_path(&dest);
                    vike_utility::atomic_file::write_atomic(&beside, &new)
                        .map_err(with_path(&beside))?;
                }
                &mut report.new_version_beside
            }
            // As shipped, edited with no new version shipped, or a folder or a link.
            OnDisk::File(_) | OnDisk::Other => &mut report.kept,
        };
        list.push(relative);
        // Written only when it moves, so a seed with nothing new writes nothing (a read-only mount).
        if !dry_run && previous.as_ref() != Some(&new) {
            if let Some(parent) = twin.parent() {
                std::fs::create_dir_all(parent).map_err(with_path(parent))?;
            }
            vike_utility::atomic_file::write_atomic(&twin, &new).map_err(with_path(&twin))?;
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

/// What a path holds, read without following a link.
enum OnDisk {
    Absent,
    /// A folder, a link or anything else that is not a plain file.
    Other,
    File(Vec<u8>),
}

fn on_disk(path: &Path) -> io::Result<OnDisk> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(OnDisk::Absent),
        Err(e) => Err(with_path(path)(e)),
        Ok(meta) if meta.is_file() => {
            std::fs::read(path).map(OnDisk::File).map_err(with_path(path))
        }
        Ok(_) => Ok(OnDisk::Other),
    }
}

/// Create `dest` holding `bytes` with `create_new`: `false` when something is already there.
fn create(dest: &Path, bytes: &[u8]) -> io::Result<bool> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(with_path(parent))?;
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(dest) {
        Ok(mut out) => {
            if let Err(e) = out.write_all(bytes).and_then(|()| out.sync_all()) {
                drop(out);
                let _ = std::fs::remove_file(dest);
                return Err(with_path(dest)(e));
            }
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(with_path(dest)(e)),
    }
}

/// `dest` with [`NEW_VERSION_SUFFIX`] appended to its file name.
fn new_version_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(NEW_VERSION_SUFFIX);
    PathBuf::from(name)
}

/// An I/O error that names the path it happened on.
fn with_path(path: &Path) -> impl FnOnce(io::Error) -> io::Error + '_ {
    move |e| io::Error::new(e.kind(), format!("{}: {e}", path.display()))
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
