//! Compaction for [`DataFusionHist`]: merge the small fragments of each `date=` partition into
//! sealed, sorted parts — ts order for a per-symbol series, SYMBOL-MAJOR for a grouped one (see
//! `super::sort_for_merge`) — with the decode → sort → re-encode rewrite that upgrades older-schema
//! parts on the way out, and its opt-in source-ranked twin that drops superseded duplicates.
//!
//! Split out of `datafusion_hist.rs` by concern, bodies verbatim. The two `compact_roundtrip*`
//! methods are the `kind` → codec dispatch (the runtime string off the `kind=` path segment becomes
//! a type here, and nowhere else), `compact_dir_inner` is the plan / merge / publish pass, and
//! `compact_series` / `compact_series_superseding` are the public entry points. The pure helpers a
//! pass leans on (`plan_merge_groups`, `encode_chunked`, `supersede_by_rank`, `write_parquet`) stay
//! in the parent, each with its own test module. `run_maintenance`, which calls this pass over the
//! whole store beside retention, is `super::maintenance`'s.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use datafusion::arrow::datatypes::Schema;
use datafusion::arrow::record_batch::RecordBatch;

use crate::store::hist::DataError;
use crate::store::hist_maint::{
    CompactionConfig, CompactionReport, Durability, SourceRankPolicy, WriteProfile,
};

use super::codec::{
    BarCodec, BookCodec, ChainCodec, CohortCodec, EquityCodec, ExecFillCodec, ExecOrderCodec,
    FundingCodec, PerpMetricsCodec, PropertiesCodec, QuoteCodec, SeriesCodec, TradeCodec,
};
use super::delta::DeltaFrame;
use super::manifest::{self, SeriesLock, merge_tmp_name, publish, read_manifest};
use super::{
    DataFusionHist, encode_chunked, file_url, fsync_dir, grouped_symbol_of, part_dir,
    plan_merge_groups, sort_for_merge, supersede_by_rank, write_parquet,
};

