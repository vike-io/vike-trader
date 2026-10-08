//! `MemHistStore`: the DataFusion-free in-memory `HistStore` double and its catalog fold.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

use super::{HistStore, persisted_bar};
use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::funding_log::FundingRow;
use crate::perp_metrics_log::PerpMetricRow;
use crate::store::hist::{DataError, TsRange};
use crate::store::series::{SeriesCoverage, SeriesId};

/// OHLCV bars observed for one `(venue, symbol, interval)` series, in append order — already
/// through [`persisted_bar`], so what is held is what the real store would give back.
type BarRows = Vec<Bar>;

/// `(ts_ms, properties)` rows observed for one `(venue, symbol)` series, in append order.
type PropertiesRows = Vec<(i64, SymbolProperties)>;

/// Equity-curve samples observed for one `(venue, symbol)` series, in append order.
type EquityRows = Vec<EquitySample>;

/// Account fills observed for one `(venue, symbol)` series, in append order.
type ExecFillRows = Vec<ExecFillRow>;

/// Order lifecycle snapshots observed for one `(venue, symbol)` series, in append order.
type ExecOrderRows = Vec<ExecOrderRow>;

/// Realized funding payments observed for one `(venue, symbol=<coin>)` series, in append order.
type FundingRows = Vec<FundingRow>;

/// Option-chain snapshot rows observed for one `(venue, symbol=<underlying>)` series, in append
/// order.
type ChainRows = Vec<ChainRow>;

/// Cohort marginals observed for one `(venue, symbol=<asset>)` series, in append order — every
/// axis, label, grading and label basis that was written, exactly as the real series holds them.
type CohortRows = Vec<CohortRow>;

/// Perp market-context rows observed for one `(venue, symbol)` series, in append order.
type PerpMetricRows = Vec<PerpMetricRow>;

