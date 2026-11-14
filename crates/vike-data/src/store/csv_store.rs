//! `HistoryStoreCsv`: a READ-ONLY `HistoryStore` over one CSV file of bars (decision 0268, verdict 5), what a
//! backtest reads when its profile names `data.file`, without importing the file.
//!
//! It holds ONE series: the identity its opener gives the file's bars, under the source
//! [`FILE_SOURCE`], so it lists as `kind=bar`, `source=file` and is read only through
//! `load_bars_sourced_capped` naming that source. A read of the sourceless series, the venue's own
//! candles, is REFUSED: a file's bars can never be read as the venue's. Every write, every other kind and
//! every other lane is refused with the reason; another bar series of the file lane is the missing
//! series' empty answer.
//!
//! The bars come through the one decoder, `crate::csv_bars`, so a file this store reads is a file the
//! `csv-bars` import takes, line for line. Beside `crate::HistoryStoreMemory` in spirit and NOT test-only: it
//! ships, so its refusals are `crate::hist_store_stubs!`'s, which ships with it.

use std::path::Path;

use vike_marketdata::Bar;
use vike_marketdata::interval::bar_step_ms;

use crate::cohort_log::CohortRow;
use crate::csv_bars::{CsvBarsColumns, CsvBarsFormat, decode_bars};
use crate::perp_metrics_log::PerpMetricRow;
use crate::store::coverage::find_gaps;
use crate::store::hist::{HistoryStore, HistoryStoreError, TimestampRange};
use crate::store::series::{SeriesCoverage, SeriesId};
use crate::store::store_kind::{FILE_SOURCE, StoreKindId};

/// One CSV file's bars, as the series `(venue, symbol, interval)` of the [`FILE_SOURCE`] lane.
#[derive(Debug, Clone)]
pub struct HistoryStoreCsv {
    /// What a refusal calls the file: its path as it was opened.
    name: String,
    id: SeriesId,
    bars: Vec<Bar>,
    step_ms: i64,
}

impl HistoryStoreCsv {
    /// Read the file at `path` as the bars of `(venue, symbol, interval)`, its columns found by
    /// `columns` and its cells read in `format`.
    ///
    /// # Errors
    /// [`HistoryStoreError::Io`] when the file cannot be read; [`HistoryStoreError::Query`] naming the file, the line and
    /// the rule when the decoder refuses it, or when its step is not `interval`'s.
    pub fn open(
        path: &Path,
        columns: &CsvBarsColumns,
        format: &CsvBarsFormat,
        venue: &str,
        symbol: &str,
        interval: &str,
    ) -> Result<Self, HistoryStoreError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| HistoryStoreError::Io(format!("{}: {e}", path.display())))?;
        let name = path.display().to_string();
        Self::from_csv(&name, &text, columns, format, venue, symbol, interval)
    }

    /// [`Self::open`] over a file's text already in hand; `name` is what a refusal calls it.
    ///
    /// # Errors
    /// [`Self::open`]'s, but for the read.
    pub fn from_csv(
        name: &str,
        text: &str,
        columns: &CsvBarsColumns,
        format: &CsvBarsFormat,
        venue: &str,
        symbol: &str,
        interval: &str,
    ) -> Result<Self, HistoryStoreError> {
        let Some(step_ms) = bar_step_ms(interval) else {
            return Err(HistoryStoreError::Query(format!(
                "{name}: the interval {interval:?} has no fixed width, so the file's step cannot be \
                 checked against it"
            )));
        };
        let decoded = decode_bars(text, columns, format)
            .map_err(|e| HistoryStoreError::Query(format!("{name}: {e}")))?;
        if let Some(step) = decoded.step_ms
            && step != step_ms
        {
            return Err(HistoryStoreError::Query(format!(
                "{name}: its bars are {step} ms apart and the series asked for is {interval} \
                 ({step_ms} ms): a file holds the one interval it was written in"
            )));
        }
        let id = SeriesId::per_symbol(StoreKindId::Bar, venue, symbol, Some(interval.to_string()))
            .with_source(FILE_SOURCE);
        Ok(Self { name: name.to_string(), id, bars: decoded.bars, step_ms })
    }

    /// The one series this store holds.
    pub fn series(&self) -> &SeriesId {
        &self.id
    }

    fn holds(&self, venue: &str, symbol: &str, interval: &str) -> bool {
        self.id.venue == venue
            && self.id.symbol == symbol
            && self.id.interval.as_deref() == Some(interval)
    }

    fn refuse_kind(&self, kind: StoreKindId) -> HistoryStoreError {
        HistoryStoreError::Unsupported(format!(
            "{} is a CSV file of bars and holds no kind={kind}; nothing was read or written",
            self.name
        ))
    }
}

