//! Binance's public OPTION-market archive (`data.binance.vision`'s `data/option/daily/`) as two archive
//! lanes, read by the datahub's import lane (`vike-cli data history import`) under `venue=binance`:
//!
//! - [`summary`], the vendor's `EOHSummary`: once an hour, every listed option of one underlying,
//!   with its best bid and ask, mark, implied volatility, greeks, volume and open interest. It lands in
//!   `kind=chain` ([`vike_data::ChainRow`]), the series a study reads point-in-time
//!   (`StudyContext::chain_as_of`), each row stamped at the LAST millisecond of the hour it summarises.
//! - [`volatility_index`], the vendor's `BVOLIndex`: Binance's implied-volatility index of one
//!   underlying, about one value a second. It lands in `kind=trade` as one synthetic tick per value,
//!   the shape `crates/bridges/deribit/src/feed/dvol.rs` stores Deribit's DVOL in (`size = 0`: an index
//!   observation, never a print).
//!
//! `crate::archive_import` runs the day; the datahub's format
//! (`crates/vike-datahub/src/import/formats/binance_options.rs`) checks each zip against its
//! `.CHECKSUM` before a byte of it is decoded. The zip reader is [`super::decode`]'s. The operator's
//! page is `docs/ops/binance-archive-import.md`.
//!
//! # What the vendor publishes (measured 2026-11-16)
//!
//! `EOHSummary`: five underlyings, daily zips from 2023-05-18 to 2023-10-23 and none after, about
//! 5,500 rows a day for BTCUSDT. `BVOLIndex`: two indices, daily zips from 2023-06-20 on, about 86,000
//! rows a day. So the summary is a closed half-year of history, and anything later is the datahub's
//! own chain recording (`flags.record_chains`) or another vendor's.
//!
//! # A dataset is the vendor's folder name
//!
//! The vendor's symbol, upper-case letters and digits (`BTCUSDT` for the summary, `BTCBVOLUSDT` for the
//! index), which is also the series' symbol: the summary's `underlying` column, the index's `symbol`
//! column. Every row must carry it, so another dataset's file cannot land in this series.
//!
//! # One owner per UTC day, and no bars
//!
//! The store dedups by commit key, never by row, and no other producer writes these series: there is
//! no live recorder of Binance's options. So each lane's day key owns its day, and any other key meets
//! EVERY day, the conservative direction ([`own_day_owners`]): such a day is `Overlapped`, refused, and
//! never stored twice. Neither lane derives bars (a chain has none, and bars of an index would be a
//! lane of their own under decision 0268): the format refuses `--bars`.

use vike_data::SeriesCoverage;
use vike_data::archive_day::ArchiveDayRefusal;
use vike_model::MS_PER_DAY;
use vike_store::HistoryStoreDataFusion;

use super::{PUBLICATION_MARGIN_MS, archive_dataset, day_of_label};
use crate::archive_import::{ArchiveLane, DayOwnerKey, DayOwnerRole, DayOwners};
use crate::error::CollectError;

pub mod summary;
pub mod volatility_index;

/// **One option-market dataset of the vendor's daily zips**: what the datahub's format asks of a
/// lane beyond [`ArchiveLane`]. Implemented by [`summary::OptionSummaryLane`] and
/// [`volatility_index::VolatilityIndexLane`], and nothing else.
pub trait OptionArchiveLane: ArchiveLane + Clone + Send + Sync {
    /// The format id, the folder under the imports root, and the head of the lane's day keys.
    const SOURCE: &'static str;
    /// The vendor's dataset word in a file name: `<dataset>-<word>-YYYY-MM-DD.zip`.
    const FILE_WORD: &'static str;
    /// The lane over `dataset`, or the whole-request refusal of a name that is not one.
    fn for_dataset(dataset: &str) -> Result<Self, String>;
    /// The rows of the zip `blob`, the file of the UTC day starting at `day`: refused, never guessed.
    fn decode(&self, blob: &[u8], day: i64) -> Result<Vec<Self::Row>, ArchiveDayRefusal>;
    /// The plan's admission line: what lands where, and what is dropped.
    fn admission(&self) -> String;
    /// The series' coverage and commit log, from ONE manifest parse: what a plan that writes nothing
    /// classifies its days against, without the day-owner lock.
    fn series_facts(
        &self,
        hist: &HistoryStoreDataFusion,
    ) -> Result<(SeriesCoverage, Vec<String>), CollectError> {
        Ok(hist.series_facts(&self.row_series())?)
    }
}

