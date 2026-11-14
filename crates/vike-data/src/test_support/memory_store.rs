//! `HistoryStoreMemory`: the DataFusion-free in-memory `HistoryStore` double and its catalog fold.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use vike_marketdata::Bar;
use vike_model::{EquitySample, SymbolProperties};

use super::{HistoryStore, persisted_bar};
use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::funding_log::ExecutionHistoryFundingRow;
use crate::perp_metrics_log::PerpMetricRow;
use crate::store::hist::{HistoryStoreError, TimestampRange};
use crate::store::series::{SeriesCoverage, SeriesId};
use crate::store::store_kind::{StoreKindId, refuse_an_unknown_lane};

/// OHLCV bars observed for one `(venue, symbol, interval)` series, in append order — already
/// through [`persisted_bar`], so what is held is what the real store would give back.
type BarRows = Vec<Bar>;

/// `(timestamp_ms, properties)` rows observed for one `(venue, symbol)` series, in append order.
type PropertiesRows = Vec<(i64, SymbolProperties)>;

/// Equity-curve samples observed for one `(venue, symbol)` series, in append order.
type EquityRows = Vec<EquitySample>;

/// Account fills observed for one `(venue, symbol)` series, in append order.
type ExecutionFillRows = Vec<ExecFillRow>;

/// Order lifecycle snapshots observed for one `(venue, symbol)` series, in append order.
type ExecutionOrderRows = Vec<ExecOrderRow>;

/// Realized funding payments observed for one `(venue, symbol=<coin>)` series, in append order.
type FundingRows = Vec<ExecutionHistoryFundingRow>;

/// Option-chain snapshot rows observed for one `(venue, symbol=<underlying>)` series, in append
/// order.
type ChainRows = Vec<ChainRow>;

/// Cohort marginals observed for one `(venue, symbol=<asset>)` series, in append order — every
/// axis, label, grading and label basis that was written, exactly as the real series holds them.
type CohortRows = Vec<CohortRow>;

/// Perp market-context rows observed for one `(venue, symbol)` series, in append order.
type PerpMetricRows = Vec<PerpMetricRow>;

/// In-memory capturing [`HistoryStore`]: records `append_bars`/`append_symbol_properties`/
/// `append_equity` rows so a test can load them back. Thread-safe (`Mutex`), so it drops into an
/// `Arc<dyn HistoryStore>` a recorder can hold.
#[derive(Default)]
pub struct HistoryStoreMemory {
    /// `(venue, symbol, interval)` → observed OHLCV bars (`kind=bar`).
    bars: Mutex<HashMap<(String, String, String), BarRows>>,
    /// `(venue, source, symbol, interval)` → bars of one `source=` lane
    /// ([`HistoryStoreMemory::append_bars_sourced`]), never the sourceless series above.
    lanes: Mutex<HashMap<(String, String, String, String), BarRows>>,
    /// `(venue, symbol)` → observed properties rows.
    properties: Mutex<HashMap<(String, String), PropertiesRows>>,
    /// `(venue, symbol)` → observed equity-curve samples (portfolio-observer PR-3 Task 2).
    equity: Mutex<HashMap<(String, String), EquityRows>>,
    /// `(venue, symbol)` → observed account fills (`kind=exec_fill`).
    execution_fills: Mutex<HashMap<(String, String), ExecutionFillRows>>,
    /// `(venue, symbol)` → observed order lifecycle snapshots (`kind=execution_order`).
    execution_orders: Mutex<HashMap<(String, String), ExecutionOrderRows>>,
    /// `(venue, symbol=<coin>)` -> observed realized funding payments (`kind=exec_funding`).
    funding: Mutex<HashMap<(String, String), FundingRows>>,
    /// `(venue, symbol=<underlying>)` → observed option-chain snapshot rows (`kind=chain`).
    chains: Mutex<HashMap<(String, String), ChainRows>>,
    /// `(venue, symbol=<asset>)` → observed cohort marginals (`kind=cohort`).
    cohorts: Mutex<HashMap<(String, String), CohortRows>>,
    /// `(venue, symbol)` → observed perp market-context rows (`kind=perp_metrics`).
    perp_metrics: Mutex<HashMap<(String, String), PerpMetricRows>>,
    /// Commit keys already ingested — the batch-level idempotency guard, shared across every series
    /// kind this double supports (mirrors how a real commit_key is a caller-chosen string with no
    /// collision across kinds in practice).
    seen: Mutex<HashSet<String>>,
}

