//! **FXCM's public candle archive as a store-free, KEYLESS history source** — [`FxcmKlines`], this
//! bridge's `vike_data::source::KlineSource`: the FETCH half of a backfill and nothing else. It
//! returns rows; the commit key, the still-forming guard and the append are `vike-backfill`'s.
//!
//! No credential, no ForexConnect, no shim: anonymous HTTPS GETs of `candledata.fxcorporate.com`
//! (the `fxcm/MarketData` README names it), `<interval>/<PAIR>/<year>/<week>.csv.gz` for `m1` and
//! `H1` (one file per TRADING WEEK) and `D1/<PAIR>/<year>.csv.gz` (one file per year), 21 pairs, UTC,
//! `DateTime,BidOpen,..,BidClose,AskOpen,..,AskClose` in CRLF CSV with NO volume column. The
//! interval vocabulary is the archive's: `1m`, `1h`, `1d`. The datahub reaches it through the
//! gatewayhub's keyless `Klines` lane (`crates/vike-gatewayhub/src/rest/kline_sources.rs`'s
//! `KLINE_SOURCES`): ONE `fetch` per request, the whole window as one `Vec`, one window commit key.
//!
//! # What is stored — decided here, once
//!
//! * **The MID** of each price: `(bid + ask) / 2` for open, high, low and close separately. That is
//!   the side Dukascopy's line stores (`vike_marketdata::consolidate_quotes` folds the quote mid)
//!   and OANDA's (`price=M`), so a pair read from any of the three is comparable. It is an
//!   approximation: the mid of the highs is not the high of the mids, and the archive's own bid and
//!   ask files stay the source of truth.
//! * **One side, because the store holds one.** `vike_store`'s `BarCodec` writes
//!   `ts, open, high, low, close, volume, funding` and no `bid`/`ask` (a `Bar`'s own fields are
//!   dropped on write and read back as `None`). So the returned bars leave `bid` and `ask` `None`
//!   rather than pretend, and a bid series or an ask series cannot be stored beside the mid one.
//! * **`volume = 0.0`**, the codec's only "no volume" (the funding bar and a one-price quote bar
//!   carry the same). The archive has no volume or tick-count column.
//! * **The timestamp as the file states it**, UTC, milliseconds: the OPEN of the bar. A D1 bar is
//!   stamped at 17:00 New York (see below), not at midnight, exactly as OANDA's daily candles are
//!   stored as served.
//! * **The window is HALF-OPEN, `[start, end)`: `--to` is EXCLUSIVE here** (since 2026-10-24; every other
//!   source's end is inclusive, and `--to` is "an instant" in the CLI's help). A bar stamped exactly at
//!   the end is not returned, so `--to 2024-05-01` then `--from 2024-05-01` store the 00:00 bar once; the
//!   window of one bar step `[t, t + step)` is the bar `t`. The cut is made on the BARS (`append_window`),
//!   never on the key: `vike-backfill` still spends `fxcm:{symbol}:{interval}:{start}-{end}` over the
//!   request's own bounds, so a window stored before this change has the same key and repeating it is
//!   still a no-op (it keeps the bar at its end, and is not repaired). The week and year selection
//!   (`weeks_covering`, the `1d` year) and the unfinished-week rule use the last instant, `end - 1`.
//!
//! # The traps, each with a test (`data_tests.rs`)
//!
//! * **The week number in the URL is irregular and is never computed.** Measured on 2026-10-20
//!   (392 GETs, `crates/bridges/fxcm/scripts/archive-map.sh`; the map is in `docs/ops/fxcm-history.md`):
//!   2012..2014 and 2016..2025 follow the ISO week of the trading week's MONDAY, 2015 and 2026 are one
//!   lower (`2026/41` holds the week of Monday 12 October, ISO week 42), and the week whose Sunday open
//!   is in the old year but whose Monday is ISO week 1 is filed under the OLD year as number 53
//!   (`2025/53` holds the week of Monday 29 December) with no `/1` in the new one (the README's own
//!   `m1/EURUSD/2020/1` answers 404 and is no fixture). So `WeekWalk` asks the candidates around the
//!   ISO week (`week_candidates`; the last offset that worked first), reads the `DateTime` column
//!   INSIDE the file, and trusts that: a file for another week is cached under the Monday its own rows
//!   say (it is usually the next request) and the walk goes on.
//! * **A 404 is "not available", never "no candles" — and neither is a 200 holding a sliver.** The
//!   archive has holes (measured: `2025/1` and `/2`, `2024/52` and `/53`, `2026/18` and `/26`, the same
//!   on every pair and interval asked), a TRUNCATED week (`2025/26` holds one Friday evening: 93 rows
//!   of EURUSD, 200 of GBPUSD), a header-only week (`2023/27`), a ZERO-BYTE one (`2024/51` and `/52`: a
//!   200 with Content-Length 0), and publishes a week only after it ends. `vike_backfill`'s ingests
//!   turn an empty answer into permanent state: the window's commit key is spent on whatever came
//!   back, and the day-chunked ingest (`ingest_klines_chunked`) additionally MARKS an empty day empty
//!   forever once a later day holds data. So a week with no usable file is `SourceError::Fetch` —
//!   never `Ok` with the week missing. The walk goes on past a hole and the error lists EVERY missing
//!   week or year with the URLs tried, and the pieces of the window that do exist as exact
//!   `--from`/`--to` arguments (a date when the bound is a UTC midnight, else epoch milliseconds, so
//!   no bound is rounded), so one failure is enough to plan the whole fetch. What a "usable" week is
//!   is a SPAN, not a count: its kept rows must run at least three days from first to last
//!   (`WeekFloor`; holiday weeks that end on a Thursday pass), and a week that CLAIMS to be whole, by
//!   its first kept row being the Sunday open, must reach Thursday 12:00 UTC (a file cut after three
//!   days passes the span and fails this). A yearly file must keep at least `D1_MIN_ROWS` bars.
//!   The walk gives up on a stretch with no file at all: after `MAX_EMPTY_WEEKS` consecutive weeks
//!   whose every candidate answered 404 it stops asking, and says which weeks it did not ask. A window
//!   that starts before the archive does (2012-01-01) is refused up front, and so is a `1d` window that
//!   reaches a year after `D1_LAST_YEAR` (the yearly files stop there, on every pair: it is the archive's
//!   end, not an unpublished year), with `--to <the 1 January after it>` as the hint.
//! * **Which weeks a window needs is decided by the Sunday open and the Friday close, at BOTH ends.**
//!   A start inside the closed window (Friday 17:00 New York to Sunday 17:00 New York) begins at the
//!   next week; an END inside it ends at the week before — a Sunday end before 21:00/22:00 UTC does NOT
//!   ask for the next, possibly unpublished, week (`week_from`, `week_to`), and, the end being
//!   exclusive, neither does one AT the open (22:00 UTC in winter, 21:00 in summer: `weeks_covering`
//!   judges `end - 1`). A `1d` window needs the years of its stamps, which are 21:00 or 22:00 UTC of
//!   their day, so `--to 2022-01-01` needs no `D1/2022`. A window wholly inside the closed window needs
//!   no file and is an empty `Ok`: nothing there is ever kept.
//! * **Rows stamped while the market is shut are dropped.** The archive carries a few: Saturday
//!   quotes (`H1/EURUSD/2024/18` ends Saturday 09:00 UTC with two flat bars a 6.6-pip spread apart, the
//!   truncated `2025/26` has `1m` rows to Saturday 21:00), the 2012 files' last bar stamped exactly at
//!   the Friday close (`m1_eurusd_2012_04_tail` is the committed excerpt; three more such files, weeks
//!   4 and 5 of `m1` and `H1`, come from the map run), the same edge well after 2012 (`m1` 2022/50 and
//!   the `H1` files 2023/50, 2024/26, 2024/50 and 2025/34, also from the map run), and `1d` bars
//!   stamped on a Saturday (51 in 2020). Those are NOT slivers: a Saturday-stamped `1d` bar is the WEEKEND GAP. Its open is the
//!   previous session's close and its close is the next (Sunday-stamped) session's open, to the digit,
//!   in all 51, so its range is the gap itself: on the bid side 17.6 pips on average, 48 of the 51 over
//!   five pips, 15 over twenty, the widest 71.4 (2020-03-07). Dropping them is the choice made here,
//!   not a measurement of emptiness: the Friday-to-Monday jump stays visible between the kept bars (the
//!   shape OANDA's daily candles have) and no trading session loses its own open, high, low or close —
//!   but the gap's own extremes are not kept (the 2020-03-14 bar's low, 1.10537, is below the next
//!   session's low, 1.1065). `market_closed` is the rule for `1m`/`1h`, `open_rows` drops them.
//! * **A window that reaches a week not yet published (the "unfinished" week) is refused before any
//!   request.** The archive publishes a week on the MONDAY AFTER it ends (Last-Modified Monday 04:10
//!   UTC in summer, 05:10 in winter, on four files), so a week counts as published from that Monday
//!   06:00 UTC (`PUBLISHED_FROM_MONDAY_MS`) and, from the Friday close until then, the week just ended
//!   is refused too, not asked: nothing can answer it. This includes every window that ends now (a CLI
//!   `--days N`, the chart seed). The refusal's first line says so without naming a tool; its second
//!   line is the CLI hint, with the Saturday to end on (the one before the first unpublished week, so
//!   it works), and it can reach the GUI chart seed too (accepted). It is `Refused`, not an empty `Ok`.
//! * **D1 stamps are 17:00 New York, which is 21:00 or 22:00 UTC.** The file's `DateTime` is UTC and
//!   its D1 hour moves with US daylight saving (second Sunday of March to first Sunday of November).
//!   The stamp is stored as given and is held to that rule (`ny_close_utc_hour`): a D1 row at any
//!   other instant is a decode error, so a file whose convention changed is refused rather than
//!   stored shifted. Measured on 926 rows of three files with no exception; the rule is the post-2007
//!   US one, so a stamp before 2007 is refused as unverifiable. D1 files exist for 2012 to `D1_LAST_YEAR`
//!   only.
//! * **Bid and ask collapse to one side** (above).
//!
//! # Pace, size, retries
//!
//! No limit is stated; 26 back-to-back requests passed without a 429. This module takes at most two
//! requests a second across the PROCESS (`MAX_REQUESTS_PER_SECOND`), as a courtesy. A transient
//! failure (429, 5xx, a transport error) is retried on `RETRY_DELAYS` with the pattern of
//! `crates/bridges/dukascopy/src/data.rs`'s `fetch_hour_with`: an injected attempt and sleep, so the
//! schedule is testable with neither a network nor a clock. A file is read through a size cap and
//! decompressed through another (`MAX_BODY_BYTES`, `MAX_DECODED_BYTES`).
//!
//! `fetch` holds the whole window, so a window needing more than `Grain::max_weeks` weekly files is
//! `Refused` before a request leaves: about two years of `1m` (some 750,000 bars) and about five of
//! `1h`. It is one call with NO CANCEL POINT: `KlineSource::fetch` carries no cancel flag, so after a
//! Ctrl-C or a dropped link the gatewayhub's worker keeps issuing GETs, holding one of its fetch slots
//! and the bars it has built, until the window is done. The caps keep that to a minute or two. Lifting
//! them needs THREE hops, all shared homes, and none of them is this file: the trait (a cancel flag on
//! `KlineSource::fetch`), the gatewayhub's `fetch_klines` call site (it checks the cancel only before
//! the call), and the datahub's `real_backfill_table` kline row, which today ignores the request's stop
//! probe (so Ctrl-C and `data hist cancel` are noticed only at chunk boundaries, and a cancelled
//! one-batch window is still completed and committed) and must use the client's `with_stop` /
//! `fetch_until` and map a cancelled fetch to the lane's cancelled answer rather than `Fetch`. A long
//! call is not a stalled one: the gatewayhub's progress frames come from a timer, not from the fetch.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use flate2::read::GzDecoder;
use vike_bridge_core::http::blocking_agent;
use vike_bridge_core::ratelimit::RateGate;
use vike_data::source::{KlineSource, SourceError};
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::time::utc_weekday;
use vike_util::calendar::{civil_from_days, days_from_civil};

