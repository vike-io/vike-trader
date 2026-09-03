//! Maintenance surface for the DataFusion hist store (feature `hist-datafusion`): the write
//! profile, compaction config, and retention policy — the knobs for
//! [`crate::DataFusionHist::compact_series`] and [`crate::DataFusionHist::apply_retention`] — plus
//! the whole-store rollups: a [`crate::SeriesId`] (what [`crate::DataFusionHist::list_series`] enumerates),
//! a [`MaintenanceConfig`] + [`MaintenanceReport`] for the one-shot
//! [`crate::DataFusionHist::run_maintenance`] pass, and its per-series breakdown
//! [`SeriesMaintenance`]. The background driver that runs that pass on a timer lives in
//! [`crate::hist_sched`].
//!
//! These are BACKEND ops (not part of the [`crate::HistStore`] read/ingest seam), so they stay
//! inherent on `DataFusionHist` rather than on the trait. Kept in their own module so the hot
//! `datafusion_hist.rs` body stays about the read/write path.

use crate::series::SeriesId; // hoisted to the feature-free base; `SeriesMaintenance` names it

/// How a Parquet part is written. `Hot` = the live append firehose (fast, light zstd); `Sealed` =
/// a compacted archival file (heavier zstd + bounded row groups so page-level `ts` pruning fires).
///
/// Encoding/compression are BYTE-level only — every f64 is bit-preserved across both profiles, so
/// the store's `to_bits()` parity gate holds regardless of which profile wrote the part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteProfile {
    /// Live append: single batch, light compression.
    Hot,
    /// Compaction output: bounded row groups, heavier compression.
    Sealed,
    /// Grouped append: light compression like [`WriteProfile::Hot`], but with **bounded row
    /// groups**, because a grouped part is read one symbol at a time.
    ///
    /// A grouped series holds every symbol of its group in one part, and a one-symbol read prunes
    /// row groups by the `symbol_col` statistics. Pruning can only skip whole ROW GROUPS — so under
    /// `Hot`'s unbounded groups (arrow's 1,048,576-row default) any part below a million rows is a
    /// SINGLE row group and pruning skips nothing at all. Measured: 112,400 rows wrote as exactly
    /// **1 row group**, and per-symbol reads stayed 2.7x slower than the per-symbol layout even
    /// with the predicate pushed into DataFusion — because every read still had to touch the whole
    /// part.
    ///
    /// The bound cuts both ways: smaller groups prune better but cost metadata and compression
    /// ratio, and DuckDB measures row groups under 5,000 rows at "5-10x" worse. See
    /// [`GROUPED_ROW_GROUP_ROWS`].
    Grouped,
}

/// Rows per row group for [`WriteProfile::Grouped`].
///
/// Sized so one symbol's contiguous run (grouped parts are sorted symbol-major) lands in a FEW row
/// groups out of many — that is what makes the `symbol_col` statistics selective. 8,192 is a common
/// Arrow/Parquet granularity and sits above the ~5,000-row cliff DuckDB measures at "5-10x" worse.
/// The reference point in the other direction is the `data.vike.io` archive, whose ~1M-row groups
/// get a 1-of-562-token read down to 8.3% of compressed bytes — on a **74M-row** day file. A bound
/// only means anything RELATIVE to part size, and this store's parts are far smaller than that.
pub const GROUPED_ROW_GROUP_ROWS: usize = 8_192;

/// How hard a write path works to survive a POWER LOSS (as opposed to a process crash, which the
/// page cache survives unaided and for which no `fsync` is ever needed).
///
/// This is orthogonal to [`WriteProfile`]: the profile decides how a part is ENCODED, this decides
/// what is made durable before the manifest that names it is published. Both write paths uphold the
/// same invariant — **a manifest entry is never published before the part it names is durable** —
/// they just differ in what happens if the manifest publish itself is lost:
///
/// - [`Durability::Fsync`] (live append, WAL recovery, compaction): the part AND the directories
///   that name it are fsynced, and so is the manifest, so a published commit stays published.
/// - [`Durability::Bulk`] (bulk import): the part is still fsynced — that is the invariant, and it
///   is what stops a durable manifest entry from outliving a truncated part — but the manifest
///   publish is not. Losing the publish reverts the series to its previous version, and the import
///   is keyed and idempotent, so the re-run simply redoes that commit.
///
/// Skipping the part fsync in either mode would reintroduce the exact incoherence this enum was
/// added to remove: a durable manifest entry pointing at a part that is still only in page cache,
/// unrecoverable because its commit key is already recorded and so the retry is a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Fsync the part, its directories, and the manifest. Used by every path whose input is gone
    /// once it is written — the live recorder above all (there is no second copy of a live tape).
    Fsync,
    /// Fsync the part only. Used by bulk import, whose source file is still on disk: a lost publish
    /// costs a re-run, not data.
    Bulk,
}

