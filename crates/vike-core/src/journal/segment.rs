//! Segment files on disk: how they are NAMED (`journal-{idx:08}.vjl`, and the `.prep` staging name
//! that keeps a prepared-but-not-yet-live segment invisible to every scanner) and how their blocks
//! are RESERVED up front so the append path never pays a filesystem allocation inside a write
//! fault. Split out of `journal.rs` verbatim.
//!
//! The segment LIFECYCLE (`map_segment` / `prepare_next` / `roll`) stays with the writer, in
//! [`super::writer`] — it is inseparable from the `CommandJournal` state it swaps.

use std::path::{Path, PathBuf};

/// How full the live segment must get before its successor is built ahead of time
/// (`CommandJournal::prepare_next`). 75% leaves a quarter of a segment's worth of appends for the
/// worker to finish sizing + reserving — on a 64MB segment that is ~150k appends against a
/// measured ~1.7ms of work, i.e. an enormous margin — while still being late enough that a journal
/// which never fills a segment never spawns the worker at all.
pub(super) const PREP_WATERMARK_PCT: usize = 75;

pub(super) fn seg_path(dir: &Path, idx: u64) -> PathBuf {
    dir.join(format!("journal-{idx:08}.vjl"))
}

/// Where a segment is BUILT before it becomes live (see `prepare_next`), then renamed into
/// [`seg_path`] by [`super::CommandJournal::roll`].
///
/// The `.prep` suffix is load-bearing for CRASH SAFETY, not cosmetic. Every segment scanner in
/// this module resolves an index with `strip_prefix("journal-")` + `strip_suffix(".vjl")` —
/// `open`'s resume scan, `latest_segment_version`, `read_since`, `prune_before_latest_snap` — and
/// `"journal-00000001.vjl.prep"` fails that suffix, so a prepared-but-not-yet-live segment is
/// invisible to all of them. That matters: were it named `.vjl`, a crash between preparation and
/// roll would leave an EMPTY highest-index segment, and `open` would resume into it, find no MAGIC,
/// treat it as fresh and stamp `first_seq = 0` — restarting the sequence at zero while earlier
/// segments hold higher seqs. A leftover `.prep` file after a crash is inert; the next preparation
/// simply truncates and reuses it.
pub(super) fn prep_path(dir: &Path, idx: u64) -> PathBuf {
    dir.join(format!("journal-{idx:08}.vjl.prep"))
}

/// Reserve real filesystem blocks for `[0, len)` of `file` (`posix_fallocate`).
///
/// [`std::fs::File::set_len`] only moves the size marker; the extents stay unallocated, so the
/// filesystem defers each block allocation to the first write fault on that page — landing it on
/// the journal's append path, which the `p99 < 10µs` core-hop gate measures. Reserving up front
/// moves that work to segment open/roll as one metadata operation.
///
/// **Deliberately infallible** (returns `()`, never `io::Result`): this is a latency optimization,
/// never a correctness requirement — the append path is correct against a sparse file too, merely
/// occasionally slower. So `EOPNOTSUPP` (a filesystem with no fallocate — tmpfs, some network
/// mounts) and every other errno degrade to exactly today's behavior instead of refusing to open a
/// journal. Idempotent: re-opening an already-reserved segment costs one cheap syscall.
/// The second of this journal's two audited unsafe carve-outs (see `writer.rs`'s `map_segment`
/// for the other); the crate lint is `unsafe_code = "deny"`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub(super) fn reserve_blocks(file: &std::fs::File, len: u64) {
    use std::os::unix::io::AsRawFd;
    // `posix_fallocate` takes a signed length; a segment that large is not a real configuration,
    // and there is nothing useful to do about it here (the sparse path still works).
    let Ok(len) = i64::try_from(len) else { return };
    // SAFETY: `file` is borrowed for the whole call, so its descriptor is open and valid
    // throughout. `posix_fallocate` only reserves storage — it writes no bytes, does not move the
    // file offset, and leaves the contents (zeroes, which `map_segment` relies on to detect a
    // fresh segment via the MAGIC check) unchanged.
    let rc = unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, len) };
    if rc != 0 {
        // `posix_fallocate` RETURNS the errno rather than setting `errno`. Not an error path —
        // see the "deliberately infallible" note above.
        tracing::debug!(errno = rc, len, "journal segment fallocate unavailable — staying sparse");
    }
}