use crate::recon_client::VENUE;

#[path = "data_tests.rs"]
#[cfg(test)]
mod data_tests;

/// The archive's host. Its README (`github.com/fxcm/MarketData`) names it and the paths below.
const ARCHIVE: &str = "https://candledata.fxcorporate.com";

/// The 21 pairs the archive README lists, in its spelling — also the folder names, so the store
/// symbol IS the folder name. Only EURUSD, GBPUSD and USDJPY were ever asked for; the other
/// eighteen are the README's word.
const PAIRS: [&str; 21] = [
    "AUDCAD", "AUDCHF", "AUDJPY", "AUDNZD", "CADCHF", "EURAUD", "EURCHF", "EURGBP", "EURJPY",
    "EURUSD", "GBPCHF", "GBPJPY", "GBPNZD", "GBPUSD", "NZDCAD", "NZDCHF", "NZDJPY", "NZDUSD",
    "USDCAD", "USDCHF", "USDJPY",
];

/// The header every file of every interval carries, measured on m1, H1 and D1.
const HEADER: &str = "DateTime,BidOpen,BidHigh,BidLow,BidClose,AskOpen,AskHigh,AskLow,AskClose";

/// The most requests this process sends the archive in any one second, across EVERY fetch. A
/// courtesy, not a measured limit (module doc).
const MAX_REQUESTS_PER_SECOND: usize = 2;

/// The wait before each RETRY of one request — four waits, so five attempts, about 67 s in all. A
/// guess in the shape of Dukascopy's (a CDN fails in bursts); this archive's failure mode is unmeasured.
const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(45),
];

/// The longest a server-stated `Retry-After` is honoured for.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);

/// The most compressed bytes read from one response. A week of `1m` is about 150 KB.
const MAX_BODY_BYTES: u64 = 16 << 20;

/// The most bytes one file may decompress to. A week of `1m` is about 650 KB; this stops a
/// decompression bomb, not a big week.
const MAX_DECODED_BYTES: u64 = 64 << 20;

/// The last year the archive holds a yearly `1d` file for. Measured 2026-10-24 on all 21 pairs:
/// `D1/<PAIR>/2021` answers 200 and `D1/<PAIR>/2022` 404 (EURUSD: 2023 to 2026 too), and the 2020 and
/// 2021 files were last modified 2022-11-07. It is NOT "the current year's file appears after the year
/// ends": 2022 to 2025 are over and still absent. A window reaching a later year is refused up front
/// (`fetch_with`), as one that starts before [`ARCHIVE_FROM_MS`] is; it also bounds the `1d` walk to the
/// ten years the archive holds.
const D1_LAST_YEAR: i64 = 2021;

