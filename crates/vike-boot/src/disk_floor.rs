//! **The FREE-SPACE FLOOR the OPTIONAL writers check before they write** (REVIEW-LIVE-MONEY H1).
//!
//! A trading node's journal, its `vike.db` and every optional output share one filesystem, and the live core
//! cannot trade on a full one: the journal's append touches no file (decision 0279), its write thread retries a
//! failed write without end, and an order whose record it cannot write within `vike_journal`'s fence bound is
//! refused NOT SENT and HALTS the node (cancels and covered exits still leave; decision 0279's addendum of
//! 2026-11-23), so every demo and live mount of the node stops opening risk. The optional writers are the
//! ones whose output is worth keeping but never worth that: the backtesthub's saved runs, the datahub's
//! live-bar and text recorders and the execution stream's forecast rows, the tradehub's forward runs. Each asks
//! [`DiskFloor::refusal`] before it writes; below the floor it writes nothing and says so in words, and the
//! next write past the floor goes ahead by itself. The journal's own behaviour is untouched.
//!
//! - **The floor** is `max(config.optional_writes_min_free_mib, MIN_FREE_PERCENT of the filesystem's size)`
//!   free ([`DiskFloor::of`]; the row's default is `vike_config::config::DEFAULT_OPTIONAL_WRITES_MIN_FREE_MIB`,
//!   whose doc says why 10 GiB), so a large disk keeps a share in proportion and a small one a fixed reserve.
//! - **Free** is the NON-privileged free space ([`filesystem_space`]: `f_bavail`, never the root-reserved
//!   blocks a daemon cannot write into), of the nearest directory of the target that exists (a runs root not
//!   created yet is measured on the filesystem it will be created on).
//! - **A filesystem that cannot be measured is not a full one**: [`DiskFloor::refusal`] answers `None` and
//!   the writer writes as before. On unix that is a failed `statvfs`; off unix (no daemon ships there) it is
//!   every call.
//!
//! This is the ONE statvfs site of the workspace: the tradehub's startup preflight measures its journal
//! directory through [`filesystem_space`] too.

use std::path::Path;

/// The share of the filesystem's SIZE, in percent, the floor never goes below.
pub const MIN_FREE_PERCENT: u64 = 5;

/// One look at a filesystem: what a non-privileged writer may still use, and its whole size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemSpace {
    /// Free bytes a normal user may write (`f_bavail · f_frsize`).
    pub available_bytes: u64,
    /// The filesystem's size in bytes (`f_blocks · f_frsize`).
    pub total_bytes: u64,
}

/// The space of the filesystem holding `dir`, which must exist.
///
/// ⚠ On unix this is `rustix::fs::statvfs`, a SAFE wrapper (no `unsafe`, no `UNSAFE_EXEMPT` carve-out in an
/// order-signing binary). `f_bavail` is the NON-privileged free space, not `f_bfree`, whose root-reserved
/// blocks a normal-user daemon can never write into.
///
/// # Errors
/// The syscall's failure in words (an absent directory included).
#[cfg(unix)]
pub fn filesystem_space(dir: &Path) -> Result<FilesystemSpace, String> {
    let stat = rustix::fs::statvfs(dir).map_err(|e| format!("statvfs failed: {e}"))?;
    Ok(FilesystemSpace {
        available_bytes: stat.f_bavail.saturating_mul(stat.f_frsize),
        total_bytes: stat.f_blocks.saturating_mul(stat.f_frsize),
    })
}

/// The non-unix half of [`filesystem_space`]: it declares rather than measures (`GetDiskFreeSpaceExW` has no
/// dependency-free safe route, and both shipped daemons run on Linux).
///
/// # Errors
/// Always, with the reason.
#[cfg(not(unix))]
pub fn filesystem_space(_dir: &Path) -> Result<FilesystemSpace, String> {
    Err("free space is not queried on this platform (rustix's statvfs is POSIX-only, and the Windows \
         call has no dependency-free safe route); it is measured on unix, where both shipped daemons run"
        .to_string())
}

/// The optional writers' free-space floor: an absolute reserve, raised to [`MIN_FREE_PERCENT`] of a large
/// filesystem. Built once per daemon from its settings ([`Self::of`]) and asked before each write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskFloor {
    min_free_bytes: u64,
}

impl DiskFloor {
    /// The floor `config.optional_writes_min_free_mib` names, its default when the row is absent.
    #[must_use]
    pub fn of(config: &vike_config::Config) -> Self {
        let mib = config
            .optional_writes_min_free_mib
            .unwrap_or(vike_config::config::DEFAULT_OPTIONAL_WRITES_MIN_FREE_MIB);
        DiskFloor::of_mib(mib)
    }

    /// A floor of `mib` MiB (and the percent share).
    #[must_use]
    pub fn of_mib(mib: u32) -> Self {
        DiskFloor { min_free_bytes: u64::from(mib) << 20 }
    }

    /// The free bytes a filesystem of `total_bytes` must keep: the reserve, or the percent share when larger.
    #[must_use]
    pub fn floor_bytes(self, total_bytes: u64) -> u64 {
        self.min_free_bytes.max(total_bytes / 100 * MIN_FREE_PERCENT)
    }

    /// Why an optional write under `dir` must not happen now (its filesystem is below the floor), in words;
    /// `None` when there is room or the filesystem cannot be measured (the module doc).
    #[must_use]
    pub fn refusal(self, dir: &Path) -> Option<String> {
        self.refusal_with(dir, filesystem_space)
    }

    /// [`Self::refusal`] over the space `measure` reports, the seam a test hands a fake reader.
    pub fn refusal_with(
        self,
        dir: &Path,
        measure: impl Fn(&Path) -> Result<FilesystemSpace, String>,
    ) -> Option<String> {
        let existing = dir.ancestors().find(|p| !p.as_os_str().is_empty() && p.exists())?;
        let space = measure(existing).ok()?;
        let floor = self.floor_bytes(space.total_bytes);
        (space.available_bytes < floor).then(|| {
            format!(
                "the filesystem holding {} has {} MiB free, below the optional writers' floor of {} MiB \
                 (config.optional_writes_min_free_mib, or {MIN_FREE_PERCENT}% of its {} MiB when larger): \
                 nothing optional is written until there is room, so the journal and vike.db keep theirs",
                existing.display(),
                space.available_bytes >> 20,
                floor >> 20,
                space.total_bytes >> 20,
            )
        })
    }
}

#[cfg(test)]
#[path = "disk_floor_tests.rs"]
mod disk_floor_tests;
