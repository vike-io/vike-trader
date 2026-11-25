//! The volatility index's decode over rows copied from the vendor's
//! `BTCBVOLUSDT-BVOLIndex-2023-10-23.csv`, its refusals, and the day over a real store.

use vike_data::TimestampRange;

use super::*;
use crate::archive_import::{ArchiveDayClass, ArchiveDayResult, ArchiveImport};
use crate::testing::zip_of;
use crate::venues::binance_archive::dataset_zip_day;

const SYMBOL: &str = "BTCBVOLUSDT";
/// 2023-10-23, a UTC midnight: the first row's `calc_time`.
const DAY: i64 = 1_698_019_200_000;
const LABEL: &str = "2023-10-23";
const LATER: i64 = DAY + 400 * MS_PER_DAY;

const ROWS: &str = "\
1698019200000,BTCBVOLUSDT,BTCBVOL,USDT,50.4646
1698019201000,BTCBVOLUSDT,BTCBVOL,USDT,50.4647
1698019202000,BTCBVOLUSDT,BTCBVOL,USDT,50.4649
";

fn member() -> String {
    dataset_member_name(SYMBOL, FILE_WORD, DAY)
}

fn decode(body: &str) -> Result<Vec<TickTrade>, ArchiveDayRefusal> {
    decode_index_zip(&zip_of(&member(), format!("{HEADER_LINE}\n{body}").as_bytes()), SYMBOL, DAY)
}

fn class_of(body: &str) -> String {
    decode(body).expect_err("this file must be refused").class
}

#[test]
fn only_a_daily_index_name_is_a_day() {
    assert_eq!(dataset_zip_day("BTCBVOLUSDT-BVOLIndex-2023-10-23.zip", FILE_WORD), Some(DAY));
    assert_eq!(dataset_zip_day("BTCBVOLUSDT-BVOLIndex-2023-10.zip", FILE_WORD), None);
}

/// One synthetic tick per value, DVOL's shape: the index as the price, size 0, the time twice.
#[test]
fn a_day_decodes_to_one_index_tick_per_value() {
    let ticks = decode(ROWS).unwrap();
    assert_eq!(ticks.len(), 3);
    assert_eq!(
        ticks[0],
        TickTrade {
            timestamp: DAY,
            local_timestamp: DAY,
            price: 50.4646,
            size: 0.0,
            is_buyer_maker: false,
            symbol: Default::default(),
        }
    );
    assert_eq!(ticks[2].timestamp, DAY + 2_000);
    let headless = decode_index_zip(&zip_of(&member(), ROWS.as_bytes()), SYMBOL, DAY).unwrap();
    assert_eq!(headless, ticks, "the header line is optional");
}

#[test]
fn a_row_out_of_order_or_out_of_the_day_refuses_the_day() {
    let first = ROWS.lines().next().unwrap();
    let second = ROWS.lines().nth(1).unwrap();
    assert_eq!(decode(&format!("{first}\n{first}\n")).unwrap().len(), 1, "an exact repeat drops");
    let other_value = first.replacen("50.4646", "50.5", 1);
    assert_eq!(class_of(&format!("{first}\n{other_value}\n")), "ContradictoryRepeat");
    assert_eq!(class_of(&format!("{second}\n{first}\n")), "NotIncreasing");
    let next_day = first.replacen("1698019200000", "1698105600000", 1);
    assert_eq!(class_of(&next_day), "OutsideDay");
    for bad in [
        first.replacen("BTCBVOLUSDT", "ETHBVOLUSDT", 1),
        first.replacen("50.4646", "-1", 1),
        first.replacen("50.4646", "", 1),
        first.replacen("1698019200000", "x", 1),
        format!("{first},extra"),
    ] {
        assert_eq!(class_of(&bad), "BadRow", "{bad}");
    }
}

#[test]
fn a_day_lands_once_as_the_index_trades() {
    let dir = tempfile::tempdir().unwrap();
    let hist = HistoryStoreDataFusion::open(dir.path().join("hist")).unwrap();
    let lane = || VolatilityIndexLane::for_dataset(SYMBOL).unwrap();
    let mut first = ArchiveImport::open(&hist, lane(), &[], LATER).unwrap();
    let result = first.import_day(DAY, || decode(ROWS)).unwrap();
    assert_eq!(result, ArchiveDayResult::Imported { ticks: 3, bars: vec![] });
    drop(first);
    let back = hist.scan_trades(VENUE, SYMBOL, TimestampRange::all()).unwrap();
    assert_eq!(
        back.iter().map(|t| (t.timestamp, t.price)).collect::<Vec<_>>(),
        vec![(DAY, 50.4646), (DAY + 1_000, 50.4647), (DAY + 2_000, 50.4649)]
    );
    let keys = hist.series_commits(&index_series(SYMBOL)).unwrap();
    assert_eq!(keys, vec![day_commit_key(SYMBOL, LABEL)]);
    let second = ArchiveImport::open(&hist, lane(), &[], LATER).unwrap();
    assert_eq!(second.classify(DAY), ArchiveDayClass::HeldByArchive);
}