impl DataFusionHist {
    /// Decode the given parts to domain rows, sort them, and RE-ENCODE with the CURRENT schema for
    /// `kind` — the decode-time schema-upgrade compaction path (book-recording plan Task 4). Parts
    /// are read per file (schema-tolerant, via [`Self::read_batches_per_file`]), so inputs written
    /// under an older schema revision (missing a later-added nullable column) decode under their own
    /// schema; re-encoding through `C::schema()` + `C::columns()` (dispatched below by `kind`) writes
    /// the merged part under today's schema, upgrading them. The f64 decode→encode is bit-exact
    /// (`Float64Array::value(i)` → `Float64Array::from`), so the store's `to_bits()` parity gate
    /// holds across a compaction. The sort ([`sort_for_merge`]) is stable, keeping input (file)
    /// order among equal-key rows. `grouped` says the parts come from a `group=` directory, which
    /// is merged SYMBOL-MAJOR rather than in ts order — see [`sort_for_merge`]. The returned `bool`
    /// is [`Self::compact_roundtrip_generic`]'s "grouped, but no row symbol" flag.
    fn compact_roundtrip(
        &self,
        kind: &str,
        urls: Vec<String>,
        grouped: bool,
    ) -> Result<(Arc<Schema>, Vec<RecordBatch>, bool), DataError> {
        // Dispatch: `kind` is a runtime string (from `SeriesId`/the on-disk `kind=` path segment),
        // so the match is the unavoidable runtime->type boundary; each arm is otherwise a one-liner
        // onto the ONE generic rewrite (`compact_roundtrip_generic`), byte-identical to the former
        // per-kind bodies (same decode fns via `SeriesCodec`, same sort key, same schema/columns).
        // Ticks/equity re-encode with NO symbol/venue column (see `quote_columns`/`equities_to_batch`
        // etc.), so the decode-time context is irrelevant here — pass ""; a later real scan
        // re-injects the true symbol/venue from its own argument.
        match kind {
            "bar" => self.compact_roundtrip_generic::<BarCodec>(urls, "", grouped),
            "quote" => self.compact_roundtrip_generic::<QuoteCodec>(urls, "", grouped),
            "trade" => self.compact_roundtrip_generic::<TradeCodec>(urls, "", grouped),
            "properties" => self.compact_roundtrip_generic::<PropertiesCodec>(urls, "", grouped), // (was kind=filters; renamed with SymbolFilters→SymbolProperties)
            "equity" => self.compact_roundtrip_generic::<EquityCodec>(urls, "", grouped),
            // Execution trade-log kinds (Tier-2). Every field is a stored column (venue+symbol
            // included), so the `ctx=""` re-encode is lossless — see the codec note.
            "exec_fill" => self.compact_roundtrip_generic::<ExecFillCodec>(urls, "", grouped),
            "exec_order" => self.compact_roundtrip_generic::<ExecOrderCodec>(urls, "", grouped),
            // Realized funding (Tier-2): every field is a stored column, so the `ctx=""` re-encode is
            // lossless — see the codec note.
            "exec_funding" => self.compact_roundtrip_generic::<FundingCodec>(urls, "", grouped),
            // Option-chain snapshots (PIT options surface): every field is a stored column
            // (underlying included), so the `ctx=""` re-encode is lossless — see the codec note.
            "chain" => self.compact_roundtrip_generic::<ChainCodec>(urls, "", grouped),
            // Cohort open interest (the graded positioning panel): every field is a stored column
            // (asset included), so the `ctx=""` re-encode is lossless — see the codec note.
            "cohort" => self.compact_roundtrip_generic::<CohortCodec>(urls, "", grouped),
            // Perp market-context metrics: `(venue, symbol)` is the path and nothing else is an
            // identity column, so the `ctx=""` re-encode is lossless — see the codec note.
            "perp_metrics" => self.compact_roundtrip_generic::<PerpMetricsCodec>(urls, "", grouped),
            // Per-level ROWS are the domain unit for the book kind — a row-level rewrite preserves
            // every event (no regroup needed); `BookCodec::sort_key` is `(ts, seq)`, keeping rows of
            // one event adjacent and in feed order, exactly as `scan_book_updates` does.
            // `depth` shares BookCodec: same rows, different (conflating) lane — see `append_depth`.
            "book" | "depth" => self.compact_roundtrip_generic::<BookCodec>(urls, "", grouped),
            other => Err(DataError::Query(format!("compaction: unknown series kind {other:?}"))),
        }
    }

    /// Decode every batch, sort ([`sort_for_merge`]: by `C::sort_key`, or `(symbol, sort_key)` in a
    /// grouped series), and re-encode ONE part under `C`'s CURRENT schema — the shared
    /// decode→sort→re-encode rewrite behind every `compact_roundtrip` arm.
    ///
    /// A GROUPED directory of a kind with no row symbol (`SeriesCodec::ROW_SYMBOL` is `None`; no
    /// writer produces one) merges in `sort_key` order and returns `true` so the caller can say so,
    /// once per pass. It is NOT refused here, unlike on the superseding path
    /// ([`grouped_symbol_of`]): this merge keeps every row whatever the order, so a refusal would
    /// protect nothing and would also skip the retention `run_maintenance` runs after it.
    fn compact_roundtrip_generic<C: SeriesCodec>(
        &self,
        urls: Vec<String>,
        ctx: &str,
        grouped: bool,
    ) -> Result<(Arc<Schema>, Vec<RecordBatch>, bool), DataError> {
        let (symbol_of, unkeyed) = match (grouped, C::ROW_SYMBOL) {
            (true, Some(symbol)) => (Some(symbol), false),
            (true, None) => (None, true),
            (false, _) => (None, false),
        };
        let batches = self.read_batches_per_file(urls)?;
        let mut rows: Vec<C::Row> = Vec::new();
        for b in &batches {
            rows.extend(C::decode(b, ctx)?);
        }
        sort_for_merge::<C, _>(&mut rows, |row| row, symbol_of);
        let schema = C::schema();
        Ok((schema.clone(), encode_chunked::<C>(&rows, &schema)?, unkeyed))
    }

