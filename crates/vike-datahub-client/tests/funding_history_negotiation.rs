//! The strategy lane's STORED FUNDING read (`Request::StrategyFundingHistory`, decision 0262's amendment of
//! 2026-11-08) is negotiated by `FEATURE_FUNDING_HISTORY` (decision 0112): a daemon that does not advertise
//! it is sent NOTHING (an older one could not decode the verb), and one that does is asked and its bars are
//! the answer. The peer is the crate's recording fake, so "nothing was sent" is an empty frame list.

use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{
    FEATURE_FUNDING_HISTORY, FEATURE_STRATEGY_LANE, Request, Response,
};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_marketdata::Bar;

fn settlement(ts: i64) -> Bar {
    Bar {
        timestamp: ts,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 60_000.0,
        volume: 0.0,
        funding: Some(0.0001),
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// **A daemon without the string gets nothing new**: the read is refused locally, naming the string, and the
/// peer received no frame (a strategy lane alone does not stand in for it).
#[test]
fn a_daemon_without_the_string_is_sent_nothing() {
    let (addr, received, peer) = spawn_fake_peer(vec![FEATURE_STRATEGY_LANE.to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("connect");
    let err = client
        .strategy_funding_history("binance", "BTCUSDT.P", 0, 1)
        .expect_err("refused before anything is sent");
    assert!(err.contains(FEATURE_FUNDING_HISTORY) && err.contains("Nothing was sent"), "{err}");
    drop(client);
    assert!(received.recv().expect("the peer reports").is_empty(), "a frame reached the peer");
    peer.join().expect("the peer ends cleanly");
}

/// **A daemon with the string is asked once, and its bars are the answer**, unchanged.
#[test]
fn a_daemon_with_the_string_is_asked_and_its_bars_are_the_answer() {
    let (addr, received, peer) = spawn_fake_peer(vec![FEATURE_FUNDING_HISTORY.to_string()], |r| {
        matches!(r, Request::StrategyFundingHistory { .. })
            .then(|| Response::Bars(vec![settlement(5), settlement(9)]))
    });
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert_eq!(
        client.strategy_funding_history("binance", "BTCUSDT.P", 0, 10),
        Ok(vec![settlement(5), settlement(9)])
    );
    drop(client);
    let sent = received.recv().expect("the peer reports");
    assert!(
        matches!(
            &sent[..],
            [Request::StrategyFundingHistory { venue, symbol, start: 0, end: 10 }]
                if venue == "binance" && symbol == "BTCUSDT.P"
        ),
        "{sent:?}"
    );
    peer.join().expect("the peer ends cleanly");
}
