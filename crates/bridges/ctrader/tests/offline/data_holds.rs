//! **One wire stream, however many keys hold it**: what `CtraderData` writes when keys of one symbol come and
//! go, the reading `crates/vike-datahub/src/feeds/venues.rs`'s `EXPLICIT_SHARING` row for ctrader stands on
//! (decision 0193, the sharing column of decision 0092). A quote key and every trend-bar key of a symbol stand on
//! its ONE spot subscription, so releasing any of them must not touch the others.
//!
//! The first half reads the commands off a bare channel, so the exact order is asserted with no socket and no
//! clock. The second half runs against the fake venue: one login for every key, the replay after a dropped
//! socket, and the re-auth after a token rotation on the same socket.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_ctrader::conn::grant::SharedGrant;
use vike_ctrader::conn::{Command, ConnConfig, ConnShared, connect_and_auth};
use vike_ctrader::data::CtraderData;
use vike_ctrader::positions::{CloseTracker, PositionBook};
use vike_ctrader::proto::{ProtoOaLightSymbol, pt};
use vike_ctrader::symbols::SymbolMap;
use vike_ctrader::test_support::{FakeCtrader, NoopSink, TrendbarsFault, token};
use vike_data::DataClient;

const EURUSD: i64 = 1;
const GBPUSD: i64 = 2;

/// A client over a bare channel, for two symbols.
fn client() -> (CtraderData, Receiver<Command>) {
    let light = [(EURUSD, "EURUSD"), (GBPUSD, "GBPUSD")].map(|(id, name)| ProtoOaLightSymbol {
        symbol_id: id,
        symbol_name: Some(name.to_string()),
        ..Default::default()
    });
    let (tx, rx) = mpsc::channel();
    let shared = ConnShared {
        tx,
        symbols: Arc::new(SymbolMap::from_symbols(&light, &[])),
        ctid: 99,
        orders: Arc::default(),
        positions: Arc::new(Mutex::new(PositionBook::default())),
        closes: Arc::new(Mutex::new(CloseTracker::default())),
    };
    (CtraderData::from_shared(shared), rx)
}

/// The stream commands written since the last call, in order. A seed (`GetTrendbars`) is per key by design and
/// streams nothing, so it is not one of them.
fn wire(rx: &Receiver<Command>) -> Vec<String> {
    rx.try_iter()
        .filter_map(|cmd| match cmd {
            Command::SubscribeSpots { symbol_id } => Some(format!("+spots {symbol_id}")),
            Command::UnsubscribeSpots { symbol_id } => Some(format!("-spots {symbol_id}")),
            Command::SubscribeTrendbar { symbol_id, period } => {
                Some(format!("+bars {symbol_id} {}", period.as_str_name()))
            }
            Command::UnsubscribeTrendbar { symbol_id, period } => {
                Some(format!("-bars {symbol_id} {}", period.as_str_name()))
            }
            Command::Shutdown => Some("shutdown".to_string()),
            _ => None,
        })
        .collect()
}

/// **The pin of the `EXPLICIT_SHARING` row.** Releasing the quote key leaves the bar key's spot stream standing,
/// and the last key out writes the trend bars' unsubscribe before the spots' they need.
#[test]
fn releasing_a_quote_key_keeps_the_spot_stream_of_a_bar_key_of_the_same_symbol() {
    let (mut data, rx) = client();
    let quotes = data.subscribe_quotes("EURUSD").expect("quotes");
    let bars = data.subscribe_bars("EURUSD", "1m").expect("bars");
    assert_eq!(
        wire(&rx),
        ["+spots 1", "+bars 1 M1"],
        "a second subscribe would be ALREADY_SUBSCRIBED"
    );

    data.unsubscribe(quotes);
    assert_eq!(wire(&rx), Vec::<String>::new(), "the bar key still needs the spots");

    data.unsubscribe(bars);
    assert_eq!(wire(&rx), ["-bars 1 M1", "-spots 1"]);
}

