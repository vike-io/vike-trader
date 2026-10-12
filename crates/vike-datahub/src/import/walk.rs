//! **The import lane's WALK and its SAFE OPEN** — the two halves of "a daemon reading files a user
//! placed never reads anything outside the directory it was told to read"
//! (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §2.3, and
//! `docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md`
//! verdict 2). Shared by every archive format; each format supplies only its GRAMMAR (which
//! directory names it descends into, and what a file name means), through
//! [`super::ArchiveFormat`].
//!
//! # The walk never follows anything
//!
//! Every entry below the dataset directory is examined with `symlink_metadata` — the entry ITSELF,
//! never what it points at — and only two kinds are accepted: a plain directory whose name the
//! format's grammar expects at that depth, and a plain regular file. Everything else is classified
//! and SKIPPED, never opened: a symlink, a FIFO, a socket, a device, and (on unix) a regular file
//! with more than one hard link, which could be a second name for a file outside the root. An
//! unexpected NAME or DEPTH is counted as an "other object" with its bytes and never opened either.
//! The format directory and the dataset directory themselves face the same check: a symlinked
//! dataset is refused as unreadable rather than walked.
//!
//! The walk goes at most [`WalkCaps::max_depth`] levels below the dataset and visits at most
//! [`WalkCaps::max_entries`] entries — the design's §5 bounds, `4` (the hourly layout's depth) and
//! `100,000` (EURUSD is 26,586 objects; a directory beyond the cap is not one instrument's archive,
//! and the request is REFUSED rather than half-walked).
//!
//! # The open closes the race the walk leaves open
//!
//! The walk vets an entry; something with write access to the imports directory could replace it
//! before the import opens it. [`open_vetted`] makes that harmless for EVERY path component:
//!
//! - **unix** — the open is `O_NOFOLLOW | O_NONBLOCK`, and the HANDLE is then `fstat`ed: it must be
//!   a regular file whose `(dev, ino)` EQUALS what the walk's `lstat` recorded. Whatever the path
//!   resolves to at open time — a symlink swapped into a directory above it included — the object
//!   read is the object the walk vetted, or nothing is read. `O_NONBLOCK` is what stops a FIFO
//!   swapped in after the walk from parking the thread inside `open` (a FIFO opened for reading
//!   blocks until a writer appears); the `fstat` then refuses it as not a regular file.
//! - **elsewhere** — there is no `O_NOFOLLOW`, so a symlink at the last component is probed
//!   explicitly (a TOCTOU-racy check), and the identity compared is the file's LENGTH and
//!   MODIFICATION TIME rather than its inode. That is weaker, and it is DECLARED as the design's
//!   residual R8 rather than claimed: the datahub deploys on unix, where the image runs too.
//!
//! The read itself goes through `take(cap + 1)`, so a file that grew past the format's size cap
//! after the walk is refused having buffered at most one byte more than the cap.
//!
//! # Nothing read is ever echoed
//!
//! A refusal names a path RELATIVE to the imports root and a class — never file bytes. The skipped
//! entries the plan reports are the same relative paths.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use vike_datahub_client::archive::{DatasetDir, DayRefusal, EntryClass, SkippedEntry};

use super::{ArchiveFormat, LayoutFile};

/// The walk's two bounds (design §5). [`WalkCaps::DEFAULT`] is what every mounted lane uses; a
/// test narrows them to reach the refusal with a handful of entries rather than a hundred thousand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkCaps {
    /// The most entries ONE walk visits — directories and files alike, skipped ones included. One
    /// past it REFUSES the request: a directory that large is not one instrument's archive, and a
    /// plan built from part of it would report gaps that are not gaps.
    pub max_entries: usize,
    /// How many levels below the dataset directory an entry may sit. A directory at this depth is
    /// never descended into, whatever the format's grammar says, so no entry deeper than this is
    /// ever examined.
    pub max_depth: usize,
}

impl WalkCaps {
    /// The design's §5 values: `100,000` entries (EURUSD's whole bucket prefix is 26,586 objects)
    /// and `4` levels — the hourly layout's `<YYYY>/<MM>/<DD>/<HH>h_ticks.bi5`, the deeper of the two
    /// layouts this lane recognises. DEFAULTS, like every §5 bound, until the acceptance run (T7).
    pub const DEFAULT: WalkCaps = WalkCaps { max_entries: 100_000, max_depth: 4 };
}

