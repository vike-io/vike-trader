//! The two option-market formats through the lane's own verb, over a real store: a summary day
//! lands as chain rows a point-in-time read answers, an index day as trades, a zip whose `.CHECKSUM`
//! does not vouch for it is refused, and a request naming bars is refused before the folder is read.

use std::path::PathBuf;
use std::sync::Arc;

use vike_backfill::testing::zip_of;
use vike_backfill::venues::binance_archive::dataset_member_name;
use vike_backfill::venues::binance_archive::options::{summary, volatility_index};
use vike_data::archive_day::{BINANCE_OPTION_SUMMARY_FORMAT, BINANCE_VOLATILITY_INDEX_FORMAT};
use vike_data::{HistoryStore, TimestampRange};
use vike_datahub_client::archive::{ImportDone, ImportSpec};
use vike_datahub_client::proto::Response;
use vike_model::MS_PER_DAY;

use super::super::binance_klines::CHECKSUM_MISMATCH;
use super::*;
use crate::import::{COMPANION_MISSING, ImportLane, import_archive_verb};

/// 2023-10-23, a UTC midnight.
const DAY: i64 = 1_698_019_200_000;
const HOUR: i64 = 3_600_000;

const SUMMARY_ROWS: &str = "\
date,hour,symbol,underlying,type,strike,open,high,low,close,volume_contracts,volume_usdt,best_bid_price,best_ask_price,best_bid_qty,best_ask_qty,best_buy_iv,best_sell_iv,mark_price,mark_iv,delta,gamma,vega,theta,openinterest_contracts,openinterest_usdt
2023-10-23,01,BTC-231229-34000-C,BTCUSDT,C,231229-34000,725.00000000,725.00000000,725.00000000,725.00000000,0E-8,0E-8,1430.00000000,1460.00000000,2.83000000,2.35000000,\"\",0.53434968,1444.90000000,0.53123981,0.34994338,0.00005354,48.23448652,-19.05135683,4.98000000,150940.70036045
2023-10-23,00,BTC-231027-33000-C,BTCUSDT,C,231027-33000,75.00000000,100.00000000,65.00000000,90.00000000,29.71000000,29.71000000,80.00000000,90.00000000,8.71000000,16.48000000,\"\",0.62905252,84.90000000,0.61981410,0.09250348,0.00008187,5.41260825,-39.08507011,44.79000000,1349056.18015695
";

const INDEX_ROWS: &str = "\
calc_time,symbol,base_asset,quote_asset,index_value
1698019200000,BTCBVOLUSDT,BTCBVOL,USDT,50.4646
1698019201000,BTCBVOLUSDT,BTCBVOL,USDT,50.4647
";

/// A project in a temporary directory: an imports root, and a real store beside it.
struct Project {
    tmp: tempfile::TempDir,
    store: Arc<HistoryStoreDataFusion>,
}

impl Project {
    fn new() -> Project {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let store =
            Arc::new(HistoryStoreDataFusion::open(tmp.path().join("hist")).expect("a store"));
        Project { tmp, store }
    }

    fn folder(&self, format: &str, dataset: &str) -> PathBuf {
        let dir = self.tmp.path().join("imports").join(format).join(dataset);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The day's zip of `rows`, named as the vendor names it, and, when `checked`, its `.CHECKSUM`.
    fn put_day(&self, format: &str, word: &str, dataset: &str, rows: &str, checked: bool) {
        let member = dataset_member_name(dataset, word, DAY);
        let zip = zip_of(&member, rows.as_bytes());
        let name = member.replace(".csv", ".zip");
        let folder = self.folder(format, dataset);
        std::fs::write(folder.join(&name), &zip).unwrap();
        if checked {
            let line = format!("{}  {name}", vike_hub_protocol::hash::sha256_hex(&zip));
            std::fs::write(folder.join(format!("{name}.CHECKSUM")), line).unwrap();
        }
    }

    /// A tampered `.CHECKSUM`: the right line for other bytes.
    fn put_wrong_checksum(&self, format: &str, word: &str, dataset: &str) {
        let name = dataset_member_name(dataset, word, DAY).replace(".csv", ".zip");
        let line = format!("{}  {name}", vike_hub_protocol::hash::sha256_hex(b"other bytes"));
        std::fs::write(self.folder(format, dataset).join(format!("{name}.CHECKSUM")), line)
            .unwrap();
    }

    fn answer(&self, spec: &ImportSpec) -> Response {
        let root = self.tmp.path().join("imports");
        let formats: Vec<Box<dyn ArchiveFormat>> = vec![
            Box::new(BinanceOptionArchive::<summary::OptionSummaryLane>::new(Arc::clone(
                &self.store,
            ))),
            Box::new(BinanceOptionArchive::<volatility_index::VolatilityIndexLane>::new(
                Arc::clone(&self.store),
            )),
        ];
        import_archive_verb(spec, Some(&ImportLane::new(root, formats)), &|| false)
    }

    fn run(&self, spec: &ImportSpec) -> ImportDone {
        match self.answer(spec) {
            Response::ArchiveImported(done) => *done,
            other => panic!("not an import's answer: {other:?}"),
        }
    }
}

fn spec(format: &str, dataset: &str, dry_run: bool) -> ImportSpec {
    ImportSpec {
        format: format.to_string(),
        dataset: dataset.to_string(),
        from_day: Some(DAY),
        to_day: Some(DAY),
        bars: Vec::new(),
        dry_run,
        verify: false,
        replace_holed: false,
        target: None,
    }
}

fn result(done: &ImportDone) -> &ArchiveDayResultWire {
    let outcome = done.outcome.as_ref().expect("an outcome");
    &outcome.days.iter().find(|d| d.day == DAY).expect("the day").result
}

/// A summary day lands as chain rows, each hour's at its last millisecond, and a point-in-time read
/// at 01:30 knows hour 00 and not hour 01. A second run finds the day held and reads no file.
#[test]
fn a_summary_day_lands_as_a_chain_a_study_reads_point_in_time() {
    let project = Project::new();
    project.put_day(
        BINANCE_OPTION_SUMMARY_FORMAT,
        summary::FILE_WORD,
        "BTCUSDT",
        SUMMARY_ROWS,
        true,
    );
    let plan = project.run(&spec(BINANCE_OPTION_SUMMARY_FORMAT, "BTCUSDT", true));
    assert!(plan.plan.admission.contains("point-in-time"), "{}", plan.plan.admission);
    let done = project.run(&spec(BINANCE_OPTION_SUMMARY_FORMAT, "BTCUSDT", false));
    assert!(matches!(result(&done), ArchiveDayResultWire::Imported { ticks: 2, .. }), "{done:?}");
    let rows = project.store.scan_chain(VENUE, "BTCUSDT", TimestampRange::all()).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.ts).collect::<Vec<_>>(),
        vec![DAY + HOUR - 1, DAY + 2 * HOUR - 1]
    );
    let at = project.store.chain_as_of_within(VENUE, "BTCUSDT", DAY + 90 * 60_000, HOUR).unwrap();
    assert_eq!(at.len(), 1);
    assert_eq!(at[0].instrument, "BTC-231027-33000-C");

    let again = project.run(&spec(BINANCE_OPTION_SUMMARY_FORMAT, "BTCUSDT", false));
    assert!(matches!(result(&again), ArchiveDayResultWire::ToppedUp { .. }), "{again:?}");
    assert_eq!(project.store.scan_chain(VENUE, "BTCUSDT", TimestampRange::all()).unwrap(), rows);
}