    /// SOURCE-RANKED supersession twin of [`Self::compact_roundtrip`] — the opt-in variant that,
    /// after tagging each part's rows with that part's source `rank` (parallel to `urls`, in the
    /// same order), keeps ONE row per natural key by the lowest rank present and drops the
    /// superseded duplicates. Same runtime `kind`→type dispatch as `compact_roundtrip`; each arm is
    /// a one-liner onto [`Self::compact_roundtrip_superseding_generic`]. Returns the re-encoded part
    /// PLUS the count of rows dropped as superseded. (`ctx=""` for every arm, exactly like
    /// `compact_roundtrip` — see its note.) `grouped` says the parts come from a `group=` directory,
    /// where a row's natural key carries its symbol — see [`supersede_by_rank`].
    fn compact_roundtrip_superseding(
        &self,
        kind: &str,
        urls: Vec<String>,
        ranks: &[usize],
        grouped: bool,
    ) -> Result<(Arc<Schema>, Vec<RecordBatch>, usize), DataError> {
        let ranked: Vec<(String, usize)> = urls.into_iter().zip(ranks.iter().copied()).collect();
        match kind {
            "bar" => self.compact_roundtrip_superseding_generic::<BarCodec>(ranked, "", grouped),
            "quote" => {
                self.compact_roundtrip_superseding_generic::<QuoteCodec>(ranked, "", grouped)
            }
            "trade" => {
                self.compact_roundtrip_superseding_generic::<TradeCodec>(ranked, "", grouped)
            }
            "properties" => {
                self.compact_roundtrip_superseding_generic::<PropertiesCodec>(ranked, "", grouped)
            }
            "equity" => {
                self.compact_roundtrip_superseding_generic::<EquityCodec>(ranked, "", grouped)
            }
            "exec_fill" => {
                self.compact_roundtrip_superseding_generic::<ExecFillCodec>(ranked, "", grouped)
            }
            "exec_order" => {
                self.compact_roundtrip_superseding_generic::<ExecOrderCodec>(ranked, "", grouped)
            }
            "exec_funding" => {
                self.compact_roundtrip_superseding_generic::<FundingCodec>(ranked, "", grouped)
            }
            "chain" => {
                self.compact_roundtrip_superseding_generic::<ChainCodec>(ranked, "", grouped)
            }
            // ⚠ `CohortCodec::sort_key` is `(ts, 0)`, so a supersession RUN is a whole HOUR — every
            // label, axis, grading and basis recorded at that ts. [`supersede_by_rank`] keeps every
            // row sharing the run's minimum rank, so a single-source series (which is all this kind
            // has today) drops nothing; but a SECOND source ranked above the first would supersede
            // that hour whole, taking axes and gradings the winning source never served. Opt-in
            // only — plain `compact_series` cannot reach this arm — and the reason a cohort store
            // fed by two sources wants `compact_series`, not its superseding twin.
            "cohort" => {
                self.compact_roundtrip_superseding_generic::<CohortCodec>(ranked, "", grouped)
            }
            // `PerpMetricsCodec::sort_key` is `(ts, 0)` and one funding interval is exactly ONE row
            // here, so a supersession run is a single observation and the higher-ranked source
            // simply wins it — the cohort arm's whole-hour hazard above has no analogue.
            "perp_metrics" => {
                self.compact_roundtrip_superseding_generic::<PerpMetricsCodec>(ranked, "", grouped)
            }
            "book" | "depth" => {
                self.compact_roundtrip_superseding_generic::<BookCodec>(ranked, "", grouped)
            }
            other => Err(DataError::Query(format!("compaction: unknown series kind {other:?}"))),
        }
    }

