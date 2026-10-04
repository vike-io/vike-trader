//! The WRITE path of [`DataFusionHist`]: split a batch by UTC day, seal one part per date, publish
//! the manifest once, idempotent by commit key — plus the `SeriesCodec`-generic append shape every
//! `HistStore::append_*` verb is a one-line call onto.
//!
//! Split out of `datafusion_hist.rs` by concern, bodies verbatim. This file holds the seal → WAL →
//! publish sequence (`commit_rows` / `commit_rows_inner`), the grouped verbs (`append_*_grouped`),
//! the superseding verbs (`append_*_superseding`), the key-only `spend_keys_without_rows` and the
//! external-Parquet loader `append_bars_from_parquet`. What stays in the parent is what every writer
//! shares: the supersede decision (`SupersedeStep`), the path helpers and the `HistStore` impl,
//! whose verbs reach this file through `append_series`. The bulk profile's own commit path is
//! `super::bulk`'s, not this file's.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use datafusion::arrow::array::ArrayRef;
use datafusion::arrow::datatypes::Schema;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::prelude::{ParquetReadOptions, SessionContext, col};

use vike_model::{Bar, BookUpdate, QuoteTick, TradeTick, consolidate_quotes};

use crate::hist::{DataError, HistStore, TsRange};
use crate::hist_maint::{Durability, WriteOpts, WriteProfile};
use crate::series::SeriesId;

use super::codec::{
    BarCodec, BookCodec, QuoteCodec, SeriesCodec, TradeCodec, book_rows, f64_col, i64_col,
};
use super::delta::DeltaFrame;
use super::manifest::{SeriesLock, publish, read_manifest, seal_into_manifest};
use super::wal::{wal_append, wal_rewrite_keeping_unapplied};
use super::{DataFusionHist, SupersedeStep, file_url, interval_ms, q, unlink_superseded};

impl DataFusionHist {
    // ---- ingest: split a batch by UTC day, seal one part per date, publish once (idempotent) --

    /// Append `ts.len()` rows to `series_dir`, split into one sealed part per UTC `date=`, then
    /// publish the manifest ONCE (atomic). `build_cols(idxs)` builds the Arrow columns for a row
    /// subset — the caller indexes its own slice, so this stays schema-agnostic. Idempotent by
    /// `commit_key` (batch-level, never per-row value). Returns rows written (0 if the key was
    /// already committed or the batch is empty).
    fn commit_rows<F>(
        &self,
        series_dir: &Path,
        commit_key: Option<&str>,
        ts: &[i64],
        schema: Arc<Schema>,
        build_cols: F,
        profile: WriteProfile,
    ) -> Result<usize, DataError>
    where
        F: Fn(&[usize]) -> Vec<ArrayRef>,
    {
        self.commit_rows_inner(series_dir, commit_key, None, ts, schema, build_cols, profile)
    }

