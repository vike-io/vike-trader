#![cfg(feature = "data")]
//! **R1 — the `DataClient` contract, held over the hop.** `RemoteDataClient` must be
//! indistinguishable from an in-process venue client to the feed broker that owns it: `subscribe_*`
//! is fire-and-forget over an ack, `Err` keeps its variant and text, `Unsupported(&'static str)`
//! survives by interning, an unknown `unsubscribe` is a no-op, and the two-phase teardown means what
//! it means in `vike_data::live`. Each test names the clause it holds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{
    DataClient, LiveDataError, LiveDataSink, RecordingSink, SinkCall, SubscriptionId,
    require_live_verb,
};
use vike_gatewayhub_client::client::{GatewayLink, GatewayTimings};
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::wire::{
    ByeReason, GapVerb, StreamDown, StreamUp, SubErrKind, SubOp, WireStream,
};
use vike_marketdata::{QuoteTick, TradeTick};
use vike_model::StreamStatus;
use vike_venues::LiveVerb;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_peer.rs"]
mod fake_peer;

use fake_peer::{FakePeer, PeerConfig, SubAnswer, dead_addr, fast_timings, link_to};

const WAIT: Duration = Duration::from_secs(5);

fn client(link: GatewayLink, venue: &str) -> (RemoteDataClient, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn LiveDataSink> = sink.clone();
    (
        RemoteDataClient::new(GatewayOpener::new(link), venue, dyn_sink)
            .expect("the reader thread"),
        sink,
    )
}

fn quote(ts: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: ts,
        bid: 1.0,
        ask: 2.0,
        bid_size: 3.0,
        ask_size: 4.0,
        symbol: String::new(),
    }
}

fn trade(ts: i64) -> TradeTick {
    TradeTick {
        ts,
        local_ts: ts,
        price: 1.5,
        size: 0.5,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

fn wait_calls(sink: &RecordingSink, n: usize) -> Vec<SinkCall> {
    let deadline = Instant::now() + WAIT;
    while sink.recorded().len() < n {
        assert!(Instant::now() < deadline, "the sink saw {} of {n} calls", sink.recorded().len());
        std::thread::sleep(Duration::from_millis(2));
    }
    sink.recorded()
}

#[test]
fn subscribe_returns_after_the_ack_with_a_client_allocated_sequential_id() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link(), "binance");

    let a = c.subscribe_quotes("BTCUSDT.P").expect("acked");
    let b = c.subscribe_trades("BTCUSDT.P").expect("acked");
    assert_eq!((a, b), (SubscriptionId(1), SubscriptionId(2)), "ids are the client's, sequential");
    assert_eq!(
        peer.ups(),
        [
            StreamUp::Sub { sid: 1, op: SubOp::Quotes { symbol: "BTCUSDT.P".into() }, class: None },
            StreamUp::Sub { sid: 2, op: SubOp::Trades { symbol: "BTCUSDT.P".into() }, class: None },
        ],
        "the daemon saw exactly the two Sub frames, with the client's ids"
    );
}

#[test]
fn each_subscribe_verb_sends_its_own_lane() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link(), "binance");
    c.subscribe_bars("ETHUSDT", "5m").unwrap();
    c.subscribe_quotes("q").unwrap();
    c.subscribe_trades("t").unwrap();
    c.subscribe_book("b").unwrap();
    c.subscribe_depth("d").unwrap();
    let ops: Vec<SubOp> = peer
        .ups()
        .into_iter()
        .filter_map(|u| match u {
            StreamUp::Sub { op, .. } => Some(op),
            _ => None,
        })
        .collect();
    assert_eq!(
        ops,
        [
            SubOp::Bars { symbol: "ETHUSDT".into(), interval: "5m".into() },
            SubOp::Quotes { symbol: "q".into() },
            SubOp::Trades { symbol: "t".into() },
            SubOp::Book { symbol: "b".into() },
            SubOp::Depth { symbol: "d".into() },
        ]
    );
}