/// How to write a part: its encoding ([`WriteProfile`]) and how durable the surrounding publish is
/// ([`Durability`]). Bundled because every seal site needs both and they are always chosen together
/// — a live append is `Hot` + `Fsync`, a bulk flush is `Hot` + `Bulk`, a compaction is `Sealed` +
/// `Fsync`. Passing them as one value also keeps the seal signature under the argument-count lint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOpts {
    pub profile: WriteProfile,
    pub durability: Durability,
}

impl WriteOpts {
    /// The live append firehose: light compression, everything fsynced. Also the WAL-recovery
    /// replay, which is republishing exactly what a live append would have.
    pub fn live() -> Self {
        WriteOpts { profile: WriteProfile::Hot, durability: Durability::Fsync }
    }
    /// A bulk-import flush: same encoding as live, but only the part is fsynced (see
    /// [`Durability::Bulk`]).
    pub fn bulk() -> Self {
        WriteOpts { profile: WriteProfile::Hot, durability: Durability::Bulk }
    }
    /// Compaction output: heavier compression + bounded row groups, everything fsynced (compaction
    /// REPLACES parts, so a half-published merge must not survive).
    pub fn sealed() -> Self {
        WriteOpts { profile: WriteProfile::Sealed, durability: Durability::Fsync }
    }
}

/// Compaction knobs. A `date=` partition holding at least `min_parts` fragments is merged into
/// sealed, ts-sorted parts of about `target_bytes` each (the spec targets 256–512 MB), never
/// decoding more than `max_merge_rows` rows at a time.
///
/// **The two size knobs answer different questions and must not be conflated.** `target_bytes` is
/// about the FILES: how big should a sealed part be, and which parts are already big enough to
/// leave alone. `max_merge_rows` is about MEMORY: how much may one merge decode at once. Sizing a
/// writer's memory cap reads the second, never the first.
#[derive(Debug, Clone, Copy)]
pub struct CompactionConfig {
    /// Target sealed-file size in bytes. A part at or over it is finished and skipped, so this sets
    /// the size compaction converges toward — it says nothing about memory (see `max_merge_rows`).
    pub target_bytes: u64,
    /// Minimum fragment count in a `date=` before it is worth compacting.
    pub min_parts: usize,
    /// **The memory bound**: the most rows one merge may decode. A merge is materialized whole (the
    /// output is ts-sorted, and a sort cannot stream), so this is what stands between a compaction
    /// pass and the OOM killer.
    ///
    /// Rows rather than bytes, because compressed size is a poor proxy for Arrow's in-memory form:
    /// measured on the CI box's Polymarket book series (2026-08-04), the parts were only **2.5x**
    /// compressed, yet a pass bounded at 64 MB of compressed input peaked at **8.95 GB** — the
    /// blow-up is dictionary/RLE decoding, the sort's copy, and per-file buffering across the ~950
    /// files a byte budget admits. The same series costs roughly **1 KB of peak RSS per row**, so
    /// the 1,000,000 default lands near 1 GB for the widest rows this store holds and far less for
    /// bars or trades. Measure before raising it: the ratio is a property of the DATA, not of this
    /// code.
    pub max_merge_rows: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self { target_bytes: 384 * 1024 * 1024, min_parts: 4, max_merge_rows: 1_000_000 }
    }
}

/// Source-rank precedence for the OPT-IN compaction supersession pass
/// ([`crate::DataFusionHist::compact_series_superseding`]). Orders the WRITERS that target the same
/// series by precedence — highest FIRST — so that when two overlapping capture windows record the
/// SAME natural key (`(ts, seq)` for a book event, `ts` for a trade/quote/bar), exactly one row
/// survives per the highest-precedence source and the superseded duplicate is dropped instead of
/// double-counted in a scan/replay.
///
/// A part's source is derived from its commit-key namespace: the four Polymarket writers each key
/// their appends with a DISJOINT prefix on the SAME `venue=polymarket/symbol=<token_id>` series
/// (`RecorderSink` → `live-…`, `pmxt_backfill` → `pmxt:…`, `clickhouse_poly_backfill` →
/// `clickhouse:…`, `poly_reparse` → its own), so an operator expresses "live beats pmxt beats
/// clickhouse" as an ordered prefix list. This is why supersession is a maintenance operation the
/// operator turns on for the PM series and NOT part of the always-on
/// [`crate::DataFusionHist::run_maintenance`] pass — it CHANGES stored output (drops rows), whereas
/// the default `compact_series` only re-groups fragments and is byte-identical.
///
/// Per-row source is recovered by TAGGING each part's rows with the part's rank BEFORE the
/// merge-sort — no codec/schema change (the row schema carries no source column). See
/// [`SourceRankPolicy::rank_of`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceRankPolicy {
    /// Commit-key prefixes in DESCENDING precedence (index 0 = highest precedence). A part whose
    /// commit key starts with `prefixes[k]` has rank `k`; a LOWER rank wins a collision. A part
    /// matching NO prefix takes the lowest precedence (`prefixes.len()`). An EMPTY policy ranks
    /// every part `0`, so nothing is ever superseded (a safe no-op).
    pub prefixes: Vec<String>,
}

