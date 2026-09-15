//! Bulk/offline write profile for [`DataFusionHist`] — an explicit, opt-in ingest mode for
//! backfilling from an already-on-disk, re-fetchable source (a venue archive export), where the
//! live path's per-commit WAL + fsync durability is not the right tradeoff. NautilusTrader's
//! `ParquetDataCatalog` draws the same split between its live/streaming writer and a bulk
//! `write_data` batch path; this module is the same split for this store.
//!
//! ## The problem this exists to fix
//! Profiling a flat (all-Polymarket-markets) archive row group found the store WRITE dominated
//! ingest cost (~156 ms/100k rows vs ~38 ms/100k rows for decode+map), and that cost scales with
//! the number of DISTINCT `token_id`s in a row group (~450 on a flat day), not with row count: each
//! distinct token is its own [`super::DataFusionHist::commit_rows`] call, and each such call pays a
//! ~50-70 ms FIXED floor — `SeriesLock` acquire, a manifest read, a WAL append + fsync, a manifest
//! publish + fsync, then a WAL rewrite + fsync — three `fsync`s and about ten syscalls, regardless
//! of how few rows that token contributed to this one row group. See
//! `.superpowers/sdd/2026-07-28-poly-mm-latency-batch/ingest-profile-report.md` for the measurement
//! this module implements the "ranked fix #1" from.
//!
//! ## What changes vs the live path
//! - Rows are STAGED in memory per series `(kind, venue, symbol)` across many `stage_*` calls
//!   instead of committed one call at a time. [`BulkIngestSession::flush`] (auto-triggered by the
//!   configured [`BulkConfig`] window, via [`BulkIngestSession::end_batch`], or called explicitly)
//!   commits every currently-buffered series ONCE — collapsing what would have been N per-series
//!   `commit_rows` calls (each with its own lock + manifest read/write + WAL fsync) into one per
//!   series per window. This is where the "N-fold" win comes from, N being the window size.
//! - The per-commit WAL is SKIPPED entirely (see "Crash safety" below) — trims the remaining fixed
//!   floor further (2 of the 3 fsyncs the profiling report measured).
//!
//! ## What does NOT change
//! The [`crate::HistStore`] trait impl, `commit_rows`, `append_series`, and every byte the `wal`
//! module writes are UNTOUCHED by this module — it only calls the same lower-level primitives
//! (`SeriesLock` / `read_manifest` / `seal_into_manifest` / `write_manifest`) that `commit_rows`
//! already uses, through a sibling function ([`commit_rows_bulk`]) that never appends to a WAL. A
//! caller that never constructs a [`BulkIngestSession`] observes IDENTICAL behavior to before this
//! module existed — nothing here is reachable from an ordinary `append_*`/`HistStore` call.
//!
//! ## Crash safety (read before pointing this at a real backfill)
//! Sealing a part + publishing the manifest is already a stage-then-finalize scheme without the
//! WAL's help: [`super::manifest::seal_into_manifest`] writes new Parquet part file(s) that the
//! CURRENT (pre-flush) manifest does not reference, then [`super::manifest::write_manifest`]
//! publishes the updated manifest by atomic rename (write tmp → fsync → rename; `write_manifest`'s
//! rename has a remove-then-retry fallback, which is load-bearing on Windows — unlike POSIX,
//! `rename` there fails outright when the destination already exists). A crash at any point up to
//! (but not including) that rename leaves the OLD manifest intact and the new part file(s) simply
//! unreferenced — invisible to every read, and safe to overwrite (a retry recomputes the exact same
//! per-date part name from the manifest it re-reads, not from a directory listing, so it lands on
//! the same path and replaces the orphan). A crash AFTER the rename is a completed, durable commit.
//! There is no state where a reader observes a half-applied window: one series' flush is
//! all-or-nothing at the manifest-publish granularity, exactly like a live `commit_rows` call minus
//! its extra WAL safety net.
//!
//! Consequence for a bulk backfill run: **idempotent commit keys are what make a crash recoverable
//! here, not a WAL.** A window whose flush crashed mid-flight never recorded its commit key, so
//! simply re-running the same backfill invocation redoes exactly the series/windows that did not
//! complete and skips (no-ops) every one that did — the same commit-key idempotency contract every
//! append in this store already honors, just applied to a bigger batch per call. **What this
//! profile does NOT protect against**: an already-published series is never at risk, but a crash
//! mid-flush means the rows that series was mid-committing are simply ABSENT until the run is
//! repeated — never partially visible, never duplicated, but also not "free" the way the live WAL
//! makes a crash a pure no-loss event. That tradeoff is exactly why this is an explicit opt-in for a
//! re-runnable offline source, never the default `HistStore` path.
//!
//! ## Constructing
//! There is no boolean to flip by accident: bulk semantics only exist behind
//! [`DataFusionHist::bulk_session`], a distinct entry point the `HistStore` trait impl never calls
//! and an ordinary `append_*` caller never reaches. `BulkIngestSession` borrows the store rather
//! than owning a second handle, so a caller cannot construct one without already holding an opened
//! (live-capable) store — there is no separate "bulk store" type to open by mistake.
//!
//! ## Memory
//! [`BulkConfig`] is a MEMORY BUDGET, not "hold everything": whichever of `max_batches` /
//! `max_rows` / `max_bytes` is hit first triggers a flush of every currently-buffered series. This
//! is what keeps a wide window from reintroducing the whole-file-buffered RSS balloon a prior
//! version of the archive-ingest streaming rewrite had to fix (see `vike-backfill`'s
//! `vike_archive.rs` module doc).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use vike_model::{BookUpdate, QuoteTick, TradeTick};

