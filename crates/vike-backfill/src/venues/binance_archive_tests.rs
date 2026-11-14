//! The zip reader, the header handling, the continuity check, and the one-owner rule over a real
//! store: a recorded day and a kline day are never stored twice.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;
use vike_data::store::store_kind::STORE_KINDS;
use vike_marketdata::Bar;

use super::decode::{AggEdge, AggLimits, DecodedDay, decode_daily_zip};
use super::*;
use crate::archive_import::{ArchiveDayClass, ArchiveDayResult, ArchiveImport};
use crate::testing::{zip_of, zip_parts};

const SYMBOL: &str = "SOLUSDT.P";
const DATASET: &str = "SOLUSDT";
/// 2024-01-15, a UTC midnight.
const DAY: i64 = 19_737 * MS_PER_DAY;
const LABEL: &str = "2024-01-15";
const LATER: i64 = DAY + 400 * MS_PER_DAY;
const HEADER: &str =
    "agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker\n";

fn member() -> String {
    daily_member_name(DATASET, DAY)
}

/// A row `n` of the fixture: consecutive ids, one second apart, one trade id each.
fn row(n: u64) -> String {
    format!(
        "{},89.{n},3.5,{},{},{},{}\n",
        100 + n,
        500 + n,
        500 + n,
        DAY + n as i64 * 1000,
        n.is_multiple_of(2)
    )
}

fn rows(count: u64) -> String {
    (0..count).map(row).collect()
}

fn decode(blob: &[u8]) -> Result<DecodedDay, ArchiveDayRefusal> {
    decode_daily_zip(blob, &member(), DAY, &AggLimits::DEFAULT)
}

fn class_of(blob: &[u8]) -> String {
    decode(blob).expect_err("this file must be refused").class
}

// ---- the grammar ----------------------------------------------------------------------------

#[test]
fn only_a_daily_name_is_a_day() {
    assert_eq!(daily_zip_day("SOLUSDT-aggTrades-2024-01-15.zip"), Some(DAY));
    for other in [
        "SOLUSDT-aggTrades-2024-01.zip",
        "SOLUSDT-aggTrades-2024-01-15.zip.CHECKSUM",
        "SOLUSDT-aggTrades-2024-02-30.zip",
        "SOLUSDT-trades-2024-01-15.zip",
        "-aggTrades-2024-01-15.zip",
    ] {
        assert_eq!(daily_zip_day(other), None, "{other}");
    }
}

// ---- the zip reader -------------------------------------------------------------------------

#[test]
fn a_file_with_a_header_and_one_without_decode_to_the_same_rows() {
    let plain = decode(&zip_of(&member(), rows(3).as_bytes())).unwrap();
    let headed = decode(&zip_of(&member(), format!("{HEADER}{}", rows(3)).as_bytes())).unwrap();
    assert_eq!(plain.trades, headed.trades);
    assert_eq!(plain.trades.len(), 3);
    let first = &plain.trades[0];
    assert_eq!((first.timestamp, first.local_timestamp), (DAY, 0));
    assert!(first.is_buyer_maker);
    assert_eq!(first.size, 3.5);
    assert!(!plain.trades[1].is_buyer_maker);
}

#[test]
fn a_header_is_only_the_first_line() {
    let late = format!("{}{HEADER}", rows(1));
    assert_eq!(class_of(&zip_of(&member(), late.as_bytes())), "BadRow");
    let other = "agg_trade_id,price\n";
    assert_eq!(class_of(&zip_of(&member(), other.as_bytes())), "BadRow");
}

#[test]
fn a_file_with_no_final_newline_keeps_its_last_row() {
    let body = rows(2);
    let blob = zip_of(&member(), body.trim_end().as_bytes());
    assert_eq!(decode(&blob).unwrap().trades.len(), 2);
}

