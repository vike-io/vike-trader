//! **The journal's ARCHIVE** (`docs/decisions/0304-a-boot-restores-from-the-newest-snapshot-and-its-tail-and-old-segments-are-archived-never-deleted.md`,
//! verdict 2): where a segment the newest durable `Snapshot` superseded goes instead of being deleted,
//! `<journal_dir>/archive/`, and how it is kept there. The user's record of what the node did is never destroyed
//! by default.
//!
//! - **The move** is `Journal::prune_before_latest_snap` (`writer/offline.rs`), which picks the segments and hands
//!   each to [`archive_segment`]: a RENAME into `archive/` on the same filesystem, so a crash leaves the segment in
//!   the live directory or in the archive, never in neither, and never over an archived file (a rename onto an
//!   existing file replaces it on Linux, so a taken name gets a `-<n>` suffix).
//! - **The compression** is [`Journal::compress_archive`], after the move: `journal-N.vjl` is gzipped to
//!   `journal-N.vjl.gz.part`, read back against the segment byte for byte, synced, renamed to `journal-N.vjl.gz`,
//!   and only then is the plain file removed. A crash before the rename leaves the plain file (the next pass writes
//!   the `.part` again); after it, both (the next pass removes the plain one). `gunzip` gives the segment back
//!   exactly as it was, to be read with any journal reader.
//! - **The limit** ([`JournalArchivePolicy`]): the archive's bytes and its oldest file's age, by the file's
//!   modification time (the segment's own: the rename keeps it and the compression copies it). What a pass does
//!   once it is reached is `config.journal_archive_full`'s word (`vike_model::JournalArchiveFull`): under `keep`,
//!   the default, it moves nothing and deletes nothing ([`JournalArchivePass::limit`]); under `drop_oldest`, the
//!   operator's opt-in, it first DELETES the oldest finished `.vjl.gz` copies until the archive is under both
//!   limits, each said at INFO, and goes on ([`JournalArchivePolicy::drop_oldest`]). No deletion reaches a file
//!   outside `archive/`, a plain archived segment or a `.part`.
//!
//! Every reader keys on the top-level `journal-*.vjl` names, so nothing under `archive/` is read back as journal,
//! and a restore never needs it: only segments below the newest DURABLE `Snapshot`'s segment go there. A journal
//! moved aside (`Journal::set_aside`) takes its archive with it. Nothing here reads a clock or a setting: the
//! caller passes both, and calls it off the fold (the tradehub's `journal-archive` thread).

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use vike_model::JournalArchiveFull;

use crate::segment::segment_path;
use crate::writer::Journal;

/// The subdirectory of a journal directory superseded segments move into. Like [`crate::TORN_DIR`], no reader
/// looks below the top level, so nothing in it is read back as journal.
pub const ARCHIVE_DIR: &str = "archive";

/// What the archive pass keeps where: the settings rows, handed over as parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalArchivePolicy {
    /// How old a segment's NEWEST record must be before the segment leaves the live directory
    /// (`config.journal_keep_hours`), measured on the next segment's first record, which no record of it is newer
    /// than, against the caller's clock.
    pub keep_ms: i64,
    /// The archive's size limit (`config.journal_archive_max_gib`), in bytes, every file under `archive/` counted.
    pub max_bytes: u64,
    /// The archive's age limit (`config.journal_archive_max_days`): how old its oldest file may be.
    pub max_age_ms: i64,
    /// What a pass does at either limit (`config.journal_archive_full`).
    pub at_limit: JournalArchiveFull,
}

/// What one archive pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalArchivePass {
    /// The indices of the segments it moved into the archive, oldest first.
    pub moved: Vec<u64>,
    /// The archive's limit, when the pass found it reached (under `drop_oldest`, still reached after its
    /// deletions): the pass then moved nothing.
    pub limit: Option<JournalArchiveLimit>,
}

/// Which of the archive's limits a pass found reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalArchiveLimit {
    /// The archive holds `bytes`, at least [`JournalArchivePolicy::max_bytes`].
    Size { bytes: u64 },
    /// Its oldest file is `age_ms` old, more than [`JournalArchivePolicy::max_age_ms`].
    Age { age_ms: i64 },
}

