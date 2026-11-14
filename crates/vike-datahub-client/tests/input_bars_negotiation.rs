//! The strategy lane's STORED INPUT-BARS read (`Request::StrategyInputBars`, decision 0271's live half) is
//! negotiated by `FEATURE_INPUT_BARS` (decision 0112): a daemon that does not advertise it is sent NOTHING
//! (an older one could not decode the verb), and one that does is asked, the `source=` lane carried, and its
//! bars are the answer. The peer is the crate's recording fake, so "nothing was sent" is an empty frame list.

use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{
    FEATURE_FUNDING_HISTORY, FEATURE_INPUT_BARS, FEATURE_STRATEGY_LANE, Request, Response,
};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_marketdata::Bar;

fn bar(ts: i64) -> Bar {
    Bar {
        timestamp: ts,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 10.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// **A daemon without the string gets nothing new**: the read is refused locally, naming the string, and the
/// peer received no frame (neither the lane nor the funding read stands in for it).
#[test]
fn a_daemon_without_the_string_is_sent_nothing() {
    let features = vec![FEATURE_STRATEGY_LANE.to_string(), FEATURE_FUNDING_HISTORY.to_string()];
    let (addr, received, peer) = spawn_fake_peer(features, |_| None);
    let mut client = DatahubClient::connect(addr).expect("connect");
    let err = client
        .strategy_input_bars("binance", "USDT-USD", "1d", Some("user-coinbase"), 0, 1)
        .expect_err("refused before anything is sent");
    assert!(err.contains(FEATURE_INPUT_BARS) && err.contains("Nothing was sent"), "{err}");
    drop(client);
    assert!(received.recv().expect("the peer reports").is_empty(), "a frame reached the peer");
    peer.join().expect("the peer ends cleanly");
}

/// **A daemon with the string is asked once per read, with the exact request** (the `source=` lane carried,
/// and absent for the venue's own candles), and its bars are the answer, unchanged.
#[test]
fn a_daemon_with_the_string_is_asked_the_exact_request_and_its_bars_are_the_answer() {
    let (addr, received, peer) = spawn_fake_peer(vec![FEATURE_INPUT_BARS.to_string()], |r| {
        matches!(r, Request::StrategyInputBars { .. }).then(|| Response::Bars(vec![bar(5), bar(9)]))
    });
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert_eq!(
        client.strategy_input_bars("binance", "USDT-USD", "1d", Some("user-coinbase"), 0, 10),
        Ok(vec![bar(5), bar(9)])
    );
    assert_eq!(
        client.strategy_input_bars("binance", "BTCUSDT", "1h", None, 3, 7),
        Ok(vec![bar(5), bar(9)])
    );
    drop(client);
    let sent = received.recv().expect("the peer reports");
    let lane = Request::StrategyInputBars {
        venue: "binance".into(),
        symbol: "USDT-USD".into(),
        interval: "1d".into(),
        source: Some("user-coinbase".into()),
        start: 0,
        end: 10,
    };
    let own = Request::StrategyInputBars {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        source: None,
        start: 3,
        end: 7,
    };
    assert_eq!(
        sent.iter().map(|r| serde_json::to_string(r).unwrap()).collect::<Vec<_>>(),
        [&lane, &own].map(|r| serde_json::to_string(r).unwrap()),
        "the exact two requests"
    );
    peer.join().expect("the peer ends cleanly");
}
