//! The klines lane: the file names, the row grammar and its time unit, the one-owner table, and the
//! import and the replacement of a holed day over a real store.

use super::*;
use crate::archive_import::{ArchiveDayClass, ArchiveDayResult, ArchiveImport};
use crate::testing::zip_of;

const SYMBOL: &str = "BTCUSDT.P";
const VENDOR: &str = "BTCUSDT";
const IV: &str = "1h";
const HOUR: i64 = 3_600_000;
/// 2024-01-15, a UTC midnight.
const DAY: i64 = 19_737 * MS_PER_DAY;
const LABEL: &str = "2024-01-15";
const LATER: i64 = DAY + 400 * MS_PER_DAY;
const HEADER: &str = "open_time,open,high,low,close,volume,close_time,quote_volume,count,\
                      taker_buy_volume,taker_buy_quote_volume,ignore\n";

fn lane() -> BinanceKlinesLane {
    BinanceKlinesLane::new(SYMBOL, VENDOR, IV).unwrap()
}

fn daily() -> String {
    format!("{VENDOR}-{IV}-{LABEL}.csv")
}

/// Hour `h` of `day`'s row, its open time written in units of `per` per millisecond (1 or 1000).
fn row_at(day: i64, h: i64, per: i64) -> String {
    let open = (day + h * HOUR) * per;
    let close = open + HOUR * per - 1;
    format!("{open},100.{h},101.5,99.5,100.{h},2.25,{close},225.0,7,1.0,100.0,0\n")
}

fn rows(day: i64, hours: std::ops::Range<i64>, per: i64) -> String {
    hours.map(|h| row_at(day, h, per)).collect()
}

fn decode(member: &str, body: &str) -> Result<Vec<Bar>, ArchiveDayRefusal> {
    lane().decode(&zip_of(member, body.as_bytes()), DAY)
}

fn class_of(member: &str, body: &str) -> String {
    decode(member, body).expect_err("this file must be refused").class
}

// ---- the grammar ----------------------------------------------------------------------------

#[test]
fn a_daily_name_is_its_day_and_a_monthly_name_its_month() {
    assert_eq!(kline_zip_days("BTCUSDT-1m-2024-01-15.zip"), Some((DAY, DAY)));
    let jan1 = DAY - 14 * MS_PER_DAY;
    assert_eq!(kline_zip_days("BTCUSDT-1h-2024-01.zip"), Some((jan1, jan1 + 30 * MS_PER_DAY)));
    let feb1 = jan1 + 31 * MS_PER_DAY;
    assert_eq!(kline_zip_days("BTCUSDT-1h-2024-02.zip"), Some((feb1, feb1 + 28 * MS_PER_DAY)));
    for other in [
        "BTCUSDT-1m-2024-01-15.zip.CHECKSUM",
        "BTCUSDT-1m-2024-02-30.zip",
        "BTCUSDT-1m-2024-13.zip",
        "BTCUSDT-2024-01-15.zip",
        "-1m-2024-01-15.zip",
        "BTCUSDT-aggTrades-2024-01.csv",
    ] {
        assert_eq!(kline_zip_days(other), None, "{other}");
    }
}

#[test]
fn an_interval_a_day_cannot_hold_whole_is_refused() {
    for iv in ["1w", "1M", "3d", "7m", "01m", ""] {
        assert!(BinanceKlinesLane::new(SYMBOL, VENDOR, iv).is_err(), "{iv}");
    }
    for iv in ["1s", "1m", "15m", "4h", "1d"] {
        assert!(BinanceKlinesLane::new(SYMBOL, VENDOR, iv).is_ok(), "{iv}");
    }
}

// ---- the rows -------------------------------------------------------------------------------

/// A futures file in milliseconds with its header line and a spot file in microseconds decode to
/// the same bars, each the row as published, base volume and all.
#[test]
fn milliseconds_with_a_header_and_microseconds_without_decode_to_the_same_bars() {
    let ms = decode(&daily(), &format!("{HEADER}{}", rows(DAY, 0..24, 1))).unwrap();
    let us = decode(&daily(), &rows(DAY, 0..24, 1000)).unwrap();
    assert_eq!(ms, us);
    assert_eq!(ms.len(), 24);
    let first = &ms[0];
    assert_eq!((first.timestamp, first.open, first.high, first.low), (DAY, 100.0, 101.5, 99.5));
    assert_eq!((first.close, first.volume, first.funding), (100.0, 2.25, None));
    assert_eq!(ms[23].timestamp, DAY + 23 * HOUR);
}

