//! Whole-series operations on [`DataFusionHist`]: fold a per-symbol tick series into a grouped one,
//! and delete a series outright.
//!
//! Split out of `datafusion_hist.rs` by concern, bodies verbatim. Both are `pub`, both act on a
//! series that already exists, and both end in a removal — `migrate_series_to_group` after its
//! copy-and-verify, `delete_series_checked` under the series lock with its provenance assertion
//! re-checked at that instant. `delete_series` is the no-assertion form of the second.

use std::io::ErrorKind;

use vike_model::{BookUpdate, QuoteTick, TradeTick};

use crate::store::hist::{DataError, TsRange};
use crate::store::series::SeriesId;

use super::DataFusionHist;
use super::codec::{BookCodec, QuoteCodec, TradeCodec, book_updates_from_rows};
use super::manifest::{SeriesLock, read_manifest, remove_series_contents};

impl DataFusionHist {
    /// Move a PER-SYMBOL tick series into a GROUPED one, then delete the original.
    ///
    /// The repair for a store that has the same instrument in both layouts. That happens for two
    /// real reasons: a family recorded before grouping existed, and — the case this was written for —
    /// two recorder bugs that let rows escape to per-symbol series before they were fixed (a
    /// max-rows flush that skipped the group resolver, and a rotation that unmapped a symbol while
    /// its rows were still buffered).
    ///
    /// Such a store is not WRONG — `scan_*` reads both layouts, so no row is lost or hidden. It is
    /// untidy in ways that cost later: the coverage report lists one instrument twice (they are
    /// deliberately different `InstrumentKey`s), maintenance treats them as unrelated series, and
    /// every scan opens both.
    ///
    /// **Order is deliberate: copy, verify, THEN delete.** The rows are appended under a
    /// `migrate:{kind}:{symbol}` commit key (so re-running is a no-op rather than a double-append),
    /// the grouped read is checked to contain them, and only then is the source removed. A failure
    /// anywhere before that leaves BOTH copies — untidy, which is what it already was, rather than
    /// missing.
    ///
    /// Returns rows moved. `Ok(0)` when the source series does not exist or is empty.
    pub fn migrate_series_to_group(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        group: &str,
    ) -> Result<usize, DataError> {
        // ⚠ The ONE operator-typed group in the tree: `migrate_to_group --group NAME`. Refused
        // BEFORE the copy, so a hostile name costs nothing — this verb's own contract is
        // copy, verify, THEN delete, and a refusal at the append would abort mid-way having already
        // written part of a directory the reader's platform cannot open.
        //
        // ⚠ `symbol` is deliberately NOT checked here. It is not caller free text on this path:
        // `crates/vike-backfill/src/bin/migrate_to_group.rs` builds its list from
        // `store.list_series()`, so every symbol it passes was PARSED OFF DISK and is therefore
        // already a directory name that exists. Checking it would be defence against a future
        // library caller, which is a different argument and belongs with the other seven
        // symbol-taking verbs rather than smuggled in here.
        Self::refuse_a_hostile_group(group)?;
        let key = format!("migrate:{kind}:{symbol}");
        let moved = match kind {
            "quote" => {
                // The PER-SYMBOL series only. `scan_quotes` spans BOTH layouts, so using it here
                // would re-read rows already in the group and copy them back into it — which is
                // exactly what made the first version non-idempotent.
                let rows = self.scan_series::<QuoteCodec>(
                    &self.ticks_dir("quote", venue, symbol),
                    TsRange::all(),
                    symbol,
                )?;
                if rows.is_empty() {
                    return Ok(0);
                }
                // Stamp the symbol: a grouped series tells rows apart by their symbol column, and a
                // per-symbol row may legally have left it empty (the path carried it).
                let rows: Vec<QuoteTick> = rows
                    .into_iter()
                    .map(|mut q| {
                        q.symbol = symbol.to_string();
                        q
                    })
                    .collect();
                self.append_quotes_grouped(venue, group, &rows, Some(&key))?;
                rows.len()
            }
            "trade" => {
                let rows = self.scan_series::<TradeCodec>(
                    &self.ticks_dir("trade", venue, symbol),
                    TsRange::all(),
                    symbol,
                )?;
                if rows.is_empty() {
                    return Ok(0);
                }
                let rows: Vec<TradeTick> = rows
                    .into_iter()
                    .map(|mut t| {
                        t.symbol = symbol.to_string();
                        t
                    })
                    .collect();
                self.append_trades_grouped(venue, group, &rows, Some(&key))?;
                rows.len()
            }
            "book" => {
                let raw = self.scan_series::<BookCodec>(
                    &self.ticks_dir("book", venue, symbol),
                    TsRange::all(),
                    symbol,
                )?;
                let rows = book_updates_from_rows(raw, symbol)?;
                if rows.is_empty() {
                    return Ok(0);
                }
                let rows: Vec<BookUpdate> = rows
                    .into_iter()
                    .map(|mut u| {
                        u.symbol = symbol.to_string();
                        u
                    })
                    .collect();
                self.append_book_updates_grouped(venue, group, &rows, Some(&key))?;
                rows.len()
            }
            other => {
                return Err(DataError::Query(format!(
                    "migrate_series_to_group: kind `{other}` has no grouped form"
                )));
            }
        };

        // VERIFY before deleting — read the GROUP directory specifically, not `scan_*`, which spans
        // both layouts and would largely re-report what we started with.
        //
        // Counted as ROWS, not events: a `book` event is one row per level, and two identical copies
        // of one event FOLD BACK into a single event on read (the regroup keys on `(ts, seq)`), so an
        // event-count comparison reads 1 where 2 were expected. That is how the first version of this
        // check failed on the book lane.
        let gdir = self.group_dir(kind, venue, group, None);
        let landed = match kind {
            "quote" => self
                .scan_series_for::<QuoteCodec>(&gdir, TsRange::all(), symbol, Some(symbol))?
                .len(),
            "trade" => self
                .scan_series_for::<TradeCodec>(&gdir, TsRange::all(), symbol, Some(symbol))?
                .len(),
            _ => self
                .scan_series_for::<BookCodec>(&gdir, TsRange::all(), symbol, Some(symbol))?
                .len(),
        };
        if landed == 0 {
            return Err(DataError::Query(format!(
                "migrate_series_to_group({kind}/{venue}/{symbol} -> {group}): the grouped write \
                 landed no rows — refusing to delete the source"
            )));
        }

        self.delete_series(&SeriesId::per_symbol(kind, venue, symbol, None))?;
        Ok(moved)
    }

