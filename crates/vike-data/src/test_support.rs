//! DataFusion-free in-memory [`HistStore`] for tests — the test double venue bridges build a
//! [`crate::PropertiesRecorder`] over when asserting their opt-in properties-recording wiring, WITHOUT
//! pulling the heavy Arrow/DataFusion tree into their test builds (the `hist-datafusion` feature).
//!
//! Also home to the shared [`RecordingSink`]/[`NoopSink`] `LiveDataSink` doubles (testing-arch
//! Phase 4d) - one capturing sink instead of the ~13 inline copies the venue bridges carried.
//!
//! Behind the `test-support` feature (off by default). The `bars`, `symbol_properties` and
//! `equity` methods carry real behavior — they honor the batch-level `commit_key` idempotency
//! contract (a repeated key is a no-op) and return rows ts-ascending, matching `DataFusionHist`.
//! Unlike `DataFusionHist` (which has no venue/symbol Parquet column and must re-inject
//! `EquitySample.venue` from the caller's `symbol` argument on scan — see
//! `datafusion_hist::codec::batch_to_equities`), this in-memory double holds the real domain value,
//! so `equity` rows come back with whatever `.venue` the caller originally passed in, untouched.
//! Bars take the OPPOSITE care for the same fidelity reason: [`persisted_bar`] erases on append the
//! fields the real bar schema never persists, so the round trip loses exactly what
//! `DataFusionHist`'s does. The still-inert remainder: quotes/trades/book read empty and append
//! zero rows, depth inherits the trait's refusing defaults, and the two `resample_*_to_bars` verbs
//! return `Ok(0)` HONESTLY rather than fabricating — this double never holds ticks, and resampling
//! zero ticks derives zero bars on the real store too. The catalog pair
//! (`list_series`/`inventory`) answers with a REAL fold over exactly the held seams — an honest
//! empty when nothing was seeded — rather than inheriting a default: the trait's old empty default
//! had a SEEDED double answering "no series" while holding rows, and the refusing default that
//! replaced it would deny a fold this double can genuinely perform.
//!
//! Empty-batch fidelity: every real append here returns `Ok(0)` for an empty `rows` slice WITHOUT
//! consuming the `commit_key`, matching `DataFusionHist::commit_rows` (which short-circuits on
//! `ts.is_empty()` before registering the key). A double that burned the key on an empty batch would
//! turn a subsequent real append under that key into a no-op that happens only in tests.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use vike_model::{
    Bar, BookUpdate, EquitySample, L2Book, Level, QuoteTick, SymbolProperties, TradeTick,
};

use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::funding_log::FundingRow;
use crate::hist::{DataError, HistStore, TsRange};
use crate::live::{LiveDataSink, StreamStatus};
use crate::perp_metrics_log::PerpMetricRow;
use crate::series::{SeriesCoverage, SeriesId};

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
    /// `(venue, symbol=<coin>)` → observed realized funding payments (`kind=funding`).
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
            push("funding", v, s, cov(rows.iter().map(|r| r.ts)));
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

/// What a bar looks like AFTER the real store's round trip: the bar schema persists
/// ts/OHLC/volume/funding and no bid/ask/symbol column, so
/// `crates/vike-data/src/datafusion_hist/codec.rs`'s `bars_from_batch` decodes those three as
/// `None` regardless of what was appended. The double erases at the same boundary (the write), so
/// a test asserting `symbol` survives `append_bars` → `load_bars` fails here exactly as it would
/// on `DataFusionHist`.
fn persisted_bar(b: &Bar) -> Bar {
    Bar { bid: None, ask: None, symbol: None, ..b.clone() }
}