    /// [`Self::commit_rows`], with an extra `supersede_key`: if `Some`, remove — in the SAME locked
    /// manifest publish that seals `rows` — every file whose ENTIRE commit-key set is exactly
    /// `[supersede_key]`. A no-op removal (no such file, or `supersede_key` is `None`) costs one extra
    /// pass over the manifest already held in memory. Physical files are unlinked only AFTER the
    /// publish durably succeeds — manifest-first, exactly like [`Self::apply_retention_at`].
    ///
    /// ⚠ **Does not short-circuit on an empty `ts`.** A settled window that genuinely has nothing new
    /// to add must still be able to clean up a stale provisional entry from an earlier, incomplete
    /// fetch of the same window — an early return here would strand that entry forever. The seal step
    /// alone is skipped when `ts` is empty (matching [`Self::commit_rows`]'s existing contract: an
    /// empty batch spends no key and writes nothing); the supersede check always runs.
    ///
    /// ⚠ **A superseding commit must be KEYED, and a keyless one is refused before anything is
    /// locked or read.** [`seal_into_manifest`] builds a part's key list as the commit key chained
    /// with the stamp, so a keyless seal would carry `[supersede_key]` ALONE — the very key set of
    /// the provisional part it had just removed. The next supersede of that key would then delete
    /// it wholesale, keyless rows included, and a repeat of the call would replace it again rather
    /// than being idempotent. No production caller does this; the refusal keeps a future one from
    /// starting. `commit_key: None` with no `supersede_key` stays the plain keyless append. The WAL
    /// replay never meets the check: a WAL record is always keyed.
    #[allow(clippy::too_many_arguments)] // commit_rows' existing 7 knobs plus supersede_key
    fn commit_rows_inner<F>(
        &self,
        series_dir: &Path,
        commit_key: Option<&str>,
        supersede_key: Option<&str>,
        ts: &[i64],
        schema: Arc<Schema>,
        build_cols: F,
        profile: WriteProfile,
    ) -> Result<usize, DataError>
    where
        F: Fn(&[usize]) -> Vec<ArrayRef>,
    {
        if let (None, Some(sk)) = (commit_key, supersede_key) {
            // NOT the supersede refusal's wording (`DataError::is_supersede_refusal`): a backfill
            // steps over a refused KEY, and this is a caller's mistake it must not step over.
            return Err(DataError::Query(format!(
                "a superseding commit must be keyed: supersede_key {sk:?} came with no commit_key, \
                 and a keyless seal would carry [{sk:?}] alone — the key set of the part it \
                 removes. Nothing was locked, read or written"
            )));
        }
        let _guard = SeriesLock::acquire(series_dir)?;
        let mut m = read_manifest(series_dir)?;
        // idempotency: a batch key already committed is a no-op (NEVER dedup by row value).
        // `has_commit` reads the key off the parts that hold its rows — see `Manifest::has_commit`
        // for why that is the same cost class as v2's top-level array and not an index build.
        if let Some(k) = commit_key
            && m.has_commit(k)
        {
            return Ok(0);
        }
        // Decide what a supersede must do BEFORE sealing anything — see `plan_supersede`'s doc for
        // why this ordering is load-bearing (checking after sealing lets the freshly-sealed part,
        // once stamped, answer its own "already folded away" question). `Err` means "already folded
        // into a multi-key part" and this whole call is refused before anything is sealed or
        // published. The WAL replay decides through the SAME step — see `SupersedeStep`.
        let step = SupersedeStep::decide(&m, supersede_key)?;

        let mut written = 0usize;
        let mut frame = DeltaFrame { version: m.version, ..Default::default() };
        if !ts.is_empty() {
            // (1) WAL the accepted append + fsync it BEFORE sealing, so a crash before the manifest
            // publish (2) is recovered on next open. Keyed appends only — keyless has no key to guard
            // a replay against (see the WAL section), so it keeps today's manifest-boundary
            // durability.
            if let Some(k) = commit_key {
                let all: Vec<usize> = (0..ts.len()).collect();
                let batch = RecordBatch::try_new(schema.clone(), build_cols(&all)).map_err(q)?;
                // `supersede_key` rides in the SAME WAL record as the rows it is paired with — see
                // `wal`'s module doc for why: without it, a crash in the exact window
                // `skip_publish_for_test` simulates below would recover the seal on reopen with no
                // memory that a supersede was ever requested. The RAW `supersede_key` goes to the
                // WAL (recovery re-derives its own step from the recovered manifest state), not
                // `step.stamp`.
                wal_append(series_dir, k, supersede_key, &batch)?;
            }
            // (2a) seal one parquet part per UTC day into the manifest struct (still in memory).
            let (sealed_written, sealed_frame) = seal_into_manifest(
                series_dir,
                &mut m,
                commit_key,
                step.stamp,
                ts,
                &schema,
                &build_cols,
                WriteOpts { profile, durability: Durability::Fsync },
            )?;
            if self.skip_publish_for_test.load(Ordering::SeqCst) {
                // TEST-ONLY: parts sealed + WAL fsynced, but the manifest is NOT published — exactly
                // the (1)→(2) crash window `commit_rows` has always reproduced (see
                // `skip_publish_for_test`'s own doc comment). ⚠ This is EXACTLY the window a
                // supersede would be LOST in if the WAL record above did not also carry
                // `supersede_key`: a crash here means `commit_key`'s rows never became durable, so a
                // retry — or a fresh `open`'s recovery sweep — must redo the WHOLE commit, supersede
                // included, from the WAL record alone (a retry through this function cannot help: it
                // would hit `has_commit(commit_key)` above and return `Ok(0)` before the supersede
                // logic below ever ran again). `wal::recover_series` is what actually finishes this
                // window on next open, replaying both the seal AND this same exact-match removal from
                // that one WAL record, through the same `SupersedeStep`.
                return Ok(sealed_written);
            }
            written = sealed_written;
            frame = sealed_frame;
        }
        // (2b) supersede: fold the removal decided above into the SAME frame the seal (if any)
        // already populated, so one publish carries both the add and the remove atomically — by
        // (name, date) IDENTITY, never by re-matching a key set, because the part just sealed may
        // itself carry `supersede_key` now (`step.stamp`). (2c) publish by APPENDING a framed delta
        // record and fsyncing it — the durability boundary. It was a whole-manifest rewrite until
        // v3; on the live box's largest series that was 104 MB per commit, and `manifest.rs`'s
        // module doc carries the measurement. The atomicity the rewrite bought comes from the
        // framing instead (`delta.rs`), and the fsync this waits on is a few hundred bytes rather
        // than the whole file. Skipped when there is nothing to publish (an empty settled batch with
        // no stale key to supersede either) — the same no-op `commit_rows` has always had for an
        // empty `ts`.
        let published = step.publish(series_dir, &mut m, frame, self.fold_bytes())?;
        // (3) the commit_key is now durable in the manifest → GC the applied WAL record(s)
        if published.published && commit_key.is_some() {
            wal_rewrite_keeping_unapplied(series_dir, &m)?;
        }
        if self.stop_after_supersede_publish_for_test.load(Ordering::SeqCst) {
            // TEST-ONLY: the publish above has, by this point, already succeeded (the new manifest is
            // durable and no longer lists any superseded file), but nothing below has unlinked the
            // superseded file(s) from disk yet. This is the exact crash window "manifest-first,
            // unlink-after" exists to make harmless — see `stop_after_supersede_publish_for_test`.
            return Ok(written);
        }
        unlink_superseded(published.to_unlink);
        Ok(written)
    }