#[test]
fn a_subscribe_error_keeps_its_variant_and_its_text() {
    let peer = FakePeer::start(PeerConfig::new().with_sub(|_, op| {
        SubAnswer::Err(match op.symbol() {
            "bad" => SubErrKind::Subscribe("the OS refused a feed thread".into()),
            "rejected" => SubErrKind::SymbolRejected("a subscription symbol is BLANK".into()),
            "full" => SubErrKind::Budget { scope: "venue".into(), held: 16, cap: 16 },
            _ => SubErrKind::Draining,
        })
    }));
    let (mut c, _sink) = client(peer.link(), "binance");

    let Err(LiveDataError::Subscribe(text)) = c.subscribe_quotes("bad") else {
        panic!("Subscribe")
    };
    assert_eq!(text, "the OS refused a feed thread", "the inner text is byte-identical");
    let Err(LiveDataError::Subscribe(text)) = c.subscribe_trades("rejected") else { panic!() };
    assert_eq!(text, "a subscription symbol is BLANK");
    let Err(LiveDataError::Subscribe(text)) = c.subscribe_book("full") else { panic!() };
    assert!(
        text.contains("venue") && text.contains("16"),
        "a budget refusal names scope and cap: {text}"
    );
    let Err(LiveDataError::Subscribe(text)) = c.subscribe_depth("other") else { panic!() };
    assert!(text.contains("draining"), "{text}");
    assert_eq!(c.held(), 0, "a refused subscription is not held, so it is never replayed");
}

#[test]
fn unsupported_survives_as_a_static_str_and_a_canonical_reason_is_the_same_pointer() {
    let canonical = {
        let Err(LiveDataError::Unsupported(text)) = require_live_verb("", LiveVerb::Depth) else {
            panic!("an unknown venue refuses every verb")
        };
        text
    };
    let peer = FakePeer::start(PeerConfig::new().with_sub(|_, op| {
        SubAnswer::Err(SubErrKind::Unsupported(match op.symbol() {
            "canonical" => {
                let Err(LiveDataError::Unsupported(text)) = require_live_verb("", LiveVerb::Depth)
                else {
                    unreachable!()
                };
                text.to_string()
            }
            _ => "contract-test reason: this bridge serves no such lane".to_string(),
        }))
    }));
    let (mut c, _sink) = client(peer.link(), "binance");

    let Err(LiveDataError::Unsupported(got)) = c.subscribe_depth("canonical") else { panic!() };
    assert_eq!(got, canonical);
    assert!(std::ptr::eq(got, canonical), "a canonical reason maps back to the SAME &'static str");

    let Err(LiveDataError::Unsupported(first)) = c.subscribe_depth("foreign") else { panic!() };
    let Err(LiveDataError::Unsupported(second)) = c.subscribe_depth("foreign") else { panic!() };
    assert_eq!(first, "contract-test reason: this bridge serves no such lane");
    assert!(std::ptr::eq(first, second), "a foreign reason is interned once");
}

#[test]
fn an_unknown_unsubscribe_is_a_no_op_and_a_known_one_sends_one_frame() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link(), "binance");
    c.unsubscribe(SubscriptionId(999)); // never issued: no panic, no frame
    let id = c.subscribe_quotes("X").unwrap();
    c.unsubscribe(id);
    c.unsubscribe(id); // already stopped: a no-op again
    let probe = c.subscribe_trades("Y").unwrap(); // a later ack proves the earlier frames were read
    assert_eq!(probe, SubscriptionId(2));
    assert_eq!(
        peer.ups(),
        [
            StreamUp::Sub { sid: 1, op: SubOp::Quotes { symbol: "X".into() }, class: None },
            StreamUp::Unsub { sid: 1 },
            StreamUp::Sub { sid: 2, op: SubOp::Trades { symbol: "Y".into() }, class: None },
        ],
        "one Unsub, none for the unknown id, none for the repeat"
    );
}

#[test]
fn begin_shutdown_returns_at_once_and_the_session_stays_usable() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link(), "binance");
    c.subscribe_quotes("X").unwrap();
    let started = Instant::now();
    c.begin_shutdown();
    assert!(started.elapsed() < Duration::from_millis(500), "phase one joins nothing");
    // The broker's drain can end and a new key can be wanted: the session still serves.
    c.subscribe_trades("Y").expect("a begun shutdown is not a closed session");
    assert!(
        FakePeer::wait_until(WAIT, || peer.ups().contains(&StreamUp::BeginShutdown)),
        "the daemon was told to raise its stop flags"
    );
    assert!(!peer.ups().contains(&StreamUp::Close), "begin_shutdown closes nothing");
}