#[test]
fn anything_but_one_deflated_member_is_refused() {
    let body = rows(2);
    let good = zip_of(&member(), body.as_bytes());
    assert_eq!(decode(&good).unwrap().trades.len(), 2);

    assert_eq!(
        class_of(b"not a zip at all, but long enough to hold a header"),
        "NotOneDeflatedMember"
    );

    let mut crc = flate2::Crc::new();
    crc.update(body.as_bytes());
    let stored = zip_parts(&member(), 0, body.as_bytes(), crc.sum(), 1);
    assert_eq!(class_of(&stored), "NotOneDeflatedMember", "a stored member");

    let mut deflate = DeflateEncoder::new(Vec::new(), Compression::default());
    deflate.write_all(body.as_bytes()).unwrap();
    let stream = deflate.finish().unwrap();
    let two = zip_parts(&member(), 8, &stream, crc.sum(), 2);
    assert_eq!(class_of(&two), "NotOneDeflatedMember", "two members in the directory");

    let mut second_local = zip_parts(&member(), 8, &stream, crc.sum(), 0);
    second_local.truncate(second_local.len() - 22);
    second_local.extend(good);
    assert_eq!(class_of(&second_local), "NotOneDeflatedMember", "a second member follows");
}

#[test]
fn another_instruments_file_is_refused_by_its_member_name() {
    let foreign = zip_of("BTCUSDT-aggTrades-2024-01-15.csv", rows(1).as_bytes());
    assert_eq!(class_of(&foreign), "ForeignMember");
}

#[test]
fn a_stream_that_ends_early_is_truncated_at_any_cut() {
    let body = rows(400);
    let blob = zip_of(&member(), body.as_bytes());
    let name_end = 30 + member().len();
    let directory = blob.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    // Cut inside the stream, the decoder may give up first, and that refuses the day just the same.
    for cut in [name_end + 1, name_end + (directory - name_end) / 2] {
        let class = class_of(&blob[..cut]);
        assert!(matches!(class.as_str(), "Truncated" | "Corrupt"), "cut at {cut}: {class}");
    }
    assert_eq!(class_of(&blob[..directory]), "Truncated", "the whole stream and nothing after it");
}

#[test]
fn a_corrupt_stream_or_a_wrong_crc_is_refused() {
    let body = rows(400);
    let mut crc = flate2::Crc::new();
    crc.update(body.as_bytes());
    let mut deflate = DeflateEncoder::new(Vec::new(), Compression::default());
    deflate.write_all(body.as_bytes()).unwrap();
    let stream = deflate.finish().unwrap();
    let wrong = zip_parts(&member(), 8, &stream, crc.sum() ^ 1, 1);
    assert_eq!(class_of(&wrong), "Corrupt");
}

// ---- the rows -------------------------------------------------------------------------------

#[test]
fn a_row_that_does_not_parse_refuses_the_day_and_names_the_line_never_its_bytes() {
    for bad in [
        "SECRET,89.3,3.5,500,500,{ts},true",
        "1,SECRET,3.5,500,500,{ts},true",
        "1,0,3.5,500,500,{ts},true",
        "1,89.3,-3.5,500,500,{ts},true",
        "1,NaN,3.5,500,500,{ts},true",
        "1,89.3,3.5,501,500,{ts},true",
        "1,89.3,3.5,500,500,{ts},maybe",
        "1,89.3,3.5,500,500,{ts}",
        "1,89.3,3.5,500,500,{ts},true,extra",
        "",
    ] {
        let line = bad.replace("{ts}", &DAY.to_string());
        let body = format!("{}{line}\n{}", rows(1), row(1));
        let refusal = decode(&zip_of(&member(), body.as_bytes())).expect_err(bad);
        assert_eq!(refusal.class, "BadRow", "{bad}");
        assert!(refusal.detail.contains("line 2"), "{}", refusal.detail);
        assert!(!refusal.detail.contains("SECRET"), "{}", refusal.detail);
    }
}

#[test]
fn a_time_in_another_unit_or_outside_the_day_is_refused() {
    for ts in [DAY * 1000, DAY / 1000, DAY - 1, DAY + MS_PER_DAY] {
        let body = format!("1,89.3,3.5,500,500,{ts},true\n");
        assert_eq!(class_of(&zip_of(&member(), body.as_bytes())), "OutsideDay", "{ts}");
    }
}