    /// Decode each `(url, rank)` part PER FILE (so a row keeps its part's source rank — the row
    /// schema carries no source column, so the tag must be attached before the merge), STABLE-sort
    /// the tagged rows by `C::sort_key`, then [`supersede_by_rank`] keeps one source per natural-key
    /// run and re-encodes ONE part under `C`'s current schema. Byte-identical to
    /// [`Self::compact_roundtrip_generic`] when no run holds a cross-source collision (every row in
    /// a run shares the run's minimum rank → nothing dropped, same stable order).
    ///
    /// `grouped`: the natural key of a `group=` series is `(symbol, sort_key)`, so the precedence
    /// contest is per symbol there — [`grouped_symbol_of`] refuses a grouped kind without a row
    /// symbol before a single part is read.
    fn compact_roundtrip_superseding_generic<C: SeriesCodec>(
        &self,
        ranked_urls: Vec<(String, usize)>,
        ctx: &str,
        grouped: bool,
    ) -> Result<(Arc<Schema>, Vec<RecordBatch>, usize), DataError> {
        let symbol_of = grouped_symbol_of::<C>(grouped)?;
        let mut ranked: Vec<(usize, C::Row)> = Vec::new();
        for (url, rank) in ranked_urls {
            // per-file read keeps the part→rank association the flat `read_batches_per_file` loses
            let batches = self.read_batches_per_file(vec![url])?;
            for b in &batches {
                for row in C::decode(b, ctx)? {
                    ranked.push((rank, row));
                }
            }
        }
        // The SAME stable merge order as the non-superseding path (`sort_for_merge` is shared), so
        // a collision-free series stays byte-identical to a plain compaction of it.
        sort_for_merge::<C, _>(&mut ranked, |(_, row)| row, symbol_of);
        let (rows, superseded) = supersede_by_rank::<C>(ranked, symbol_of);
        let schema = C::schema();
        Ok((schema.clone(), encode_chunked::<C>(&rows, &schema)?, superseded))
    }

    /// Merge the small fragments in each `date=` partition of a series into one sealed, ts-sorted
    /// part (reduces read fan-out). Manifest-first publish: the new part + rewritten manifest land
    /// atomically, THEN the superseded fragments are unlinked (a crash leaves orphans the READ path
    /// cannot see, never a dangling manifest reference — and, since manifest recovery reads the
    /// directory instead, orphans it can now tell apart; see [`manifest::rebuild_manifest`]). The
    /// commit-log is untouched — compaction never drops
    /// or resurrects a committed batch (must-fix #5). Row content is preserved EXACTLY (every
    /// duplicate kept); [`Self::compact_series_superseding`] is the opt-in variant that drops
    /// source-superseded duplicates.
    pub fn compact_series(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        interval: Option<&str>,
        cfg: &CompactionConfig,
    ) -> Result<CompactionReport, DataError> {
        self.compact_series_inner(kind, venue, symbol, interval, cfg, None)
    }

    /// OPT-IN source-ranked window supersession compaction — the same per-`date=` merge as
    /// [`Self::compact_series`], but when two overlapping capture windows record the SAME natural
    /// key (`(ts, seq)` for a book event; `ts` for a trade/quote/bar) exactly one row survives per
    /// `policy`'s source precedence and the superseded duplicate is DROPPED (counted in
    /// [`CompactionReport::rows_superseded`]) rather than double-counted in a later scan/replay. A
    /// part's source is derived from its commit-key namespace ([`SourceRankPolicy::rank_of`]); rows
    /// are tagged with their part's rank BEFORE the merge-sort (the row schema carries no source
    /// column, so this needs no codec/schema change).
    ///
    /// This CHANGES stored output, so it is a deliberate maintenance operation the operator runs for
    /// a multi-writer series (the four Polymarket writers on one `venue=polymarket/symbol=<token_id>`
    /// series) — NOT part of the always-on [`Self::run_maintenance`] pass. When `policy` produces no
    /// cross-source collision in any natural-key run (e.g. a single-source series, or an empty
    /// policy) it drops nothing and is byte-identical to [`Self::compact_series`].
    pub fn compact_series_superseding(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        interval: Option<&str>,
        cfg: &CompactionConfig,
        policy: &SourceRankPolicy,
    ) -> Result<CompactionReport, DataError> {
        self.compact_series_inner(kind, venue, symbol, interval, cfg, Some(policy))
    }

