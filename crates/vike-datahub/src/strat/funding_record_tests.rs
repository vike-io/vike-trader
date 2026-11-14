use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_marketdata::FundingRate;

use super::*;

/// A settlement (2026-11-08 16:00 UTC) and the next ones, eight hours apart.
const T1: i64 = 1_794_153_600_000;
const T2: i64 = T1 + 8 * 3_600_000;
const T3: i64 = T2 + 8 * 3_600_000;

fn observation(rate: f64, next_ts: i64, price: Option<f64>) -> FundingRate {
    FundingRate {
        timestamp: 0,
        symbol: "BTCUSDT.P".to_string(),
        rate,
        interval_ms: None,
        next_timestamp: Some(next_ts),
        predicted: None,
        price,
    }
}

/// What a test writer was handed: `(venue, symbol, settlement)` in order.
type Kept = Arc<Mutex<Vec<(String, String, FundingRatePoint)>>>;

/// A recorder whose writer keeps what it is handed.
fn recorder() -> (Arc<FundingRateRecorder>, Kept) {
    let kept: Kept = Arc::default();
    let into = Arc::clone(&kept);
    let writer: FundingRateWriter = Box::new(move |venue, symbol, settled| {
        into.lock().unwrap().push((venue.to_string(), symbol.to_string(), settled));
        Ok(1)
    });
    (FundingRateRecorder::spawn(writer).expect("the worker starts"), kept)
}

/// What the writer was handed once it holds `n` settlements (the worker is another thread).
fn written(kept: &Kept, n: usize) -> Vec<FundingRatePoint> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while kept.lock().unwrap().len() < n {
        assert!(Instant::now() < deadline, "the writer never got {n}: {:?}", kept.lock().unwrap());
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(30)); // a settlement too many would land by now
    kept.lock().unwrap().iter().map(|(_, _, s)| *s).collect()
}

/// **A settlement is recorded once, when the venue rolls past it, at the last rate it stated for it and the
/// last mark the lane carried**; the first observation and a repeat record nothing, a late observation of a
/// recorded settlement changes nothing, and a stated funding price wins over the mark.
#[test]
fn a_settlement_is_recorded_once_when_the_venue_rolls_past_it() {
    let (rec, kept) = recorder();
    rec.mark("binance", "BTCUSDT.P", 59_000.0); // no clock yet: not remembered
    rec.observe("binance", "BTCUSDT.P", &observation(0.0001, T1, None));
    rec.observe("binance", "BTCUSDT.P", &observation(0.0002, T1, None));
    rec.mark("binance", "BTCUSDT.P", 60_000.0);
    rec.observe("binance", "BTCUSDT.P", &observation(0.0003, T2, None)); // rolled: T1 happened
    rec.observe("binance", "BTCUSDT.P", &observation(0.0003, T2, None));
    rec.observe("binance", "BTCUSDT.P", &observation(0.0009, T1, None)); // a late T1 observation
    rec.observe("binance", "BTCUSDT.P", &observation(0.0004, T2, Some(60_500.0)));
    rec.observe("binance", "BTCUSDT.P", &observation(0.0005, T3, None)); // rolled: T2 happened
    assert_eq!(
        written(&kept, 2),
        vec![
            FundingRatePoint {
                timestamp_ms: T1,
                rate: 0.0002,
                premium: None,
                mark: Some(60_000.0)
            },
            FundingRatePoint {
                timestamp_ms: T2,
                rate: 0.0004,
                premium: None,
                mark: Some(60_500.0)
            },
        ]
    );
    assert!(kept.lock().unwrap().iter().all(|(v, s, _)| v == "binance" && s == "BTCUSDT.P"));
}

/// **A settlement brought forward is recorded at its new time** (binance and okx drop to an hourly
/// interval at a capped rate), and the later time it replaced is never recorded.
#[test]
fn a_settlement_brought_forward_is_recorded_at_its_new_time() {
    let (rec, kept) = recorder();
    rec.observe("okx", "BTC-USDT-SWAP", &observation(0.0001, T2, None));
    rec.observe("okx", "BTC-USDT-SWAP", &observation(0.003, T1, None)); // capped: T1 is next
    rec.observe("okx", "BTC-USDT-SWAP", &observation(0.003, T1 + 3_600_000, None));
    assert_eq!(
        written(&kept, 1),
        vec![FundingRatePoint { timestamp_ms: T1, rate: 0.003, premium: None, mark: None }]
    );
}

