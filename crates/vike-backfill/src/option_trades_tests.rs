use std::sync::Mutex;

use super::*;
use crate::test_support::store;
use vike_data::TimestampRange;
use vike_data::option_trade_log::OptionTradeRow;
use vike_data::source::SourceError;
use vike_marketdata::OptionKind;

/// 2024-06-03 00:00:00 UTC.
const DAY0: i64 = 1_717_372_800_000;
const HOUR: i64 = 3_600_000;

fn trade(ts: i64, underlying: &str, id: &str) -> OptionTradeRow {
    OptionTradeRow {
        ts,
        underlying: underlying.into(),
        instrument: format!("{underlying}-21JUN24-94000-C"),
        expiry_ms: 1_718_956_800_000,
        strike: 94_000.0,
        kind: OptionKind::Call,
        trade_id: id.into(),
        price: 0.0007,
        amount: 1.0,
        is_buyer_maker: false,
        iv: Some(0.63),
        mark_price: Some(0.000_8),
        index_price: 67_000.0,
        premium_in_coin: underlying == "BTC",
    }
}

/// A scripted source: the trades of each day it is asked for, and every window it was asked.
struct Scripted {
    days: Vec<(i64, Result<Vec<OptionTradeRow>, SourceError>)>,
    asked: Mutex<Vec<(i64, i64)>>,
}

impl OptionTradeSource for Scripted {
    fn venue(&self) -> &str {
        "deribit"
    }

    fn fetch(
        &self,
        currency: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<OptionTradeRow>, SourceError> {
        assert_eq!(currency, "BTC");
        self.asked.lock().unwrap().push((start_ms, end_ms));
        self.days.iter().find(|(d, _)| *d == start_ms).map_or(Ok(Vec::new()), |(_, a)| a.clone())
    }
}

fn scripted(days: Vec<(i64, Result<Vec<OptionTradeRow>, SourceError>)>) -> Scripted {
    Scripted { days, asked: Mutex::new(Vec::new()) }
}

const NOW: i64 = DAY0 + 10 * MS_PER_DAY;

/// A window inside one day stores the WHOLE day under its key, derives that day's hourly chain rows,
/// and a repeat asks the venue nothing and writes nothing.
#[test]
fn a_day_is_stored_with_its_chain_and_a_repeat_is_free() {
    let (_dir, hist) = store();
    let source = scripted(vec![(
        DAY0,
        Ok(vec![
            trade(DAY0 + 10, "BTC", "a"),
            trade(DAY0 + 20, "BTC", "b"),
            trade(DAY0 + HOUR, "BTC", "c"),
        ]),
    )]);
    let written = backfill_option_trades_at(
        &hist,
        &source,
        "BTC",
        (DAY0 + 5 * HOUR, DAY0 + 6 * HOUR),
        NOW,
        &|| false,
    )
    .unwrap();
    assert_eq!(written, 3);
    assert_eq!(*source.asked.lock().unwrap(), [(DAY0, DAY0 + MS_PER_DAY - 1)]);
    let stored = hist.scan_option_trades("deribit", "BTC", TimestampRange::all()).unwrap();
    assert_eq!(stored.iter().map(|t| t.trade_id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
    let chain = hist.scan_chain("deribit", "BTC", TimestampRange::all()).unwrap();
    assert_eq!(
        chain.iter().map(|r| r.ts).collect::<Vec<_>>(),
        [DAY0 + HOUR - 1, DAY0 + 2 * HOUR - 1]
    );
    assert_eq!(chain[0].volume, Some(2.0));
    // what the datahub reads back
    let edges = hist.option_trade_edges("deribit", "BTC", TimestampRange::all()).unwrap();
    assert_eq!(
        (edges.first_timestamp, edges.last_timestamp, edges.rows),
        (Some(DAY0 + 10), Some(DAY0 + HOUR), 3)
    );

    let again =
        backfill_option_trades_at(&hist, &source, "BTC", (DAY0, DAY0 + 1), NOW, &|| false).unwrap();
    assert_eq!(again, 0);
    assert_eq!(source.asked.lock().unwrap().len(), 1, "a stored day is never asked again");
}

/// One currency's log of several underlyings fans its chain into one series per underlying.
#[test]
fn the_chain_of_each_underlying_lands_in_its_own_series() {
    let (_dir, hist) = store();
    let source =
        scripted(vec![(DAY0, Ok(vec![trade(DAY0 + 1, "SOL", "s"), trade(DAY0 + 2, "XRP", "x")]))]);
    backfill_option_trades_at(&hist, &source, "BTC", (DAY0, DAY0), NOW, &|| false).unwrap();
    for underlying in ["SOL", "XRP"] {
        let chain = hist.scan_chain("deribit", underlying, TimestampRange::all()).unwrap();
        assert_eq!(chain.len(), 1, "{underlying}");
        assert_eq!(chain[0].underlying, underlying);
    }
}

/// The day holding "now" (and the margin after it) is not written; an empty day spends no key and is
/// asked again; a failure names its day and keeps the days before it; a stop is never `Ok`.
#[test]
fn unsettled_empty_failed_and_stopped_days() {
    let (_dir, hist) = store();
    let day1 = DAY0 + MS_PER_DAY;
    let source = scripted(vec![(DAY0, Ok(vec![trade(DAY0 + 1, "BTC", "a")]))]);
    // now is inside day1: day0 stored, day1 left for later
    let now = day1 + HOUR;
    assert_eq!(
        backfill_option_trades_at(&hist, &source, "BTC", (DAY0, day1), now, &|| false).unwrap(),
        1
    );
    assert_eq!(source.asked.lock().unwrap().len(), 1, "the unsettled day was not asked");
    // an empty settled day: asked, nothing spent, asked again next time
    let empty = scripted(vec![]);
    backfill_option_trades_at(&hist, &empty, "BTC", (day1, day1), NOW, &|| false).unwrap();
    backfill_option_trades_at(&hist, &empty, "BTC", (day1, day1), NOW, &|| false).unwrap();
    assert_eq!(empty.asked.lock().unwrap().len(), 2);
    // a failure on day1 after day0 was skipped: named, and day0 stays
    let failing = scripted(vec![(day1, Err(SourceError::Fetch("venue said no".into())))]);
    let err = backfill_option_trades_at(&hist, &failing, "BTC", (DAY0, day1), NOW, &|| false)
        .unwrap_err();
    assert!(
        matches!(&err, CollectError::Fetch(t) if t.text().contains("2024-06-04") && t.text().contains("venue said no")),
        "{err:?}"
    );
    assert_eq!(hist.scan_option_trades("deribit", "BTC", TimestampRange::all()).unwrap().len(), 1);
    // a stop before the first day is a Stopped, never Ok
    let stop =
        backfill_option_trades_at(&hist, &failing, "BTC", (DAY0, day1), NOW, &|| true).unwrap_err();
    assert!(
        matches!(&stop, CollectError::Stopped(t) if t.contains("resumes at 2024-06-03")),
        "{stop:?}"
    );
}

/// Refused before anything is read: an inverted window, a currency that cannot be a directory name.
#[test]
fn refusals_come_first() {
    let (_dir, hist) = store();
    let source = scripted(vec![]);
    assert!(matches!(
        backfill_option_trades_at(&hist, &source, "BTC", (2, 1), NOW, &|| false),
        Err(CollectError::Refused(_))
    ));
    assert!(matches!(
        backfill_option_trades_at(&hist, &source, "../x", (DAY0, DAY0), NOW, &|| false),
        Err(CollectError::Refused(_))
    ));
    assert!(source.asked.lock().unwrap().is_empty());
}

/// The bytes of every `.parquet` file under `dir`, per `kind=` directory.
fn parquet_bytes(dir: &std::path::Path, out: &mut BTreeMap<String, u64>, kind: &str) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            let kind = name.strip_prefix("kind=").map_or(kind.to_string(), str::to_string);
            parquet_bytes(&path, out, &kind);
        } else if name.ends_with(".parquet") {
            *out.entry(kind.to_string()).or_default() += entry.metadata().unwrap().len();
        }
    }
}

