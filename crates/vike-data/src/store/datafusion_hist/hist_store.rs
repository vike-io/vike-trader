//! `impl HistStore for DataFusionHist` — the trait seam, answered by the inherent methods.

use vike_model::{
    Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick, consolidate_quotes,
    consolidate_trades,
};

use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::funding_log::FundingRow;
use crate::perp_metrics_log::PerpMetricRow;
use crate::store::hist::{BarEdges, DataError, TsRange};
use crate::store::hist_maint::WriteProfile;
use crate::store::series::{SeriesCoverage, SeriesId};

use super::codec::{
    BarCodec, BookCodec, ChainCodec, CohortCodec, EquityCodec, ExecFillCodec, ExecOrderCodec,
    FundingCodec, PerpMetricsCodec, PropertiesCodec, QuoteCodec, TradeCodec, book_rows,
    book_updates_from_rows,
};
use super::parquet_io::interval_ms;
use super::query::Layout;
use super::{DataFusionHist, HistStore};

impl HistStore for DataFusionHist {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        // parts may arrive unordered; contract is ts-ascending (BarCodec::sort_key = (ts, 0))
        self.scan_series::<BarCodec>(&self.bars_dir(venue, symbol, interval), range, "")
    }

    /// The BOUNDED twin of `load_bars` — the same series, the same parts, the same exact
    /// `ts` filter, answered by an aggregate over the `ts` column instead of a decode of every row
    /// (see `DataFusionHist::ts_edges` in the `query` module, which carries the argument and the
    /// layouts it must survive). The trait default would be correct here and would materialise the
    /// whole range, which is the defect this method exists to remove.
    fn bar_edges(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<BarEdges, DataError> {
        self.ts_edges(&self.bars_dir(venue, symbol, interval), range)
    }

    /// The BOUNDED twin of `load_bars` for a caller that wants only the START of a range — the same
    /// series, the same parts and the same exact `ts` filter, read a block of parts at a time and
    /// stopped once `n` rows of the range are in (see `DataFusionHist::collect_head` in the `query`
    /// module, which carries the loop and why its blocks join into a prefix of the whole read). The
    /// trait default would be correct here and would materialise the whole range, which is the
    /// defect this method exists to remove.
    fn load_bars_head(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        n: usize,
    ) -> Result<Vec<Bar>, DataError> {
        // One layout, and it holds one series, so `keep` is never asked about a row.
        let bars = [Layout { dir: self.bars_dir(venue, symbol, interval), symbol: None }];
        self.collect_head::<BarCodec>(&bars, range, "", n, |_| true)
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        self.scan_quotes_capped(venue, symbol, range, None)
    }

    fn scan_quotes_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<QuoteTick>, DataError> {
        self.scan_symbol_across_layouts::<QuoteCodec>(
            "quote",
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        self.scan_trades_capped(venue, symbol, range, None)
    }

    fn scan_trades_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<TradeTick>, DataError> {
        self.scan_symbol_across_layouts::<TradeCodec>(
            "trade",
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )
    }

    // ---- store inventory / metadata: the TRAIT overrides delegate to the inherent methods -----
    // Promote the concrete `DataFusionHist::{list_series, inventory, series_gaps, coverage_report}`
    // (the inherent impl above) onto the `HistStore` trait so a `&dyn HistStore` — e.g. the datahub server answering a
    // `RemoteHistStore` — reaches the real manifest walk, not the trait default (which REFUSES the
    // catalog pair and answers the derived views empty).
    // `DataFusionHist::method(self)` resolves to the INHERENT method (inherent items shadow trait
    // items in path resolution), so this is a one-line delegation, NOT recursion.

    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        DataFusionHist::list_series(self)
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
        DataFusionHist::inventory(self)
    }

    /// The trait half of the INHERENT `DataFusionHist::series_facts` — one manifest parse, one
    /// answer, no second spelling. Promoted so a routed reader can ask for it at all.
    fn series_facts(&self, id: &SeriesId) -> Result<(SeriesCoverage, Vec<String>), DataError> {
        DataFusionHist::series_facts(self, id)
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
        DataFusionHist::series_gaps(self, id)
    }

    fn coverage_report(
        &self,
    ) -> Result<Vec<crate::store::coverage::InstrumentCoverage>, DataError> {
        DataFusionHist::coverage_report(self)
    }

    // ...and the two PROVENANCE/REMOVAL verbs, promoted for the same reason and one more: the
    // datahub's delete verb answers through `&dyn HistStore`, so without these the server would
    // reach the trait's refusing defaults while holding the one store that can actually answer.
    fn series_commits(&self, id: &SeriesId) -> Result<Vec<String>, DataError> {
        DataFusionHist::series_commits(self, id)
    }

    fn delete_series_checked(
        &self,
        id: &SeriesId,
        require_produced_by: Option<&str>,
    ) -> Result<(), DataError> {
        DataFusionHist::delete_series_checked(self, id, require_produced_by)
    }

    fn append_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<BarCodec>(
            &self.bars_dir(venue, symbol, interval),
            commit_key,
            bars,
            WriteProfile::Hot,
        )
    }

    fn append_quotes(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[QuoteTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<QuoteCodec>(
            &self.ticks_dir("quote", venue, symbol),
            commit_key,
            ticks,
            WriteProfile::Hot,
        )
    }

    fn append_trades(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TradeTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<TradeCodec>(
            &self.ticks_dir("trade", venue, symbol),
            commit_key,
            ticks,
            WriteProfile::Hot,
        )
    }

    fn append_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        let rows = book_rows(updates);
        let written_rows = self.append_series::<BookCodec>(
            &self.ticks_dir("book", venue, symbol),
            commit_key,
            &rows,
            WriteProfile::Hot,
        )?;
        // append_series (via commit_rows) returns per-level ROWS; the seam's contract is EVENTS
        // written.
        Ok(if written_rows == 0 { 0 } else { updates.len() })
    }

    fn append_depth(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        // Byte-identical to `append_book_updates` except the `kind` — same codec, same row shape,
        // same idempotency. The SERIES is the disclosure that this lane is conflated; the trait doc
        // has the argument for why that separation is not cosmetic.
        Self::refuse_a_hostile_symbol(symbol)?;
        let rows = book_rows(updates);
        let written_rows = self.append_series::<BookCodec>(
            &self.ticks_dir("depth", venue, symbol),
            commit_key,
            &rows,
            WriteProfile::Hot,
        )?;
        Ok(if written_rows == 0 { 0 } else { updates.len() })
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.scan_depth_capped(venue, symbol, range, None)
    }

    fn scan_depth_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<BookUpdate>, DataError> {
        // ⚠ The budget is in ROWS and one event is many rows — the write side explodes a
        // `BookUpdate` into one row per price level. So a budget bounds the READ, which is the
        // allocation that matters, while the event count it yields is smaller and
        // data-dependent. The cut never splits a `ts`, so it never splits an event either.
        // ⚠ The SAME layout walk as the tick verbs, not a copy of it: a copy is how the per-series
        // budget would survive here after being fixed there. Nothing writes a grouped depth part
        // today, so the group half of this read finds nothing — as it did before the walk.
        let rows = self.scan_symbol_across_layouts::<BookCodec>(
            "depth",
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )?;
        book_updates_from_rows(rows, symbol)
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        // (ts, seq) sort (BookCodec::sort_key): rows of one event share both; events order by time
        // then feed seq. sort_by_key is stable, preserving intra-event level order from the write.
        // Both layouts, like quotes/trades: the per-symbol series first (unchanged), then any
        // grouped series with the symbol predicate pushed into the read. `ctx` stays `symbol` so a
        // row whose own symbol column is absent or empty still resolves correctly.
        self.scan_book_updates_capped(venue, symbol, range, None)
    }

    fn scan_book_updates_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<BookUpdate>, DataError> {
        // ⚠ The budget is in ROWS and one event is many rows — the write side explodes a
        // `BookUpdate` into one row per price level. So a budget bounds the READ, which is the
        // allocation that matters, while the event count it yields is smaller and
        // data-dependent. The cut never splits a `ts`, so it never splits an event either.
        // ⚠ The SAME layout walk as the tick verbs, not a copy of it — `scan_depth_capped` says why.
        // The rows arrive sorted by `(ts, seq)` (`BookCodec::sort_key`), which is what the regroup
        // below keys on.
        let rows = self.scan_symbol_across_layouts::<BookCodec>(
            "book",
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )?;
        book_updates_from_rows(rows, symbol)
    }

    fn append_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[(i64, SymbolProperties)],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<PropertiesCodec>(
            &self.ticks_dir("properties", venue, symbol), // (was kind=filters; renamed with SymbolFilters→SymbolProperties)
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        self.scan_series::<PropertiesCodec>(&self.ticks_dir("properties", venue, symbol), range, "")
        // (was kind=filters; renamed with SymbolFilters→SymbolProperties)
    }

    fn append_equity(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[EquitySample],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<EquityCodec>(
            &self.ticks_dir("equity", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        self.scan_series::<EquityCodec>(&self.ticks_dir("equity", venue, symbol), range, symbol)
    }

    /// The bounded twin of `scan_equity`: the same series, the same decode argument (the symbol,
    /// standing in for the venue column), read a block of parts at a time until the budget is met.
    fn scan_equity_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<EquitySample>, DataError> {
        self.scan_one_series_capped::<EquityCodec>(
            self.ticks_dir("equity", venue, symbol),
            range,
            symbol,
            budget,
        )
    }

    fn append_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecFillRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        // The same guard `append_exec_orders` carries, for the same reason — and this side is the
        // one that MATTERS MORE, which is why it must not be left behind again.
        //
        // A reconcile pre-seed scans every `kind=exec_fill` leaf by `id.symbol` and folds the
        // trade_ids it finds into the SEEN-FILL dedup set (`vike-app`'s did, as this said in the
        // present tense until 2026-09-28; `crates/vike-core/src/journal_view.rs`'s
        // `journal_view_from_store` is that walk today, reached by no production root yet). A fill
        // that never reaches that set is
        // a fill reconciliation believes it has not booked — a `MissingFill`, which is one of the two
        // kinds `hybrid` AUTO-APPLIES. So an unattributable fill row does not merely sit unreadable:
        // it is a live-money double-book waiting for the next reconcile pass.
        //
        // ⚠ The write side and the read side must land TOGETHER. `parse_series_id` (this file) now
        // refuses such a leaf, so leaving this guard off would create a shape that writes fine and
        // then silently vanishes from `list_series` — stranding exactly the trade_ids the dedup set
        // needs. Measured before this landed: zero `symbol=` leaves across 24,699 `symbol=*`
        // directories on the live boxes, so nothing existing is affected.
        //
        // ⚠ `vike_journal::materialize`'s `materialize_once` guards its ORDER path
        // (`symbol.is_empty() || venue.is_empty()`) but not its FILL path, which keys on
        // `(f.venue, f.symbol)` — that is how this shape could still be produced.
        if symbol.is_empty() {
            return Err(DataError::Query(format!(
                "append_exec_fills({venue}): empty symbol — a per-symbol series is addressed BY its \
                 `symbol=` path segment, so an empty one is unattributable, invisible to every scan, \
                 and its trade_ids would be missing from the reconcile seen-fill set"
            )));
        }
        self.append_series::<ExecFillCodec>(
            &self.ticks_dir("exec_fill", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_exec_fills(&self, venue: &str, symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        // Every field (venue+symbol included) is a stored column, so decode ignores `ctx` — pass "".
        self.scan_series::<ExecFillCodec>(
            &self.ticks_dir("exec_fill", venue, symbol),
            TsRange::all(),
            "",
        )
    }

    /// The bounded twin of `scan_exec_fills` for a caller that wants only the START of a range of
    /// the series: the same series and decode, read a block of parts at a time and stopped once `n`
    /// rows are in — the walk `load_bars_head` takes. A COUNT, not a budget, so `n == 0` is
    /// "nothing" (`collect_head` answers it without reading) rather than the whole range.
    fn scan_exec_fills_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        n: usize,
    ) -> Result<Vec<ExecFillRow>, DataError> {
        let fills = [Layout { dir: self.ticks_dir("exec_fill", venue, symbol), symbol: None }];
        self.collect_head::<ExecFillCodec>(&fills, range, "", n, |_| true)
    }

    fn append_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecOrderRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        // REJECTS an empty `symbol` — the per-symbol twin of `append_book_updates_grouped`'s guard,
        // and for the same reason: a row that cannot be attributed must never become durable.
        //
        // A grouped series tells rows apart by their symbol COLUMN, so an empty column there is
        // unattributable. Here the symbol is the `symbol=` PATH SEGMENT, and an empty one is worse
        // than merely unaddressable: `crates/vike-data/src/store/series.rs`'s `SeriesId::group` reserves an
        // empty `symbol` as the GROUPED-series sentinel ("exactly one of `symbol`/`group` is
        // meaningful for any given series"), so
        // `parse_series_id` reads the leaf back as a `SeriesId { symbol: "", group: None }` that no
        // consumer can tell from a grouped id by the very field that doc says to tell them apart by.
        // Reconciliation queries this kind to learn what it knows; a leaf it cannot name is a row
        // that silently is not there. Enforced writer-side rather than papered over on read.
        //
        // The check is on the ARGUMENT and therefore unconditional — deliberately NOT skipped for an
        // empty `rows`, because `commit_rows` reaches `SeriesLock::acquire` (which `create_dir_all`s
        // the leaf) BEFORE it can notice the batch is empty: "no rows" is not "no partition".
        //
        // Only `symbol`, not `venue`: an empty `venue=` collides with no sentinel and the grouped
        // sibling checks no venue either, so widening here would be a local invention rather than
        // this precedent.
        if symbol.is_empty() {
            return Err(DataError::Query(format!(
                "append_exec_orders({venue}): empty symbol — a per-symbol series is addressed BY its \
                 `symbol=` path segment, so an empty one is unattributable and would be invisible to \
                 every scan"
            )));
        }
        self.append_series::<ExecOrderCodec>(
            &self.ticks_dir("exec_order", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_exec_orders(&self, venue: &str, symbol: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        self.scan_series::<ExecOrderCodec>(
            &self.ticks_dir("exec_order", venue, symbol),
            TsRange::all(),
            "",
        )
    }

    fn append_funding(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[FundingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<FundingCodec>(
            &self.ticks_dir("exec_funding", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_funding(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<FundingRow>, DataError> {
        // Every field (hash included) is a stored column, so decode ignores `ctx` — pass "".
        self.scan_series::<FundingCodec>(&self.ticks_dir("exec_funding", venue, symbol), range, "")
    }

    fn append_chain_snapshot(
        &self,
        venue: &str,
        underlying: &str,
        rows: &[ChainRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<ChainCodec>(
            &self.ticks_dir("chain", venue, underlying),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TsRange,
    ) -> Result<Vec<ChainRow>, DataError> {
        // Every field (underlying included) is a stored column, so decode ignores `ctx` — pass "".
        // `sort_by_key` on (ts, 0) is stable, preserving within-snapshot row order from the write.
        self.scan_series::<ChainCodec>(&self.ticks_dir("chain", venue, underlying), range, "")
    }

    fn append_cohort(
        &self,
        venue: &str,
        asset: &str,
        rows: &[CohortRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<CohortCodec>(
            &self.ticks_dir("cohort", venue, asset),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TsRange,
    ) -> Result<Vec<CohortRow>, DataError> {
        // Every field (asset included) is a stored column, so decode ignores `ctx` — pass "".
        // `sort_by_key` on (ts, 0) is stable, so the labels of one hour come back in the order they
        // were written: the caller's own axis/grading order, not an arbitrary one.
        self.scan_series::<CohortCodec>(&self.ticks_dir("cohort", venue, asset), range, "")
    }

    /// The bounded twin of `scan_cohort`. The walk's blocks join in the whole read's order, so the
    /// labels of one hour still come back in the order they were written.
    fn scan_cohort_capped(
        &self,
        venue: &str,
        asset: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<CohortRow>, DataError> {
        self.scan_one_series_capped::<CohortCodec>(
            self.ticks_dir("cohort", venue, asset),
            range,
            "",
            budget,
        )
    }

    fn append_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[PerpMetricRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<PerpMetricsCodec>(
            &self.ticks_dir("perp_metrics", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        // `(venue, symbol)` is the whole identity and lives in the path, so no column needs
        // re-injecting on decode — pass "".
        self.scan_series::<PerpMetricsCodec>(
            &self.ticks_dir("perp_metrics", venue, symbol),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_perp_metrics`.
    fn scan_perp_metrics_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        self.scan_one_series_capped::<PerpMetricsCodec>(
            self.ticks_dir("perp_metrics", venue, symbol),
            range,
            "",
            budget,
        )
    }

    fn resample_quotes_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let step = interval_ms(interval)?;
        let quotes = self.scan_quotes(venue, symbol, range)?; // ts-ascending
        let bars = consolidate_quotes(&quotes, step); // the parity-tested tick->bar math
        self.append_bars(venue, symbol, interval, &bars, commit_key)
    }

    fn resample_trades_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let step = interval_ms(interval)?;
        let trades = self.scan_trades(venue, symbol, range)?; // ts-ascending
        let bars = consolidate_trades(&trades, step);
        self.append_bars(venue, symbol, interval, &bars, commit_key)
    }
}
