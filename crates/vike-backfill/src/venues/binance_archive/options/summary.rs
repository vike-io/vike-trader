//! Binance's `EOHSummary` ("end of hour" summary) as an archive lane: one daily zip per underlying,
//! every listed option once an hour, into `kind=chain` under `venue=binance`, `symbol=<underlying>`.
//!
//! # The row, column by column
//!
//! `date,hour,symbol,underlying,type,strike,open,high,low,close,volume_contracts,volume_usdt,`
//! `best_bid_price,best_ask_price,best_bid_qty,best_ask_qty,best_buy_iv,best_sell_iv,mark_price,`
//! `mark_iv,delta,gamma,vega,theta,openinterest_contracts,openinterest_usdt`, prices in USDT.
//!
//! - **`ts` is the LAST millisecond of the hour the row summarises** (`hour` 00 is stamped
//!   00:59:59.999). The summary describes the hour that ended, so a study deciding at any time `t`
//!   that reads rows with `ts <= t` never sees an hour that had not run; the one millisecond before
//!   the hour's end keeps hour 23 inside its own UTC day, the archive day the file is keyed by.
//! - `instrument` is `symbol` verbatim (`BTC-231027-33000-C`); `underlying` must be the dataset;
//!   `kind` is `type` (`C`/`P`); `strike` and the expiry date come from `strike` (`231027-33000`),
//!   which must agree with the instrument's name, and `expiry_ms` is 08:00 UTC of that date, when
//!   Binance's European options expire.
//! - `bid`, `ask`, `mark` are `best_bid_price`, `best_ask_price`, `mark_price`, ABSENT when the vendor
//!   writes zero (`0E-8` is its spelling of a side with no order); `iv` is `mark_iv`, already a
//!   fraction (0.62 = 62%), absent when zero.
//! - `delta`, `gamma`, `vega`, `theta` are the VENUE's own greeks, as published: theta per day and
//!   vega per volatility point, the convention `vike_data::ChainRow` states. They are not recomputed,
//!   because the summary carries no underlying price to recompute them from.
//! - `volume` is `volume_contracts` and `open_interest` is `openinterest_contracts`, both in
//!   contracts.
//! - **Dropped, because `vike_data::ChainRow` has no column for them:** the hour's trade prices
//!   (`open`..`close`), the USDT volume and open interest, the best quantities, and the best-quote
//!   implied volatilities (`best_buy_iv` is empty in every file measured).
//!
//! The vendor's rows are not in time order, so the day's rows are sorted by `(ts, instrument)`. A
//! row repeating an earlier one exactly is dropped; one repeating an option's hour with any field
//! different refuses the day (`ContradictoryRepeat`). A row that does not parse refuses the whole
//! day (`BadRow`): a row is never skipped.

use std::collections::HashMap;

use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal, BINANCE_OPTION_SUMMARY_FORMAT};
use vike_data::{ChainRow, HistoryStore, SeriesId, TimestampRange};
use vike_marketdata::OptionKind;
use vike_model::MS_PER_HOUR;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use super::super::decode::{read_member_lines, refused};
use super::super::{VENUE, dataset_member_name, day_of_label};
use super::{OptionArchiveLane, not_negative, number, option_dataset, own_day_owners, quote};
use crate::archive_import::{ArchiveDayAppend, ArchiveLane, DayOwnerRole, DayOwners};
use crate::error::CollectError;

/// The vendor's dataset word in a file name: `BTCUSDT-EOHSummary-2023-10-23.zip`.
pub const FILE_WORD: &str = "EOHSummary";

/// The header line every file measured opens with.
const HEADER_LINE: &str = "date,hour,symbol,underlying,type,strike,open,high,low,close,\
                           volume_contracts,volume_usdt,best_bid_price,best_ask_price,best_bid_qty,\
                           best_ask_qty,best_buy_iv,best_sell_iv,mark_price,mark_iv,delta,gamma,\
                           vega,theta,openinterest_contracts,openinterest_usdt";

/// The fields of a row.
const FIELDS: usize = 26;

/// The longest row this format can hold, generously: the longest measured is 303 bytes.
const MAX_LINE_BYTES: u64 = 1024;

/// The most rows one day may decode to: a whole chain once an hour is about 5,500 for BTCUSDT.
const MAX_ROWS: usize = 1_000_000;

