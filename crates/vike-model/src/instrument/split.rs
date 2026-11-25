//! **A share's SPLIT, as a fact about one stored price series** (`docs/decisions/0289-a-backtest-split-adjusts-a-share-series-from-the-operators-split-rows.md`).
//!
//! The operator enters each one (`vike-cli data splits add`), the settings database keeps it (`vike_secrets::stock_split`),
//! the datahub serves the rows with its kept instrument lists, and a backtest reading the series divides every price
//! before the split by its ratio and multiplies the volume by it, as TradingView shows a share's history. Nothing here
//! is compiled in: a split is DATA, typed in at run time.
//!
//! The row names the series as the store names it (`dukascopy AAPL.CFD`, the instrument's user name of decision 0290:
//! what `vike-cli data history ls` prints and a backtest's profile names), not the venue's instrument name (`AAPL.US/USD`): the price series is what is adjusted, and
//! keying it so needs no name mapping.

use serde::{Deserialize, Serialize};
use vike_utility::calendar::days_from_civil;

use crate::time::{MS_PER_DAY, parse_ymd};

/// One split of one stored series.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StockSplit {
    /// The venue's roster slug (`dukascopy`).
    pub venue: String,
    /// The series' symbol as the store names it (`AAPL.CFD`).
    pub symbol: String,
    /// The first day the series trades at the new price, `YYYY-MM-DD`; it takes effect at 00:00 UTC of that day.
    pub date: String,
    /// New shares per old share: `4` for a 4-for-1 split, `0.1` for a 1-for-10 reverse split.
    pub ratio: f64,
}

impl StockSplit {
    /// The row an operator typed, its date written canonically (`2020-8-31` is `2020-08-31`), or why it is no split
    /// ([`Self::effective_ms`]).
    ///
    /// # Errors
    /// The refusal in words.
    pub fn new(venue: &str, symbol: &str, date: &str, ratio: f64) -> Result<Self, String> {
        let (y, m, d) = parse_ymd(date)?;
        let split = Self {
            venue: venue.trim().to_string(),
            symbol: symbol.trim().to_string(),
            date: format!("{y:04}-{m:02}-{d:02}"),
            ratio,
        };
        split.effective_ms().map(|_| split)
    }

    /// **The instant the split takes effect, 00:00 UTC of [`Self::date`], epoch ms**, or why the row is no split: a
    /// blank venue or symbol, a date that is no calendar day, or a ratio that is not a finite number above zero other
    /// than one. Every reader judges a row here, so a row read back from a store is judged as it was typed.
    ///
    /// # Errors
    /// The refusal in words.
    pub fn effective_ms(&self) -> Result<i64, String> {
        if self.venue.trim().is_empty() || self.symbol.trim().is_empty() {
            return Err("a split names its venue and the series' symbol".to_string());
        }
        if !(self.ratio.is_finite() && self.ratio > 0.0) || self.ratio == 1.0 {
            return Err(format!(
                "the ratio {} is no split: new shares per old share, above zero and not 1 (4 for a 4-for-1, \
                 0.1 for a 1-for-10)",
                self.ratio
            ));
        }
        let (y, m, d) = parse_ymd(&self.date)?;
        Ok(days_from_civil(y, m, d) * MS_PER_DAY)
    }
}

#[path = "split_tests.rs"]
#[cfg(test)]
mod split_tests;