#[test]
fn a_day_past_the_row_cap_is_refused() {
    let limits = AggLimits { max_rows: 2, ..AggLimits::DEFAULT };
    let blob = zip_of(&member(), rows(3).as_bytes());
    let refusal = decode_daily_zip(&blob, &member(), DAY, &limits).unwrap_err();
    assert_eq!(refusal.class, "DecodedTooLarge");
}

// ---- exact repeats --------------------------------------------------------------------------

/// **A clean day decodes as it did before repeats were looked for**: every row, in order, each field
/// as the fixture wrote it, nothing dropped and nothing to report.
#[test]
fn a_clean_day_decodes_exactly_as_before() {
    let day = decode(&zip_of(&member(), format!("{HEADER}{}", rows(50)).as_bytes())).unwrap();
    let expected: Vec<TickTrade> = (0..50u64)
        .map(|n| TickTrade {
            timestamp: DAY + n as i64 * 1000,
            local_timestamp: 0,
            price: format!("89.{n}").parse().unwrap(),
            size: 3.5,
            is_buyer_maker: n.is_multiple_of(2),
            symbol: String::new(),
        })
        .collect();
    assert_eq!(day.trades, expected);
    assert_eq!(day.duplicates, 0);
    assert_eq!(day.continuity.report(), None);
    assert_eq!(day.report(), None);
}

/// **The measured case, `SOLUSDT-aggTrades-2022-09-11.zip`**: every row twice, each pair identical.
/// Each trade is kept once, the count says how many were dropped, and the ids run on, because the
/// continuity check sees only the kept rows.
#[test]
fn an_exact_repeat_is_dropped_and_counted_wherever_it_sits() {
    let twice: String = (0..5).flat_map(|n| [row(n), row(n)]).collect();
    // One more, far from its first copy.
    let body = format!("{HEADER}{twice}{}", row(1));
    let day = decode(&zip_of(&member(), body.as_bytes())).unwrap();
    assert_eq!(day.trades, ticks(5), "each trade once, in file order");
    assert_eq!(day.duplicates, 6);
    assert_eq!(day.continuity.report(), None, "the kept ids run on");
    assert_eq!(
        day.report().unwrap(),
        "6 rows repeat an earlier row of the file exactly, and are dropped. An import stores each \
         of the 5 trades once, as published."
    );

    // An id out of order first, so the repeat is found by the index rather than the binary search.
    let body = [row(1), row(0), row(1), row(0)].concat();
    let day = decode(&zip_of(&member(), body.as_bytes())).unwrap();
    assert_eq!(day.trades.len(), 2);
    assert_eq!(day.duplicates, 2);
    assert_eq!(day.continuity.repeated_agg_ids, 1, "the out-of-order row is still counted");
    let report = day.report().unwrap();
    assert!(report.starts_with("0 agg_trade_id gaps [] · "), "{report}");
    assert!(report.contains(" · 2 rows repeat an earlier row of the file exactly"), "{report}");
}

/// **A repeated id whose row differs is a contradiction**: whichever field differs, the day is refused
/// by name, on the line that contradicts, out of order or not.
#[test]
fn a_repeated_id_with_a_different_row_refuses_the_day() {
    let t = DAY + 1000;
    for (field, line) in [
        ("price", format!("101,89.2,3.5,501,501,{t},false")),
        ("quantity", format!("101,89.1,3.6,501,501,{t},false")),
        ("first_trade_id", format!("101,89.1,3.5,500,501,{t},false")),
        ("last_trade_id", format!("101,89.1,3.5,501,502,{t},false")),
        ("transact_time", format!("101,89.1,3.5,501,501,{},false", t + 1)),
        ("is_buyer_maker", format!("101,89.1,3.5,501,501,{t},true")),
    ] {
        let in_order = format!("{}{line}\n", rows(3));
        let out_of_order = format!("{}{}{line}\n", row(1), row(0));
        for (body, at) in [(in_order, "line 4"), (out_of_order, "line 3")] {
            let refusal = decode(&zip_of(&member(), body.as_bytes())).expect_err(field);
            assert_eq!(refusal.class, "ContradictoryRepeat", "{field}");
            assert!(refusal.detail.contains(at), "{field}: {}", refusal.detail);
            assert!(refusal.detail.contains("agg_trade_id 101"), "{}", refusal.detail);
            assert!(refusal.detail.contains("no commit key was spent"), "{}", refusal.detail);
        }
    }
}