const HOUR_MS: i64 = 3_600_000;

/// The fewest bars a yearly `1d` file may KEEP (after the Saturday-stamped weekend-gap bars are dropped)
/// and still count as a year. A whole year keeps about 260 (measured: 2020 holds 311 rows, 51 of them
/// Saturday-stamped, so 260); 2012, the thinnest, holds 263 rows before the drop.
const D1_MIN_ROWS: usize = 200;

/// The first instant of the archive's span, 2012-01-01 00:00 UTC: the earliest file measured is
/// `2012/1` (first row Monday 2012-01-02 03:00 UTC) and nothing was found for 2011. A window starting
/// before it is refused up front instead of walking about four hundred weeks of 404.
const ARCHIVE_FROM_MS: i64 = 1_325_376_000_000;

/// How many CONSECUTIVE weeks may answer 404 on every candidate, with nothing usable between, before
/// the walk stops asking: eight weeks clears the longest hole measured (three consecutive weeks on
/// EURUSD: `2024/52`, `2024/53` and `2025/2`) and keeps a window with no archive behind it to about
/// thirty requests. The same constant counts consecutive YEARS for a `1d` window.
const MAX_EMPTY_WEEKS: usize = 8;

/// How far into its week a file that CLAIMS to be whole (its first kept row is the Sunday open) must
/// reach, from the Monday's midnight: Thursday 12:00 UTC. The shortest real week measured ends Thursday
/// 19:03 (a Christmas week); none of 321 Sunday-opening files in the map ends earlier.
const WHOLE_WEEK_REACHES_MS: i64 = 3 * MS_PER_DAY + 12 * HOUR_MS;

/// The earliest a `1d` stamp falls in its UTC day: 17:00 New York is 21:00 UTC in US summer time. A
/// window ending before this on a day holds no stamp of that day (`fetch_with`'s last year).
const D1_EARLIEST_STAMP_MS: i64 = 21 * HOUR_MS;

/// How long a weekly file's kept rows must run, first to last, to count as a week: three days. The
/// shortest real week measured (a Christmas week ending Thursday 19:03 UTC after a Sunday 22:00 open) runs
/// three days and twenty-one hours; the truncated `2025/26` runs an hour and a half.
const MIN_WEEK_SPAN_MS: i64 = 3 * MS_PER_DAY;

/// How far into the Monday after a week ends that week counts as published: 06:00 UTC. The files'
/// Last-Modified stamps (read 2026-10-24: `H1/EURUSD/2026/41` Monday 2026-10-19 04:10:35, `2023/27` Monday
/// 2023-07-10 04:10:22, `2024/51` Monday 2024-12-23 05:10:26, `2024/52` Monday 2024-12-30 05:10:24 GMT) are
/// 06:10 Central European time, a week after the Monday of the week they hold: 04:10 UTC in summer, 05:10
/// in winter. Four stamps of one pair and one interval, so this is a margin, not a measured limit.
const PUBLISHED_FROM_MONDAY_MS: i64 = 6 * HOUR_MS;

// ------------------------------------------------------------------------------------------------
// the source
// ------------------------------------------------------------------------------------------------

/// **FXCM's candle-archive source** — the `KlineSource` the datahub's keyless lane dispatches an
/// `fxcm` `Backfill` to. A unit struct: it holds no credential and no state, so it is a `&'static`
/// registry row, and every fetch shares one process-wide request budget.
#[derive(Debug, Clone, Copy, Default)]
pub struct FxcmKlines;

impl KlineSource for FxcmKlines {
    fn venue(&self) -> &str {
        VENUE
    }

    /// The mid bars of `[start_ms, end_ms)` — the end is EXCLUSIVE, unlike the other sources, so
    /// adjacent windows never share a bar (module doc) — oldest first.
    ///
    /// `Refused` — before anything is sent — for an interval other than `1m`/`1h`/`1d`, a symbol
    /// that is not one of the 21 pairs in the archive's own upper-case spelling (a second spelling
    /// would store a second series), a window needing more weekly files than `Grain::max_weeks`
    /// allows, a window that reaches a week the archive has not published yet, and a `1d` window that
    /// reaches a year after the last yearly file (`D1_LAST_YEAR`). Every other failure is `Fetch`, and a
    /// week or year with no usable file is one (module doc).
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        fetch_with(
            &mut LiveWire::new(),
            symbol,
            interval,
            (start_ms, end_ms),
            vike_model::now_ms(),
            None,
        )
    }
}

/// The three archive granularities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grain {
    M1,
    H1,
    D1,
}

impl Grain {
    fn of(interval: &str) -> Result<Self, SourceError> {
        match interval {
            "1m" => Ok(Self::M1),
            "1h" => Ok(Self::H1),
            "1d" => Ok(Self::D1),
            other => Err(SourceError::Refused(format!(
                "fxcm candle archive: {other:?} is not an interval it holds (only 1m, 1h and 1d: \
                 `m1` and `H1` weekly files, `D1` yearly ones)"
            ))),
        }
    }

    /// The archive's folder for this granularity.
    fn dir(self) -> &'static str {
        match self {
            Self::M1 => "m1",
            Self::H1 => "H1",
            Self::D1 => "D1",
        }
    }

    /// What makes a WEEKLY file a week: the SPAN of its kept rows (the measure that does not depend on
    /// how liquid a pair is) and, as a sanity check only, a very low row count.
    fn week_floor(self) -> WeekFloor {
        let min_rows = match self {
            Self::M1 => 100,
            Self::H1 => 12,
            Self::D1 => 0,
        };
        WeekFloor {
            min_rows,
            min_span_ms: MIN_WEEK_SPAN_MS,
            whole_week_reaches_ms: WHOLE_WEEK_REACHES_MS,
        }
    }

    /// The most weekly files one `fetch` holds. Not a memory bound alone: `KlineSource::fetch` has no
    /// cancel flag, so a call whose client has gone keeps the gatewayhub's worker issuing GETs (at
    /// two a second) and holding its bars until it ends. About two years of `1m` (105 weeks, some
    /// 750,000 bars, a minute or so) and about five of `1h` (270 weeks, 32,000 bars, two minutes).
    fn max_weeks(self) -> i64 {
        match self {
            Self::M1 => 105,
            Self::H1 => 270,
            Self::D1 => 0,
        }
    }

    /// [`Grain::max_weeks`] in words, for the refusal.
    fn max_span_words(self) -> &'static str {
        match self {
            Self::M1 => "two years",
            Self::H1 | Self::D1 => "five years",
        }
    }
}

/// What a weekly file's KEPT rows must show to count as a week: at least `min_rows` of them, running
/// at least `min_span_ms` from the first to the last, and — for a file whose first kept row is the
/// Sunday open, which claims to be a whole week — reaching `whole_week_reaches_ms` past its Monday's
/// midnight (a file cut after exactly three days passes the span and fails this). A holiday week that
/// opens on Monday or later claims nothing and is held to the span alone. A file under the floor is a
/// fragment (the truncated `2025/26`) and is treated as missing, never stored as a short week.
#[derive(Debug, Clone, Copy)]
struct WeekFloor {
    min_rows: usize,
    min_span_ms: i64,
    whole_week_reaches_ms: i64,
}