#[test]
fn a_file_mixing_the_two_units_is_refused_as_ambiguous() {
    let mixed = format!("{}{}", rows(DAY, 0..1, 1), rows(DAY, 1..2, 1000));
    assert_eq!(class_of(&daily(), &mixed), "MixedTimeUnit");
    let mixed = format!("{}{}", rows(DAY, 0..1, 1000), rows(DAY, 1..2, 1));
    assert_eq!(class_of(&daily(), &mixed), "MixedTimeUnit");
}

#[test]
fn every_row_rule_refuses_the_whole_day() {
    let next_day = rows(DAY + MS_PER_DAY, 0..1, 1);
    assert_eq!(class_of(&daily(), &next_day), "OutsideDay");
    assert_eq!(class_of(&daily(), "17,1,1,1,1,1,17,0,0,0,0,0\n"), "OutsideDay");
    let wrong_close = row_at(DAY, 0, 1).replacen(&format!(",{},", DAY + HOUR - 1), ",1,", 1);
    assert_eq!(class_of(&daily(), &wrong_close), "BadCloseTime");
    let off = format!("{},1,1,1,1,1,{},0,0,0,0,0\n", DAY + 1, DAY + HOUR);
    assert_eq!(class_of(&daily(), &off), "OffGrid");
    let backward = format!("{}{}", rows(DAY, 1..2, 1), rows(DAY, 0..1, 1));
    assert_eq!(class_of(&daily(), &backward), "NotIncreasing");
    let twice = format!("{}{}", rows(DAY, 0..1, 1), rows(DAY, 0..1, 1));
    assert_eq!(class_of(&daily(), &twice), "NotIncreasing");
    let unbounded = row_at(DAY, 0, 1).replacen("101.5", "99.9", 1);
    assert_eq!(class_of(&daily(), &unbounded), "BadRow");
    let short = row_at(DAY, 0, 1).replacen(",0\n", "\n", 1);
    assert_eq!(class_of(&daily(), &short), "BadRow");
    let late_header = format!("{}{HEADER}", rows(DAY, 0..1, 1));
    assert_eq!(class_of(&daily(), &late_header), "BadRow");
}

/// Binance's spot 4h file of 2021-04-25 as published (`BTCUSDT-4h-2021-04-25.zip`): the 04:00 bar
/// closes 58,146 ms after it opens, cut short by a maintenance.
const MAINTENANCE_DAY: i64 = 1_619_308_800_000;
const MAINTENANCE_FILE: &str = "\
1619308800000,50047.84000000,50452.85000000,49600.00000000,49615.54000000,6421.00871300,1619323199999,321413769.80197329,127917,3291.66894200,164803285.50893402,0
1619323200000,49626.76000000,49705.04000000,49600.24000000,49683.94000000,5.88703400,1619323258146,292226.01345715,224,4.22924500,209956.01047409,0
1619337600000,49683.95000000,50280.00000000,49161.94000000,49550.73000000,10523.72690600,1619351999999,521764471.26820163,274159,5388.76875300,267100195.82279548,0
1619352000000,49550.74000000,50567.91000000,49450.00000000,50257.97000000,10588.50125500,1619366399999,529768203.87670160,256965,4942.09208500,247301738.27568634,0
1619366400000,50257.97000000,50434.00000000,49295.09000000,49333.01000000,8232.07500600,1619380799999,410376008.77636428,306064,4173.66027300,208078574.78371161,0
1619380800000,49333.01000000,49593.18000000,46930.00000000,49066.77000000,22484.44609000,1619395199999,1084243635.45062163,510150,9966.91209600,480427121.12656493,0
";