use crate::hist::DataError;
use crate::hist_maint::{Durability, WriteOpts};

use super::DataFusionHist;
use super::codec::{BookCodec, BookRow, QuoteCodec, SeriesCodec, TradeCodec, book_rows};
use super::manifest::{SeriesLock, read_manifest, seal_into_manifest, write_manifest};

/// Bounds for [`BulkIngestSession`]'s auto-flush window. Byte estimates are per-row constants (a
/// coarse `size_of`-ballpark, not an exact Arrow-encoded size) — good enough to keep the window a
/// bounded memory BUDGET without adding real accounting cost to the staging hot path.
const BOOK_ROW_BYTES: usize = 64;
const TRADE_ROW_BYTES: usize = 48;
const QUOTE_ROW_BYTES: usize = 64;

/// Window knobs for [`BulkIngestSession`]'s auto-flush: whichever threshold is crossed FIRST (by
/// [`BulkIngestSession::end_batch`]) triggers a flush of every series currently buffered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkConfig {
    /// Flush after this many [`BulkIngestSession::end_batch`] calls since the last flush (the
    /// caller's own unit of work — one Parquet row group in the archive-ingest caller).
    pub max_batches: usize,
    /// Flush after this many accumulated (unflushed) rows across every buffered series.
    pub max_rows: usize,
    /// Flush after this many ESTIMATED accumulated bytes across every buffered series (see the
    /// per-row constants above) — the actual memory-budget knob.
    pub max_bytes: usize,
}

impl Default for BulkConfig {
    fn default() -> Self {
        // Defensible defaults: 20 batches / 2M rows / 256 MB (estimated), whichever comes first.
        // 256 MB comfortably covers this store's ~100-300 MB/row-group ballpark (the profiling
        // report's own numbers) while still collapsing a wide-fanout (~450 distinct token_id) day's
        // fixed-per-commit floor by roughly the window factor.
        Self { max_batches: 20, max_rows: 2_000_000, max_bytes: 256 * 1024 * 1024 }
    }
}

/// What one [`BulkIngestSession::flush`] (or a no-op [`BulkIngestSession::end_batch`]) did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BulkFlushReport {
    /// Distinct series committed this flush (0 for a no-op `end_batch` that didn't cross the
    /// window threshold, or for a flush over an empty session).
    pub series_flushed: usize,
    /// Rows written across all of them (0 for any series whose commit_key was already durable —
    /// same idempotent-no-op contract as a live `append_*` call).
    pub rows_written: usize,
}

impl BulkFlushReport {
    fn absorb(&mut self, written: usize) {
        self.series_flushed += 1;
        self.rows_written += written;
    }
}