    /// Delete an entire stored series — its `kind=/venue=/symbol=[/interval=]` leaf dir with the
    /// manifest, commit-log, and every Parquet part. **Irreversible.** Removes ONLY that one
    /// series' dir; siblings are untouched. Idempotent: an absent series is `Ok(())`. Powers the
    /// Data Manager's per-series Delete action (the GUI confirms first).
    ///
    /// Takes the series lock — see [`Self::delete_series_checked`], of which this is the
    /// no-assertion form.
    pub fn delete_series(&self, id: &SeriesId) -> Result<(), DataError> {
        self.delete_series_checked(id, None)
    }

    /// [`Self::delete_series`], optionally asserting that EVERY commit key the series records
    /// carries `require_produced_by` — and doing so **under the series lock, immediately before the
    /// removal**.
    ///
    /// # ⚠ Why the lock, and why it cannot simply wrap the removal
    ///
    /// Until 2026-09-07 this was the ONE mutating verb on this type that took no
    /// [`SeriesLock`]: every append (`commit_rows`, the bulk writer), the compaction publish,
    /// `apply_retention_at`, WAL `recover_series` and `rebuild_series_manifest` take it. That was
    /// survivable while the only caller was a GUI on a developer's box; it is not survivable for a
    /// cleanup run against the box that is RECORDING, which is the case this verb now serves.
    ///
    /// The naive fix does not work, and the reason is structural rather than a platform quirk:
    /// `_manifest.lock` lives INSIDE the leaf, so a guard held across `remove_dir_all` is deleting
    /// the file it holds — and on Windows `remove_dir_all` fails outright while any file in the
    /// tree is open, which is a configuration this crate genuinely runs in (the Data Manager).
    /// Dropping the guard first reopens the race it was taken for.
    ///
    /// So: **lock, verify, remove the CONTENTS, release, remove the leaf.** Under the guard this
    /// re-reads the manifest, re-checks the assertion, and deletes every `date=` partition and
    /// `_manifest.json` — everything but the lock file itself. The guard is then dropped (closing
    /// the descriptor) and the now-nearly-empty leaf is removed.
    ///
    /// A crash between the two halves leaves a leaf holding only `_manifest.lock`. That is
    /// RECOVERABLE and INVISIBLE: `read_manifest` answers `Manifest::empty()` for a missing file so
    /// every reader sees an empty series, `find_manifest_series_dirs` skips a leaf with no manifest
    /// so `list_series` does not even report it, and re-running finishes the job (this verb stays
    /// idempotent).
    ///
    /// # ⚠ The assertion is re-checked HERE, not only at plan time
    ///
    /// A live recorder can commit a new key between a plan and its execution, and a provenance
    /// check that only ran at plan time would be a TOCTOU on the one property the whole verb turns
    /// on. `require_produced_by` is therefore evaluated inside the critical section, against the
    /// manifest as it is at that instant — and a series whose commit log is EMPTY cannot satisfy an
    /// assertion, so it is refused rather than passed (see [`Self::series_commits`]).
    pub fn delete_series_checked(
        &self,
        id: &SeriesId,
        require_produced_by: Option<&str>,
    ) -> Result<(), DataError> {
        let dir = self.series_dir_of(id);
        // Probed BEFORE the lock, deliberately: `SeriesLock::acquire` does a `create_dir_all`, so
        // locking an absent series would CREATE the leaf this call is supposed to find missing —
        // turning the idempotent no-op into a phantom-series writer.
        if !dir.exists() {
            return Ok(());
        }
        {
            let _guard = SeriesLock::acquire(&dir)?;
            if let Some(prefix) = require_produced_by {
                let commits = read_manifest(&dir)?.commit_keys();
                if commits.is_empty() {
                    return Err(DataError::Query(format!(
                        "refusing to delete {}: it records NO commit keys, so it cannot satisfy \
                         --produced-by {prefix:?} (a keyless append, or parts sealed before the \
                         commit-key metadata existed)",
                        dir.display()
                    )));
                }
                if let Some(foreign) = commits
                    .iter()
                    .find(|k| !crate::store::store_kind::key_matches_prefix(k, prefix))
                {
                    return Err(DataError::Query(format!(
                        "refusing to delete {}: its commit key {foreign:?} does not carry \
                         --produced-by {prefix:?}. Nothing was deleted.",
                        dir.display()
                    )));
                }
            }
            remove_series_contents(&dir)?;
        }
        // The guard is dropped, so the only descriptor this process held on the tree is closed and
        // `_manifest.lock` — the one file `remove_series_contents` leaves — can go with the leaf.
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(ref e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DataError::Query(format!("delete series {}: {e}", dir.display()))),
        }
    }
}