impl WeekFloor {
    /// Why `bars` (oldest first, closed rows already dropped) are not a week, or `None` when they are.
    fn truncation(&self, bars: &[Bar]) -> Option<String> {
        let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
            return Some("no rows kept: a truncated week".to_string());
        };
        let span = last.ts - first.ts;
        if bars.len() < self.min_rows || span < self.min_span_ms {
            return Some(format!(
                "{} rows over {} hours: a truncated week, a week runs at least {} hours",
                bars.len(),
                span / HOUR_MS,
                self.min_span_ms / HOUR_MS
            ));
        }
        let opens_on_sunday = weekday(first.ts.div_euclid(MS_PER_DAY)) == 6;
        let reaches = trading_monday(first.ts) * MS_PER_DAY + self.whole_week_reaches_ms;
        if opens_on_sunday && last.ts < reaches {
            return Some(format!(
                "a week that opens on Sunday but ends {} UTC: a truncated week, a whole week \
                 reaches at least Thursday 12:00 UTC",
                stamp_words(last.ts)
            ));
        }
        None
    }
}

/// `2026-10-15 19:03` of a UTC instant, for the messages.
fn stamp_words(ms: i64) -> String {
    let time = ms.rem_euclid(MS_PER_DAY) / 60_000;
    format!("{} {:02}:{:02}", vike_model::time::epoch_ms_to_utc_date(ms), time / 60, time % 60)
}

/// The store symbol as the archive's folder name, or the refusal.
fn pair_for(symbol: &str) -> Result<&'static str, SourceError> {
    PAIRS.iter().copied().find(|p| *p == symbol).ok_or_else(|| {
        let shown: String = symbol.chars().take(40).collect();
        SourceError::Refused(format!(
            "fxcm candle archive: {shown:?} is not one of its 21 pairs (six upper-case letters, \
             like EURUSD; the store symbol is the archive's folder name)"
        ))
    })
}

/// [`KlineSource::fetch`] over an injected [`Wire`] and an injected clock: the whole fetch —
/// refusals, the walk, the parse, the window — with the network, the clock and the budget replaced.
/// `floor` is `None` in production (the interval's own [`Grain::week_floor`]); only a test whose files
/// are short excerpts of real weeks passes `Some`.
fn fetch_with(
    wire: &mut impl Wire,
    symbol: &str,
    interval: &str,
    (start_ms, end_ms): (i64, i64),
    now_ms: i64,
    floor: Option<WeekFloor>,
) -> Result<Vec<Bar>, SourceError> {
    let grain = Grain::of(interval)?;
    let pair = pair_for(symbol)?;
    // `[start_ms, end_ms)`: an empty or inverted window holds nothing.
    if end_ms <= start_ms {
        return Ok(Vec::new());
    }
    if start_ms < ARCHIVE_FROM_MS {
        return Err(SourceError::Refused(format!(
            "fxcm candle archive: the window starts at {}, before the archive does: its earliest \
             sample is the week of Monday 2012-01-02 (`2012/1`, first row 2012-01-02 03:00 UTC) and \
             nothing was found for 2011. Start the window on or after 2012-01-01 \
             (`--from 2012-01-01`)",
            vike_model::time::epoch_ms_to_utc_date(start_ms)
        )));
    }
    let mut out: Vec<Bar> = Vec::new();
    let mut files = 0usize;
    let mut holes: Vec<Hole> = Vec::new();
    // The consecutive keys whose every candidate answered 404 (see `MAX_EMPTY_WEEKS`) and whether
    // that stopped the walk early; `walked_last` is the last key asked.
    let mut empty_run = 0usize;
    let mut stopped = false;
    let keys;
    let mut walked_last;
    if grain == Grain::D1 {
        let year_of = |ms: i64| civil_from_days(ms.div_euclid(MS_PER_DAY)).0;
        // A stamp is at least 21:00 UTC of its day, so a window whose last instant (`end_ms - 1`, the end
        // is exclusive) is earlier holds none of its end day's.
        let (first_year, last_year) =
            (year_of(start_ms), year_of((end_ms - 1).saturating_sub(D1_EARLIEST_STAMP_MS)));
        keys = (first_year, last_year);
        walked_last = last_year;
        if last_year > D1_LAST_YEAR {
            // ⚠ Two lines, like `unfinished_week_refusal`: a GUI (a chart seed) shows the first.
            let to = format!("{}-01-01", D1_LAST_YEAR + 1);
            return Err(SourceError::Refused(format!(
                "fxcm candle archive: the window reaches {last_year}, and FXCM's yearly 1d files \
                 stop at {D1_LAST_YEAR}: it holds none for a later year.\nFrom the command line: \
                 end the window on or before {to} (`--to {to}`), with a `--from` before it; \
                 `--days N` windows always end now and always land here."
            )));
        }
        for year in first_year..=last_year {
            let url = format!("{ARCHIVE}/D1/{pair}/{year}.csv.gz");
            let Some(body) = get_with_retries(wire, &url)? else {
                holes.push(Hole { key: year, tried: vec![format!("{url} (HTTP 404)")] });
                empty_run += 1;
                if empty_run >= MAX_EMPTY_WEEKS && year < last_year {
                    (walked_last, stopped) = (year, true);
                    break;
                }
                continue;
            };
            empty_run = 0;
            let bars = decode(&body, grain).map_err(|why| fetch_failure(&url, &why))?;
            let bars = open_rows(bars, grain);
            if bars.len() < D1_MIN_ROWS {
                let why = format!("{} bars kept: a truncated year", bars.len());
                holes.push(Hole { key: year, tried: vec![format!("{url} ({why})")] });
                continue;
            }
            if holes.is_empty() {
                append_window(&mut out, bars, start_ms, end_ms, &url)?;
            }
            files += 1;
        }
    } else {
        if let Some(refusal) = unfinished_week_refusal(end_ms, now_ms) {
            return Err(SourceError::Refused(refusal));
        }
        // A window wholly inside the closed window (a weekend) holds nothing we keep: no file to ask.
        let Some((first_monday, last_monday)) = weeks_covering(start_ms, end_ms) else {
            return Ok(Vec::new());
        };
        keys = (first_monday, last_monday);
        walked_last = last_monday;
        let weeks = (last_monday - first_monday) / 7 + 1;
        if weeks > grain.max_weeks() {
            return Err(SourceError::Refused(format!(
                "fxcm candle archive: [{start_ms}, {end_ms}) needs {weeks} weekly {interval} files \
                 and one fetch holds at most {} (about {}): the call cannot be cancelled once \
                 started, so it holds a gatewayhub worker and its bars until the last file is in. \
                 Ask for a shorter window, and keep the pieces from overlapping, because a \
                 different window over the same days stores them again",
                grain.max_weeks(),
                grain.max_span_words()
            )));
        }
        let mut walk = WeekWalk::new(wire, grain, pair, floor.unwrap_or(grain.week_floor()));
        for monday in (first_monday..=last_monday).step_by(7) {
            match walk.week(monday)? {
                Week::Rows(bars) => {
                    empty_run = 0;
                    if holes.is_empty() {
                        append_window(&mut out, bars, start_ms, end_ms, pair)?;
                    }
                    files += 1;
                }
                Week::Missing(tried) => {
                    let nothing_there = !tried.is_empty()
                        && tried.iter().all(|attempt| attempt.ends_with("(HTTP 404)"));
                    empty_run = if nothing_there { empty_run + 1 } else { 0 };
                    holes.push(Hole { key: monday, tried });
                    if empty_run >= MAX_EMPTY_WEEKS && monday < last_monday {
                        (walked_last, stopped) = (monday, true);
                        break;
                    }
                }
            }
        }
    }
    if !holes.is_empty() {
        let walked = Walked { last: walked_last, stopped };
        return Err(holes_error(grain, (start_ms, end_ms), keys, walked, &holes));
    }
    tracing::info!(
        target: "vike_fxcm::data",
        "fxcm {pair} {interval} [{start_ms}, {end_ms}): {} bars from {files} archive file(s)",
        out.len()
    );
    Ok(out)
}

