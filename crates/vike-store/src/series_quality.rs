//! The store-scanning convenience over `vike_data::store::quality`: read a recorded series out of a
//! [`HistoryStoreDataFusion`] and score it per day. It lives with the store engine — the pure scorers need
//! no DataFusion. These methods READ ONLY (one `scan_*` each); they write nothing.

use vike_data::store::hist::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_data::store::quality::{
    DayQuality, DayQualityConfig, score_book_series, score_quote_series, score_trade_series,
};

use crate::HistoryStoreDataFusion;

impl HistoryStoreDataFusion {
    /// Per-UTC-day data-quality report for the recorded BOOK series `(venue, symbol)`.
    pub fn book_series_quality(
        &self,
        venue: &str,
        symbol: &str,
        config: &DayQualityConfig,
    ) -> Result<Vec<DayQuality>, HistoryStoreError> {
        let rows = self.scan_book_updates(venue, symbol, TimestampRange::all())?;
        Ok(score_book_series(venue, symbol, &rows, config))
    }

    /// Per-UTC-day data-quality report for the recorded QUOTE series `(venue, symbol)`.
    pub fn quote_series_quality(
        &self,
        venue: &str,
        symbol: &str,
        config: &DayQualityConfig,
    ) -> Result<Vec<DayQuality>, HistoryStoreError> {
        let rows = self.scan_quotes(venue, symbol, TimestampRange::all())?;
        Ok(score_quote_series(venue, symbol, &rows, config))
    }

    /// Per-UTC-day data-quality report for the recorded TRADE series `(venue, symbol)`.
    pub fn trade_series_quality(
        &self,
        venue: &str,
        symbol: &str,
        config: &DayQualityConfig,
    ) -> Result<Vec<DayQuality>, HistoryStoreError> {
        let rows = self.scan_trades(venue, symbol, TimestampRange::all())?;
        Ok(score_trade_series(venue, symbol, &rows, config))
    }
}

#[path = "series_quality_tests.rs"]
#[cfg(test)]
mod series_quality_tests;
