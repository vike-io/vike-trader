//! Tests of the FXCM candle-archive source. NO NETWORK: the wire is a fake, and the files are REAL
//! excerpts of the archive (`tests/fixtures/fxcm_history/`, fetched once on 2026-10-20: the first 360
//! rows of five weekly `m1` files and the whole of an `H1` week and a `D1` year, gzip as served).
//!
//! The README's own example, `m1/EURUSD/2020/1`, answers 404 and is not a fixture here: it is the 404
//! case of `a_week_with_no_file_is_not_available_and_never_an_empty_answer`.

use std::assert_matches;
use std::collections::{HashMap, VecDeque};

use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write as _;

use super::*;

const M1_2012_04_TAIL: &[u8] =
    include_bytes!("../tests/fixtures/fxcm_history/m1_eurusd_2012_04_tail.csv.gz");
const M1_2025_10: &[u8] = include_bytes!("../tests/fixtures/fxcm_history/m1_eurusd_2025_10.csv.gz");
const M1_2025_11: &[u8] = include_bytes!("../tests/fixtures/fxcm_history/m1_eurusd_2025_11.csv.gz");
const M1_2025_26_TRUNCATED: &[u8] =
    include_bytes!("../tests/fixtures/fxcm_history/m1_eurusd_2025_26_truncated.csv.gz");
const M1_2025_53: &[u8] = include_bytes!("../tests/fixtures/fxcm_history/m1_eurusd_2025_53.csv.gz");
const M1_2026_40: &[u8] = include_bytes!("../tests/fixtures/fxcm_history/m1_eurusd_2026_40.csv.gz");
const M1_2026_41: &[u8] = include_bytes!("../tests/fixtures/fxcm_history/m1_eurusd_2026_41.csv.gz");
const M1_USDJPY_2025_10: &[u8] =
    include_bytes!("../tests/fixtures/fxcm_history/m1_usdjpy_2025_10.csv.gz");
const H1_2026_40: &[u8] = include_bytes!("../tests/fixtures/fxcm_history/h1_eurusd_2026_40.csv.gz");
const H1_2024_18: &[u8] = include_bytes!("../tests/fixtures/fxcm_history/h1_eurusd_2024_18.csv.gz");
const D1_2020: &[u8] = include_bytes!("../tests/fixtures/fxcm_history/d1_eurusd_2020.csv.gz");

/// Epoch milliseconds of a UTC instant.
fn at(year: i64, month: u32, day: u32, hour: i64, minute: i64) -> i64 {
    (days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60) * 1000
}

/// The UTC day number of a date.
fn day(year: i64, month: u32, d: u32) -> i64 {
    days_from_civil(year, month, d)
}

/// A `now` after every window a test asks for, so the unfinished-week refusal stays out of the way of
/// the tests that are not about it.
const LATER: i64 = 1 << 50;

/// What an EXCERPT counts as a week with: the fixtures are the first 360 rows of real weeks, six hours
/// long, far under [`Grain::week_floor`], which the truncated-week tests use at full strength.
const EXCERPT: WeekFloor = WeekFloor { min_rows: 1, min_span_ms: 0, whole_week_reaches_ms: 0 };

/// A fetch at full strength (the interval's own floor), with `now` far off.
fn fetch_over(
    wire: &mut impl Wire,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, SourceError> {
    fetch_with(wire, symbol, interval, (start_ms, end_ms), LATER, None)
}

/// [`fetch_over`] for a test whose files are short excerpts of real weeks.
fn fetch_excerpts(
    wire: &mut impl Wire,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, SourceError> {
    fetch_with(wire, symbol, interval, (start_ms, end_ms), LATER, Some(EXCERPT))
}

/// A full-strength fetch at a given `now`.
fn fetch_at(
    now_ms: i64,
    wire: &mut impl Wire,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, SourceError> {
    fetch_with(wire, symbol, interval, (start_ms, end_ms), now_ms, None)
}

/// A whole synthetic week as a gzip file: a row every `step_ms` from `from_ms` to `to_ms`, with prices
/// that vary row to row and bid below ask. For the floor and hole tests, which need weeks of the right
/// SHAPE (span, row count) and not of the right prices; the real files are the fixtures.
fn synthetic_week(from_ms: i64, to_ms: i64, step_ms: i64) -> Vec<u8> {
    let mut text = format!("{HEADER}\r\n");
    let mut ts = from_ms;
    let mut n = 0u32;
    while ts <= to_ms {
        let bid = 1.1 + f64::from(n % 50) * 0.0001;
        let line = format!(
            "{},{:.5},{:.5},{:.5},{:.5},{:.5},{:.5},{:.5},{:.5}\r\n",
            stamp(ts),
            bid,
            bid + 0.0002,
            bid - 0.0002,
            bid + 0.0001,
            bid + 0.0003,
            bid + 0.0005,
            bid + 0.0001,
            bid + 0.0004
        );
        text.push_str(&line);
        ts += step_ms;
        n += 1;
    }
    gz(&text)
}

/// `MM/DD/YYYY HH:MM:SS.000` of a UTC instant, the archive's spelling.
fn stamp(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(MS_PER_DAY));
    let s = ms.rem_euclid(MS_PER_DAY) / 1000;
    format!("{m:02}/{d:02}/{y:04} {:02}:{:02}:{:02}.000", s / 3600, s % 3600 / 60, s % 60)
}

fn weekly_url(dir: &str, pair: &str, year: i64, number: u32) -> String {
    format!("{ARCHIVE}/{dir}/{pair}/{year}/{number}.csv.gz")
}

fn parsed(gz: &[u8], grain: Grain) -> Vec<Bar> {
    decode(gz, grain).unwrap_or_else(|e| panic!("a committed fixture must decode: {e}"))
}

fn gz(text: &str) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(text.as_bytes()).unwrap();
    enc.finish().unwrap()
}

/// A wire that serves `files` by URL (anything else is a 404) and remembers every request.
#[derive(Default)]
struct FakeWire {
    files: HashMap<String, Vec<u8>>,
    asked: Vec<String>,
}

impl FakeWire {
    fn with(files: &[(String, &[u8])]) -> Self {
        Self {
            files: files.iter().map(|(u, b)| (u.clone(), b.to_vec())).collect(),
            asked: Vec::new(),
        }
    }
}

impl Wire for FakeWire {
    fn pace(&mut self) {}
    fn get(&mut self, url: &str) -> Attempt {
        self.asked.push(url.to_string());
        match self.files.get(url) {
            Some(body) => Attempt::Response { status: 200, body: body.clone(), retry_after: None },
            None => Attempt::Response { status: 404, body: Vec::new(), retry_after: None },
        }
    }
    fn sleep(&mut self, _: Duration) {}
}

/// A wire that answers a script, one answer per attempt, and records every wait.
struct ScriptWire {
    script: VecDeque<Attempt>,
    attempts: usize,
    waits: Vec<Duration>,
}

impl ScriptWire {
    fn new(script: Vec<Attempt>) -> Self {
        Self { script: script.into(), attempts: 0, waits: Vec::new() }
    }
}

impl Wire for ScriptWire {
    fn pace(&mut self) {}
    fn get(&mut self, _: &str) -> Attempt {
        self.attempts += 1;
        self.script.pop_front().expect("the policy attempted more often than the script answers")
    }
    fn sleep(&mut self, wait: Duration) {
        self.waits.push(wait);
    }
}

fn status(status: u16) -> Attempt {
    Attempt::Response { status, body: Vec::new(), retry_after: None }
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

// ── what a file becomes ─────────────────────────────────────────────────────────────────────────

/// **The side convention: the MID, per price.** The first row of a real week, computed by hand from
/// the CSV line `03/02/2025 22:00:00.000,1.04025,1.04025,1.04018,1.04018,1.04075,1.04075,1.04067,
/// 1.04067`. Volume is the codec's "none" and the store-dropped `bid`/`ask` stay `None`.
#[test]
fn a_winter_week_head_is_stored_as_mid_bars_with_no_volume() {
    let bars = parsed(M1_2025_10, Grain::M1);
    assert_eq!(bars.len(), 360);
    let first = &bars[0];
    assert_eq!(first.ts, at(2025, 3, 2, 22, 0));
    assert_eq!(first.open, (1.04025 + 1.04075) / 2.0);
    assert_eq!(first.high, (1.04025 + 1.04075) / 2.0);
    assert_eq!(first.low, (1.04018 + 1.04067) / 2.0);
    assert_eq!(first.close, (1.04018 + 1.04067) / 2.0);
    assert_eq!(first.volume, 0.0);
    assert!(first.bid.is_none() && first.ask.is_none() && first.funding.is_none());
    assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts), "strictly increasing");
}

/// **The column mapping, on a row where every price differs.** The first row of a file has open equal
/// to high and low equal to close on both sides, so a swapped mapping reads the same there and the test
/// above cannot see it. This is row 6 of the same real file, `03/02/2025 22:05:00.000,1.04037,1.0404,
/// 1.04019,1.04021,1.04055,1.04056,1.04048,1.0405` — bid O/H/L/C then ask O/H/L/C, all eight distinct —
/// and ALL FOUR mids are asserted, so any permutation of the O/H/L/C columns, or of bid against ask,
/// moves at least one of them. (Shown red against a source that swaps high and low: the lane run in the
/// report.)
#[test]
fn every_price_of_a_row_where_all_eight_differ_lands_in_its_own_column() {
    let bars = parsed(M1_2025_10, Grain::M1);
    let row = &bars[5];
    assert_eq!(row.ts, at(2025, 3, 2, 22, 5));
    assert_eq!(row.open, (1.04037 + 1.04055) / 2.0, "open is bid open + ask open");
    assert_eq!(row.high, (1.0404 + 1.04056) / 2.0, "high is bid high + ask high");
    assert_eq!(row.low, (1.04019 + 1.04048) / 2.0, "low is bid low + ask low");
    assert_eq!(row.close, (1.04021 + 1.0405) / 2.0, "close is bid close + ask close");
    assert!(row.low < row.open && row.open < row.high && row.low < row.close);
}