    /// Shared body of [`Self::compact_series`] (`ranks = None`) and
    /// [`Self::compact_series_superseding`] (`ranks = Some`). With `None` the per-date rewrite is the
    /// duplicate-preserving [`Self::compact_roundtrip`]; with `Some(policy)` it is the
    /// source-superseding [`Self::compact_roundtrip_superseding`], each part's rank derived from its
    /// `commit_keys`. Everything else (per-`date=` threshold, manifest-first publish, commit-key
    /// union carried forward, unlink-after-publish) is identical for both.
    fn compact_series_inner(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        interval: Option<&str>,
        cfg: &CompactionConfig,
        ranks: Option<&SourceRankPolicy>,
    ) -> Result<CompactionReport, DataError> {
        // A `symbol=` path, never a `group=` one — hence `grouped: false`.
        self.compact_dir_inner(
            &self.series_dir(kind, venue, symbol, interval, None),
            kind,
            false,
            cfg,
            ranks,
        )
    }

    /// [`Self::compact_series_inner`] over an explicit series DIR, so a GROUPED series
    /// (`group=…`, which has no `symbol=` segment) can be compacted too. Maintenance resolves the
    /// dir from the `SeriesId` — see `series_dir_of`.
    /// ## The lock is held for the PLAN and the PUBLISH, never for the merge
    ///
    /// This used to hold [`SeriesLock`] across the whole decode→sort→re-encode→write loop. On a
    /// large series that outlasts `SeriesLock::acquire`'s ~4 s spin, so a concurrent
    /// `RecorderSink` flush times out — and its failure path **discards the buffer**, up to
    /// `max_rows` (5,000) rows. Measured on the CI box (2026-08-02): **110,354 rows discarded in about an
    /// hour** from one 52 MB `kind=book` series, while `hist_sched`'s own doc claimed compaction was
    /// "safe to run alongside live appends… loses no rows". True while compaction is fast; false
    /// exactly when a series is big enough to be worth compacting.
    ///
    /// So: take the lock to decide, RELEASE it to do the work, take it again to swap.
    ///
    /// **Why the unlocked merge is safe.** Compaction removes exactly the input files it read, BY
    /// NAME, and adds one output containing their rows. A concurrent append creates a differently
    /// named part, so it is untouched by the swap and survives. The publish phase re-reads the
    /// manifest and verifies every input is still listed; if any vanished (another pass merged them,
    /// or retention pruned them) that date is ABANDONED and its orphan output deleted, rather than
    /// publishing a manifest that references files someone else already removed.
    ///
    /// Output names carry a `part-c` prefix, so they cannot collide with an append's `part-NNNNN`
    /// even though the merge runs unlocked.
    ///
    /// **And they are not written under that name.** The merge writes
    /// [`manifest::MERGE_TMP_PREFIX`]`+part-c…` and the publish RENAMES it, so for the whole unlocked
    /// window the output is distinguishable from the inputs it duplicates by name alone. That is not
    /// cosmetic: the inputs are still live in the manifest and hold the same rows, so anything
    /// reading the DIRECTORY (as manifest recovery must) would otherwise count them twice — measured
    /// as 34–68% row inflation across a SIGKILL soak. `rebuild_manifest` carries the other half of
    /// the argument, for the window after this publish and before the unlinks below.
    ///
    /// `grouped` is true exactly when `series_dir` is a `group=` leaf (one part, many symbols). The
    /// caller knows it from the `SeriesId`; nothing here re-derives it from the path.
    pub(super) fn compact_dir_inner(
        &self,
        series_dir: &Path,
        kind: &str,
        grouped: bool,
        cfg: &CompactionConfig,
        ranks: Option<&SourceRankPolicy>,
    ) -> Result<CompactionReport, DataError> {
        let series_dir = series_dir.to_path_buf();
        let mut report = CompactionReport::default();
        if !series_dir.exists() {
            return Ok(report);
        }

        // ---- PHASE 1: PLAN (locked, fast — a manifest read and some arithmetic) ----------------
        struct DatePlan {
            date: String,
            /// The input part names, in manifest order — the identity the publish phase re-verifies.
            inputs: Vec<String>,
            urls: Vec<String>,
            file_ranks: Vec<usize>,
            ts_min: i64,
            ts_max: i64,
            commit_keys: Vec<String>,
            out_name: String,
        }

        let plans: Vec<DatePlan> = {
            let _guard = SeriesLock::acquire(&series_dir)?;
            let m = read_manifest(&series_dir)?;
            let dates: BTreeSet<String> = m.files.iter().map(|f| f.date.clone()).collect();
            let mut plans = Vec::new();
            let mut group_seq = 0usize;
            for date in dates {
                let idxs: Vec<usize> = m
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.date == date)
                    .map(|(i, _)| i)
                    .collect();
                if idxs.len() < cfg.min_parts {
                    continue; // not worth compacting this date yet
                }
                let dir = part_dir(&series_dir, &date);
                for group in plan_merge_groups(&dir, &m.files, &idxs, cfg) {
                    let mut commit_keys: Vec<String> =
                        group.iter().flat_map(|&i| m.files[i].commit_keys.clone()).collect();
                    commit_keys.sort();
                    commit_keys.dedup();
                    plans.push(DatePlan {
                        urls: group
                            .iter()
                            .map(|&i| file_url(&dir.join(&m.files[i].name)))
                            .collect(),
                        file_ranks: match ranks {
                            None => Vec::new(),
                            Some(p) => {
                                group.iter().map(|&i| p.rank_of(&m.files[i].commit_keys)).collect()
                            }
                        },
                        ts_min: group.iter().map(|&i| m.files[i].ts_min).min().expect("non-empty"),
                        ts_max: group.iter().map(|&i| m.files[i].ts_max).max().expect("non-empty"),
                        inputs: group.iter().map(|&i| m.files[i].name.clone()).collect(),
                        // `part-c` prefix: cannot collide with an append's `part-NNNNN`, which is
                        // what makes it safe to write this while unlocked. The `-NNN` suffix keeps
                        // one pass's several groups apart; `next_part_name` ignores the whole name
                        // either way (it wants a bare u64, and neither form parses as one).
                        out_name: format!("part-c{:08}-{group_seq:03}.parquet", m.version + 1),
                        commit_keys,
                        date: date.clone(),
                    });
                    group_seq += 1;
                }
            }
            plans
        }; // <-- lock released; the expensive part runs without it