/// WHICH filesystem object the walk vetted — what [`open_vetted`] compares the opened handle to.
///
/// ⚠ On unix it is the `(dev, ino)` pair and NOTHING else, deliberately: a file appended to in place
/// is still the object that was vetted (the size cap bounds what is read), while a file REPLACED by
/// a rename — however identical its length — is a different object, and only the inode can say so.
/// Comparing the length too would make the swap check depend on the swapped-in file happening to
/// differ in size, which a planted file need not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    /// The weaker non-unix identity (the design's R8): length and modification time.
    #[cfg(not(unix))]
    len: u64,
    #[cfg(not(unix))]
    modified: Option<std::time::SystemTime>,
}

impl Identity {
    /// The identity `meta` describes — an `lstat` of the entry during the walk, or an `fstat` of
    /// the opened handle.
    pub fn of(meta: &Metadata) -> Identity {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Identity { dev: meta.dev(), ino: meta.ino() }
        }
        #[cfg(not(unix))]
        {
            Identity { len: meta.len(), modified: meta.modified().ok() }
        }
    }
}

/// One daily file the walk VETTED: where it is, how it is reported, and which object it was.
#[derive(Debug, Clone)]
pub struct VettedFile {
    /// The composed path under the imports root.
    pub path: PathBuf,
    /// The same path RELATIVE to the imports root, `/`-joined — what every report names.
    pub rel: String,
    /// Its size as the walk saw it.
    pub len: u64,
    /// The object the walk vetted.
    pub identity: Identity,
}

/// What one walk of one dataset directory found.
#[derive(Debug, Clone)]
pub struct Walk {
    /// Whether the dataset directory could be walked at all.
    pub dir: DatasetDir,
    /// Every daily file of the importable layout, by the epoch-ms of its UTC day.
    pub daily: BTreeMap<i64, VettedFile>,
    /// Days holding a file of a RECOGNISED layout the format does not import (Dukascopy's hourly
    /// files) — with or without a daily file beside it.
    pub other_layout: BTreeSet<i64>,
    /// Entries with an unexpected name or depth: counted, never opened.
    pub other_objects: u64,
    /// The bytes of [`Self::other_objects`] (a directory counts as zero — it is not descended).
    pub other_bytes: u64,
    /// Entries the walk refused to follow or open, sorted by path.
    pub skipped: Vec<SkippedEntry>,
}

impl Walk {
    /// A walk that found nothing, because `dir` could not be walked.
    fn empty(dir: DatasetDir) -> Walk {
        Walk {
            dir,
            daily: BTreeMap::new(),
            other_layout: BTreeSet::new(),
            other_objects: 0,
            other_bytes: 0,
            skipped: Vec::new(),
        }
    }
}

/// The whole-request refusal a walk past [`WalkCaps::max_entries`] ends in. `Err` from
/// [`walk_dataset`] is this and nothing else: every OTHER problem a walk meets is a property of the
/// directory, reported in the plan as [`DatasetDir::Unreadable`] or a skipped entry.
pub fn too_many_entries(caps: &WalkCaps) -> String {
    format!(
        "the dataset directory holds more than {} entries (the walk's cap): that is not one \
         instrument's archive, and a plan built from part of it would report gaps that are not \
         gaps. Point the import at one instrument's folder. Nothing was read and nothing was \
         written.",
        caps.max_entries
    )
}

