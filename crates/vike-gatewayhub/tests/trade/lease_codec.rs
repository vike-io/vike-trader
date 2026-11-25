//! **The trade lease's codec, on the daemon's side** (`src/server/trade.rs`'s module doc): a `TradeOpen` written in
//! msgpack (a client that saw `wire_msgpack` advertised) is answered in msgpack and the whole lease is, both ways; one
//! written in JSON (an older tradehub) is answered in JSON, its reply the bytes it always read; a frame that does
//! not decode ends a msgpack lease with the same `TradeBye::Protocol` an undecodable JSON frame earns; and the REAL
//! client against this daemon moves its orders and fills whole, the money's bits included.
//!
//! The measuring member (`#[ignore]`d, print-only): the bytes and the encode and decode cost of the lease's
//! commonest frames and of a kline fetch page under either codec, `cargo test -p vike-gatewayhub --release --test
//! trade -- --ignored --nocapture lease_codec::`.
#![cfg(feature = "trade")]

use std::hint::black_box;
use std::time::{Duration, Instant};

use vike_execution::{ExecutionClient, Ingest};
use vike_gatewayhub::testkit::gateway::trade_keys;
use vike_gatewayhub::testkit::trade::ExecutionCall;
use vike_gatewayhub::testkit::{raw_peer, trade_rig};
use vike_gatewayhub_client::proto::{
    AdapterReply, DetachPhase, FetchRows, Request, Response, TradeBye, TradeDown, TradeUp,
};
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_hub_protocol::frame::{
    BodyCodec, MSGPACK_BODY_MAGIC, decode_body, encode_frame, read_frame_raw, write_frame_as,
};
use vike_model::AccountTier;
use vike_model::events::{Event, FillEvent, LiquiditySide, OrderFilled, PositionSide, TradeId};

use raw_peer::Raw;
use trade_rig::{Rig, open, order, rig, start_args};
use vike_log::poll::wait_until;

const WAIT: Duration = Duration::from_secs(5);

/// A raw lease opened in `codec`, and the open reply's body.
fn raw_lease(rig: &Rig, codec: BodyCodec) -> (Raw, Vec<u8>) {
    let mut raw = Raw::open(rig.gw.addr(), &trade_keys(), Scope::Trade);
    assert!(
        raw.features.iter().any(|f| f == FEATURE_WIRE_MSGPACK),
        "a daemon of this build advertises the codec: {:?}",
        raw.features
    );
    let open = Request::TradeOpen {
        client_id: "codec".to_string(),
        venue: "binance".to_string(),
        account: None,
        tier: AccountTier::Demo,
        resume: None,
        takeover: false,
        row: None,
    };
    write_frame_as(&mut raw.stream, codec, &open).expect("written");
    let body = read_frame_raw(&mut raw.stream).expect("the open reply");
    assert!(
        matches!(decode_body::<Response>(&body), Ok(Response::TradeOpened(_))),
        "{:?}",
        decode_body::<Response>(&body)
    );
    (raw, body)
}

/// The next lease frame that is not a heartbeat, and its body.
fn down(raw: &mut Raw) -> (TradeDown, Vec<u8>) {
    loop {
        let body = read_frame_raw(&mut raw.stream).expect("a frame");
        let frame = decode_body::<TradeDown>(&body).expect("a lease frame");
        if !matches!(frame, TradeDown::Heartbeat) {
            return (frame, body);
        }
    }
}

#[test]
fn a_lease_opened_in_each_codec_is_answered_in_it_both_ways() {
    let rig = rig();
    for codec in [BodyCodec::Msgpack, BodyCodec::Json] {
        let (mut raw, reply) = raw_lease(&rig, codec);
        assert_eq!(BodyCodec::of(&reply), codec, "the open reply");
        write_frame_as(&mut raw.stream, codec, &TradeUp::TradeStatus { rid: 7 }).expect("written");
        let (frame, body) = down(&mut raw);
        assert!(matches!(frame, TradeDown::TradeStatusIs { rid: 7, .. }), "{frame:?}");
        assert_eq!(BodyCodec::of(&body), codec, "a request's reply");
        write_frame_as(&mut raw.stream, codec, &TradeUp::Ping).expect("written");
        let (frame, body) = down(&mut raw);
        assert!(matches!(frame, TradeDown::Pong), "{frame:?}");
        assert_eq!(BodyCodec::of(&body), codec, "a control frame");
        let detach = TradeUp::Detach { phase: DetachPhase::Final };
        write_frame_as(&mut raw.stream, codec, &detach).expect("written");
        assert!(matches!(down(&mut raw).0, TradeDown::Bye(TradeBye::ClientClosed)));
    }
}

