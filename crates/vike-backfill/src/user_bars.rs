//! A user's file of bars as an archive lane: the `csv-bars` import's store half (decision 0268, point
//! 3). Its provenance is not one vike can know, so its rows land ONLY in the user's lane
//! (`vike_data::store::store_kind::user_lane`, `kind=bar/venue=…/source=user-<label>/…`), never in the
//! venue's own series, and only a read that names the lane meets them.
//!
//! `crate::archive_import` runs the day, as for every archive format. What this file adds: the lane's
//! series and day key, the one-owner table inside the lane, and the check of a decoded file against the
//! operator's interval. The decode is the one CSV decoder's, the backtest's file read's too:
//! `vike_data::csv_bars::decode_bars`, or `decode_bars_sparse` (every rule but the one interval it would
//! read off the rows) for a SPARSE lane.
//!
//! - **One owner per UTC day inside the lane**: this lane's day key holds a day, and any other key the
//!   lane's series holds meets every day, the conservative direction. A second file over a day an
//!   earlier one imported is not stored twice; to replace a lane, remove it and import again.
//! - **The interval is the operator's word, and the file must agree** (decision 0271's amendment of
//!   2026-11-18): every bar opens on the interval's UTC grid, each strictly after the one before. A file
//!   is DENSE unless the operator says `--sparse` ([`ArchiveLaneUserBars::new`]'s `sparse`): its step,
//!   the commonest distance between its rows, is then the interval's, so 5-minute bars at `--interval
//!   1m` are refused naming both ways out. A sparse file holds a row only at the bars that carry a value,
//!   any number of the interval's bars apart, and no step is read off it.
//! - **A file whose every row lies on a COARSER grid is refused in both modes** ([`coarse_grid`]): more
//!   than [`COARSE_GRID_ROWS`] rows all on the 5-minute grid are 5-minute data, whatever the operator
//!   said, because a bar read at a shorter interval closes before it did and a backtest would see it
//!   early. A dense file never reaches it (two rows one interval apart lie on no coarser grid); it is the
//!   check a sparse file has instead of the step. A sparse file of events at round times (releases at
//!   :00 and :30) passes it when the operator says `--allow-coarse-grid` too; that flag skips nothing
//!   else, and nothing at all without `--sparse` (the wire's door refuses the pair split).
//! - **No publication margin**: a user's file is published when it is placed, but a day that has not
//!   ended is still refused as too recent, so a partial day never spends its key.

use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal, CSV_BARS_FORMAT};
use vike_data::csv_bars::{
    CsvBarsColumns, CsvBarsFormat, CsvBarsRule, decode_bars, decode_bars_sparse,
};
use vike_data::store::store_kind::user_lane;
use vike_data::{SeriesId, TimestampRange};
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use crate::archive_import::{ArchiveDayAppend, ArchiveLane, DayOwnerKey, DayOwnerRole, DayOwners};
use crate::error::CollectError;

/// A day's key in `lane`, spent by its bars: `day` is `YYYY-MM-DD`.
pub fn day_commit_key(lane: &str, symbol: &str, interval: &str, day: &str) -> String {
    format!("csv-bars:{lane}:{symbol}:{interval}:{day}")
}

/// `YYYY-MM-DD` as the epoch-ms start of that UTC day; `None` for anything else.
fn day_of_label(label: &str) -> Option<i64> {
    let (y, m, d) = vike_model::parse_ymd(label).ok().filter(|_| label.len() == 10)?;
    Some(vike_utility::calendar::days_from_civil(y, m, d) * MS_PER_DAY)
}

/// The most rows a file may hold and still lie wholly on a coarser grid ([`coarse_grid`]): a few
/// events may all fall on round minutes, more cannot by chance. Eleven rows of a sparse one-minute file
/// all on the 5-minute grid by accident is a chance of 5^-11, about 2 in 100 million.
pub const COARSE_GRID_ROWS: usize = 10;

/// The coarser grids [`coarse_grid`] tries, coarsest first: each interval, and how a refusal names it.
const COARSE_GRIDS: [(&str, &str); 6] = [
    ("1d", "1-day"),
    ("4h", "4-hour"),
    ("1h", "1-hour"),
    ("30m", "30-minute"),
    ("15m", "15-minute"),
    ("5m", "5-minute"),
];

/// **The coarsest of `COARSE_GRIDS` every row of `bars` lies on**, when it is coarser than the
/// lane's `step_ms` (and a whole number of it) and there are more than [`COARSE_GRID_ROWS`] rows: that
/// grid's interval and its name in a refusal. `None` for a file any row of which is off every one.
pub fn coarse_grid(bars: &[Bar], step_ms: i64) -> Option<(&'static str, &'static str)> {
    if bars.len() <= COARSE_GRID_ROWS {
        return None;
    }
    COARSE_GRIDS.into_iter().find(|(interval, _)| {
        vike_marketdata::interval::canonical_bar_step_ms(interval).is_some_and(|grid| {
            grid > step_ms
                && grid % step_ms == 0
                && bars.iter().all(|b| b.timestamp.rem_euclid(grid) == 0)
        })
    })
}

