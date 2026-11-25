//! Binance's public KLINES archive as an archive lane: the venue's own candles, read by the datahub's
//! import lane from the vendor's daily or monthly zips into the SOURCELESS bar series under
//! `venue=binance`, the series the REST backfill and the live path write (decision 0268, point 2).
//!
//! `crate::archive_import` runs the day. This file is what only this lane knows: the file names, the
//! row grammar and its time unit, the day key, which of a series' keys own which days, and the
//! append and replace calls. The zip reader is [`super::decode`]'s. The `.CHECKSUM` beside every zip
//! is checked by the datahub's format before a byte of the zip reaches this file
//! (`crates/vike-datahub/src/import/formats/binance_klines.rs`). The operator's page is
//! `docs/ops/binance-archive-import.md`.
//!
//! # Spot or perpetual is the series' spelling
//!
//! Under `venue=binance` the store tells the two apart by the symbol alone: the spot pair is
//! `BTCUSDT` and the USDⓈ-M perpetual is `BTCUSDT.P` (`vike_venues::PERP_SUFFIX`; the REST lane picks
//! the spot or the futures host from the same suffix, `crates/bridges/binance/src/history.rs`'s
//! `fetch_klines_range`). A lane is built over the store's spelling and the vendor's own symbol
//! (`BTCUSDT` for both, [`super::archive_dataset`]), and the files say nothing about which market they
//! come from. So the operator says it TWICE and the two must agree: the dataset's spelling, and the
//! vendor's own market folder under it ([`super::vendor_market`]: `BTCUSDT/spot/<interval>/`,
//! `BTCUSDT.P/futures/um/<interval>/`). A USDⓈ-M zip dropped in a folder without its market word is
//! never read as the spot pair.
//!
//! # The time unit is decided per file
//!
//! An open time is epoch milliseconds, or, in Binance's spot files from 2025 on, microseconds. The
//! first row decides, by which unit puts it inside the file's own span (its day, or its month), and
//! every later row must read in that same unit. A value neither unit places, a row in the other
//! unit, a close time at or before the open time or later than the open time plus one interval, an
//! open time off the interval's grid, and rows out of order are each refused, never guessed: the
//! whole day is refused.
//!
//! # A bar a maintenance cut short, or stamped one unit late, is kept as published
//!
//! A whole bar closes at its open plus one interval less one unit. Binance's own files also hold:
//!
//! - bars that close EARLY: the exchange stopped for maintenance inside the interval, and the row's
//!   close time is before that (19 of the spot 4h bars of BTCUSDT and of ETHUSDT, 2017 to 2021, the
//!   same rows in the daily and the monthly zips);
//! - a bar whose close Binance stamped ONE UNIT LATE, at its open plus exactly one interval: the
//!   monthly spot 4h zips of 2017-09 (BTCUSDT and ETHUSDT), on the bar of 2017-09-06 12:00.
//!
//! Such a row opens on the grid and is the venue's candle for that interval, so it is stored at its
//! open time with its prices and volume as written. The file is the vendor's own, verified against
//! its `.CHECKSUM`, and decision 0268 (point 2) makes those files the venue's candles: refusing the
//! row refused the venue's own series, and since a refusal takes its whole file, a monthly zip's whole
//! month. A close later than one interval after the open, at or before the open (the zero-length
//! filler bar of the DAILY 2017-09-06 zip, which the monthly zip does not hold), or an open off the
//! grid is still refused: that is another interval's file, or a damaged one.
//!
//! # Mark, index and premium candles are refused
//!
//! `data.binance.vision` publishes three more candle families beside `klines`: `markPriceKlines`,
//! `indexPriceKlines` and `premiumIndexKlines`. Their zips carry the SAME file name, the same one CSV
//! member and the same header line, byte for byte (measured 2026-11-16 on BTCUSDT 2025-06-01, 1m and
//! 1d), and the `.CHECKSUM` only proves a file is the one Binance published. The operator's folder does
//! not name the family either (`<dataset>/futures/um/<interval>/`), and the datahub downloads no Binance
//! zip, so no path the import sees could say it honestly. The ROWS do: a family that samples a price
//! writes `volume`, `quote_volume` and both taker volumes as 0 and `count` as its number of samples (60
//! on a mark or index 1m row, 86,400 on a 1d row, 12 and 17,280 on a premium one), where a candle's
//! `count` is its trades, and a trade moves volume. So a file whose EVERY row has a volume of 0 beside a
//! non-zero count is refused whole as [`NOT_LAST_PRICE`], naming the families: no mark or index candle
//! is ever filed as the venue's last-price series. A file of tradeless candles (volume 0, count 0) is
//! still candles and still reads. A premium file is refused earlier still, whenever a row holds a
//! value at or below zero (`BadRow`). The families get no series of their own: none is named anywhere
//! in the store (decision 0290), and inventing one is a decision.
//!
//! # One owner per UTC day
//!
//! The store dedups by commit key, never by row, so each day of the series has one owner
//! (`BinanceKlinesLane`'s `owners`): this lane's day key holds it; a REST window key over exactly the
//! day holds it too; any other key that meets the day (a REST window with other bounds, the live
//! path's one-bar start-history keys, a key this lane cannot read) makes it `Overlapped`. The bars
//! built from aggTrades are a lane of their own (`source=aggtrades`), another series, so they never
//! meet a day here. A day another owner holds can be REPLACED whole when its stored bars are fewer
//! than a whole day's (decision 0268, point 4): the lane's `day_fill` counts them and its
//! `replace_day` writes the archive's day in their place.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read};