#[cfg(not(unix))]
pub(super) fn reserve_blocks(_file: &std::fs::File, _len: u64) {}

/// Populate a mapping's PAGE TABLES, writable — [`reserve_blocks`]'s twin, and needed BECAUSE that
/// one is not enough.
///
/// `reserve_blocks` takes the filesystem ALLOCATION out of the write fault. It does not take out
/// the FAULT. A `MAP_SHARED` mapping starts with no page-table entries at all, so the first store
/// into each 4 KiB page still traps into the kernel whether or not its block is already on disk —
/// and that trap lands on the append path, which the `p99 < 10µs` core-hop gate measures.
///
/// MEASURED against this exact journal (256 MiB segment, `flush_every` 256, 100 000 appends,
/// `journal::writer`'s `measure_first_touch_faults_on_the_append_path`): the run touched **2 239**
/// pages and took **2 439** minor faults — one per page, plus the allocator's own. Of those, 124
/// appends exceeded 20 µs and 3 exceeded 100 µs, which is the tail this removes. ⚠ Note the honest
/// shape of that: most faults are CHEAP, so the prize is the TAIL, not 2 439 slow appends.
///
/// ⚠ **AND THE CORE-HOP A/B, which is the number that decides what this is worth.** Run on the latency box
/// per `crates/vike-core/CLAUDE.md`'s recipe — two release binaries parked side by side,
/// INTERLEAVED, medians over 8 reps each of `p99_core_hop_under_10us_with_journal`:
///
/// | median of 8 | cold | warmed |
/// |---|---|---|
/// | hop p99 | 4 408 ns | **3 912 ns** (−11 %) |
/// | hop p999 | 37 721 ns | **37 300 ns** (−1 %) |
/// | hops over 100 µs | 2 | 1 |
///
/// **The p999 does not move.**
///
/// ⚠⚠ **AND THE CONCLUSION THAT WAS DRAWN FROM IT IS WRONG — read this before acting on the table
/// above.** That A/B was read as "page faults are not the p99.9", and this doc went on to instruct
/// the next reader to "start with the SERIALIZATION, and not repeat this experiment". Both halves
/// were false, and the reason is that **the "warmed" column never removed the faults it is
/// credited with removing**:
///
///   * the FAULT COUNT (2 439 → 200) comes from `measure_first_touch_faults_on_the_append_path`,
///     which appends [`super::testutil::ingest`]'s **85 B** cancel — 100 000 of them are ~9.3 MB
///     of WAL, which fits inside ONE 8 MiB window. For that harness, warming really did remove
///     nearly every fault;
///   * the latency GATE journals a **275 B** `FillEvent` — ~28 MB of WAL, three times the window.
///     Its faults were never removed at all. The measurement crate and the gate were writing
///     different-sized records, and nobody had put the two numbers side by side.
///
/// `journal::writer`'s `measure_append_cost_split` puts them side by side. On the latency box's shielded
/// cores, medians of 8 reps, the APPEND's own cost (the gate's 275 B record, 256 MiB segment):
///
/// | | window (today) | whole mapping populated |
/// |---|---|---|
/// | serialize p50 | 401 ns | 401 ns |
/// | serialize p99.9 | 1 223 ns | 1 193 ns |
/// | frame (memcpy) p99.9 | **37 391 ns** | **982 ns** |
/// | append total p99.9 | **37 782 ns** | **1 733 ns** |
///
/// So the append tail IS first-touch faults, by a factor of 22, and it sits exactly where the
/// `journal` variant's ~37 µs core-hop p99.9 sits. **Serialization is 61 % of the append's MEDIAN
/// and 3 % of its p99.9** — replacing `serde_json::to_vec` with a reused buffer moves the median by
/// ~90 ns and the tail by nothing, which is the experiment this doc's old advice would have sent
/// the next reader to run.
///
/// What was actually wrong is [`warm_ahead_bytes`]'s subject: an 8 MiB window is ~20 ms of runway
/// at the appender's measured rate, refreshed only AFTER a blocking `msync` that takes 36-141 ms —
/// so the cursor left the window during every chunk. Sizing the window against the chunk and aiming
/// it at the appender rather than at the watermark is the fix; `sync.rs`'s `run_syncer` is where the
/// aiming happens.
///
/// # THE CORE-HOP A/B OF THAT FIX — same recipe, two binaries, INTERLEAVED, 8 reps each
///
/// the latency box at load1 3.9-11.9 with ClickHouse at 0 % CPU (the quietest the box has been measured);
/// medians over the 8 reps:
///
/// | median of 8 | before | after |
/// |---|---|---|
/// | `journal` p50 | 987 ns | 957 ns |
/// | `journal` p99 | 3 612 ns | **1 773 ns** (−51 %) |
/// | `journal` **p99.9** | **37 150 ns** | **3 342 ns** (−91 %, 11x) |
/// | `journal` max | 90 385 ns | 81 388 ns |
/// | `journal` hops over 100 µs | 2 across 8 reps | 0 |
/// | `journal-snap` p99 | 3 492 ns | 2 961 ns (−15 %) |
/// | `journal-snap` p99.9 | 6 382 ns | 6 061 ns (−5 %) |
///
/// Every one of the 8 `journal` reps improved; the before column spans 36 308-38 513 ns and the
/// after column 3 166-3 566 ns, so the bands do not touch. The baseline (journal-less) variant's own
/// p99.9 is 3-7 µs, which is where this now sits: **the journal's p99.9 debt is gone, not reduced.**
///
/// ⚠ **Read the two variants' before-columns against each other — they disagree, and the
/// disagreement is the whole shape of this defect.** `journal-snap` is `JournalConfig::at`, i.e.
/// PRODUCTION (64 MiB segments ⇒ 4 MiB chunks), and it was already at 6.4 µs before the fix. The
/// 37 µs everyone has been chasing belongs to the `journal` variant's **256 MiB** segment, whose
/// 16 MiB chunk takes ~141 ms to `msync` — four times the runway an 8 MiB window buys at the gate's
/// request/response pacing (~105 MB/s there, against the ~430 MB/s a free-running appender reaches).
/// So production was inside the window at ITS pacing and outside it in the harness's; the fix closes
/// both, and the honest summary is that it removes a large gate number and a smaller real one.
///
/// # What it COSTS, measured the same way
///
/// The window is populated at `open`, so a bigger window is a slower open.
/// `measure_first_touch_faults_on_the_append_path`, 8 interleaved reps, 256 MiB segment (window
/// 8 MiB → 64 MiB): **open 41 ms → 135 ms**, with residual minor faults 391 → 201 and the append's
/// own p99.9 4 904 ns → 1 653 ns. A PRODUCTION 64 MiB segment moves 8 MiB → 16 MiB, i.e. one eighth
/// of that extra warm work — ~14 ms, extrapolated from this slope rather than measured, because the
/// harness hardcodes the gate's segment size. Paid once per segment at session start, and on the
/// SYNCER thread for every segment after the first. Still 5x under the 694 ms that ruled out warming
/// a segment whole.
///
/// `MADV_POPULATE_WRITE` rather than `MADV_WILLNEED` or `MAP_POPULATE`, and the distinction is the
/// whole point: those two populate for READING. A file-backed shared page faulted in read-only
/// still traps on the first WRITE, because that is how the kernel learns the page became dirty. Only
/// `POPULATE_WRITE` establishes a WRITABLE entry — and it does so, in the man page's words, "just as
/// if manually writing to each page; however, avoid the actual writing", so it does NOT dirty
/// 256 MiB into writeback. That property is what makes warming the whole segment affordable.
///
/// FLOOR for [`warm_ahead_bytes`] — how far ahead the page tables are kept populated when the
/// segment is small enough that a chunk-derived window would be smaller still.
///
/// A WINDOW rather than the whole segment, and the measurement is why: warming a 256 MiB segment
/// whole cost **694 ms** at open against a **1.7 ms** baseline, to serve a run that touched 9 MiB
/// of it. 8 MiB is ~2 000 pages, about 11 ms.
///
/// `cfg(unix)` with the rest of this family: the only callers are the `#[cfg(unix)]` `warm_ahead`
/// pair below, so on a platform with no `madvise` the whole group is dead and says so in every
/// `just windows-check`.
#[cfg(unix)]
pub(super) const WARM_AHEAD_MIN: usize = 8 * 1024 * 1024;