/// A JPY pair keeps its three decimals (the mid of two 3-decimal prices), and a row whose bid side is
/// all four prices apart and whose ask side differs from its bid: row 4 of the real USDJPY file,
/// `03/02/2025 22:10:00.000,150.673,150.684,150.646,150.683,150.737,150.737,150.714,150.724`.
#[test]
fn a_yen_pair_is_parsed_with_its_own_decimals_in_the_right_columns() {
    let bars = parsed(M1_USDJPY_2025_10, Grain::M1);
    assert_eq!(bars[0].ts, at(2025, 3, 2, 22, 7));
    assert_eq!(bars[0].open, (150.728 + 150.777) / 2.0);
    assert_eq!(bars[0].close, (150.685 + 150.77) / 2.0);
    let row = &bars[3];
    assert_eq!(row.ts, at(2025, 3, 2, 22, 10));
    assert_eq!(row.open, (150.673 + 150.737) / 2.0);
    assert_eq!(row.high, (150.684 + 150.737) / 2.0);
    assert_eq!(row.low, (150.646 + 150.714) / 2.0);
    assert_eq!(row.close, (150.683 + 150.724) / 2.0);
}

/// The weekly open moves with US summer time (22:00 UTC, then 21:xx after 9 March 2025), and the
/// parser reads the file's own UTC stamps without a calendar rule of its own.
#[test]
fn the_weekly_open_moves_with_us_summer_time_and_the_stamps_are_taken_as_utc() {
    assert_eq!(parsed(M1_2025_10, Grain::M1)[0].ts, at(2025, 3, 2, 22, 0));
    assert_eq!(parsed(M1_2025_11, Grain::M1)[0].ts, at(2025, 3, 9, 21, 6));
}

#[test]
fn an_h1_week_has_hour_marks() {
    let bars = parsed(H1_2026_40, Grain::H1);
    assert_eq!(bars.len(), 120);
    assert_eq!(bars[0].ts, at(2026, 10, 4, 21, 0));
    assert!(bars.iter().all(|b| b.ts % 3_600_000 == 0));
}

// ── trap (c): D1 stamps are 17:00 New York ─────────────────────────────────────────────────────

/// A real D1 year holds both seasons AND both switch weekends, and every stamp passes the rule:
/// 22:00 UTC in winter, 21:00 UTC in US summer time — including the Saturday 7 March stamp (still
/// winter) and the Sunday 8 March one (summer began that day).
#[test]
fn d1_stamps_are_17_00_new_york_in_both_seasons() {
    let bars = parsed(D1_2020, Grain::D1);
    assert_eq!(bars.len(), 311);
    let has = |ts| bars.iter().any(|b| b.ts == ts);
    assert!(has(at(2020, 1, 2, 22, 0)), "a January stamp is 22:00 UTC");
    assert!(has(at(2020, 7, 14, 21, 0)), "a July stamp is 21:00 UTC");
    assert!(has(at(2020, 3, 7, 22, 0)) && has(at(2020, 3, 8, 21, 0)), "the March switch");
    assert!(has(at(2020, 10, 31, 21, 0)) && has(at(2020, 11, 1, 22, 0)), "the November switch");
    assert!(bars.iter().all(|b| is_new_york_close(b.ts)));
}

#[test]
fn new_york_17_00_is_21_utc_in_summer_time_and_22_otherwise() {
    let hour = |y, m, d| ny_close_utc_hour(day(y, m, d));
    // 2020: summer time from Sunday 8 March to Saturday 31 October.
    assert_eq!(hour(2020, 3, 7), Some(22));
    assert_eq!(hour(2020, 3, 8), Some(21));
    assert_eq!(hour(2020, 10, 31), Some(21));
    assert_eq!(hour(2020, 11, 1), Some(22));
    // 2021: from Sunday 14 March to Saturday 6 November.
    assert_eq!(hour(2021, 3, 13), Some(22));
    assert_eq!(hour(2021, 3, 14), Some(21));
    assert_eq!(hour(2021, 11, 6), Some(21));
    assert_eq!(hour(2021, 11, 7), Some(22));
    assert_eq!(hour(2020, 1, 15), Some(22));
    assert_eq!(hour(2020, 7, 15), Some(21));
    // Before 2007 the US rule differed, so nothing is claimed.
    assert_eq!(hour(2006, 7, 1), None);
}

/// A D1 row at any other instant — the winter hour in July, or midnight — fails the whole file: a
/// file whose convention changed is refused, not stored shifted.
#[test]
fn a_d1_stamp_at_another_instant_refuses_the_file() {
    let prices = "1.1,1.2,1.0,1.1,1.1,1.2,1.0,1.1";
    for stamp in ["07/14/2020 22:00:00.000", "07/14/2020 00:00:00.000", "01/02/2020 21:00:00.000"] {
        let err =
            parse_csv(&format!("{HEADER}\r\n{stamp},{prices}\r\n"), Grain::D1).expect_err(stamp);
        assert!(err.contains("17:00 New York"), "{stamp}: {err}");
    }
    // ...and the same stamp is fine in an m1 file, which has no such rule.
    assert!(
        parse_csv(&format!("{HEADER}\r\n07/14/2020 22:00:00.000,{prices}\r\n"), Grain::M1).is_ok()
    );
}

/// **The `1d` Saturday-stamped bars are the WEEKEND GAP, and they are dropped knowing that.** The real
/// 2020 file has 311 rows, 51 of them stamped on a Saturday. They are not slivers: each OPENS at the
/// previous bar's close and CLOSES at the next bar's open (equal to the digit, so exactly), and their
/// range is the gap's own path — 48 of the 51 are over five pips wide on the mid and the widest is
/// 70.7 pips (2020-03-07, 1.12871 to 1.13578). Dropping them leaves 260 bars, none on a Saturday; the
/// Friday-to-Monday jump stays visible between the kept bars (the Sunday-stamped bar opens 70+ pips
/// from the Friday-session close), and what is lost is the gap's own extremes: the 2020-03-14 bar's low
/// is below the next session's low. The Sunday-stamped bar is Monday's session and stays.
#[test]
fn the_weekend_gap_bars_stamped_on_saturday_are_dropped_and_the_jump_stays_visible() {
    let raw = parsed(D1_2020, Grain::D1);
    let is_saturday = |b: &Bar| weekday(b.ts.div_euclid(MS_PER_DAY)) == 5;
    let saturdays: Vec<usize> = (0..raw.len()).filter(|&i| is_saturday(&raw[i])).collect();
    assert_eq!((raw.len(), saturdays.len()), (311, 51));

    // They are the gap: open == the previous bar's close, close == the next bar's open, in all 51.
    for &i in &saturdays {
        assert_eq!(
            raw[i].open,
            raw[i - 1].close,
            "{}: opens at the Friday session's close",
            raw[i].ts
        );
        assert_eq!(
            raw[i].close,
            raw[i + 1].open,
            "{}: closes at the next session's open",
            raw[i].ts
        );
    }
    // ...and their range is the gap, not nothing: 48 over five pips, the widest 70.7.
    let range = |i: usize| raw[i].high - raw[i].low;
    assert_eq!(saturdays.iter().filter(|&&i| range(i) > 0.0005).count(), 48);
    let widest = saturdays.iter().copied().max_by(|&a, &b| range(a).total_cmp(&range(b))).unwrap();
    assert_eq!(raw[widest].ts, at(2020, 3, 7, 22, 0));
    assert_eq!(raw[widest].low, (1.12855 + 1.12887) / 2.0);
    assert_eq!(raw[widest].high, (1.13569 + 1.13587) / 2.0);
    assert!((0.0070..0.0071).contains(&range(widest)), "{}", range(widest));

    let url = format!("{ARCHIVE}/D1/EURUSD/2020.csv.gz");
    let mut wire = FakeWire::with(&[(url.clone(), D1_2020)]);
    let bars =
        fetch_over(&mut wire, "EURUSD", "1d", at(2020, 1, 1, 0, 0), at(2021, 1, 1, 0, 0)).unwrap();
    assert_eq!(wire.asked, [url]);
    assert_eq!(bars.len(), 260);
    assert!(bars.iter().all(|b| weekday(b.ts.div_euclid(MS_PER_DAY)) != 5));
    assert!(bars.iter().any(|b| b.ts == at(2020, 1, 5, 22, 0)), "the Sunday-stamped bar stays");
    assert!(!bars.iter().any(|b| b.ts == at(2020, 1, 4, 22, 0)), "the Saturday gap bar goes");

    // The jump is still there between the kept bars of 2020-03-06/08: the Friday-session close and the
    // next session's open are 70+ pips apart.
    let at_ts = |ts: i64| bars.iter().find(|b| b.ts == ts).expect("a kept bar");
    let thursday_stamped = at_ts(at(2020, 3, 5, 22, 0));
    let sunday_stamped = at_ts(at(2020, 3, 8, 21, 0));
    assert!(sunday_stamped.open - thursday_stamped.close > 0.0065);

    // What the drop costs: the 2020-03-14 gap bar's low is below the next kept session's low.
    let march_14 = raw.iter().find(|b| b.ts == at(2020, 3, 14, 21, 0)).expect("the gap bar");
    let next_kept = at_ts(at(2020, 3, 15, 21, 0));
    assert!(march_14.low < next_kept.low, "the gap's own extreme is not kept");
}