impl HistStore for MemHistStore {
    /// Held bars for the `(venue, symbol, interval)` triple in `range` (inclusive both ends,
    /// `None` = unbounded), ts-ascending — the stable sort mirrors `BarCodec`'s
    /// `sort_key = (ts, 0)`, so equal-ts bars keep append order on both stores. An unknown triple
    /// answers an honest empty, exactly as the real store's missing series directory reads as an
    /// empty manifest.
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

// ===================================================================================================
// Shared `LiveDataSink` doubles (testing-arch Phase 4d).
// ===================================================================================================

/// One recorded [`LiveDataSink`] call, with its full payload — what [`RecordingSink`] captures.
/// Structured (not pre-formatted) so a test can assert on exactly the fields it cares about via
/// the typed accessors, while [`RecordingSink::calls`] renders the canonical one-line form for
/// exact-sequence assertions.
#[derive(Debug, Clone)]
pub enum SinkCall {
    SeedBars {
        venue: String,
        symbol: String,
        interval: String,
        bars: Vec<Bar>,
    },
    CloseBar {
        venue: String,
        symbol: String,
        interval: String,
        bar: Bar,
    },
    FormingBar {
        venue: String,
        symbol: String,
        interval: String,
        bar: Bar,
    },
    MarkTick {
        venue: String,
        symbol: String,
        px: f64,
        ts: i64,
    },
    BarCloseTick {
        venue: String,
        symbol: String,
        px: f64,
        ts: i64,
    },
    L2Snapshot {
        venue: String,
        symbol: String,
        tick_size: f64,
        bids: Vec<Level>,
        asks: Vec<Level>,
        ts: i64,
    },
    Quote {
        venue: String,
        symbol: String,
        quote: QuoteTick,
    },
    Trade {
        venue: String,
        symbol: String,
        trade: TradeTick,
    },
    Book {
        venue: String,
        symbol: String,
        book: L2Book,
    },
    BookUpdate {
        venue: String,
        symbol: String,
        update: BookUpdate,
    },
    StreamStatus {
        venue: String,
        symbol: String,
        stream: String,
        status: StreamStatus,
    },
}

impl std::fmt::Display for SinkCall {
    /// The canonical one-line form, the UNION of the formats the per-crate copies used (the
    /// bar-lane `verb(venue,symbol,…)` convention from `vike_data::live`'s original double, the
    /// tick-lane `verb:venue:symbol:…` convention at polymarket's richer field set) — so
    /// exact-sequence assertions read unchanged where they existed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SinkCall::SeedBars { venue, symbol, interval, bars } => {
                write!(f, "seed_bars({venue},{symbol},{interval},{})", bars.len())
            }
            SinkCall::CloseBar { venue, symbol, interval, bar } => {
                write!(f, "close_bar({venue},{symbol},{interval},{})", bar.close)
            }
            SinkCall::FormingBar { venue, symbol, interval, bar } => {
                write!(f, "forming_bar({venue},{symbol},{interval},{})", bar.close)
            }
            SinkCall::MarkTick { venue, symbol, px, ts } => {
                write!(f, "mark_tick({venue},{symbol},{px},{ts})")
            }
            SinkCall::BarCloseTick { venue, symbol, px, ts } => {
                write!(f, "bar_close_tick({venue},{symbol},{px},{ts})")
            }
            SinkCall::L2Snapshot { venue, symbol, tick_size, bids, asks, ts } => {
                write!(
                    f,
                    "l2_snapshot({venue},{symbol},{tick_size},{}b/{}a,{ts})",
                    bids.len(),
                    asks.len()
                )
            }
            SinkCall::Quote { venue, symbol, quote } => {
                write!(
                    f,
                    "quote:{venue}:{symbol}:{}/{}:{}x{}",
                    quote.bid, quote.ask, quote.bid_size, quote.ask_size
                )
            }
            SinkCall::Trade { venue, symbol, trade } => {
                write!(
                    f,
                    "trade:{venue}:{symbol}:{}/{}:maker={}",
                    trade.price, trade.size, trade.is_buyer_maker
                )
            }
            SinkCall::Book { venue, symbol, book } => {
                write!(f, "book:{venue}:{symbol}:{:?}", book.mid())
            }
            SinkCall::BookUpdate { venue, symbol, update } => {
                write!(
                    f,
                    "book_update:{venue}:{symbol}:{:?}:seq={}:bids={}:asks={}:local_ts_pos={}:tick={}",
                    update.kind,
                    update.seq,
                    update.bids.len(),
                    update.asks.len(),
                    update.local_ts > 0,
                    update.tick_size,
                )
            }
            SinkCall::StreamStatus { venue, symbol, stream, status } => {
                write!(f, "stream_status:{venue}:{symbol}:{stream}:{status:?}")
            }
        }
    }
}

/// The ONE shared capturing [`LiveDataSink`] (testing-arch Phase 4d): records EVERY sink call —
/// full payloads, in delivery order — into a `Mutex<Vec<SinkCall>>`. Replaces the ~13 per-crate
/// inline copies (vike-data's own, the crypto bridges' `market_feed` test modules and smoke
/// tests, polymarket's scripted-pump tests, ctrader's `tests/common`).
///
/// Two assertion styles, covering the union of what those copies asserted:
/// - [`RecordingSink::calls`] — the canonical formatted strings ([`SinkCall`]'s `Display`), for
///   exact-emission-sequence assertions;
/// - the typed accessors ([`RecordingSink::quotes`], [`RecordingSink::trades`], …) — full
///   payloads for field-level assertions (ts threading, per-fill deltas, counts on live smokes).
#[derive(Default)]
pub struct RecordingSink {
    calls: Mutex<Vec<SinkCall>>,
}