#[test]
fn shutdown_closes_joins_and_is_idempotent_and_a_subscribe_after_it_is_refused() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link(), "binance");
    c.subscribe_quotes("X").unwrap();

    let started = Instant::now();
    c.shutdown();
    assert!(started.elapsed() < Duration::from_secs(3), "the reader saw Bye and joined");
    assert!(peer.ups().contains(&StreamUp::Close), "the daemon was told to close");
    c.shutdown(); // a second call is a no-op
    let Err(LiveDataError::Subscribe(text)) = c.subscribe_quotes("Z") else {
        panic!("a shut-down client starts nothing")
    };
    assert!(text.contains("shut down"), "{text}");
    drop(c); // Drop after shutdown is also a no-op
}

#[test]
fn dropping_a_client_closes_its_session_without_hanging() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link(), "binance");
    c.subscribe_book("X").unwrap();
    let started = Instant::now();
    drop(c);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(FakePeer::wait_until(WAIT, || peer.ups().contains(&StreamUp::Close)));
}

/// "It never blocks the broker's per-venue mutex on a dead socket."
#[test]
fn a_subscribe_on_a_dead_gateway_is_a_fast_transient_error_not_a_stall() {
    let link = link_to(&dead_addr());
    let (mut c, _sink) = client(link.clone(), "binance");

    let started = Instant::now();
    let Err(LiveDataError::Subscribe(text)) = c.subscribe_quotes("X") else { panic!() };
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "the first subscribe waits for the dial in flight and a refused dial ends at once: {:?}",
        started.elapsed()
    );
    assert!(text.contains("gatewayhub") && text.contains(link.addr()), "{text}");

    let again = Instant::now();
    assert!(matches!(c.subscribe_trades("X"), Err(LiveDataError::Subscribe(_))));
    assert!(
        again.elapsed() < Duration::from_millis(200),
        "a link that is down answers with no wait at all: {:?}",
        again.elapsed()
    );
    let t = Instant::now();
    c.shutdown();
    assert!(t.elapsed() < Duration::from_secs(3), "shutdown is not held up by the backoff sleep");
}

/// The ack is bounded: a half-open daemon holds the broker's mutex for `sub_reply`, not for ever.
#[test]
fn a_silent_daemon_times_the_ack_out_and_a_tombstone_unsub_follows() {
    let peer = FakePeer::start(PeerConfig::new().with_sub(|_, _| SubAnswer::Silent));
    let timings = GatewayTimings { sub_reply: Duration::from_millis(400), ..fast_timings() };
    let (mut c, _sink) = client(peer.link().with_timings(timings), "binance");

    let started = Instant::now();
    let Err(LiveDataError::Subscribe(text)) = c.subscribe_quotes("X") else { panic!() };
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(350) && took < Duration::from_secs(3), "{took:?}");
    assert!(text.contains("no ack"), "{text}");
    assert_eq!(c.held(), 0);
    assert!(
        FakePeer::wait_until(WAIT, || peer.ups().contains(&StreamUp::Unsub { sid: 1 })),
        "an abandoned subscription is tombstoned at the daemon"
    );
}

#[test]
fn two_lanes_on_one_symbol_share_a_key_and_frames_route_by_symbol() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link(), "binance");
    c.subscribe_quotes("ETHUSDT").unwrap();
    c.subscribe_trades("ETHUSDT").unwrap();
    c.subscribe_quotes("BTCUSDT").unwrap();
    let s = peer.wait_stream(0, WAIT);
    let (eth, btc) = (s.key_of("ETHUSDT").unwrap(), s.key_of("BTCUSDT").unwrap());
    assert_ne!(eth, btc);

    s.send(&StreamDown::Quote { k: eth, seq: 1, quote: quote(10) });
    s.send(&StreamDown::Trade { k: eth, seq: 1, trade: trade(11) });
    s.send(&StreamDown::Quote { k: btc, seq: 1, quote: quote(12) });
    let calls = wait_calls(&sink, 3);
    let symbols: Vec<String> = calls
        .iter()
        .map(|c| match c {
            SinkCall::Quote { venue, symbol, .. } | SinkCall::Trade { venue, symbol, .. } => {
                assert_eq!(venue, "binance");
                symbol.clone()
            }
            other => panic!("unexpected call {other}"),
        })
        .collect();
    assert_eq!(symbols, ["ETHUSDT", "ETHUSDT", "BTCUSDT"], "delivered in wire order, by symbol");
}