/// Move the rows of one file that lie in `[start_ms, end_ms)` (the end is EXCLUSIVE: this is where the
/// cut is made, on the bars, so the request's own `end_ms` stays in the commit key unchanged) onto `out`,
/// which must stay STRICTLY increasing: two files that overlap are an error here, because the store
/// dedups by commit key and never by row and a duplicated minute would be stored twice.
fn append_window(
    out: &mut Vec<Bar>,
    mut bars: Vec<Bar>,
    start_ms: i64,
    end_ms: i64,
    what: &str,
) -> Result<(), SourceError> {
    bars.retain(|b| (start_ms..end_ms).contains(&b.ts));
    if let (Some(prev), Some(next)) = (out.last(), bars.first())
        && next.ts <= prev.ts
    {
        return Err(SourceError::Fetch(format!(
            "fxcm candle archive {what}: its first row ({}) is not after the previous file's last \
             ({}); two files overlap, and storing both would duplicate bars",
            next.ts, prev.ts
        )));
    }
    out.append(&mut bars);
    Ok(())
}

fn fetch_failure(url: &str, why: &str) -> SourceError {
    SourceError::Fetch(format!("fxcm candle archive {url}: {why}"))
}

/// A week (`key` = its Monday's UTC day number) or a year (`key` = the year) the archive could not
/// give, and the URLs tried with what each answered.
struct Hole {
    key: i64,
    tried: Vec<String>,
}

/// The most holes and pieces [`holes_error`] spells out; a window over a very holey stretch says how many more.
const MAX_LISTED: usize = 12;

/// Every week or year of a window with no usable file, in ONE error. ⚠ NOT "no candles": the archive
/// has holes, truncated weeks, and publishes a week only after it ends. Returned as an error so no
/// ingest can store the gap as an empty answer (module doc, "A 404 is …").
///
/// It also prints the PIECES of the window the archive does hold, as the exact `--from`/`--to` of
/// `vike-cli data hist fetch` (a date is a UTC midnight, and `--to` is an instant, not a day): a piece
/// ends on the Saturday after its last week, which is inside the closed window and so asks for no week
/// beyond it, and the next begins on the Sunday before its first, which is before the open. The window's
/// OWN ends are printed as a date only when they are UTC midnights and as epoch milliseconds otherwise
/// (`--from`/`--to` read a bare integer as epoch milliseconds), so no bound is rounded. `keys` are the
/// first and last week (Monday day numbers, a week apart) or year the window needs; `walked` says how
/// far the walk got when it gave up early.
fn holes_error(
    grain: Grain,
    (start_ms, end_ms): (i64, i64),
    keys: (i64, i64),
    walked: Walked,
    holes: &[Hole],
) -> SourceError {
    let weekly = grain != Grain::D1;
    let step = if weekly { 7 } else { 1 };
    let date = |day: i64| vike_model::time::epoch_ms_to_utc_date(day * MS_PER_DAY);
    let bound = |ms: i64| {
        if ms.rem_euclid(MS_PER_DAY) == 0 {
            vike_model::time::epoch_ms_to_utc_date(ms)
        } else {
            ms.to_string()
        }
    };
    let name = |key: i64| {
        if weekly {
            format!("the trading week of Monday {}", date(key))
        } else {
            format!("year {key}")
        }
    };
    let listed: Vec<String> = holes
        .iter()
        .take(MAX_LISTED)
        .map(|h| {
            if h.tried.is_empty() {
                format!("{} (every file it could be in holds another week)", name(h.key))
            } else {
                format!("{} (no usable file at {})", name(h.key), h.tried.join(", "))
            }
        })
        .collect();
    let more = holes.len().saturating_sub(MAX_LISTED);
    // The pieces: maximal runs of keys between the holes.
    let mut runs: Vec<(i64, i64)> = Vec::new();
    let mut key = keys.0;
    while key <= walked.last {
        if !holes.iter().any(|h| h.key == key) {
            match runs.last_mut() {
                Some((_, last)) if *last + step == key => *last = key,
                _ => runs.push((key, key)),
            }
        }
        key += step;
    }
    let pieces: Vec<String> = runs
        .into_iter()
        .map(|(a, b)| {
            let from = match (a == keys.0, weekly) {
                (true, _) => bound(start_ms),
                (false, true) => date(a - 1),
                (false, false) => format!("{a}-01-01"),
            };
            let to = match (b == keys.1, weekly) {
                (true, _) => bound(end_ms),
                (false, true) => date(b + 5),
                (false, false) => format!("{}-01-01", b + 1),
            };
            format!("--from {from} --to {to}")
        })
        .collect();
    let pieces_word = if pieces.is_empty() {
        "none: no week or year of this window is in the archive".to_string()
    } else {
        let extra = pieces.len().saturating_sub(MAX_LISTED);
        let mut shown = pieces.into_iter().take(MAX_LISTED).collect::<Vec<_>>().join("; ");
        if extra > 0 {
            shown.push_str(&format!("; and {extra} more"));
        }
        shown
    };
    let not_asked = if walked.stopped {
        let later = (keys.1 - walked.last) / step;
        let unit = if weekly { "week" } else { "year" };
        // One left over reads "the 1 later week ... was not asked", not "the 1 later weeks ... were".
        let (plural, were, are) = if later == 1 { ("", "was", "is") } else { ("s", "were", "are") };
        format!(
            ". The walk STOPPED at {} after {MAX_EMPTY_WEEKS} consecutive {unit}s with no file \
             at all: the {later} later {unit}{plural} of the window {were} not asked and {are} \
             not in the pieces below",
            name(walked.last),
        )
    } else {
        String::new()
    };
    SourceError::Fetch(format!(
        "fxcm candle archive: {} NOT AVAILABLE: {}{}{not_asked}. That is not proof that there were \
         no candles: the archive has holes and truncated weeks, and publishes a week only after it \
         ends. Nothing was stored for this window. The pieces of it the archive holds, as \
         `vike-cli data hist fetch` arguments (UTC; keep them from overlapping, because a \
         different window over the same days stores them again): {pieces_word}",
        holes.len(),
        listed.join("; "),
        if more > 0 { format!("; and {more} more") } else { String::new() }
    ))
}

/// How far a walk got: the last week (Monday day number) or year it asked, and whether it gave up
/// there (`MAX_EMPTY_WEEKS`) rather than reaching the window's end.
#[derive(Debug, Clone, Copy)]
struct Walked {
    last: i64,
    stopped: bool,
}