use flate2::CrcReader;
use flate2::bufread::DeflateDecoder;
use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal, BINANCE_KLINES_FORMAT};
use vike_data::{HistoryStore, SeriesCoverage, SeriesId, TimestampRange};
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use super::decode::{
    LOCAL_HEADER_BYTES, MAX_LINE_BYTES, check_after_stream, corrupt_stream, drain, not_one_member,
    read_local_header, refused,
};
use super::{PUBLICATION_MARGIN_MS, VENUE, day_of_label};
use crate::archive_import::{
    ArchiveDayAppend, ArchiveDayFill, ArchiveLane, DayOwnerKey, DayOwnerRole, DayOwners,
};
use crate::error::CollectError;

/// The refusal class of a file of the vendor's sampled-price families (the module doc's "Mark, index
/// and premium candles are refused").
pub const NOT_LAST_PRICE: &str = "NotLastPrice";

/// The header line Binance's futures files open with; a spot file has none.
const HEADER_LINE: &str = "open_time,open,high,low,close,volume,close_time,quote_volume,count,\
                           taker_buy_volume,taker_buy_quote_volume,ignore";

/// A day's key, spent by its bars: `day` is `YYYY-MM-DD`.
pub fn day_commit_key(symbol: &str, interval: &str, day: &str) -> String {
    format!("binance-klines:{symbol}:{interval}:{day}")
}

/// The sourceless series `symbol`'s candles at `interval` land in: the venue's own.
pub fn kline_series(symbol: &str, interval: &str) -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Bar, VENUE, symbol, Some(interval.to_string()))
}

/// The series' coverage and commit log, from ONE manifest parse: what a plan that writes nothing
/// classifies its days against, without the day-owner lock.
pub fn kline_series_facts(
    hist: &HistoryStoreDataFusion,
    symbol: &str,
    interval: &str,
) -> Result<(SeriesCoverage, Vec<String>), CollectError> {
    Ok(hist.series_facts(&kline_series(symbol, interval))?)
}

/// `YYYY-MM` as the epoch-ms starts of that month's first and last UTC day.
fn month_days(label: &str) -> Option<(i64, i64)> {
    if label.len() != 7 || label.as_bytes()[4] != b'-' {
        return None;
    }
    let first = day_of_label(&format!("{label}-01"))?;
    let (y, m, _) = vike_utility::calendar::civil_from_days(first / MS_PER_DAY);
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let next = vike_utility::calendar::days_from_civil(ny, nm, 1) * MS_PER_DAY;
    Some((first, next - MS_PER_DAY))
}