/// A yearly file keeping fewer than `D1_MIN_ROWS` bars is a truncated year, not stored as the year —
/// and the real full year, right above it, passes. Both directions.
#[test]
fn a_1d_year_is_judged_by_the_bars_it_keeps() {
    let full = gz_text(D1_2020);
    let head: String = full.lines().take(151).map(|l| format!("{l}\r\n")).collect();
    let url = format!("{ARCHIVE}/D1/EURUSD/2020.csv.gz");
    let mut wire = FakeWire::with(&[(url.clone(), gz(&head).as_slice())]);
    let err = fetch_over(&mut wire, "EURUSD", "1d", at(2020, 1, 1, 0, 0), at(2021, 1, 1, 0, 0))
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("NOT AVAILABLE") && text.contains("a truncated year"), "{text}");
    assert!(text.contains("year 2020"), "{text}");
    // (150 raw rows keep at most 150 bars, under the floor; the whole real year keeps 260 and passes
    // in the test above.)
}

/// A file's bytes as text, to cut a real fixture down.
fn gz_text(bytes: &[u8]) -> String {
    let mut text = String::new();
    GzDecoder::new(bytes).read_to_string(&mut text).expect("a fixture is gzip text");
    text
}

// ── rows stamped while the market is shut ────────────────────────────────────────────────────────

/// The closed window is Friday 17:00 New York to Sunday 17:00 New York — 21:00 UTC in US summer time,
/// 22:00 otherwise — and all of Saturday. Each edge is tested one minute either side, in both seasons
/// and on the weekends the clocks change.
#[test]
fn the_market_is_shut_from_the_friday_close_to_the_sunday_open_in_both_seasons() {
    let shut = |y, mo, d, h, mi| market_closed(at(y, mo, d, h, mi));
    // Summer (2025-07): Friday 11th closes 21:00, Sunday 13th opens 21:00.
    assert!(!shut(2025, 7, 11, 20, 59) && shut(2025, 7, 11, 21, 0));
    assert!(shut(2025, 7, 13, 20, 59) && !shut(2025, 7, 13, 21, 0));
    // Winter (2025-01): Friday 10th closes 22:00, Sunday 12th opens 22:00.
    assert!(!shut(2025, 1, 10, 21, 59) && shut(2025, 1, 10, 22, 0));
    assert!(shut(2025, 1, 12, 21, 59) && !shut(2025, 1, 12, 22, 0));
    // The Friday before the March switch is still winter, the Sunday of it is summer already.
    assert!(!shut(2025, 3, 7, 21, 59) && shut(2025, 3, 7, 22, 0));
    assert!(shut(2025, 3, 9, 20, 59) && !shut(2025, 3, 9, 21, 0));
    // The Friday before the November switch is still summer, the Sunday of it is winter already.
    assert!(!shut(2025, 10, 31, 20, 59) && shut(2025, 10, 31, 21, 0));
    assert!(shut(2025, 11, 2, 21, 59) && !shut(2025, 11, 2, 22, 0));
    // All of Saturday, and the open week around it.
    assert!(shut(2025, 7, 12, 0, 0) && shut(2025, 7, 12, 23, 59));
    assert!(!shut(2025, 7, 9, 3, 0) && !shut(2025, 7, 14, 0, 0));
    // Before the DST rule is known (2007) a Friday or Sunday instant is left alone; a Saturday is not.
    assert!(!market_closed(at(2006, 7, 7, 23, 0)) && market_closed(at(2006, 7, 8, 12, 0)));
}

/// The real `1h` week that ends on a Saturday: 122 rows, the last two stamped Saturday 08:00 and 09:00
/// UTC (flat bars, a 6.6-pip spread). The 120 rows of the open week remain, the last one Friday 20:00
/// (summer: the close is 21:00), and the two flat bars are not stored.
#[test]
fn the_saturday_rows_of_a_real_week_are_dropped() {
    let raw = parsed(H1_2024_18, Grain::H1);
    assert_eq!((raw.len(), raw[121].ts), (122, at(2024, 5, 4, 9, 0)));
    let kept = open_rows(raw, Grain::H1);
    assert_eq!(kept.len(), 120);
    assert_eq!(kept[119].ts, at(2024, 5, 3, 20, 0));
    assert!(kept.iter().all(|b| !market_closed(b.ts)));

    let url = weekly_url("H1", "EURUSD", 2024, 18);
    let mut wire = FakeWire::with(&[(url.clone(), H1_2024_18)]);
    let week = fetch_over(&mut wire, "EURUSD", "1h", at(2024, 4, 28, 21, 0), at(2024, 5, 4, 23, 0))
        .unwrap();
    assert_eq!((week.len(), week[119].ts), (120, at(2024, 5, 3, 20, 0)));
}

/// The truncated `2025/26`'s rows to Saturday 21:00 are shut-market rows too: what is kept ends at the
/// Friday close, whatever the file claims.
#[test]
fn a_1m_week_loses_its_friday_close_and_saturday_rows() {
    let raw = parsed(M1_2025_26_TRUNCATED, Grain::M1);
    let kept = open_rows(raw.clone(), Grain::M1);
    assert!(kept.len() < raw.len() && !kept.is_empty());
    assert!(
        kept.iter().all(|b| b.ts < at(2025, 6, 27, 21, 0)),
        "summer: the Friday close is 21:00"
    );
    assert_eq!(raw.last().map(|b| b.ts), Some(at(2025, 6, 28, 21, 0)));
}

/// **The 2012 close-stamp rule has its evidence in the repo.** The tail of the real `m1/EURUSD/2012/4`
/// (its last twenty rows, fetched 2026-10-20) ends with a bar stamped Friday 2012-01-27 22:00:00 UTC —
/// exactly the winter close, 17:00 New York — one minute after the last bar of the open week (21:59).
/// The map run found the same last bar in `m1` and `H1` of weeks 4 and 5 of 2012 (four files); this
/// excerpt is the one committed. The bar opens when the market has just shut, and is not kept.
#[test]
fn a_bar_stamped_exactly_at_the_friday_close_is_not_kept() {
    let raw = parsed(M1_2012_04_TAIL, Grain::M1);
    assert_eq!((raw.len(), raw[0].ts), (20, at(2012, 1, 27, 21, 41)));
    assert_eq!(raw[19].ts, at(2012, 1, 27, 22, 0), "the last row is stamped at the close");
    assert!(market_closed(raw[19].ts) && !market_closed(raw[18].ts));
    let kept = open_rows(raw, Grain::M1);
    assert_eq!((kept.len(), kept[18].ts), (19, at(2012, 1, 27, 21, 59)));
}

/// A window wholly inside the closed window holds nothing we keep, so it needs no file and is an empty
/// `Ok` (the one empty answer this source gives, and a true one: nothing there is ever stored).
#[test]
fn a_window_inside_the_closed_window_needs_no_file() {
    let mut wire = FakeWire::default();
    for (start, end) in [
        (at(2025, 3, 8, 0, 0), at(2025, 3, 8, 23, 59)), // a Saturday
        (at(2025, 3, 7, 23, 0), at(2025, 3, 9, 20, 59)), // Friday after the winter close to Sunday before the summer open
    ] {
        let bars = fetch_over(&mut wire, "EURUSD", "1m", start, end).unwrap();
        assert!(bars.is_empty());
    }
    assert!(wire.asked.is_empty());
}

// ── trap (a): the week number in the URL ───────────────────────────────────────────────────────

#[test]
fn iso_weeks_of_known_mondays() {
    assert_eq!(iso_week_of(day(2020, 1, 6)), (2020, 2));
    assert_eq!(iso_week_of(day(2021, 1, 4)), (2021, 1));
    assert_eq!(iso_week_of(day(2024, 12, 30)), (2025, 1));
    assert_eq!(iso_week_of(day(2025, 3, 3)), (2025, 10));
    assert_eq!(iso_week_of(day(2026, 10, 12)), (2026, 42));
    assert_eq!(iso_week_of(day(2020, 12, 28)), (2020, 53));
    assert_eq!((weeks_in_iso_year(2025), weeks_in_iso_year(2026)), (52, 53));
}

/// The candidates are the ISO week and its neighbours, the last good offset first, and a number
/// outside the ISO year wraps into the neighbouring one as the calendar does.
#[test]
fn candidates_surround_the_iso_week_and_wrap_at_the_year_edge() {
    let c = |y, m, d, first| week_candidates(day(y, m, d), first);
    assert_eq!(c(2025, 3, 3, 0), [(2025, 10, Some(0)), (2025, 9, Some(-1)), (2025, 11, Some(1))]);
    assert_eq!(
        c(2026, 10, 12, -1),
        [(2026, 41, Some(-1)), (2026, 42, Some(0)), (2026, 43, Some(1))]
    );
    // 2020 has 53 weeks, and its week 53 has no successor in 2020.
    assert_eq!(c(2020, 12, 28, 0), [(2020, 53, Some(0)), (2020, 52, Some(-1)), (2021, 1, Some(1))]);
    // ISO week 1 of 2021 opens on Sunday 3 January 2021: no week 0, so the one below is 2020's last.
    assert_eq!(c(2021, 1, 4, 0), [(2021, 1, Some(0)), (2020, 53, Some(-1)), (2021, 2, Some(1))]);
}

/// **The year-edge week is filed under the OLD year as number 53**, and that candidate comes FIRST
/// (and moves no memo). Monday 29 December 2025 is ISO week 1 of 2026 and its Sunday open is in 2025:
/// the archive's `2025/53`. Monday 30 December 2019 is the README's `2020/1` — which answers 404 —
/// and is really `2019/53`. Monday 1 January 2024 is `2023/53`.
#[test]
fn the_year_edge_week_is_filed_under_the_old_year() {
    let c = |y, m, d| week_candidates(day(y, m, d), 0);
    assert_eq!(
        c(2025, 12, 29),
        [(2025, 53, None), (2026, 1, Some(0)), (2025, 52, Some(-1)), (2026, 2, Some(1))]
    );
    assert_eq!(c(2019, 12, 30)[0], (2019, 53, None));
    assert_eq!(c(2019, 12, 30)[1], (2020, 1, Some(0)), "the README's example is the SECOND guess");
    assert_eq!(c(2024, 1, 1)[0], (2023, 53, None));
    // A Monday that is ISO week 1 with its Sunday in the same year has no such candidate.
    assert_eq!(c(2023, 1, 2)[0], (2023, 1, Some(0)));
    assert_eq!(c(2021, 1, 4)[0], (2021, 1, Some(0)));
}

