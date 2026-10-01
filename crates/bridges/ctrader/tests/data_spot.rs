//! Scripted integration test for the `DataClient` live-quote path: `connect_and_auth` against the
//! in-process fake cTrader server (Task 1-3), `CtraderData::subscribe_quotes` sends
//! `SubscribeSpots`, the fake server replies with `SUBSCRIBE_SPOTS_RES` + an unsolicited
//! `SPOT_EVENT` (EURUSD id=1, bid=113911/ask=113912 — see `tests/common/mod.rs::script_reply`),
//! and the actor thread decodes+routes it to the `LiveDataSink` given at construction. Asserts the
//! `RecordingSink` test double actually received the descaled quote — proves the whole
//! actor-owns-the-sink wiring end to end, not just the pure mapper (see `tests/offline/data_mapper.rs`).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::conn::{ConnConfig, connect_and_auth};
use vike_ctrader::data::CtraderData;
use vike_data::DataClient;

use common::{FakeCtrader, RecordingSink, SPOT_EVENT_TIMESTAMP_MS};

/// Poll `sink.quotes` until at least one arrived or the deadline elapses — the SPOT_EVENT is
/// pushed by the fake server asynchronously (a background thread), so there is no synchronous
/// point to block on within `subscribe_quotes` itself (fire-and-forget, per `DataClient`'s
/// contract).
fn wait_for_quote(
    sink: &RecordingSink,
    timeout: Duration,
) -> Option<(String, String, vike_model::QuoteTick)> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(first) = sink.quotes().first().cloned() {
            return Some(first);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn spot_event_reaches_the_sink_as_a_descaled_quote() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let handle = connect_and_auth(cfg, sink.clone()).expect("auth ok");
    let mut data = CtraderData::new(handle);

    data.subscribe_quotes("EURUSD").expect("subscribe ok");

    let (venue, symbol, quote) =
        wait_for_quote(&sink, Duration::from_secs(5)).expect("expected a quote to arrive");
    assert_eq!(venue, "ctrader");
    assert_eq!(symbol, "EURUSD");
    assert!((quote.bid - 1.13911).abs() < 1e-9, "bid={}", quote.bid);
    assert!((quote.ask - 1.13912).abs() < 1e-9, "ask={}", quote.ask);

    server.assert_saw(&["SUBSCRIBE_SPOTS_REQ"]);
}

/// Locks `conn::on_inbound`'s `ts: ev.timestamp.unwrap_or_else(now_ms)` behavior: the scripted
/// `SPOT_EVENT`'s `timestamp` (`SPOT_EVENT_TIMESTAMP_MS`, a live-captured 13-digit value — already
/// epoch milliseconds, live-verified against a concurrent `now_ms()` reading) must flow into
/// `QuoteTick::ts` UNCHANGED — no rescaling, no seconds<->milliseconds conversion.
#[test]
fn spot_event_timestamp_flows_through_as_milliseconds_unchanged() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let handle = connect_and_auth(cfg, sink.clone()).expect("auth ok");
    let mut data = CtraderData::new(handle);

    data.subscribe_quotes("EURUSD").expect("subscribe ok");

    let (_, _, quote) =
        wait_for_quote(&sink, Duration::from_secs(5)).expect("expected a quote to arrive");
    assert_eq!(
        quote.ts, SPOT_EVENT_TIMESTAMP_MS,
        "quote.ts should equal the wire timestamp verbatim"
    );
}

#[test]
fn subscribe_quotes_unknown_symbol_is_unsupported() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let handle = connect_and_auth(cfg, sink).expect("auth ok");
    let mut data = CtraderData::new(handle);

    match data.subscribe_quotes("GBPUSD") {
        Err(vike_data::LiveDataError::Unsupported(_)) => {}
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn subscribe_bars_unknown_interval_is_unsupported() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let handle = connect_and_auth(cfg, sink).expect("auth ok");
    let mut data = CtraderData::new(handle);

    match data.subscribe_bars("EURUSD", "3s") {
        Err(vike_data::LiveDataError::Unsupported(_)) => {}
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

/// The two live-data verbs cTrader declares NO capability for now refuse THROUGH
/// `require_live_verb` (driven off `VenueCaps.live_data.{trades,book}` = false) rather than a
/// hand-rolled message — the refusal stays pinned to the declared matrix. Same observable outcome
/// (an `Unsupported` variant), which this asserts still holds for both verbs.
#[test]
fn trades_and_book_are_caps_refused() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let handle = connect_and_auth(cfg, sink).expect("auth ok");
    let mut data = CtraderData::new(handle);

    assert!(matches!(
        data.subscribe_trades("EURUSD"),
        Err(vike_data::LiveDataError::Unsupported(_))
    ));
    assert!(matches!(data.subscribe_book("EURUSD"), Err(vike_data::LiveDataError::Unsupported(_))));
}

#[test]
fn subscribe_bars_sends_spots_then_trendbar_subscribe() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let handle = connect_and_auth(cfg, sink).expect("auth ok");
    let mut data = CtraderData::new(handle);

    data.subscribe_bars("EURUSD", "1m").expect("subscribe ok");
    server.wait_until_saw(
        &["SUBSCRIBE_SPOTS_REQ", "SUBSCRIBE_LIVE_TRENDBAR_REQ"],
        Duration::from_secs(3),
    );
}

#[test]
fn unsubscribe_sends_the_matching_unsubscribe_command() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let handle = connect_and_auth(cfg, sink).expect("auth ok");
    let mut data = CtraderData::new(handle);

    let id = data.subscribe_quotes("EURUSD").expect("subscribe ok");
    data.unsubscribe(id);
    server
        .wait_until_saw(&["SUBSCRIBE_SPOTS_REQ", "UNSUBSCRIBE_SPOTS_REQ"], Duration::from_secs(3));

    // Unknown/already-removed id is a documented no-op — never panics.
    data.unsubscribe(id);
}