/// The UTC days a zip's file NAME covers, as the starts of its first and last day, whatever symbol
/// and interval it spells: `BTCUSDT-1m-2024-01-15.zip` is that one day, `BTCUSDT-1m-2024-01.zip`
/// every day of January 2024. `None` for anything else, a `.CHECKSUM` included.
pub fn kline_zip_days(name: &str) -> Option<(i64, i64)> {
    // Binance's symbols and intervals hold no `-`, so the third piece is the whole date.
    let mut pieces = name.strip_suffix(".zip")?.splitn(3, '-');
    let (symbol, interval, date) = (pieces.next()?, pieces.next()?, pieces.next()?);
    if symbol.is_empty() || interval.is_empty() {
        return None;
    }
    match date.len() {
        10 => day_of_label(date).map(|day| (day, day)),
        7 => month_days(date),
        _ => None,
    }
}

/// The unit an open time is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeUnit {
    Millis,
    Micros,
}

impl TimeUnit {
    fn per_ms(self) -> i64 {
        match self {
            TimeUnit::Millis => 1,
            TimeUnit::Micros => 1000,
        }
    }

    fn name(self) -> &'static str {
        match self {
            TimeUnit::Millis => "milliseconds",
            TimeUnit::Micros => "microseconds",
        }
    }

    /// `raw` in this unit as epoch ms, when it is a whole millisecond inside `[lo, hi]`.
    fn ms_in(self, raw: i64, lo: i64, hi: i64) -> Option<i64> {
        let per = self.per_ms();
        (raw.rem_euclid(per) == 0).then(|| raw.div_euclid(per)).filter(|ms| (lo..=hi).contains(ms))
    }

    /// The one unit that places `raw` inside `[lo, hi]`, `None` when neither or both do.
    fn of(raw: i64, lo: i64, hi: i64) -> Option<TimeUnit> {
        match (TimeUnit::Millis.ms_in(raw, lo, hi), TimeUnit::Micros.ms_in(raw, lo, hi)) {
            (Some(_), None) => Some(TimeUnit::Millis),
            (None, Some(_)) => Some(TimeUnit::Micros),
            _ => None,
        }
    }
}

/// One row, parsed: the open and close times as written, the bar's prices and base volume, and its
/// `count` (`None` when it does not read as a whole number: only the sampled-price check reads it).
struct KlineRow {
    open_time: i64,
    close_time: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    count: Option<u64>,
}

/// Parse one line of twelve fields, or name the field that does not parse. The volume is the BASE
/// asset's, column 5, as the REST lane reads it.
fn parse_kline(line: &str) -> Result<KlineRow, &'static str> {
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() != 12 {
        return Err("the row, which does not have twelve fields");
    }
    let time = |i: usize, name| fields[i].parse::<i64>().map_err(|_| name);
    let price = |i: usize, name| match fields[i].parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Ok(v),
        _ => Err(name),
    };
    let row = KlineRow {
        open_time: time(0, "open_time")?,
        open: price(1, "open")?,
        high: price(2, "high")?,
        low: price(3, "low")?,
        close: price(4, "close")?,
        volume: match fields[5].parse::<f64>() {
            Ok(v) if v.is_finite() && v >= 0.0 => v,
            _ => return Err("volume"),
        },
        close_time: time(6, "close_time")?,
        count: fields[8].parse().ok(),
    };
    if row.low > row.open.min(row.close) || row.high < row.open.max(row.close) {
        return Err("high or low, which do not bound open and close");
    }
    Ok(row)
}

