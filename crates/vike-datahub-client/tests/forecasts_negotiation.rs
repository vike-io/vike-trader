//! The two reads of the STORED FORECASTS (`kind=exec_forecast`), each negotiated by its own string (decision
//! 0112): the strategy lane's `Request::StrategyForecasts` (book W9's live drift rule) by `FEATURE_FORECASTS`,
//! and the data plane's `Request::ScanForecasts` (a research study's point-in-time read) by
//! `FEATURE_SCAN_FORECASTS`. A daemon that does not advertise a read's string is sent NOTHING (an older one could
//! not decode the verb), and one that does is asked the exact window and its rows are the answer. The peer is the
//! crate's recording fake, so "nothing was sent" is an empty frame list; the data plane's read is also driven
//! against the real server, through `HistoryStoreDatahub`, so `HistoryStore::forecasts_as_of` crosses whole.

use std::sync::Arc;

use vike_data::{ExecForecastRow, HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::proto::{
    FEATURE_FORECASTS, FEATURE_INPUT_BARS, FEATURE_SCAN_FORECASTS, FEATURE_STRATEGY_LANE, Request,
    Response,
};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};

fn forecast(ts: i64, value: f64) -> ExecForecastRow {
    ExecForecastRow { ts, mount_id: "m1".into(), value, horizon_ms: 60_000, confidence: None }
}

/// **A daemon without the string gets nothing new**: the read is refused locally, naming the string, and the
/// peer received no frame.
#[test]
fn a_daemon_without_the_string_is_sent_nothing() {
    let features = vec![FEATURE_STRATEGY_LANE.to_string(), FEATURE_INPUT_BARS.to_string()];
    let (addr, received, peer) = spawn_fake_peer(features, |_| None);
    let mut client = DatahubClient::connect(addr).expect("connect");
    let err = client
        .strategy_forecasts("binance", "BTCUSDT", 0, 1)
        .expect_err("refused before anything is sent");
    assert!(err.contains(FEATURE_FORECASTS) && err.contains("Nothing was sent"), "{err}");
    drop(client);
    assert!(received.recv().expect("the peer reports").is_empty(), "a frame reached the peer");
    peer.join().expect("the peer ends cleanly");
}

/// **A daemon with the string is asked the exact window**, and its rows are the answer, unchanged.
#[test]
fn a_daemon_with_the_string_is_asked_the_exact_window_and_its_rows_are_the_answer() {
    let rows = vec![forecast(5, 0.01), forecast(9, -0.02)];
    let answer = rows.clone();
    let (addr, received, peer) = spawn_fake_peer(vec![FEATURE_FORECASTS.to_string()], move |r| {
        matches!(r, Request::StrategyForecasts { .. }).then(|| Response::Forecasts(answer.clone()))
    });
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert_eq!(client.strategy_forecasts("binance", "BTCUSDT", 3, 10), Ok(rows));
    drop(client);
    let sent = received.recv().expect("the peer reports");
    let asked = Request::StrategyForecasts {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        start: 3,
        end: 10,
        columns: None,
    };
    assert_eq!(
        sent.iter().map(|r| serde_json::to_string(r).unwrap()).collect::<Vec<_>>(),
        [serde_json::to_string(&asked).unwrap()],
        "the exact request"
    );
    peer.join().expect("the peer ends cleanly");
}

/// **The data plane's read is its own string**: a daemon that advertises only the lane's `forecasts` is sent
/// nothing on the data plane, and the refusal names `scan_forecasts`.
#[test]
fn the_data_plane_read_is_sent_nothing_without_its_own_string() {
    let (addr, received, peer) = spawn_fake_peer(vec![FEATURE_FORECASTS.to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("connect");
    let err = client
        .scan_forecasts("binance", "BTCUSDT", TimestampRange::all(), Some(10))
        .expect_err("refused before anything is sent");
    assert!(err.contains(FEATURE_SCAN_FORECASTS) && err.contains("nothing was sent"), "{err}");
    drop(client);
    assert!(received.recv().expect("the peer reports").is_empty(), "a frame reached the peer");
    peer.join().expect("the peer ends cleanly");
}

/// **With its string, the data plane's read asks the exact window and cap**, and its rows are the answer.
#[test]
fn the_data_plane_read_asks_the_exact_window_and_its_rows_are_the_answer() {
    // Finite values only: `vike_model::Forecast::new` records no other, and this wire is JSON.
    let rows = vec![forecast(5, 0.01), forecast(9, -0.02)];
    let answer = rows.clone();
    let (addr, received, peer) =
        spawn_fake_peer(vec![FEATURE_SCAN_FORECASTS.to_string()], move |r| {
            matches!(r, Request::ScanForecasts { .. }).then(|| Response::Forecasts(answer.clone()))
        });
    let mut client = DatahubClient::connect(addr).expect("connect");
    let got = client.scan_forecasts("binance", "BTCUSDT", TimestampRange::of(3, 10), Some(100));
    assert_eq!(got, Ok(rows));
    drop(client);
    let sent = received.recv().expect("the peer reports");
    let asked = Request::ScanForecasts {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        start: Some(3),
        end: Some(10),
        limit: Some(100),
        columns: None,
    };
    assert_eq!(
        sent.iter().map(|r| serde_json::to_string(r).unwrap()).collect::<Vec<_>>(),
        [serde_json::to_string(&asked).unwrap()],
        "the exact request"
    );
    peer.join().expect("the peer ends cleanly");
}

/// **The real server advertises and answers the data plane's read**: a soft page keeps one decision time whole
/// (two mounts' rows at one `ts`), and the seam's point-in-time read equals the store's at every decision time,
/// with and without a start.
#[test]
fn the_server_answers_the_data_plane_read_and_the_point_in_time_read_crosses_whole() {
    let store = Arc::new(HistoryStoreMemory::new());
    let rows = [
        forecast(1_000, 0.1),
        forecast(2_000, 0.2),
        ExecForecastRow { mount_id: "m2".into(), ..forecast(2_000, -0.2) },
        forecast(3_000, 0.3),
    ];
    store.append_forecasts("binance", "BTCUSDT", &rows, Some("a")).unwrap();
    store.append_forecasts("binance", "ETHUSDT", &[forecast(1_500, 9.0)], Some("b")).unwrap();
    let addr = spawn_server(Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>);

    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(
        client.features().iter().any(|f| f == FEATURE_SCAN_FORECASTS),
        "{:?}",
        client.features()
    );
    let page = client.scan_forecasts("binance", "BTCUSDT", TimestampRange::all(), Some(2)).unwrap();
    assert_eq!(page, rows[..1], "a soft page of two stops before the decision time it would split");

    let remote = HistoryStoreDatahub::new(addr.to_string());
    for decision_ms in [500, 1_000, 2_000, 2_500, 3_000] {
        for start_ms in [None, Some(1_500)] {
            assert_eq!(
                remote.forecasts_as_of("binance", "BTCUSDT", decision_ms, start_ms).unwrap(),
                store.forecasts_as_of("binance", "BTCUSDT", decision_ms, start_ms).unwrap(),
                "the wire answers what the store answers at {decision_ms} from {start_ms:?}"
            );
        }
    }
    assert_eq!(remote.forecasts_as_of("binance", "BTCUSDT", 2_000, None).unwrap(), rows[..3]);
}