/// A bar a maintenance cut short is the venue's candle: kept at its open time, its values as
/// published, in either time unit.
#[test]
fn a_bar_a_maintenance_cut_short_is_kept_as_published() {
    let lane = BinanceKlinesLane::new(VENDOR, VENDOR, "4h").unwrap();
    let blob = zip_of(&format!("{VENDOR}-4h-2021-04-25.csv"), MAINTENANCE_FILE.as_bytes());
    let bars = lane.decode(&blob, MAINTENANCE_DAY).unwrap();
    assert_eq!(bars.len(), 6);
    let cut = &bars[1];
    let opened = MAINTENANCE_DAY + 4 * HOUR;
    assert_eq!(
        (cut.timestamp, cut.open, cut.high, cut.low),
        (opened, 49626.76, 49705.04, 49600.24)
    );
    assert_eq!((cut.close, cut.volume), (49683.94, 5.887034));
    assert_eq!(bars[2].timestamp, MAINTENANCE_DAY + 8 * HOUR, "the next bar keeps its own open");
    let open = DAY * 1000;
    let cut_us = format!("{open},1,1,1,1,1,{},0,0,0,0,0\n", open + 1);
    assert_eq!(decode(&daily(), &cut_us).unwrap()[0].timestamp, DAY);
}

/// Binance's monthly spot 4h file of 2017-09 as published, its rows of 2017-09-06
/// (`BTCUSDT-4h-2017-09.zip`): the 12:00 bar's close is stamped at its open plus exactly 4h, one
/// unit past the bar's last millisecond.
const STAMPED_LATE_DAY: i64 = 1_504_656_000_000;
const STAMPED_LATE_FILE: &str = "\
1504656000000,4366.49000000,4522.81000000,4365.73000000,4430.02000000,187.42056900,1504670399999,836042.03160931,2090,95.93009600,429028.95978482,12032.93128042
1504670400000,4448.10000000,4505.96000000,4335.26000000,4461.29000000,166.30794200,1504684799999,734060.92994898,1510,79.00881300,349102.46296538,11807.54755958
1504684800000,4476.13000000,4597.79000000,4469.82000000,4548.13000000,175.29482000,1504699199999,793406.98682728,1753,98.25274600,445363.81861372,11647.75053621
1504699200000,4544.59000000,4619.47000000,4422.28000000,4619.43000000,261.59679700,1504713600000,1185456.99647709,2560,144.11303000,653752.68879593,11146.71897399
1504728000000,4619.43000000,4662.87000000,4610.00000000,4619.77000000,16.74359800,1504742399999,77379.11058057,43,5.61837800,25983.81075555,11138.37278647
";

/// A bar whose close Binance stamped at its interval's end is the venue's candle, kept as published
/// in either unit; one unit later than that is refused.
#[test]
fn a_close_stamped_at_the_interval_end_is_kept_and_one_unit_later_is_refused() {
    let lane = BinanceKlinesLane::new(VENDOR, VENDOR, "4h").unwrap();
    let blob = zip_of(&format!("{VENDOR}-4h-2017-09.csv"), STAMPED_LATE_FILE.as_bytes());
    let bars = lane.decode(&blob, STAMPED_LATE_DAY).unwrap();
    assert_eq!(bars.len(), 5);
    let late = &bars[3];
    let opened = STAMPED_LATE_DAY + 12 * HOUR;
    assert_eq!(
        (late.timestamp, late.open, late.high, late.low),
        (opened, 4544.59, 4619.47, 4422.28)
    );
    assert_eq!((late.close, late.volume), (4619.43, 261.596797));
    assert_eq!(bars[4].timestamp, STAMPED_LATE_DAY + 20 * HOUR, "the next bar keeps its own open");
    let row = |open: i64, close: i64| format!("{open},1,1,1,1,1,{close},0,0,0,0,0\n");
    assert_eq!(decode(&daily(), &row(DAY * 1000, (DAY + HOUR) * 1000)).unwrap()[0].timestamp, DAY);
    let two_late = STAMPED_LATE_FILE.replacen(",1504713600000,", ",1504713600001,", 1);
    let blob = zip_of(&format!("{VENDOR}-4h-2017-09.csv"), two_late.as_bytes());
    let refusal = lane.decode(&blob, STAMPED_LATE_DAY).expect_err("two units late");
    assert_eq!(refusal.class, "BadCloseTime");
}

