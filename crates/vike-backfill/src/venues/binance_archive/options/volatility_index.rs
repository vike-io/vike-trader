//! Binance's `BVOLIndex` as an archive lane: one daily zip per index, about one value a second, into
//! `kind=trade` under `venue=binance`, `symbol=<the index's symbol>` (`BTCBVOLUSDT`).
//!
//! # The row
//!
//! `calc_time,symbol,base_asset,quote_asset,index_value`: `calc_time` in epoch milliseconds inside
//! the file's UTC day, `symbol` the dataset, `index_value` the index in volatility points (50.46 is
//! 50.46%), as published.
//!
//! # Why a trade row
//!
//! The store's only `(ts, price)` lane is `kind=trade`, and Deribit's DVOL index is stored there the
//! same way (`crates/bridges/deribit/src/feed/dvol.rs`'s `dvol_trade_tick`): `price` is the index,
//! `size` is `0` (an observation, not a print), `local_timestamp` is `calc_time` and `is_buyer_maker`
//! is `false`. A consumer that reads every trade row as a matched execution is wrong for this symbol,
//! as `vike_data::store::store_kind::STORE_KINDS`' `trade` row says. No bar is derived: a study
//! resamples the index itself.
//!
//! # Order
//!
//! Times must run forward. A row repeating the time and value of the row before is dropped; one
//! repeating its time with another value refuses the day (`ContradictoryRepeat`); one going back in
//! time refuses it too (`NotIncreasing`). A missing second is not a gap anything here can fill: the
//! vendor's file for 2023-10-23 holds 85,700 of the day's 86,400 seconds.

use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal, BINANCE_VOLATILITY_INDEX_FORMAT};
use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_marketdata::TickTrade;
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use super::super::VENUE;
use super::super::dataset_member_name;
use super::super::decode::{read_member_lines, refused};
use super::{OptionArchiveLane, not_negative, option_dataset, own_day_owners};
use crate::archive_import::{ArchiveDayAppend, ArchiveLane, DayOwnerRole, DayOwners};
use crate::error::CollectError;

/// The vendor's dataset word in a file name: `BTCBVOLUSDT-BVOLIndex-2023-10-23.zip`.
pub const FILE_WORD: &str = "BVOLIndex";

/// The header line every file measured opens with.
const HEADER_LINE: &str = "calc_time,symbol,base_asset,quote_asset,index_value";

/// The longest row this format can hold, generously: five short fields.
const MAX_LINE_BYTES: u64 = 256;

/// The most rows one day may decode to: one a second is 86,400, and this bounds a file that is not.
const MAX_ROWS: usize = 1_000_000;

/// A day's key, spent by its rows: `day` is `YYYY-MM-DD`.
pub fn day_commit_key(symbol: &str, day: &str) -> String {
    format!("binance-bvolindex:{symbol}:{day}")
}

/// The series `symbol`'s index values land in.
pub fn index_series(symbol: &str) -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Trade, VENUE, symbol, None)
}

/// One row, parsed: its time and value.
fn parse_row(line: &str, symbol: &str) -> Result<(i64, f64), String> {
    let f: Vec<&str> = line.split(',').collect();
    if f.len() != 5 {
        return Err(format!("it has {} fields, not 5", f.len()));
    }
    let time = f[0].parse::<i64>().map_err(|_| "calc_time is not an integer".to_string())?;
    if f[1] != symbol {
        return Err(format!("symbol {:?} is not this dataset's {symbol}", f[1]));
    }
    let value = not_negative(f[4], "index_value")?.ok_or("index_value is empty")?;
    Ok((time, value))
}

/// Decode the zip `blob`, the index `symbol`'s file for the UTC day starting at `day`: one synthetic
/// tick per value, in time order. See the module doc for every rule.
pub fn decode_index_zip(
    blob: &[u8],
    symbol: &str,
    day: i64,
) -> Result<Vec<TickTrade>, ArchiveDayRefusal> {
    let member = dataset_member_name(symbol, FILE_WORD, day);
    let end = day + MS_PER_DAY - 1;
    let mut ticks: Vec<TickTrade> = Vec::new();
    read_member_lines(blob, &member, MAX_LINE_BYTES, |number, line| {
        if number == 1 && line == HEADER_LINE {
            return Ok(());
        }
        let at = |class: &str, what: String| {
            refused(
                class,
                format!("line {number} of `{member}` {what}, so the whole day is refused."),
            )
        };
        let (time, value) = parse_row(line, symbol).map_err(|why| {
            at(
                "BadRow",
                format!("does not parse: {why}. A row that does not parse is never skipped"),
            )
        })?;
        if !(day..=end).contains(&time) {
            return Err(at(
                "OutsideDay",
                format!(
                    "has a calc_time outside the file's UTC day [{day}, {end}] in epoch milliseconds"
                ),
            ));
        }
        match ticks.last() {
            Some(last) if last.timestamp == time && last.price == value => return Ok(()),
            Some(last) if last.timestamp == time => {
                return Err(at(
                    "ContradictoryRepeat",
                    "repeats the time of the row above it with another value".to_string(),
                ));
            }
            Some(last) if last.timestamp > time => {
                return Err(at("NotIncreasing", "goes back in time".to_string()));
            }
            _ => {}
        }
        if ticks.len() == MAX_ROWS {
            return Err(at(
                "DecodedTooLarge",
                format!("is past the {MAX_ROWS} rows one day may hold"),
            ));
        }
        ticks.push(TickTrade {
            timestamp: time,
            local_timestamp: time,
            price: value,
            size: 0.0,
            is_buyer_maker: false,
            symbol: Default::default(),
        });
        Ok(())
    })?;
    Ok(ticks)
}