    // ---- append_series: the SeriesCodec-generic shape behind every HistStore append_* method ---
    // (dedup site: these were 5 nearly-identical append bodies, differing only in which `codec::`
    // schema/columns trio ran). `commit_rows` above already took its `schema`/`build_cols` as
    // parameters — `append_series` just supplies them FROM the codec, so behavior (schema Arc,
    // column builders, WAL/manifest path) is unchanged; see [`codec::SeriesCodec`] for the
    // byte-identity argument. Its read twin, `scan_series`, stays in the parent beside the other
    // scan helpers.

    /// Append MANY symbols' rows to ONE grouped series in a SINGLE commit — the write shape
    /// storage-study item #5 exists for.
    ///
    /// **This, not the `group=` path, is where the win lives.** Pointing the existing per-symbol
    /// verbs at a shared directory would be strictly WORSE than today: 562 `append_quotes` calls
    /// against one series dir contend on ONE `SeriesLock` and still pay 562 commits, where today's
    /// 562 independent series at least lock independently. The 201x measured in PR #938 came from
    /// one call carrying all the rows — which is exactly what this verb is.
    ///
    /// Every row carries its own symbol (the additive `symbol_col` column), so one part holds the
    /// whole group and reads tell rows apart by that column instead of by the path.
    ///
    /// **Rejects rows with an empty symbol.** On a per-symbol path an empty symbol is legal and
    /// means "tag me from the path" — here there is no such path, so an empty symbol would make a
    /// row permanently unattributable and silently invisible to every `scan_*(venue, symbol)`.
    /// Enforced writer-side rather than papered over on read.
    /// Quotes for many symbols → one grouped series, one commit.
    pub fn append_quotes_grouped(
        &self,
        venue: &str,
        group: &str,
        ticks: &[QuoteTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_grouped::<QuoteCodec>("quote", venue, group, ticks, commit_key, |r| &r.symbol)
    }

    /// Trades for many symbols → one grouped series, one commit.
    pub fn append_trades_grouped(
        &self,
        venue: &str,
        group: &str,
        ticks: &[TradeTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_grouped::<TradeCodec>("trade", venue, group, ticks, commit_key, |r| &r.symbol)
    }

    /// Book updates for many symbols → one grouped series, one commit.
    ///
    /// **The one that matters by volume.** A Polymarket token's market is ~352,935 book rows against
    /// ~17,364 quotes and ~1,087 trades, so grouping quotes and trades alone would leave ~95% of the
    /// data still committing per symbol.
    ///
    /// Explodes to per-level rows exactly like the per-symbol path — each row now carries its own
    /// symbol, so the regroup on read reconstructs every event under the right one.
    pub fn append_book_updates_grouped(
        &self,
        venue: &str,
        group: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        if let Some(bad) = updates.iter().position(|u| u.symbol.is_empty()) {
            return Err(DataError::Query(format!(
                "append_book_updates_grouped({venue}/{group}): update {bad} has an empty symbol — a \
                 grouped series tells rows apart by their symbol column, so an empty one is \
                 unattributable and would be invisible to every scan"
            )));
        }
        let rows = book_rows(updates);
        self.append_grouped::<BookCodec>("book", venue, group, &rows, commit_key, |r| &r.symbol)
    }

    /// The generic behind [`Self::append_quotes_grouped`] / [`Self::append_trades_grouped`].
    /// Private because `SeriesCodec` is an internal seam — callers get the concrete verbs above.
    fn append_grouped<C: SeriesCodec>(
        &self,
        kind: &str,
        venue: &str,
        group: &str,
        rows: &[C::Row],
        commit_key: Option<&str>,
        symbol_of: impl Fn(&C::Row) -> &str,
    ) -> Result<usize, DataError>
    where
        C::Row: Clone,
    {
        // ⚠ The GROUP is a directory component, exactly as `symbol` is on the per-symbol verbs, and
        // it was left uncovered when those gained this refusal — declared in that change rather
        // than forgotten, because this verb takes a `group` and the helper's message says "symbol".
        //
        // ⚠ **This is the LAST line, not the first.** A refusal here does not fail loudly in the
        // live recorder: `crates/vike-data/src/rec/live_rec.rs`'s `flush_buf_with` retries once,
        // increments `discarded`, logs one `warn!` and carries on — so a hostile group would be a
        // permanent per-flush silent-loss loop behind a healthy-looking daemon. The place that
        // actually protects the recorder is the group NAME being rendered path-safe at the source
        // (`vike_recorder::resolve::group_name_for`). This catches the caller that types a
        // group directly, which today is the `migrate_to_group` bin's `--group`.
        Self::refuse_a_hostile_group(group)?;
        if let Some(bad) = rows.iter().position(|r| symbol_of(r).is_empty()) {
            return Err(DataError::Query(format!(
                "append_grouped({kind}/{venue}/{group}): row {bad} has an empty symbol — a grouped \
                 series tells rows apart by their symbol column, so an empty one is unattributable \
                 and would be invisible to every scan"
            )));
        }
        // SORT SYMBOL-MAJOR. This is the single most load-bearing line in the grouped write path,
        // and it is not tidiness: a one-symbol read prunes row groups by the `symbol_col`
        // statistics, and in a ts-ordered part with symbols interleaved EVERY row group spans every
        // symbol — each min/max covers everything and nothing prunes. Measured without it: 562
        // per-symbol scans took 20,945 ms against a grouped series vs 2,731 ms per-symbol (7.7x
        // slower), which ate almost all of the 140x write win and left a NET of 1.15x.
        //
        // `(symbol, ts)` rather than `(ts, …)` costs nothing downstream: the manifest's ts_min/max
        // come from the ts VALUES, not from row order, and every scan re-sorts its own output by
        // `C::sort_key` before returning. The archive files are `token_id`-sorted for the same
        // reason — it is what gets a 1-of-562 read down to 8.3% of compressed bytes.
        let mut ordered: Vec<usize> = (0..rows.len()).collect();
        ordered.sort_by(|&a, &b| {
            symbol_of(&rows[a])
                .cmp(symbol_of(&rows[b]))
                .then_with(|| C::sort_key(&rows[a]).cmp(&C::sort_key(&rows[b])))
        });
        let sorted: Vec<C::Row> = ordered.into_iter().map(|i| rows[i].clone()).collect();
        self.append_series::<C>(
            &self.group_dir(kind, venue, group, None),
            commit_key,
            &sorted,
            WriteProfile::Grouped,
        )
    }

    /// Append `rows` to `series_dir`, `C`-encoded. The `ts` vector `commit_rows` needs for
    /// date-splitting comes from `C::sort_key(row).0` — the same `ts` every codec's original
    /// `append_*` body computed by hand (`rows.iter().map(|r| r.ts).collect()` / `.0` for properties).
    pub(super) fn append_series<C: SeriesCodec>(
        &self,
        series_dir: &Path,
        commit_key: Option<&str>,
        rows: &[C::Row],
        profile: WriteProfile,
    ) -> Result<usize, DataError> {
        let ts: Vec<i64> = rows.iter().map(|r| C::sort_key(r).0).collect();
        self.commit_rows(
            series_dir,
            commit_key,
            &ts,
            C::schema(),
            |idxs| C::columns(rows, idxs),
            profile,
        )
    }

    /// [`Self::append_series`], with a `supersede_key` — see [`Self::commit_rows_inner`].
    fn append_series_superseding<C: SeriesCodec>(
        &self,
        series_dir: &Path,
        commit_key: Option<&str>,
        supersede_key: Option<&str>,
        rows: &[C::Row],
        profile: WriteProfile,
    ) -> Result<usize, DataError> {
        let ts: Vec<i64> = rows.iter().map(|r| C::sort_key(r).0).collect();
        self.commit_rows_inner(
            series_dir,
            commit_key,
            supersede_key,
            &ts,
            C::schema(),
            |idxs| C::columns(rows, idxs),
            profile,
        )
    }

    /// Seal `ticks` under `commit_key` — exactly like [`Self::append_quotes`] — and, in the SAME
    /// locked manifest publish, remove `supersede_key`'s prior commit from the store.
    /// Byte-identical to `append_quotes(venue, symbol, ticks, commit_key)` when `supersede_key` is
    /// `None`.
    ///
    /// **What "remove a prior commit" means.** It removes EVERY file whose commit-key set is
    /// EXACTLY `[supersede_key]` — not just one: a commit whose rows straddle a UTC-day boundary
    /// seals one part per date, all stamped with the same key, and this removes all of them in the
    /// one publish.
    ///
    /// **Refuses rather than guesses.** If `supersede_key` is found folded into some OTHER part's
    /// key set — e.g. background compaction merged the provisional part into a multi-key part
    /// alongside an unrelated commit — this returns `Err` and writes NOTHING: no rows are sealed,
    /// no key is spent, and no removal happens. Partially removing would mean either dropping rows
    /// the other key still owns, or leaving the provisional rows in place and risking a
    /// double-counted read; this call does neither.
    ///
    /// **The new part is stamped with both keys — whether or not anything was superseded.** Every
    /// part this call seals carries `supersede_key` IN ADDITION TO `commit_key`, so
    /// `has_commit(supersede_key)` is `true` afterward: the key is spent forever, never freed for
    /// reuse, and a commit under it that arrives LATER writes nothing. That is the PRE-SPEND: a
    /// canonical commit spends its provisional twin even when the twin was never written, so a
    /// provisional write that loses a race to it — or the twin's own WAL record, replayed after it
    /// — cannot seal early rows beside the settled ones. A settled window with no provisional entry
    /// therefore stores the same ROWS as before provisional commits, and one more key. The stamp is
    /// also what lets `rebuild_manifest`'s orphan-recognition rule see a crash-orphaned copy of a
    /// superseded file as contained in the new part, rather than as an unexplained stranger (see
    /// `crates/vike-data/src/datafusion_hist/manifest.rs`'s `rebuild_manifest` doc). Only a sealed
    /// part carries it: an empty batch stamps nothing.
    ///
    /// **Idempotency is unchanged.** An already-spent `commit_key` returns `Ok(0)` before the
    /// supersede is even attempted — a repeat of an already-applied commit does not re-run the
    /// removal.
    ///
    /// **An empty batch still performs the supersede.** An empty `ticks` skips the SEAL step
    /// (nothing to write), but the supersede check and removal still run — a settled fetch that
    /// genuinely found nothing new must still be able to clean up a stale provisional entry from an
    /// earlier fetch of the same window, or that entry would be stranded forever. It spends neither
    /// key, so the next settled pass retries the window.
    ///
    /// **`commit_key` must be `Some` whenever `supersede_key` is.** A keyless superseding call is
    /// REFUSED before anything is locked or read — `commit_rows_inner`'s doc says what its seal
    /// would otherwise do. The refusal is a plain `DataError::Query`, and deliberately NOT the
    /// supersede refusal `DataError::is_supersede_refusal` recognizes: that one is a property of a
    /// key a caller may step over; this one is the caller's mistake.
    ///
    /// ⚠ **Constraint: every writer on a series that uses this must be KEYED.** A keyless append
    /// (`commit_key: None`) seals a part with an EMPTY commit-key set. Compaction's key union
    /// (`compact_dir_inner`) takes the union of its inputs' commit-key sets, so merging a keyless
    /// part together with a `[supersede_key]`-only part produces a merged part whose commit-key set
    /// is STILL exactly `[supersede_key]` — indistinguishable, to the exact-match check above, from
    /// a pure provisional part. A later supersede would then delete that merged part WHOLESALE,
    /// silently destroying the keyless writer's rows along with the provisional ones. This
    /// primitive does not detect that case; it is a real constraint on any series that mixes a
    /// superseding writer with a keyless one, not merely a theoretical one.
    pub fn append_quotes_superseding(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[QuoteTick],
        commit_key: Option<&str>,
        supersede_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series_superseding::<QuoteCodec>(
            &self.ticks_dir("quote", venue, symbol),
            commit_key,
            supersede_key,
            ticks,
            WriteProfile::Hot,
        )
    }

    /// [`Self::append_bars`], with a `supersede_key` — see [`Self::append_quotes_superseding`] for
    /// the full contract (removal semantics, the refusal case, idempotency, and the keyed-writers
    /// constraint), all of which apply here unchanged.
    pub fn append_bars_superseding(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
        supersede_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series_superseding::<BarCodec>(
            &self.bars_dir(venue, symbol, interval),
            commit_key,
            supersede_key,
            bars,
            WriteProfile::Hot,
        )
    }

    /// [`Self::resample_quotes_to_bars`], with a `supersede_key` — see
    /// [`Self::append_quotes_superseding`] for the full contract (removal semantics, the refusal
    /// case, idempotency, and the keyed-writers constraint), all of which apply here unchanged.
    pub fn resample_quotes_to_bars_superseding(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        commit_key: Option<&str>,
        supersede_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let step = interval_ms(interval)?;
        let quotes = self.scan_quotes(venue, symbol, range)?;
        let bars = consolidate_quotes(&quotes, step);
        self.append_bars_superseding(venue, symbol, interval, &bars, commit_key, supersede_key)
    }

    /// Spend `keys` in `id`'s commit log WITHOUT writing a row or a part — the key-only marker a
    /// day-chunked ingest records for a day it was told holds nothing
    /// (`docs/superpowers/specs/2026-10-02-oanda-empty-days-design.md` §3). Returns how many keys
    /// were newly spent.
    ///
    /// # What it writes
    ///
    /// Under [`SeriesLock`] it re-reads the manifest, drops every key that is already spent (on a
    /// part or as an orphan) and every repeat within `keys`, and publishes the rest as ONE delta
    /// frame carrying them as `keys_add` — the manifest v3 hatch `docs/decisions/0060-…` reserved
    /// for its Q2 and that nothing wrote until this. Replay folds such a frame into
    /// `Manifest::orphan_commits`, and a fold serializes that list into the base, so a key spent
    /// here answers [`Self::series_has_commit`] across a fold and a reopen exactly like a key that
    /// rides on a part. With nothing left to spend it publishes NOTHING: no frame, no version bump.
    ///
    /// # What it deliberately does not do
    ///
    /// - **No WAL record.** The publish is the durability boundary and there is no row to replay: a
    ///   crash before it loses only the keys, and a lost marker costs one request on the next run.
    /// - **No part, so nothing that reads parts sees it**: coverage, gaps, reads, compaction and
    ///   retention are unchanged. A rebuild ([`Self::rebuild_series_manifest`]) derives keys from
    ///   part footers and so DROPS every key spent here; [`Self::plan_series_manifest_rebuild`]
    ///   counts the empty markers among them as a note rather than a loss.
    /// - **Not a [`HistStore`] verb.** It writes no row of any kind, and its one caller already holds
    ///   a `&DataFusionHist`.
    ///
    /// ⚠ **A key spent here is as permanent as any other spent key** — nothing in this workspace
    /// retires a single key, and `vike-cli data hist rm` of the whole series is the only remedy. The
    /// CALLER owns the proof that the key is true; for an empty marker that proof is ORDER (a later
    /// window of the same request held data), never a time margin. This method checks nothing about
    /// the keys' meaning.
    pub fn spend_keys_without_rows(
        &self,
        id: &SeriesId,
        keys: &[&str],
    ) -> Result<usize, DataError> {
        match &id.group {
            Some(g) => Self::refuse_a_hostile_group(g)?,
            None => Self::refuse_a_hostile_symbol(&id.symbol)?,
        }
        if keys.is_empty() {
            return Ok(0);
        }
        let dir = self.series_dir_of(id);
        let _guard = SeriesLock::acquire(&dir)?;
        let mut m = read_manifest(&dir)?;
        let mut fresh: Vec<String> = Vec::new();
        for k in keys {
            if !m.has_commit(k) && !fresh.iter().any(|f| f == k) {
                fresh.push((*k).to_string());
            }
        }
        if fresh.is_empty() {
            return Ok(0);
        }
        // `publish` wants the change ALREADY applied to `m` (a base-pending series writes `m`
        // whole), and the frame describing it — the same contract `seal_into_manifest` keeps.
        m.orphan_commits.extend(fresh.iter().cloned());
        m.version += 1;
        let frame =
            DeltaFrame { version: m.version, keys_add: fresh.clone(), ..Default::default() };
        publish(&dir, &mut m, frame, Durability::Fsync, self.fold_bytes())?;
        Ok(fresh.len())
    }

    /// Ingest an EXTERNAL Parquet file (a venue export or the Python bar-cache) as bars for
    /// `(venue, symbol, interval)`. Reads `[ts, open, high, low, close, volume]` (funding = None),
    /// then appends via [`HistStore::append_bars`] — so it date-partitions and is idempotent by
    /// `commit_key` like any other append. `ts` is expected as Int64 epoch-ms. This keeps DataFusion
    /// contained in vike-data: consumers hand it a path, not a parquet reader.
    pub fn append_bars_from_parquet(
        &self,
        path: &Path,
        venue: &str,
        symbol: &str,
        interval: &str,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let url = file_url(path);
        let batches = self.rt.block_on(async move {
            let ctx = SessionContext::new();
            let df = ctx
                .read_parquet(vec![url], ParquetReadOptions::default())
                .await
                .map_err(q)?
                .select_columns(&["ts", "open", "high", "low", "close", "volume"])
                .map_err(q)?
                .sort_by(vec![col("ts")])
                .map_err(q)?;
            df.collect().await.map_err(q)
        })?;
        let mut bars = Vec::new();
        for b in &batches {
            let (ts, open, high, low, close, volume) = (
                i64_col(b, "ts")?,
                f64_col(b, "open")?,
                f64_col(b, "high")?,
                f64_col(b, "low")?,
                f64_col(b, "close")?,
                f64_col(b, "volume")?,
            );
            for i in 0..b.num_rows() {
                bars.push(Bar {
                    ts: ts.value(i),
                    open: open.value(i),
                    high: high.value(i),
                    low: low.value(i),
                    close: close.value(i),
                    volume: volume.value(i),
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                });
            }
        }
        self.append_bars(venue, symbol, interval, &bars, commit_key)
    }
}
