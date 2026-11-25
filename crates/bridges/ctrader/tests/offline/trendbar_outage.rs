//! **A live trendbar series across a socket RECONNECT**, end to end: the real actor over the in-process fake
//! (`vike_ctrader::test_support::TrendbarOutage`), whose first socket dies right after the live snapshots of a
//! period, so the actor really reconnects. What the sink is told is the strategy's input series
//! (`vike_ctrader::feed::bar::TrendbarCloses` closes a period at the next one's first snapshot), and a period
//! nothing closes is a hole in it. The fake never echoes a request's envelope id, so what tells the reconnect's fill
//! from a seed here is what was asked alone; the fills go one at a time, paced, beside the read (an order's event read
//! meanwhile is delivered at once), and a refusal is its fill's, the actor's probe's excepted.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_bridge_core::data_client::bus::DataClientBus;
use vike_ctrader::feed::decode::trendbar_to_bar;
use vike_ctrader::feed::{DataClientBusSpecCtrader, connect};
use vike_ctrader::test_support::{FakeCtrader, RecordingSink, TrendbarOutage};
use vike_ctrader::transport::CtraderClient;
use vike_ctrader::transport::connection::{ConnectionConfig, SEED_TIMEOUT};
use vike_ctrader::transport::proto::{ProtoOaTrendbar, ProtoOaTrendbarPeriod};
use vike_data::{DataClient, SinkCall};
use vike_marketdata::Bar;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

/// An hourly trendbar opened at `ts`, in the venue's 1/100000 points: its low, and its open, close and high above it.
fn h1(ts: i64, low: i64, open: u64, close: u64, high: u64, volume: i64) -> ProtoOaTrendbar {
    ProtoOaTrendbar {
        volume,
        period: Some(ProtoOaTrendbarPeriod::H1 as i32),
        low: Some(low),
        delta_open: Some(open),
        delta_close: Some(close),
        delta_high: Some(high),
        utc_timestamp_in_minutes: Some(u32::try_from(ts / MINUTE).expect("a stamp in minutes")),
    }
}

fn bar(tb: &ProtoOaTrendbar) -> Bar {
    trendbar_to_bar(tb).expect("a complete trendbar")
}

/// The four periods of an outage: `previous` (closed before it), `p` (the socket dies inside it, two snapshots heard),
/// `next` (the socket is back inside it; opened this minute, so an hour's width keeps it forming for the whole test)
/// and `after`.
struct Periods {
    previous: ProtoOaTrendbar,
    p_heard: [ProtoOaTrendbar; 2],
    p_stored: ProtoOaTrendbar,
    next_stored: ProtoOaTrendbar,
    next_heard: ProtoOaTrendbar,
    after_heard: ProtoOaTrendbar,
}

fn periods() -> Periods {
    let next = vike_model::now_ms() / MINUTE * MINUTE;
    let (previous, p, after) = (next - 2 * HOUR, next - HOUR, next + HOUR);
    Periods {
        previous: h1(previous, 110_000, 5, 12, 20, 40),
        p_heard: [h1(p, 110_010, 2, 8, 9, 3), h1(p, 110_010, 2, 15, 15, 5)],
        p_stored: h1(p, 109_990, 22, 41, 47, 31),
        next_stored: h1(next, 110_030, 3, 6, 9, 4),
        next_heard: h1(next, 110_030, 3, 11, 12, 6),
        after_heard: h1(after, 110_040, 1, 2, 3, 1),
    }
}

/// Connect to a fake venue serving `script` and subscribe EURUSD at each of `intervals`.
fn start(
    script: TrendbarOutage,
    intervals: &[&str],
) -> (FakeCtrader, Arc<RecordingSink>, DataClientBus<DataClientBusSpecCtrader>) {
    let server = FakeCtrader::start_trendbar_outage(script);
    let sink = Arc::new(RecordingSink::default());
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let mut data = connect(config, sink.clone()).expect("initial handshake ok");
    for interval in intervals {
        data.subscribe_bars("EURUSD", interval).expect("subscribe ok");
    }
    (server, sink, data)
}