/// A distance in ms in the interval words a refusal names it by: `5m`, `1h`, `1d`, `30s`, else `N ms`.
fn step_word(ms: i64) -> String {
    let units = [(MS_PER_DAY, "d"), (3_600_000, "h"), (60_000, "m"), (1_000, "s")];
    match units.into_iter().find(|(unit, _)| ms % unit == 0) {
        Some((unit, word)) => format!("{}{word}", ms / unit),
        None => format!("{ms} ms"),
    }
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
    /// `--sparse`: the file holds a row only at the bars that carry a value (the module doc).
    sparse: bool,
    /// `--allow-coarse-grid`: beside `sparse`, the coarse-grid refusal is not made (the module doc).
    allow_coarse_grid: bool,
}

impl ArchiveLaneUserBars {
    /// The lane `label` names for `venue`'s `symbol` at `interval`, reading files written in
    /// `format` whose fields `columns` finds, dense or, when `sparse`, a row only at the bars that carry a
    /// value, and, when `sparse` and `allow_coarse_grid` both, events whose rows may all lie on a coarser
    /// grid (`allow_coarse_grid` alone changes nothing: the wire's door refuses it). Refused: a label the
    /// user lane's grammar refuses, and an interval a day's import cannot hold whole (no fixed step, a
    /// second spelling, or a step that does not divide a UTC day:
    /// `vike_marketdata::interval::measures_bar_step` is the first of the three).
    pub fn new(
        venue: &str,
        symbol: &str,
        interval: &str,
        label: &str,
        format: CsvBarsFormat,
        columns: CsvBarsColumns,
        sparse: bool,
        allow_coarse_grid: bool,
    ) -> Result<Self, String> {
        let lane = user_lane(label)?;
        let step = vike_marketdata::interval::canonical_bar_step_ms(interval)
            .filter(|_| vike_marketdata::interval::measures_bar_step(interval))
            .filter(|step| MS_PER_DAY % step == 0);
        let Some(step_ms) = step else {
            return Err(format!(
                "{CSV_BARS_FORMAT} stores one UTC day at a time, so its interval is a canonical step that \
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
            sparse,
            allow_coarse_grid,
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

    /// Decode a whole file with the one CSV decoder, then hold it to this lane's interval (the module
    /// doc): its bars, ascending, or the sentence that refuses the file (the line and the rule). A dense
    /// file's step is the interval's; a sparse one's is not read (`decode_bars_sparse`).
    pub fn decode(&self, text: &str) -> Result<Vec<Bar>, String> {
        let bars = if self.sparse {
            decode_bars_sparse(text, &self.columns, &self.format).map_err(|r| r.to_string())?
        } else {
            let file =
                decode_bars(text, &self.columns, &self.format).map_err(|r| match r.rule {
                    CsvBarsRule::Interval => {
                        format!("{r}; if this file holds only events, add --sparse")
                    }
                    _ => r.to_string(),
                })?;
            if let Some(step) = file.step_ms
                && step != self.step_ms
            {
                return Err(format!(
                    "rows are {} apart but --interval is {}; if this file holds only events, add \
                     --sparse",
                    step_word(step),
                    self.interval
                ));
            }
            file.bars
        };
        if let Some(off) = bars.iter().find(|b| b.timestamp.rem_euclid(self.step_ms) != 0) {
            return Err(format!(
                "the bar at {} opens off the UTC {} grid, so which bucket it is is not in the file",
                off.timestamp, self.interval
            ));
        }
        // `--allow-coarse-grid` counts only beside `--sparse`: a dense file keeps its step.
        if !(self.sparse && self.allow_coarse_grid)
            && let Some((interval, grid)) = coarse_grid(&bars, self.step_ms)
        {
            return Err(format!(
                "all {} rows lie on the {grid} grid: is this {grid} data? use --interval {interval}, \
                 or add --allow-coarse-grid if these are events at round times",
                bars.len()
            ));
        }
        Ok(bars)
    }

    fn key(&self, day: i64) -> String {
        day_commit_key(&self.lane, &self.symbol, &self.interval, &epoch_ms_to_utc_date(day))
    }
}

impl ArchiveLane for ArchiveLaneUserBars {
    type Row = Bar;

    fn source(&self) -> &str {
        CSV_BARS_FORMAT
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
        format!(
            "{CSV_BARS_FORMAT} derives no bars: its rows ARE the bars, at the interval it was given."
        )
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
        Err(CollectError::Refused(self.undividable_interval_remedy().into()))
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