/// How many sync chunks of runway the window carries — see [`warm_ahead_bytes`].
#[cfg(unix)]
const WARM_AHEAD_CHUNKS: usize = 4;

/// How far AHEAD of the APPENDER's cursor the page tables are kept populated, for a segment of
/// `segment_bytes`.
///
/// # ⚠ Why this is derived from the sync chunk rather than being one constant
///
/// It was `const WARM_AHEAD_BYTES: usize = 8 MiB`, and that constant's own doc argued the window
/// could not be outrun: "the syncer refreshes the window at least once per chunk and the cursor
/// never reaches its far edge". **Measured, the cursor reaches it every time**, and the reason is
/// that the refresh is not paced in BYTES — it is paced by the blocking `msync` the syncer performs
/// first (`sync.rs`'s `run_syncer`), and the appender keeps running for the whole of it.
///
/// The arithmetic, from `journal::writer`'s `measure_append_cost_split` on the latency box's shielded cores
/// (medians of 8 reps, 100 000 appends of the latency gate's own 275 B `FillEvent` record):
///
///   * the appender's median append is ~660 ns for 283 framed bytes ⇒ **~430 MB/s**, so an 8 MiB
///     window is **~20 ms** of runway;
///   * the chunk `msync` it must outlast is the one this module's `sync_chunk_bytes` sizes —
///     measured at **~36 ms** for a 4 MiB chunk (64 MiB segment) and ~141 ms for a 16 MiB chunk
///     (256 MiB segment).
///
/// So a FREE-RUNNING appender left the window mid-`msync`, every chunk, in BOTH configurations —
/// and the post-flush refresh then re-aimed it at the *synced* watermark, i.e. BEHIND a cursor that
/// had already run past it, warming pages the appender had already faulted on. That showed up as an
/// append p99.9 of **37 391 ns** (256 MiB) / **37 430 ns** (64 MiB, the production shape) against
/// **982 ns** for the same run with every page table populated at open — a 38x tail made entirely of
/// first-touch faults.
///
/// ⚠ The GATE runs slower than that (request/response pacing, ~105 MB/s), which is why only its
/// 256 MiB variant showed the full 37 µs and the 64 MiB one sat at 6.4 µs — see the A/B table on
/// [`warm_ahead`]. Both rates are real: this sizes for the faster one.
///
/// [`WARM_AHEAD_CHUNKS`] = 4 buys runway for one whole `msync` plus the chunk that follows it,
/// which is the quantity that has to be covered. ⚠ **4 rather than 2, and the free-running rate is
/// why**: at the gate's request/response pacing (~105 MB/s) two chunks would do, but a burst-loaded
/// appender reaches ~430 MB/s, where 2 x 16 MiB is 76 ms against a 141 ms `msync` and 4 x 16 MiB is
/// 152 ms — the first configuration that covers the fast case. It leaves the PRODUCTION 64 MiB
/// segment at 16 MiB and the 256 MiB latency variant at 64 MiB; the open cost that buys, and the
/// core-hop A/B it produced, are measured on [`warm_ahead`]'s doc above. Paid at session start, or
/// on the SYNCER thread for every segment after the first — never on the fold.
#[cfg(unix)]
pub(super) fn warm_ahead_bytes(segment_bytes: usize) -> usize {
    let chunks =
        super::sync::sync_chunk_bytes(segment_bytes as u64).saturating_mul(WARM_AHEAD_CHUNKS);
    chunks.max(WARM_AHEAD_MIN)
}

