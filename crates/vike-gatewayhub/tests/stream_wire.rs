//! The `stream` class on the WIRE: the frames and numbers a raw peer sees, which a client library
//! hides. Key ids and `seq` per `(k, verb)`; `SubOk` before any frame carrying its `k`; the closing
//! frames and their reasons; liveness by heartbeat instead of an idle timeout; and the caps.

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use vike_data::LiveDataSink;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::proto::{Request, Response};
use vike_hub_proto::auth::Scope;
use vike_market_stream::wire::{ByeReason, StreamDown, StreamUp, SubErrKind, SubOp};
use vike_marketdata::{Bar, QuoteTick, TradeTick};
use vike_model::StreamStatus;

use harness::{Harness, test_keys};
use raw_peer::Raw;

fn open(h: &Harness, venue: &str) -> Raw {
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    raw.open_stream(venue);
    raw
}

fn sub(raw: &mut Raw, sid: u64, op: SubOp) -> StreamDown {
    raw.up(&StreamUp::Sub { sid, op, class: None });
    raw.down_until(|f| matches!(f, StreamDown::SubOk { sid: s, .. } | StreamDown::SubErr { sid: s, .. } if *s == sid))
        .pop()
        .expect("a reply")
}

fn quotes(sym: &str) -> SubOp {
    SubOp::Quotes { symbol: sym.to_string() }
}

fn tick(ts: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    }
}

fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 3.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn keys_are_per_symbol_and_sequences_are_per_key_and_verb() {
    let h = Harness::start();
    let mut raw = open(&h, "binance");
    let btc_q = sub(&mut raw, 1, quotes("BTCUSDT"));
    let btc_t = sub(&mut raw, 2, SubOp::Trades { symbol: "BTCUSDT".into() });
    let eth_q = sub(&mut raw, 3, quotes("ETHUSDT"));
    let key = |f: &StreamDown| match f {
        StreamDown::SubOk { k, .. } => *k,
        other => panic!("{other:?}"),
    };
    assert_eq!(key(&btc_q), key(&btc_t), "one symbol, one key");
    assert_ne!(key(&btc_q), key(&eth_q));

    let sink = h.data.session(0).sink.clone();
    for ts in 0..3 {
        sink.quote("binance", "BTCUSDT", tick(ts));
    }
    for ts in 0..2 {
        sink.quote("binance", "ETHUSDT", tick(ts));
    }
    for ts in 0..2 {
        sink.trade(
            "binance",
            "BTCUSDT",
            TradeTick {
                ts,
                local_ts: 0,
                price: 1.0,
                size: 1.0,
                is_buyer_maker: false,
                symbol: String::new(),
            },
        );
    }
    let mut seen: Vec<(char, u32, u64)> = Vec::new();
    while seen.len() < 7 {
        match raw.down() {
            StreamDown::Quote { k, seq, .. } => seen.push(('q', k, seq)),
            StreamDown::Trade { k, seq, .. } => seen.push(('t', k, seq)),
            StreamDown::Heartbeat => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    let btc = key(&btc_q);
    let eth = key(&eth_q);
    let of = |kind: char, k: u32| -> Vec<u64> {
        seen.iter().filter(|(c, kk, _)| *c == kind && *kk == k).map(|(_, _, s)| *s).collect()
    };
    assert_eq!(of('q', btc), vec![1, 2, 3]);
    assert_eq!(of('q', eth), vec![1, 2]);
    assert_eq!(of('t', btc), vec![1, 2]);
}

#[test]
fn a_seed_emitted_inside_subscribe_comes_after_its_sub_ok_on_the_wire() {
    let h = Harness::start();
    h.data.set_default_hook(Arc::new(|sink: &dyn LiveDataSink, verb: &str, symbol: &str| {
        if verb == "bars[1m]" {
            sink.seed_bars("binance", symbol, "1m", vec![bar(0), bar(60_000)]);
            sink.stream_status(
                "binance",
                symbol,
                "1m",
                StreamStatus::Live { gap_started_ts_ms: None },
            );
        }
    }));
    let mut raw = open(&h, "binance");
    raw.up(&StreamUp::Sub {
        sid: 1,
        op: SubOp::Bars { symbol: "BTCUSDT".into(), interval: "1m".into() },
        class: None,
    });
    let ok = raw.down();
    let k = match ok {
        StreamDown::SubOk { sid: 1, k } => k,
        other => panic!("the first frame must be the SubOk, not {other:?}"),
    };
    match raw.down() {
        StreamDown::Seed { k: sk, seq: 1, bars, .. } => {
            assert_eq!(sk, k);
            assert_eq!(bars.len(), 2);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(raw.down(), StreamDown::Status { k: sk, .. } if sk == k));
}

#[test]
fn a_repeated_sub_is_answered_again_and_the_venue_is_asked_once() {
    let h = Harness::start();
    let mut raw = open(&h, "binance");
    let first = sub(&mut raw, 5, quotes("BTCUSDT"));
    let second = sub(&mut raw, 5, quotes("BTCUSDT"));
    match (first, second) {
        (StreamDown::SubOk { k: a, .. }, StreamDown::SubOk { k: b, .. }) => assert_eq!(a, b),
        other => panic!("{other:?}"),
    }
    let asked =
        h.data.session(0).calls().iter().filter(|c| c.starts_with("subscribe_quotes")).count();
    assert_eq!(asked, 1, "a repeated sid reached the venue client twice");
    // An unknown id is a no-op: no error, no reply, and the session goes on.
    raw.up(&StreamUp::Unsub { sid: 999 });
    raw.up(&StreamUp::Ping);
    assert!(matches!(raw.down(), StreamDown::Heartbeat));
}

#[test]
fn a_symbol_the_daemon_will_not_carry_is_refused_before_the_venue_is_asked() {
    let h = Harness::start();
    let mut raw = open(&h, "binance");
    match sub(&mut raw, 1, SubOp::Quotes { symbol: "A\nB".into() }) {
        StreamDown::SubErr { err: SubErrKind::SymbolRejected(text), .. } => {
            assert!(text.contains("control character"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert!(h.data.session(0).calls().is_empty(), "the venue client was asked");
}

#[test]
fn a_start_failure_is_a_sub_err_and_the_session_survives() {
    let h = Harness::start();
    h.data.refuse_all("BAD", fake_lanes::Refusal::Subscribe("nope".into()));
    h.data.refuse_all("NOTRADES", fake_lanes::Refusal::Unsupported("no live trade prints"));
    let mut raw = open(&h, "binance");
    assert!(matches!(
        sub(&mut raw, 1, quotes("BAD")),
        StreamDown::SubErr { err: SubErrKind::Subscribe(t), .. } if t == "nope"
    ));
    assert!(matches!(
        sub(&mut raw, 2, SubOp::Trades { symbol: "NOTRADES".into() }),
        StreamDown::SubErr { err: SubErrKind::Unsupported(t), .. } if t == "no live trade prints"
    ));
    assert!(matches!(sub(&mut raw, 3, quotes("GOOD")), StreamDown::SubOk { .. }));
}

#[test]
fn a_ping_is_answered_with_a_heartbeat_at_once() {
    let h = Harness::start();
    let mut raw = open(&h, "binance");
    raw.up(&StreamUp::Ping);
    assert!(matches!(raw.down(), StreamDown::Heartbeat));
}

#[test]
fn a_quiet_stream_is_proved_alive_by_heartbeats_and_is_not_reaped_by_the_idle_timeout() {
    let mut config = ServerConfig::PRODUCTION;
    config.timings.stream_heartbeat = Duration::from_millis(120);
    config.timings.idle_read_timeout = Duration::from_millis(250);
    let h = Harness::start_config(config);
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    let (_session, _epoch, heartbeat_ms) = raw.open_stream("binance");
    assert_eq!(heartbeat_ms, 120, "the open reply must state the period the writer keeps");
    let started = Instant::now();
    let mut beats = 0;
    while started.elapsed() < Duration::from_millis(900) {
        match raw.recv_within::<StreamDown>(Duration::from_millis(1_000)) {
            Ok(StreamDown::Heartbeat) => beats += 1,
            Ok(other) => panic!("{other:?}"),
            Err(e) => panic!("the stream died after {:?}: {e}", started.elapsed()),
        }
    }
    assert!(beats >= 3, "only {beats} heartbeats in 900 ms at a 120 ms period");
    // The same idle timeout does reap an rpc connection that goes quiet.
    let mut quiet = Raw::open(h.addr, &test_keys(), Scope::Read);
    assert!(quiet.closed_within(Duration::from_secs(3)), "an idle rpc connection was kept");
}

#[test]
fn close_is_answered_with_bye_client_closed_and_the_venue_client_is_shut_down() {
    let h = Harness::start();
    let mut raw = open(&h, "binance");
    assert!(matches!(sub(&mut raw, 1, quotes("BTCUSDT")), StreamDown::SubOk { .. }));
    raw.up(&StreamUp::BeginShutdown);
    raw.up(&StreamUp::Close);
    let frames = raw.down_until(|f| matches!(f, StreamDown::Bye(_)));
    assert!(matches!(frames.last(), Some(StreamDown::Bye(ByeReason::ClientClosed))), "{frames:?}");
    assert!(raw.closed_within(Duration::from_secs(3)));
    let session = h.data.session(0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.shutdowns.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "the venue client was never shut down");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        session.begin_shutdowns.load(Ordering::SeqCst),
        2,
        "begin_shutdown, then the teardown's own"
    );
}

#[test]
fn a_link_that_just_vanishes_still_shuts_the_venue_client_down() {
    let h = Harness::start();
    let mut raw = open(&h, "binance");
    assert!(matches!(sub(&mut raw, 1, quotes("BTCUSDT")), StreamDown::SubOk { .. }));
    drop(raw);
    let session = h.data.session(0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.shutdowns.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "a vanished client left its venue client running");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn an_undecodable_stream_frame_closes_the_session() {
    let h = Harness::start();
    let mut raw = open(&h, "binance");
    raw.send(&"NotAStreamUpVerb");
    assert!(raw.closed_within(Duration::from_secs(3)));
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.data.session(0).shutdowns.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_session_holding_nothing_is_ended_idle() {
    let mut config = ServerConfig::PRODUCTION;
    config.timings.session_idle = Duration::from_millis(300);
    let h = Harness::start_config(config);
    let mut raw = open(&h, "binance");
    let frames = raw.down_until(|f| matches!(f, StreamDown::Bye(_)));
    assert!(matches!(frames.last(), Some(StreamDown::Bye(ByeReason::SessionIdle))), "{frames:?}");
}

#[test]
fn the_session_cap_is_a_refusal_that_keeps_the_connection_and_is_no_transient() {
    let mut config = ServerConfig::PRODUCTION;
    config.limits.max_stream_sessions = 1;
    config.limits.max_subs_per_session = 2;
    let h = Harness::start_config(config);
    let mut first = open(&h, "binance");
    // The second DataOpen finds the plane full: a positional Error, and the connection survives.
    let mut second = Raw::open(h.addr, &test_keys(), Scope::Read);
    second.send(&Request::DataOpen { venue: "binance".into(), network: None });
    match second.recv::<Response>() {
        Response::Error(text) => {
            assert!(text.contains("cap") && text.contains('1'), "{text}");
            // A cap persists until an operator raises it: it is not a transient a caller may restart on.
            assert!(text.contains(vike_market_stream::market::CAP_MARK), "{text}");
            assert!(!vike_market_stream::market::is_transient_refusal(&text), "{text}");
        }
        other => panic!("{other:?}"),
    }
    second.send(&Request::Ping);
    assert!(matches!(second.recv::<Response>(), Response::Pong));
    // The first session's own cap: the third distinct symbol is refused with a Budget error.
    assert!(matches!(sub(&mut first, 1, quotes("A")), StreamDown::SubOk { .. }));
    assert!(matches!(sub(&mut first, 2, quotes("B")), StreamDown::SubOk { .. }));
    assert!(matches!(
        sub(&mut first, 3, quotes("C")),
        StreamDown::SubErr { err: SubErrKind::Budget { held: 2, cap: 2, .. }, .. }
    ));
    // Ending the first session frees the place.
    first.up(&StreamUp::Close);
    first.down_until(|f| matches!(f, StreamDown::Bye(_)));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        second.send(&Request::DataOpen { venue: "binance".into(), network: None });
        match second.recv::<Response>() {
            Response::DataOpened { .. } => break,
            Response::Error(_) => {
                assert!(Instant::now() < deadline, "the plane never freed a place");
                std::thread::sleep(Duration::from_millis(20));
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_venue_the_build_does_not_stream_is_refused_naming_what_it_does() {
    let h = Harness::start();
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    raw.send(&Request::DataOpen { venue: "okx".into(), network: None });
    match raw.recv::<Response>() {
        Response::Error(text) => {
            assert!(
                text.contains("`okx`") && text.contains("md_venue=") && text.contains("binance"),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }
    // The connection stayed positional.
    raw.send(&Request::Ping);
    assert!(matches!(raw.recv::<Response>(), Response::Pong));
    assert_eq!(h.data.opened(), 0);
}

/// **The per-session cap refuses by NAME, and the venue is never asked.** A stream subscription is, at
/// the CEX bridges, a thread and a socket at the venue; the gatewayhub's session cap is what stands
/// between one peer's frames and that cost (the datahub's own per-venue key cap sits in front of it for
/// the clients it serves). So a `Sub` past the cap must be answered with the cap's name and numbers and
/// must not reach `subscribe_*` at all, however many are sent. The plane cap is the same kind of fence
/// one level up: a refused `DataOpen` builds no venue client.
#[test]
fn the_caps_refuse_by_name_before_any_venue_socket_is_opened() {
    let mut config = ServerConfig::PRODUCTION;
    config.limits.max_stream_sessions = 1;
    config.limits.max_subs_per_session = 3;
    let h = Harness::start_config(config);
    let mut raw = open(&h, "binance");
    assert_eq!(h.data.opened(), 1);

    // The plane cap: a second session is refused and no second venue client exists.
    let mut second = Raw::open(h.addr, &test_keys(), Scope::Read);
    second.send(&Request::DataOpen { venue: "binance".into(), network: None });
    match second.recv::<Response>() {
        Response::Error(text) => assert!(text.contains("cap"), "{text}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(h.data.opened(), 1, "a refused session built a venue client");

    // The session cap: three subscriptions are held, and the venue was asked three times.
    for (sid, symbol) in [(1, "A"), (2, "B"), (3, "C")] {
        assert!(matches!(sub(&mut raw, sid, quotes(symbol)), StreamDown::SubOk { .. }));
    }
    let asked = h.data.session(0).calls();
    assert_eq!(asked, ["subscribe_quotes A", "subscribe_quotes B", "subscribe_quotes C"]);

    // Every further distinct symbol is refused by name, with the held count and the cap.
    for sid in 4..=40u64 {
        match sub(&mut raw, sid, quotes(&format!("S{sid}"))) {
            StreamDown::SubErr { err: SubErrKind::Budget { scope, held: 3, cap: 3 }, .. } => {
                assert_eq!(scope, "subscriptions per session");
            }
            other => panic!("sid {sid} past the cap: {other:?}"),
        }
    }
    assert_eq!(
        h.data.session(0).calls(),
        asked,
        "a subscription past the cap reached the venue client (a thread and a socket at the venue)"
    );

    // Ending one subscription frees its place, and only then is the venue asked again.
    raw.up(&StreamUp::Unsub { sid: 1 });
    assert!(matches!(sub(&mut raw, 41, quotes("D")), StreamDown::SubOk { .. }));
    let calls = h.data.session(0).calls();
    assert_eq!(&calls[asked.len()..], ["unsubscribe 1", "subscribe_quotes D"]);
}