/// A BULK/offline ingest handle over one already-`open`ed [`DataFusionHist`] — see the module doc.
/// Borrows the store (never owns a second handle), so constructing one requires an already-open,
/// live-capable store; there is no separate "bulk store" type. Not `Send`-restricted beyond what
/// `&DataFusionHist` already is — but see the module doc's exclusivity assumption: a session is
/// meant to be the only writer touching the series it stages while it is staging them.
/// Maps a series to the GROUP it commits into: `(venue, symbol) -> Option<group>`.
///
/// `None` keeps that series per-symbol (today's layout), so a resolver can migrate one venue — or
/// one family — at a time rather than as a flag day. `Arc<dyn Fn>` because a recorder holds one
/// across threads and a caller's mapping is data (a subscription config), not a type.
pub type GroupResolver = std::sync::Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

pub struct BulkIngestSession<'a> {
    store: &'a DataFusionHist,
    cfg: BulkConfig,
    /// `Some` ⇒ flush commits into grouped series (see [`DataFusionHist::bulk_session_grouped`]).
    grouping: Option<GroupResolver>,
    /// The next window index this session will stamp into a commit key on its next flush —
    /// entirely session-owned so a caller only supplies a STABLE prefix; determinism across re-runs
    /// falls out of "same input windowed the same way", not any wall-clock/random component.
    window_idx: u64,
    batches_since_flush: usize,
    rows_since_flush: usize,
    bytes_since_flush: usize,
    book: HashMap<(String, String), Vec<BookRow>>,
    trade: HashMap<(String, String), Vec<TradeTick>>,
    quote: HashMap<(String, String), Vec<QuoteTick>>,
    total_commits: usize,
    total_rows: usize,
}

impl DataFusionHist {
    /// The distinct, explicit entry point into the bulk/offline write profile (see the module doc).
    /// Borrows `self` — the ordinary `HistStore` trait surface on this same store is completely
    /// unaffected by a session's existence; nothing here flips a flag on `self`.
    pub fn bulk_session(&self, cfg: BulkConfig) -> BulkIngestSession<'_> {
        BulkIngestSession {
            store: self,
            cfg,
            grouping: None,
            window_idx: 0,
            batches_since_flush: 0,
            rows_since_flush: 0,
            bytes_since_flush: 0,
            book: HashMap::new(),
            trade: HashMap::new(),
            quote: HashMap::new(),
            total_commits: 0,
            total_rows: 0,
        }
    }

    /// [`Self::bulk_session`] that commits into GROUPED series — the flush shape storage-study
    /// item #5 exists for.
    ///
    /// `grouping(venue, symbol) -> Option<group>` decides per series whether its rows join a grouped
    /// commit (`Some`) or keep their own per-symbol series (`None`). A resolver returning `None` for
    /// everything is exactly [`Self::bulk_session`].
    ///
    /// **Staging alone does not collapse commits.** A window holding 562 symbols still issues 562
    /// `commit_rows_bulk` calls — one per series — each paying the ~30-39 ms fixed floor. Grouping
    /// merges those into ONE commit per group per window, which is where the measured ~150x write /
    /// ~7x net (`write_granularity_ab`) comes from.
    pub fn bulk_session_grouped(
        &self,
        cfg: BulkConfig,
        grouping: GroupResolver,
    ) -> BulkIngestSession<'_> {
        let mut s = self.bulk_session(cfg);
        s.grouping = Some(grouping);
        s
    }
}

impl<'a> BulkIngestSession<'a> {
    /// Stage recorded L2 book events for `(venue, symbol)` — exploded to per-level rows immediately
    /// (matching [`super::DataFusionHist::append_book_updates`]'s own encode-time shape), appended
    /// to whatever this series already has buffered. A no-op on an empty slice.
    pub fn stage_book_updates(&mut self, venue: &str, symbol: &str, updates: &[BookUpdate]) {
        if updates.is_empty() {
            return;
        }
        let rows = book_rows(updates);
        self.rows_since_flush += rows.len();
        self.bytes_since_flush += rows.len() * BOOK_ROW_BYTES;
        self.book.entry((venue.to_string(), symbol.to_string())).or_default().extend(rows);
    }

    /// Stage trade ticks for `(venue, symbol)`. A no-op on an empty slice.
    pub fn stage_trades(&mut self, venue: &str, symbol: &str, ticks: &[TradeTick]) {
        if ticks.is_empty() {
            return;
        }
        self.rows_since_flush += ticks.len();
        self.bytes_since_flush += ticks.len() * TRADE_ROW_BYTES;
        self.trade
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(ticks);
    }