// ---- the continuity check -------------------------------------------------------------------

#[test]
fn consecutive_ids_report_nothing_and_the_edges_are_the_files_own() {
    let day = decode(&zip_of(&member(), rows(5).as_bytes())).unwrap();
    assert_eq!(day.continuity.report(), None);
    assert_eq!(day.continuity.first.unwrap().agg_trade_id, 100);
    assert_eq!(day.continuity.first.unwrap().trade_id, 500);
    assert_eq!(day.continuity.last.unwrap().agg_trade_id, 104);
    assert_eq!(day.continuity.last.unwrap().trade_id, 504);
}

/// A repeated id inside one file never gets here: the decode drops or refuses it (the exact-repeat
/// tests below). An id below the one before, never seen, is out of order, and that is counted.
#[test]
fn a_gap_an_id_out_of_order_and_a_backward_time_are_counted_and_every_row_is_kept() {
    let ts = |s: i64| DAY + s * 1000;
    let body = [
        format!("10,1.0,1.0,100,101,{},true\n", ts(5)),
        // agg ids jump by 3 (2 missing), trade ids by 5 (4 missing)
        format!("13,1.0,1.0,106,106,{},true\n", ts(6)),
        format!("14,1.0,1.0,107,107,{},true\n", ts(7)),
        // the agg id goes back, its trade id repeats, and the time goes backward
        format!("12,1.0,1.0,106,106,{},true\n", ts(4)),
    ]
    .concat();
    let day = decode(&zip_of(&member(), body.as_bytes())).unwrap();
    assert_eq!(day.trades.len(), 4, "a report filters nothing");
    let c = &day.continuity;
    assert_eq!((c.agg_gaps, c.trade_gaps), (1, 1));
    assert_eq!((c.repeated_agg_ids, c.repeated_trade_ids, c.backward_timestamp), (1, 1, 1));
    assert_eq!(c.agg_listed[0].missing(), 2);
    assert_eq!((c.agg_listed[0].after, c.agg_listed[0].next, c.agg_listed[0].ts), (10, 13, ts(6)));
    assert_eq!(c.trade_listed[0].missing(), 4);
    let report = c.report().unwrap();
    assert!(report.contains("1 agg_trade_id gaps [10 -> 13 (2 missing"), "{report}");
    assert!(report.contains("1 backward timestamps"), "{report}");
}

/// A real file skips trade ids as a matter of course (10,351 rows of 348,395 on SOLUSDT 2026-11-04), so
/// that alone is counted and reported and needs no operator. Each of the others does.
#[test]
fn a_skipped_trade_id_alone_is_counted_and_breaks_nothing() {
    let ts = |s: i64| DAY + s * 1000;
    let row =
        |id: u64, first: u64, s: i64| format!("{id},1.0,1.0,{first},{first},{},true\n", ts(s));
    let day = |body: String| decode(&zip_of(&member(), body.as_bytes())).unwrap().continuity;

    let skipped = day([row(1, 100, 1), row(2, 102, 2)].concat());
    assert_eq!((skipped.agg_gaps, skipped.trade_gaps), (0, 1));
    assert!(!skipped.is_broken());
    assert!(skipped.report().unwrap().contains("1 trade id gaps [100 -> 102 (1 missing"));

    assert!(!day([row(1, 100, 1), row(2, 101, 2)].concat()).is_broken());
    assert!(day([row(1, 100, 1), row(3, 101, 2)].concat()).is_broken(), "an aggregated trade gone");
    assert!(day([row(2, 100, 1), row(1, 101, 2)].concat()).is_broken(), "one out of order");
    assert!(day([row(1, 100, 1), row(2, 100, 2)].concat()).is_broken(), "a trade id repeated");
    assert!(day([row(1, 100, 2), row(2, 101, 1)].concat()).is_broken(), "a time going backward");
}