impl JournalArchivePolicy {
    /// The limit the archive of `dir` has reached at `now_ms` (milliseconds since the epoch), if any, over every
    /// file under `<dir>/archive/` (no archive holds nothing). The size is named when both are reached.
    pub fn limit_reached(
        &self,
        dir: &Path,
        now_ms: i64,
    ) -> io::Result<Option<JournalArchiveLimit>> {
        let files = archived_files(&dir.join(ARCHIVE_DIR))?;
        let bytes = total_bytes(&files);
        if bytes >= self.max_bytes {
            return Ok(Some(JournalArchiveLimit::Size { bytes }));
        }
        Ok(files
            .iter()
            .map(|f| f.modified_ms)
            .min()
            .map(|oldest| now_ms.saturating_sub(oldest))
            .filter(|age_ms| *age_ms > self.max_age_ms)
            .map(|age_ms| JournalArchiveLimit::Age { age_ms }))
    }

    /// **`drop_oldest`** (`config.journal_archive_full`, the operator's opt-in; `keep` never calls it): DELETE the
    /// oldest finished copies under `<dir>/archive/` ([`is_finished_copy`]), oldest modification time first, until
    /// the archive is under both limits at `now_ms`, each said at INFO with its path and size. Returns the limit
    /// still reached after it, if any (the pass then moves nothing, as under `keep`).
    ///
    /// A copy goes only while it is past the age limit itself, or while the archive is at its size limit and the
    /// files no deletion may take are under it on their own (else deleting every copy would not bring it under,
    /// and none is deleted for the size). It lists nothing but `archive/`, so the live segments, the newest
    /// `Snapshot`'s and every later one, which never leave the live directory, are out of its reach.
    pub(crate) fn drop_oldest(
        &self,
        dir: &Path,
        now_ms: i64,
    ) -> io::Result<Option<JournalArchiveLimit>> {
        let mut files = archived_files(&dir.join(ARCHIVE_DIR))?;
        files.sort_by(|a, b| a.modified_ms.cmp(&b.modified_ms).then_with(|| a.path.cmp(&b.path)));
        let (copies, kept): (Vec<ArchivedFile>, Vec<ArchivedFile>) =
            files.into_iter().partition(is_finished_copy);
        let kept_bytes = total_bytes(&kept);
        let mut bytes = kept_bytes.saturating_add(total_bytes(&copies));
        for copy in &copies {
            let too_old = now_ms.saturating_sub(copy.modified_ms) > self.max_age_ms;
            let too_big = bytes >= self.max_bytes && kept_bytes < self.max_bytes;
            if !too_old && !too_big {
                break; // oldest first: no later copy is older, and the size only falls
            }
            match std::fs::remove_file(&copy.path) {
                Ok(()) => tracing::info!(
                    journal = %dir.display(),
                    file = %copy.path.display(),
                    bytes = copy.bytes,
                    "journal archive: deleted the oldest archived file (config.journal_archive_full = drop_oldest)"
                ),
                // taken off the box by hand meanwhile: gone either way
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            bytes = bytes.saturating_sub(copy.bytes);
        }
        self.limit_reached(dir, now_ms)
    }
}

/// One file directly under `archive/`: its path, its bytes and its modification time in milliseconds since the
/// epoch.
struct ArchivedFile {
    path: PathBuf,
    bytes: u64,
    modified_ms: i64,
}

/// Every file directly under `archive`. A missing archive holds none.
fn archived_files(archive: &Path) -> io::Result<Vec<ArchivedFile>> {
    let entries = match std::fs::read_dir(archive) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let since = meta.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default();
        files.push(ArchivedFile {
            path: entry.path(),
            bytes: meta.len(),
            modified_ms: i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
        });
    }
    Ok(files)
}

fn total_bytes(files: &[ArchivedFile]) -> u64 {
    files.iter().fold(0, |sum, f| sum.saturating_add(f.bytes))
}