/// Live sample, stored: the days `vike-deribit`'s `live_sample_days_for_the_size_estimate` fetched, run
/// through THIS ingest into a temp store, and the Parquet it wrote, per kind and per trade — the size half
/// of a long backfill's estimate (`cargo test -p vike-backfill --lib live_sample -- --ignored`, after it).
#[test]
#[ignore = "reads the live sample vike-deribit's ignored test writes to the workspace's tmp/"]
fn the_live_sample_in_parquet() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tmp/vike-option-trades-sample.jsonl");
    let text = std::fs::read_to_string(&path).expect("run vike-deribit's live sample first");
    for currency in ["BTC", "ETH"] {
        let (dir, hist) = store();
        let mut days = Vec::new();
        let (mut seconds, mut trades) = (0.0, 0);
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v["currency"] != currency {
                continue;
            }
            let rows: Vec<OptionTradeRow> = serde_json::from_value(v["rows"].clone()).unwrap();
            seconds += v["seconds"].as_f64().unwrap();
            trades += rows.len();
            days.push((v["day_ms"].as_i64().unwrap(), Ok(rows)));
        }
        let source = scripted(days.clone());
        for (d0, _) in &days {
            backfill_option_trades_at(
                &hist,
                &source,
                "BTC",
                (*d0, *d0),
                NOW + 1_000 * MS_PER_DAY,
                &|| false,
            )
            .unwrap();
        }
        let mut bytes = BTreeMap::new();
        parquet_bytes(dir.path(), &mut bytes, "");
        let total: u64 = bytes.values().sum();
        eprintln!(
            "sample {currency}: {} days, {trades} trades, fetched in {seconds:.1}s; parquet {bytes:?} = {total} bytes, \
             {:.1} bytes per trade; 1461 days at this rate = {:.2} GB and {:.1} h of fetching",
            days.len(),
            total as f64 / trades.max(1) as f64,
            total as f64 / days.len() as f64 * 1461.0 / 1e9,
            seconds / days.len() as f64 * 1461.0 / 3600.0
        );
    }
}