/// Walk `<root>/<format id>/<dataset>` under `caps`, vetting every entry — see the module doc.
///
/// `now_ms` is handed to the format's grammar (Dukascopy's refuses a year after the current one).
/// `Err` is the entry-cap refusal ([`too_many_entries`]); everything else is in the [`Walk`].
pub fn walk_dataset(
    root: &Path,
    format: &dyn ArchiveFormat,
    dataset: &str,
    now_ms: i64,
    caps: &WalkCaps,
) -> Result<Walk, String> {
    let format_dir = root.join(format.id());
    if let Some(stop) = vet_directory(&format_dir, format.id()) {
        return Ok(stop);
    }
    let dataset_rel = format!("{}/{dataset}", format.id());
    let dataset_dir = format_dir.join(dataset);
    if let Some(stop) = vet_directory(&dataset_dir, &dataset_rel) {
        return Ok(stop);
    }

    let mut walk = Walk::empty(DatasetDir::Present);
    let mut entries = 0usize;
    // Depth-first over directories the grammar accepted; `rel` is the path's segments relative to
    // the dataset directory. Visit order does not matter — every result is keyed or sorted below.
    let mut stack: Vec<(PathBuf, Vec<String>)> = vec![(dataset_dir, Vec::new())];
    while let Some((dir, rel)) = stack.pop() {
        let listing = match std::fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(e) => return Ok(unreadable_below(&dataset_rel, &rel, &e)),
        };
        for entry in listing {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => return Ok(unreadable_below(&dataset_rel, &rel, &e)),
            };
            entries += 1;
            if entries > caps.max_entries {
                return Err(too_many_entries(caps));
            }
            let path = entry.path();
            // `symlink_metadata`: the ENTRY, never what it points at — the whole of "the walk
            // never follows anything".
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                // Gone between the listing and the look — a sync's temporary file renamed away.
                // Nothing was there to vet, so nothing is reported.
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Ok(unreadable_below(&dataset_rel, &rel, &e)),
            };
            let Ok(name) = entry.file_name().into_string() else {
                // A name that is not UTF-8 is no layout name: counted, never opened.
                walk.other_objects += 1;
                walk.other_bytes += if meta.is_file() { meta.len() } else { 0 };
                continue;
            };
            let mut child = rel.clone();
            child.push(name);
            let child_rel = format!("{dataset_rel}/{}", child.join("/"));
            let kind = meta.file_type();
            if kind.is_symlink() {
                walk.skipped.push(SkippedEntry { path: child_rel, class: EntryClass::Symlink });
            } else if kind.is_dir() {
                let segments: Vec<&str> = child.iter().map(String::as_str).collect();
                if child.len() < caps.max_depth && format.accepts_dir(&segments, now_ms) {
                    stack.push((path, child));
                } else {
                    // An unexpected name, or a directory at the depth cap: counted, not descended.
                    walk.other_objects += 1;
                }
            } else if kind.is_file() {
                if is_hard_linked(&meta) {
                    walk.skipped
                        .push(SkippedEntry { path: child_rel, class: EntryClass::HardLinked });
                    continue;
                }
                let segments: Vec<&str> = child.iter().map(String::as_str).collect();
                match format.classify_file(&segments, now_ms) {
                    LayoutFile::Daily(day) => {
                        let vetted = VettedFile {
                            path,
                            rel: child_rel,
                            len: meta.len(),
                            identity: Identity::of(&meta),
                        };
                        walk.daily.insert(day, vetted);
                    }
                    LayoutFile::OtherLayout(day) => {
                        walk.other_layout.insert(day);
                    }
                    LayoutFile::Other => {
                        walk.other_objects += 1;
                        walk.other_bytes += meta.len();
                    }
                }
            } else {
                walk.skipped.push(SkippedEntry { path: child_rel, class: special_class(&meta) });
            }
        }
    }
    walk.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(walk)
}

/// The check the format directory and the dataset directory face before the walk enters them:
/// `None` when `dir` is a plain directory, otherwise the [`Walk`] the request answers with.
///
/// A symlink here is refused like any symlink below it — `rel` (relative to the imports root) is
/// reported as a skipped entry and the dataset as unreadable — and it is the case that matters
/// most, because a symlinked DATASET would otherwise redirect the whole walk.
fn vet_directory(dir: &Path, rel: &str) -> Option<Walk> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let mut walk = Walk::empty(DatasetDir::Unreadable {
                why: format!(
                    "`{rel}` is a symbolic link, and this lane never follows one below the imports \
                     root. Put the files themselves there (a symlink or a mount AT the imports \
                     root is the way to keep them on another disk)."
                ),
            });
            walk.skipped.push(SkippedEntry { path: rel.to_string(), class: EntryClass::Symlink });
            Some(walk)
        }
        Ok(meta) if meta.is_dir() => None,
        Ok(_) => Some(Walk::empty(DatasetDir::Unreadable {
            why: format!("`{rel}` exists and is not a directory."),
        })),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(Walk::empty(DatasetDir::Absent)),
        Err(e) => Some(Walk::empty(DatasetDir::Unreadable {
            why: format!("`{rel}` exists and cannot be read ({}).", e.kind()),
        })),
    }
}

/// A directory inside the dataset that could not be listed or examined makes the WHOLE dataset
/// unreadable rather than half-walked: a plan built from part of it would report the missing part's
/// days as gaps, and an import would treat them as never synced.
fn unreadable_below(dataset_rel: &str, rel: &[String], e: &io::Error) -> Walk {
    let at = if rel.is_empty() {
        dataset_rel.to_string()
    } else {
        format!("{dataset_rel}/{}", rel.join("/"))
    };
    Walk::empty(DatasetDir::Unreadable {
        why: format!(
            "`{at}` could not be listed ({}), so no part of the dataset is planned.",
            e.kind()
        ),
    })
}

/// Whether a regular file has more than one hard link — a second NAME that could belong to a file
/// outside the root. Unix only; elsewhere the question has no portable answer and this is `false`.
fn is_hard_linked(meta: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// The class of an entry that is neither a directory, a regular file nor a symlink.
fn special_class(meta: &Metadata) -> EntryClass {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let kind = meta.file_type();
        if kind.is_fifo() {
            return EntryClass::Fifo;
        }
        if kind.is_socket() {
            return EntryClass::Socket;
        }
        if kind.is_block_device() || kind.is_char_device() {
            return EntryClass::Device;
        }
    }
    let _ = meta;
    EntryClass::Unknown
}

