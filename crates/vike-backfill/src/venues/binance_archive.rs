//! Binance's public aggTrades archive as an archive lane: one zip per UTC day, read by the datahub's
//! import lane into `kind=trade` under `venue=binance`, with bars only when asked, in a lane of their
//! own.
//!
//! `crate::archive_import` runs the day. This file is what only this lane knows: its key templates,
//! which of a series' keys own which days, the append call and where the bars go. The zip reader, the
//! row parser and the id continuity check are [`decode`]'s. The operator's page is
//! `docs/ops/binance-archive-import.md`. The venue's own candles, from its klines zips, are the
//! second Binance lane, [`klines`], which shares the zip reader and nothing else. The OPTION market's
//! two daily datasets, the hourly chain summary and the volatility index, are [`options`].
//!
//! # Why its bars have a lane of their own
//!
//! An aggregated trade carries one time, its first fill's, even when its fills run past a minute
//! boundary, so bars built from these rows cannot equal Binance's klines, and nothing in the file can
//! recover it, because the fills' own times are not there. Measured on ETHUSDT 2025-06-01 against the
//! `trades`, `aggTrades` and 1-minute kline files: bars from the individual fills differ from the
//! klines on 0 of 1,440 minutes, and bars from aggTrades on 236 (open 70, high 3, low 7, close 0,
//! volume 236); the aggregated time equals the first fill's on 1,120,447 of 1,120,447 rows. And
//! `kind=bar` under this venue holds the venue's own klines, in a store that dedups by commit key and
//! never by row, so derived bars there would be a second version of the same minute. So they land in
//! `kind=bar/venue=binance/source=aggtrades/symbol=…/interval=…`
//! ([`vike_data::store::store_kind::AGGTRADES_LANE`]), where only a read that names the lane meets them,
//! and the kline ingest and a chart stay exactly as they were.
//!
//! # One owner per UTC day, against the live recorder
//!
//! The store dedups by commit key, never by row, and the live recorder writes the same series, so a
//! day it touched and the file for that day would store the shared trades twice. A recorder key
//! (`live-binance-{symbol}-trade-{first_timestamp}-{last_timestamp}-{seq}`) therefore makes every day its range meets
//! `Overlapped`: refused and listed, never completed. ⚠ A key this lane cannot read meets EVERY day,
//! the conservative direction: it may be another writer's, and nothing here can say which days it
//! holds. A part with NO key at all is invisible to this rule; the recorder and every collector key
//! theirs.
//!
//! ⚠ A live series spends a key every few seconds, so [`BinanceArchiveLane::owners`] keeps ONE key per
//! day the recorder touched. Without that, the one-owner table would walk millions of keys for every
//! planned day, and a plan would carry each of them to the client.

use std::collections::BTreeMap;

use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal, BINANCE_AGGREGATE_TRADES_FORMAT};
use vike_data::store::store_kind::AGGTRADES_LANE;
use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_marketdata::TickTrade;
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use crate::archive_import::{ArchiveDayAppend, ArchiveLane, DayOwnerKey, DayOwnerRole, DayOwners};
use crate::error::CollectError;

pub mod decode;
pub mod klines;
pub mod options;

/// The store venue this lane's rows land under.
pub const VENUE: &str = "binance";

/// How long after a day ends its file is still `TooRecent`: twelve hours. Measured on 2026-11-05, the
/// file for 2026-11-04 was stamped 06:57 UTC, about seven hours after its day ended, so this leaves it
/// a few hours to settle and lets yesterday's file import on the day it appears. A file's key is spent
/// once stored, so a correction after that is not picked up: nothing here can see one.
pub const PUBLICATION_MARGIN_MS: i64 = 12 * 3_600_000;

/// A day's key, spent by its trades: `day` is `YYYY-MM-DD`.
pub fn day_commit_key(symbol: &str, day: &str) -> String {
    format!("binance-aggtrades:{symbol}:{day}")
}

/// The key of one day's bars at `interval`, in the lane: `day` is `YYYY-MM-DD`.
pub fn bar_commit_key(symbol: &str, interval: &str, day: &str) -> String {
    format!("binance-aggtrades-bars:{symbol}:{interval}:{day}")
}

/// The lane `symbol`'s bars at `interval` land in: see the module doc.
pub fn bar_lane_series(symbol: &str, interval: &str) -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Bar, VENUE, symbol, Some(interval.to_string()))
        .with_source(AGGTRADES_LANE)
}

/// The recorder's keys for `symbol`, up to their `{first_timestamp}-{last_timestamp}-{seq}` tail:
/// `crates/vike-store/src/live_rec/writer.rs`'s `live-{venue}-{symbol}-{}-{first_timestamp}-{last_timestamp}-{seq}`,
/// with the kind's tag in the slot. ⚠ That template is the recorder's, so a change there must change
/// this: `the_recorder_template_is_the_one_this_lane_reads` holds this to the store's `trade` row, which
/// `crates/vike-ops/tests/venues/store_kind_gate/shape.rs` holds to the writer.
fn recorder_key_head(symbol: &str) -> String {
    format!("live-{VENUE}-{symbol}-{}-", StoreKindId::Trade)
}