    /// Stage L1 quote ticks for `(venue, symbol)`. A no-op on an empty slice.
    pub fn stage_quotes(&mut self, venue: &str, symbol: &str, ticks: &[QuoteTick]) {
        if ticks.is_empty() {
            return;
        }
        self.rows_since_flush += ticks.len();
        self.bytes_since_flush += ticks.len() * QUOTE_ROW_BYTES;
        self.quote
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(ticks);
    }

    fn should_flush(&self) -> bool {
        self.batches_since_flush >= self.cfg.max_batches
            || self.rows_since_flush >= self.cfg.max_rows
            || self.bytes_since_flush >= self.cfg.max_bytes
    }

    /// Call once per logical unit of work (e.g. one Parquet row group) after staging its rows.
    /// Auto-flushes under a `{key_prefix}:w{window}` commit key when the configured window
    /// threshold is crossed this call; otherwise a no-op ([`BulkFlushReport::default`]). Pass the
    /// SAME `key_prefix` across a re-run of the same logical backfill invocation — the window index
    /// is this session's own counter, so windows land identically as long as the caller stages
    /// batches in the same order each run (true of a deterministic file/date/token-filter replay).
    pub fn end_batch(&mut self, key_prefix: &str) -> Result<BulkFlushReport, DataError> {
        self.batches_since_flush += 1;
        if self.should_flush() { self.flush(key_prefix) } else { Ok(BulkFlushReport::default()) }
    }

    /// Force-commit everything currently buffered under `{key_prefix}:w{window}` (the window index
    /// is THIS session's own counter — see [`Self::end_batch`]), one manifest publish PER SERIES
    /// (never per row) — the collapse this whole profile exists for. Idempotent: re-flushing a
    /// series whose commit key is already durable is a no-op for that series (`rows_written`
    /// contribution 0), same contract as a live `append_*` call. Always call this once more after
    /// the last [`Self::end_batch`] of a run to flush a partial trailing window — an unflushed
    /// buffer is simply not durable yet (see the module doc's "Crash safety" section).
    pub fn flush(&mut self, key_prefix: &str) -> Result<BulkFlushReport, DataError> {
        let commit_key = format!("{key_prefix}:w{}", self.window_idx);
        self.window_idx += 1;
        let mut report = BulkFlushReport::default();

        // Merge staged buffers by TARGET SERIES before committing. Ungrouped, each (venue, symbol)
        // is its own target and this is exactly the previous per-series loop. Grouped, every symbol
        // of a group shares one target, so N per-symbol commits collapse into ONE — which is the
        // whole point: staging by itself never reduced the commit COUNT, only when they land.
        let g = self.grouping.clone();
        let target = |kind: &str, venue: &str, symbol: &str| -> (PathBuf, bool) {
            match g.as_ref().and_then(|f| f(venue, symbol)) {
                Some(group) => (self.store.group_dir(kind, venue, &group), true),
                None => (self.store.ticks_dir(kind, venue, symbol), false),
            }
        };
        merge_and_commit::<BookCodec>(
            self.store,
            self.book.drain(),
            &commit_key,
            |v, s| target("book", v, s),
            |r| &r.symbol,
            &mut report,
        )?;
        merge_and_commit::<TradeCodec>(
            self.store,
            self.trade.drain(),
            &commit_key,
            |v, s| target("trade", v, s),
            |r| &r.symbol,
            &mut report,
        )?;
        merge_and_commit::<QuoteCodec>(
            self.store,
            self.quote.drain(),
            &commit_key,
            |v, s| target("quote", v, s),
            |r| &r.symbol,
            &mut report,
        )?;

        self.batches_since_flush = 0;
        self.rows_since_flush = 0;
        self.bytes_since_flush = 0;
        self.total_commits += report.series_flushed;
        self.total_rows += report.rows_written;
        Ok(report)
    }

    /// Nothing staged and unflushed right now.
    pub fn is_empty(&self) -> bool {
        self.book.is_empty() && self.trade.is_empty() && self.quote.is_empty()
    }

    /// Running total of [`BulkFlushReport::series_flushed`] across every flush this session has
    /// issued so far — the "commits issued" figure the bulk-write measurement reports.
    pub fn total_commits(&self) -> usize {
        self.total_commits
    }