/// Only an EARLY close or one at the interval's end is kept: a close later than that, one at or
/// before the open (the zero-length filler bar of Binance's DAILY 2017-09-06 4h spot zip), and an
/// open off the grid stay refused, in either unit.
#[test]
fn a_late_close_a_close_not_after_the_open_and_an_open_off_the_grid_stay_refused() {
    let row = |open: i64, close: i64| format!("{open},1,1,1,1,1,{close},0,0,0,0,0\n");
    assert_eq!(class_of(&daily(), &row(DAY, DAY + HOUR + 1)), "BadCloseTime");
    assert_eq!(class_of(&daily(), &row(DAY * 1000, (DAY + HOUR) * 1000 + 1)), "BadCloseTime");
    assert_eq!(class_of(&daily(), &row(DAY, DAY)), "BadCloseTime");
    assert_eq!(class_of(&daily(), &row(DAY + HOUR, DAY + HOUR - 1)), "BadCloseTime");
    assert_eq!(class_of(&daily(), &row(DAY + 1, DAY + 60_000)), "OffGrid");
}

/// Another symbol's or another interval's file is refused by what it holds, whatever its name.
#[test]
fn a_member_of_another_symbol_or_interval_is_foreign() {
    let body = rows(DAY, 0..24, 1);
    assert_eq!(class_of(&format!("ETHUSDT-{IV}-{LABEL}.csv"), &body), "ForeignMember");
    assert_eq!(class_of(&format!("{VENDOR}-1m-{LABEL}.csv"), &body), "ForeignMember");
}

/// A monthly file hands each day its own bars and refuses a row outside its month.
#[test]
fn a_monthly_file_hands_each_day_its_own_rows() {
    let member = format!("{VENDOR}-{IV}-2024-01.csv");
    let body = format!("{}{}", rows(DAY - MS_PER_DAY, 0..24, 1), rows(DAY, 0..24, 1));
    let bars = decode(&member, &body).unwrap();
    assert_eq!(bars.len(), 24);
    assert!(bars.iter().all(|b| (DAY..DAY + MS_PER_DAY).contains(&b.timestamp)));
    let blob = zip_of(&member, body.as_bytes());
    assert!(lane().decode(&blob, DAY + MS_PER_DAY).unwrap().is_empty(), "no row of the 16th");
    let february = format!("{body}{}", rows(DAY + 20 * MS_PER_DAY, 0..1, 1));
    assert_eq!(class_of(&member, &february), "OutsideDay");
}

// ---- the other candle families ---------------------------------------------------------------

/// 2025-06-01, a UTC midnight: the day the four families were measured.
const FAMILY_DAY: i64 = 1_748_736_000_000;

/// The first three 1m rows of each family's `BTCUSDT-1m-2025-06-01.zip`, and the one row of each
/// `BTCUSDT-1d-2025-06-01.zip`, under `data/futures/um/daily/<family>/BTCUSDT/`, as published (measured
/// 2026-11-17; every family's zip has this name, this member and this header).
const KLINES_1M: &str = "\
1748736000000,104544.60,104599.60,104544.60,104599.50,72.711,1748736059999,7603109.23370,1105,62.555,6540970.47090,0
1748736060000,104599.50,104599.60,104536.30,104554.50,64.235,1748736119999,6716735.01250,1458,22.305,2332036.26560,0
1748736120000,104554.60,104554.60,104476.80,104540.00,206.472,1748736179999,21577414.62510,3414,33.745,3526272.82940,0
";
const MARK_1M: &str = "\
1748736000000,104544.70000000,104599.60000000,104544.60000000,104599.60000000,0,1748736059999,0,60,0,0,0
1748736060000,104599.60000000,104599.60000000,104538.85956667,104554.50000000,0,1748736119999,0,60,0,0,0
1748736120000,104554.50000000,104554.60000000,104487.98836558,104539.90000000,0,1748736179999,0,60,0,0,0
";
const INDEX_1M: &str = "\
1748736000000,104595.52850000,104647.72500000,104595.52850000,104647.70325000,0,1748736059999,0,60,0,0,0
1748736060000,104647.71075000,104648.15900000,104594.94550000,104603.07975000,0,1748736119999,0,60,0,0,0
1748736120000,104603.08800000,104603.32550000,104547.02565217,104587.90521739,0,1748736179999,0,60,0,0,0
";
const KLINES_1D: &str = "1748736000000,104544.60,105823.60,103704.10,105583.30,97420.240,1748822399999,10189734693.27650,1959949,48780.478,5102309101.60380,0\n";
const MARK_1D: &str = "1748736000000,104544.70000000,105807.03132246,103715.70000000,105589.61297464,0,1748822399999,0,86400,0,0,0\n";
const INDEX_1D: &str = "1748736000000,104595.52850000,105865.30934783,103760.89347826,105643.54369565,0,1748822399999,0,86400,0,0,0\n";
const PREMIUM_1D: &str =
    "1748736000000,-0.00048588,0.00110886,-0.00109924,-0.00052419,0,1748822399999,0,17280,0,0,0\n";