/// The refusal for a window that reaches a trading week the archive has not published at `now_ms`, or
/// `None`. A week is published on the MONDAY AFTER it ends (see [`PUBLISHED_FROM_MONDAY_MS`]), so the
/// first unpublished week is the one whose Monday opens the calendar week containing `now - 6h`: from
/// the Friday close to that Monday a week has ended and is still not there. No request could answer it:
/// this is every window that ends now (a CLI `--days N`, the chart seed). Said before any request
/// leaves. ⚠ The text is read in a GUI as well as on a command line: its FIRST line is neutral (no
/// flag, no tool) and its SECOND line is the command-line hint, with the Saturday to end on — the one
/// before the first unpublished week, whose own week is published, so the hint works.
fn unfinished_week_refusal(end_ms: i64, now_ms: i64) -> Option<String> {
    let day = (now_ms - PUBLISHED_FROM_MONDAY_MS).div_euclid(MS_PER_DAY);
    let current = day - weekday(day);
    // The window is `[.., end_ms)`: its last instant is `end_ms - 1`.
    if week_to(end_ms - 1) < current {
        return None;
    }
    let date = |day: i64| vike_model::time::epoch_ms_to_utc_date(day * MS_PER_DAY);
    let (hours, minutes) =
        (PUBLISHED_FROM_MONDAY_MS / HOUR_MS, PUBLISHED_FROM_MONDAY_MS % HOUR_MS / 60_000);
    Some(format!(
        "fxcm candle archive: the window ends at {}, in or after the trading week of Monday {}, \
         which FXCM has not published yet: it publishes a week on the Monday after it ends, and \
         this source counts it as there from {hours:02}:{minutes:02} UTC that Monday.\nFrom the \
         command line: end the window on or before the Saturday before \
         it, {} (`--to {}`); `--days N` windows always end now and always land here.",
        vike_model::time::epoch_ms_to_utc_date(end_ms),
        date(current),
        date(current - 2),
        date(current - 2)
    ))
}

// ------------------------------------------------------------------------------------------------
// the week walk: which file holds which week
// ------------------------------------------------------------------------------------------------

/// Finds each trading week's file WITHOUT trusting the number in its name (module doc, first trap).
struct WeekWalk<'a, W: Wire> {
    wire: &'a mut W,
    grain: Grain,
    pair: &'static str,
    /// Downloaded weeks not yet used, by the Monday their OWN rows say. A candidate that held another
    /// week lands here, and is usually the next request's answer.
    cache: BTreeMap<i64, Vec<Bar>>,
    /// Every `(year, number)` URL asked so far: `None` = answered with usable rows (their week is in
    /// `cache` or already used), `Some(why)` = unusable, and why. Nothing is asked twice.
    asked: HashMap<(i64, u32), Option<String>>,
    /// The offset from the ISO week that worked last — tried first, so a regime costs one request a week.
    offset: i32,
    /// What a file's kept rows must show to count as a week ([`Grain::week_floor`]).
    floor: WeekFloor,
}

/// What [`WeekWalk::week`] found: the week's kept rows, or the URLs tried that gave none.
#[derive(Debug)]
enum Week {
    Rows(Vec<Bar>),
    Missing(Vec<String>),
}

impl<'a, W: Wire> WeekWalk<'a, W> {
    fn new(wire: &'a mut W, grain: Grain, pair: &'static str, floor: WeekFloor) -> Self {
        Self { wire, grain, pair, cache: BTreeMap::new(), asked: HashMap::new(), offset: 0, floor }
    }

    /// The kept rows of the trading week whose Monday is the UTC day `monday`, or — in
    /// [`Week::Missing`] — every candidate URL tried and what it answered. A transport failure is the
    /// `Err`; an ABSENT week is not, so the caller can go on and find every hole in one pass.
    fn week(&mut self, monday: i64) -> Result<Week, SourceError> {
        if let Some(bars) = self.cache.remove(&monday) {
            return Ok(Week::Rows(bars));
        }
        let mut tried: Vec<String> = Vec::new();
        for (year, number, offset) in week_candidates(monday, self.offset) {
            let url =
                format!("{ARCHIVE}/{}/{}/{year}/{number}.csv.gz", self.grain.dir(), self.pair);
            match self.asked.get(&(year, number)) {
                Some(None) => continue, // its week is cached under the Monday its own rows state
                Some(Some(why)) => {
                    tried.push(format!("{url} ({why})"));
                    continue;
                }
                None => {}
            }
            let Some(body) = get_with_retries(self.wire, &url)? else {
                self.asked.insert((year, number), Some("HTTP 404".to_string()));
                tried.push(format!("{url} (HTTP 404)"));
                continue;
            };
            // What was stamped while the market was shut is not kept, so the floor judges the week.
            let bars = open_rows(
                decode(&body, self.grain).map_err(|why| fetch_failure(&url, &why))?,
                self.grain,
            );
            // A 200 is not proof of a whole week either: 2025/26 holds one Friday evening.
            if let Some(why) = self.floor.truncation(&bars) {
                tried.push(format!("{url} ({why})"));
                self.asked.insert((year, number), Some(why));
                continue;
            }
            self.asked.insert((year, number), None);
            let (Some(first), Some(last)) = (bars.first(), bars.last()) else { continue };
            // THE FILE'S OWN DATES decide which week it is.
            let actual = trading_monday(first.ts);
            // Kept rows never fall on a Saturday, so one at or after the next Sunday's midnight is
            // the next week's open: the file spans two weeks and is not a weekly file.
            if last.ts >= (actual + 6) * MS_PER_DAY {
                return Err(fetch_failure(
                    &url,
                    "its rows run past one trading week; not a weekly file",
                ));
            }
            tracing::debug!(
                target: "vike_fxcm::data",
                "{url}: holds the trading week of UTC day {actual} (asked for {monday})"
            );
            self.cache.entry(actual).or_insert(bars);
            if actual == monday {
                if let Some(offset) = offset {
                    self.offset = offset;
                }
                if let Some(bars) = self.cache.remove(&monday) {
                    return Ok(Week::Rows(bars));
                }
            }
        }
        Ok(Week::Missing(tried))
    }
}

/// `(year, number, offset)` of the URLs that may hold the trading week whose Monday is the UTC day
/// `monday`, most likely first.
///
/// * **The year-edge week, first, when it applies.** Measured: a week whose Sunday open falls in the
///   PREVIOUS calendar year but whose Monday is ISO week 1 (29 December .. 1 January) is filed under
///   the old year as number 53 — `2018/53`, `2019/53`, `2023/53` and `2025/53` — and the new year
///   has no `/1` for it (the README's own `2020/1` is that absence). `offset` is `None`: this is
///   not a drift of the ISO number and must not move [`WeekWalk`]'s memo.
/// * **The ISO week, and one either side**, `first` leading: 2020..2025 follow the ISO week of the
///   Monday (offset 0) and 2015 and 2026 are one lower (offset −1). A number outside the ISO year
///   wraps into the neighbouring year, as the ISO calendar does.
fn week_candidates(monday: i64, first: i32) -> Vec<(i64, u32, Option<i32>)> {
    let (iso_year, iso_week) = iso_week_of(monday);
    let sunday_year = civil_from_days(monday - 1).0;
    let mut out = Vec::new();
    if sunday_year != iso_year {
        out.push((sunday_year, 53, None));
    }
    let mut offsets = vec![first];
    offsets.extend([0, -1, 1].into_iter().filter(|o| *o != first));
    for off in offsets {
        let (mut year, mut number) = (iso_year, i64::from(iso_week) + i64::from(off));
        if number < 1 {
            year -= 1;
            number += i64::from(weeks_in_iso_year(year));
        } else if number > i64::from(weeks_in_iso_year(year)) {
            number -= i64::from(weeks_in_iso_year(year));
            year += 1;
        }
        out.push((year, u32::try_from(number).unwrap_or(1), Some(off)));
    }
    out
}