        if plans.is_empty() {
            return Ok(report);
        }

        // ---- PHASE 2: MERGE (UNLOCKED — the decode/sort/re-encode that used to block writers) ---
        struct Merged {
            plan: DatePlan,
            rows: usize,
            superseded: usize,
        }
        let mut done: Vec<Merged> = Vec::new();
        // Whether this pass already said that a grouped dir merged without a symbol key — ONCE per
        // series per pass, however many dates it merges.
        let mut warned_unkeyed = false;
        for plan in plans {
            // decode→sort→re-encode with the current schema — bounded to ONE date (NOT a global
            // ORDER BY over the series), and upgrades any older-schema inputs on the way out. With a
            // supersession policy, tag each part's rows with its source rank (from that part's
            // commit-key namespace) and drop cross-source duplicates.
            let (schema, batches, superseded) = match ranks {
                None => {
                    let (schema, batches, unkeyed) =
                        self.compact_roundtrip(kind, plan.urls.clone(), grouped)?;
                    if unkeyed && !warned_unkeyed {
                        warned_unkeyed = true;
                        tracing::warn!(
                            series = %series_dir.display(),
                            kind,
                            "compaction: a GROUPED series of a kind whose rows carry no symbol — \
                             merged in ts order, every row kept; a one-symbol read cannot prune it, \
                             and a store source policy would refuse to supersede it"
                        );
                    }
                    (schema, batches, 0)
                }
                Some(_) => self.compact_roundtrip_superseding(
                    kind,
                    plan.urls.clone(),
                    &plan.file_ranks,
                    grouped,
                )?,
            };
            let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
            if rows == 0 {
                continue;
            }
            // Under the UNPUBLISHED name — the merge output and every input it copied are in this
            // directory together until phase 3 renames it, and anything that reads the directory
            // rather than the manifest (there is one such caller: `rebuild_manifest`) has to be able
            // to tell them apart. See [`manifest::MERGE_TMP_PREFIX`].
            write_parquet(
                &part_dir(&series_dir, &plan.date).join(merge_tmp_name(&plan.out_name)),
                schema,
                batches,
                WriteProfile::Sealed,
                &plan.commit_keys,
            )?;
            done.push(Merged { plan, rows, superseded });
        }