/// Binance's European options expire at 08:00 UTC of the date in their name.
const EXPIRY_TIME_OF_DAY_MS: i64 = 8 * MS_PER_HOUR;

/// A day's key, spent by its rows: `day` is `YYYY-MM-DD`.
pub fn day_commit_key(underlying: &str, day: &str) -> String {
    format!("binance-eohsummary:{underlying}:{day}")
}

/// The series `underlying`'s chain rows land in.
pub fn chain_series(underlying: &str) -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Chain, VENUE, underlying, None)
}

/// One row, parsed: the option's chain row (its `ts` the hour's last millisecond).
fn parse_row(line: &str, underlying: &str, day: i64, label: &str) -> Result<ChainRow, String> {
    let f: Vec<&str> = line.split(',').collect();
    if f.len() != FIELDS {
        return Err(format!("it has {} fields, not {FIELDS}", f.len()));
    }
    if f[0] != label {
        return Err(format!("its date {:?} is not the file's day {label}", f[0]));
    }
    let hour = match f[1].as_bytes() {
        [a, b] if a.is_ascii_digit() && b.is_ascii_digit() => {
            i64::from((a - b'0') * 10 + (b - b'0'))
        }
        _ => return Err("hour is not two digits".to_string()),
    };
    if hour > 23 {
        return Err("hour is past 23".to_string());
    }
    let instrument = f[2];
    if f[3] != underlying {
        return Err(format!("underlying {:?} is not this dataset's {underlying}", f[3]));
    }
    let kind = OptionKind::from_cp(f[4]).ok_or("type is neither C nor P")?;
    let (date, strike_text) = f[5].split_once('-').ok_or("strike is not YYMMDD-STRIKE")?;
    let suffix = format!("-{}-{}", f[5], f[4]);
    if instrument.len() <= suffix.len() || !instrument.ends_with(&suffix) {
        return Err(format!(
            "symbol {instrument:?} does not end in its strike and type ({suffix})"
        ));
    }
    let six_digits = date.len() == 6 && date.bytes().all(|b| b.is_ascii_digit());
    let expiry_day = six_digits
        .then(|| day_of_label(&format!("20{}-{}-{}", &date[..2], &date[2..4], &date[4..])))
        .flatten()
        .ok_or("the strike's date is not a real YYMMDD")?;
    let strike = number(strike_text, "the strike")?
        .filter(|s| *s > 0.0)
        .ok_or("the strike is not positive")?;
    Ok(ChainRow {
        ts: day + (hour + 1) * MS_PER_HOUR - 1,
        underlying: underlying.to_string(),
        instrument: instrument.to_string(),
        expiry_ms: expiry_day + EXPIRY_TIME_OF_DAY_MS,
        strike,
        kind,
        bid: quote(f[12], "best_bid_price")?,
        ask: quote(f[13], "best_ask_price")?,
        mark: quote(f[18], "mark_price")?,
        iv: quote(f[19], "mark_iv")?,
        open_interest: not_negative(f[24], "openinterest_contracts")?,
        volume: not_negative(f[10], "volume_contracts")?,
        delta: number(f[20], "delta")?,
        gamma: number(f[21], "gamma")?,
        theta: number(f[23], "theta")?,
        vega: number(f[22], "vega")?,
    })
}