impl HistoryStoreMemory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bars for `(venue, symbol, interval)` into ONE `source=` lane, never the sourceless series: the
    /// twin of `vike_store`'s `HistoryStoreDataFusion::append_bars_sourced` (a lane the store does not declare
    /// is refused, an empty batch never burns the commit key). Only
    /// [`HistoryStore::load_bars_sourced_capped`] reads them back, and [`HistoryStore::list_series`] names
    /// them with their source; [`HistoryStore::inventory`] answers for the sourceless leaves alone, as
    /// the real store's does (decision 0255).
    pub fn append_bars_sourced(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        refuse_an_unknown_lane(source).map_err(HistoryStoreError::Query)?;
        if bars.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.lanes
            .lock()
            .unwrap()
            .entry((
                venue.to_string(),
                source.to_string(),
                symbol.to_string(),
                interval.to_string(),
            ))
            .or_default()
            .extend(bars.iter().map(persisted_bar));
        Ok(bars.len())
    }

    /// The REAL catalog fold both `HistoryStore::list_series` and `HistoryStore::inventory` answer from:
    /// one `(SeriesId, SeriesCoverage)` row per series this double actually holds, one kind per
    /// storage map, sorted by id (matching `HistoryStoreDataFusion::list_series`'s sorted contract). Bar
    /// rows carry their interval — `kind=bar` sub-partitions by bar step, so the id mirrors what
    /// the real store parses back out of its `interval=` path segment; every tick-shaped kind
    /// carries `None`. An unseeded store folds to an EMPTY catalog — an honest empty, from a
    /// store that looked — where inheriting the trait's refusing default would claim the double
    /// cannot enumerate at all, and inheriting the OLD empty default (as this double used to) had a
    /// SEEDED store answering "no series" while holding rows.
    ///
    /// Coverage is the fold's truth, not `HistoryStoreDataFusion`'s: `first_timestamp`/`last_timestamp`/`rows` are
    /// computed from the held rows, while `bytes`/`parts`/`dates` stay 0 because they are ON-DISK
    /// facts (summed part-file size, part count, `date=` partition count) and an in-memory store
    /// genuinely has none of any of them.
    fn catalog(&self) -> Vec<(SeriesId, SeriesCoverage)> {
        fn cov(ts: impl Iterator<Item = i64>) -> SeriesCoverage {
            let mut out = SeriesCoverage {
                first_timestamp: i64::MAX,
                last_timestamp: i64::MIN,
                ..Default::default()
            };
            for t in ts {
                out.first_timestamp = out.first_timestamp.min(t);
                out.last_timestamp = out.last_timestamp.max(t);
                out.rows += 1;
            }
            // Unreachable through the appends (an empty batch never inserts a map entry), but a
            // zero-row entry must fold to the all-zero coverage, never to sentinel timestamps.
            if out.rows == 0 {
                return SeriesCoverage::default();
            }
            out
        }
        let mut out: Vec<(SeriesId, SeriesCoverage)> = Vec::new();
        // Bars push directly rather than through the closure below: theirs is the one id shape
        // that carries an interval.
        for ((v, s, i), rows) in self.bars.lock().unwrap().iter() {
            out.push((
                SeriesId::per_symbol(StoreKindId::Bar, v.as_str(), s.as_str(), Some(i.clone())),
                cov(rows.iter().map(|b| b.timestamp)),
            ));
        }
        let mut push = |kind: StoreKindId, venue: &str, symbol: &str, c: SeriesCoverage| {
            out.push((SeriesId::per_symbol(kind, venue, symbol, None), c));
        };
        for ((v, s), rows) in self.properties.lock().unwrap().iter() {
            push(StoreKindId::Properties, v, s, cov(rows.iter().map(|(ts, _)| *ts)));
        }
        for ((v, s), rows) in self.equity.lock().unwrap().iter() {
            push(StoreKindId::Equity, v, s, cov(rows.iter().map(|r| r.timestamp)));
        }
        for ((v, s), rows) in self.execution_fills.lock().unwrap().iter() {
            push(StoreKindId::ExecFill, v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.execution_orders.lock().unwrap().iter() {
            push(StoreKindId::ExecOrder, v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.funding.lock().unwrap().iter() {
            push(StoreKindId::ExecFunding, v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.chains.lock().unwrap().iter() {
            push(StoreKindId::Chain, v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.cohorts.lock().unwrap().iter() {
            push(StoreKindId::Cohort, v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.perp_metrics.lock().unwrap().iter() {
            push(StoreKindId::PerpMetrics, v, s, cov(rows.iter().map(|r| r.ts)));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

fn in_range(ts: i64, range: TimestampRange) -> bool {
    range.start.map(|s| ts >= s).unwrap_or(true) && range.end.map(|e| ts <= e).unwrap_or(true)
}

impl HistoryStore for HistoryStoreMemory {
    /// Held bars for the `(venue, symbol, interval)` triple in `range` (inclusive both ends,
    /// `None` = unbounded), ts-ascending — the stable sort mirrors `BarCodec`'s
    /// `sort_key = (ts, 0)`, so equal-ts bars keep append order on both stores. An unknown triple
    /// answers an honest empty, exactly as the real store's missing series directory reads as an
    /// empty manifest.
    ///
    /// `HistoryStore::bar_edges` is NOT overridden: this double is memory-resident, so the trait
    /// default (the edges of THIS read) is exact and there is nothing for an override to bound. It
    /// is also what keeps a second implementation of the contract out of a test double —
    /// `test_support_tests.rs` pins the default through it.
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let map = self.bars.lock().unwrap();
        let mut out: Vec<Bar> = map
            .get(&(venue.to_string(), symbol.to_string(), interval.to_string()))
            .map(|rows| rows.iter().filter(|b| in_range(b.timestamp, range)).cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|b| b.timestamp);
        Ok(out)
    }
    /// The bars of ONE lane ([`HistoryStoreMemory::append_bars_sourced`]) in `range`, ts-ascending: a lane
    /// it holds nothing of answers an honest empty, a lane the store does not declare is refused. The
    /// budget is not honoured, for the reason `HistoryStore::scan_quotes_capped`'s default gives: this
    /// double is small by construction.
    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let _ = budget;
        refuse_an_unknown_lane(source).map_err(HistoryStoreError::Query)?;
        let key = (venue.to_string(), source.to_string(), symbol.to_string(), interval.to_string());
        let map = self.lanes.lock().unwrap();
        let mut out: Vec<Bar> = map
            .get(&key)
            .map(|rows| rows.iter().filter(|b| in_range(b.timestamp, range)).cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|b| b.timestamp);
        Ok(out)
    }
    /// Real bar storage, keyed on the RAW `(venue, symbol, interval)` strings — parity, not a
    /// shortcut: the real store's `bars_dir` embeds the interval string as a path segment with no
    /// validation (`interval_ms` is consulted only by resample/gaps), so `"1m"` and `"60s"` are
    /// distinct series THERE too. Rows land through [`persisted_bar`], which erases what the bar
    /// schema never persists.
    fn append_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // An EMPTY batch never burns the commit key — `HistoryStoreDataFusion::commit_rows` returns
        // `Ok(0)` on `ts.is_empty()` BEFORE it registers the key, so a later real append under the
        // same key still lands there. Without this guard the double would silently swallow that
        // second append and diverge from the store it stands in for.
        if bars.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.bars
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string(), interval.to_string()))
            .or_default()
            .extend(bars.iter().map(persisted_bar));
        Ok(bars.len())
    }
    // Ticks and the book are not held (the module doc's "still-inert remainder"): reads answer empty,
    // appends and both resamples `Ok(0)`.
    crate::hist_store_stubs!(inert: scan_quotes, scan_trades, append_quotes, append_trades,
        append_book_updates, scan_book_updates, resample_quotes_to_bars, resample_trades_to_bars);
    // `append_depth`/`scan_depth` are NOT overridden: this double holds neither book nor depth, so
    // the trait's defaults — which now REFUSE on both halves — are the honest answer for it, and
    // inheriting them means this file does not grow a stub every time the seam gains a kind. ⚠ The
    // read half is the one that changed: it used to inherit an empty `Ok`, which made this double
    // indistinguishable from a real store that holds no depth rows in the range asked for.

    /// The REAL catalog: what this double actually holds, kind by kind (see
    /// [`HistoryStoreMemory::catalog`]). It used to inherit the trait's old empty default, which had a
    /// SEEDED store answering "no series" while holding rows — the same fabrication the default
    /// itself carried, worn by the one impl whose truthful answer costs a fold over its maps.
    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        let mut out: Vec<SeriesId> = self.catalog().into_iter().map(|(id, _)| id).collect();
        // The lanes too, with their source, as the real store's listing names a sourced leaf.
        out.extend(self.lanes.lock().unwrap().keys().map(|(v, src, s, i)| {
            SeriesId::per_symbol(StoreKindId::Bar, v.as_str(), s.as_str(), Some(i.clone()))
                .with_source(src.as_str())
        }));
        out.sort();
        Ok(out)
    }

    /// The coverage twin of [`HistoryStore::list_series`], from the same fold — `first_timestamp`/`last_timestamp`/
    /// `rows` are the held rows' truth; the on-disk members stay 0 (see [`HistoryStoreMemory::catalog`]).
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        Ok(self.catalog())
    }

    fn append_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[(i64, SymbolProperties)],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // An EMPTY batch never burns the commit key — `HistoryStoreDataFusion::commit_rows` returns
        // `Ok(0)` on `ts.is_empty()` BEFORE it registers the key, so a later real append under the
        // same key still lands there. Without this guard the double would silently swallow that
        // second append and diverge from the store it stands in for.
        if rows.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.properties
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        let map = self.properties.lock().unwrap();
        let mut out: Vec<(i64, SymbolProperties)> = map
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|rows| rows.iter().copied().filter(|(ts, _)| in_range(*ts, range)).collect())
            .unwrap_or_default();
        out.sort_by_key(|r| r.0);
        Ok(out)
    }

    fn append_equity(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[EquitySample],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // An EMPTY batch never burns the commit key — `HistoryStoreDataFusion::commit_rows` returns
        // `Ok(0)` on `ts.is_empty()` BEFORE it registers the key, so a later real append under the
        // same key still lands there. Without this guard the double would silently swallow that
        // second append and diverge from the store it stands in for.
        if rows.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.equity
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        let map = self.equity.lock().unwrap();
        let mut out: Vec<EquitySample> = map
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|rows| rows.iter().filter(|r| in_range(r.timestamp, range)).cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|r| r.timestamp);
        Ok(out)
    }

    fn append_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecFillRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // An EMPTY batch never burns the commit key — `HistoryStoreDataFusion::commit_rows` returns
        // `Ok(0)` on `ts.is_empty()` BEFORE it registers the key, so a later real append under the
        // same key still lands there. Without this guard the double would silently swallow that
        // second append and diverge from the store it stands in for.
        if rows.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.execution_fills
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        let map = self.execution_fills.lock().unwrap();
        let mut out: Vec<ExecFillRow> =
            map.get(&(venue.to_string(), symbol.to_string())).cloned().unwrap_or_default();
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }

    fn append_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecOrderRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // An EMPTY batch never burns the commit key — `HistoryStoreDataFusion::commit_rows` returns
        // `Ok(0)` on `ts.is_empty()` BEFORE it registers the key, so a later real append under the
        // same key still lands there. Without this guard the double would silently swallow that
        // second append and diverge from the store it stands in for.
        if rows.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.execution_orders
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        let map = self.execution_orders.lock().unwrap();
        let mut out: Vec<ExecOrderRow> =
            map.get(&(venue.to_string(), symbol.to_string())).cloned().unwrap_or_default();
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }

    fn append_funding(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecutionHistoryFundingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // An EMPTY batch never burns the commit key — `HistoryStoreDataFusion::commit_rows` returns
        // `Ok(0)` on `ts.is_empty()` BEFORE it registers the key, so a later real append under the
        // same key still lands there. Without this guard the double would silently swallow that
        // second append and diverge from the store it stands in for.
        if rows.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.funding
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_funding(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecutionHistoryFundingRow>, HistoryStoreError> {
        let map = self.funding.lock().unwrap();
        let mut out: Vec<ExecutionHistoryFundingRow> = map
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|rows| rows.iter().filter(|r| in_range(r.ts, range)).cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }

    fn append_chain_snapshot(
        &self,
        venue: &str,
        underlying: &str,
        rows: &[ChainRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // An EMPTY batch never burns the commit key — `HistoryStoreDataFusion::commit_rows` returns
        // `Ok(0)` on `ts.is_empty()` BEFORE it registers the key, so a later real append under the
        // same key still lands there. Without this guard the double would silently swallow that
        // second append and diverge from the store it stands in for.
        if rows.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.chains
            .lock()
            .unwrap()
            .entry((venue.to_string(), underlying.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        let map = self.chains.lock().unwrap();
        let mut out: Vec<ChainRow> = map
            .get(&(venue.to_string(), underlying.to_string()))
            .map(|rows| rows.iter().filter(|r| in_range(r.ts, range)).cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|r| r.ts); // stable: within-snapshot append order preserved
        Ok(out)
    }

    fn append_cohort(
        &self,
        venue: &str,
        asset: &str,
        rows: &[CohortRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // Empty-batch guard, for the reason `append_chain_snapshot` states above: an empty append
        // must not burn the key, or a later real append under it silently vanishes here while the
        // real store accepts it.
        if rows.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.cohorts
            .lock()
            .unwrap()
            .entry((venue.to_string(), asset.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        let map = self.cohorts.lock().unwrap();
        let mut out: Vec<CohortRow> = map
            .get(&(venue.to_string(), asset.to_string()))
            .map(|rows| rows.iter().filter(|r| in_range(r.ts, range)).cloned().collect())
            .unwrap_or_default();
        // Stable, so the labels of one hour come back in write order — the same guarantee
        // `HistoryStoreDataFusion::scan_cohort` gives, and the reason a test can assert on order at all.
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }

    fn append_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[PerpMetricRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        // Empty-batch guard, for the reason `append_chain_snapshot` states above: an empty append
        // must not burn the key, or a later real append under it silently vanishes here while the
        // real store accepts it.
        if rows.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.perp_metrics
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        let map = self.perp_metrics.lock().unwrap();
        let mut out: Vec<PerpMetricRow> = map
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|rows| rows.iter().filter(|r| in_range(r.ts, range)).copied().collect())
            .unwrap_or_default();
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }
}