/// **A recorded live settlement is in the funding series** the backfill fills, through its own ingest:
/// the rate on `funding`, the price on `close`, the settlement time as `ts`, read back as the catch-up reads
/// it; and a settlement the series already holds (a backfilled one, or the same one twice) writes nothing.
#[cfg(feature = "backfill-serve")]
#[test]
fn a_recorded_live_settlement_is_in_the_funding_series() {
    use vike_data::HistoryStore;

    let dir = tempfile::tempdir().expect("a temporary directory");
    let store =
        Arc::new(vike_store::HistoryStoreDataFusion::open(dir.path()).expect("the store opens"));
    let rec =
        FundingRateRecorder::spawn(store_writer(Arc::clone(&store))).expect("the worker starts");
    rec.observe("binance", "BTCUSDT.P", &observation(0.0001, T1, None));
    rec.mark("binance", "BTCUSDT.P", 79_748.2);
    rec.observe("binance", "BTCUSDT.P", &observation(0.0002, T2, None));
    let read = || {
        store
            .load_bars(
                "binance",
                "BTCUSDT.P",
                vike_data::source::FUNDING_INTERVAL,
                vike_data::TimestampRange::all(),
            )
            .expect("reads")
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while read().is_empty() {
        assert!(Instant::now() < deadline, "the settlement never reached the series");
        std::thread::sleep(Duration::from_millis(10));
    }
    let bars = read();
    assert_eq!(bars.len(), 1, "{bars:?}");
    assert_eq!((bars[0].timestamp, bars[0].funding, bars[0].close), (T1, Some(0.0001), 79_748.2));

    let write = store_writer(Arc::clone(&store));
    let again =
        FundingRatePoint { timestamp_ms: T1, rate: 0.0001, premium: None, mark: Some(79_748.2) };
    assert_eq!(write("binance", "BTCUSDT.P", again), Ok(0), "the same settlement writes nothing");
    let jittered =
        FundingRatePoint { timestamp_ms: T1 + 6, rate: 0.0001, premium: None, mark: None };
    assert_eq!(write("binance", "BTCUSDT.P", jittered), Ok(0), "nor one in the same hour");
    assert_eq!(
        write(
            "binance",
            "BTCUSDT.P",
            FundingRatePoint { timestamp_ms: T2, rate: 0.0002, premium: None, mark: None }
        ),
        Ok(1)
    );
    assert_eq!(read().len(), 2);
}

/// **Recorded and backfilled settlements never double up**: three settlements the datahub recorded live,
/// then a funding backfill of a window covering them (the venue's own history, a few milliseconds off the
/// hour as Hyperliquid stamps it): the series holds three rows, not six.
#[cfg(feature = "backfill-serve")]
#[test]
fn a_backfill_over_recorded_settlements_leaves_one_row_per_settlement() {
    use vike_data::HistoryStore;
    use vike_data::source::{FundingRateSource, SourceError};
    use vike_marketdata::FundingRatePoint;

    struct History(Vec<FundingRatePoint>);
    impl FundingRateSource for History {
        fn venue(&self) -> &str {
            "binance"
        }
        fn fetch(&self, _: &str, _: i64, _: i64) -> Result<Vec<FundingRatePoint>, SourceError> {
            Ok(self.0.clone())
        }
    }

    let dir = tempfile::tempdir().expect("a temporary directory");
    let store =
        Arc::new(vike_store::HistoryStoreDataFusion::open(dir.path()).expect("the store opens"));
    let rec =
        FundingRateRecorder::spawn(store_writer(Arc::clone(&store))).expect("the worker starts");
    let t4 = T3 + 8 * 3_600_000;
    for (rate, next) in [(0.0001, T1), (0.0002, T2), (0.0003, T3), (0.0004, t4)] {
        rec.observe("binance", "BTCUSDT.P", &observation(rate, next, None));
    }
    let read = || {
        store
            .load_bars(
                "binance",
                "BTCUSDT.P",
                vike_data::source::FUNDING_INTERVAL,
                vike_data::TimestampRange::all(),
            )
            .expect("reads")
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while read().len() < 3 {
        assert!(Instant::now() < deadline, "the three settlements were never recorded");
        std::thread::sleep(Duration::from_millis(10));
    }
    let history = History(
        [(T1, 0.0001), (T2, 0.0002), (T3, 0.0003)]
            .map(|(ts, rate)| FundingRatePoint {
                timestamp_ms: ts + 6,
                rate,
                premium: None,
                mark: None,
            })
            .to_vec(),
    );
    let written =
        vike_backfill::funding_rate::backfill_funding_rate(&store, &history, "BTCUSDT.P", T1, T3)
            .expect("the backfill runs");
    assert_eq!(written.rate_rows, 0, "every settlement was held");
    let times: Vec<i64> = read().iter().map(|b| b.timestamp).collect();
    assert_eq!(times, vec![T1, T2, T3], "three rows, not six");
}