#[test]
fn a_frame_for_a_key_the_session_never_bound_is_dropped_and_the_stream_goes_on() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link(), "binance");
    c.subscribe_quotes("X").unwrap();
    let s = peer.wait_stream(0, WAIT);
    let k = s.key_of("X").unwrap();
    s.send(&StreamDown::Quote { k: 4242, seq: 1, quote: quote(1) });
    s.send(&StreamDown::Quote { k, seq: 1, quote: quote(2) });
    let calls = wait_calls(&sink, 1);
    assert_eq!(calls.len(), 1, "only the bound key's frame was delivered");
    assert!(
        matches!(&calls[0], SinkCall::Quote { symbol, quote, .. } if symbol == "X" && quote.ts == 2)
    );
}

fn statuses(sink: &RecordingSink) -> Vec<(String, String, StreamStatus)> {
    sink.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::StreamStatus { symbol, stream, status, .. } => Some((symbol, stream, status)),
            _ => None,
        })
        .collect()
}

/// A jump in a lossless verb's `seq` with no `Gap` before it is a SILENT loss, and the client says so.
#[test]
fn a_silent_sequence_break_on_a_lossless_verb_is_disclosed_as_a_hole() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link(), "binance");
    c.subscribe_quotes("X").unwrap();
    let s = peer.wait_stream(0, WAIT);
    let k = s.key_of("X").unwrap();
    for seq in [1, 2, 4] {
        s.send(&StreamDown::Quote { k, seq, quote: quote(i64::try_from(seq).unwrap()) });
    }
    let calls = wait_calls(&sink, 5);
    let kinds: Vec<&str> = calls
        .iter()
        .map(|c| match c {
            SinkCall::Quote { .. } => "quote",
            SinkCall::StreamStatus { status: StreamStatus::GapStart { .. }, .. } => "gap",
            SinkCall::StreamStatus { status: StreamStatus::Live { .. }, .. } => "live",
            other => panic!("unexpected {other}"),
        })
        .collect();
    assert_eq!(
        kinds,
        ["quote", "quote", "gap", "live", "quote"],
        "the hole is disclosed BEFORE the frame after it"
    );
    let st = statuses(&sink);
    assert_eq!(st[0].1, "quotes", "the disclosure names the lane's stream word");
    // The hole opens at the LAST FRAME DELIVERED before it (the quote stamped 2), never at this
    // reader's arrival clock: replay orders rows by their stamps, so a marker stamped "now" would sort
    // after every survivor still queued behind a slow reader.
    assert_eq!(st[0].2, StreamStatus::GapStart { at_ts_ms: 2 });
    assert_eq!(st[1].2, StreamStatus::Live { gap_started_ts_ms: Some(2) });
}

#[test]
fn a_gap_frame_is_disclosed_once_and_the_frame_after_it_adds_no_second_hole() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link(), "binance");
    c.subscribe_bars("X", "1m").unwrap();
    let s = peer.wait_stream(0, WAIT);
    let k = s.key_of("X").unwrap();
    let bar = vike_marketdata::Bar {
        ts: 1,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    s.send(&StreamDown::BarClose { k, seq: 1, interval: "1m".into(), bar: bar.clone() });
    s.send(&StreamDown::Gap {
        k,
        verb: GapVerb::BarClose,
        stream: WireStream::Bar("1m".into()),
        dropped: 3,
        from_seq: 1,
        to_seq: 5,
        from_ts_ms: 1_700_000_000_123,
    });
    s.send(&StreamDown::BarClose { k, seq: 5, interval: "1m".into(), bar });
    let calls = wait_calls(&sink, 4);
    assert_eq!(calls.len(), 4, "close, gap, live, close — nothing extra: {calls:?}");
    let st = statuses(&sink);
    assert_eq!(st.len(), 2);
    assert_eq!(st[0].1, "1m", "a bar lane's disclosure carries its interval");
    // Both markers are stamped by the frame (when the hole OPENED), not by this client's clock.
    assert_eq!(st[0].2, StreamStatus::GapStart { at_ts_ms: 1_700_000_000_123 });
    assert_eq!(st[1].2, StreamStatus::Live { gap_started_ts_ms: Some(1_700_000_000_123) });
}