#[test]
fn a_gap_between_two_files_is_counted_on_the_later_one() {
    let mut day = decode(&zip_of(&member(), rows(2).as_bytes())).unwrap().continuity;
    day.note_previous_file(AggEdge { agg_trade_id: 99, trade_id: 499, ts: DAY - 1 });
    assert_eq!(day.report(), None, "the file before it ends where this one starts");
    day.note_previous_file(AggEdge { agg_trade_id: 90, trade_id: 480, ts: DAY - 1 });
    assert_eq!((day.agg_gaps, day.trade_gaps), (1, 1));
    assert_eq!(day.agg_listed[0].missing(), 9);
    assert_eq!(day.trade_listed[0].missing(), 19);
}

// ---- the one-owner table --------------------------------------------------------------------

fn lane() -> BinanceArchiveLane {
    BinanceArchiveLane::new(SYMBOL)
}

fn live_key(first: i64, last: i64, seq: u64) -> String {
    format!("live-binance-{SYMBOL}-trade-{first}-{last}-{seq}")
}

/// The recorder's template is the store's `trade` row's, and `store_kind_gate` holds that row to the
/// writer, so a change of the recorder's spelling reddens here rather than reading its keys as a
/// stranger's. A stranger's key would still refuse the day, but it would refuse every day.
#[test]
fn the_recorder_template_is_the_one_this_lane_reads() {
    let row = STORE_KINDS.iter().find(|k| k.kind == StoreKindId::Trade).unwrap();
    assert!(
        row.commit_keys
            .iter()
            .any(|ck| ck.template
                == "live-{venue}-{symbol}-{}-{first_timestamp}-{last_timestamp}-{seq}"),
        "the recorder's trade key changed: fix `recorder_key_head` and `recorder_range`"
    );
    assert_eq!(recorder_key_head(SYMBOL), format!("live-{VENUE}-{SYMBOL}-trade-"));
}

#[test]
fn the_one_owner_table_for_a_series_this_lane_and_the_recorder_share() {
    let own = day_commit_key(SYMBOL, LABEL);
    let held = lane().owners(std::slice::from_ref(&own));
    assert_eq!(held.classify(DAY, LATER), ArchiveDayClass::HeldByArchive);
    assert_eq!(held.classify(DAY + MS_PER_DAY, LATER), ArchiveDayClass::Free);

    let recorded = live_key(DAY + 5, DAY + 9, 1);
    let table = lane().owners(std::slice::from_ref(&recorded));
    assert_eq!(table.classify(DAY, LATER), ArchiveDayClass::Overlapped { keys: vec![recorded] });
    assert_eq!(table.classify(DAY - MS_PER_DAY, LATER), ArchiveDayClass::Free);
    assert_eq!(table.classify(DAY + MS_PER_DAY, LATER), ArchiveDayClass::Free);

    let across = live_key(DAY + MS_PER_DAY - 5, DAY + MS_PER_DAY + 5, 2);
    let table = lane().owners(std::slice::from_ref(&across));
    assert!(matches!(table.classify(DAY, LATER), ArchiveDayClass::Overlapped { .. }));
    assert!(matches!(table.classify(DAY + MS_PER_DAY, LATER), ArchiveDayClass::Overlapped { .. }));

    let stranger = "tardis:trades:SOLUSDT.P:2020-01-01".to_string();
    let table = lane().owners(std::slice::from_ref(&stranger));
    assert!(matches!(table.classify(DAY, LATER), ArchiveDayClass::Overlapped { .. }));
    assert!(matches!(
        table.classify(LATER - 2 * MS_PER_DAY, LATER),
        ArchiveDayClass::Overlapped { .. }
    ));
}