/// Monday = 0 .. Sunday = 6 of a UTC day number.
fn weekday(day: i64) -> i64 {
    i64::from(utc_weekday(day * MS_PER_DAY))
}

/// ISO year and week number of the week whose Monday is the UTC day `monday`.
fn iso_week_of(monday: i64) -> (i64, u32) {
    let thursday = monday + 3;
    let year = civil_from_days(thursday).0;
    (year, u32::try_from((thursday - days_from_civil(year, 1, 1)) / 7 + 1).unwrap_or(1))
}

/// How many weeks the ISO year `year` has (52 or 53): the week of 28 December, which always lies in
/// the last one.
fn weeks_in_iso_year(year: i64) -> u32 {
    let dec28 = days_from_civil(year, 12, 28);
    iso_week_of(dec28 - weekday(dec28)).1
}

/// The Monday (UTC day number) of the trading week a row at `ts` belongs to. A weekly file opens on
/// the Sunday evening before its Monday, so a Sunday belongs to the NEXT day's week.
fn trading_monday(ts: i64) -> i64 {
    let day = ts.div_euclid(MS_PER_DAY);
    match weekday(day) {
        6 => day + 1,
        w => day - w,
    }
}

/// Whether the market is shut at `ts`: from the Friday close to the Sunday open, both 17:00 New York
/// (21:00 UTC while US daylight saving is in force, else 22:00; `ny_close_utc_hour`), and all of
/// Saturday. A bar stamped here is the archive's own frozen quote, not trading: `open_rows` drops it. Where the DST rule is unknown (before 2007) a Friday or Sunday instant is called open —
/// nothing is dropped on a guess; the archive starts in 2012.
fn market_closed(ts: i64) -> bool {
    let day = ts.div_euclid(MS_PER_DAY);
    let time_of_day = ts.rem_euclid(MS_PER_DAY);
    let boundary = || ny_close_utc_hour(day).map(|hour| hour * HOUR_MS);
    match weekday(day) {
        5 => true,
        4 => boundary().is_some_and(|close| time_of_day >= close),
        6 => boundary().is_some_and(|open| time_of_day < open),
        _ => false,
    }
}

/// The Monday of the first trading week that can hold a row at or after `start_ms`: the week of the
/// instant itself, or — when it falls in the closed window before the open — the Monday after.
fn week_from(start_ms: i64) -> i64 {
    let monday = trading_monday(start_ms);
    // A Sunday before the open already maps to the NEXT Monday; a Friday evening or a Saturday does not.
    let day = start_ms.div_euclid(MS_PER_DAY);
    if market_closed(start_ms) && weekday(day) != 6 { monday + 7 } else { monday }
}

/// The Monday of the last trading week that can hold a row at or before `end_ms`: the week of the
/// instant itself, or — when it falls before the Sunday open — the week before (a Sunday morning maps to
/// the NEXT Monday, whose first row is that evening).
fn week_to(end_ms: i64) -> i64 {
    let monday = trading_monday(end_ms);
    let day = end_ms.div_euclid(MS_PER_DAY);
    if market_closed(end_ms) && weekday(day) == 6 { monday - 7 } else { monday }
}

/// The first and last Monday (UTC day numbers, a week apart from each other) of the trading weeks
/// whose files can hold a row of `[start_ms, end_ms)`, or `None` when the window lies wholly inside
/// the closed window and so holds nothing we keep. The end is EXCLUSIVE, so the last instant that can
/// hold a row is `end_ms - 1`: a window ending exactly at a Sunday open (22:00 UTC in winter, 21:00 in
/// summer) does not ask for the week that opens there, one ending at Monday 00:00 still asks for the
/// week whose Sunday evening it ends in.
fn weeks_covering(start_ms: i64, end_ms: i64) -> Option<(i64, i64)> {
    let (first, last) = (week_from(start_ms), week_to(end_ms - 1));
    (first <= last).then_some((first, last))
}

/// Drop the rows that were stamped while the market was shut (`market_closed`), and from a `1d` file the
/// bars stamped on a Saturday UTC — the stamp is 17:00 New York, so a Saturday bar spans the closed
/// weekend, Saturday 17:00 to Sunday 17:00 New York (51 in the 2020 file). ⚠ Those are the WEEKEND GAP,
/// not slivers: each opens at the previous session's close and closes at the next (Sunday-stamped)
/// session's open, to the digit, so the range is the gap's own path (bid side: 17.6 pips on average, up
/// to 71.4). Dropping them keeps the Friday-to-Monday jump visible between the kept bars (the shape
/// OANDA's daily candles have) and no session loses its own OHLC, but the gap's extremes are not kept.
/// The Sunday-stamped bar is Monday's session and stays.
fn open_rows(mut bars: Vec<Bar>, grain: Grain) -> Vec<Bar> {
    bars.retain(|bar| match grain {
        Grain::D1 => weekday(bar.ts.div_euclid(MS_PER_DAY)) != 5,
        Grain::M1 | Grain::H1 => !market_closed(bar.ts),
    });
    bars
}

// ------------------------------------------------------------------------------------------------
// the file: gzip, CSV, mid
// ------------------------------------------------------------------------------------------------

/// Decompress and parse one file. `Err` is the reason, in words, for the caller to attach the URL to.
fn decode(body: &[u8], grain: Grain) -> Result<Vec<Bar>, String> {
    decode_capped(body, grain, MAX_DECODED_BYTES)
}

/// [`decode`] with the decompression cap a parameter, so a test can reach it without a 64 MiB file.
fn decode_capped(body: &[u8], grain: Grain, cap: u64) -> Result<Vec<Bar>, String> {
    // A 200 with NO bytes (`H1/EURUSD/2024/51` and `/52`, Content-Length 0) is a week the archive
    // published empty, like the header-only `2023/27`: no rows, so the callers' floors report a hole
    // and the walk goes on. A gzip with SOME bytes that cannot be read is a damaged transfer or a
    // changed format, and stays an error below.
    if body.is_empty() {
        return Ok(Vec::new());
    }
    let mut text = String::new();
    GzDecoder::new(body)
        .take(cap + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("not a readable gzip text file ({e})"))?;
    if text.len() as u64 > cap {
        return Err(format!("decompresses past {cap} bytes"));
    }
    parse_csv(&text, grain)
}