/// The series `symbol`'s trades land in.
pub fn trade_series(symbol: &str) -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Trade, VENUE, symbol, None)
}

/// The series' coverage and commit log, from ONE manifest parse: what a plan that writes nothing
/// classifies its days against, without the day-owner lock.
pub fn trade_series_facts(
    hist: &HistoryStoreDataFusion,
    symbol: &str,
) -> Result<(vike_data::SeriesCoverage, Vec<String>), CollectError> {
    Ok(hist.series_facts(&trade_series(symbol))?)
}

/// `{first_timestamp}-{last_timestamp}-{seq}` as its two times, or `None` when they do not parse.
fn recorder_range(tail: &str) -> Option<(i64, i64)> {
    let mut fields = tail.split('-');
    let (first, last, seq) = (fields.next()?, fields.next()?, fields.next()?);
    if fields.next().is_some() || seq.parse::<u64>().is_err() {
        return None;
    }
    Some((first.parse().ok()?, last.parse().ok()?))
}

/// `YYYY-MM-DD` as the epoch-ms START of that UTC day, `None` for anything else, a date that does
/// not exist included (`vike_model::parse_ymd` refuses it).
pub(crate) fn day_of_label(label: &str) -> Option<i64> {
    let bytes = label.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let (y, m, d) = vike_model::parse_ymd(label).ok()?;
    Some(vike_utility::calendar::days_from_civil(y, m, d) * MS_PER_DAY)
}

/// The day a daily zip's file NAME names, whatever symbol it spells: `SOLUSDT-aggTrades-2026-11-05.zip`
/// is 2026-11-05. `None` for anything else, a monthly `SOLUSDT-aggTrades-2026-09.zip` included.
pub fn daily_zip_day(name: &str) -> Option<i64> {
    dataset_zip_day(name, "aggTrades")
}

/// The one member a daily zip may hold, for `vendor` (the vendor's symbol, `SOLUSDT`) and `day`.
pub fn daily_member_name(vendor: &str, day: i64) -> String {
    dataset_member_name(vendor, "aggTrades", day)
}

/// The day a daily zip of the vendor's dataset `word` (`aggTrades`, `EOHSummary`, `BVOLIndex`) names,
/// whatever symbol it spells: `<symbol>-<word>-YYYY-MM-DD.zip`. `None` for anything else.
pub fn dataset_zip_day(name: &str, word: &str) -> Option<i64> {
    let (symbol, rest) = name.split_once(&format!("-{word}-"))?;
    if symbol.is_empty() {
        return None;
    }
    day_of_label(rest.strip_suffix(".zip")?)
}

/// The one member a daily zip of the vendor's dataset `word` may hold, for `vendor` and `day`.
pub fn dataset_member_name(vendor: &str, word: &str, day: i64) -> String {
    format!("{vendor}-{word}-{}.csv", epoch_ms_to_utc_date(day))
}

/// **The ONE grammar of a Binance archive dataset, for both formats** (aggTrades and klines): the
/// store's own name of the instrument, `BTCUSDT` the spot pair and `BTCUSDT.P` the USDⓈ-M perpetual
/// (`vike_venues::PERP_SUFFIX`). Answers the vendor's symbol (what its files are named by) and
/// whether the dataset is the perpetual; `None` for any other spelling (lower case, a dated future,
/// a COIN-M contract).
pub fn archive_dataset(dataset: &str) -> Option<(&str, bool)> {
    let vendor = dataset.strip_suffix(vike_venues::PERP_SUFFIX).unwrap_or(dataset);
    let named =
        !vendor.is_empty() && vendor.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    named.then_some((vendor, vendor.len() != dataset.len()))
}

/// The vendor's own market folder (`data.binance.vision`'s `data/<market>/...`) of a spot pair
/// (`spot`) or a USDⓈ-M perpetual (`futures/um`). The files of both markets carry the same names, so a
/// klines dataset's folder must say this word too, and it must agree with the dataset's spelling.
pub fn vendor_market(perp: bool) -> &'static str {
    if perp { "futures/um" } else { "spot" }
}

/// Binance's daily aggTrades zips of one instrument, as trades under `venue=binance`.
pub struct BinanceArchiveLane {
    symbol: String,
}

impl BinanceArchiveLane {
    /// The lane over the series `symbol`, in the store's spelling (`SOLUSDT.P`).
    pub fn new(symbol: &str) -> Self {
        Self { symbol: symbol.to_string() }
    }
}

impl ArchiveLane for BinanceArchiveLane {
    type Row = TickTrade;