/// Binance's volatility index of one underlying, as synthetic ticks under `venue=binance`.
#[derive(Debug, Clone)]
pub struct VolatilityIndexLane {
    symbol: String,
}

impl VolatilityIndexLane {
    fn key(&self, day: i64) -> String {
        day_commit_key(&self.symbol, &epoch_ms_to_utc_date(day))
    }
}

impl OptionArchiveLane for VolatilityIndexLane {
    const SOURCE: &'static str = BINANCE_VOLATILITY_INDEX_FORMAT;
    const FILE_WORD: &'static str = FILE_WORD;

    fn for_dataset(dataset: &str) -> Result<Self, String> {
        Ok(Self { symbol: option_dataset(BINANCE_VOLATILITY_INDEX_FORMAT, dataset)? })
    }

    fn decode(&self, blob: &[u8], day: i64) -> Result<Vec<TickTrade>, ArchiveDayRefusal> {
        decode_index_zip(blob, &self.symbol, day)
    }

    fn admission(&self) -> String {
        let s = &self.symbol;
        format!(
            "the vendor's implied-volatility index {s}, in volatility points, as published · stored as \
             {VENUE} {s} trades, one synthetic tick per value (price = the index, size = 0: an index, \
             never a print), the way Deribit's DVOL is stored · every zip is checked against its \
             .CHECKSUM (SHA-256) before it is read · daily zips only · no bars"
        )
    }
}

impl ArchiveLane for VolatilityIndexLane {
    type Row = TickTrade;

    fn source(&self) -> &str {
        BINANCE_VOLATILITY_INDEX_FORMAT
    }

    fn symbol(&self) -> &str {
        &self.symbol
    }

    fn row_series(&self) -> SeriesId {
        index_series(&self.symbol)
    }

    fn row_timestamp(row: &TickTrade) -> i64 {
        row.timestamp
    }

    fn owners(&self, keys: &[String]) -> DayOwners {
        own_day_owners(keys, &day_commit_key(&self.symbol, ""))
    }

    fn overlapped_detail(&self, keys: &str) -> String {
        super::overlapped(BINANCE_VOLATILITY_INDEX_FORMAT, keys)
    }

    fn append_day(
        &self,
        hist: &HistoryStoreDataFusion,
        rows: &[TickTrade],
        day: i64,
        _end: i64,
        _bars: &[String],
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError> {
        let key = self.key(day);
        let rows = hist.append_trades(VENUE, &self.symbol, rows, Some(&key))?;
        let spent = vec![(DayOwnerRole::Archive, key.clone())];
        Ok(Ok(ArchiveDayAppend { rows, key, spent }))
    }

    fn undividable_interval_remedy(&self) -> String {
        super::no_bars(BINANCE_VOLATILITY_INDEX_FORMAT)
    }

    fn bar_series(&self, _interval: &str) -> SeriesId {
        self.row_series()
    }

    fn bar_commit_key(&self, _interval: &str, range: TimestampRange) -> String {
        self.key(range.start.unwrap_or(0))
    }

    /// Never reached: the format opens every import with no bar interval.
    fn derive_bars(
        &self,
        _hist: &HistoryStoreDataFusion,
        _interval: &str,
        _range: TimestampRange,
    ) -> Result<usize, CollectError> {
        Err(CollectError::Refused(super::no_bars(BINANCE_VOLATILITY_INDEX_FORMAT).into()))
    }

    fn bars_refused(
        &self,
        refused: &[(String, String)],
        _written: &[ArchiveDayBars],
        rows: &str,
    ) -> ArchiveDayRefusal {
        super::bars_refused(refused, rows)
    }
}

#[cfg(test)]
#[path = "volatility_index_tests.rs"]
mod volatility_index_tests;
