//! Reading, writing and folding a series manifest: the base file and its delta log.

use std::io::{ErrorKind, Write};
use std::path::Path;

use crate::store::hist::DataError;
use crate::store::hist_maint::Durability;

use super::super::delta::{self, DeltaFrame};
use super::super::{fsync_dir, io, q};
use super::{MANIFEST, MANIFEST_FORMAT, Manifest};

/// Parse the BASE file alone — no delta replay. [`read_manifest`] is what callers want.
///
/// Only the current format is read (`MANIFEST_FORMAT`): any other tag, including a v2 file, is
/// refused with the repair that rebuilds the manifest from the series' parts. An ABSENT base is
/// not an error: it is a brand-new series, and `base_pending` makes its first publish write one.
pub(crate) fn read_base(dir: &Path) -> Result<Manifest, DataError> {
    let bytes = match std::fs::read(dir.join(MANIFEST)) {
        Ok(b) => b,
        // A series with no base yet. `base_pending` so the FIRST publish writes one: `list_series`
        // finds leaves by the presence of `_manifest.json`, so a series that only ever appended
        // frames would be invisible to maintenance, to the Data Manager and to `delete_series`.
        Err(ref e) if e.kind() == ErrorKind::NotFound => {
            return Ok(Manifest { base_pending: true, ..Manifest::empty() });
        }
        Err(e) => return Err(io(e)),
    };
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| {
        DataError::Query(format!(
            "manifest parse at {} (incompatible format? this build reads only \
             v{MANIFEST_FORMAT}): {e}",
            dir.display()
        ))
    })?;
    if m.format != MANIFEST_FORMAT {
        return Err(DataError::Query(format!(
            "manifest format v{} at {} — this build reads only v{MANIFEST_FORMAT}; rebuild the \
             series' manifest from its parts with `vike-cli data hist repair --kind K --venue V …`, \
             which reaches this state: the rebuild never parses the base, it asks \
             `last_published_version`, which swallows this very error to 0",
            m.format,
            dir.display()
        )));
    }
    Ok(m)
}

/// The manifest as of this instant: the base file, with every delta frame it does not already carry
/// replayed over it.
///
/// ⚠ **The LOG is read before the BASE, and the order is load-bearing rather than stylistic.** A
/// fold publishes a new base and THEN clears the log; a reader that took the base first could pair
/// a pre-fold base with a post-fold (empty) log and silently lose every frame the fold had just
/// folded in. Taking the log first cannot lose anything — `delta.rs`'s "Ordering" section carries
/// the case analysis. Do not reorder these two statements.
///
/// Replay is version-guarded, so a fold that published its base and died before clearing the log
/// re-reads frames the base already holds and skips every one. Without that, such a crash would
/// double every `FileEntry` the fold had absorbed, and a doubled entry is a part READ TWICE.
pub(crate) fn read_manifest(dir: &Path) -> Result<Manifest, DataError> {
    let (frames, intact_len) = delta::delta_read_with_end(dir)?;
    // ⚠ A base and its log are ONE object, and half of one is CORRUPTION rather than a degraded
    // read. Replaying frames onto an empty manifest would succeed and produce an index naming SOME
    // of the series' parts — every commit since the last fold and none before it — so a query over
    // the older window would return fewer rows with no error at all. That is the silent shape; an
    // error is the recoverable one, and the recovery already exists.
    //
    // No legitimate writer produces this state: a series' first commit publishes a base before any
    // frame exists and a fold RENAMES over the base so it is never absent. Someone deleted the base.
    if !frames.is_empty() && !dir.join(MANIFEST).is_file() {
        let delta_name = delta::DELTA;
        return Err(DataError::Query(format!(
            "manifest at {} has a delta log but NO base ({MANIFEST} is missing): replaying the log \
             alone would index only the parts committed since the last fold and silently hide the \
             rest. The repair is `DataFusionHist::rebuild_series_manifest`, which re-derives the \
             index from the parts themselves, and the OPERATOR spelling is \
             `vike-cli data hist repair --kind K --venue V (--symbol S [--interval I] | \
             --group G) [--store DIR]` — which REHEARSES by default and writes only with --yes. \
             Removing `{delta_name}` as well \
             would also make the series readable, at the cost of every commit since the last fold.",
            dir.display()
        )));
    }
    let mut m = read_base(dir)?;
    for f in &frames {
        if f.version <= m.version {
            continue; // already folded into the base
        }
        m.apply(f);
    }
    // Carry the tear (if any) up to the next writer — see `Manifest::log_torn_at`. Nothing is
    // repaired here: this function runs lock-free.
    let on_disk = delta::delta_len(dir);
    m.log_torn_at = (on_disk > intact_len).then_some(intact_len);
    Ok(m)
}