    fn source(&self) -> &str {
        BINANCE_AGGREGATE_TRADES_FORMAT
    }

    fn symbol(&self) -> &str {
        &self.symbol
    }

    fn row_series(&self) -> SeriesId {
        trade_series(&self.symbol)
    }

    fn row_timestamp(row: &TickTrade) -> i64 {
        row.timestamp
    }

    fn owners(&self, keys: &[String]) -> DayOwners {
        let own_head = day_commit_key(&self.symbol, "");
        let recorder_head = recorder_key_head(&self.symbol);
        // One key per day the recorder touched: see the module doc.
        let mut recorded: BTreeMap<i64, &String> = BTreeMap::new();
        let mut owners = Vec::new();
        for key in keys {
            if let Some(day) = key.strip_prefix(&own_head).and_then(day_of_label) {
                let range = Some((day, day + MS_PER_DAY - 1));
                owners.push(DayOwnerKey { lane: DayOwnerRole::Archive, range, key: key.clone() });
            } else if let Some((first, last)) =
                key.strip_prefix(&recorder_head).and_then(recorder_range)
            {
                let (a, b) = (first.min(last), first.max(last));
                // A flush spans seconds. A key spanning days is kept whole, not walked day by day.
                if b.saturating_sub(a) > MS_PER_DAY {
                    owners.push(DayOwnerKey {
                        lane: DayOwnerRole::Other,
                        range: Some((a, b)),
                        key: key.clone(),
                    });
                    continue;
                }
                for index in a.div_euclid(MS_PER_DAY)..=b.div_euclid(MS_PER_DAY) {
                    recorded.entry(index * MS_PER_DAY).or_insert(key);
                }
            } else {
                owners.push(DayOwnerKey {
                    lane: DayOwnerRole::Other,
                    range: None,
                    key: key.clone(),
                });
            }
        }
        owners.extend(recorded.into_iter().map(|(day, key)| DayOwnerKey {
            lane: DayOwnerRole::Other,
            range: Some((day, day + MS_PER_DAY - 1)),
            key: key.clone(),
        }));
        DayOwners::new(PUBLICATION_MARGIN_MS, owners)
    }

    fn overlapped_detail(&self, keys: &str) -> String {
        format!(
            "this series already holds rows on this day under other keys ({keys}): the live \
             recorder's, or another writer's. Importing the day would store those trades twice, so \
             it is not imported. Nothing was read and no commit key was spent."
        )
    }

    fn append_day(
        &self,
        hist: &HistoryStoreDataFusion,
        rows: &[TickTrade],
        day: i64,
        _end: i64,
        _bars: &[String],
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError> {
        let key = day_commit_key(&self.symbol, &epoch_ms_to_utc_date(day));
        let rows = hist.append_trades(VENUE, &self.symbol, rows, Some(&key))?;
        let spent = vec![(DayOwnerRole::Archive, key.clone())];
        Ok(Ok(ArchiveDayAppend { rows, key, spent }))
    }

    fn undividable_interval_remedy(&self) -> String {
        "Import with a step that divides a day.".to_string()
    }

    fn bar_series(&self, interval: &str) -> SeriesId {
        bar_lane_series(&self.symbol, interval)
    }

    fn bar_commit_key(&self, interval: &str, range: TimestampRange) -> String {
        let day = epoch_ms_to_utc_date(range.start.unwrap_or(0));
        bar_commit_key(&self.symbol, interval, &day)
    }

    /// Resample the day's stored trades into the lane, never the sourceless series.
    fn derive_bars(
        &self,
        hist: &HistoryStoreDataFusion,
        interval: &str,
        range: TimestampRange,
    ) -> Result<usize, CollectError> {
        let key = self.bar_commit_key(interval, range);
        Ok(hist.resample_trades_to_bars_sourced(
            VENUE,
            AGGTRADES_LANE,
            &self.symbol,
            interval,
            range,
            Some(&key),
        )?)
    }

    /// This lane supersedes nothing, so the store has no such refusal to give it; the sentence is here
    /// for a store that one day does.
    fn bars_refused(
        &self,
        refused: &[(String, String)],
        written: &[ArchiveDayBars],
        rows: &str,
    ) -> ArchiveDayRefusal {
        let why = refused.iter().map(|(iv, e)| format!("{iv}: {e}")).collect::<Vec<_>>().join("; ");
        let others =
            written.iter().map(|b| format!("{} {}", b.rows, b.interval)).collect::<Vec<_>>();
        ArchiveDayRefusal::new(
            "BarsRefused",
            format!(
                "the store refused these bars ({why}), so they were not written; {rows}. Bars \
                 written for the other intervals: {others:?}."
            ),
        )
    }
}

#[path = "binance_archive_tests.rs"]
#[cfg(test)]
mod binance_archive_tests;