/// Wait up to fifteen seconds for `done`, then assert it.
fn wait_for(sink: &RecordingSink, what: &str, done: impl Fn(&[SinkCall]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done(&sink.recorded()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(done(&sink.recorded()), "{what}: {:?}", sink.recorded());
}

/// Whether the sink heard a live snapshot of the period opened at `ts`.
fn heard(calls: &[SinkCall], ts: i64) -> bool {
    calls.iter().any(|c| matches!(c, SinkCall::FormingBar { bar, .. } if bar.timestamp == ts))
}

/// Every close of EURUSD the sink was told, with its interval.
fn closes(sink: &RecordingSink) -> Vec<(String, Bar)> {
    sink.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::CloseBar { symbol, interval, bar, .. } if symbol == "EURUSD" => {
                Some((interval, bar))
            }
            _ => None,
        })
        .collect()
}

fn seeds(sink: &RecordingSink) -> Vec<Vec<Bar>> {
    sink.seeded_bars().into_iter().map(|(_, _, _, bars)| bars).collect()
}

/// Run one outage of EURUSD 1h over `script` and return what the sink was told once it heard `last`'s snapshot (the
/// last thing the venue says) and two closes (a live close may wait for the fill, which can be read after that
/// snapshot): every close, and every seed.
fn closes_and_seeds(script: TrendbarOutage, last: &ProtoOaTrendbar) -> (Vec<Bar>, Vec<Vec<Bar>>) {
    let (server, sink, mut data) = start(script, &["1h"]);
    let last = bar(last).timestamp;
    wait_for(&sink, "the reconnected stream never reached its last period", |c| {
        heard(c, last) && c.iter().filter(|c| matches!(c, SinkCall::CloseBar { .. })).count() >= 2
    });
    data.shutdown();
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 2, "one outage, one reconnect");
    assert_eq!(server.count_seen("GET_TRENDBARS_REQ"), 2, "the subscribe's seed, then the fill");
    (closes(&sink).into_iter().map(|(_, bar)| bar).collect(), seeds(&sink))
}

/// **The period an outage hid is closed from the venue's STORED bar of it, before the stream's next close.** The venue
/// answers the fill at once, after the live snapshots the replay's subscribe started. The sink is told `p` from the
/// stored bar (never from the last snapshot before the outage), then `next` at `after`'s first snapshot, and the
/// subscribe's seed is the ONLY seed: a seed replaces the core's series.
#[test]
fn a_period_an_outage_hid_is_closed_from_the_venues_stored_bar() {
    let at = periods();
    let script = TrendbarOutage {
        stored_before: vec![at.previous],
        stored_after: vec![at.previous, at.p_stored, at.next_stored],
        live_before: at.p_heard.to_vec(),
        live_after: vec![at.next_heard, at.after_heard],
        ..Default::default()
    };
    let (closed, seeds) = closes_and_seeds(script, &at.after_heard);
    assert_eq!(
        closed,
        [bar(&at.p_stored), bar(&at.next_heard)],
        "`p` from the venue's stored bar, then `next` from its last snapshot"
    );
    assert_eq!(seeds, [vec![bar(&at.previous)]], "the subscribe's seed, and no other");
}

/// **A fill answered after its wait is still the fill's, and still exact while the stream has closed nothing later.**
/// The venue answers past the reconnect's wait, with `next` already heard: the answer, carrying no id of the request,
/// closes `p` from the stored bar, then `after`'s snapshot closes `next`; it is never a seed.
#[test]
fn a_fill_answered_after_its_wait_is_still_the_fills_and_never_a_seed() {
    let at = periods();
    let script = TrendbarOutage {
        stored_before: vec![at.previous],
        stored_after: vec![at.previous, at.p_stored, at.next_stored],
        live_before: at.p_heard.to_vec(),
        live_after: vec![at.next_heard],
        answer_after: SEED_TIMEOUT + Duration::from_secs(1),
        live_after_answer: vec![at.after_heard],
        ..Default::default()
    };
    let (closed, seeds) = closes_and_seeds(script, &at.after_heard);
    assert_eq!(closed, [bar(&at.p_stored), bar(&at.next_heard)], "late, and as exact");
    assert_eq!(seeds, [vec![bar(&at.previous)]], "the late answer is not a seed");
}

/// A trendbar of `period` opened at `ts`, its OHLC one price.
fn flat(period: ProtoOaTrendbarPeriod, ts: i64, low: i64) -> ProtoOaTrendbar {
    ProtoOaTrendbar { period: Some(period as i32), ..h1(ts, low, 0, 0, 0, 1) }
}