/// In-memory capturing [`HistStore`]: records `append_bars`/`append_symbol_properties`/
/// `append_equity` rows so a test can load them back. Thread-safe (`Mutex`), so it drops into an
/// `Arc<dyn HistStore>` a recorder can hold.
#[derive(Default)]
pub struct MemHistStore {
    /// `(venue, symbol, interval)` → observed OHLCV bars (`kind=bar`).
    bars: Mutex<HashMap<(String, String, String), BarRows>>,
    /// `(venue, symbol)` → observed properties rows.
    properties: Mutex<HashMap<(String, String), PropertiesRows>>,
    /// `(venue, symbol)` → observed equity-curve samples (portfolio-observer PR-3 Task 2).
    equity: Mutex<HashMap<(String, String), EquityRows>>,
    /// `(venue, symbol)` → observed account fills (`kind=exec_fill`).
    exec_fills: Mutex<HashMap<(String, String), ExecFillRows>>,
    /// `(venue, symbol)` → observed order lifecycle snapshots (`kind=exec_order`).
    exec_orders: Mutex<HashMap<(String, String), ExecOrderRows>>,
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

impl MemHistStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The REAL catalog fold both `HistStore::list_series` and `HistStore::inventory` answer from:
    /// one `(SeriesId, SeriesCoverage)` row per series this double actually holds, one kind per
    /// storage map, sorted by id (matching `DataFusionHist::list_series`'s sorted contract). Bar
    /// rows carry their interval — `kind=bar` sub-partitions by bar step, so the id mirrors what
    /// the real store parses back out of its `interval=` path segment; every tick-shaped kind
    /// carries `None`. An unseeded store folds to an EMPTY catalog — an honest empty, from a
    /// store that looked — where inheriting the trait's refusing default would claim the double
    /// cannot enumerate at all, and inheriting the OLD empty default (as this double used to) had a
    /// SEEDED store answering "no series" while holding rows.
    ///
    /// Coverage is the fold's truth, not `DataFusionHist`'s: `first_ts`/`last_ts`/`rows` are
    /// computed from the held rows, while `bytes`/`parts`/`dates` stay 0 because they are ON-DISK
    /// facts (summed part-file size, part count, `date=` partition count) and an in-memory store
    /// genuinely has none of any of them.
    fn catalog(&self) -> Vec<(SeriesId, SeriesCoverage)> {
        fn cov(ts: impl Iterator<Item = i64>) -> SeriesCoverage {
            let mut out =
                SeriesCoverage { first_ts: i64::MAX, last_ts: i64::MIN, ..Default::default() };
            for t in ts {
                out.first_ts = out.first_ts.min(t);
                out.last_ts = out.last_ts.max(t);
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
                SeriesId::per_symbol("bar", v.as_str(), s.as_str(), Some(i.clone())),
                cov(rows.iter().map(|b| b.ts)),
            ));
        }
        let mut push = |kind: &str, venue: &str, symbol: &str, c: SeriesCoverage| {
            out.push((SeriesId::per_symbol(kind, venue, symbol, None), c));
        };
        for ((v, s), rows) in self.properties.lock().unwrap().iter() {
            push("properties", v, s, cov(rows.iter().map(|(ts, _)| *ts)));
        }
        for ((v, s), rows) in self.equity.lock().unwrap().iter() {
            push("equity", v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.exec_fills.lock().unwrap().iter() {
            push("exec_fill", v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.exec_orders.lock().unwrap().iter() {
            push("exec_order", v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.funding.lock().unwrap().iter() {
            push("exec_funding", v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.chains.lock().unwrap().iter() {
            push("chain", v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.cohorts.lock().unwrap().iter() {
            push("cohort", v, s, cov(rows.iter().map(|r| r.ts)));
        }
        for ((v, s), rows) in self.perp_metrics.lock().unwrap().iter() {
            push("perp_metrics", v, s, cov(rows.iter().map(|r| r.ts)));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

fn in_range(ts: i64, range: TsRange) -> bool {
    range.start.map(|s| ts >= s).unwrap_or(true) && range.end.map(|e| ts <= e).unwrap_or(true)
}

impl HistStore for MemHistStore {
    /// Held bars for the `(venue, symbol, interval)` triple in `range` (inclusive both ends,
    /// `None` = unbounded), ts-ascending — the stable sort mirrors `BarCodec`'s
    /// `sort_key = (ts, 0)`, so equal-ts bars keep append order on both stores. An unknown triple
    /// answers an honest empty, exactly as the real store's missing series directory reads as an
    /// empty manifest.
    ///
    /// `HistStore::bar_edges` is NOT overridden: this double is memory-resident, so the trait
    /// default (the edges of THIS read) is exact and there is nothing for an override to bound. It
    /// is also what keeps a second implementation of the contract out of a test double —
    /// `test_support_tests.rs` pins the default through it.
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        let map = self.bars.lock().unwrap();
        let mut out: Vec<Bar> = map
            .get(&(venue.to_string(), symbol.to_string(), interval.to_string()))
            .map(|rows| rows.iter().filter(|b| in_range(b.ts, range)).cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|b| b.ts);
        Ok(out)
    }
    fn scan_quotes(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        Ok(Vec::new())
    }
    fn scan_trades(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<TradeTick>, DataError> {
        Ok(Vec::new())
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
    ) -> Result<usize, DataError> {
        // An EMPTY batch never burns the commit key — `DataFusionHist::commit_rows` returns
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
    fn append_quotes(
        &self,
        _v: &str,
        _s: &str,
        _t: &[QuoteTick],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_trades(
        &self,
        _v: &str,
        _s: &str,
        _t: &[TradeTick],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_book_updates(
        &self,
        _v: &str,
        _s: &str,
        _u: &[BookUpdate],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_book_updates(
        &self,
        _v: &str,
        _s: &str,
        _r: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Ok(Vec::new())
    }
    // `append_depth`/`scan_depth` are NOT overridden: this double holds neither book nor depth, so
    // the trait's defaults — which now REFUSE on both halves — are the honest answer for it, and
    // inheriting them means this file does not grow a stub every time the seam gains a kind. ⚠ The
    // read half is the one that changed: it used to inherit an empty `Ok`, which made this double
    // indistinguishable from a real store that holds no depth rows in the range asked for.

    /// The REAL catalog: what this double actually holds, kind by kind (see
    /// [`MemHistStore::catalog`]). It used to inherit the trait's old empty default, which had a
    /// SEEDED store answering "no series" while holding rows — the same fabrication the default
    /// itself carried, worn by the one impl whose truthful answer costs a fold over its maps.
    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        Ok(self.catalog().into_iter().map(|(id, _)| id).collect())
    }

    /// The coverage twin of [`HistStore::list_series`], from the same fold — `first_ts`/`last_ts`/
    /// `rows` are the held rows' truth; the on-disk members stay 0 (see [`MemHistStore::catalog`]).
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
        Ok(self.catalog())
    }

    fn append_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[(i64, SymbolProperties)],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        // An EMPTY batch never burns the commit key — `DataFusionHist::commit_rows` returns
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
        range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
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
    ) -> Result<usize, DataError> {
        // An EMPTY batch never burns the commit key — `DataFusionHist::commit_rows` returns
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
        range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        let map = self.equity.lock().unwrap();
        let mut out: Vec<EquitySample> = map
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|rows| rows.iter().filter(|r| in_range(r.ts, range)).cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }

    fn append_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecFillRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        // An EMPTY batch never burns the commit key — `DataFusionHist::commit_rows` returns
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
        self.exec_fills
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_exec_fills(&self, venue: &str, symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        let map = self.exec_fills.lock().unwrap();
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
    ) -> Result<usize, DataError> {
        // An EMPTY batch never burns the commit key — `DataFusionHist::commit_rows` returns
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
        self.exec_orders
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(rows);
        Ok(rows.len())
    }

    fn scan_exec_orders(&self, venue: &str, symbol: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        let map = self.exec_orders.lock().unwrap();
        let mut out: Vec<ExecOrderRow> =
            map.get(&(venue.to_string(), symbol.to_string())).cloned().unwrap_or_default();
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }

    fn append_funding(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[FundingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        // An EMPTY batch never burns the commit key — `DataFusionHist::commit_rows` returns
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
        range: TsRange,
    ) -> Result<Vec<FundingRow>, DataError> {
        let map = self.funding.lock().unwrap();
        let mut out: Vec<FundingRow> = map
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
    ) -> Result<usize, DataError> {
        // An EMPTY batch never burns the commit key — `DataFusionHist::commit_rows` returns
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
        range: TsRange,
    ) -> Result<Vec<ChainRow>, DataError> {
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
    ) -> Result<usize, DataError> {
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
        range: TsRange,
    ) -> Result<Vec<CohortRow>, DataError> {
        let map = self.cohorts.lock().unwrap();
        let mut out: Vec<CohortRow> = map
            .get(&(venue.to_string(), asset.to_string()))
            .map(|rows| rows.iter().filter(|r| in_range(r.ts, range)).cloned().collect())
            .unwrap_or_default();
        // Stable, so the labels of one hour come back in write order — the same guarantee
        // `DataFusionHist::scan_cohort` gives, and the reason a test can assert on order at all.
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }

    fn append_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[PerpMetricRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
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
        range: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        let map = self.perp_metrics.lock().unwrap();
        let mut out: Vec<PerpMetricRow> = map
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|rows| rows.iter().filter(|r| in_range(r.ts, range)).copied().collect())
            .unwrap_or_default();
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }

    fn resample_quotes_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn resample_trades_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
}
