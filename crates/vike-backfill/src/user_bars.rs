//! A user's file of bars as an archive lane: the `csv-bars` import's store half (decision 0268, point
//! 3). Its provenance is not one vike can know, so its rows land ONLY in the user's lane
//! (`vike_data::store::store_kind::user_lane`, `kind=bar/venue=…/source=user-<label>/…`), never in the
//! venue's own series, and only a read that names the lane meets them.
//!
//! `crate::archive_import` runs the day, as for every archive format. What this file adds: the lane's
//! series and day key, the one-owner table inside the lane, and the check of a decoded file against the
//! operator's interval. The decode is `vike_data::csv_bars::decode_bars`'s, the one CSV decoder the
//! backtest's file read shares, so a file imports exactly when it can be read.
//!
//! - **One owner per UTC day inside the lane**: this lane's day key holds a day, and any other key the
//!   lane's series holds meets every day, the conservative direction. A second file over a day an
//!   earlier one imported is not stored twice; to replace a lane, remove it and import again.
//! - **The interval is the operator's word, and the file must agree**: its step is the interval's, and
//!   every bar opens on the interval's UTC grid. A file of one bar has no step and is held to the grid
//!   alone.
//! - **No publication margin**: a user's file is published when it is placed, but a day that has not
//!   ended is still refused as too recent, so a partial day never spends its key.

use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal};
use vike_data::csv_bars::{CsvBarsColumns, CsvBarsFormat, decode_bars};
use vike_data::store::store_kind::user_lane;
use vike_data::{SeriesId, StoreKindId, TimestampRange};
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use crate::archive_import::{ArchiveDayAppend, ArchiveLane, DayOwnerKey, DayOwnerRole, DayOwners};
use crate::error::CollectError;

/// The format's id, and the word that opens this lane's refusals and log lines.
pub const SOURCE: &str = "csv-bars";

/// A day's key in `lane`, spent by its bars: `day` is `YYYY-MM-DD`.
pub fn day_commit_key(lane: &str, symbol: &str, interval: &str, day: &str) -> String {
    format!("csv-bars:{lane}:{symbol}:{interval}:{day}")
}

/// `YYYY-MM-DD` as the epoch-ms start of that UTC day; `None` for anything else.
fn day_of_label(label: &str) -> Option<i64> {
    let (y, m, d) = vike_model::parse_ymd(label).ok().filter(|_| label.len() == 10)?;
    Some(vike_utility::calendar::days_from_civil(y, m, d) * MS_PER_DAY)
}

/// A user's bars of one instrument at one interval, in one lane.
#[derive(Debug, Clone)]
pub struct ArchiveLaneUserBars {
    venue: String,
    symbol: String,
    interval: String,
    lane: String,
    step_ms: i64,
    /// How the file is written: the decoder's plain form unless the operator said otherwise.
    format: CsvBarsFormat,
    /// Which column is which field: found by the header's names unless the operator mapped them.
    columns: CsvBarsColumns,
}

impl ArchiveLaneUserBars {
    /// The lane `label` names for `venue`'s `symbol` at `interval`, reading files written in
    /// `format` whose fields `columns` finds. Refused: a label the user lane's grammar refuses, and an
    /// interval a day's import cannot hold whole (no fixed step, a second spelling, or a step that does
    /// not divide a UTC day: `vike_marketdata::interval::measures_bar_step` is the first of the three).
    pub fn new(
        venue: &str,
        symbol: &str,
        interval: &str,
        label: &str,
        format: CsvBarsFormat,
        columns: CsvBarsColumns,
    ) -> Result<Self, String> {
        let lane = user_lane(label)?;
        let step = vike_marketdata::interval::canonical_bar_step_ms(interval)
            .filter(|_| vike_marketdata::interval::measures_bar_step(interval))
            .filter(|step| MS_PER_DAY % step == 0);
        let Some(step_ms) = step else {
            return Err(format!(
                "{SOURCE} stores one UTC day at a time, so its interval is a canonical step that \
                 divides a day (1m, 5m, 1h, 1d …); {interval:?} is not one."
            ));
        };
        Ok(Self {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            lane,
            step_ms,
            format,
            columns,
        })
    }

    /// The lane's name, `user-<label>`.
    pub fn lane(&self) -> &str {
        &self.lane
    }

    /// The series the lane's bars land in.
    pub fn series(&self) -> SeriesId {
        SeriesId::per_symbol(
            StoreKindId::Bar,
            &self.venue,
            &self.symbol,
            Some(self.interval.clone()),
        )
        .with_source(self.lane.clone())
    }