/// An index day lands as one synthetic tick per value under the index's own symbol.
#[test]
fn an_index_day_lands_as_trades() {
    let project = Project::new();
    project.put_day(
        BINANCE_VOLATILITY_INDEX_FORMAT,
        volatility_index::FILE_WORD,
        "BTCBVOLUSDT",
        INDEX_ROWS,
        true,
    );
    let done = project.run(&spec(BINANCE_VOLATILITY_INDEX_FORMAT, "BTCBVOLUSDT", false));
    assert!(matches!(result(&done), ArchiveDayResultWire::Imported { ticks: 2, .. }), "{done:?}");
    let range = TimestampRange::of(DAY, DAY + MS_PER_DAY - 1);
    let ticks = project.store.scan_trades(VENUE, "BTCBVOLUSDT", range).unwrap();
    assert_eq!(
        ticks.iter().map(|t| (t.price, t.size)).collect::<Vec<_>>(),
        vec![(50.4646, 0.0), (50.4647, 0.0)]
    );
}

/// A zip without its `.CHECKSUM`, or with one for other bytes, is refused and writes nothing.
#[test]
fn a_zip_the_checksum_does_not_vouch_for_is_refused() {
    let project = Project::new();
    project.put_day(
        BINANCE_OPTION_SUMMARY_FORMAT,
        summary::FILE_WORD,
        "BTCUSDT",
        SUMMARY_ROWS,
        false,
    );
    let done = project.run(&spec(BINANCE_OPTION_SUMMARY_FORMAT, "BTCUSDT", false));
    assert!(
        matches!(result(&done), ArchiveDayResultWire::Refused(r) if r.class == COMPANION_MISSING),
        "{done:?}"
    );
    project.put_wrong_checksum(BINANCE_OPTION_SUMMARY_FORMAT, summary::FILE_WORD, "BTCUSDT");
    let done = project.run(&spec(BINANCE_OPTION_SUMMARY_FORMAT, "BTCUSDT", false));
    assert!(
        matches!(result(&done), ArchiveDayResultWire::Refused(r) if r.class == CHECKSUM_MISMATCH),
        "{done:?}"
    );
    assert!(project.store.scan_chain(VENUE, "BTCUSDT", TimestampRange::all()).unwrap().is_empty());
}

/// Bars and a perpetual's spelling are refused before the folder is read.
#[test]
fn a_request_for_bars_or_a_perpetuals_dataset_is_refused() {
    let project = Project::new();
    let bars = ImportSpec {
        bars: vec!["1m".to_string()],
        ..spec(BINANCE_OPTION_SUMMARY_FORMAT, "BTCUSDT", true)
    };
    match project.answer(&bars) {
        Response::Refused(why) => assert!(why.text.contains("derives no bars"), "{why}"),
        other => panic!("not refused: {other:?}"),
    }
    match project.answer(&spec(BINANCE_VOLATILITY_INDEX_FORMAT, "BTCBVOLUSDT.P", true)) {
        Response::Refused(why) => assert!(why.text.contains("option-market"), "{why}"),
        other => panic!("not refused: {other:?}"),
    }
}