/// **More keys with a hole than the vendor takes history requests a second: every one is filled, the fills paced.**
/// Seven intervals of EURUSD, each seeded with `previous` before the outage and stored with `p` after it: each closes `p`
/// from the venue's stored bar, the only seeds are the subscribes' seven, and no second holds more than five of the
/// reconnect's asks (the vendor's limit per connection).
#[test]
fn more_keys_than_five_a_second_are_all_filled_and_the_fills_paced() {
    use ProtoOaTrendbarPeriod::{D1, H1, H4, H12, M5, M15, M30};
    // `next` opened this minute, so five minutes and more keep it forming for the whole test.
    let next = vike_model::now_ms() / MINUTE * MINUTE;
    let keys =
        [("5m", M5), ("15m", M15), ("30m", M30), ("1h", H1), ("4h", H4), ("12h", H12), ("1d", D1)];
    let mut script = TrendbarOutage { outage_at_subscribe: keys.len(), ..Default::default() };
    let mut want = Vec::new();
    for (k, (interval, period)) in keys.iter().enumerate() {
        let width = vike_ctrader::feed::decode::trendbar_period_ms(*period);
        let low = 110_000 + 100 * k as i64;
        let (previous, p) =
            (flat(*period, next - 2 * width, low), flat(*period, next - width, low + 1));
        script.stored_before.push(previous);
        script.stored_after.extend([previous, p, flat(*period, next, low + 2)]);
        want.push((interval.to_string(), bar(&p)));
    }
    let asked = script.fills_asked.clone();
    let intervals: Vec<&str> = keys.iter().map(|(interval, _)| *interval).collect();
    let (server, sink, mut data) = start(script, &intervals);
    wait_for(&sink, "every key's hole closed", |c| {
        c.iter().filter(|c| matches!(c, SinkCall::CloseBar { .. })).count() >= keys.len()
    });
    data.shutdown();
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 2, "one outage, one reconnect");

    let mut closed = closes(&sink);
    closed.sort_by(|a, b| a.0.cmp(&b.0));
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(closed, want, "each key's `p`, from the venue's stored bar");
    assert_eq!(seeds(&sink).len(), keys.len(), "the subscribes' seeds, and no other");
    let asked = asked.lock().expect("asked lock").clone();
    assert_eq!(asked.len(), keys.len(), "one fill per key with a hole");
    for (k, window) in asked.windows(6).enumerate() {
        let span = window[5] - window[0];
        assert!(span >= Duration::from_secs(1), "fills {k}..{} asked within {span:?}", k + 5);
    }
}