        if done.is_empty() {
            return Ok(report);
        }
        // TEST-ONLY: park HERE — every merge output is written, no lock is held, and the publish
        // has not begun. That is the window an append has to be able to land in, and holding it
        // open is what lets a test WITNESS the overlap instead of racing for it. See
        // `pause_in_merge_for_test`. The guard is dropped before the wait so a second compaction
        // meets an empty seam rather than this thread's mutex.
        let paused = self.pause_in_merge_for_test.lock().expect("pause seam mutex").take();
        if let Some((parked, resume)) = paused {
            let _ = parked.send(());
            let _ = resume.recv();
        }
        if self.stop_after_merge_for_test.load(Ordering::SeqCst) {
            // TEST-ONLY: the merge outputs are on disk and NOTHING is published — the exact window a
            // SIGKILL mid-compaction leaves. See `stop_after_merge_for_test`.
            return Ok(report);
        }

        // ---- PHASE 3: PUBLISH (locked, fast — re-read, verify, swap) ---------------------------
        let _guard = SeriesLock::acquire(&series_dir)?;
        // RE-READ: the manifest may have advanced while we merged (that is the point of releasing).
        let mut m = read_manifest(&series_dir)?;
        let mut to_delete: Vec<PathBuf> = Vec::new();
        let mut drop_idx: BTreeSet<usize> = BTreeSet::new();
        let mut merged_entries: Vec<manifest::FileEntry> = Vec::new();

