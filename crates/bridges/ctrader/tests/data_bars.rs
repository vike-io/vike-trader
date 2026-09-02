//! Scripted integration test for the F2 historical-seed path: `CtraderData::subscribe_bars` now
//! fires a bounded `GetTrendbars` request BEFORE `SubscribeTrendbar`'s live stream (see
//! `data::CtraderData::subscribe_bars`'s doc); the fake cTrader server
//! (`tests/common/mod.rs::get_trendbars_res`) answers with two scripted trendbars, deliberately in
//! reverse-chronological (newer-first) wire order, and the actor thread
//! (`conn::on_get_trendbars_res`) must sort them ascending before handing the whole batch to the
//! `LiveDataSink` via ONE `seed_bars` call — independent of, and before, any live trendbar event.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::conn::{connect_and_auth, connect_and_auth_exec, ConnConfig};
use vike_ctrader::data::CtraderData;
use vike_data::DataClient;
use vike_exec::event_channel;
use vike_exec::lanes::Ingest;
use vike_model::events::Event;

use common::{
    FakeCtrader, RecordingSink, SeedBarsCall, SEED_BAR_NEWER_MINUTES, SEED_BAR_OLDER_MINUTES,
};

/// Poll `sink.seeded_bars` until at least one `seed_bars` call arrived or the deadline elapses —
/// the `GET_TRENDBARS_RES` reply is decoded and routed asynchronously by the actor thread, so
/// there's no synchronous point to block on within `subscribe_bars` itself (fire-and-forget).
fn wait_for_seed(sink: &RecordingSink, timeout: Duration) -> Option<SeedBarsCall> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(first) = sink.seeded_bars().first().cloned() {
            return Some(first);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn subscribe_bars_seeds_history_before_starting_the_live_stream() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let handle = connect_and_auth(cfg, sink.clone()).expect("auth ok");
    let mut data = CtraderData::new(handle);

    data.subscribe_bars("EURUSD", "1m").expect("subscribe ok");

    // The seed request/response round-trips independently of (and, per `subscribe_bars`'s command
    // order, before) the live `SubscribeTrendbar` request.
    let (venue, symbol, interval, bars) =
        wait_for_seed(&sink, Duration::from_secs(5)).expect("expected a seed_bars call to arrive");
    assert_eq!(venue, "ctrader");
    assert_eq!(symbol, "EURUSD");
    assert_eq!(interval, "1m");
    assert_eq!(bars.len(), 2, "both scripted trendbars should have made it through");

    // Chronological order: the fake server replies newer-first (`SEED_BAR_NEWER_MINUTES` after
    // `SEED_BAR_OLDER_MINUTES`), so `on_get_trendbars_res` must have sorted ascending by ts.
    assert_eq!(bars[0].ts, SEED_BAR_OLDER_MINUTES as i64 * 60_000);
    assert_eq!(bars[1].ts, SEED_BAR_NEWER_MINUTES as i64 * 60_000);
    assert!(bars[0].ts < bars[1].ts);

    // Descaled OHLC for the older (first-delivered) bar: low=113900, delta_open=5,
    // delta_close=12, delta_high=20 (all ÷ RELATIVE_PRICE_SCALE = 1e5) — see
    // `tests/common/mod.rs::get_trendbars_res`'s doc for the full scripted values.
    let older = &bars[0];
    assert!((older.low - 1.139).abs() < 1e-9, "low={}", older.low);
    assert!((older.open - 1.13905).abs() < 1e-9, "open={}", older.open);
    assert!((older.high - 1.1392).abs() < 1e-9, "high={}", older.high);
    assert!((older.close - 1.13912).abs() < 1e-9, "close={}", older.close);
    assert_eq!(older.volume, 42.0);

    server.wait_until_saw(
        &["GET_TRENDBARS_REQ", "SUBSCRIBE_SPOTS_REQ", "SUBSCRIBE_LIVE_TRENDBAR_REQ"],
        Duration::from_secs(3),
    );
}

/// F2b regression test: on a COMBINED data+exec connection (`connect_and_auth_exec`, matching the
/// module doc's "ONE socket carries both market data and order flow"), a `GetTrendbars` request
/// that the venue answers with `ERROR_RES` must NOT synthesize a phantom `Event::OrderRejected`.
/// Before the fix, `write_command` stamped every non-order command's envelope `clientMsgId` with a
/// static per-verb label ("gtb" for `GetTrendbars`) — cTrader echoes that id back on the
/// `ERROR_RES`, and `on_error_res`'s "non-empty clientMsgId ⇒ order reject" logic can't tell that
/// id apart from a real order's coid, so it fabricated an `OrderRejected{client_order_id:"gtb"}`
/// out of thin air. After the fix, non-order commands send an EMPTY envelope `clientMsgId`, so
/// `on_error_res` falls into its safe `warn!`-only branch instead.
#[test]
fn failing_get_trendbars_request_does_not_synthesize_phantom_order_reject() {
    let server = FakeCtrader::start_reject_trendbars_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, sink.clone(), events).expect("auth ok");
    let mut data = CtraderData::new(handle);

    data.subscribe_bars("EURUSD", "1m").expect("subscribe ok");

    // Confirm the failing request actually round-tripped before asserting on its aftermath.
    server.wait_until_saw(&["GET_TRENDBARS_REQ"], Duration::from_secs(3));
    // Give the actor thread a beat to decode+dispatch the ERROR_RES the fake replies with.
    std::thread::sleep(Duration::from_millis(300));

    let mut got: Vec<Event> = Vec::new();
    while let Ok(ing) = ingest.try_recv() {
        if let Ingest::Event(e) = ing {
            got.push(e);
        }
    }
    assert!(
        got.is_empty(),
        "a failed GetTrendbars (non-order command) must not emit any order event on the ingest \
         lane, got: {got:?}"
    );
    assert!(
        sink.seeded_bars().is_empty(),
        "no bars should have been seeded when GET_TRENDBARS_RES never arrived"
    );
}
