//! `impl HistoryStore for CachedHistoryStore`: the bar reads and the three tick reads (each decoded from the kept
//! bodies, and `read_columns` of each as they are) go through the cache (`CachedHistoryStore::read`), and every other
//! verb is the inner store's, forwarded as it is. A wrapper
//! that fronts a store must forward each verb (the trait's FRONTS rule): an inherited default would answer
//! for the datahub without asking it (`kept_instruments` would answer "no lists" and lose a run's kept rows).
//! Generated from the trait's signatures; a verb added to the trait needs its forward here.

use vike_data::option_trade_log::OptionTradeRow;
use vike_data::{
    BarDay, BarEdges, ChainRow, CohortRow, ExecFillRow, ExecForecastRow, ExecOrderRow,
    ExecutionHistoryFundingRow, ExecutionSeries, FinancialFactRow, HistoryStore, HistoryStoreError,
    InstitutionalHoldingRow, InstrumentCoverage, PerpMetricRow, SeriesCoverage, SeriesId,
    SharedColumns, TextRow, TextScoreRow, TimestampRange,
};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::StoreKindId;
use vike_model::{EquitySample, SymbolProperties};

use super::{Answer, CachedHistoryStore};

impl HistoryStore for CachedHistoryStore {
    // ---- the cached reads: bars and three of ticks, decoded, and each as it is kept ----
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.read_bars(venue, None, symbol, interval, range).and_then(Answer::rows)
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.read_quotes(venue, symbol, range).and_then(Answer::rows)
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.read_trades(venue, symbol, range).and_then(Answer::rows)
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.read_books(venue, symbol, range).and_then(Answer::rows)
    }

    /// The cached reads themselves: the kept days' bodies, shared with the cache (the module doc of `bar_cache`).
    fn read_columns(
        &self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Option<Result<SharedColumns, HistoryStoreError>> {
        Some(match (kind, source, interval) {
            (StoreKindId::Bar, source, Some(interval)) => {
                self.read_bars(venue, source, symbol, interval, range).map(Answer::columns)
            }
            (StoreKindId::Quote, None, None) => {
                self.read_quotes(venue, symbol, range).map(Answer::columns)
            }
            (StoreKindId::Trade, None, None) => {
                self.read_trades(venue, symbol, range).map(Answer::columns)
            }
            (StoreKindId::Book, None, None) => {
                self.read_books(venue, symbol, range).map(Answer::columns)
            }
            _ => return None,
        })
    }

    /// Cached only when unbudgeted (`None`); ANY budget forwards, `Some(0)` included: the trait reads a zero
    /// budget as the whole range, but `HistoryStoreDatahub` answers it EMPTY, and a cache filling whole days from
    /// that answer would keep real days empty under their real stamps.
    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        if budget.is_some() {
            return self
                .inner
                .load_bars_sourced_capped(venue, source, symbol, interval, range, budget);
        }
        self.read_bars(venue, Some(source), symbol, interval, range).and_then(Answer::rows)
    }

    // ---- every other verb is the inner store's, forwarded (the trait's FRONTS rule) ----
    fn bar_edges(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<BarEdges, HistoryStoreError> {
        self.inner.bar_edges(venue, symbol, interval, range)
    }
    fn load_bars_head(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.inner.load_bars_head(venue, symbol, interval, range, n)
    }
    fn append_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_bars(venue, symbol, interval, bars, commit_key)
    }
    fn append_quotes(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TickQuote],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_quotes(venue, symbol, ticks, commit_key)
    }
    fn append_trades(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TickTrade],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_trades(venue, symbol, ticks, commit_key)
    }
    fn append_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[L2BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_book_updates(venue, symbol, updates, commit_key)
    }
    fn scan_quotes_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.inner.scan_quotes_capped(venue, symbol, range, budget)
    }
    fn scan_quotes_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.inner.scan_quotes_sourced_capped(venue, source, symbol, range, budget)
    }
    fn series_days(
        &self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Option<Vec<BarDay>>, HistoryStoreError> {
        self.inner.series_days(kind, venue, source, symbol, interval, range)
    }
    fn scan_trades_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.inner.scan_trades_capped(venue, symbol, range, budget)
    }
    fn scan_book_updates_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.inner.scan_book_updates_capped(venue, symbol, range, budget)
    }
    fn scan_depth_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.inner.scan_depth_capped(venue, symbol, range, budget)
    }
    fn append_depth(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[L2BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_depth(venue, symbol, updates, commit_key)
    }
    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.inner.scan_depth(venue, symbol, range)
    }
    fn append_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[(i64, SymbolProperties)],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_symbol_properties(venue, symbol, rows, commit_key)
    }
    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        self.inner.scan_symbol_properties(venue, symbol, range)
    }
    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, HistoryStoreError> {
        self.inner.properties_as_of(venue, symbol, ts)
    }
    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        self.inner.list_series()
    }
    fn prefetch(&self, reads: &[vike_data::PlannedRead]) {
        self.inner.prefetch(reads);
    }
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        self.inner.inventory()
    }
    fn execution_series(
        &self,
        venue: Option<&str>,
        symbol: Option<&str>,
        range: TimestampRange,
    ) -> Result<ExecutionSeries, HistoryStoreError> {
        self.inner.execution_series(venue, symbol, range)
    }
    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        self.inner.series_gaps(id)
    }
    fn series_facts(
        &self,
        id: &SeriesId,
    ) -> Result<(SeriesCoverage, Vec<String>), HistoryStoreError> {
        self.inner.series_facts(id)
    }
    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        self.inner.kept_instruments()
    }
    fn stock_splits(&self) -> Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError> {
        self.inner.stock_splits()
    }
    fn kept_instruments_and_splits(
        &self,
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        self.inner.kept_instruments_and_splits()
    }
    fn kept_instruments_and_splits_of(
        &self,
        venues: &[String],
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        self.inner.kept_instruments_and_splits_of(venues)
    }
    fn kept_instruments_and_splits_encoded_of(
        &self,
        venues: &[String],
    ) -> Option<Result<vike_data::EncodedKept, HistoryStoreError>> {
        self.inner.kept_instruments_and_splits_encoded_of(venues)
    }
    fn coverage_report(&self) -> Result<Vec<InstrumentCoverage>, HistoryStoreError> {
        self.inner.coverage_report()
    }
    fn series_commits(&self, id: &SeriesId) -> Result<Vec<String>, HistoryStoreError> {
        self.inner.series_commits(id)
    }
    fn delete_series_checked(
        &self,
        id: &SeriesId,
        require_produced_by: Option<&str>,
    ) -> Result<(), HistoryStoreError> {
        self.inner.delete_series_checked(id, require_produced_by)
    }
    fn append_equity(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[EquitySample],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_equity(venue, symbol, rows, commit_key)
    }
    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        self.inner.scan_equity(venue, symbol, range)
    }
    fn scan_equity_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        self.inner.scan_equity_capped(venue, symbol, range, budget)
    }
    fn append_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecFillRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_exec_fills(venue, symbol, rows, commit_key)
    }
    fn scan_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        self.inner.scan_exec_fills(venue, symbol)
    }
    fn scan_execution_fills_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        self.inner.scan_execution_fills_head(venue, symbol, range, n)
    }
    fn append_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecOrderRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_exec_orders(venue, symbol, rows, commit_key)
    }
    fn scan_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        self.inner.scan_exec_orders(venue, symbol)
    }
    fn scan_execution_orders_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        self.inner.scan_execution_orders_head(venue, symbol, range, n)
    }
    fn append_funding(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecutionHistoryFundingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_funding(venue, symbol, rows, commit_key)
    }
    fn scan_funding(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecutionHistoryFundingRow>, HistoryStoreError> {
        self.inner.scan_funding(venue, symbol, range)
    }
    fn append_forecasts(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecForecastRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_forecasts(venue, symbol, rows, commit_key)
    }
    fn scan_forecasts(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        self.inner.scan_forecasts(venue, symbol, range)
    }
    fn scan_forecasts_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        self.inner.scan_forecasts_capped(venue, symbol, range, budget)
    }
    fn forecasts_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        start_ms: Option<i64>,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        self.inner.forecasts_as_of(venue, symbol, decision_ms, start_ms)
    }
    fn append_chain_snapshot(
        &self,
        venue: &str,
        underlying: &str,
        rows: &[ChainRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_chain_snapshot(venue, underlying, rows, commit_key)
    }
    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.inner.scan_chain(venue, underlying, range)
    }
    fn scan_chain_capped(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.inner.scan_chain_capped(venue, underlying, range, budget)
    }
    fn append_option_trades(
        &self,
        venue: &str,
        currency: &str,
        rows: &[OptionTradeRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_option_trades(venue, currency, rows, commit_key)
    }
    fn scan_option_trades(
        &self,
        venue: &str,
        currency: &str,
        range: TimestampRange,
    ) -> Result<Vec<OptionTradeRow>, HistoryStoreError> {
        self.inner.scan_option_trades(venue, currency, range)
    }
    fn option_trade_edges(
        &self,
        venue: &str,
        currency: &str,
        range: TimestampRange,
    ) -> Result<BarEdges, HistoryStoreError> {
        self.inner.option_trade_edges(venue, currency, range)
    }
    fn append_cohort(
        &self,
        venue: &str,
        asset: &str,
        rows: &[CohortRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_cohort(venue, asset, rows, commit_key)
    }
    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        self.inner.scan_cohort(venue, asset, range)
    }
    fn scan_cohort_capped(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        self.inner.scan_cohort_capped(venue, asset, range, budget)
    }
    fn append_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[PerpMetricRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_perp_metrics(venue, symbol, rows, commit_key)
    }
    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        self.inner.scan_perp_metrics(venue, symbol, range)
    }
    fn scan_perp_metrics_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        self.inner.scan_perp_metrics_capped(venue, symbol, range, budget)
    }
    fn append_texts(
        &self,
        text_kind: &str,
        collector: &str,
        rows: &[TextRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_texts(text_kind, collector, rows, commit_key)
    }
    fn scan_texts(
        &self,
        text_kind: &str,
        collector: &str,
        range: TimestampRange,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        self.inner.scan_texts(text_kind, collector, range)
    }
    fn scan_texts_capped(
        &self,
        text_kind: &str,
        collector: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        self.inner.scan_texts_capped(text_kind, collector, range, budget)
    }
    fn texts_as_of(
        &self,
        text_kind: &str,
        collector: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        self.inner.texts_as_of(text_kind, collector, decision_ms, lookback_ms)
    }
    fn append_financial_facts(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[FinancialFactRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_financial_facts(venue, symbol, rows, commit_key)
    }
    fn scan_financial_facts(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        self.inner.scan_financial_facts(venue, symbol, range)
    }
    fn scan_financial_facts_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        self.inner.scan_financial_facts_capped(venue, symbol, range, budget)
    }
    fn financial_facts_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        self.inner.financial_facts_as_of(venue, symbol, decision_ms, lookback_ms)
    }
    fn append_institutional_holdings(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[InstitutionalHoldingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_institutional_holdings(venue, symbol, rows, commit_key)
    }
    fn scan_institutional_holdings(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        self.inner.scan_institutional_holdings(venue, symbol, range)
    }
    fn scan_institutional_holdings_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        self.inner.scan_institutional_holdings_capped(venue, symbol, range, budget)
    }
    fn institutional_holdings_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        self.inner.institutional_holdings_as_of(venue, symbol, decision_ms, lookback_ms)
    }
    fn append_text_scores(
        &self,
        text_kind: &str,
        model_name: &str,
        rows: &[TextScoreRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.append_text_scores(text_kind, model_name, rows, commit_key)
    }
    fn scan_text_scores(
        &self,
        text_kind: &str,
        model_name: &str,
        range: TimestampRange,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        self.inner.scan_text_scores(text_kind, model_name, range)
    }
    fn scan_text_scores_capped(
        &self,
        text_kind: &str,
        model_name: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        self.inner.scan_text_scores_capped(text_kind, model_name, range, budget)
    }
    fn text_scores_as_of(
        &self,
        text_kind: &str,
        model_name: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        self.inner.text_scores_as_of(text_kind, model_name, decision_ms, lookback_ms)
    }
    fn chain_as_of(
        &self,
        venue: &str,
        underlying: &str,
        ts: i64,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.inner.chain_as_of(venue, underlying, ts)
    }
    fn chain_as_of_within(
        &self,
        venue: &str,
        underlying: &str,
        ts: i64,
        lookback_ms: i64,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.inner.chain_as_of_within(venue, underlying, ts, lookback_ms)
    }
    fn resample_quotes_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.resample_quotes_to_bars(venue, symbol, interval, range, commit_key)
    }
    fn resample_trades_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        self.inner.resample_trades_to_bars(venue, symbol, interval, range, commit_key)
    }
}