/// Decode the zip `blob` for the UTC day starting at `day`: the daily file of that day, or the
/// monthly file of its month, of `vendor` (Binance's symbol) at `interval`, whose step is `step_ms`.
/// The day's bars, in order; empty when the file holds no row of it.
///
/// ⚠ The member's name is checked because the walk sees only a file's name, never what it holds:
/// this is what stops another symbol's or another interval's file landing in this series.
pub fn decode_klines_zip(
    blob: &[u8],
    vendor: &str,
    interval: &str,
    step_ms: i64,
    day: i64,
) -> Result<Vec<Bar>, ArchiveDayRefusal> {
    let header = read_local_header(blob)?;
    let start = LOCAL_HEADER_BYTES + header.name_len + header.extra_len;
    let name = blob.get(LOCAL_HEADER_BYTES..LOCAL_HEADER_BYTES + header.name_len);
    let Some(stream) = blob.get(start..) else {
        return Err(not_one_member("its local header runs past the end of the file"));
    };
    let label = epoch_ms_to_utc_date(day);
    let daily = format!("{vendor}-{interval}-{label}.csv");
    let monthly = format!("{vendor}-{interval}-{}.csv", &label[..7]);
    let (member, first, last) = if name == Some(daily.as_bytes()) {
        (daily, day, day)
    } else if name == Some(monthly.as_bytes())
        && let Some((first, last)) = month_days(&label[..7])
    {
        (monthly, first, last)
    } else {
        return Err(refused(
            "ForeignMember",
            format!(
                "this zip holds neither `{daily}` nor `{monthly}`, one of which this dataset's day \
                 must hold. It was renamed, or it is another symbol's or another interval's file."
            ),
        ));
    };
    let (lo, hi, end) = (first, last + MS_PER_DAY - 1, day + MS_PER_DAY - 1);

    let inflate = CrcReader::new(DeflateDecoder::new(stream));
    let mut reader = BufReader::with_capacity(1 << 16, inflate);
    let mut buffer = Vec::with_capacity(MAX_LINE_BYTES as usize);
    let (mut number, mut unit, mut previous) = (0u64, None::<TimeUnit>, None::<i64>);
    // Whether every row so far reads as a sampled-price row (the module doc's "Mark, index and premium
    // candles are refused"), and the last row's count, for the refusal's words.
    let (mut sampled, mut samples) = (true, 0u64);
    let mut bars = Vec::new();
    loop {
        buffer.clear();
        let n = (&mut reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut buffer)
            .map_err(|e| corrupt_stream(&e))?;
        if n == 0 {
            break;
        }
        number += 1;
        let complete = buffer.last() == Some(&b'\n');
        let text = if complete { &buffer[..n - 1] } else { &buffer[..] };
        let row = std::str::from_utf8(text)
            .map_err(|_| "a field, because the line is not UTF-8")
            .and_then(|line| {
                if number == 1 && line == HEADER_LINE {
                    Ok(None)
                } else {
                    parse_kline(line).map(Some)
                }
            });
        let row = match row {
            Ok(Some(row)) => row,
            Ok(None) => continue,
            Err(field) => {
                // A partial last line is what a truncated stream looks like: name that, if it is.
                if !complete {
                    let mut inflate = reader.into_inner();
                    drain(&mut inflate)?;
                    let crc = inflate.crc().sum();
                    check_after_stream(inflate.into_inner().into_inner(), header.flags, crc)?;
                }
                return Err(refused(
                    "BadRow",
                    format!(
                        "line {number} of `{member}` does not parse: {field}. A row that does not \
                         parse is never skipped, so the whole day is refused."
                    ),
                ));
            }
        };
        let at = |class: &str, what: String| {
            refused(
                class,
                format!("line {number} of `{member}` {what}, so the whole day is refused."),
            )
        };
        let decided = match unit {
            Some(decided) => decided,
            None => *unit.insert(TimeUnit::of(row.open_time, lo, hi).ok_or_else(|| {
                at(
                    "OutsideDay",
                    format!(
                        "has an open_time that neither milliseconds nor microseconds place inside \
                         the file's span [{lo}, {hi}]"
                    ),
                )
            })?),
        };
        let Some(open) = decided.ms_in(row.open_time, lo, hi) else {
            let mixed = TimeUnit::of(row.open_time, lo, hi).is_some();
            return Err(if mixed {
                at(
                    "MixedTimeUnit",
                    format!(
                        "has an open_time in the other unit than the file's first row, which is in \
                         {}: the file is ambiguous",
                        decided.name()
                    ),
                )
            } else {
                at("OutsideDay", format!("has an open_time outside the file's span [{lo}, {hi}]"))
            });
        };
        // A bar a maintenance cut short closes early, and one Binance stamped one unit late closes
        // at its interval's end: both are kept, see the module doc.
        let last_close = row.open_time + step_ms * decided.per_ms();
        if !(row.open_time + 1..=last_close).contains(&row.close_time) {
            return Err(at(
                "BadCloseTime",
                format!(
                    "has a close_time that is not after its open_time, or is later than its \
                     open_time plus one {interval}: the file is another interval's, or damaged"
                ),
            ));
        }
        if open.rem_euclid(step_ms) != 0 {
            return Err(at("OffGrid", format!("opens off the {interval} grid")));
        }
        if previous.is_some_and(|p| open <= p) {
            return Err(at("NotIncreasing", "does not open after the row above it".to_string()));
        }
        previous = Some(open);
        sampled &= row.volume == 0.0 && row.count.is_some_and(|c| c > 0);
        samples = row.count.unwrap_or(0);
        if (day..=end).contains(&open) {
            bars.push(Bar {
                timestamp: open,
                open: row.open,
                high: row.high,
                low: row.low,
                close: row.close,
                volume: row.volume,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            });
        }
    }
    let inflate = reader.into_inner();
    let crc = inflate.crc().sum();
    check_after_stream(inflate.into_inner().into_inner(), header.flags, crc)?;
    if sampled && previous.is_some() {
        return Err(refused(
            NOT_LAST_PRICE,
            format!(
                "every row of `{member}` has a volume of 0 beside a count of {samples} on its last \
                 {interval} row: a count of price SAMPLES, where the venue's candle counts its trades. \
                 That is Binance's markPriceKlines or indexPriceKlines (one sample a second) or \
                 premiumIndexKlines (one every five seconds), whose zips carry the same name, member \
                 and header as its klines. They are not the venue's last-price candles, and this \
                 format stores no other family: take the file from the vendor's klines folder."
            ),
        ));
    }
    Ok(bars)
}