impl SourceRankPolicy {
    /// Build a policy from source key-prefixes in DESCENDING precedence (highest first), e.g.
    /// `SourceRankPolicy::new(["live-", "pmxt:", "clickhouse:"])`.
    pub fn new<I, S>(prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { prefixes: prefixes.into_iter().map(Into::into).collect() }
    }

    /// The rank (precedence, LOWER = higher) of a part from its `commit_keys` — the BEST (minimum)
    /// rank over all the part's keys (so a part that has already been compacted to carry keys from
    /// several sources is treated as its highest-precedence source, i.e. is the HARDEST to
    /// supersede). A key matches a prefix by `str::starts_with`; the FIRST prefix it matches (in the
    /// descending-precedence order) is that key's rank. No key matches any prefix → the lowest
    /// precedence, `prefixes.len()`.
    pub fn rank_of(&self, commit_keys: &[String]) -> usize {
        commit_keys
            .iter()
            .filter_map(|k| self.prefixes.iter().position(|p| k.starts_with(p.as_str())))
            .min()
            .unwrap_or(self.prefixes.len())
    }
}

/// Retention: drop data older than a cutoff. `before_ts` is an explicit epoch-ms cutoff
/// (deterministic — preferred for tests + reproducible jobs); `max_age_ms` derives the cutoff from
/// wall-clock now. If both are `None`, retention is a no-op.
#[derive(Debug, Clone, Copy, Default)]
pub struct RetentionPolicy {
    /// Drop parts entirely before this epoch-ms (exclusive on `ts_max`).
    pub before_ts: Option<i64>,
    /// Drop parts older than this many ms relative to `now`.
    pub max_age_ms: Option<i64>,
}

impl RetentionPolicy {
    /// Resolve the epoch-ms cutoff: a part with `ts_max < cutoff` is dropped. `before_ts` wins if
    /// set; otherwise `now_ms - max_age_ms`. `None` = nothing to prune.
    pub fn cutoff(&self, now_ms: i64) -> Option<i64> {
        self.before_ts.or_else(|| self.max_age_ms.map(|age| now_ms - age))
    }
}

/// What a compaction pass did (per series).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionReport {
    /// Fragment parts consumed (removed from the manifest + disk).
    pub parts_merged: usize,
    /// Sealed parts produced.
    pub parts_written: usize,
    /// Rows carried through into the sealed part(s). The default `compact_series` never adds or
    /// drops rows, so this equals the input row count; the opt-in
    /// [`crate::DataFusionHist::compact_series_superseding`] pass drops superseded duplicates, so
    /// this is the KEPT count and [`CompactionReport::rows_superseded`] holds the dropped count.
    pub rows: usize,
    /// Rows DROPPED as source-superseded duplicates by the opt-in
    /// [`crate::DataFusionHist::compact_series_superseding`] pass. Always `0` for the default
    /// `compact_series` / [`crate::DataFusionHist::run_maintenance`] path (which keeps every row).
    pub rows_superseded: usize,
}

/// What a retention pass dropped (per series).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Part files removed from the manifest + disk.
    pub files_dropped: usize,
    /// `date=` directories emptied and unlinked.
    pub dates_dropped: usize,
    /// Rows removed.
    pub rows_dropped: usize,
}

/// Knobs for a whole-store maintenance pass ([`crate::DataFusionHist::run_maintenance`] and the
/// [`crate::MaintenanceScheduler`] that drives it on a timer): the compaction config applied to every
/// series, plus an OPTIONAL retention policy. `retention: None` = compact only, never prune.
#[derive(Debug, Clone, Default)]
pub struct MaintenanceConfig {
    /// Compaction knobs applied to every series each pass.
    pub compaction: CompactionConfig,
    /// If set, retention is applied to each series AFTER its compaction; `None` = never prune.
    pub retention: Option<RetentionPolicy>,
}