/// The refusal class a file gets when what the open finds is not what the walk vetted.
pub const CHANGED_SINCE_WALK: &str = "ChangedSinceWalk";
/// The refusal class a file gets when it cannot be opened or read at all.
pub const UNREADABLE_FILE: &str = "UnreadableFile";
/// The refusal class a file gets when it holds more than the format's size cap at read time.
pub const FILE_TOO_LARGE: &str = "FileTooLarge";

/// Open `file` — the object the walk vetted, or nothing (see the module doc).
///
/// ⚠ **The identity comparison is the load-bearing line.** `O_NOFOLLOW` guards only the LAST path
/// component; a symlink swapped in for a directory ABOVE the file is followed by every `open`. What
/// makes that harmless is comparing the handle's `(dev, ino)` with the walk's — the object reached,
/// by whatever route, must be the object vetted. Without it, a file renamed over the vetted one
/// between the walk and the open would be read as if it had been vetted.
pub fn open_vetted(file: &VettedFile) -> Result<File, DayRefusal> {
    let opened = match open_no_follow(&file.path) {
        Ok(opened) => opened,
        Err(e) => return Err(open_refusal(file, &e)),
    };
    let meta = opened.metadata().map_err(|e| unreadable(file, &e))?;
    if !meta.file_type().is_file() {
        return Err(changed(file, "is no longer a regular file"));
    }
    if Identity::of(&meta) != file.identity {
        return Err(changed(
            file,
            "is not the object the walk vetted — it was replaced after the walk",
        ));
    }
    Ok(opened)
}

/// The first `n` bytes of `file` (fewer if it is shorter) — what a plan reads to learn what a file's
/// header declares, without decoding it.
pub fn read_vetted_prefix(file: &VettedFile, n: usize) -> Result<Vec<u8>, DayRefusal> {
    let opened = open_vetted(file)?;
    let mut buf = Vec::with_capacity(n);
    opened.take(n as u64).read_to_end(&mut buf).map_err(|e| unreadable(file, &e))?;
    Ok(buf)
}

/// The whole of `file`, refused if it holds more than `cap` bytes — read through `take(cap + 1)`,
/// so at most one byte past the cap is ever buffered.
pub fn read_vetted(file: &VettedFile, cap: u64) -> Result<Vec<u8>, DayRefusal> {
    let opened = open_vetted(file)?;
    let mut buf = Vec::with_capacity(file.len.min(cap) as usize);
    opened.take(cap.saturating_add(1)).read_to_end(&mut buf).map_err(|e| unreadable(file, &e))?;
    if buf.len() as u64 > cap {
        return Err(DayRefusal {
            class: FILE_TOO_LARGE.to_string(),
            detail: format!(
                "`{}` holds more than the format's {cap}-byte file cap. Nothing was decoded or \
                 stored and no commit key was spent.",
                file.rel
            ),
        });
    }
    Ok(buf)
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// No `O_NOFOLLOW` off unix: the last component is probed explicitly — racy, where the unix arm is
/// not, and declared as the design's R8 — and the identity check after the open is the weaker
/// length-and-mtime one.
#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> io::Result<File> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a symbolic link"));
    }
    File::open(path)
}

/// The refusal an `open` failure maps to — a symlink at the last component (unix `ELOOP` under
/// `O_NOFOLLOW`) and a vanished file are CHANGES since the walk; anything else is unreadable.
fn open_refusal(file: &VettedFile, e: &io::Error) -> DayRefusal {
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ELOOP) {
        return changed(file, "is now a symbolic link, which this lane never follows");
    }
    if e.kind() == io::ErrorKind::NotFound {
        return changed(file, "is gone");
    }
    unreadable(file, e)
}

fn changed(file: &VettedFile, what: &str) -> DayRefusal {
    DayRefusal {
        class: CHANGED_SINCE_WALK.to_string(),
        detail: format!(
            "`{}` {what} since the walk vetted it, so it was NOT read. Nothing was stored and no \
             commit key was spent; run the import again to re-plan the directory as it is now.",
            file.rel
        ),
    }
}

fn unreadable(file: &VettedFile, e: &io::Error) -> DayRefusal {
    DayRefusal {
        class: UNREADABLE_FILE.to_string(),
        detail: format!(
            "`{}` could not be read ({}). Nothing was stored and no commit key was spent.",
            file.rel,
            e.kind()
        ),
    }
}