/// Numbering that follows the ISO week of the Monday (the 2020..2025 regime): one request per
/// week, and the rows of both weeks come back in order.
#[test]
fn two_weeks_in_the_iso_regime_cost_two_requests() {
    let mut wire = FakeWire::with(&[
        (weekly_url("m1", "EURUSD", 2025, 10), M1_2025_10),
        (weekly_url("m1", "EURUSD", 2025, 11), M1_2025_11),
    ]);
    let bars =
        fetch_excerpts(&mut wire, "EURUSD", "1m", at(2025, 3, 2, 22, 0), at(2025, 3, 10, 3, 9))
            .unwrap();
    assert_eq!(
        wire.asked,
        [weekly_url("m1", "EURUSD", 2025, 10), weekly_url("m1", "EURUSD", 2025, 11)]
    );
    assert_eq!(bars.len(), 720);
    assert_eq!(bars[0].ts, at(2025, 3, 2, 22, 0));
    assert_eq!(bars[360].ts, at(2025, 3, 9, 21, 6));
    assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts));
}

/// **The 2026 regime: the number is one LOWER than the ISO week.** The week of Monday 5 October is
/// ISO week 41, and the file `2026/41` holds the week of 12 October. The walk asks `41` first (the
/// ISO number), READS that file's own dates, finds a different week, caches it under its true
/// Monday and goes on to `40`; the second week then costs no request at all. A source that trusted
/// the number would store the week of 12 October as the week of 5 October.
#[test]
fn a_shifted_year_is_found_by_reading_the_dates_not_the_number() {
    let mut wire = FakeWire::with(&[
        (weekly_url("m1", "EURUSD", 2026, 40), M1_2026_40),
        (weekly_url("m1", "EURUSD", 2026, 41), M1_2026_41),
    ]);
    let bars =
        fetch_excerpts(&mut wire, "EURUSD", "1m", at(2026, 10, 4, 21, 0), at(2026, 10, 12, 3, 5))
            .unwrap();
    assert_eq!(
        wire.asked,
        [weekly_url("m1", "EURUSD", 2026, 41), weekly_url("m1", "EURUSD", 2026, 40)],
        "the ISO number first, then the lower one; the second week came from the cache"
    );
    assert_eq!(bars.len(), 720);
    assert_eq!(bars[0].ts, at(2026, 10, 4, 21, 3), "the week of 5 October first");
    assert_eq!(bars[360].ts, at(2026, 10, 11, 21, 3), "then the week of 12 October");
}

/// The last offset that worked is tried first: a one-week window after a hit costs one request.
#[test]
fn the_offset_that_worked_is_tried_first() {
    let mut wire = FakeWire::with(&[
        (weekly_url("m1", "EURUSD", 2026, 40), M1_2026_40),
        (weekly_url("m1", "EURUSD", 2026, 41), M1_2026_41),
    ]);
    let mut walk = WeekWalk::new(&mut wire, Grain::M1, "EURUSD", EXCERPT);
    assert_matches!(walk.week(day(2026, 10, 5)), Ok(Week::Rows(_)));
    assert_eq!(walk.offset, -1);
    assert_eq!(week_candidates(day(2026, 10, 19), walk.offset)[0], (2026, 42, Some(-1)));
}

/// **A year-edge week costs ONE request** because its candidate leads: `2025/53` holds the week of
/// Monday 29 December 2025 (ISO week 1 of 2026), found without the README-style `2026/1` guess.
#[test]
fn the_year_edge_week_is_found_in_one_request_under_the_old_year() {
    let mut wire = FakeWire::with(&[(weekly_url("m1", "EURUSD", 2025, 53), M1_2025_53)]);
    let bars =
        fetch_excerpts(&mut wire, "EURUSD", "1m", at(2025, 12, 28, 22, 0), at(2025, 12, 29, 3, 0))
            .unwrap();
    assert_eq!(wire.asked, [weekly_url("m1", "EURUSD", 2025, 53)]);
    assert_eq!(bars[0].ts, at(2025, 12, 28, 22, 54));
    assert!(bars.len() > 200 && bars.iter().all(|b| b.ts <= at(2025, 12, 29, 3, 0)));
}

/// **A 200 is not proof of a whole week either.** `2025/26` is a real file of 93 rows (one Friday
/// evening, ending at a Saturday stamp): it is skipped as truncated, the neighbours are asked, and
/// the week is NOT AVAILABLE — not stored as a one-evening week under a spent key.
#[test]
fn a_truncated_week_is_not_available() {
    let mut wire = FakeWire::with(&[(weekly_url("m1", "EURUSD", 2025, 26), M1_2025_26_TRUNCATED)]);
    let err = fetch_over(&mut wire, "EURUSD", "1m", at(2025, 6, 23, 0, 0), at(2025, 6, 27, 22, 0))
        .unwrap_err();
    let SourceError::Fetch(text) = &err else { panic!("{err:?}") };
    assert!(text.contains("NOT AVAILABLE") && text.contains("a truncated week"), "{text}");
    assert!(text.contains("86 rows over 1 hours"), "the kept rows and their span: {text}");
    assert_eq!(wire.asked.len(), 3, "the ISO week and its two neighbours");
    // The same file, parsed, is what it says: it is the fixture, not a bad copy.
    let bars = parsed(M1_2025_26_TRUNCATED, Grain::M1);
    assert_eq!((bars.len(), bars[0].ts), (93, at(2025, 6, 27, 19, 33)));
}

// ── trap (b): a 404 is not "no candles" ───────────────────────────────────────────────────────

/// **A week with no file is an ERROR naming the week and the URLs tried — never `Ok` with the
/// week left out**, which an ingest would store as an empty answer and seal under a spent key. The
/// README's example URL is the 404 here.
#[test]
fn a_week_with_no_file_is_not_available_and_never_an_empty_answer() {
    // Monday 30 December 2019 is ISO week 1 of 2020: the README's own example, `2020/1`, is among the
    // guesses and answers 404 (the real file is `2019/53`; here the archive has neither).
    let mut wire = FakeWire::default();
    let err = fetch_over(&mut wire, "EURUSD", "1m", at(2019, 12, 31, 0, 0), at(2019, 12, 31, 1, 0))
        .expect_err("a week with no file must not be an Ok");
    let SourceError::Fetch(text) = &err else { panic!("a hole is a Fetch failure, got {err:?}") };
    assert!(text.contains("NOT AVAILABLE") && text.contains("not proof"), "{text}");
    assert!(text.contains("Monday 2019-12-30"), "{text}");
    assert!(text.contains(&weekly_url("m1", "EURUSD", 2020, 1)), "the README example: {text}");
    assert!(text.contains("(HTTP 404)"), "{text}");
    assert_eq!(
        wire.asked.len(),
        4,
        "the year-edge number and the ISO week with its two neighbours"
    );
}

/// A hole in the MIDDLE of a window fails the whole window; nothing partial comes back.
#[test]
fn a_hole_between_two_good_weeks_fails_the_window() {
    let mut wire = FakeWire::with(&[(weekly_url("m1", "EURUSD", 2025, 10), M1_2025_10)]);
    let err =
        fetch_excerpts(&mut wire, "EURUSD", "1m", at(2025, 3, 2, 22, 0), at(2025, 3, 10, 3, 9))
            .unwrap_err();
    let SourceError::Fetch(text) = &err else { panic!("{err:?}") };
    assert!(text.contains("Monday 2025-03-10") && text.contains("2025/11.csv.gz"), "{text}");
    // The window starts at the Sunday open, not at midnight, so its start is printed in milliseconds
    // (a date would round it down to the Sunday midnight, before the open, which is harmless here
    // but would be a different instant).
    let piece = format!("--from {} --to 2025-03-08", at(2025, 3, 2, 22, 0));
    assert!(text.contains(&piece), "the piece before it: {text}");
}

#[test]
fn a_d1_year_with_no_file_is_not_available() {
    let mut wire = FakeWire::default();
    let err = fetch_over(&mut wire, "EURUSD", "1d", at(2023, 1, 3, 0, 0), at(2023, 6, 1, 0, 0))
        .unwrap_err();
    let SourceError::Fetch(text) = &err else { panic!("{err:?}") };
    assert!(text.contains("year 2023") && text.contains("D1/EURUSD/2023.csv.gz"), "{text}");
    assert!(text.contains("none: no week or year of this window is in the archive"), "{text}");
}

// ── which files a window needs: both ends are judged by the open and the close ───────────────────