/// **An order's event read while a trendbar fill is outstanding is delivered AT ONCE**: the fills go beside the read,
/// never inside it. On one socket that carries orders and trendbars, the venue pushes a fill of an order the moment it
/// reads the reconnect's trendbar fill, and answers that fill three seconds later. The order's `OrderFilled` reaches the
/// ingest lane within half a second of the ask, with the hidden period still unclosed, and the late answer then closes
/// it from the venue's stored bar, ahead of the live close that waited for it.
#[test]
fn an_order_event_read_during_a_trendbar_fill_is_delivered_at_once() {
    use vike_ctrader::test_support::INFLIGHT_FILL_ORDER_CLIENT_ID;
    use vike_execution::{Ingest, event_channel};
    use vike_model::events::Event;

    let at = periods();
    let script = TrendbarOutage {
        stored_before: vec![at.previous],
        stored_after: vec![at.previous, at.p_stored, at.next_stored],
        live_before: at.p_heard.to_vec(),
        live_after: vec![at.next_heard],
        answer_after: Duration::from_secs(3),
        live_after_answer: vec![at.after_heard],
        event_at_fill: true,
        ..Default::default()
    };
    let asked = script.fills_asked.clone();
    let server = FakeCtrader::start_trendbar_outage(script);
    let sink = Arc::new(RecordingSink::default());
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(256);
    let client =
        CtraderClient::connect(config, sink.clone(), events).expect("initial handshake ok");
    let mut data = client.data();
    data.subscribe_bars("EURUSD", "1h").expect("subscribe ok");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut delivered = None;
    while delivered.is_none() && Instant::now() < deadline {
        match ingest.try_recv() {
            Ok(Ingest::Event(Event::OrderFilled(f)))
                if f.order_client_id == INFLIGHT_FILL_ORDER_CLIENT_ID =>
            {
                delivered = Some(Instant::now());
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let delivered = delivered.expect("the order's fill reached the ingest lane");
    let closed_by_then = closes(&sink);
    let asked_at = *asked.lock().expect("asked lock").first().expect("the fill was asked");
    let took = delivered.saturating_duration_since(asked_at);
    assert!(took < Duration::from_millis(500), "the order's event waited {took:?} behind the fill");
    assert!(closed_by_then.is_empty(), "the fill was still outstanding: {closed_by_then:?}");

    wait_for(&sink, "the late answer, then `after`", |c| heard(c, bar(&at.after_heard).timestamp));
    data.shutdown();
    let closed: Vec<Bar> = closes(&sink).into_iter().map(|(_, bar)| bar).collect();
    assert_eq!(
        closed,
        [bar(&at.p_stored), bar(&at.next_heard)],
        "`p` from the venue's stored bar, then `next`"
    );
}

/// **A refused fill is its key's**: the fills go one at a time, so the id-less refusal read while this one is
/// outstanding drops it, and that key's next answer (a second subscriber's seed) is a seed again, not taken as the fill.
#[test]
fn a_refused_fill_is_its_keys_and_that_keys_next_answer_is_a_seed() {
    let at = periods();
    let script = TrendbarOutage {
        stored_before: vec![at.previous],
        stored_after: vec![at.previous, at.p_stored, at.next_stored],
        live_before: at.p_heard.to_vec(),
        live_after: vec![at.next_heard],
        refuse_first_fill: true,
        ..Default::default()
    };
    let (server, sink, mut data) = start(script, &["1h"]);
    // The snapshot the replay's subscribe started is dispatched as it is read, the fill outstanding or not.
    let next = bar(&at.next_heard).timestamp;
    wait_for(&sink, "the reconnected stream never reached `next`", |c| heard(c, next));
    data.subscribe_bars("EURUSD", "1h").expect("a second subscriber of the key");
    wait_for(&sink, "the second subscriber's seed", |_| seeds(&sink).len() >= 2);
    data.shutdown();
    assert_eq!(
        server.count_seen("GET_TRENDBARS_REQ"),
        3,
        "the seed, the refused fill, the second seed"
    );
    assert!(
        closes(&sink).is_empty(),
        "a refused fill closes nothing, `p` is a hole until the seed files it: {:?}",
        closes(&sink)
    );
    assert_eq!(
        seeds(&sink),
        [vec![bar(&at.previous)], vec![bar(&at.previous), bar(&at.p_stored)]],
        "the second answer is a seed: the stored series up to now, its forming period held back"
    );
}

/// **A refusal of the actor's probe read while a fill waits is the PROBE's, never the fill's.** The probe asks once a
/// minute whatever is outstanding (narrowed here so one falls inside the wait). The venue holds the fill's answer,
/// refuses the probe echoing its id, in words that end no session, then answers the fill. The answer still closes `p`
/// from the venue's stored bar and is never a seed: a probe with no id would have dropped the fill, and its answer would
/// have replaced the series.
#[test]
fn a_probe_refused_during_a_fill_wait_leaves_the_fill_its_answer() {
    let at = periods();
    let script = TrendbarOutage {
        stored_before: vec![at.previous],
        stored_after: vec![at.previous, at.p_stored, at.next_stored],
        live_before: at.p_heard.to_vec(),
        live_after: vec![at.next_heard],
        live_after_answer: vec![at.after_heard],
        refuse_probe_during_fill: true,
        ..Default::default()
    };
    let server = FakeCtrader::start_trendbar_outage(script);
    let sink = Arc::new(RecordingSink::default());
    let mut config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    config.auth_probe_every = Duration::from_millis(200);
    let mut data = connect(config, sink.clone()).expect("initial handshake ok");
    data.subscribe_bars("EURUSD", "1h").expect("subscribe ok");
    let after = bar(&at.after_heard).timestamp;
    wait_for(&sink, "the fill's answer, then `after`", |c| {
        heard(c, after) && c.iter().filter(|c| matches!(c, SinkCall::CloseBar { .. })).count() >= 2
    });
    data.shutdown();
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 2, "one outage, one reconnect");
    let closed: Vec<Bar> = closes(&sink).into_iter().map(|(_, bar)| bar).collect();
    assert_eq!(
        closed,
        [bar(&at.p_stored), bar(&at.next_heard)],
        "`p` from the venue's stored bar, then `next`"
    );
    assert_eq!(seeds(&sink), [vec![bar(&at.previous)]], "the fill's answer is not a seed");
}