/// A day's plan row carries one key of the thousands a recorded day spends: see the module doc.
#[test]
fn a_recorded_day_keeps_one_key_however_many_flushes_it_took() {
    let keys: Vec<String> =
        (0..5_000).map(|seq| live_key(DAY + seq, DAY + seq, seq as u64)).collect();
    let ArchiveDayClass::Overlapped { keys: shown } = lane().owners(&keys).classify(DAY, LATER)
    else {
        panic!("a recorded day is overlapped");
    };
    assert_eq!(shown, vec![keys[0].clone()]);
}

#[test]
fn a_day_inside_the_margin_is_too_recent() {
    let table = lane().owners(&[]);
    let end = DAY + MS_PER_DAY - 1;
    assert_eq!(table.classify(DAY, end + PUBLICATION_MARGIN_MS), ArchiveDayClass::TooRecent);
    assert_eq!(table.classify(DAY, end + PUBLICATION_MARGIN_MS + 1), ArchiveDayClass::Free);
}

// ---- over a real store ----------------------------------------------------------------------

fn store() -> (tempfile::TempDir, HistoryStoreDataFusion) {
    let dir = tempfile::tempdir().unwrap();
    let hist = HistoryStoreDataFusion::open(dir.path().join("hist")).unwrap();
    (dir, hist)
}

fn ticks(count: u64) -> Vec<TickTrade> {
    decode(&zip_of(&member(), rows(count).as_bytes())).unwrap().trades
}

fn session(hist: &HistoryStoreDataFusion) -> ArchiveImport<'_, BinanceArchiveLane> {
    ArchiveImport::open(hist, lane(), &[], LATER).unwrap()
}

fn stored(hist: &HistoryStoreDataFusion) -> usize {
    hist.scan_trades(VENUE, SYMBOL, TimestampRange::all()).unwrap().len()
}

/// A day lands once, every row as the file holds it and no bar; a second run reads no file.
#[test]
fn a_day_lands_once_as_trades_alone_and_a_second_run_reads_no_file() {
    let (_dir, hist) = store();
    let mut first = session(&hist);
    assert_eq!(first.classify(DAY), ArchiveDayClass::Free);
    let result = first.import_day(DAY, || Ok(ticks(120))).unwrap();
    assert_eq!(result, ArchiveDayResult::Imported { ticks: 120, bars: vec![] });
    assert_eq!(
        first.classify(DAY),
        ArchiveDayClass::HeldByArchive,
        "the session saw its own write"
    );
    drop(first);
    let back = hist.scan_trades(VENUE, SYMBOL, TimestampRange::all()).unwrap();
    let written = ticks(120);
    assert_eq!(back.len(), written.len());
    for (read, row) in back.iter().zip(&written) {
        let row_as_read = TickTrade { symbol: SYMBOL.to_string(), ..row.clone() };
        assert_eq!(*read, row_as_read);
    }
    let keys = hist.series_commits(&trade_series(SYMBOL)).unwrap();
    assert_eq!(keys, vec![day_commit_key(SYMBOL, LABEL)]);
    assert!(hist.list_series().unwrap().iter().all(|s| s.kind == StoreKindId::Trade), "no bar");

    let mut second = session(&hist);
    assert_eq!(second.classify(DAY), ArchiveDayClass::HeldByArchive);
    let again = second.import_day(DAY, || panic!("a held day's file is never read")).unwrap();
    assert_eq!(again, ArchiveDayResult::ToppedUp { bars: vec![] });
    assert_eq!(stored(&hist), 120);
}

/// A day whose file holds every row twice stores each trade once, under its one day key.
#[test]
fn a_day_with_exact_repeats_imports_each_trade_once() {
    let (_dir, hist) = store();
    let twice: String = (0..120).flat_map(|n| [row(n), row(n)]).collect();
    let blob = zip_of(&member(), twice.as_bytes());
    let day = decode(&blob).unwrap();
    assert_eq!(day.duplicates, 120);
    let mut import = session(&hist);
    let result = import.import_day(DAY, || decode(&blob).map(|d| d.trades)).unwrap();
    assert_eq!(result, ArchiveDayResult::Imported { ticks: 120, bars: vec![] });
    drop(import);
    let back = hist.scan_trades(VENUE, SYMBOL, TimestampRange::all()).unwrap();
    let once: Vec<TickTrade> =
        ticks(120).into_iter().map(|t| TickTrade { symbol: SYMBOL.to_string(), ..t }).collect();
    assert_eq!(back, once);
    let keys = hist.series_commits(&trade_series(SYMBOL)).unwrap();
    assert_eq!(keys, vec![day_commit_key(SYMBOL, LABEL)]);
}