/// `{start_ms}-{end_ms}`, with or without the chunked ingest's `:empty` marker, as its two times.
fn rest_window(tail: &str) -> Option<(i64, i64)> {
    let (a, b) = tail.strip_suffix(":empty").unwrap_or(tail).split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Binance's klines zips of one instrument at one interval, as candles under `venue=binance`.
#[derive(Debug, Clone)]
pub struct BinanceKlinesLane {
    symbol: String,
    vendor: String,
    interval: String,
    step_ms: i64,
}

impl BinanceKlinesLane {
    /// The lane over the series `symbol` (the store's spelling, `BTCUSDT.P`) at `interval`, reading
    /// `vendor`'s files (Binance's symbol, `BTCUSDT`). An interval a day's import cannot hold whole is
    /// refused: one with no fixed step (`1w`, `1M`: `vike_marketdata::interval::measures_bar_step` is
    /// false), a second spelling of a step (`01m`), or a step that does not divide a UTC day (`3d`).
    pub fn new(symbol: &str, vendor: &str, interval: &str) -> Result<Self, String> {
        let step = vike_marketdata::interval::canonical_bar_step_ms(interval)
            .filter(|_| vike_marketdata::interval::measures_bar_step(interval))
            .filter(|step| MS_PER_DAY % step == 0);
        let Some(step_ms) = step else {
            return Err(format!(
                "{BINANCE_KLINES_FORMAT} reads one interval whose candles a UTC day holds whole: a canonical step \
                 that divides a day (1s, 1m, 5m, 1h, 1d …). {interval:?} is not one: a week, a month \
                 or 3d straddles the day's import, and a second spelling would be a second series."
            ));
        };
        Ok(Self {
            symbol: symbol.to_string(),
            vendor: vendor.to_string(),
            interval: interval.to_string(),
            step_ms,
        })
    }

    /// [`decode_klines_zip`] for this lane's symbol and interval.
    pub fn decode(&self, blob: &[u8], day: i64) -> Result<Vec<Bar>, ArchiveDayRefusal> {
        decode_klines_zip(blob, &self.vendor, &self.interval, self.step_ms, day)
    }

    fn key(&self, day: i64) -> String {
        day_commit_key(&self.symbol, &self.interval, &epoch_ms_to_utc_date(day))
    }
}

impl ArchiveLane for BinanceKlinesLane {
    type Row = Bar;

    fn source(&self) -> &str {
        BINANCE_KLINES_FORMAT
    }

    fn symbol(&self) -> &str {
        &self.symbol
    }

    fn row_series(&self) -> SeriesId {
        kline_series(&self.symbol, &self.interval)
    }

    fn row_timestamp(row: &Bar) -> i64 {
        row.timestamp
    }

    /// See the module doc. ⚠ The live path saves one key per bar, so a day it touched keeps ONE key
    /// here, as `BinanceArchiveLane`'s `owners` keeps one per recorder day.
    fn owners(&self, keys: &[String]) -> DayOwners {
        let own_head = day_commit_key(&self.symbol, &self.interval, "");
        let rest_head = format!("{VENUE}:{}:{}:", self.symbol, self.interval);
        let mut touched: BTreeMap<i64, &String> = BTreeMap::new();
        let mut owners = Vec::new();
        for key in keys {
            if let Some(day) = key.strip_prefix(&own_head).and_then(day_of_label) {
                let range = Some((day, day + MS_PER_DAY - 1));
                owners.push(DayOwnerKey { lane: DayOwnerRole::Archive, range, key: key.clone() });
            } else if let Some((a, b)) = key.strip_prefix(&rest_head).and_then(rest_window) {
                let (a, b) = (a.min(b), a.max(b));
                if a.rem_euclid(MS_PER_DAY) == 0 && b == a + MS_PER_DAY - 1 {
                    owners.push(DayOwnerKey {
                        lane: DayOwnerRole::Http,
                        range: Some((a, b)),
                        key: key.clone(),
                    });
                } else if b - a >= MS_PER_DAY {
                    owners.push(DayOwnerKey {
                        lane: DayOwnerRole::Other,
                        range: Some((a, b)),
                        key: key.clone(),
                    });
                } else {
                    for index in a.div_euclid(MS_PER_DAY)..=b.div_euclid(MS_PER_DAY) {
                        touched.entry(index * MS_PER_DAY).or_insert(key);
                    }
                }
            } else {
                owners.push(DayOwnerKey {
                    lane: DayOwnerRole::Other,
                    range: None,
                    key: key.clone(),
                });
            }
        }
        owners.extend(touched.into_iter().map(|(day, key)| DayOwnerKey {
            lane: DayOwnerRole::Other,
            range: Some((day, day + MS_PER_DAY - 1)),
            key: key.clone(),
        }));
        DayOwners::new(PUBLICATION_MARGIN_MS, owners)
    }

    fn overlapped_detail(&self, keys: &str) -> String {
        format!(
            "this series already holds candles on this day under other keys ({keys}): a REST \
             backfill's, the live path's, or another writer's. Importing the day would store those \
             bars twice, so it is not imported; --replace-holed replaces the day whole when it holds \
             fewer bars than a whole day. Nothing was read and no commit key was spent."
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
        let rows = hist.append_bars(VENUE, &self.symbol, &self.interval, rows, Some(&key))?;
        let spent = vec![(DayOwnerRole::Archive, key.clone())];
        Ok(Ok(ArchiveDayAppend { rows, key, spent }))
    }

    fn undividable_interval_remedy(&self) -> String {
        format!(
            "{BINANCE_KLINES_FORMAT} derives no bars: its rows ARE the candles, at the interval it reads."
        )
    }

    fn bar_series(&self, _interval: &str) -> SeriesId {
        self.row_series()
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

    /// The distinct bars stored on the day, against a whole day's at this interval.
    fn day_fill(
        &self,
        hist: &HistoryStoreDataFusion,
        _owners: &DayOwners,
        day: i64,
        end: i64,
    ) -> Result<Option<ArchiveDayFill>, CollectError> {
        let stored =
            hist.load_bars(VENUE, &self.symbol, &self.interval, TimestampRange::of(day, end))?;
        let distinct: BTreeSet<i64> = stored.iter().map(|b| b.timestamp).collect();
        let full = usize::try_from(MS_PER_DAY / self.step_ms).unwrap_or(usize::MAX);
        Ok(Some(ArchiveDayFill { stored: distinct.len(), full, rows: distinct.len() }))
    }

    /// Every part of the day out, the archive's bars in, under this lane's day key, in one publish:
    /// `vike_store::HistoryStoreDataFusion`'s `replace_bars_day`, which keeps every removed key spent.
    fn replace_day(
        &self,
        hist: &HistoryStoreDataFusion,
        rows: &[Bar],
        day: i64,
        _end: i64,
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError> {
        let key = self.key(day);
        let date = epoch_ms_to_utc_date(day);
        let rows = hist.replace_bars_day(VENUE, &self.symbol, &self.interval, &date, rows, &key)?;
        let spent = vec![(DayOwnerRole::Archive, key.clone())];
        Ok(Ok(ArchiveDayAppend { rows, key, spent }))
    }
}

#[path = "klines_tests.rs"]
#[cfg(test)]
mod klines_tests;