/// An older tradehub's open (JSON, the field set it always sent) reads the reply it always read: JSON, with no key
/// it does not know.
#[test]
fn an_older_clients_open_is_answered_in_json_with_the_reply_it_always_read() {
    let rig = rig();
    let (_raw, reply) = raw_lease(&rig, BodyCodec::Json);
    let text = String::from_utf8(reply).expect("JSON text");
    assert!(text.starts_with(r#"{"TradeOpened":{"gateway_epoch":"#), "{text}");
}

/// An undecodable frame ends a msgpack lease exactly as it ends a JSON one: a `Bye(Protocol)` saying so, in the
/// lease's codec, then a closed connection (the client rejects what it wrote and was not acknowledged).
#[test]
fn a_frame_that_does_not_decode_ends_a_msgpack_lease_with_a_protocol_bye() {
    let rig = rig();
    // A msgpack body whose value is not msgpack (`0xc1` is the one marker msgpack never uses), one cut short, and
    // a JSON body that names no verb.
    let msgpack = |tail: &[u8]| [&MSGPACK_BODY_MAGIC[..], tail].concat();
    let garbage =
        [msgpack(&[0xc1]), msgpack(&[0x81, 0xa4, b'P', b'i']), br#"{"NoSuchVerb":1}"#.to_vec()];
    for body in garbage {
        let (mut raw, _) = raw_lease(&rig, BodyCodec::Msgpack);
        let mut frame = u32::try_from(body.len()).expect("small").to_be_bytes().to_vec();
        frame.extend_from_slice(&body);
        raw.send_bytes(&frame);
        let (bye, bytes) = down(&mut raw);
        match bye {
            TradeDown::Bye(TradeBye::Protocol(words)) => {
                assert!(words.contains("undecodable"), "{words}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(BodyCodec::of(&bytes), BodyCodec::Msgpack, "in the lease's codec");
        assert!(raw.closed_within(Duration::from_secs(3)), "the connection closes after the Bye");
    }
}

/// The real client against this daemon: its orders and fills cross whole, the money's bits included.
#[test]
fn the_real_client_moves_orders_and_fills_whole_over_this_daemon() {
    let rig = rig();
    let (session, mut lane) = open(&rig, "tradehub-1");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        AdapterReply::Running { .. }
    ));
    let adapter = rig.lane.adapter("binance", None).expect("the adapter was started");
    let mut sent = order("c1");
    sent.qty = 0.1 + 0.2;
    sent.price = Some(f64::MIN_POSITIVE / 4.0);
    let mut exec = session.execution_client();
    exec.submit(&sent);
    assert!(wait_until(WAIT, || !adapter.calls().is_empty()));
    assert_eq!(adapter.calls()[0], ExecutionCall::Submit(sent.clone()));
    let fill = FillEvent {
        trade_id: TradeId::new("t1").expect("an id"),
        order_client_id: "c1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: sent.qty,
        last_price: -0.0,
        commission: 1e-300,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 9,
        mark_price: Some(f64::MAX),
        position_side: PositionSide::Both,
    };
    let event =
        Event::OrderFilled(OrderFilled { order_client_id: "c1".into(), fill, timestamp: 9 });
    assert!(adapter.emit(event.clone()));
    let seen = lane.collect_until(WAIT, 1, |i| matches!(i, Ingest::GatewayEvent(_)));
    let Some(Ingest::GatewayEvent(g)) = seen.first() else { panic!("{seen:?}") };
    assert_eq!(serde_json::to_string(&g.event).unwrap(), serde_json::to_string(&event).unwrap());
    assert_eq!(g.event, event);
}

/// A fill as a bridge reports it, in its ring frame: the commonest frame down.
fn fill_frame() -> TradeDown {
    TradeDown::Event {
        account: "binance".into(),
        gateway_epoch: 0x1234_5678_9abc_def0,
        sequence: 48_211,
        timestamp_ms: 1_763_560_000_123,
        replayed: false,
        event: Event::OrderPartiallyFilled(vike_model::events::OrderPartiallyFilled {
            order_client_id: "VKM9c2f1a7e5b3d".into(),
            fill: FillEvent {
                trade_id: TradeId::new("2210983475").expect("an id"),
                order_client_id: "VKM9c2f1a7e5b3d".into(),
                venue: "bybit".into(),
                symbol: "BTCUSDT.P".into(),
                side: -1,
                last_qty: 0.004,
                last_price: 91_234.5,
                commission: 0.072_987_6,
                commission_asset: "USDT".into(),
                liquidity_side: LiquiditySide::Maker,
                timestamp: 1_763_560_000_101,
                mark_price: Some(91_230.17),
                position_side: PositionSide::Both,
            },
            timestamp: 1_763_560_000_101,
        }),
    }
}

/// The median nanoseconds of `rounds` runs of `f`, each run `batch` calls.
fn median_ns(rounds: usize, batch: u32, mut f: impl FnMut()) -> f64 {
    let mut samples: Vec<f64> = (0..rounds)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..batch {
                f();
            }
            t.elapsed().as_nanos() as f64 / f64::from(batch)
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    samples[rounds / 2]
}

/// One `LEASE-CODEC` line per codec for `frame`: its bytes and the median encode and decode nanoseconds.
fn measure<T: serde::Serialize + serde::de::DeserializeOwned>(name: &str, frame: &T, batch: u32) {
    for codec in [BodyCodec::Json, BodyCodec::Msgpack] {
        let body = encode_frame(codec, frame).expect("encodes")[4..].to_vec();
        let encode_ns = median_ns(31, batch, || drop(black_box(encode_frame(codec, frame))));
        let decode_ns =
            median_ns(31, batch, || drop(black_box(decode_body::<T>(black_box(&body)))));
        println!(
            "LEASE-CODEC frame={name} codec={codec:?} bytes={} encode_ns={encode_ns:.0} decode_ns={decode_ns:.0}",
            body.len()
        );
    }
}

/// Print-only: bytes and the encode and decode time of a maker's `Modify` (99.9% of its intents on the CI box), a
/// `Submit`, a fill's ring frame and a 10 000-bar kline fetch page, JSON against msgpack. No ceiling (an
/// uncalibrated one is a flaky gate).
#[test]
#[ignore = "measuring: run in release on a quiet box"]
fn lease_codec_cost_of_the_commonest_frames() {
    let mut submit = order("VKM9c2f1a7e5b3d");
    submit.symbol = "BTCUSDT.P".into();
    submit.venue = "bybit".into();
    submit.qty = 0.004;
    submit.price = Some(91_234.5);
    let modify = TradeUp::Modify {
        command_sequence: 91_850,
        order: submit.clone(),
        new_qty: None,
        new_price: Some(91_235.0),
        new_trigger_price: None,
    };
    measure("Modify", &modify, 2_000);
    measure("Submit", &TradeUp::Submit { command_sequence: 1, order: submit }, 2_000);
    measure("Event(fill)", &fill_frame(), 2_000);
    let bars: Vec<vike_marketdata::Bar> = (0..10_000)
        .map(|i| {
            let close = 91_234.5 + f64::from(i % 97) * 0.5;
            vike_marketdata::Bar::ohlcv(
                1_763_560_000_000 + i64::from(i) * 60_000,
                close - 1.5,
                close + 4.0,
                close - 6.5,
                close,
                12.345 + f64::from(i % 13),
            )
        })
        .collect();
    let page = Response::FetchPage { rid: 1, page: 0, rows: FetchRows::Bars(bars) };
    measure("FetchPage(10000 bars)", &page, 3);
}