/// `dataset` as an option-market dataset of `source`: the vendor's symbol, upper-case letters and
/// digits. A perpetual's spelling (`BTCUSDT.P`) or anything else is refused.
fn option_dataset(source: &str, dataset: &str) -> Result<String, String> {
    match archive_dataset(dataset) {
        Some((vendor, false)) => Ok(vendor.to_string()),
        _ => Err(format!(
            "{source} reads one of the vendor's option-market folders, named as the vendor names it \
             (upper-case letters and digits, such as BTCUSDT or BTCBVOLUSDT): {dataset:?} is not one"
        )),
    }
}

/// The one-owner table of a series this lane alone writes: its own day keys (`own_head` then
/// `YYYY-MM-DD`) hold their days, and any other key meets every day.
fn own_day_owners(keys: &[String], own_head: &str) -> DayOwners {
    let owners = keys
        .iter()
        .map(|key| match key.strip_prefix(own_head).and_then(day_of_label) {
            Some(day) => DayOwnerKey {
                lane: DayOwnerRole::Archive,
                range: Some((day, day + MS_PER_DAY - 1)),
                key: key.clone(),
            },
            None => DayOwnerKey { lane: DayOwnerRole::Other, range: None, key: key.clone() },
        })
        .collect();
    DayOwners::new(PUBLICATION_MARGIN_MS, owners)
}

/// The sentence a day another key meets is refused with.
fn overlapped(source: &str, keys: &str) -> String {
    format!(
        "this series already holds rows on this day under keys {source} did not write ({keys}). \
         Importing the day could store them twice, so it is not imported. Nothing was read and no \
         commit key was spent."
    )
}

/// Why an option-market lane derives no bars, for every bar question [`ArchiveLane`] asks.
fn no_bars(source: &str) -> String {
    format!("{source} derives no bars: import it with no --bars, and resample in the study.")
}

/// The store's refusal of a bar write, which an option-market lane never asks for.
fn bars_refused(refused: &[(String, String)], rows: &str) -> ArchiveDayRefusal {
    let why = refused.iter().map(|(iv, e)| format!("{iv}: {e}")).collect::<Vec<_>>().join("; ");
    ArchiveDayRefusal::new("BarsRefused", format!("the store refused ({why}); {rows}."))
}

/// A numeric CSV field: empty (or the vendor's `""`) is `None`; anything else must be a finite
/// number (`0E-8` is the vendor's zero).
fn number(text: &str, field: &'static str) -> Result<Option<f64>, String> {
    if text.is_empty() || text == "\"\"" {
        return Ok(None);
    }
    match text.parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(Some(value)),
        _ => Err(format!("{field} is not a finite number")),
    }
}

/// A field that may not be negative: a price, a volume, an open interest.
fn not_negative(text: &str, field: &'static str) -> Result<Option<f64>, String> {
    match number(text, field)? {
        Some(value) if value < 0.0 => Err(format!("{field} is negative")),
        value => Ok(value),
    }
}

/// A quote that is absent when zero: the vendor writes `0E-8` for a side with no order, and an
/// absent quote stays absent, never `0.0` ([`vike_data::ChainRow`]'s rule).
fn quote(text: &str, field: &'static str) -> Result<Option<f64>, String> {
    Ok(not_negative(text, field)?.filter(|value| *value > 0.0))
}

#[cfg(test)]
#[path = "options/options_tests.rs"]
mod options_tests;