/// A day whose file contradicts itself stores nothing and spends no key, so it stays free.
#[test]
fn a_day_with_a_contradictory_repeat_is_refused_and_stores_nothing() {
    let (_dir, hist) = store();
    let body = format!("{}{}", rows(120), row(7).replace("3.5", "3.6"));
    let blob = zip_of(&member(), body.as_bytes());
    let mut import = session(&hist);
    let result = import.import_day(DAY, || decode(&blob).map(|d| d.trades)).unwrap();
    let ArchiveDayResult::Refused(refusal) = result else {
        panic!("{result:?}");
    };
    assert_eq!(refusal.class, "ContradictoryRepeat");
    assert_eq!(import.classify(DAY), ArchiveDayClass::Free, "the day stays free");
    drop(import);
    assert_eq!(stored(&hist), 0);
    assert!(hist.series_commits(&trade_series(SYMBOL)).unwrap().is_empty(), "no key spent");
}

fn session_with_bars<'h>(
    hist: &'h HistoryStoreDataFusion,
    bars: &[&str],
) -> ArchiveImport<'h, BinanceArchiveLane> {
    let bars: Vec<String> = bars.iter().map(|b| (*b).to_string()).collect();
    ArchiveImport::open(hist, lane(), &bars, LATER).unwrap()
}

fn lane_bars(hist: &HistoryStoreDataFusion, interval: &str) -> Vec<Bar> {
    hist.load_bars_sourced_capped(
        VENUE,
        AGGTRADES_LANE,
        SYMBOL,
        interval,
        TimestampRange::all(),
        None,
    )
    .unwrap()
}