#[test]
fn the_weeks_a_window_needs() {
    let (mon_3, mon_10) = (day(2025, 3, 3), day(2025, 3, 10));
    let weeks = |from, to| weeks_covering(from, to);
    // One minute on a Monday: that week. Sunday 22:00 UTC (the winter open): the week that opens then.
    assert_eq!(weeks(at(2025, 3, 3, 12, 0), at(2025, 3, 3, 12, 1)), Some((mon_3, mon_3)));
    assert_eq!(weeks(at(2025, 3, 2, 22, 0), at(2025, 3, 2, 22, 1)), Some((mon_3, mon_3)));
    // A START inside the closed window begins at the next open: Saturday, and Friday after the close.
    assert_eq!(weeks(at(2025, 3, 8, 0, 0), at(2025, 3, 11, 0, 0)), Some((mon_10, mon_10)));
    assert_eq!(weeks(at(2025, 3, 7, 22, 0), at(2025, 3, 11, 0, 0)), Some((mon_10, mon_10)));
    // ...and a Sunday START before the open already is the next week's.
    assert_eq!(weeks(at(2025, 3, 9, 0, 0), at(2025, 3, 11, 0, 0)), Some((mon_10, mon_10)));
    // An END inside the closed window ends at the week before: Saturday, Friday after the close, and
    // a Sunday before the open, which is what the open's own hour decides (summer 21:00, winter 22:00).
    assert_eq!(weeks(at(2025, 3, 3, 0, 0), at(2025, 3, 8, 23, 59)), Some((mon_3, mon_3)));
    assert_eq!(weeks(at(2025, 3, 3, 0, 0), at(2025, 3, 7, 22, 0)), Some((mon_3, mon_3)));
    assert_eq!(weeks(at(2025, 3, 3, 0, 0), at(2025, 3, 9, 0, 0)), Some((mon_3, mon_3)));
    assert_eq!(weeks(at(2025, 3, 3, 0, 0), at(2025, 3, 9, 20, 59)), Some((mon_3, mon_3)));
    assert_eq!(weeks(at(2025, 3, 3, 0, 0), at(2025, 3, 9, 21, 0)), Some((mon_3, mon_10)));
    // A window wholly inside the closed window needs no week at all.
    assert_eq!(weeks(at(2025, 3, 8, 0, 0), at(2025, 3, 8, 23, 59)), None);
    assert_eq!(weeks(at(2025, 3, 7, 23, 0), at(2025, 3, 9, 20, 59)), None);
    // Friday 21:00 to the Sunday evening after the open: both weeks.
    assert_eq!(weeks(at(2025, 3, 7, 21, 0), at(2025, 3, 9, 22, 0)), Some((mon_3, mon_10)));
    assert_eq!(trading_monday(at(2025, 3, 2, 22, 0)), mon_3);
    assert_eq!(trading_monday(at(2025, 3, 7, 21, 58)), mon_3);
}

/// **A window that ends on a Sunday before the open does not ask for the next week.** The reviewer's
/// input `--from 2026-10-04 --to 2026-10-18` (a Sunday, midnight) on Tuesday 2026-10-20 used to demand
/// the unpublished week of Monday 2026-10-19 — Sunday maps to the NEXT Monday though that week's first
/// row is Sunday 21:00 — and failed whole. Only the two weeks it holds are asked. (The files are the
/// real `2026/40` and `2026/41` excerpts; the number-then-date walk is `a_shifted_year...` above.)
#[test]
fn a_window_ending_on_a_sunday_before_the_open_does_not_ask_for_the_next_week() {
    let mut wire = FakeWire::with(&[
        (weekly_url("m1", "EURUSD", 2026, 40), M1_2026_40),
        (weekly_url("m1", "EURUSD", 2026, 41), M1_2026_41),
    ]);
    let tuesday = at(2026, 10, 20, 12, 0);
    let bars = fetch_with(
        &mut wire,
        "EURUSD",
        "1m",
        (at(2026, 10, 4, 0, 0), at(2026, 10, 18, 0, 0)),
        tuesday,
        Some(EXCERPT),
    )
    .unwrap();
    assert_eq!(bars.len(), 720);
    assert_eq!(
        wire.asked,
        [weekly_url("m1", "EURUSD", 2026, 41), weekly_url("m1", "EURUSD", 2026, 40)],
        "no candidate of the week of Monday 2026-10-19 (`2026/42`, `/43`) was asked"
    );
}

/// **A `1d` window ending at midnight of New Year does not ask for the new year's file.** A `1d` stamp
/// is 21:00 or 22:00 UTC of its day, so `--to 2021-01-01` holds no stamp of 2021 and needs `D1/2020`
/// alone (it used to ask `D1/2021` too, and `--to 2022-01-01` asked `D1/2022`, which does not exist).
/// A window that runs on to 21:00 on New Year's Day can hold the first stamp of the year, and asks.
#[test]
fn a_1d_window_ending_at_new_year_does_not_ask_for_the_new_years_file() {
    let url = format!("{ARCHIVE}/D1/EURUSD/2020.csv.gz");
    let mut wire = FakeWire::with(&[(url.clone(), D1_2020)]);
    let bars =
        fetch_over(&mut wire, "EURUSD", "1d", at(2020, 1, 1, 0, 0), at(2021, 1, 1, 0, 0)).unwrap();
    assert_eq!((wire.asked.as_slice(), bars.len()), ([url.clone()].as_slice(), 260));

    let mut wire = FakeWire::with(&[(url, D1_2020)]);
    let err = fetch_over(&mut wire, "EURUSD", "1d", at(2020, 1, 1, 0, 0), at(2021, 1, 1, 21, 0))
        .unwrap_err();
    assert!(err.to_string().contains("D1/EURUSD/2021.csv.gz"), "{err}");
    assert_eq!(wire.asked.len(), 2, "2020, then 2021");
}

/// **Every hole in ONE error, and the pieces it prints are fetchable.** A window over five weeks with
/// the weeks of Monday 10 and Monday 24 March 2025 missing: the walk goes on past the first hole, names
/// both, and prints the three pieces the archive does hold as `--from`/`--to` dates. Each piece is then
/// fetched through the same archive and succeeds — the advice is true (the earlier piece ends on the
/// Saturday, a Sunday before the open would do as well, the later one starts on the Sunday before its
/// Monday or on the Saturday before that).
#[test]
fn every_hole_is_listed_once_and_the_pieces_it_prints_can_be_fetched() {
    let week = |open: i64, close: i64| synthetic_week(open, close, 5 * 60_000);
    let (w10, w12, w14) = (
        week(at(2025, 3, 2, 22, 0), at(2025, 3, 7, 21, 55)),
        week(at(2025, 3, 16, 21, 0), at(2025, 3, 21, 20, 55)),
        week(at(2025, 3, 30, 21, 0), at(2025, 4, 4, 20, 55)),
    );
    let files = [
        (weekly_url("m1", "EURUSD", 2025, 10), w10.as_slice()),
        (weekly_url("m1", "EURUSD", 2025, 12), w12.as_slice()),
        (weekly_url("m1", "EURUSD", 2025, 14), w14.as_slice()),
    ];
    let mut wire = FakeWire::with(&files);
    let err = fetch_over(&mut wire, "EURUSD", "1m", at(2025, 3, 2, 0, 0), at(2025, 4, 5, 0, 0))
        .unwrap_err();
    let SourceError::Fetch(text) = &err else { panic!("{err:?}") };
    assert!(text.contains("2 NOT AVAILABLE"), "{text}");
    assert!(text.contains("Monday 2025-03-10") && text.contains("Monday 2025-03-24"), "{text}");
    let pieces = pieces_in(text);
    assert_eq!(
        pieces,
        [
            (at(2025, 3, 2, 0, 0), at(2025, 3, 8, 0, 0)),
            (at(2025, 3, 16, 0, 0), at(2025, 3, 22, 0, 0)),
            (at(2025, 3, 30, 0, 0), at(2025, 4, 5, 0, 0)),
        ],
        "{text}"
    );
    for (from, to) in pieces {
        let mut wire = FakeWire::with(&files);
        let bars = fetch_over(&mut wire, "EURUSD", "1m", from, to)
            .unwrap_or_else(|e| panic!("the printed piece {from}..{to} failed: {e}"));
        assert!(bars.len() > 1000 && bars.iter().all(|b| (from..=to).contains(&b.ts)));
    }
    // Other ways round the first hole: end on the Sunday before the open, start on the Saturday.
    let mut wire = FakeWire::with(&files);
    assert!(
        fetch_over(&mut wire, "EURUSD", "1m", at(2025, 3, 2, 0, 0), at(2025, 3, 9, 0, 0)).is_ok()
    );
    let mut wire = FakeWire::with(&files);
    assert!(
        fetch_over(&mut wire, "EURUSD", "1m", at(2025, 3, 15, 0, 0), at(2025, 3, 22, 0, 0)).is_ok()
    );
}

/// **The pieces keep the window's own ends.** The printed `--from`/`--to` read a UTC midnight as a date
/// and anything else as epoch milliseconds, because `vike-cli data hist fetch` takes both: a window that
/// ends Friday 15:00 used to print `--to <Friday>`, which is Friday 00:00 and drops the session, and one
/// that ends Sunday 21:30 printed Sunday 00:00 and lost its last week. Here both windows are re-fetched
/// as printed and the last bar is the window's own last row.
#[test]
fn the_pieces_keep_the_windows_own_ends_when_they_are_not_midnights() {
    let week = |open: i64, close: i64| synthetic_week(open, close, 5 * 60_000);
    let (w10, w12, w14, w15) = (
        week(at(2025, 3, 2, 22, 0), at(2025, 3, 7, 21, 55)),
        week(at(2025, 3, 16, 21, 0), at(2025, 3, 21, 20, 55)),
        week(at(2025, 3, 30, 21, 0), at(2025, 4, 4, 20, 55)),
        week(at(2025, 4, 6, 21, 0), at(2025, 4, 11, 20, 55)),
    );
    let files = [
        (weekly_url("m1", "EURUSD", 2025, 10), w10.as_slice()),
        (weekly_url("m1", "EURUSD", 2025, 12), w12.as_slice()),
        (weekly_url("m1", "EURUSD", 2025, 14), w14.as_slice()),
        (weekly_url("m1", "EURUSD", 2025, 15), w15.as_slice()),
    ];
    let start = at(2025, 3, 2, 22, 0); // the winter open: not a midnight either
    for end in [at(2025, 4, 4, 15, 0), at(2025, 4, 6, 21, 30)] {
        let mut wire = FakeWire::with(&files);
        let err = fetch_over(&mut wire, "EURUSD", "1m", start, end).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("2 NOT AVAILABLE"), "{text}");
        // Written out, not rounded: the milliseconds appear, the midnights stay dates.
        assert!(text.contains(&format!("--from {start} --to 2025-03-08")), "{text}");
        assert!(text.contains("--from 2025-03-16 --to 2025-03-22"), "{text}");
        assert!(text.contains(&format!("--from 2025-03-30 --to {end}")), "{text}");
        let pieces = pieces_in(&text);
        assert_eq!(pieces.len(), 3, "{text}");
        assert_eq!((pieces[0].0, pieces[2].1), (start, end), "{text}");
        for (from, to) in pieces {
            let mut wire = FakeWire::with(&files);
            let bars = fetch_over(&mut wire, "EURUSD", "1m", from, to)
                .unwrap_or_else(|e| panic!("the printed piece {from}..{to} failed: {e}"));
            assert!(!bars.is_empty() && bars.iter().all(|b| (from..=to).contains(&b.ts)));
        }
        // The last piece reaches the window's own last row (Friday 15:00, or Sunday 21:30 into the next
        // week) — the row the rounded `--to` would have cut.
        let mut wire = FakeWire::with(&files);
        let last = fetch_over(&mut wire, "EURUSD", "1m", at(2025, 3, 30, 0, 0), end).unwrap();
        assert_eq!(last.last().map(|b| b.ts), Some(end));
    }
}

