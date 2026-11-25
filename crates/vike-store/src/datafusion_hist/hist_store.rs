//! `impl HistoryStore for HistoryStoreDataFusion` — the trait seam, answered by the inherent methods.

use vike_marketdata::{
    Bar, L2BookUpdate, TickQuote, TickTrade, consolidate_quotes, consolidate_trades,
};
use vike_model::{EquitySample, SymbolProperties};

use crate::hist_maint::WriteProfile;
use vike_data::chain_log::ChainRow;
use vike_data::cohort_log::CohortRow;
use vike_data::funding_log::ExecutionHistoryFundingRow;
use vike_data::option_trade_log::OptionTradeRow;
use vike_data::perp_metrics_log::PerpMetricRow;
use vike_data::store::hist::{BarDay, BarEdges, HistoryStoreError, TimestampRange};
use vike_data::store::series::{SeriesCoverage, SeriesId};
use vike_data::text_log::TextRow;
use vike_data::{
    ExecFillRow, ExecForecastRow, ExecOrderRow, FinancialFactRow, InstitutionalHoldingRow,
    TextScoreRow,
};
use vike_model::StoreKindId;

use super::codec::{
    BarCodec, BookCodec, ChainCodec, CohortCodec, EquityCodec, ExecFillCodec, ExecOrderCodec,
    FinancialFactCodec, ForecastCodec, FundingCodec, InstitutionalHoldingCodec, OptionTradeCodec,
    PerpMetricsCodec, PropertiesCodec, QuoteCodec, TextCodec, TextScoreCodec, TradeCodec,
    book_rows, book_updates_from_rows,
};
use super::parquet_io::interval_ms;
use super::query::Layout;
use super::{HistoryStore, HistoryStoreDataFusion, PriceWrite, WrittenPrices};

