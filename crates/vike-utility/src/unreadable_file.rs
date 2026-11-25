//! **A state file that exists and cannot be read is SET ASIDE, never written over and never
//! deleted**: [`set_aside`] renames it beside itself, so the next save of that state starts a new
//! file and the bytes nobody could read survive for a person to look at. Its read is
//! `crate::atomic_file::read_if_present`, where only "not found" is absent.
//!
//! The new name is `<file name>.unreadable-<YYYYMMDD>T<HHMMSS>Z` in the same directory, dated by the
//! file's OWN last change (UTC). That date is read off the file, not off a clock: this crate is under
//! the clock ratchet (`crates/vike-ops/tests/architecture/clock_pin.rs`'s
//! `DETERMINISM_CRITICAL_CRATES`), and the caller's ERROR line carries when it was set aside. A name
//! already taken gets `-2`, `-3`, ...: a rename onto an existing name REPLACES that file on unix and
//! fails on Windows, and an earlier copy must survive either way.
//!
//! What a caller does around it is its own: the words of its ERROR line, and whether anything writes
//! its file again in that run.

use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// How many numbered names [`set_aside`] tries after the dated one before it gives up.
const FREE_NAME_TRIES: u32 = 1000;

/// Rename the file (or whatever stands) at `path` to a free `<file name>.unreadable-<date>[-n]`
/// beside it, and answer that new path. `Err` leaves `path` as it was: nothing at `path` (its
/// `NotFound`), a path that names no file (`InvalidInput`), no free name (`AlreadyExists`), or the
/// rename's own error.
pub fn set_aside(path: &Path) -> io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, format!("{} names no file", path.display()))
    })?;
    // A file whose time cannot be read is dated 1970-01-01: the name is still free and unique.
    let seconds = std::fs::symlink_metadata(path)?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_secs());
    let (year, month, day) = crate::calendar::civil_from_days((seconds / 86_400) as i64);
    let of_day = seconds % 86_400;
    let stamp = format!(
        ".unreadable-{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    );
    for n in 1..=FREE_NAME_TRIES {
        let mut name = file_name.to_owned();
        name.push(&stamp);
        if n > 1 {
            name.push(format!("-{n}"));
        }
        let aside = path.with_file_name(name);
        // shortcut: checked, then renamed, not atomic; one process owns each state file, so no second
        // writer can take the name in between. Upgrade to a no-replace rename if two ever share one.
        match std::fs::symlink_metadata(&aside) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::rename(path, &aside)?;
                return Ok(aside);
            }
            Err(e) => return Err(e),
            Ok(_) => {}
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} has no free set-aside name left", path.display()),
    ))
}

#[cfg(test)]
#[path = "unreadable_file_tests.rs"]
mod tests;