/// The `--from X --to Y` pieces printed in an error, each bound a date or epoch milliseconds, as the
/// instants `vike-cli data hist fetch` reads them as.
fn pieces_in(text: &str) -> Vec<(i64, i64)> {
    let token =
        |s: &str| s.chars().take_while(|c| !c.is_whitespace() && *c != ';').collect::<String>();
    text.split("--from ")
        .skip(1)
        .filter_map(|rest| {
            let from = token(rest);
            let to = token(rest.split_once(" --to ")?.1);
            Some((
                vike_model::parse_date_label(&from).ok()?,
                vike_model::parse_date_label(&to).ok()?,
            ))
        })
        .collect()
}

/// The holes of a `1d` window are listed together too, with the years between them as pieces.
#[test]
fn the_missing_years_of_a_1d_window_are_listed_together() {
    let url = format!("{ARCHIVE}/D1/EURUSD/2020.csv.gz");
    let mut wire = FakeWire::with(&[(url, D1_2020)]);
    let err = fetch_over(&mut wire, "EURUSD", "1d", at(2019, 6, 1, 0, 0), at(2022, 6, 1, 0, 0))
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("3 NOT AVAILABLE"), "2019, 2021 and 2022: {text}");
    assert!(text.contains("year 2019") && text.contains("year 2021") && text.contains("year 2022"));
    assert_eq!(pieces_in(&text), [(at(2020, 1, 1, 0, 0), at(2021, 1, 1, 0, 0))], "{text}");
}

// ── what makes a file a week: its span ──────────────────────────────────────────────────────────

/// **A week is judged by its span, not by a row count measured on two pairs.** Every file here is a
/// full-strength fetch of a synthetic week. A Christmas week that opens Sunday 22:00 and ends Thursday
/// 19:03 with only a row every half hour (a thin pair) passes; so does a thin week of about 150 rows
/// over five days. A short fragment passes no row count: 2,880 rows packed into two days is refused,
/// and so are 50 rows spread over five days (under the very low sanity floor). The real truncated
/// `2025/26` is `a_truncated_week_is_not_available`.
#[test]
fn a_week_is_judged_by_its_span_not_by_how_many_rows_it_has() {
    let minute = 60_000;
    // (what, first row, last row, step, a week?)
    let christmas = (at(2020, 12, 20, 22, 0), at(2020, 12, 24, 19, 3));
    let march = (at(2025, 3, 2, 22, 0), at(2025, 3, 7, 21, 0));
    let fragment = (at(2025, 3, 4, 0, 0), at(2025, 3, 6, 0, 0));
    let cases = [
        ("a Christmas week ending Thursday, a row per half hour", christmas, 30 * minute, true),
        ("a thin pair, about 150 rows over five days", march, 47 * minute, true),
        ("a dense two-day fragment, 2,880 rows", fragment, minute, false),
        ("fifty rows over five days", march, 144 * minute, false),
    ];
    for (what, (first, last), step, is_a_week) in cases {
        let (monday, number, year) = if first < at(2021, 1, 1, 0, 0) {
            (day(2020, 12, 21), 52, 2020)
        } else {
            (day(2025, 3, 3), 10, 2025)
        };
        let file = synthetic_week(first, last, step);
        let mut wire =
            FakeWire::with(&[(weekly_url("m1", "EURUSD", year, number), file.as_slice())]);
        let (from, to) = ((monday - 1) * MS_PER_DAY, (monday + 5) * MS_PER_DAY);
        match (is_a_week, fetch_over(&mut wire, "EURUSD", "1m", from, to)) {
            (true, Ok(bars)) => assert!(bars.len() >= 100, "{what}: {} bars", bars.len()),
            (false, Err(SourceError::Fetch(text))) => {
                assert!(text.contains("a truncated week"), "{what}: {text}");
            }
            (_, other) => panic!("{what}: expected a week = {is_a_week}, got {other:?}"),
        }
    }
}

/// The `1h` floor is the same span with a lower sanity count.
#[test]
fn an_h1_week_must_span_three_days_too() {
    let monday = day(2025, 3, 3);
    let full = synthetic_week(at(2025, 3, 2, 22, 0), at(2025, 3, 7, 21, 0), HOUR_MS);
    let short = synthetic_week(at(2025, 3, 4, 0, 0), at(2025, 3, 5, 12, 0), HOUR_MS);
    for (file, is_a_week) in [(full, true), (short, false)] {
        let mut wire = FakeWire::with(&[(weekly_url("H1", "EURUSD", 2025, 10), file.as_slice())]);
        let (from, to) = ((monday - 1) * MS_PER_DAY, (monday + 5) * MS_PER_DAY);
        let result = fetch_over(&mut wire, "EURUSD", "1h", from, to);
        assert_eq!(result.is_ok(), is_a_week, "{result:?}");
    }
}

/// **A file cut after exactly three days is not a whole week when it claims to be one.** A file whose
/// first kept row is the Sunday open claims a whole week and must reach Thursday 12:00 UTC (the
/// shortest real week measured, a Christmas week, ends Thursday 19:03; none of 321 Sunday-opening files
/// in the map ends earlier). Cut at Wednesday 23:00 it spans three days and an hour and passes the span
/// guard — and fails this. A holiday week that opens on Monday claims nothing and is held to the span
/// alone, so it passes with the same Thursday-morning end. Both directions.
#[test]
fn a_sunday_opening_week_must_reach_thursday_noon_but_a_holiday_week_claims_nothing() {
    let half_hour = 30 * 60_000;
    // (what, first row, last row, monday of the week, url year, url number, a week?)
    let cases = [
        (
            "Sunday open, cut Wednesday 23:00 (three days and an hour)",
            (at(2025, 3, 2, 22, 0), at(2025, 3, 5, 23, 0)),
            false,
        ),
        (
            "Sunday open, ends Thursday 11:30",
            (at(2025, 3, 2, 22, 0), at(2025, 3, 6, 11, 30)),
            false,
        ),
        ("Sunday open, ends Thursday 12:00", (at(2025, 3, 2, 22, 0), at(2025, 3, 6, 12, 0)), true),
        (
            "Sunday open, a Christmas week ending Thursday 19:03",
            (at(2025, 3, 2, 22, 0), at(2025, 3, 6, 19, 3)),
            true,
        ),
    ];
    for (what, (first, last), is_a_week) in cases {
        let file = synthetic_week(first, last, half_hour);
        let mut wire = FakeWire::with(&[(weekly_url("m1", "EURUSD", 2025, 10), file.as_slice())]);
        let (from, to) = (at(2025, 3, 2, 0, 0), at(2025, 3, 8, 0, 0));
        match (is_a_week, fetch_over(&mut wire, "EURUSD", "1m", from, to)) {
            (true, Ok(bars)) => assert!(bars.len() > 100, "{what}: {} bars", bars.len()),
            (false, Err(SourceError::Fetch(text))) => {
                assert!(text.contains("a truncated week"), "{what}: {text}");
                assert!(text.contains("reaches at least Thursday 12:00 UTC"), "{what}: {text}");
                assert!(text.contains("ends 2025-03-0"), "the end is named: {what}: {text}");
            }
            (_, other) => panic!("{what}: expected a week = {is_a_week}, got {other:?}"),
        }
    }
    // A holiday week: a synthetic week shaped like 2023/1, cut short. It opens Monday 2023-01-02 06:09
    // and ends Thursday 08:00 UTC, three days and two hours later. No Sunday open, so no claim to be
    // whole: it is a week.
    let file = synthetic_week(at(2023, 1, 2, 6, 9), at(2023, 1, 5, 8, 0), half_hour);
    let mut wire = FakeWire::with(&[(weekly_url("m1", "EURUSD", 2023, 1), file.as_slice())]);
    let bars = fetch_over(&mut wire, "EURUSD", "1m", at(2023, 1, 1, 0, 0), at(2023, 1, 7, 0, 0))
        .expect("a holiday week that opens on Monday is a week");
    assert!(bars.len() > 100 && bars[0].ts == at(2023, 1, 2, 6, 9));
}

// ── a window the archive cannot answer: before it begins, and where it holds nothing ──────────────