/// A FINISHED copy: a `journal-*.vjl.gz` whose plain `journal-*.vjl` is gone, which [`Journal::compress_archive`]
/// removes only once the copy was read back, synced and named. Never a plain segment (not compressed yet), a
/// `.part` (being written or verified), or a copy beside the plain file it was made from (a crash between the
/// copy's rename and the plain file's removal); a plain file that cannot be looked for counts as there.
fn is_finished_copy(file: &ArchivedFile) -> bool {
    let Some(name) = file.path.file_name().and_then(|n| n.to_str()) else { return false };
    name.starts_with("journal-")
        && name.strip_suffix(".gz").is_some_and(|plain| {
            plain.ends_with(".vjl")
                && file.path.with_file_name(plain).try_exists().is_ok_and(|there| !there)
        })
}

/// MOVE segment `index` of `dir` into `<dir>/archive/`, a rename, under a name no archived file holds
/// (`journal-<index>.vjl`, else `journal-<index>-<n>.vjl`). Returns where it went. The caller syncs both
/// directories once its moves are done.
pub(crate) fn archive_segment(dir: &Path, index: u64) -> io::Result<PathBuf> {
    let archive = dir.join(ARCHIVE_DIR);
    std::fs::create_dir_all(&archive)?;
    let mut name = format!("journal-{index:08}.vjl");
    let mut n = 1u32;
    while archive.join(&name).exists() || archive.join(format!("{name}.gz")).exists() {
        name = format!("journal-{index:08}-{n}.vjl");
        n += 1;
    }
    let to = archive.join(name);
    std::fs::rename(segment_path(dir, index), &to)?;
    Ok(to)
}

impl Journal {
    /// **Compress every plain segment under `<dir>/archive/`** (the module doc's order: gzip to a `.part`, read it
    /// back against the segment, sync, rename, and only then remove the plain file), oldest name first. Returns the
    /// compressed files. A copy that does not read back as its segment is an `Err` and the segment stays as it
    /// was; so does every segment after it, until the next call.
    ///
    /// It touches nothing a reader of the live journal reads, so it runs beside a live core, on the caller's
    /// thread (never the fold): a 64 MiB segment is about a second of one core.
    pub fn compress_archive(dir: &Path) -> io::Result<Vec<PathBuf>> {
        let archive = dir.join(ARCHIVE_DIR);
        let mut plain: Vec<PathBuf> = match std::fs::read_dir(&archive) {
            Ok(entries) => entries
                .filter_map(|e| {
                    let path = e.ok()?.path();
                    path.to_str()?.ends_with(".vjl").then_some(path)
                })
                .collect(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        plain.sort();
        let mut done = Vec::with_capacity(plain.len());
        for path in plain {
            let packed = with_suffix(&path, ".gz");
            // A `.gz` beside it was synced before it was named: a crash came between the rename and the removal.
            if !packed.exists() {
                let part = with_suffix(&packed, ".part");
                gzip_verified(&path, &part)?;
                std::fs::rename(&part, &packed)?;
                vike_utility::atomic_file::sync_dir(&archive)?;
            }
            std::fs::remove_file(&path)?;
            done.push(packed);
        }
        Ok(done)
    }
}

/// `path` with `suffix` appended to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Gzip `plain` into `part`, give it `plain`'s modification time (the age the archive's limit reads), sync it, and
/// read it back against `plain` byte for byte: a copy that differs is an `Err`.
fn gzip_verified(plain: &Path, part: &Path) -> io::Result<()> {
    let modified = std::fs::metadata(plain)?.modified()?;
    let mut encoder = GzEncoder::new(File::create(part)?, Compression::fast());
    io::copy(&mut File::open(plain)?, &mut encoder)?;
    let file = encoder.finish()?;
    file.set_modified(modified)?;
    file.sync_all()?;
    drop(file);
    if !same_bytes(GzDecoder::new(File::open(part)?), File::open(plain)?)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "journal archive: the compressed copy {} does not read back as {}; the segment stays uncompressed",
                part.display(),
                plain.display()
            ),
        ));
    }
    Ok(())
}

/// Whether two readers yield the same bytes up to their ends.
fn same_bytes(mut a: impl Read, mut b: impl Read) -> io::Result<bool> {
    let (mut x, mut y) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let n = fill(&mut a, &mut x)?;
        if n != fill(&mut b, &mut y)? || x[..n] != y[..n] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Read into `buffer` until it is full or `reader` ends: how many bytes it holds.
fn fill(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buffer.len() {
        match reader.read(&mut buffer[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[path = "archive_tests.rs"]
#[cfg(test)]
mod archive_tests;