/// The reverse: releasing the bar key stops its trend bars and nothing the quote key stands on. And a bar key
/// that was the symbol's only key no longer leaves its spot stream running for ever.
#[test]
fn releasing_a_bar_key_keeps_the_spot_stream_of_a_quote_key_of_the_same_symbol() {
    let (mut data, rx) = client();
    let bars = data.subscribe_bars("EURUSD", "1m").expect("bars");
    let quotes = data.subscribe_quotes("EURUSD").expect("quotes");
    assert_eq!(wire(&rx), ["+spots 1", "+bars 1 M1"]);

    data.unsubscribe(bars);
    assert_eq!(wire(&rx), ["-bars 1 M1"]);

    data.unsubscribe(quotes);
    assert_eq!(wire(&rx), ["-spots 1"]);

    let alone = data.subscribe_bars("EURUSD", "1m").expect("bars");
    assert_eq!(wire(&rx), ["+spots 1", "+bars 1 M1"]);
    data.unsubscribe(alone);
    assert_eq!(wire(&rx), ["-bars 1 M1", "-spots 1"]);
}

/// Two periods of one symbol are two trend-bar streams on one spot stream; two keys of ONE period are one stream
/// (a release of either must not silence the other); another symbol is not involved in any of it.
#[test]
fn keys_of_other_periods_and_other_symbols_are_not_disturbed() {
    let (mut data, rx) = client();
    let m1 = data.subscribe_bars("EURUSD", "1m").expect("1m");
    let m1_twin = data.subscribe_bars("EURUSD", "1m").expect("1m again");
    let m5 = data.subscribe_bars("EURUSD", "5m").expect("5m");
    let other = data.subscribe_quotes("GBPUSD").expect("other symbol");
    assert_eq!(wire(&rx), ["+spots 1", "+bars 1 M1", "+bars 1 M5", "+spots 2"]);

    data.unsubscribe(m1);
    assert_eq!(wire(&rx), Vec::<String>::new(), "the twin still holds the 1m stream");
    data.unsubscribe(m1_twin);
    assert_eq!(wire(&rx), ["-bars 1 M1"]);
    data.unsubscribe(m5);
    assert_eq!(wire(&rx), ["-bars 1 M5", "-spots 1"], "GBPUSD is not touched");
    data.unsubscribe(other);
    assert_eq!(wire(&rx), ["-spots 2"]);

    data.unsubscribe(other);
    assert_eq!(wire(&rx), Vec::<String>::new(), "an id already released is a no-op");
}

/// `begin_shutdown` stops every stream at once and keeps the socket (no `Shutdown`): the guard counts on it for a
/// replaced session's bar keys, and the client may subscribe again. A key held before it is forgotten, so its
/// late release (the guard's reaper, the broker's drain) cannot take down a key subscribed since.
#[test]
fn begin_shutdown_stops_every_stream_and_a_late_release_cannot_touch_a_new_key() {
    let (mut data, rx) = client();
    let quotes = data.subscribe_quotes("EURUSD").expect("quotes");
    let bars = data.subscribe_bars("EURUSD", "1m").expect("bars");
    wire(&rx);

    data.begin_shutdown();
    assert_eq!(
        wire(&rx),
        ["-bars 1 M1", "-spots 1"],
        "one unsubscribe per stream, and no Shutdown"
    );

    let fresh = data.subscribe_quotes("EURUSD").expect("the client may subscribe again");
    assert_eq!(wire(&rx), ["+spots 1"]);
    data.unsubscribe(quotes);
    data.unsubscribe(bars);
    assert_eq!(wire(&rx), Vec::<String>::new(), "the old ids are unknown now");
    data.unsubscribe(fresh);
    assert_eq!(wire(&rx), ["-spots 1"]);
}