/// **A window that starts before the archive does is refused up front.** `--from 2010-01-01 --to
/// 2011-12-30` used to walk about four hundred weeks of 404 (some 420 GETs, three and a half minutes)
/// before saying NOT AVAILABLE. The earliest sample is the week of Monday 2012-01-02; the cut is the
/// start of 2012.
#[test]
fn a_window_starting_before_the_archive_is_refused_before_a_request() {
    assert_eq!(ARCHIVE_FROM_MS, at(2012, 1, 1, 0, 0));
    for interval in ["1m", "1h", "1d"] {
        let mut wire = FakeWire::default();
        for (start, end) in [
            (at(2010, 1, 1, 0, 0), at(2011, 12, 30, 0, 0)),
            (at(2011, 12, 31, 23, 59), at(2012, 6, 1, 0, 0)), // one minute short of the cut
            (0, at(2012, 6, 1, 0, 0)),
            (i64::MIN / 4, at(2012, 6, 1, 0, 0)),
        ] {
            let err =
                fetch_at(i64::MAX / 2, &mut wire, "EURUSD", interval, start, end).unwrap_err();
            let SourceError::Refused(text) = &err else { panic!("{interval} {start}: {err:?}") };
            assert!(
                text.contains("before the archive does") && text.contains("2012-01-02"),
                "{text}"
            );
            assert!(text.contains("--from 2012-01-01"), "{text}");
        }
        assert!(wire.asked.is_empty(), "{interval} asked the archive");
    }
    // ...and the cut itself is the first instant that is asked (the empty archive says NOT AVAILABLE).
    let mut wire = FakeWire::default();
    let err = fetch_over(&mut wire, "EURUSD", "1m", at(2012, 1, 1, 0, 0), at(2012, 1, 6, 0, 0))
        .unwrap_err();
    assert!(matches!(err, SourceError::Fetch(_)), "{err:?}");
    assert!(!wire.asked.is_empty());
}

/// **The walk gives up on a stretch with no file at all.** An archive that holds nothing for the pair
/// answers 404 to every candidate; the walk used to ask them all (about three and a half minutes for a
/// two-year window). It stops after eight consecutive weeks of that, says which weeks it did not ask,
/// and prints no piece for them.
#[test]
fn the_walk_stops_after_eight_weeks_with_no_file_at_all() {
    let mut wire = FakeWire::default();
    let err = fetch_over(&mut wire, "EURUSD", "1m", at(2022, 1, 2, 0, 0), at(2023, 12, 23, 0, 0))
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("8 NOT AVAILABLE"), "{text}");
    assert!(text.contains("STOPPED") && text.contains("after 8 consecutive weeks"), "{text}");
    assert!(text.contains("later weeks of the window were not asked"), "{text}");
    assert!(text.contains("none: no week or year of this window is in the archive"), "{text}");
    // About a request per week plus the neighbours' guesses, not four hundred of them.
    assert!(wire.asked.len() < 50, "{} requests", wire.asked.len());

    // One week left over (nine in the window, eight asked) reads in the singular.
    let mut wire = FakeWire::default();
    let err = fetch_over(&mut wire, "EURUSD", "1m", at(2022, 1, 2, 0, 0), at(2022, 3, 5, 0, 0))
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("the 1 later week of the window was not asked and is not in the pieces"),
        "{text}"
    );

    // The same rule over the years of a 1d window (an empty archive: eleven years in the window).
    let mut wire = FakeWire::default();
    let err = fetch_over(&mut wire, "EURUSD", "1d", at(2012, 1, 1, 0, 0), at(2023, 1, 1, 0, 0))
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("STOPPED") && text.contains("after 8 consecutive years"), "{text}");
    assert_eq!(wire.asked.len(), 8, "eight years asked, three not");
}

/// ...but a file BETWEEN the holes resets the count: seven weeks of nothing, one good week, seven more
/// of nothing is walked to the end, listing all fourteen and printing the one piece in the middle.
#[test]
fn a_good_week_between_holes_resets_the_count() {
    let good = synthetic_week(at(2025, 2, 23, 22, 0), at(2025, 2, 28, 21, 55), 5 * 60_000);
    let mut wire = FakeWire::with(&[(weekly_url("m1", "EURUSD", 2025, 9), good.as_slice())]);
    let err = fetch_over(&mut wire, "EURUSD", "1m", at(2025, 1, 5, 22, 0), at(2025, 4, 19, 0, 0))
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("14 NOT AVAILABLE") && !text.contains("STOPPED"), "{text}");
    // The Sunday before the good week's Monday, to the Saturday after its Friday.
    assert_eq!(pieces_in(&text), [(at(2025, 2, 23, 0, 0), at(2025, 3, 1, 0, 0))], "{text}");
}

// ── a window that reaches the unfinished week ─────────────────────────────────────────────────────

/// **The unfinished week is refused before any request**, with the Saturday to end on. `--days N` ends
/// now and always lands here; so does the chart seed. Wednesday 2026-10-21: the current week is the one
/// of Monday 2026-10-19.
#[test]
fn a_window_reaching_the_unfinished_week_is_refused_before_a_request_and_names_the_saturday() {
    let wednesday = at(2026, 10, 21, 12, 0);
    let mut wire = FakeWire::default();
    for (start, end) in [
        (wednesday - 30 * MS_PER_DAY, wednesday), // `--days 30`
        (at(2026, 10, 4, 0, 0), at(2026, 10, 19, 0, 0)), // up to the Monday morning
        (at(2026, 10, 4, 0, 0), at(2026, 10, 18, 21, 30)), // past the Sunday open
        (at(2026, 10, 4, 0, 0), at(2027, 6, 1, 0, 0)), // the future
    ] {
        let err = fetch_at(wednesday, &mut wire, "EURUSD", "1m", start, end).unwrap_err();
        let SourceError::Refused(text) = &err else { panic!("{start}..{end}: {err:?}") };
        // Two lines. The first is what a GUI shows (a chart seed lands here too): no flag, no tool,
        // just the week and why. The second is the command-line hint.
        let (first, second) = text.split_once('\n').unwrap_or_else(|| panic!("one line: {text}"));
        assert!(
            first.contains("Monday 2026-10-19") && first.contains("only after it ends"),
            "{text}"
        );
        assert!(
            !first.contains("--") && !first.contains("vike-cli") && !first.contains('`'),
            "the first line names no command: {first}"
        );
        assert!(second.starts_with("From the command line:"), "{text}");
        assert!(second.contains("--to 2026-10-17") && second.contains("`--days N`"), "{text}");
        assert!(!second.contains('\n'), "{text}");
    }
    assert!(wire.asked.is_empty(), "nothing was asked of the archive");
}

/// ...and what is NOT in the unfinished week is asked as usual: the Sunday midnight before the open,
/// the Saturday, and — on a Saturday, with the week just ended — that week itself (its file may not be
/// published yet, which is the ordinary NOT AVAILABLE, not a refusal). `1d` has no such refusal.
#[test]
fn a_window_that_stops_before_the_unfinished_week_is_asked_for_real() {
    let wednesday = at(2026, 10, 21, 12, 0);
    for end in [at(2026, 10, 18, 0, 0), at(2026, 10, 18, 20, 59), at(2026, 10, 17, 0, 0)] {
        let mut wire = FakeWire::default();
        let err =
            fetch_at(wednesday, &mut wire, "EURUSD", "1m", at(2026, 10, 4, 0, 0), end).unwrap_err();
        assert!(matches!(err, SourceError::Fetch(_)), "{end}: {err:?}");
        assert!(!wire.asked.is_empty());
    }
    let saturday = at(2026, 10, 17, 12, 0);
    let mut wire = FakeWire::default();
    let err = fetch_at(saturday, &mut wire, "EURUSD", "1m", at(2026, 10, 11, 0, 0), saturday)
        .unwrap_err();
    assert!(matches!(err, SourceError::Fetch(_)), "the week that just ended is asked: {err:?}");
    let mut wire = FakeWire::default();
    let err = fetch_at(wednesday, &mut wire, "EURUSD", "1d", at(2026, 1, 1, 0, 0), wednesday)
        .unwrap_err();
    assert!(matches!(err, SourceError::Fetch(_)), "1d is not refused for the week: {err:?}");
}

/// A window needing more than the cap of weekly files is refused BEFORE a request, however absurd its
/// bounds — computed from the two end weeks, never by building the list. The cap is per interval:
/// about two years of `1m` and five of `1h`, and the refusal says why.
#[test]
fn a_window_of_too_many_weeks_is_refused_without_building_it() {
    // The whole archive (fourteen years of weeks) and a far end at the edge of `i64`, with `now` further
    // off still so that the unfinished-week refusal is not what answers.
    let (whole, far) =
        ((at(2012, 1, 1, 0, 0), at(2026, 1, 1, 0, 0)), (at(2012, 1, 1, 0, 0), i64::MAX / 4));
    let cases = [("1m", whole), ("1h", whole), ("1m", far), ("1h", far), ("1d", far)];
    for (interval, (start, end)) in cases {
        let mut wire = FakeWire::default();
        let err = fetch_at(i64::MAX / 2, &mut wire, "EURUSD", interval, start, end).unwrap_err();
        let SourceError::Refused(text) = &err else { panic!("{interval}: {err:?}") };
        assert!(text.contains("files") || text.contains("years"), "{interval}: {text}");
        assert!(!text.contains("before the archive does"), "{interval}: {text}");
        assert!(wire.asked.is_empty(), "{interval} asked the archive");
    }
    // The boundary of `1m`: 105 weeks (2024-01-01 to 2026-01-01) is asked, three years is not.
    let mut wire = FakeWire::default();
    let two_years =
        fetch_over(&mut wire, "EURUSD", "1m", at(2024, 1, 1, 0, 0), at(2026, 1, 1, 0, 0));
    assert_matches!(two_years, Err(SourceError::Fetch(_)));
    assert!(!wire.asked.is_empty(), "two years passed the cap and reached the archive");
    let mut wire = FakeWire::default();
    let three_years =
        fetch_over(&mut wire, "EURUSD", "1m", at(2023, 1, 1, 0, 0), at(2026, 1, 1, 0, 0));
    let Err(SourceError::Refused(text)) = three_years else { panic!("{three_years:?}") };
    assert!(text.contains("two years") && text.contains("cannot be cancelled"), "{text}");
    // `1h` allows more: four years passes (208 weeks), six does not (313).
    let mut wire = FakeWire::default();
    assert_matches!(
        fetch_over(&mut wire, "EURUSD", "1h", at(2022, 1, 1, 0, 0), at(2026, 1, 1, 0, 0)),
        Err(SourceError::Fetch(_))
    );
    assert!(!wire.asked.is_empty(), "the 1h window passed the cap and reached the archive");
    let mut wire = FakeWire::default();
    assert_matches!(
        fetch_over(&mut wire, "EURUSD", "1h", at(2020, 1, 1, 0, 0), at(2026, 1, 1, 0, 0)),
        Err(SourceError::Refused(_))
    );
}