    /// Running total of [`BulkFlushReport::rows_written`] across every flush this session has
    /// issued so far.
    pub fn total_rows_written(&self) -> usize {
        self.total_rows
    }
}

/// The WAL-free sibling of [`super::DataFusionHist::commit_rows`] — see the module doc's "Crash
/// safety" section for why skipping the WAL here is safe for this profile. Shares every other step
/// (series lock, manifest read, seal, atomic manifest publish) with the live path via the exact same
/// private helpers `commit_rows` itself calls, so a flush's on-disk result (parts + manifest shape)
/// is indistinguishable from what an equivalent sequence of live `commit_rows` calls would have
/// produced — only the WAL bookkeeping between seal and publish is absent. Idempotent on
/// `commit_key` exactly like `commit_rows`: a batch key already in the manifest's commit-log is a
/// no-op (`Ok(0)`), never a per-row dedup.
fn commit_rows_bulk<C: SeriesCodec>(
    store: &DataFusionHist,
    series_dir: &Path,
    commit_key: &str,
    rows: &[C::Row],
) -> Result<usize, DataError> {
    let _guard = SeriesLock::acquire(series_dir)?;
    let mut m = read_manifest(series_dir)?;
    if m.commits.iter().any(|c| c == commit_key) {
        return Ok(0); // already durably committed (a prior flush, or a re-run) — resumable no-op
    }
    if rows.is_empty() {
        return Ok(0);
    }
    let ts: Vec<i64> = rows.iter().map(|r| C::sort_key(r).0).collect();
    let schema = C::schema();
    let written = seal_into_manifest(
        series_dir,
        &mut m,
        Some(commit_key),
        &ts,
        &schema,
        |idxs| C::columns(rows, idxs),
        WriteOpts::bulk(),
    )?;
    // TEST-ONLY: reuses the SAME crash-injection switch the live path's `commit_rows` checks (see
    // `DataFusionHist::skip_publish_for_test`) — seals the part(s) but skips publishing the
    // manifest, reproducing "crashed after seal, before publish" for the crash-safety tests below.
    if store.skip_publish_for_test.load(Ordering::SeqCst) {
        return Ok(written);
    }
    write_manifest(series_dir, &m, Durability::Bulk)?;
    Ok(written)
}

/// Merge one kind's staged buffers by target series, then commit each target ONCE.
///
/// The commit-count collapse lives here. Ungrouped, every `(venue, symbol)` resolves to its own
/// target and this is the per-series loop it replaces. Grouped, all of a group's symbols resolve to
/// the same target and their rows merge into a single commit — turning N per-symbol commits, each
/// paying the ~30-39 ms fixed floor, into one.
///
/// Grouped targets are sorted SYMBOL-MAJOR before committing, for the same reason
/// `DataFusionHist::append_grouped` does it: a one-symbol read prunes row groups by the `symbol_col`
/// statistics, and in a ts-ordered part with symbols interleaved every row group spans every symbol,
/// so nothing prunes. Ungrouped targets are left in their staged order — byte-identical to before.
fn merge_and_commit<C: SeriesCodec>(
    store: &DataFusionHist,
    staged: impl Iterator<Item = ((String, String), Vec<C::Row>)>,
    commit_key: &str,
    target: impl Fn(&str, &str) -> (PathBuf, bool),
    symbol_of: impl Fn(&C::Row) -> &str,
    report: &mut BulkFlushReport,
) -> Result<(), DataError>
where
    C::Row: Clone,
{
    // BTreeMap: deterministic commit ORDER across a run, so a re-run's part names line up.
    let mut by_target: BTreeMap<PathBuf, (bool, Vec<C::Row>)> = BTreeMap::new();
    for ((venue, symbol), rows) in staged {
        let (dir, grouped) = target(&venue, &symbol);
        by_target.entry(dir).or_insert_with(|| (grouped, Vec::new())).1.extend(rows);
    }
    for (dir, (grouped, mut rows)) in by_target {
        if grouped {
            rows.sort_by(|a, b| {
                symbol_of(a).cmp(symbol_of(b)).then_with(|| C::sort_key(a).cmp(&C::sort_key(b)))
            });
        }
        let written = commit_rows_bulk::<C>(store, &dir, commit_key, &rows)?;
        report.absorb(written);
    }
    Ok(())
}