#[test]
fn a_conflatable_verb_may_skip_sequence_numbers_without_a_disclosure() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link(), "binance");
    c.subscribe_depth("X").unwrap();
    let s = peer.wait_stream(0, WAIT);
    let k = s.key_of("X").unwrap();
    for seq in [1, 50, 51, 9000] {
        s.send(&StreamDown::Mark { k, seq, px: 1.0, ts: 1 });
    }
    s.send(&StreamDown::Heartbeat);
    let calls = wait_calls(&sink, 4);
    assert!(calls.iter().all(|c| matches!(c, SinkCall::MarkTick { .. })), "{calls:?}");
    assert!(statuses(&sink).is_empty(), "superseding in place is not a loss");
}

/// The daemon ending a session with a reason is a loss like any other when keys are held.
#[test]
fn bye_too_slow_discloses_the_loss_and_the_session_is_rebuilt() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link(), "binance");
    c.subscribe_quotes("X").unwrap();
    let s = peer.wait_stream(0, WAIT);
    s.bye(ByeReason::TooSlow { lapses: 9 });
    let st = {
        let deadline = Instant::now() + WAIT;
        loop {
            let st = statuses(&sink);
            if st.len() >= 2 || Instant::now() > deadline {
                break st;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    assert!(matches!(st[0].2, StreamStatus::GapStart { .. }), "{st:?}");
    assert!(matches!(st[1].2, StreamStatus::Live { gap_started_ts_ms: Some(_) }), "{st:?}");
    let _second = peer.wait_stream(1, WAIT);
    assert_eq!(
        peer.ups().iter().filter(|u| matches!(u, StreamUp::Sub { sid: 1, .. })).count(),
        2,
        "the held subscription was replayed under its ORIGINAL sid"
    );
}

/// The datahub's `ClientBuilder` shape — what `FeedBroker::new` takes — built from this client. The
/// type is spelled out so the day either side moves, THIS stops compiling.
type BrokerBuilder = Box<
    dyn Fn(&str, Arc<dyn LiveDataSink>) -> Result<Box<dyn DataClient + Send>, String> + Send + Sync,
>;

#[test]
fn the_client_is_the_brokers_client_type_and_its_builder_never_dials() {
    let peer = FakePeer::start(PeerConfig::new());
    let link = peer.link();
    let builder: BrokerBuilder = Box::new(move |venue, sink| {
        RemoteDataClient::new(GatewayOpener::new(link.clone()), venue, sink)
            .map(|c| Box::new(c) as Box<dyn DataClient + Send>)
            .map_err(|e| e.to_string())
    });
    let sink: Arc<dyn LiveDataSink> = Arc::new(RecordingSink::default());
    let mut client = builder("binance", sink).expect("the broker's builder");
    client.subscribe_quotes("X").expect("the first subscribe rides the dial in flight");
    client.begin_shutdown();
    client.shutdown();

    // …and with the gatewayhub down the builder still succeeds at once (drift register R17): the
    // broker holds its clients map across `build`, so a constructor that dialled would stall it.
    let down = link_to(&dead_addr());
    let builder: BrokerBuilder = Box::new(move |venue, sink| {
        RemoteDataClient::new(GatewayOpener::new(down.clone()), venue, sink)
            .map(|c| Box::new(c) as Box<dyn DataClient + Send>)
            .map_err(|e| e.to_string())
    });
    let started = Instant::now();
    let mut client =
        builder("binance", Arc::new(RecordingSink::default())).expect("not a failed constructor");
    assert!(started.elapsed() < Duration::from_millis(250), "{:?}", started.elapsed());
    client.shutdown();
}

/// A daemon that accepted the connection and then went quiet must not hold a shutdown for the whole
/// handshake deadline: the dial's socket is registered so `shutdown` can break it.
#[test]
fn shutdown_breaks_a_handshake_parked_on_a_daemon_that_went_quiet() {
    let quiet = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = quiet.local_addr().unwrap().to_string();
    let (mut c, _sink) = client(link_to(&addr), "binance");
    std::thread::sleep(Duration::from_millis(150)); // the reader is now parked reading `Welcome`

    let started = Instant::now();
    c.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the handshake deadline is 10 s; shutdown must not wait it out: {:?}",
        started.elapsed()
    );
    drop(quiet);
}