impl HistoryStore for HistoryStoreDataFusion {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        // The head read with no budget: the one read that keeps one bar per time, the newest write's
        // (`SeriesCodec::ONE_ROW_PER_KEY`), ts-ascending.
        let bars = [Layout { dir: self.bars_dir(venue, symbol, interval), symbol: None }];
        self.collect_head::<BarCodec>(&bars, range, "", usize::MAX, |_| true)
    }

    /// The BOUNDED twin of `load_bars` — the same series, the same parts, the same exact
    /// `ts` filter, answered by an aggregate over the `ts` column instead of a decode of every row
    /// (see `HistoryStoreDataFusion::timestamp_edges` in the `query` module, which carries the argument and the
    /// layouts it must survive). The trait default would be correct here and would materialise the
    /// whole range, which is the defect this method exists to remove.
    fn bar_edges(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<BarEdges, HistoryStoreError> {
        self.timestamp_edges(&self.bars_dir(venue, symbol, interval), range)
    }

    /// The BOUNDED twin of `load_bars` for a caller that wants only the START of a range — the same
    /// series, the same parts and the same exact `ts` filter, read a block of parts at a time and
    /// stopped once `n` rows of the range are in (see `HistoryStoreDataFusion::collect_head` in the `query`
    /// module, which carries the loop and why its blocks join into a prefix of the whole read). The
    /// trait default would be correct here and would materialise the whole range, which is the
    /// defect this method exists to remove.
    fn load_bars_head(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        // One layout, and it holds one series, so `keep` is never asked about a row.
        let bars = [Layout { dir: self.bars_dir(venue, symbol, interval), symbol: None }];
        self.collect_head::<BarCodec>(&bars, range, "", n, |_| true)
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.scan_quotes_capped(venue, symbol, range, None)
    }

    fn scan_quotes_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.scan_symbol_across_layouts::<QuoteCodec>(
            StoreKindId::Quote,
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )
    }

    fn scan_quotes_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        Self::refuse_an_undeclared_source(source)?;
        let lane = self.series_dir(StoreKindId::Quote, venue, symbol, None, Some(source));
        self.scan_one_series_capped::<QuoteCodec>(lane, range, symbol, budget)
    }

    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        Self::refuse_an_undeclared_source(source)?;
        let lane = self.series_dir(StoreKindId::Bar, venue, symbol, Some(interval), Some(source));
        self.scan_one_series_capped::<BarCodec>(lane, range, "", budget)
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.scan_trades_capped(venue, symbol, range, None)
    }

    fn scan_trades_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.scan_symbol_across_layouts::<TradeCodec>(
            StoreKindId::Trade,
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )
    }

    // ---- store inventory / metadata: the TRAIT overrides delegate to the inherent methods -----
    // Promote the concrete `HistoryStoreDataFusion::{list_series, inventory, series_gaps, coverage_report}`
    // (the inherent impl above) onto the `HistoryStore` trait so a `&dyn HistoryStore` — e.g. the datahub server answering a
    // `HistoryStoreDatahub` — reaches the real manifest walk, not the trait default (which REFUSES the
    // catalog pair and answers the derived views empty).
    // `HistoryStoreDataFusion::method(self)` resolves to the INHERENT method (inherent items shadow trait
    // items in path resolution), so this is a one-line delegation, NOT recursion.

    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        HistoryStoreDataFusion::list_series(self)
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        HistoryStoreDataFusion::inventory(self)
    }

    /// The trait half of the INHERENT `HistoryStoreDataFusion::series_facts` — one manifest parse, one
    /// answer, no second spelling. Promoted so a routed reader can ask for it at all.
    fn series_facts(
        &self,
        id: &SeriesId,
    ) -> Result<(SeriesCoverage, Vec<String>), HistoryStoreError> {
        HistoryStoreDataFusion::series_facts(self, id)
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        HistoryStoreDataFusion::series_gaps(self, id)
    }

    /// The trait half of the INHERENT `HistoryStoreDataFusion::series_days`: this store always knows its days.
    fn series_days(
        &self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        HistoryStoreDataFusion::series_days(self, kind, venue, source, symbol, interval, range)
            .map(Some)
    }

    fn coverage_report(
        &self,
    ) -> Result<Vec<vike_data::store::coverage::InstrumentCoverage>, HistoryStoreError> {
        HistoryStoreDataFusion::coverage_report(self)
    }

    // ...and the two PROVENANCE/REMOVAL verbs, promoted for the same reason and one more: the
    // datahub's delete verb answers through `&dyn HistoryStore`, so without these the server would
    // reach the trait's refusing defaults while holding the one store that can actually answer.
    fn series_commits(&self, id: &SeriesId) -> Result<Vec<String>, HistoryStoreError> {
        HistoryStoreDataFusion::series_commits(self, id)
    }

    fn delete_series_checked(
        &self,
        id: &SeriesId,
        require_produced_by: Option<&str>,
    ) -> Result<(), HistoryStoreError> {
        HistoryStoreDataFusion::delete_series_checked(self, id, require_produced_by)
    }

    fn append_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        let written = self.append_series::<BarCodec>(
            &self.bars_dir(venue, symbol, interval),
            commit_key,
            bars,
            WriteProfile::Hot,
        );
        let prices = WrittenPrices::Bars { interval, bars };
        self.shown(written, &PriceWrite { venue, source: None, symbol, prices })
    }

    fn append_quotes(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TickQuote],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        let written = self.append_series::<QuoteCodec>(
            &self.ticks_dir(StoreKindId::Quote, venue, symbol),
            commit_key,
            ticks,
            WriteProfile::Hot,
        );
        let prices = WrittenPrices::Quotes(ticks);
        self.shown(written, &PriceWrite { venue, source: None, symbol, prices })
    }

    fn append_trades(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TickTrade],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<TradeCodec>(
            &self.ticks_dir(StoreKindId::Trade, venue, symbol),
            commit_key,
            ticks,
            WriteProfile::Hot,
        )
    }

    fn append_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[L2BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_book_events(
            &self.ticks_dir(StoreKindId::Book, venue, symbol),
            updates,
            commit_key,
        )
    }

    fn append_depth(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[L2BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // `append_book_updates` with another `kind`: same codec, same row shape, same
        // idempotency. The SERIES is the disclosure that this lane is conflated; the trait doc has
        // the argument for why that separation is not cosmetic.
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_book_events(
            &self.ticks_dir(StoreKindId::Depth, venue, symbol),
            updates,
            commit_key,
        )
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.scan_depth_capped(venue, symbol, range, None)
    }

    fn scan_depth_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        // ⚠ The budget is in ROWS and one event is many rows — the write side explodes a
        // `L2BookUpdate` into one row per price level. So a budget bounds the READ, which is the
        // allocation that matters, while the event count it yields is smaller and
        // data-dependent. The cut never splits a `ts`, so it never splits an event either.
        // ⚠ The SAME layout walk as the tick verbs, not a copy of it: a copy is how the per-series
        // budget would survive here after being fixed there. Nothing writes a grouped depth part
        // today, so the group half of this read finds nothing — as it did before the walk.
        let rows = self.scan_symbol_across_layouts::<BookCodec>(
            StoreKindId::Depth,
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
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
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
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        // ⚠ The budget is in ROWS and one event is many rows — the write side explodes a
        // `L2BookUpdate` into one row per price level. So a budget bounds the READ, which is the
        // allocation that matters, while the event count it yields is smaller and
        // data-dependent. The cut never splits a `ts`, so it never splits an event either.
        // ⚠ The SAME layout walk as the tick verbs, not a copy of it — `scan_depth_capped` says why.
        // The rows arrive sorted by `(ts, seq)` (`BookCodec::sort_key`), which is what the regroup
        // below keys on.
        let rows = self.scan_symbol_across_layouts::<BookCodec>(
            StoreKindId::Book,
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
    ) -> Result<usize, HistoryStoreError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<PropertiesCodec>(
            &self.ticks_dir(StoreKindId::Properties, venue, symbol), // (was kind=filters; renamed with SymbolFilters→SymbolProperties)
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        self.scan_series::<PropertiesCodec>(
            &self.ticks_dir(StoreKindId::Properties, venue, symbol),
            range,
            "",
        )
        // (was kind=filters; renamed with SymbolFilters→SymbolProperties)
    }

    /// An OVERRIDE, because the tape is one part per symbol per day and a snapping run asks this per
    /// fill: the trait default opens every part up to `ts` to keep the last row.
    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, HistoryStoreError> {
        let dir = self.ticks_dir(StoreKindId::Properties, venue, symbol);
        Ok(self.last_row_as_of::<PropertiesCodec>(&dir, ts, "")?.map(|(_, grid)| grid))
    }

    fn append_equity(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[EquitySample],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<EquityCodec>(
            &self.ticks_dir(StoreKindId::Equity, venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        self.scan_series::<EquityCodec>(
            &self.ticks_dir(StoreKindId::Equity, venue, symbol),
            range,
            symbol,
        )
    }

    /// The bounded twin of `scan_equity`: the same series, the same decode argument (the symbol,
    /// standing in for the venue column), read a block of parts at a time until the budget is met.
    fn scan_equity_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        self.scan_one_series_capped::<EquityCodec>(
            self.ticks_dir(StoreKindId::Equity, venue, symbol),
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
    ) -> Result<usize, HistoryStoreError> {
        // The same guard `append_exec_orders` carries, for the same reason — and this side is the
        // one that MATTERS MORE, which is why it must not be left behind again.
        //
        // A reconcile pre-seed scans every `kind=exec_fill` leaf by `id.symbol` and folds the
        // trade_ids it finds into the SEEN-FILL dedup set (`vike-app`'s did, as this said in the
        // present tense until 2026-09-28; `crates/vike-tradehub-core/src/journal_view.rs`'s
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
        // The execution stream's sink (`vike_datahub::execution_stream`) skips and counts a record whose
        // venue or symbol is empty, never sending it here: one such fill would otherwise fail its
        // whole window. This refusal stays for every other writer.
        if symbol.is_empty() {
            return Err(HistoryStoreError::Query(format!(
                "append_exec_fills({venue}): empty symbol — a per-symbol series is addressed BY its \
                 `symbol=` path segment, so an empty one is unattributable, invisible to every scan, \
                 and its trade_ids would be missing from the reconcile seen-fill set"
            )));
        }
        // The other 14 writing verbs' guard: a `/` (hyperliquid's spot `PURR/USDC`) nests the leaf
        // under the perp's `symbol=PURR`, where `parse_series_id` names no series.
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<ExecFillCodec>(
            &self.ticks_dir(StoreKindId::ExecFill, venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        // Every field (venue+symbol included) is a stored column, so decode ignores `ctx` — pass "".
        self.scan_series::<ExecFillCodec>(
            &self.ticks_dir(StoreKindId::ExecFill, venue, symbol),
            TimestampRange::all(),
            "",
        )
    }

    /// The bounded twin of `scan_exec_fills` for a caller that wants only the START of a range of
    /// the series: the same series and decode, read a block of parts at a time and stopped once `n`
    /// rows are in — the walk `load_bars_head` takes. A COUNT, not a budget, so `n == 0` is
    /// "nothing" (`collect_head` answers it without reading) rather than the whole range.
    fn scan_execution_fills_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        let fills =
            [Layout { dir: self.ticks_dir(StoreKindId::ExecFill, venue, symbol), symbol: None }];
        self.collect_head::<ExecFillCodec>(&fills, range, "", n, |_| true)
    }

    fn append_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecOrderRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
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
            return Err(HistoryStoreError::Query(format!(
                "append_exec_orders({venue}): empty symbol — a per-symbol series is addressed BY its \
                 `symbol=` path segment, so an empty one is unattributable and would be invisible to \
                 every scan"
            )));
        }
        // `append_exec_fills`' twin guard (audit flag HL-1).
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<ExecOrderCodec>(
            &self.ticks_dir(StoreKindId::ExecOrder, venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        self.scan_series::<ExecOrderCodec>(
            &self.ticks_dir(StoreKindId::ExecOrder, venue, symbol),
            TimestampRange::all(),
            "",
        )
    }

    /// The bounded twin of `scan_exec_orders` — `scan_execution_fills_head`'s block walk over the
    /// order-lifecycle series: read a block of parts at a time, stop once `n` rows are in, never
    /// split a timestamp. `n == 0` is "nothing" (`collect_head` answers it without reading).
    fn scan_execution_orders_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        let orders =
            [Layout { dir: self.ticks_dir(StoreKindId::ExecOrder, venue, symbol), symbol: None }];
        self.collect_head::<ExecOrderCodec>(&orders, range, "", n, |_| true)
    }

    fn append_funding(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecutionHistoryFundingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<FundingCodec>(
            &self.ticks_dir(StoreKindId::ExecFunding, venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_funding(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecutionHistoryFundingRow>, HistoryStoreError> {
        // Every field (hash included) is a stored column, so decode ignores `ctx` — pass "".
        self.scan_series::<FundingCodec>(
            &self.ticks_dir(StoreKindId::ExecFunding, venue, symbol),
            range,
            "",
        )
    }

    fn append_forecasts(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecForecastRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<ForecastCodec>(
            &self.ticks_dir(StoreKindId::ExecForecast, venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_forecasts(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        // Every field is a stored column, so decode ignores `ctx` — pass "".
        self.scan_series::<ForecastCodec>(
            &self.ticks_dir(StoreKindId::ExecForecast, venue, symbol),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_forecasts`: the datahub's `StrategyForecasts` reads through it.
    fn scan_forecasts_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        self.scan_one_series_capped::<ForecastCodec>(
            self.ticks_dir(StoreKindId::ExecForecast, venue, symbol),
            range,
            "",
            budget,
        )
    }

    fn append_chain_snapshot(
        &self,
        venue: &str,
        underlying: &str,
        rows: &[ChainRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<ChainCodec>(
            &self.ticks_dir(StoreKindId::Chain, venue, underlying),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        // Every field (underlying included) is a stored column, so decode ignores `ctx` — pass "".
        // `sort_by_key` on (ts, 0) is stable, preserving within-snapshot row order from the write.
        self.scan_series::<ChainCodec>(
            &self.ticks_dir(StoreKindId::Chain, venue, underlying),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_chain`. The walk's blocks join in the whole read's order, so the
    /// rows of one snapshot still come back in the order they were written.
    fn scan_chain_capped(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.scan_one_series_capped::<ChainCodec>(
            self.ticks_dir(StoreKindId::Chain, venue, underlying),
            range,
            "",
            budget,
        )
    }

    fn append_option_trades(
        &self,
        venue: &str,
        currency: &str,
        rows: &[OptionTradeRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<OptionTradeCodec>(
            &self.ticks_dir(StoreKindId::OptionTrade, venue, currency),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_option_trades(
        &self,
        venue: &str,
        currency: &str,
        range: TimestampRange,
    ) -> Result<Vec<OptionTradeRow>, HistoryStoreError> {
        // Every field (underlying and instrument included) is a stored column, so decode ignores
        // `ctx` — pass "". `sort_by_key` on (ts, 0) is stable: one millisecond's trades keep their order.
        self.scan_series::<OptionTradeCodec>(
            &self.ticks_dir(StoreKindId::OptionTrade, venue, currency),
            range,
            "",
        )
    }

    /// Answered from the `ts` column alone, as `bar_edges` is: a range can hold years of trades.
    fn option_trade_edges(
        &self,
        venue: &str,
        currency: &str,
        range: TimestampRange,
    ) -> Result<BarEdges, HistoryStoreError> {
        self.timestamp_edges(&self.ticks_dir(StoreKindId::OptionTrade, venue, currency), range)
    }

    fn append_cohort(
        &self,
        venue: &str,
        asset: &str,
        rows: &[CohortRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<CohortCodec>(
            &self.ticks_dir(StoreKindId::Cohort, venue, asset),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        // Every field (asset included) is a stored column, so decode ignores `ctx` — pass "".
        // `sort_by_key` on (ts, 0) is stable, so the labels of one hour come back in the order they
        // were written: the caller's own axis/grading order, not an arbitrary one.
        self.scan_series::<CohortCodec>(
            &self.ticks_dir(StoreKindId::Cohort, venue, asset),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_cohort`. The walk's blocks join in the whole read's order, so the
    /// labels of one hour still come back in the order they were written.
    fn scan_cohort_capped(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        self.scan_one_series_capped::<CohortCodec>(
            self.ticks_dir(StoreKindId::Cohort, venue, asset),
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
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<PerpMetricsCodec>(
            &self.ticks_dir(StoreKindId::PerpMetrics, venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        // `(venue, symbol)` is the whole identity and lives in the path, so no column needs
        // re-injecting on decode — pass "".
        self.scan_series::<PerpMetricsCodec>(
            &self.ticks_dir(StoreKindId::PerpMetrics, venue, symbol),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_perp_metrics`.
    fn scan_perp_metrics_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        self.scan_one_series_capped::<PerpMetricsCodec>(
            self.ticks_dir(StoreKindId::PerpMetrics, venue, symbol),
            range,
            "",
            budget,
        )
    }

    fn append_texts(
        &self,
        text_kind: &str,
        collector: &str,
        rows: &[TextRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<TextCodec>(
            &self.ticks_dir(StoreKindId::Text, text_kind, collector),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_texts(
        &self,
        text_kind: &str,
        collector: &str,
        range: TimestampRange,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        // Every field (the kind included) is a stored column, so decode ignores `ctx` — pass "".
        // `sort_by_key` on (received_ms, 0) is stable: one append's rows keep their written order.
        self.scan_series::<TextCodec>(
            &self.ticks_dir(StoreKindId::Text, text_kind, collector),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_texts`: the datahub's `ScanTexts` reads through it.
    fn scan_texts_capped(
        &self,
        text_kind: &str,
        collector: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        self.scan_one_series_capped::<TextCodec>(
            self.ticks_dir(StoreKindId::Text, text_kind, collector),
            range,
            "",
            budget,
        )
    }

    fn append_financial_facts(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[FinancialFactRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<FinancialFactCodec>(
            &self.ticks_dir(StoreKindId::FinancialFact, venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_financial_facts(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        // Every field (the CIK included) is a stored column, so decode ignores `ctx` — pass "".
        self.scan_series::<FinancialFactCodec>(
            &self.ticks_dir(StoreKindId::FinancialFact, venue, symbol),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_financial_facts`: the datahub's `ScanFinancialFacts` reads through it.
    fn scan_financial_facts_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        self.scan_one_series_capped::<FinancialFactCodec>(
            self.ticks_dir(StoreKindId::FinancialFact, venue, symbol),
            range,
            "",
            budget,
        )
    }

    fn append_institutional_holdings(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[InstitutionalHoldingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<InstitutionalHoldingCodec>(
            &self.ticks_dir(StoreKindId::InstitutionalHolding, venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_institutional_holdings(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        self.scan_series::<InstitutionalHoldingCodec>(
            &self.ticks_dir(StoreKindId::InstitutionalHolding, venue, symbol),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_institutional_holdings`: the datahub's `ScanInstitutionalHoldings`
    /// reads through it.
    fn scan_institutional_holdings_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        self.scan_one_series_capped::<InstitutionalHoldingCodec>(
            self.ticks_dir(StoreKindId::InstitutionalHolding, venue, symbol),
            range,
            "",
            budget,
        )
    }

    fn append_text_scores(
        &self,
        text_kind: &str,
        model_name: &str,
        rows: &[TextScoreRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.append_series::<TextScoreCodec>(
            &self.ticks_dir(StoreKindId::TextScore, text_kind, model_name),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_text_scores(
        &self,
        text_kind: &str,
        model_name: &str,
        range: TimestampRange,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        // Every field is a stored column, so decode ignores `ctx` — pass "" (the text pair's rule).
        self.scan_series::<TextScoreCodec>(
            &self.ticks_dir(StoreKindId::TextScore, text_kind, model_name),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_text_scores`: the datahub's `ScanTextScores` reads through it.
    fn scan_text_scores_capped(
        &self,
        text_kind: &str,
        model_name: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        self.scan_one_series_capped::<TextScoreCodec>(
            self.ticks_dir(StoreKindId::TextScore, text_kind, model_name),
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
        range: TimestampRange,
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
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
        range: TimestampRange,
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let step = interval_ms(interval)?;
        let trades = self.scan_trades(venue, symbol, range)?; // ts-ascending
        let bars = consolidate_trades(&trades, step);
        self.append_bars(venue, symbol, interval, &bars, commit_key)
    }
}

impl HistoryStoreDataFusion {
    /// The one write of an L2 event series into `series_dir` (the `book` or the `depth` lane): the
    /// rows go through [`BookCodec`], and the answer is EVENTS written, not rows. The caller names
    /// the kind at its `ticks_dir` call, where `store_kind_gate`'s scan reads it as a literal.
    fn append_book_events(
        &self,
        series_dir: &std::path::Path,
        updates: &[L2BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let rows = book_rows(updates);
        let written_rows =
            self.append_series::<BookCodec>(series_dir, commit_key, &rows, WriteProfile::Hot)?;
        // append_series (via commit_rows) returns per-level ROWS; the seam's contract is EVENTS
        // written.
        Ok(if written_rows == 0 { 0 } else { updates.len() })
    }
}