/// The `[offset, len)` slice to warm ahead of `from`, clamped to the mapping — `None` when there is
/// nothing left to warm.
///
/// Split out so the arithmetic has ONE home: `MmapMut` and `MmapRaw` are different types with no
/// shared trait for `advise_range`, so the two callers below are unavoidably separate and only
/// this part must not diverge between them.
#[cfg(unix)]
pub(super) fn warm_window(from: usize, len: usize) -> Option<(usize, usize)> {
    let start = from.min(len);
    let end = start.saturating_add(warm_ahead_bytes(len)).min(len);
    (end > start).then_some((start, end - start))
}

/// Populate PAGE TABLES over the window ahead of `from`, writable — [`reserve_blocks`]'s twin, and
/// needed BECAUSE that one is not enough.
///
/// `reserve_blocks` takes the filesystem ALLOCATION out of the write fault. It does not take out
/// the FAULT: a `MAP_SHARED` mapping starts with no page-table entries, so the first store into
/// each 4 KiB page still traps into the kernel whether or not its block is already on disk — and
/// that trap lands on the append path, which the `p99 < 10µs` core-hop gate measures.
///
/// MEASURED against this exact journal (256 MiB segment, `flush_every` 256, 100 000 appends, via
/// `journal::writer`'s `measure_first_touch_faults_on_the_append_path`), warming the WHOLE mapping:
///
/// | | cold | warmed |
/// |---|---|---|
/// | minor faults | 2 441 | **200** |
/// | pages touched | 2 239 | 2 239 |
/// | append p99 | 3 697 ns | 450-821 ns |
/// | append max | 1 564 689 ns | 17 684-37 200 ns |
/// | open | 1 663 µs | **694 155 µs** |
///
/// One fault per page, removed. ⚠ Read the last row as the reason this is a WINDOW: the whole-map
/// form paid 694 ms at open to serve a run that used 9 MiB of 256. ⚠ And read the p99 row with the
/// scepticism `crates/vike-core/CLAUDE.md`'s A/B recipe demands — those are single runs on a shared
/// box, and the cold `over_20us` count moved between 5 and 124 across two runs of IDENTICAL code.
/// The FAULT COUNT is the structural claim here; the latency magnitude is not yet established.
///
/// `MADV_POPULATE_WRITE` rather than `MADV_WILLNEED` or `MAP_POPULATE`, and the distinction is the
/// whole point: those two populate for READING, and a file-backed shared page faulted in read-only
/// still traps on the first WRITE, because that is how the kernel learns the page went dirty. Only
/// `POPULATE_WRITE` establishes a WRITABLE entry — and, in the man page's words, does so "just as
/// if manually writing to each page; however, avoid the actual writing", so it does not push the
/// window into writeback.
///
/// **Deliberately infallible**, exactly like [`reserve_blocks`] and for the same reason: a latency
/// optimization is never a correctness requirement. A kernel below 5.14 (no `MADV_POPULATE_WRITE`)
/// or a filesystem that refuses it degrades to precisely today's behaviour.
#[cfg(unix)]
pub(super) fn warm_ahead(map: &memmap2::MmapMut, from: usize) {
    let Some((off, len)) = warm_window(from, map.len()) else { return };
    if let Err(e) = map.advise_range(memmap2::Advice::PopulateWrite, off, len) {
        tracing::debug!(error = %e, off, len, "journal page warming unavailable — first touch will fault");
    }
}