/// **The candles stay untouched.** Both versions of one minute, with different values: the venue's
/// candle in the sourceless series, and the bar built from trades in the lane. A plain read answers the
/// candle, a named read answers the lane, and the listing that drives a chart shows no lane.
#[test]
fn a_plain_read_answers_the_candle_and_a_named_read_the_lane() {
    let (_dir, hist) = store();
    let candle = Bar {
        timestamp: DAY,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    let window = format!("binance:{SYMBOL}:1m:{DAY}-{}", DAY + MS_PER_DAY);
    hist.append_bars(VENUE, SYMBOL, "1m", std::slice::from_ref(&candle), Some(&window)).unwrap();

    let mut import = session_with_bars(&hist, &["1m"]);
    assert_eq!(import.classify(DAY), ArchiveDayClass::Free, "a candle does not own a trade day");
    let result = import.import_day(DAY, || Ok(ticks(120))).unwrap();
    assert_eq!(
        result,
        ArchiveDayResult::Imported {
            ticks: 120,
            bars: vec![ArchiveDayBars { interval: "1m".into(), rows: 2 }]
        }
    );
    drop(import);

    assert_eq!(
        hist.load_bars(VENUE, SYMBOL, "1m", TimestampRange::all()).unwrap(),
        vec![candle.clone()],
        "the plain read is the candle, alone"
    );
    let series = SeriesId::per_symbol(StoreKindId::Bar, VENUE, SYMBOL, Some("1m".to_string()));
    assert_eq!(hist.series_commits(&series).unwrap(), vec![window], "its keys are its own");

    let built = lane_bars(&hist, "1m");
    assert_eq!(built.len(), 2, "two minutes of trades");
    assert_eq!(built[0].timestamp, DAY);
    assert_ne!(built[0].open, candle.open, "the two versions of the minute differ");
    assert_eq!(built[0].volume, 60.0 * 3.5);
    let keys = hist.series_commits(&bar_lane_series(SYMBOL, "1m")).unwrap();
    assert_eq!(keys, vec![bar_commit_key(SYMBOL, "1m", LABEL)]);

    let inventory = hist.inventory().unwrap();
    assert!(inventory.iter().all(|(id, _)| id.source.is_none()), "{inventory:?}");
    assert!(hist.list_series().unwrap().contains(&bar_lane_series(SYMBOL, "1m")));
}

/// A held day's missing bars are built from the stored trades with no file read, which is how a run
/// killed between a day's trades and its bars resumes, and a repeat writes none twice.
#[test]
fn a_held_days_missing_bars_are_topped_up_in_the_lane() {
    let (_dir, hist) = store();
    let mut first = session(&hist);
    assert_eq!(
        first.import_day(DAY, || Ok(ticks(120))).unwrap(),
        ArchiveDayResult::Imported { ticks: 120, bars: vec![] }
    );
    drop(first);
    assert!(lane_bars(&hist, "1m").is_empty(), "no bar unless asked");

    let mut second = session_with_bars(&hist, &["1m", "1h"]);
    let topped = second.import_day(DAY, || panic!("a held day's file is never read")).unwrap();
    assert_eq!(
        topped,
        ArchiveDayResult::ToppedUp {
            bars: vec![
                ArchiveDayBars { interval: "1m".into(), rows: 2 },
                ArchiveDayBars { interval: "1h".into(), rows: 1 },
            ]
        }
    );
    drop(second);

    let mut third = session_with_bars(&hist, &["1m", "1h"]);
    let again = third.import_day(DAY, || panic!("never read")).unwrap();
    assert_eq!(
        again,
        ArchiveDayResult::ToppedUp {
            bars: vec![
                ArchiveDayBars { interval: "1m".into(), rows: 0 },
                ArchiveDayBars { interval: "1h".into(), rows: 0 },
            ]
        }
    );
    assert_eq!(lane_bars(&hist, "1m").len(), 2);
    assert_eq!(stored(&hist), 120);
}

/// A recorded day has no bars from this lane either: its file was never read and nothing is derived
/// from the recorder's partial tape.
#[test]
fn a_recorded_day_derives_no_bar() {
    let (_dir, hist) = store();
    let recorded = &ticks(3)[..];
    let key = live_key(recorded[0].timestamp, recorded[2].timestamp, 7);
    hist.append_trades(VENUE, SYMBOL, recorded, Some(&key)).unwrap();

    let mut import = session_with_bars(&hist, &["1m"]);
    let result = import.import_day(DAY, || panic!("never read")).unwrap();
    assert!(matches!(result, ArchiveDayResult::Refused(ref r) if r.class == "Overlapped"));
    assert!(lane_bars(&hist, "1m").is_empty());
}

/// **The one-owner rule against the live recorder.** A recorded day is refused, its file unread, and
/// the store holds exactly the recorded rows afterwards; the day beside it imports as usual.
#[test]
fn a_day_the_recorder_wrote_is_never_stored_twice() {
    let (_dir, hist) = store();
    let recorded = &ticks(3)[..];
    let key = live_key(recorded[0].timestamp, recorded[2].timestamp, 7);
    hist.append_trades(VENUE, SYMBOL, recorded, Some(&key)).unwrap();

    let mut import = session(&hist);
    assert_eq!(import.coverage().rows, 3, "the plan's size is the recorder's rows");
    assert_eq!(import.classify(DAY), ArchiveDayClass::Overlapped { keys: vec![key.clone()] });
    let result = import.import_day(DAY, || panic!("a recorded day's file is never read")).unwrap();
    let ArchiveDayResult::Refused(refusal) = result else {
        panic!("{result:?}");
    };
    assert_eq!(refusal.class, "Overlapped");
    assert!(refusal.detail.contains(&key), "{}", refusal.detail);
    assert_eq!(stored(&hist), 3, "the recorded rows and no more");
    assert_eq!(hist.series_commits(&trade_series(SYMBOL)).unwrap(), vec![key]);
    assert_eq!(import.classify(DAY + MS_PER_DAY), ArchiveDayClass::Free, "the next day is free");
}
