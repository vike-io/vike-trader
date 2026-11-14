//! **The DURABLE COPY of a live segment, and the repair a boot runs from it.**
//!
//! The asynchronous journal, option C (decision 0279, the owner,
//! 2026-11-12). The WORKING segment (`journal-{idx}.vjl`, the file every reader reads, its format
//! unchanged) is written by the write thread and is NEVER synced while it is live: on btrfs a sync
//! holds the file's lock, and an order waiting on the write thread would wait behind it. What a power
//! cut must not lose goes to a second file instead, `journal-{idx}.vjl.dur`, which the SYNC thread
//! writes from the same stage bytes, at the same offsets, and syncs on the cadence the working segment
//! used to be synced on (a chunk, a snap). The copy of a segment `open` resumed holds nothing below the
//! resume point (a hole): those bytes were already durable in the working file.
//!
//! The `.dur` suffix is load-bearing exactly as `.prep`'s is: every segment scanner resolves an index
//! with `strip_suffix(".vjl")`, so a copy is invisible to all of them.
//!
//! # Lifecycle, and what a crash leaves at each step
//!
//! * **Live:** the sync thread creates the copy when the segment becomes the core's (at `open` and at a
//!   roll), syncs the directory once so its NAME survives, and appends + syncs on each request. A crash
//!   leaves the copy with every frame up to the last sync (a torn last frame at worst).
//! * **Roll:** when the write thread leaves a segment, the sync thread syncs the WORKING file whole, then
//!   deletes the copy. Crash before that sync: the copy is still there and the boot repairs from it.
//!   Crash between the sync and the delete: a stale copy the working file already contains; the boot
//!   finds nothing to repair and deletes it.
//! * **Clean shutdown:** the same as a roll for the live segment: sync the working file, delete the copy.
//! * **Boot** ([`repair_from_durable_copies`], run under the directory lock before anything reads):
//!   for each copy, the working file's longest valid prefix is extended by the copy's whole frames that
//!   follow it, the working file is synced, and only THEN is the copy deleted. A crash mid-repair leaves
//!   the copy, and the next boot repeats the repair (it is idempotent).
//!
//! # Disk
//!
//! One extra file per live segment: at most one segment's size (64 MiB at the default), less for a
//! resumed segment (a hole below the resume point). Two while a roll's seal is in flight.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::format::{HEADER, MAGIC};
use crate::frame::walk_frames;
use crate::handoff::write_at;
use crate::segment::segment_path;

/// The durable copy of segment `index`.
pub(crate) fn durable_path(dir: &Path, index: u64) -> PathBuf {
    dir.join(format!("journal-{index:08}.vjl.dur"))
}

/// The index of every durable copy in `dir`, ascending.
fn durable_indices(dir: &Path) -> io::Result<Vec<u64>> {
    let mut idxs: Vec<u64> = std::fs::read_dir(dir)?
        .filter_map(|e| {
            let name = e.ok()?.file_name().to_string_lossy().into_owned();
            name.strip_prefix("journal-")?.strip_suffix(".vjl.dur")?.parse().ok()
        })
        .collect();
    idxs.sort_unstable();
    Ok(idxs)
}

/// Create (or empty) the durable copy of segment `index`, and sync the directory once so its NAME
/// survives a power cut too.
pub(crate) fn create_durable(dir: &Path, index: u64) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(durable_path(dir, index))?;
    sync_dir(dir);
    Ok(file)
}

/// Delete the durable copy of segment `index` (gone already is fine).
pub(crate) fn remove_durable(dir: &Path, index: u64) -> io::Result<()> {
    match std::fs::remove_file(durable_path(dir, index)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

/// A header whose magic this build writes.
fn header_ok(b: &[u8]) -> bool {
    b.len() >= HEADER && u32::from_le_bytes(b[0..4].try_into().unwrap()) == MAGIC
}

/// The version field of a header, or 0 when the bytes are not there.
fn version_of(b: &[u8]) -> u32 {
    if b.len() >= 8 { u32::from_le_bytes(b[4..8].try_into().unwrap()) } else { 0 }
}

/// **Repair every working segment of `dir` from its durable copy, then delete the copies.** Run under
/// the directory's single-writer lock, before anything reads the journal: `Journal::open` and
/// the boot restore both call it. Returns one note per segment it changed.
///
/// Per copy: the working file's longest valid prefix (whole frames whose check passes, from the
/// header) is extended by the copy's whole frames that follow it — never a torn frame, the walk stops
/// at the first one in either file. A working file with no readable header takes the copy's; a
/// working file that does not exist (a crash mid-roll, after the copy was made and before the write
/// thread made the file) is made from the copy. A version the copy's header raised is raised in the
/// working file too. The working file is then synced, whether or not anything changed (after a process
/// crash its newest bytes may sit in the page cache only), and only then is the copy deleted.
pub fn repair_from_durable_copies(dir: &Path) -> io::Result<Vec<String>> {
    let mut notes = Vec::new();
    for index in durable_indices(dir)? {
        if let Some(note) = repair_one(dir, index)? {
            notes.push(note);
        }
        remove_durable(dir, index)?;
    }
    Ok(notes)
}

fn repair_one(dir: &Path, index: u64) -> io::Result<Option<String>> {
    let durable = std::fs::read(durable_path(dir, index))?;
    let path = segment_path(dir, index);
    let work = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let work_has_header = header_ok(&work);
    // Where the copy's frames are taken from: after the working file's last whole frame, or from the
    // copy's own header when the working file has none.
    let from = if work_has_header {
        walk_frames(&work, HEADER, |_| {}).0
    } else if header_ok(&durable) {
        HEADER
    } else {
        // Neither file can be read from its start: nothing to recover from the copy.
        return Ok(None);
    };
    let to = walk_frames(&durable, from.max(HEADER).min(durable.len()), |_| {}).0.max(from);
    let file =
        OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
    let mut changed = Vec::new();
    if !work_has_header {
        write_at(&file, &durable[..HEADER], 0)?;
        changed.push("the header".to_string());
    }
    if to > from {
        if file.metadata()?.len() < to as u64 {
            file.set_len(to as u64)?;
        }
        write_at(&file, &durable[from..to], from as u64)?;
        changed.push(format!("frames at {from}..{to}"));
    }
    // The sync thread writes a raised version into the copy before any frame that needs it.
    let (have, copy) = (
        if work_has_header { version_of(&work) } else { version_of(&durable) },
        version_of(&durable),
    );
    if copy > have {
        write_at(&file, &copy.to_le_bytes(), 4)?;
        changed.push(format!("version {have} -> {copy}"));
    }
    file.sync_all()?;
    Ok((!changed.is_empty()).then(|| {
        format!("journal segment {index}: repaired from its durable copy ({})", changed.join(", "))
    }))
}

#[path = "durable_tests.rs"]
#[cfg(test)]
mod durable_tests;