/// One series' outcome within a [`MaintenanceReport`] (its compaction + retention result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesMaintenance {
    /// Which series this row is for.
    pub series: SeriesId,
    /// What compaction did to it this pass.
    pub compaction: CompactionReport,
    /// What retention dropped from it this pass (zero if the config had no policy).
    pub retention: PruneReport,
}

/// The aggregate result of one [`crate::DataFusionHist::run_maintenance`] pass over every series.
/// Holds both the per-series breakdown (in `list_series` order) and store-wide totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    /// Per-series outcomes, in the (sorted) order [`crate::DataFusionHist::list_series`] returned.
    pub series: Vec<SeriesMaintenance>,
    /// Compaction totals summed across every series visited.
    pub compaction: CompactionReport,
    /// Retention totals summed across every series visited.
    pub retention: PruneReport,
    /// Series whose maintenance FAILED this pass, as `(series, error)` — one entry per series that
    /// errored, in visit order.
    ///
    /// A failure here is ISOLATED: the pass logs it, records it, and moves to the next series. It
    /// used to abort the whole pass through a `?`, which meant one persistently-broken series
    /// silently disabled compaction and retention for the ENTIRE store — no part ever merged again,
    /// nothing ever pruned, and (because [`crate::MaintenanceScheduler`] discarded the error without
    /// logging it) no signal anywhere that maintenance had stopped running.
    pub failed: Vec<(SeriesId, String)>,
}

impl MaintenanceReport {
    /// Number of series visited this pass (== `self.series.len()`).
    pub fn series_visited(&self) -> usize {
        self.series.len()
    }

    /// Fold one series' outcome into the running totals and record its per-series row. Used by
    /// [`crate::DataFusionHist::run_maintenance`] as it walks the store.
    pub(crate) fn absorb(&mut self, entry: SeriesMaintenance) {
        self.compaction.parts_merged += entry.compaction.parts_merged;
        self.compaction.parts_written += entry.compaction.parts_written;
        self.compaction.rows += entry.compaction.rows;
        self.compaction.rows_superseded += entry.compaction.rows_superseded;
        self.retention.files_dropped += entry.retention.files_dropped;
        self.retention.dates_dropped += entry.retention.dates_dropped;
        self.retention.rows_dropped += entry.retention.rows_dropped;
        self.series.push(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_rank_first_matching_prefix_wins_and_best_key_ranks_the_part() {
        // live > pmxt > clickhouse, in that descending precedence.
        let policy = SourceRankPolicy::new(["live-", "pmxt:", "clickhouse:"]);
        // a single-key part ranks by that key's namespace
        assert_eq!(policy.rank_of(&["live-polymarket-TOK-book-1-2-3".to_string()]), 0);
        assert_eq!(policy.rank_of(&["pmxt:book:TOK:2026-07-01T00".to_string()]), 1);
        assert_eq!(policy.rank_of(&["clickhouse:trade:TOK:2026-07-01".to_string()]), 2);
        // a part carrying keys from SEVERAL sources (already compacted) takes its BEST (min) rank —
        // it is the hardest to supersede, never mis-ranked to its weakest source.
        assert_eq!(
            policy.rank_of(&[
                "pmxt:book:TOK:h".to_string(),
                "live-x".to_string(),
                "clickhouse:y".to_string(),
            ]),
            0
        );
    }

    #[test]
    fn source_rank_unmatched_key_is_lowest_precedence() {
        let policy = SourceRankPolicy::new(["live-", "pmxt:"]);
        // a key matching NO configured prefix → prefixes.len() (below every named source)
        assert_eq!(policy.rank_of(&["mystery-source:1".to_string()]), 2);
        // an empty part (keyless append, no commit_keys at all) also takes the lowest precedence
        assert_eq!(policy.rank_of(&[]), 2);
    }

    #[test]
    fn empty_policy_ranks_everything_zero_so_nothing_is_superseded() {
        // an empty prefix list is a no-op policy: every part is rank 0, so within any collision run
        // all rows share the minimum rank and none is dropped (byte-identical to plain compaction).
        let policy = SourceRankPolicy::default();
        assert_eq!(policy.rank_of(&["live-x".to_string()]), 0);
        assert_eq!(policy.rank_of(&["anything".to_string()]), 0);
        assert_eq!(policy.rank_of(&[]), 0);
    }
}