impl RecordingSink {
    fn push(&self, call: SinkCall) {
        self.calls.lock().unwrap().push(call);
    }

    /// Every recorded call, in delivery order.
    pub fn recorded(&self) -> Vec<SinkCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Every recorded call in the canonical one-line form (see [`SinkCall`]'s `Display`).
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|c| c.to_string()).collect()
    }

    /// Recorded `quote` calls: `(venue, symbol, quote)`, in order.
    pub fn quotes(&self) -> Vec<(String, String, QuoteTick)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::Quote { venue, symbol, quote } => {
                    Some((venue.clone(), symbol.clone(), quote.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `trade` calls: `(venue, symbol, trade)`, in order.
    pub fn trades(&self) -> Vec<(String, String, TradeTick)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::Trade { venue, symbol, trade } => {
                    Some((venue.clone(), symbol.clone(), trade.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `book` calls: `(venue, symbol, book)`, in order.
    pub fn books(&self) -> Vec<(String, String, L2Book)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::Book { venue, symbol, book } => {
                    Some((venue.clone(), symbol.clone(), book.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `book_update` calls: `(venue, symbol, update)`, in order.
    pub fn book_updates(&self) -> Vec<(String, String, BookUpdate)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::BookUpdate { venue, symbol, update } => {
                    Some((venue.clone(), symbol.clone(), update.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `seed_bars` calls: `(venue, symbol, interval, bars)`, in order.
    pub fn seeded_bars(&self) -> Vec<(String, String, String, Vec<Bar>)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::SeedBars { venue, symbol, interval, bars } => {
                    Some((venue.clone(), symbol.clone(), interval.clone(), bars.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `forming_bar` calls: `(venue, symbol, interval, bar)`, in order.
    pub fn forming_bars(&self) -> Vec<(String, String, String, Bar)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::FormingBar { venue, symbol, interval, bar } => {
                    Some((venue.clone(), symbol.clone(), interval.clone(), bar.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `close_bar` calls: `(venue, symbol, interval, bar)`, in order.
    pub fn closed_bars(&self) -> Vec<(String, String, String, Bar)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::CloseBar { venue, symbol, interval, bar } => {
                    Some((venue.clone(), symbol.clone(), interval.clone(), bar.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Recorded `l2_snapshot` calls: `(venue, symbol, tick_size, bids, asks, ts)`, in order.
    #[allow(clippy::type_complexity)] // the verb's own signature, tuple-captured
    pub fn l2_snapshots(&self) -> Vec<(String, String, f64, Vec<Level>, Vec<Level>, i64)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SinkCall::L2Snapshot { venue, symbol, tick_size, bids, asks, ts } => Some((
                    venue.clone(),
                    symbol.clone(),
                    *tick_size,
                    bids.clone(),
                    asks.clone(),
                    *ts,
                )),
                _ => None,
            })
            .collect()
    }
}

impl LiveDataSink for RecordingSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.push(SinkCall::SeedBars {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            bars,
        });
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(SinkCall::CloseBar {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            bar,
        });
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(SinkCall::FormingBar {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            bar,
        });
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push(SinkCall::MarkTick { venue: venue.into(), symbol: symbol.into(), px, ts });
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push(SinkCall::BarCloseTick { venue: venue.into(), symbol: symbol.into(), px, ts });
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<Level>,
        asks: Vec<Level>,
        ts: i64,
    ) {
        self.push(SinkCall::L2Snapshot {
            venue: venue.into(),
            symbol: symbol.into(),
            tick_size,
            bids,
            asks,
            ts,
        });
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.push(SinkCall::Quote { venue: venue.into(), symbol: symbol.into(), quote });
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.push(SinkCall::Trade { venue: venue.into(), symbol: symbol.into(), trade });
    }
    fn book(&self, venue: &str, symbol: &str, book: std::sync::Arc<L2Book>) {
        // `SinkCall::Book` keeps an OWNED book so every existing reader (`books()`, the `Display`
        // rendering) is untouched; this test-double clone is off any hot path.
        self.push(SinkCall::Book {
            venue: venue.into(),
            symbol: symbol.into(),
            book: (*book).clone(),
        });
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        self.push(SinkCall::BookUpdate { venue: venue.into(), symbol: symbol.into(), update });
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.push(SinkCall::StreamStatus {
            venue: venue.into(),
            symbol: symbol.into(),
            stream: stream.into(),
            status,
        });
    }
}

/// A [`LiveDataSink`] that does nothing — for tests that need a warm body (a `Feeds`/client
/// constructor argument) but never assert on delivered data.
pub struct NoopSink;

impl LiveDataSink for NoopSink {
    fn seed_bars(&self, _venue: &str, _symbol: &str, _interval: &str, _bars: Vec<Bar>) {}
    fn close_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn forming_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn mark_tick(&self, _venue: &str, _symbol: &str, _px: f64, _ts: i64) {}
    fn quote(&self, _venue: &str, _symbol: &str, _quote: QuoteTick) {}
    fn trade(&self, _venue: &str, _symbol: &str, _trade: TradeTick) {}
    fn book(&self, _venue: &str, _symbol: &str, _book: std::sync::Arc<L2Book>) {}
}

// ===================================================================================================
// The double's own unit tests — the BAR verbs. The older seams are covered by the integration
// suites in `tests/` (equity_series.rs's `mem_tests`, exec_log_series.rs, funding_series.rs,
// chain_series.rs, hist_datafusion.rs's seeded-catalog test); bars are tested HERE, beside the
// erasure helper whose behavior half of them pin. Runs in the plain `cargo test -p vike-data`
// roster lane: `lib.rs` compiles this module under `#[cfg(any(test, feature = "test-support"))]`.
// ===================================================================================================

#[cfg(test)]
mod tests {
    use super::MemHistStore;
    use crate::hist::{HistStore, TsRange};
    use crate::series::SeriesId;
    use vike_model::Bar;

    /// A bar with every field spelled (`Bar` has no `Default`), already in the persisted shape —
    /// bid/ask/symbol `None` — so round-trip assertions compare against the helper's own output.
    /// The erasure test builds its `Some`-bearing bar explicitly, on top of this one.
    fn bar(ts: i64, close: f64) -> Bar {
        Bar {
            ts,
            open: close - 1.0,
            high: close + 1.0,
            low: close - 2.0,
            close,
            volume: 10.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    #[test]
    fn bars_roundtrip_ts_ascending_after_an_unordered_append() {
        let store = MemHistStore::new();
        let unordered = vec![bar(2_000, 101.0), bar(1_000, 100.0), bar(3_000, 102.0)];
        assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &unordered, None).unwrap(), 3);
        assert_eq!(
            store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap(),
            vec![bar(1_000, 100.0), bar(2_000, 101.0), bar(3_000, 102.0)],
            "load re-sorts ts-ascending, as the real store's scan does"
        );
    }

    /// Pins the two claims the `load_bars` doc makes about equal-ts rows, against the refactor
    /// that would silently break both: re-keying storage by ts (a `BTreeMap<i64, Bar>` "cleanup")
    /// collapses duplicates last-wins, while the real store NEVER dedups by row value (the trait's
    /// ingest contract) — `datafusion_multiple_parts_merge_ordered` keeps every overlapping row.
    /// Both duplicate-ts bars must survive, in append order (the stable sort's `(ts, 0)` tiebreak).
    #[test]
    fn duplicate_ts_bars_both_survive_in_append_order() {
        let store = MemHistStore::new();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 1.0)], None).unwrap();
        store
            .append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 2.0), bar(500, 3.0)], None)
            .unwrap();
        let got = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
        assert_eq!(
            got.iter().map(|b| (b.ts, b.close)).collect::<Vec<_>>(),
            vec![(500, 3.0), (1_000, 1.0), (1_000, 2.0)],
            "no value dedup, and equal-ts rows keep append order across batches"
        );
    }

    #[test]
    fn load_bars_respects_the_inclusive_range_bounds() {
        let store = MemHistStore::new();
        let bars = vec![bar(1_000, 1.0), bar(2_000, 2.0), bar(3_000, 3.0)];
        store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
        let ts_in = |r: TsRange| -> Vec<i64> {
            store.load_bars("binance", "BTCUSDT", "1m", r).unwrap().iter().map(|b| b.ts).collect()
        };
        assert_eq!(ts_in(TsRange::of(1_000, 2_000)), vec![1_000, 2_000], "both ends inclusive");
        assert_eq!(ts_in(TsRange { start: Some(2_000), end: None }), vec![2_000, 3_000]);
        assert_eq!(ts_in(TsRange { start: None, end: Some(1_999) }), vec![1_000]);
        assert!(ts_in(TsRange::of(10_000, 20_000)).is_empty(), "a disjoint range is empty");
    }

    #[test]
    fn bars_are_keyed_by_the_raw_interval_string() {
        let store = MemHistStore::new();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 1.0)], None).unwrap();
        store.append_bars("binance", "BTCUSDT", "1h", &[bar(1_000, 2.0)], None).unwrap();
        let m = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
        let h = store.load_bars("binance", "BTCUSDT", "1h", TsRange::all()).unwrap();
        assert_eq!((m.len(), h.len()), (1, 1), "each interval loads only its own series");
        assert_eq!(m[0].close, 1.0);
        assert_eq!(h[0].close, 2.0);
        // "60s" IS one minute semantically, but the raw string is the key — exactly as the real
        // store's `bars_dir` path segment behaves (no interval validation on append or load).
        assert!(store.load_bars("binance", "BTCUSDT", "60s", TsRange::all()).unwrap().is_empty());
    }

    #[test]
    fn bar_series_are_isolated_per_venue_and_symbol() {
        let store = MemHistStore::new();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 1.0)], None).unwrap();
        store.append_bars("bybit", "BTCUSDT", "1m", &[bar(1_000, 2.0)], None).unwrap();
        store.append_bars("binance", "ETHUSDT", "1m", &[bar(1_000, 3.0)], None).unwrap();
        let got = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].close, 1.0, "no cross-talk from the other venue or the other symbol");
    }

    /// The one contract every pre-existing consumer already leans on (the CLI/datahub suites
    /// assert their unseeded stores load no bars) — the control that stays green through the
    /// stub-to-real change, proving real storage widened nothing for a store nobody seeded.
    #[test]
    fn an_empty_store_answers_load_bars_with_an_honest_empty() {
        let store = MemHistStore::new();
        assert!(store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().is_empty());
    }

    #[test]
    fn bars_commit_key_is_idempotent() {
        let store = MemHistStore::new();
        let rows = vec![bar(1_000, 1.0)];
        assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 1);
    }

    #[test]
    fn an_empty_bar_batch_never_burns_the_commit_key() {
        let store = MemHistStore::new();
        assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &[], Some("k")).unwrap(), 0);
        // The later REAL append under the same key still lands — `DataFusionHist::commit_rows`
        // checks emptiness before registering the key, and so must the double.
        let rows = vec![bar(1_000, 1.0)];
        assert_eq!(store.append_bars("binance", "BTCUSDT", "1m", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 1);
    }

    #[test]
    fn load_bars_erases_bid_ask_symbol_like_the_real_codec() {
        let store = MemHistStore::new();
        let appended = Bar {
            bid: Some(99.5),
            ask: Some(100.5),
            symbol: Some("BTCUSDT.BINANCE".into()),
            funding: Some(0.01),
            ..bar(1_000, 100.0)
        };
        store.append_bars("binance", "BTCUSDT", "1m", &[appended], None).unwrap();
        let got = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
        assert_eq!(got.len(), 1);
        let b = &got[0];
        assert_eq!(
            (b.bid, b.ask, b.symbol.as_deref()),
            (None, None, None),
            "the three fields the bar schema never persists must not survive the round trip"
        );
        assert_eq!(b.funding, Some(0.01), "funding IS a persisted column and survives");
        assert_eq!(
            (b.ts, b.open, b.high, b.low, b.close, b.volume),
            (1_000, 99.0, 101.0, 98.0, 100.0, 10.0),
            "OHLCV rides through untouched"
        );
    }

    #[test]
    fn seeded_bars_join_the_catalog_with_their_interval() {
        let store = MemHistStore::new();
        store
            .append_bars("binance", "BTCUSDT", "1m", &[bar(1_000, 1.0), bar(2_000, 2.0)], None)
            .unwrap();
        assert_eq!(
            store.list_series().unwrap(),
            vec![SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".into()))],
            "bar series carry their interval, like the real store's `interval=` path segment"
        );
        let inv = store.inventory().unwrap();
        assert_eq!(inv.len(), 1, "one held series, one coverage row");
        assert_eq!(inv[0].1.rows, 2, "rows is the held row count");
        assert_eq!(inv[0].1.first_ts, 1_000, "first_ts is the earliest held ts");
        assert_eq!(inv[0].1.last_ts, 2_000, "last_ts is the latest held ts");
        assert_eq!(inv[0].1.bytes, 0, "an in-memory store truly occupies zero bytes on disk");
    }
}