/// The refusal of every verb a file of bars does not serve, for `crate::hist_store_stubs!`.
fn refused(verb: &'static str) -> HistoryStoreError {
    HistoryStoreError::Unsupported(format!(
        "{verb}: a CSV file store holds one series of bars and writes nothing"
    ))
}

impl HistoryStore for HistoryStoreCsv {
    /// REFUSED: this store holds no sourceless series, and the sourceless series is the venue's own
    /// candles. `bar_edges` and `load_bars_head` read through this, so they refuse too.
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let _ = (venue, symbol, interval, range);
        Err(HistoryStoreError::Query(format!(
            "{} holds a file's bars under source={FILE_SOURCE}, never a venue's own series: read \
             them with load_bars_sourced_capped and the source {FILE_SOURCE:?}",
            self.name
        )))
    }

    /// The file's bars in `range`, ts-ascending, for the [`FILE_SOURCE`] lane; another series of that
    /// lane is empty, another lane refused. `budget` bounds nothing: the bars are already in memory, and
    /// the whole range is a complete prefix of itself.
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
        if source != FILE_SOURCE {
            return Err(HistoryStoreError::Query(format!(
                "{} holds only the {FILE_SOURCE:?} lane, not `source={source}`",
                self.name
            )));
        }
        if !self.holds(venue, symbol, interval) {
            return Ok(Vec::new());
        }
        Ok(self
            .bars
            .iter()
            .filter(|b| {
                range.start.is_none_or(|s| b.timestamp >= s)
                    && range.end.is_none_or(|e| b.timestamp <= e)
            })
            .cloned()
            .collect())
    }

    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        Ok(vec![self.id.clone()])
    }

    /// The one series and its coverage from the bars; `bytes`, `parts` and `dates` are a store's
    /// on-disk facts and stay 0, as `crate::HistoryStoreMemory`'s do.
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        let (Some(first), Some(last)) = (self.bars.first(), self.bars.last()) else {
            return Ok(vec![(self.id.clone(), SeriesCoverage::default())]);
        };
        let cov = SeriesCoverage {
            first_timestamp: first.timestamp,
            last_timestamp: last.timestamp,
            rows: self.bars.len() as u64,
            ..Default::default()
        };
        Ok(vec![(self.id.clone(), cov)])
    }

    /// The missing bars inside the file's span, as inclusive `[first, last]` bar times.
    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        if *id != self.id {
            return Ok(Vec::new());
        }
        let ts: Vec<i64> = self.bars.iter().map(|b| b.timestamp).collect();
        Ok(find_gaps(&ts, self.step_ms))
    }

    // The two kinds whose trait defaults still answer empty (`crate::store::hist`'s cohort block):
    // overridden, because this store refuses every kind it does not hold.
    fn append_cohort(
        &self,
        venue: &str,
        asset: &str,
        rows: &[CohortRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (venue, asset, rows, commit_key);
        Err(self.refuse_kind(StoreKindId::Cohort))
    }

    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        let _ = (venue, asset, range);
        Err(self.refuse_kind(StoreKindId::Cohort))
    }

    fn append_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[PerpMetricRow],
        commit_key: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        let _ = (venue, symbol, rows, commit_key);
        Err(self.refuse_kind(StoreKindId::PerpMetrics))
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        let _ = (venue, symbol, range);
        Err(self.refuse_kind(StoreKindId::PerpMetrics))
    }

    crate::hist_store_stubs!(refuse(refused): writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

#[path = "csv_store_tests.rs"]
#[cfg(test)]
mod csv_store_tests;