/// The mid bars of one file. Strict: a drifted header, a row that is not nine fields, a price that
/// is not a positive finite number, a time that is not strictly after the previous row's, and (D1) a
/// stamp that is not 17:00 New York each fail the WHOLE file, naming the row — a half-read file would
/// be stored under a spent commit key. Whitespace around a price is the one thing forgiven.
fn parse_csv(text: &str, grain: Grain) -> Result<Vec<Bar>, String> {
    let mut lines = text.lines();
    match lines.next() {
        Some(HEADER) => {}
        other => {
            return Err(format!("unexpected header {:?} (want {HEADER:?})", other.unwrap_or("")));
        }
    }
    let mut bars: Vec<Bar> = Vec::new();
    for (n, line) in lines.enumerate().filter(|(_, l)| !l.is_empty()) {
        let row = n + 2;
        let fields: Vec<&str> = line.split(',').collect();
        let [stamp, prices @ ..] = fields.as_slice() else { unreachable!("split yields one") };
        if prices.len() != 8 {
            return Err(format!("row {row}: {} fields, want 9", prices.len() + 1));
        }
        let ts = parse_stamp(stamp).ok_or_else(|| format!("row {row}: bad time {stamp:?}"))?;
        let mut p = [0.0f64; 8];
        for (slot, raw) in p.iter_mut().zip(prices) {
            // The 2019 `D1` files pad every price with a leading space (`, 109.68400`).
            *slot = raw
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or_else(|| format!("row {row}: bad price {raw:?}"))?;
        }
        if bars.last().is_some_and(|prev| ts <= prev.ts) {
            return Err(format!("row {row}: time {stamp:?} is not after the previous row's"));
        }
        if grain == Grain::D1 && !is_new_york_close(ts) {
            return Err(format!(
                "row {row}: D1 stamp {stamp:?} is not 17:00 New York (21:00 UTC in US summer \
                 time, 22:00 otherwise); the file's convention changed or it predates 2007"
            ));
        }
        let mid = |bid: f64, ask: f64| (bid + ask) / 2.0;
        bars.push(Bar {
            ts,
            open: mid(p[0], p[4]),
            high: mid(p[1], p[5]),
            low: mid(p[2], p[6]),
            close: mid(p[3], p[7]),
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    Ok(bars)
}

/// `MM/DD/YYYY HH:MM:SS.mmm` (UTC, American order) → epoch milliseconds, or `None` for anything
/// else — including a date no calendar holds.
fn parse_stamp(s: &str) -> Option<i64> {
    let (date, time) = s.split_once(' ')?;
    let (hms, millis) = time.split_once('.')?;
    let num = |t: &str| {
        (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            .then(|| t.parse::<i64>().ok())
            .flatten()
    };
    let mut d = date.split('/');
    let (month, day, year) = (num(d.next()?)?, num(d.next()?)?, num(d.next()?)?);
    let mut t = hms.split(':');
    let (hour, minute, second) = (num(t.next()?)?, num(t.next()?)?, num(t.next()?)?);
    let millis = if millis.len() == 3 { num(millis)? } else { return None };
    if d.next().is_some() || t.next().is_some() || !(1970..=2200).contains(&year) {
        return None;
    }
    let days = days_from_civil(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?);
    // A date the calendar does not hold (31 February) rolls over; the round trip catches it.
    if civil_from_days(days) != (year, u32::try_from(month).ok()?, u32::try_from(day).ok()?) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some(days * MS_PER_DAY + ((hour * 60 + minute) * 60 + second) * 1000 + millis)
}

/// The UTC hour of 17:00 New York on the UTC calendar day `day`: 21 while US daylight saving is in
/// force (from the second Sunday of March, inclusive, to the first Sunday of November, exclusive —
/// 17:00 is after the 02:00 switch on both days), else 22. `None` before 2007, whose rule differed.
fn ny_close_utc_hour(day: i64) -> Option<i64> {
    let (year, month, dom) = civil_from_days(day);
    if year < 2007 {
        return None;
    }
    let first_sunday =
        |month: u32| 1 + (6 - weekday(days_from_civil(year, month, 1))).rem_euclid(7);
    let summer = match month {
        4..=10 => true,
        3 => i64::from(dom) >= first_sunday(3) + 7,
        11 => i64::from(dom) < first_sunday(11),
        _ => false,
    };
    Some(if summer { 21 } else { 22 })
}

/// Whether `ts` is exactly 17:00 New York on its UTC day.
fn is_new_york_close(ts: i64) -> bool {
    ny_close_utc_hour(ts.div_euclid(MS_PER_DAY))
        .is_some_and(|hour| ts.rem_euclid(MS_PER_DAY) == hour * 3_600_000)
}

// ------------------------------------------------------------------------------------------------
// the wire: one request, its retry policy, and everything they touch outside the process
// ------------------------------------------------------------------------------------------------

/// What one HTTP attempt came back with — the seam [`get_with_retries`] retries over.
enum Attempt {
    /// The server answered: its status, the body (read only for a 2xx) and the wait its
    /// `Retry-After` stated in seconds.
    Response { status: u16, body: Vec<u8>, retry_after: Option<Duration> },
    /// No usable answer — DNS, connect, TLS, a reset, a timeout, a body cut off mid-read.
    Failed(String),
}

/// Everything a fetch does to the outside world, so the walk and the retry policy above it run with
/// neither a network nor a clock.
trait Wire {
    /// Take one slot of the process-wide request budget; blocks while it is spent.
    fn pace(&mut self);
    /// One GET of `url`. No retry.
    fn get(&mut self, url: &str) -> Attempt;
    /// Wait `wait`.
    fn sleep(&mut self, wait: Duration);
}

/// The real wire: the process-wide gate, a connection pool of its own and the thread clock.
struct LiveWire {
    agent: ureq::Agent,
    gate: &'static RateGate,
}

impl LiveWire {
    fn new() -> Self {
        static GATE: OnceLock<RateGate> = OnceLock::new();
        let gate =
            GATE.get_or_init(|| RateGate::new(MAX_REQUESTS_PER_SECOND, Duration::from_secs(1)));
        Self { agent: blocking_agent(), gate }
    }
}

impl Wire for LiveWire {
    fn pace(&mut self) {
        self.gate.proceed_logged(VENUE, "candle archive");
    }

    fn get(&mut self, url: &str) -> Attempt {
        match self.agent.get(url).call() {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let mut body = Vec::new();
                if (200..300).contains(&status)
                    && let Err(e) =
                        resp.body_mut().as_reader().take(MAX_BODY_BYTES + 1).read_to_end(&mut body)
                {
                    return Attempt::Failed(format!("read: {e}"));
                }
                Attempt::Response { status, body, retry_after }
            }
            Err(e) => Attempt::Failed(format!("network: {e}")),
        }
    }

    fn sleep(&mut self, wait: Duration) {
        std::thread::sleep(wait);
    }
}

/// One URL with the retry policy: pace, attempt, and on a transient failure wait out the next of
/// [`RETRY_DELAYS`] — lengthened, never shortened, to a `Retry-After` up to [`RETRY_AFTER_CAP`] —
/// and try again. `Ok(None)` is a 404 (never retried: the archive has holes, and a retry will not
/// fill one); every other non-2xx status fails at once.
fn get_with_retries(wire: &mut impl Wire, url: &str) -> Result<Option<Vec<u8>>, SourceError> {
    let mut last = String::new();
    for delay in RETRY_DELAYS.iter().map(Some).chain(std::iter::once(None)) {
        wire.pace();
        let mut stated = Duration::ZERO;
        match wire.get(url) {
            Attempt::Response { status: 404, .. } => return Ok(None),
            Attempt::Response { status, body, .. } if (200..300).contains(&status) => {
                if body.len() as u64 > MAX_BODY_BYTES {
                    return Err(fetch_failure(url, &format!("more than {MAX_BODY_BYTES} bytes")));
                }
                return Ok(Some(body));
            }
            Attempt::Response { status, retry_after, .. } => {
                if !matches!(status, 429 | 500 | 502 | 503 | 504) {
                    return Err(fetch_failure(url, &format!("HTTP {status}")));
                }
                last = format!("HTTP {status}");
                stated = retry_after.unwrap_or(Duration::ZERO).min(RETRY_AFTER_CAP);
            }
            Attempt::Failed(why) => last = why,
        }
        if let Some(delay) = delay {
            wire.sleep((*delay).max(stated));
        }
    }
    Err(fetch_failure(url, &format!("{last} (after {} retries)", RETRY_DELAYS.len())))
}