// ── refusals send nothing ─────────────────────────────────────────────────────────────────────

#[test]
fn what_cannot_be_expressed_is_refused_before_a_request() {
    let (s, e) = (at(2025, 3, 3, 0, 0), at(2025, 3, 4, 0, 0));
    let cases = [
        ("EURUSD", "5m", s, e),
        ("EURUSD", "1w", s, e),
        ("eurusd", "1m", s, e),
        ("EUR_USD", "1m", s, e),
        ("XAUUSD", "1m", s, e),
        ("EURUSD", "1m", at(2015, 1, 1, 0, 0), at(2025, 1, 1, 0, 0)),
    ];
    for (symbol, interval, start, end) in cases {
        let mut wire = FakeWire::default();
        let err = fetch_over(&mut wire, symbol, interval, start, end).unwrap_err();
        assert!(matches!(err, SourceError::Refused(_)), "{symbol} {interval}: {err:?}");
        assert!(wire.asked.is_empty(), "{symbol} {interval} sent a request");
    }
    let mut wire = FakeWire::default();
    assert!(fetch_over(&mut wire, "EURUSD", "1m", e, s).unwrap().is_empty(), "inverted window");
}

/// The public face: the registry row's type refuses without touching the network.
#[test]
fn the_source_names_the_venue_and_refuses_a_bad_request_offline() {
    assert_eq!(FxcmKlines.venue(), "fxcm");
    assert!(matches!(FxcmKlines.fetch("EURUSD", "5m", 0, 1), Err(SourceError::Refused(_))));
    assert!(matches!(FxcmKlines.fetch("eurusd", "1m", 0, 1), Err(SourceError::Refused(_))));
}

// ── the file is held to its format ────────────────────────────────────────────────────────────

#[test]
fn a_drifted_or_damaged_file_fails_whole() {
    let row = "03/02/2025 22:00:00.000,1.1,1.2,1.0,1.1,1.1,1.2,1.0,1.1";
    let ok = |text: String| parse_csv(&text, Grain::M1);
    assert!(ok(format!("{HEADER}\r\n{row}\r\n")).is_ok());
    assert!(ok(format!("{HEADER}\r\n{row}\r\n\r\n")).is_ok(), "a blank trailing line");
    // The header drifted (a volume column appeared, or the order changed).
    assert!(ok(format!("{HEADER},Volume\r\n{row}\r\n")).unwrap_err().contains("unexpected header"));
    assert!(ok(format!("DateTime,BidOpen\r\n{row}\r\n")).is_err());
    // A price that is not a positive finite number, a short row, a date no calendar holds.
    for bad in [
        "03/02/2025 22:00:00.000,x,1.2,1.0,1.1,1.1,1.2,1.0,1.1",
        "03/02/2025 22:00:00.000,0,1.2,1.0,1.1,1.1,1.2,1.0,1.1",
        "03/02/2025 22:00:00.000,NaN,1.2,1.0,1.1,1.1,1.2,1.0,1.1",
        "03/02/2025 22:00:00.000,1.1,1.2,1.0,1.1,1.1,1.2,1.0",
        "02/31/2025 22:00:00.000,1.1,1.2,1.0,1.1,1.1,1.2,1.0,1.1",
        "2025-03-02 22:00:00,1.1,1.2,1.0,1.1,1.1,1.2,1.0,1.1",
        "03/02/2025 24:00:00.000,1.1,1.2,1.0,1.1,1.1,1.2,1.0,1.1",
    ] {
        assert!(ok(format!("{HEADER}\r\n{bad}\r\n")).is_err(), "{bad}");
    }
    // A repeated minute, and a minute that steps back.
    let later = "03/02/2025 22:01:00.000,1.1,1.2,1.0,1.1,1.1,1.2,1.0,1.1";
    assert!(ok(format!("{HEADER}\r\n{row}\r\n{row}\r\n")).unwrap_err().contains("not after"));
    assert!(ok(format!("{HEADER}\r\n{later}\r\n{row}\r\n")).unwrap_err().contains("row 3"));
}

#[test]
fn a_body_that_is_not_gzip_or_expands_past_the_cap_is_refused() {
    assert!(decode(b"<html>blocked</html>", Grain::M1).unwrap_err().contains("gzip"));
    let err = decode_capped(M1_2025_10, Grain::M1, 1000).unwrap_err();
    assert!(err.contains("decompresses past 1000"), "{err}");
    assert!(decode_capped(&gz(&format!("{HEADER}\r\n")), Grain::M1, 1000).unwrap().is_empty());
}

/// Two files that overlap would store a minute twice (the store dedups by commit key, never by row).
#[test]
fn files_that_overlap_are_an_error_not_a_duplicate() {
    let mut out = parsed(M1_2025_10, Grain::M1);
    let again = parsed(M1_2025_10, Grain::M1);
    let err = append_window(&mut out, again, 0, i64::MAX, "x").unwrap_err();
    assert!(err.to_string().contains("overlap"), "{err}");
}

/// A weekly file whose rows run past one trading week is not a weekly file.
#[test]
fn a_file_spanning_more_than_a_week_is_refused() {
    let text = format!(
        "{HEADER}\r\n03/02/2025 22:00:00.000,1.1,1.2,1.0,1.1,1.1,1.2,1.0,1.1\r\n\
         03/09/2025 22:00:00.000,1.1,1.2,1.0,1.1,1.1,1.2,1.0,1.1\r\n"
    );
    let mut wire = FakeWire::with(&[(weekly_url("m1", "EURUSD", 2025, 10), gz(&text).as_slice())]);
    let err = fetch_excerpts(&mut wire, "EURUSD", "1m", at(2025, 3, 3, 0, 0), at(2025, 3, 4, 0, 0))
        .unwrap_err();
    assert!(err.to_string().contains("not a weekly file"), "{err}");
}

// ── the retry policy, over a scripted wire: nothing sleeps or dials ────────────────────────────

#[test]
fn a_transient_failure_is_retried_on_the_schedule_until_it_succeeds() {
    let ok = Attempt::Response { status: 200, body: vec![1, 2, 3], retry_after: None };
    let mut wire = ScriptWire::new(vec![status(503), Attempt::Failed("network: reset".into()), ok]);
    assert_eq!(get_with_retries(&mut wire, "u").unwrap(), Some(vec![1, 2, 3]));
    assert_eq!((wire.attempts, wire.waits), (3, vec![secs(2), secs(5)]));
}

#[test]
fn a_url_that_keeps_failing_waits_out_the_schedule_and_reports_the_last_failure() {
    let mut wire = ScriptWire::new((0..5).map(|_| status(503)).collect());
    let err = get_with_retries(&mut wire, "u").unwrap_err();
    assert!(err.to_string().contains("HTTP 503 (after 4 retries)"), "{err}");
    assert_eq!(wire.waits, vec![secs(2), secs(5), secs(15), secs(45)]);
}

#[test]
fn a_404_is_never_retried_and_another_status_fails_at_once() {
    let mut wire = ScriptWire::new(vec![status(404)]);
    assert_eq!(get_with_retries(&mut wire, "u").unwrap(), None);
    assert_eq!((wire.attempts, wire.waits.len()), (1, 0));
    let mut wire = ScriptWire::new(vec![status(403)]);
    assert!(get_with_retries(&mut wire, "u").unwrap_err().to_string().contains("HTTP 403"));
    assert_eq!(wire.attempts, 1);
}

#[test]
fn a_retry_after_lengthens_a_wait_but_is_capped_and_never_shortens_one() {
    let after = |s| Attempt::Response { status: 429, body: Vec::new(), retry_after: Some(secs(s)) };
    let ok = Attempt::Response { status: 200, body: vec![9], retry_after: None };
    let mut wire = ScriptWire::new(vec![after(25), after(3600), after(1), ok]);
    assert_eq!(get_with_retries(&mut wire, "u").unwrap(), Some(vec![9]));
    assert_eq!(wire.waits, vec![secs(25), secs(60), secs(15)]);
}

// ── LIVE ──────────────────────────────────────────────────────────────────────────────────────

/// LIVE: one real week of `m1`, `H1` and a `D1` year through the production wire (TLS, the agent,
/// gzip, the parser). Anonymous GETs of a public CDN, a handful of them; run explicitly:
/// `cargo test -p vike-fxcm --lib data::data_tests::live -- --ignored --nocapture`
#[test]
#[ignore = "live: anonymous GETs of FXCM's public candle archive"]
fn live_one_week_of_each_interval() {
    let week = (at(2024, 3, 4, 0, 0), at(2024, 3, 8, 22, 0));
    let m1 = FxcmKlines.fetch("EURUSD", "1m", week.0, week.1).expect("m1 week");
    let h1 = FxcmKlines.fetch("EURUSD", "1h", week.0, week.1).expect("h1 week");
    let d1 =
        FxcmKlines.fetch("EURUSD", "1d", at(2021, 3, 1, 0, 0), at(2021, 3, 31, 0, 0)).expect("d1");
    println!("m1 {} bars, h1 {} bars, d1 {} bars", m1.len(), h1.len(), d1.len());
    assert!((6000..=7300).contains(&m1.len()), "a trading week of minutes: {}", m1.len());
    assert!((100..=121).contains(&h1.len()), "a trading week of hours: {}", h1.len());
    assert!((15..=31).contains(&d1.len()), "a month of days: {}", d1.len());
    for b in m1.iter().chain(&h1).chain(&d1) {
        assert!(b.low <= b.open && b.open <= b.high && b.low <= b.close && b.close <= b.high);
        assert!((0.9..1.5).contains(&b.close), "EURUSD price: {}", b.close);
    }
}