        for d in done {
            let dir = part_dir(&series_dir, &d.plan.date);
            // Every input must still be listed. If one is not, somebody else consumed it and our
            // output would reference rows they have already re-homed — abandon this date rather than
            // publish a manifest that disagrees with the disk.
            let idxs: Vec<usize> = d
                .plan
                .inputs
                .iter()
                // Match on (name, DATE): part names are unique within a `date=` dir, NOT across the
                // series — `part-00001.parquet` exists under every date, so matching on name alone
                // picks the first file with that name in ANY date and drops the wrong one.
                .filter_map(|name| {
                    m.files.iter().position(|f| &f.name == name && f.date == d.plan.date)
                })
                .collect();
            let tmp = dir.join(merge_tmp_name(&d.plan.out_name));
            if idxs.len() != d.plan.inputs.len() {
                tracing::warn!(
                    series = %series_dir.display(),
                    date = %d.plan.date,
                    "compaction: inputs changed under an unlocked merge — abandoning this date's \
                     output (it will be retried next pass)"
                );
                let _ = std::fs::remove_file(&tmp);
                continue;
            }
            // PUBLISH the output by rename, while holding the lock and BEFORE the manifest that
            // names it. Until this instant the file carried [`manifest::MERGE_TMP_PREFIX`] and was
            // provably not part of the series; after it, the manifest write below makes it so. A
            // rename that fails abandons this date exactly like a failed verify — better to redo the
            // merge next pass than to leave a final-named orphan a rebuild would count twice.
            //
            // The remove-then-retry is the same fallback [`write_manifest`] uses, and it is not
            // theoretical here: `rename` REPLACES on unix but FAILS on Windows when the destination
            // exists, and a destination CAN exist — as an orphan left by a build that wrote the
            // final name directly, or by a crash between this rename and the publish below. It is
            // never a LIVE part: `out_name` embeds `m.version + 1`, and every publish bumps the
            // version, so no manifest this store ever wrote can name the file being replaced.
            // Without the fallback such a date would abandon on every pass, forever.
            let out_path = dir.join(&d.plan.out_name);
            let mut renamed = std::fs::rename(&tmp, &out_path);
            if renamed.is_err() && out_path.exists() {
                let _ = std::fs::remove_file(&out_path);
                renamed = std::fs::rename(&tmp, &out_path);
            }
            if let Err(e) = renamed {
                tracing::warn!(
                    series = %series_dir.display(),
                    date = %d.plan.date,
                    error = %e,
                    "compaction: could not publish the merge output — abandoning this date"
                );
                let _ = std::fs::remove_file(&tmp);
                continue;
            }
            // On POSIX the rename is durable only once the containing directory is fsynced, and the
            // manifest published below is about to name it.
            fsync_dir(&dir)?;
            for &i in &idxs {
                to_delete.push(dir.join(&m.files[i].name));
                drop_idx.insert(i);
            }
            merged_entries.push(manifest::FileEntry {
                name: d.plan.out_name,
                date: d.plan.date,
                ts_min: d.plan.ts_min,
                ts_max: d.plan.ts_max,
                rows: d.rows,
                commit_keys: d.plan.commit_keys,
            });
            report.parts_merged += idxs.len();
            report.parts_written += 1;
            report.rows += d.rows;
            report.rows_superseded += d.superseded;
        }

        if merged_entries.is_empty() {
            return Ok(report); // every date was abandoned
        }
        // rebuild files: drop the merged inputs, add the sealed outputs
        let removed: Vec<(String, String)> =
            drop_idx.iter().map(|&i| (m.files[i].name.clone(), m.files[i].date.clone())).collect();
        let mut files: Vec<manifest::FileEntry> = std::mem::take(&mut m.files)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !drop_idx.contains(i))
            .map(|(_, f)| f)
            .collect();
        files.extend(merged_entries.iter().cloned());
        m.files = files;
        m.version += 1;
        let version = m.version;
        // One frame for the whole swap: the merge outputs added and their inputs removed. Removals
        // are named `(name, date)` because part names are unique within a `date=` dir and not
        // across a series — the same pairing the verify above matches on, for the same reason.
        //
        // ⚠ The merge output's NAME embeds `m.version + 1` as read at PLAN time, so a version that
        // did not advance monotonically across the log would let one publish name the file another
        // is replacing. That is why `DeltaFrame::version` is the version AFTER the frame and why
        // replay assigns it rather than incrementing.
        publish(
            &series_dir,
            &mut m,
            DeltaFrame {
                version,
                files_add: merged_entries,
                files_rm: removed,
                ..Default::default()
            },
            Durability::Fsync,
            self.fold_bytes(),
        )?;
        // manifest-first: only NOW unlink the superseded fragments
        for p in to_delete {
            let _ = std::fs::remove_file(p);
        }
        Ok(report)
    }
}