/// Decode the zip `blob`, the `underlying`'s file for the UTC day starting at `day`: the day's rows,
/// sorted by `(ts, instrument)`. See the module doc for every rule.
pub fn decode_summary_zip(
    blob: &[u8],
    underlying: &str,
    day: i64,
) -> Result<Vec<ChainRow>, ArchiveDayRefusal> {
    let member = dataset_member_name(underlying, FILE_WORD, day);
    let label = epoch_ms_to_utc_date(day);
    let mut rows: Vec<ChainRow> = Vec::new();
    let mut seen: HashMap<(i64, String), usize> = HashMap::new();
    let mut repeats = 0usize;
    read_member_lines(blob, &member, MAX_LINE_BYTES, |number, line| {
        if number == 1 && line == HEADER_LINE {
            return Ok(());
        }
        let row = parse_row(line, underlying, day, &label).map_err(|why| {
            // A real date of another day is a mis-filed file; anything else is a row that does not parse.
            let date = line.split(',').next().unwrap_or_default();
            let foreign_day = date != label && day_of_label(date).is_some();
            let class = if foreign_day { "OutsideDay" } else { "BadRow" };
            refused(
                class,
                format!(
                    "line {number} of `{member}` does not parse: {why}. A row that does not parse \
                     is never skipped, so the whole day is refused."
                ),
            )
        })?;
        match seen.get(&(row.ts, row.instrument.clone())) {
            Some(&at) if rows[at] == row => {
                repeats += 1;
                Ok(())
            }
            Some(_) => Err(refused(
                "ContradictoryRepeat",
                format!(
                    "line {number} of `{member}` repeats the hour of {} with a different row. The \
                     file contradicts itself and nothing here can say which row is right, so the \
                     whole day is refused.",
                    row.instrument
                ),
            )),
            None if rows.len() == MAX_ROWS => Err(refused(
                "DecodedTooLarge",
                format!("`{member}` holds more than the {MAX_ROWS} rows one day may decode to."),
            )),
            None => {
                seen.insert((row.ts, row.instrument.clone()), rows.len());
                rows.push(row);
                Ok(())
            }
        }
    })?;
    if repeats > 0 {
        tracing::info!(%member, repeats, "{BINANCE_OPTION_SUMMARY_FORMAT}: rows repeating an earlier row exactly were dropped");
    }
    rows.sort_by(|a, b| (a.ts, &a.instrument).cmp(&(b.ts, &b.instrument)));
    Ok(rows)
}

/// Binance's hourly option-chain summaries of one underlying, as `kind=chain` under `venue=binance`.
#[derive(Debug, Clone)]
pub struct OptionSummaryLane {
    underlying: String,
}

impl OptionSummaryLane {
    fn key(&self, day: i64) -> String {
        day_commit_key(&self.underlying, &epoch_ms_to_utc_date(day))
    }
}

impl OptionArchiveLane for OptionSummaryLane {
    const SOURCE: &'static str = BINANCE_OPTION_SUMMARY_FORMAT;
    const FILE_WORD: &'static str = FILE_WORD;

    fn for_dataset(dataset: &str) -> Result<Self, String> {
        Ok(Self { underlying: option_dataset(BINANCE_OPTION_SUMMARY_FORMAT, dataset)? })
    }

    fn decode(&self, blob: &[u8], day: i64) -> Result<Vec<ChainRow>, ArchiveDayRefusal> {
        decode_summary_zip(blob, &self.underlying, day)
    }

    fn admission(&self) -> String {
        let u = &self.underlying;
        format!(
            "the vendor's hourly summary of every listed option of {u}, as published (prices in \
             USDT) · stored as {VENUE} chain {u}, the series a study reads point-in-time, each row \
             stamped at the last millisecond of the hour it summarises · the venue's own greeks · \
             a zero bid, ask, mark or iv is stored absent · the hour's trade prices, USDT volume and \
             best-quote ivs are dropped (no column holds them) · every zip is checked against its \
             .CHECKSUM (SHA-256) before it is read · daily zips only · no bars"
        )
    }
}

impl ArchiveLane for OptionSummaryLane {
    type Row = ChainRow;

    fn source(&self) -> &str {
        BINANCE_OPTION_SUMMARY_FORMAT
    }

    fn symbol(&self) -> &str {
        &self.underlying
    }

    fn row_series(&self) -> SeriesId {
        chain_series(&self.underlying)
    }

    fn row_timestamp(row: &ChainRow) -> i64 {
        row.ts
    }

    fn owners(&self, keys: &[String]) -> DayOwners {
        own_day_owners(keys, &day_commit_key(&self.underlying, ""))
    }

    fn overlapped_detail(&self, keys: &str) -> String {
        super::overlapped(BINANCE_OPTION_SUMMARY_FORMAT, keys)
    }

    fn append_day(
        &self,
        hist: &HistoryStoreDataFusion,
        rows: &[ChainRow],
        day: i64,
        _end: i64,
        _bars: &[String],
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError> {
        let key = self.key(day);
        let rows = hist.append_chain_snapshot(VENUE, &self.underlying, rows, Some(&key))?;
        let spent = vec![(DayOwnerRole::Archive, key.clone())];
        Ok(Ok(ArchiveDayAppend { rows, key, spent }))
    }

    fn undividable_interval_remedy(&self) -> String {
        super::no_bars(BINANCE_OPTION_SUMMARY_FORMAT)
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
        Err(CollectError::Refused(super::no_bars(BINANCE_OPTION_SUMMARY_FORMAT).into()))
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
#[path = "summary_tests.rs"]
mod summary_tests;