    /// Decode a whole file with the one CSV decoder, then hold it to this lane's interval: its bars,
    /// ascending, or the sentence that refuses the file (the line and the rule).
    pub fn decode(&self, text: &str) -> Result<Vec<Bar>, String> {
        let file = decode_bars(text, &self.columns, &self.format).map_err(|r| r.to_string())?;
        if let Some(step) = file.step_ms
            && step != self.step_ms
        {
            return Err(format!(
                "the file's bars are {step} ms apart and --interval {} is {} ms: import it at the \
                 file's own interval",
                self.interval, self.step_ms
            ));
        }
        if let Some(off) = file.bars.iter().find(|b| b.timestamp.rem_euclid(self.step_ms) != 0) {
            return Err(format!(
                "the bar at {} opens off the UTC {} grid, so which bucket it is is not in the file",
                off.timestamp, self.interval
            ));
        }
        Ok(file.bars)
    }

    fn key(&self, day: i64) -> String {
        day_commit_key(&self.lane, &self.symbol, &self.interval, &epoch_ms_to_utc_date(day))
    }
}

impl ArchiveLane for ArchiveLaneUserBars {
    type Row = Bar;

    fn source(&self) -> &str {
        SOURCE
    }

    fn symbol(&self) -> &str {
        &self.symbol
    }

    fn row_series(&self) -> SeriesId {
        self.series()
    }

    fn row_timestamp(row: &Bar) -> i64 {
        row.timestamp
    }

    /// See the module doc: this lane's day keys hold their days, and any other key meets every day.
    fn owners(&self, keys: &[String]) -> DayOwners {
        let own_head = day_commit_key(&self.lane, &self.symbol, &self.interval, "");
        let owners = keys
            .iter()
            .map(|key| match key.strip_prefix(&own_head).and_then(day_of_label) {
                Some(day) => DayOwnerKey {
                    lane: DayOwnerRole::Archive,
                    range: Some((day, day + MS_PER_DAY - 1)),
                    key: key.clone(),
                },
                None => DayOwnerKey { lane: DayOwnerRole::Other, range: None, key: key.clone() },
            })
            .collect();
        DayOwners::new(0, owners)
    }

    fn overlapped_detail(&self, keys: &str) -> String {
        format!(
            "the lane {} already holds bars under other keys ({keys}), and a key it cannot read may \
             hold this day. Importing it would store bars twice, so it is not imported. Remove the \
             lane and import again to replace it. Nothing was read and no commit key was spent.",
            self.lane
        )
    }

    fn append_day(
        &self,
        hist: &HistoryStoreDataFusion,
        rows: &[Bar],
        day: i64,
        _end: i64,
        _bars: &[String],
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError> {
        let key = self.key(day);
        let (venue, lane, symbol, interval) =
            (&self.venue, &self.lane, &self.symbol, &self.interval);
        let rows = hist.append_bars_sourced(venue, lane, symbol, interval, rows, Some(&key))?;
        let spent = vec![(DayOwnerRole::Archive, key.clone())];
        Ok(Ok(ArchiveDayAppend { rows, key, spent }))
    }

    fn undividable_interval_remedy(&self) -> String {
        format!("{SOURCE} derives no bars: its rows ARE the bars, at the interval it was given.")
    }

    fn bar_series(&self, _interval: &str) -> SeriesId {
        self.series()
    }

    fn bar_commit_key(&self, _interval: &str, range: TimestampRange) -> String {
        self.key(range.start.unwrap_or(0))
    }

    /// Never reached: the format opens its import with no bar interval to derive.
    fn derive_bars(
        &self,
        _hist: &HistoryStoreDataFusion,
        _interval: &str,
        _range: TimestampRange,
    ) -> Result<usize, CollectError> {
        Err(CollectError::Refused(self.undividable_interval_remedy()))
    }

    fn bars_refused(
        &self,
        refused: &[(String, String)],
        _written: &[ArchiveDayBars],
        rows: &str,
    ) -> ArchiveDayRefusal {
        let why = refused.iter().map(|(iv, e)| format!("{iv}: {e}")).collect::<Vec<_>>().join("; ");
        ArchiveDayRefusal::new("BarsRefused", format!("the store refused ({why}); {rows}."))
    }
}

#[path = "user_bars_tests.rs"]
#[cfg(test)]
mod user_bars_tests;