/// [`warm_ahead`] for the SYNCER's own mapping, which is an `MmapRaw`.
///
/// This is the caller that matters for a long-lived journal: it runs after each chunk `msync` and
/// after a [`super::sync::SyncMsg::Roll`], so the window advances with the cursor and a ROLLED
/// segment is warmed too — on the syncer thread, never on the fold. The appender sends that message
/// already, so keeping the window ahead costs the hot path exactly nothing.
#[cfg(unix)]
pub(super) fn warm_ahead_raw(map: &memmap2::MmapRaw, from: usize) {
    let Some((off, len)) = warm_window(from, map.len()) else { return };
    if let Err(e) = map.advise_range(memmap2::Advice::PopulateWrite, off, len) {
        tracing::debug!(error = %e, off, len, "journal page warming unavailable — first touch will fault");
    }
}

/// Windows has no `madvise`, so there is nothing to populate — and nothing is LOST that is not
/// already lost: `reserve_blocks` is a no-op there too, so that platform has always paid first-touch
/// cost. No test RUNS on Windows anyway (`windows-cross` only compiles).
#[cfg(not(unix))]
pub(super) fn warm_ahead(_map: &memmap2::MmapMut, _from: usize) {}

/// [`warm_ahead`]'s non-unix twin for the syncer's mapping.
#[cfg(not(unix))]
pub(super) fn warm_ahead_raw(_map: &memmap2::MmapRaw, _from: usize) {}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::testutil::*;
    use crate::journal::{CommandJournal, JournalFileConfig};

    /// GATE: the warm window must outlast a whole chunk `msync` plus the chunk that follows it.
    ///
    /// This is the property [`warm_ahead_bytes`]'s doc measures, expressed as arithmetic a future
    /// edit cannot break silently. The old fixed 8 MiB constant FAILS it at both real segment sizes
    /// — which is exactly how the append tail got to 37 µs — and its own doc asserted the opposite
    /// ("the cursor never reaches its far edge") in prose, where nothing could check it.
    ///
    /// ⚠ The bound is stated against [`super::sync::sync_chunk_bytes`] rather than against a
    /// second copy of 4/16 MiB: the two are one derivation, and pinning a literal here would let
    /// `SYNC_CHUNK_DIVISOR` move the chunk out from under the window while this stayed green.
    #[cfg(unix)]
    #[test]
    fn the_warm_window_outlasts_a_chunk_msync_and_the_chunk_after_it() {
        // The two configurations that exist: `JournalConfig::at`'s production segment and the
        // `journal` latency variant's.
        for seg in [64 * 1024 * 1024usize, 256 * 1024 * 1024] {
            let chunk = crate::journal::sync::sync_chunk_bytes(seg as u64);
            let window = warm_ahead_bytes(seg);
            assert!(
                window >= 2 * chunk,
                "a {seg}-byte segment syncs in {chunk}-byte chunks, so its warm window must carry \
                 at least two of them (one to survive the blocking msync, one to reach the next \
                 refresh) — got {window}"
            );
            // ...and it must still be a WINDOW: warming a segment whole cost 694 ms at open, which
            // is what ruled that out. A quarter of the segment keeps that argument intact.
            assert!(
                window <= seg / 4,
                "the window must stay a fraction of the {seg}-byte segment, not approach it — \
                 got {window}"
            );
        }
    }

    /// GATE: the floor holds for a segment too small for the chunk derivation to matter, and the
    /// window is CLAMPED to the mapping rather than handed to `madvise` as a range past its end —
    /// which is what makes the floor safe for the KB-sized segments the unit tests use.
    ///
    /// ⚠ It asserts `>= WARM_AHEAD_MIN`, deliberately NOT the exact formula: an equality would
    /// restate [`WARM_AHEAD_CHUNKS`] as a literal here, and a constant spelled twice is the thing
    /// this file's neighbours keep being wrong about.
    #[cfg(unix)]
    #[test]
    fn a_tiny_segment_still_gets_the_floor_window() {
        assert!(warm_ahead_bytes(4096) >= WARM_AHEAD_MIN);
        assert_eq!(warm_window(16, 4096), Some((16, 4096 - 16)));
        assert_eq!(warm_window(4096, 4096), None);
    }

    /// `reserve_blocks` must leave the segment BACKED BY REAL BLOCKS, not merely sized.
    ///
    /// The distinction is invisible to `metadata().len()` (a sparse file reports full length), so
    /// this asserts on `st_blocks` — the only thing that separates "sized" from "allocated", and
    /// the exact property that keeps ext4 from doing block allocation inside an append's write
    /// fault. Unix-only: `reserve_blocks` is a no-op elsewhere and there is no portable st_blocks.
    ///
    /// Tolerant by design where it must be: a filesystem that cannot fallocate (tmpfs, some
    /// network mounts) legitimately degrades to sparse, and `reserve_blocks` is documented as
    /// best-effort — so a genuinely unsupported FS SKIPS rather than fails. `/tmp` on the CI
    /// runners is ext4, where it does apply.
    #[cfg(unix)]
    #[test]
    fn segment_blocks_are_reserved_not_sparse() {
        use std::os::unix::fs::MetadataExt;
        let dir = tmp_dir("fallocate");
        let seg = 8 * 1024 * 1024; // 8MB: big enough that sparse-vs-allocated is unambiguous
        let cfg = JournalFileConfig { segment_bytes: seg, flush_every: 256 };
        let j = CommandJournal::open(&dir, cfg).unwrap();
        drop(j);
        let md = std::fs::metadata(seg_path(&dir, 0)).unwrap();
        assert_eq!(md.len(), seg, "segment must be sized to segment_bytes");
        // st_blocks counts 512-byte units regardless of the FS block size.
        let allocated = md.blocks() * 512;
        if allocated < seg / 2 {
            // Under half allocated after a full-length fallocate means the FS ignored it entirely.
            eprintln!("skipping: filesystem did not honor fallocate ({allocated} of {seg} bytes)");
            return;
        }
        assert!(
            allocated >= seg,
            "segment should be fully block-allocated, got {allocated} of {seg} bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ROLLED segment must be block-reserved too — that is what `prepare_next` buys, and the
    /// thing `map_segment(reserve = false)` alone cannot deliver.
    ///
    /// Also pins the crash-safety property the `.prep` suffix exists for: once the roll is done,
    /// no `.prep` file is left behind masquerading as a segment.
    #[cfg(unix)]
    #[test]
    fn a_rolled_segment_is_reserved_by_the_prepared_successor() {
        use std::os::unix::fs::MetadataExt;
        let dir = tmp_dir("rollprep");
        // Small enough to roll quickly, large enough that sparse-vs-allocated is unambiguous.
        let seg = 1024 * 1024;
        let cfg = JournalFileConfig { segment_bytes: seg, flush_every: 64 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        // Append past one full segment so at least one roll happens.
        for i in 0..40_000u64 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        assert!(j.seg_idx >= 1, "expected at least one roll, still on segment {}", j.seg_idx);
        let rolled = seg_path(&dir, 1);
        drop(j);

        let md = std::fs::metadata(&rolled).unwrap();
        let allocated = md.blocks() * 512;
        if allocated < seg / 2 {
            eprintln!("skipping: filesystem did not honor fallocate ({allocated} of {seg})");
            return;
        }
        assert!(
            allocated >= seg,
            "rolled segment should be block-allocated by prepare_next, got {allocated} of {seg}"
        );
        // The prepared file must have been RENAMED, not copied/left: no `.prep` may survive for a
        // segment that is now live.
        assert!(!prep_path(&dir, 1).exists(), "a live segment must not leave its .prep behind");
        // And the records must still read back intact across the roll.
        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 40_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `.prep` file must be INVISIBLE to every segment scanner — the property that keeps a crash
    /// between preparation and roll from resetting the sequence to zero.
    #[test]
    fn a_leftover_prep_file_is_not_mistaken_for_a_segment() {
        let dir = tmp_dir("prepscan");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 4 };
        let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
        for i in 0..50 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        // Simulate the crash window: a prepared successor exists, but the roll never happened.
        std::fs::write(prep_path(&dir, 1), vec![0u8; 64 * 1024]).unwrap();
        // Reopening must resume segment 0 at seq 50 — NOT adopt the empty prepared file as the
        // highest segment and restart the sequence at 0.
        let j2 = CommandJournal::open(&dir, cfg).unwrap();
        assert_eq!(j2.seg_idx, 0, "the .prep file must not be seen as segment 1");
        assert_eq!(j2.next_seq(), 50, "resume must keep the sequence, not restart it");
        drop(j2);
        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 50);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
