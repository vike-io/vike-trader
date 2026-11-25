//! A bar read folded by the DATAHUB into a coarser interval (`Request::LoadBars`' `resample_to`,
//! `FEATURE_BAR_RESAMPLE`):
//!
//! - over the REAL server, the answer is `vike_marketdata::resample` of the stored rows, done here by hand,
//!   bit for bit, as JSON and as LZ4 columns alike;
//! - a server that does not advertise the field is sent NOTHING: `HistoryStoreDatahub` answers `None` (the
//!   caller reads as it did before) and the client's own read is refused locally;
//! - a read that does not resample is byte-identical on the wire to a read from before the field.

use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_datahub::test_support::{exchange, spawn_server};
use vike_datahub_client::proto::{
    BarColumnsBody, FEATURE_BAR_COLUMNS, FEATURE_BAR_RESAMPLE, FEATURE_SCAN_LIMIT, Request,
    Response,
};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_marketdata::{Bar, resample};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// 2024-01-01T00:00:00Z, a day boundary, so a day's windows start on a stored row.
const FIRST: i64 = 1_704_067_200_000;

/// Three days and seven minutes of `1m` bars, with a hole of an hour on the second day.
fn minutes() -> Vec<Bar> {
    (0..(3 * 1_440 + 7))
        .filter(|i| !(1_440 + 600..1_440 + 660).contains(i))
        .map(|i: i64| {
            let close = 42_000.0 + (i % 613) as f64 * 0.5 + 1.0 / 7.0;
            Bar {
                timestamp: FIRST + i * MINUTE,
                open: close - 0.25,
                high: close + 2.0 + (i % 5) as f64,
                low: close - 3.0 - (i % 3) as f64,
                close,
                volume: (i % 17) as f64 * 0.125,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

fn served() -> (std::net::SocketAddr, Vec<Bar>) {
    let bars = minutes();
    let store = HistoryStoreMemory::default();
    store.append_bars(VENUE, SYMBOL, "1m", &bars, None).expect("seed the store");
    (spawn_server(Arc::new(store)), bars)
}

type BarBits = (i64, [u64; 5], [Option<u64>; 3]);

fn bits(rows: &[Bar]) -> Vec<BarBits> {
    rows.iter()
        .map(|b| {
            (
                b.timestamp,
                [b.open, b.high, b.low, b.close, b.volume].map(f64::to_bits),
                [b.funding, b.bid, b.ask].map(|f| f.map(f64::to_bits)),
            )
        })
        .collect()
}

/// The datahub's answer is `resample` of the stored rows of the range, for every target and a range that
/// starts and ends inside a window, and the LZ4 columns carry exactly the JSON reply's bars.
#[test]
fn the_answer_is_the_stored_rows_resampled_by_hand() {
    let (address, stored) = served();
    let mut client = DatahubClient::connect(address).expect("dial");
    assert!(client.serves_bar_resample(), "every build advertises it");
    for (to, width) in [("5m", 5 * MINUTE), ("1h", 60 * MINUTE), ("4h", 240 * MINUTE), ("1d", DAY)]
    {
        for range in [
            TimestampRange::of(FIRST, FIRST + 3 * DAY + 6 * MINUTE),
            TimestampRange::of(FIRST + 7 * MINUTE + 13, FIRST + DAY + 777 * MINUTE),
        ] {
            let in_range: Vec<Bar> = stored
                .iter()
                .filter(|b| range.start.is_none_or(|s| b.timestamp >= s))
                .filter(|b| range.end.is_none_or(|e| b.timestamp <= e))
                .cloned()
                .collect();
            let by_hand = resample(&in_range, width);
            let json = client
                .load_bars_resampled(VENUE, SYMBOL, "1m", range, to, None)
                .expect("a JSON answer");
            assert!(bits(&json) == bits(&by_hand), "{to} over {range:?}: JSON");
            let lz4 = client
                .load_bars_resampled(VENUE, SYMBOL, "1m", range, to, Some(BarColumnsBody::Lz4))
                .expect("a columns answer");
            assert!(bits(&lz4) == bits(&by_hand), "{to} over {range:?}: LZ4 columns");
        }
    }
    let remote = HistoryStoreDatahub::new(address.to_string()).with_compressed_bars();
    let range = TimestampRange::of(FIRST, FIRST + 3 * DAY);
    let days = remote.load_bars_resampled(VENUE, SYMBOL, "1m", range, "1d").expect("a read");
    let by_hand =
        resample(&remote.load_bars(VENUE, SYMBOL, "1m", range).expect("the stored rows"), DAY);
    assert!(bits(&days.expect("a resampling server answers")) == bits(&by_hand));
}

/// What the datahub refuses, by name, on a connection that stays open: a `limit`, an open bound, a target
/// that is not a whole multiple of the stored interval, and a range of more windows than one reply carries.
#[test]
fn a_resample_it_cannot_answer_whole_is_refused_by_name() {
    let (address, _) = served();
    // Key-less, so no `Hello` is needed: one raw stream, every refusal on it, and it stays open.
    let mut stream = std::net::TcpStream::connect(address).expect("dial");
    let mut ask = |start, end, limit, to: &str| {
        let request = Request::LoadBars {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: "1m".into(),
            start,
            end,
            limit,
            source: None,
            columns: None,
            resample_to: Some(to.into()),
        };
        match exchange(&mut stream, &request) {
            Response::Error(why) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    };
    let day = (Some(FIRST), Some(FIRST + DAY));
    assert!(ask(day.0, day.1, Some(10), "1h").contains("limit"));
    assert!(ask(None, day.1, None, "1h").contains("both"));
    assert!(ask(day.0, day.1, None, "90s").contains("multiple"));
    assert!(ask(day.0, day.1, None, "30s").contains("multiple"));
    assert!(ask(Some(0), Some(FIRST), None, "5m").contains("RESAMPLED_BARS_CEILING"));
    let mut client = DatahubClient::connect(address).expect("dial");
    assert!(client.load_bars(VENUE, SYMBOL, "1m", TimestampRange::of(FIRST, FIRST), None).is_ok());
}

/// A datahub from before the field: nothing is sent, `HistoryStoreDatahub` says `None` and the client's own
/// read is an error naming the string.
#[test]
fn a_server_without_the_field_is_sent_nothing() {
    let reply = |_: &Request| Some(Response::Bars(Vec::new()));
    let features = vec![FEATURE_SCAN_LIMIT.to_string(), FEATURE_BAR_COLUMNS.to_string()];
    let (address, received, _) = spawn_fake_peer(features.clone(), reply);
    {
        let remote = HistoryStoreDatahub::new(address.to_string()).with_compressed_bars();
        let range = TimestampRange::of(FIRST, FIRST + DAY);
        assert!(remote.load_bars_resampled(VENUE, SYMBOL, "1m", range, "1h").unwrap().is_none());
    }
    assert!(received.recv().expect("the peer reports").is_empty(), "nothing was sent");

    let (address, received, _) = spawn_fake_peer(features, reply);
    {
        let mut client = DatahubClient::connect(address).expect("dial");
        let range = TimestampRange::of(FIRST, FIRST + DAY);
        let refused = client.load_bars_resampled(VENUE, SYMBOL, "1m", range, "1h", None);
        assert!(refused.unwrap_err().contains(FEATURE_BAR_RESAMPLE));
    }
    assert!(received.recv().expect("the peer reports").is_empty(), "nothing was sent");
}

/// A resampling server is asked once, whole: the target rides the frame, no `limit`, and the handle's LZ4
/// columns where the server answers them.
#[test]
fn a_resampling_server_is_asked_once_with_the_target() {
    let reply = |_: &Request| Some(Response::Bars(Vec::new()));
    let features = vec![FEATURE_SCAN_LIMIT.to_string(), FEATURE_BAR_RESAMPLE.to_string()];
    let (address, received, _) = spawn_fake_peer(features, reply);
    {
        let remote = HistoryStoreDatahub::new(address.to_string()).with_compressed_bars();
        let range = TimestampRange::of(FIRST, FIRST + DAY);
        assert_eq!(
            remote.load_bars_resampled(VENUE, SYMBOL, "1m", range, "1h").unwrap(),
            Some(vec![])
        );
    }
    let asked = received.recv().expect("the peer reports");
    let [Request::LoadBars { limit: None, columns: None, resample_to: Some(to), start, end, .. }] =
        asked.as_slice()
    else {
        panic!("one resampled read, no limit, JSON (no columns advertised): {asked:?}");
    };
    assert_eq!((to.as_str(), *start, *end), ("1h", Some(FIRST), Some(FIRST + DAY)));
}

/// A read that does not resample carries no `resample_to` key at all, so its frame is the one every
/// server before the field decodes.
#[test]
fn a_plain_read_is_byte_identical_to_one_from_before_the_field() {
    let request = Request::LoadBars {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        interval: "1m".into(),
        start: Some(1),
        end: Some(2),
        limit: Some(3),
        source: None,
        columns: None,
        resample_to: None,
    };
    let json = serde_json::to_string(&request).expect("serialize");
    assert_eq!(
        json,
        r#"{"LoadBars":{"venue":"binance","symbol":"BTCUSDT","interval":"1m","start":1,"end":2,"limit":3}}"#
    );
}