/// `body` (the vendor's header, then `rows`) as `BTCUSDT-<interval>-2025-06-01`'s zip, decoded.
fn decode_family(interval: &str, rows: &str) -> Result<Vec<Bar>, ArchiveDayRefusal> {
    let lane = BinanceKlinesLane::new(SYMBOL, VENDOR, interval).unwrap();
    let body = format!("{HEADER}{rows}");
    lane.decode(
        &zip_of(&format!("{VENDOR}-{interval}-2025-06-01.csv"), body.as_bytes()),
        FAMILY_DAY,
    )
}

/// **Mark and index candles are refused by what their rows are, naming the family; the venue's klines
/// read.** The three families share the file name, member and header, so only the rows tell them apart:
/// a volume of 0 beside a count of price samples is no last-price candle.
#[test]
fn mark_and_index_candles_are_refused_by_their_rows_and_the_klines_read() {
    let bars = decode_family("1m", KLINES_1M).unwrap();
    assert_eq!(bars.len(), 3);
    assert_eq!((bars[0].timestamp, bars[0].high, bars[0].volume), (FAMILY_DAY, 104_599.6, 72.711));
    let day = decode_family("1d", KLINES_1D).unwrap();
    assert_eq!((day.len(), day[0].high, day[0].volume), (1, 105_823.6, 97_420.24));

    for (interval, rows, count) in [
        ("1m", MARK_1M, "60"),
        ("1m", INDEX_1M, "60"),
        ("1d", MARK_1D, "86400"),
        ("1d", INDEX_1D, "86400"),
    ] {
        let refusal = decode_family(interval, rows).expect_err("not the venue's candles");
        assert_eq!(refusal.class, NOT_LAST_PRICE, "{interval}: {refusal:?}");
        for needle in ["markPriceKlines", "indexPriceKlines", count, "no commit key was spent"] {
            assert!(refusal.detail.contains(needle), "{needle}: {}", refusal.detail);
        }
    }
    // The premium index is refused as before, by its first value at or below zero.
    assert_eq!(decode_family("1d", PREMIUM_1D).expect_err("a premium file").class, "BadRow");
}

/// **One sampled row in a file of candles refuses nothing, and neither does a day without trades**: a
/// candle's volume is 0 exactly when its count of trades is, so tradeless candles are still candles.
#[test]
fn a_tradeless_day_of_candles_still_reads() {
    let quiet =
        KLINES_1M.replace(",72.711,", ",0,").replace(",7603109.23370,1105,62.555,", ",0,0,0,");
    assert_eq!(decode_family("1m", &quiet).unwrap().len(), 3, "one tradeless minute among trades");
    let tradeless: String = MARK_1M.replace(",0,60,", ",0,0,");
    assert_eq!(decode_family("1m", &tradeless).unwrap().len(), 3, "a day with no trade at all");
    let mixed = format!(
        "{}{}",
        &MARK_1M[..MARK_1M.find('\n').unwrap() + 1],
        &KLINES_1M[KLINES_1M.find('\n').unwrap() + 1..]
    );
    assert_eq!(decode_family("1m", &mixed).unwrap().len(), 3, "not EVERY row is sampled");
}

#[test]
fn a_cut_short_file_is_refused_by_the_zip_reader() {
    let blob = zip_of(&daily(), rows(DAY, 0..24, 1).as_bytes());
    let cut = &blob[..blob.len() / 2];
    let refusal = lane().decode(cut, DAY).expect_err("a partial download");
    assert!(["Truncated", "Corrupt", "BadRow"].contains(&refusal.class.as_str()), "{refusal:?}");
}

// ---- the one-owner table --------------------------------------------------------------------

fn rest_key(a: i64, b: i64) -> String {
    format!("{VENUE}:{SYMBOL}:{IV}:{a}-{b}")
}