/// A shutdown of two keys of one symbol unsubscribes each stream once; a view does not end the socket it shares.
#[test]
fn a_shutdown_unsubscribes_each_stream_once() {
    let (mut data, rx) = client();
    data.subscribe_quotes("EURUSD").expect("quotes");
    data.subscribe_bars("EURUSD", "1m").expect("bars");
    wire(&rx);

    data.shutdown();
    let stopped = wire(&rx);
    assert_eq!(stopped.iter().filter(|c| *c == "-spots 1").count(), 1, "{stopped:?}");
    assert_eq!(stopped.iter().filter(|c| *c == "-bars 1 M1").count(), 1, "{stopped:?}");
    assert_eq!(stopped.len(), 2, "no Shutdown from a view: {stopped:?}");
}

/// **One session however many keys.** Every key of one client is a frame on the one socket: one login, one spot
/// subscribe, and a release of one key says nothing to the venue.
#[test]
fn every_key_of_a_client_rides_one_login_and_one_spot_subscribe() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("auth ok");
    let mut data = CtraderData::new(handle);

    let quotes = data.subscribe_quotes("EURUSD").expect("quotes");
    let m1 = data.subscribe_bars("EURUSD", "1m").expect("1m");
    let m5 = data.subscribe_bars("EURUSD", "5m").expect("5m");
    server.wait_until_count_at_least("SUBSCRIBE_LIVE_TRENDBAR_REQ", 2, Duration::from_secs(5));

    data.unsubscribe(quotes);
    data.unsubscribe(m1);
    server.wait_until_count_at_least("UNSUBSCRIBE_LIVE_TRENDBAR_REQ", 1, Duration::from_secs(5));
    assert_eq!(server.count_seen("UNSUBSCRIBE_SPOTS_REQ"), 0, "m5 still stands on the spots");
    assert_eq!(server.count_seen("SUBSCRIBE_SPOTS_REQ"), 1);
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 1);
    assert_eq!(server.count_seen("ACCOUNT_AUTH_REQ"), 1);

    data.unsubscribe(m5);
    server.wait_until_count_at_least("UNSUBSCRIBE_SPOTS_REQ", 1, Duration::from_secs(5));
    data.shutdown();
}

/// The bug the count closes, at its worst: the old release of a quote key took the symbol out of the actor's
/// replay set, so the bar key that was still held lost its spots at the next reconnect, with no error anywhere.
#[test]
fn a_reconnect_replays_the_spots_of_a_bar_key_whose_quote_key_was_released() {
    let server = FakeCtrader::start_drop_after_subscribe();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("auth ok");
    let mut data = CtraderData::new(handle);

    let quotes = data.subscribe_quotes("EURUSD").expect("quotes");
    data.subscribe_bars("EURUSD", "1m").expect("bars");
    data.unsubscribe(quotes);

    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, Duration::from_secs(10));
    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 2, Duration::from_secs(10));
    server.wait_until_count_at_least("SUBSCRIBE_LIVE_TRENDBAR_REQ", 1, Duration::from_secs(10));
    data.shutdown();
}

/// **A token rotation stays on the one socket and loses no key.** The venue ends the session of the superseded
/// token; the data socket takes the pair a sibling rotated (`SharedGrant::renew` spends nothing), authenticates
/// again on the SAME socket and, once answered, asks for its streams again. No second login, no second socket.
#[test]
fn a_token_rotation_re_auths_on_the_same_socket_and_asks_for_every_stream_again() {
    let server = FakeCtrader::start_trendbars_fault_first(TrendbarsFault::Event(
        pt::ACCOUNTS_TOKEN_INVALIDATED_EVENT,
    ));
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let mut cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "AT_0");
    cfg.refresh_token = Some("RT_0".to_string());
    cfg.grant = Some(cell.clone());
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("auth ok");
    // A sibling socket rotates: this socket is now one generation behind and adopts instead of spending.
    cell.renew(0, || 0, |_| Ok::<_, ()>(token("AT_1", "RT_1"))).expect("the sibling's rotation");
    let mut data = CtraderData::new(handle);

    // The fake answers this key's seed request with the invalidation event.
    data.subscribe_bars("EURUSD", "1m").expect("bars");

    server.wait_until_count_at_least("ACCOUNT_AUTH_REQ", 2, Duration::from_secs(5));
    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 2, Duration::from_secs(5));
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 1, "the same socket: no second login");
    data.shutdown();
}