/// Publish a BASE manifest by atomic rename (write tmp → replace). `fs::rename` replaces on modern
/// Windows + unix; the remove-then-rename fallback covers any platform that won't.
///
/// ⚠ **This is no longer the append path's publish** — [`publish`] is, and it appends a frame. This
/// runs only at a FOLD, when a series gets its first base, and from
/// `rebuild_series_manifest`. Callers outside this module must go through [`publish`] or
/// [`fold_base`]: writing a base without clearing the log leaves frames that will replay over it.
pub(super) fn write_manifest(
    dir: &Path,
    m: &Manifest,
    durability: Durability,
) -> Result<(), DataError> {
    let bytes = serde_json::to_vec_pretty(m).map_err(q)?;
    let tmp = dir.join("_manifest.json.tmp");
    // fsync the new manifest's bytes BEFORE the rename so the published manifest is durable. This is
    // what lets `commit_rows` safely clear the WAL only AFTER a durable publish — without it a crash
    // could lose the (not-yet-durable) manifest while the WAL was already removed (an ordering
    // inversion that would strand the append).
    //
    // [`Durability::Bulk`] skips it: bulk import keeps no WAL, so there is no such inversion to
    // avoid, and losing the publish is not a loss — the manifest reverts to its previous version and
    // the re-run redoes that commit_key. What bulk must NOT skip is the PART fsync, and it doesn't
    // (see `write_parquet`) — that is what keeps a surviving manifest entry from naming a torn part.
    {
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        f.write_all(&bytes).map_err(io)?;
        if durability == Durability::Fsync {
            f.sync_all().map_err(io)?;
        }
    }
    let final_path = dir.join(MANIFEST);
    if std::fs::rename(&tmp, &final_path).is_err() {
        let _ = std::fs::remove_file(&final_path);
        std::fs::rename(&tmp, &final_path).map_err(io)?;
    }
    // On POSIX the rename above is durable only once the containing directory is fsynced — without
    // this the manifest's own bytes survive under a name that does not.
    if durability == Durability::Fsync {
        fsync_dir(dir)?;
    }
    Ok(())
}

/// The highest version this series has ever published, tolerating a manifest that is only half
/// present. ONLY [`super::DataFusionHist::rebuild_series_manifest`] uses it, because a rebuild is
/// the repair for exactly the state [`read_manifest`] refuses, and it still must not restart the
/// version counter underneath a delta log or a compaction's output names.
pub(crate) fn last_published_version(dir: &Path) -> u64 {
    let base = read_base(dir).map(|m| m.version).unwrap_or(0);
    let log = delta::delta_read_with_end(dir)
        .ok()
        .and_then(|(frames, _)| frames.last().map(|f| f.version))
        .unwrap_or(0);
    base.max(log)
}

/// The orphan keys of the base AND its delta log — what a rebuild would drop — tolerating either
/// half being absent or unreadable. ONLY the repair plan uses it.
///
/// Not [`read_manifest`]: that refuses a base-less series with a surviving log, which is the state
/// a repair is most often run for. And not the base alone: a key spent with no part
/// (`DataFusionHist::spend_keys_without_rows`) lives ONLY in the log until the next fold, so a
/// base-only count misses every one spent since. Replay is the same version-guarded
/// [`Manifest::apply`] the read path runs; an unreadable base replays over an empty one, and an
/// unreadable log contributes nothing (the plan reports both of those separately).
pub(crate) fn folded_orphan_commits(dir: &Path) -> Vec<String> {
    let mut m = read_base(dir).unwrap_or_else(|_| Manifest::empty());
    if let Ok((frames, _)) = delta::delta_read_with_end(dir) {
        for f in &frames {
            if f.version > m.version {
                m.apply(f);
            }
        }
    }
    m.orphan_commits
}

/// Collapse the delta log into a new base: publish the base, THEN clear the log.
///
/// **The order is the whole of the crash story and it may not be swapped.** Publishing first means
/// a crash between the two steps leaves a durable base plus a log whose frames that base already
/// carries — and replay skips them by version, so the state is exactly right and the next fold
/// clears the log. Clearing first would mean a crash loses every frame the base had not yet
/// absorbed, which is published commits gone.
pub(crate) fn fold_base(dir: &Path, m: &Manifest, durability: Durability) -> Result<(), DataError> {
    write_manifest(dir, m, durability)?;
    delta::delta_clear(dir)
}

/// Publish one change to a series manifest.
///
/// **The caller has already applied the change to `m`** (sealed its parts into `m.files`, dropped
/// the ones it removed, bumped `m.version`) and describes it in `frame`. This is the write side's
/// single entry point and it replaces the `m.version += 1; write_manifest(…)` pair every publish
/// site used to spell for itself.
///
/// Ordering, and why it is the same durability boundary as before:
///
/// - The frame is appended and fsynced only AFTER every part it names has had its bytes and its
///   directory entry fsynced, so a reader can never be handed a manifest naming a part that is not
///   there. That was `write_manifest`'s guarantee and it is unchanged; only the fsync got smaller.
/// - A caller may clear an applied WAL record only once this returns. That is the ordering
///   `wal.rs`'s `wal_rewrite_keeping_unapplied` depends on, and the boundary it waits on moves from
///   "the whole manifest is durable" to "this frame is durable".
/// - A crash before the frame's fsync leaves the sealed part on disk under no manifest entry —
///   exactly the window the WAL exists to close, and it closes it identically.
///
/// `fold_bytes` is the log size at which this folds (see [`FOLD_BYTES`]); it is a parameter only so
/// tests can drive a fold in milliseconds instead of at the production cadence.
pub(crate) fn publish(
    dir: &Path,
    m: &mut Manifest,
    frame: DeltaFrame,
    durability: Durability,
    fold_bytes: u64,
) -> Result<(), DataError> {
    if m.base_pending {
        // A series' first base. `m` already carries the change, so writing it whole publishes the
        // change too — there is no frame to append, and any stale log is cleared.
        fold_base(dir, m, durability)?;
        m.base_pending = false;
        m.log_torn_at = None; // the log is gone; there is no tail left to cut
        return Ok(());
    }
    // A crash left a torn tail. Cut it off BEFORE appending, or this frame lands beyond where any
    // reader stops and is silently lost — as is every frame after it, until the next fold. This is
    // the writer's job and this is the only place it is safe: we hold the series lock.
    if let Some(intact_len) = m.log_torn_at.take() {
        delta::delta_truncate(dir, intact_len)?;
    }
    delta::delta_append(dir, &frame, durability)?;
    if delta::delta_len(dir) >= fold_bytes {
        fold_base(dir, m, durability)?;
    }
    Ok(())
}