#[test]
fn the_one_owner_table_for_the_series_the_rest_lane_and_the_live_path_write() {
    let end = DAY + MS_PER_DAY - 1;
    let own = day_commit_key(SYMBOL, IV, LABEL);
    assert_eq!(lane().owners(&[own]).classify(DAY, LATER), ArchiveDayClass::HeldByArchive);
    let exact = rest_key(DAY, end);
    assert_eq!(lane().owners(&[exact]).classify(DAY, LATER), ArchiveDayClass::HeldByHttp);
    let month = rest_key(DAY - 10 * MS_PER_DAY, DAY + 10 * MS_PER_DAY);
    let table = lane().owners(std::slice::from_ref(&month));
    assert_eq!(table.classify(DAY, LATER), ArchiveDayClass::Overlapped { keys: vec![month] });
    assert_eq!(table.classify(DAY + 20 * MS_PER_DAY, LATER), ArchiveDayClass::Free);
    let stranger = "starter:binance-btcusdt-1h.parquet".to_string();
    let table = lane().owners(std::slice::from_ref(&stranger));
    assert!(matches!(table.classify(DAY, LATER), ArchiveDayClass::Overlapped { .. }));
    // A key of a shape this lane does not read meets every day, the conservative direction. In a
    // store another interval's key sits in another series and never reaches this table, and so do
    // the aggTrades bars, in their own `source=` series.
    let other_interval = format!("{VENUE}:{SYMBOL}:1m:{DAY}-{end}");
    assert!(matches!(
        lane().owners(&[other_interval]).classify(DAY, LATER),
        ArchiveDayClass::Overlapped { .. }
    ));
}

/// The live path saves one key per bar: a touched day keeps one of them, and an untouched one is
/// free.
#[test]
fn a_day_of_one_bar_keys_keeps_one_key() {
    let keys: Vec<String> =
        (0..24).map(|h| rest_key(DAY + h * HOUR, DAY + h * HOUR + HOUR - 1)).collect();
    let table = lane().owners(&keys);
    assert_eq!(
        table.classify(DAY, LATER),
        ArchiveDayClass::Overlapped { keys: vec![keys[0].clone()] }
    );
    assert_eq!(table.classify(DAY + MS_PER_DAY, LATER), ArchiveDayClass::Free);
}

// ---- over a real store ----------------------------------------------------------------------

fn store() -> (tempfile::TempDir, HistoryStoreDataFusion) {
    let dir = tempfile::tempdir().unwrap();
    let hist = HistoryStoreDataFusion::open(dir.path().join("hist")).unwrap();
    (dir, hist)
}

fn file_bars(day: i64) -> Vec<Bar> {
    let label = epoch_ms_to_utc_date(day);
    let blob = zip_of(&format!("{VENDOR}-{IV}-{label}.csv"), rows(day, 0..24, 1).as_bytes());
    lane().decode(&blob, day).unwrap()
}

fn stored(hist: &HistoryStoreDataFusion, day: i64) -> Vec<Bar> {
    let range = TimestampRange::of(day, day + MS_PER_DAY - 1);
    hist.load_bars(VENUE, SYMBOL, IV, range).unwrap()
}

/// What the REST lane would have stored for `day`: the file's bars at `hours`, under one key over
/// the whole day, each close moved so a replacement can be told from it.
fn rest_day(hist: &HistoryStoreDataFusion, day: i64, hours: std::ops::Range<i64>) {
    let bars: Vec<Bar> = file_bars(day)
        .into_iter()
        .filter(|b| hours.contains(&((b.timestamp - day) / HOUR)))
        .map(|b| Bar { close: 7.0, ..b })
        .collect();
    let key = rest_key(day, day + MS_PER_DAY - 1);
    hist.append_bars(VENUE, SYMBOL, IV, &bars, Some(&key)).unwrap();
}

fn import(hist: &HistoryStoreDataFusion) -> ArchiveImport<'_, BinanceKlinesLane> {
    ArchiveImport::open(hist, lane(), &[], LATER).unwrap()
}

/// A free day lands once, as the venue's candles in the sourceless series; a second run reads no
/// file.
#[test]
fn a_free_day_lands_once_in_the_sourceless_series() {
    let (_dir, hist) = store();
    let mut first = import(&hist);
    let result = first.import_day(DAY, || Ok(file_bars(DAY))).unwrap();
    assert_eq!(result, ArchiveDayResult::Imported { ticks: 24, bars: vec![] });
    drop(first);
    assert_eq!(stored(&hist, DAY), file_bars(DAY));
    let keys = hist.series_commits(&kline_series(SYMBOL, IV)).unwrap();
    assert_eq!(keys, vec![day_commit_key(SYMBOL, IV, LABEL)]);
    let mut second = import(&hist);
    let again = second.import_day(DAY, || panic!("a held day's file is never read")).unwrap();
    assert_eq!(again, ArchiveDayResult::ToppedUp { bars: vec![] });
}

/// A plain import leaves a day the REST lane holds as it is, holed or whole.
#[test]
fn a_plain_import_never_touches_a_day_another_lane_holds() {
    let (_dir, hist) = store();
    rest_day(&hist, DAY, 0..20);
    let mut plain = import(&hist);
    assert_eq!(plain.classify_for_import(DAY).unwrap(), ArchiveDayClass::HeldByHttp);
    let result = plain.import_day(DAY, || panic!("a held day's file is never read")).unwrap();
    assert_eq!(result, ArchiveDayResult::ToppedUp { bars: vec![] });
    drop(plain);
    assert_eq!(stored(&hist, DAY).len(), 20);
}

/// `--replace-holed`: the holed day is replaced whole and owned by the archive from then on; the
/// whole day beside it is kept; a repeat reads no file.
#[test]
fn a_holed_day_is_replaced_and_a_whole_day_is_kept() {
    let (_dir, hist) = store();
    let next = DAY + MS_PER_DAY;
    rest_day(&hist, DAY, 0..20);
    rest_day(&hist, next, 0..24);
    let mut replacing = import(&hist).replacing_holed();
    let class = replacing.classify_for_import(DAY).unwrap();
    assert_eq!(
        class,
        ArchiveDayClass::Replace {
            keys: vec![rest_key(DAY, next - 1)],
            stored: 20,
            full: 24,
            rows: 20,
        }
    );
    assert_eq!(replacing.classify_for_import(next).unwrap(), ArchiveDayClass::HeldByHttp);
    let result = replacing.import_day(DAY, || Ok(file_bars(DAY))).unwrap();
    assert_eq!(result, ArchiveDayResult::Replaced { rows: 24, stored: 20, bars: vec![] });
    let kept = replacing.import_day(next, || panic!("a whole day's file is never read")).unwrap();
    assert_eq!(kept, ArchiveDayResult::ToppedUp { bars: vec![] });
    drop(replacing);
    assert_eq!(stored(&hist, DAY), file_bars(DAY), "the archive's day, alone");
    assert!(
        stored(&hist, next).iter().all(|b| b.close == 7.0),
        "the whole day stays the REST lane's"
    );
    let keys = hist.series_commits(&kline_series(SYMBOL, IV)).unwrap();
    assert!(keys.contains(&day_commit_key(SYMBOL, IV, LABEL)));
    assert!(keys.contains(&rest_key(DAY, next - 1)), "the replaced key stays spent");
    let mut again = import(&hist).replacing_holed();
    assert_eq!(again.classify_for_import(DAY).unwrap(), ArchiveDayClass::HeldByArchive);
    let repeat = again.import_day(DAY, || panic!("an owned day's file is never read")).unwrap();
    assert_eq!(repeat, ArchiveDayResult::ToppedUp { bars: vec![] });
}

/// A file holding no more of the day than the store does replaces nothing.
#[test]
fn a_file_no_fuller_than_the_store_replaces_nothing() {
    let (_dir, hist) = store();
    rest_day(&hist, DAY, 0..20);
    let mut replacing = import(&hist).replacing_holed();
    let short: Vec<Bar> = file_bars(DAY).into_iter().take(20).collect();
    let ArchiveDayResult::Refused(refusal) = replacing.import_day(DAY, || Ok(short)).unwrap()
    else {
        panic!("a file no fuller than the store is refused");
    };
    assert_eq!(refusal.class, "ArchiveNotFuller");
    drop(replacing);
    assert!(stored(&hist, DAY).iter().all(|b| b.close == 7.0));
}
