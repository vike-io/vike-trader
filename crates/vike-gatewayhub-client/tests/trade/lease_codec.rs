#![cfg(feature = "trade")]
//! **The lease's msgpack frames: the same values as JSON, chosen per connection, and JSON with an older peer.**
//!
//! The switch is the connection's (`vike_hub_protocol::frame::BodyCodec`, `client::GatewayClient`'s module doc): a client
//! writes msgpack to a daemon that advertised `wire_msgpack`, the daemon answers in the codec the request came in, and
//! every reader reads either body.
//!
//! * every golden `TradeUp` and `TradeDown` frame (`fixtures/gatewayhub_wire/`) crosses msgpack and decodes to the
//!   value its JSON decodes to, read back as the same JSON text, so every key, every integer and every finite
//!   `f64` (shortest round trip: the text is injective) is identical; the sizes are printed;
//! * the money of an order and of a fill keeps its BITS through msgpack, the float classes that bite included;
//! * a body that is neither codec's frame is `InvalidData`, the fault the daemon answers with `TradeBye::Protocol`;
//! * the fallback both ways: a daemon that advertises `wire_msgpack` is written msgpack from the `TradeOpen` on; one that
//!   does not (an older daemon) is written JSON; under either, a `Bye(Protocol)` (what the daemon says when it
//!   cannot decode a frame) leaves no order unanswered: each written, unacknowledged submit gets its terminal reject.
//!
//! The daemon's half (it answers in the request's codec, refuses an undecodable msgpack frame by name, and an older
//! client is answered in JSON) is `crates/vike-gatewayhub/tests/trade/lease_codec.rs` and
//! `crates/vike-gatewayhub/tests/server/session_codec.rs`.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use vike_execution::{ExecutionClient, Ingest};
use vike_gatewayhub_client::proto::{TradeBye, TradeDown, TradeUp};
use vike_gatewayhub_client::test_support::fake_trade_peer;
use vike_gatewayhub_client::trade::LEASE_CLOSED_SUBMIT_REASON;
use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_hub_protocol::frame::{BodyCodec, decode_body, encode_frame};
use vike_hub_protocol::test_support::Goldens;
use vike_model::events::{Event, FillEvent, LiquiditySide, OrderFilled, PositionSide, TradeId};

use fake_trade_peer::{Seen, TradePeer, TradePeerConfig, accepted, open, order, rejects};
use vike_log::poll::wait_until;

const WAIT: Duration = Duration::from_secs(5);
const TRADE_UP: &str = "fixtures/gatewayhub_wire/v1_trade_up.jsonl";
const TRADE_DOWN: &str = "fixtures/gatewayhub_wire/v1_trade_down.jsonl";

fn goldens() -> Goldens {
    Goldens::at(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// `msg`'s frame BODY in `codec`, as a lease writes it (msgpack behind its magic).
fn body_of(codec: BodyCodec, msg: &impl serde::Serialize) -> std::io::Result<Vec<u8>> {
    Ok(encode_frame(codec, msg)?.split_off(4))
}

/// Every golden line of `rel` through msgpack: decoded, re-encoded as JSON, the same text as the JSON decode
/// re-encoded. Returns `(JSON bytes, msgpack bytes)` summed over the file.
fn crosses_msgpack<T>(rel: &str) -> (usize, usize)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let (mut json_bytes, mut msgpack_bytes) = (0, 0);
    let lines = goldens().lines(rel);
    assert!(!lines.is_empty(), "{rel} holds goldens");
    for line in lines {
        let from_json: T = decode_body(line.as_bytes()).expect("a golden decodes");
        let body = body_of(BodyCodec::Msgpack, &from_json).expect("msgpack encodes it");
        assert_eq!(BodyCodec::of(&body), BodyCodec::Msgpack, "{line}");
        let from_msgpack: T = decode_body(&body).expect("msgpack decodes it");
        assert_eq!(
            serde_json::to_string(&from_msgpack).expect("encodes"),
            serde_json::to_string(&from_json).expect("encodes"),
            "a value moved through msgpack: {line}"
        );
        json_bytes += body_of(BodyCodec::Json, &from_json).expect("encodes").len();
        msgpack_bytes += body.len();
    }
    (json_bytes, msgpack_bytes)
}

#[test]
fn every_golden_lease_frame_crosses_msgpack_with_the_values_its_json_carries() {
    let (up_json, up_msgpack) = crosses_msgpack::<TradeUp>(TRADE_UP);
    let (down_json, down_msgpack) = crosses_msgpack::<TradeDown>(TRADE_DOWN);
    println!(
        "LEASE-CODEC golden TradeUp: {up_json} B json, {up_msgpack} B msgpack; TradeDown: {down_json} B json, \
         {down_msgpack} B msgpack"
    );
    assert!(up_msgpack < up_json && down_msgpack < down_json, "msgpack is the smaller spelling");
}

/// The float classes a price, a size or a fee can take, the ones JSON text and a careless codec get wrong.
const MONEY: [f64; 9] = [
    0.1 + 0.2,
    -0.0,
    f64::MIN_POSITIVE,
    f64::MIN_POSITIVE / 4.0, // subnormal
    5e-324,
    f64::MAX,
    -123_456_789.123_456_79,
    1e-8,
    0.000_001_234_567_890_123,
];

#[test]
fn the_money_of_an_order_and_of_a_fill_keeps_its_bits_through_msgpack() {
    for (i, money) in MONEY.into_iter().enumerate() {
        let mut submit = order(&format!("m{i}"));
        submit.qty = money;
        submit.price = Some(-money);
        submit.trigger_price = Some(money / 3.0);
        let up = TradeUp::Submit { command_sequence: 1, order: submit };
        let body = body_of(BodyCodec::Msgpack, &up).expect("encodes");
        let TradeUp::Submit { order: got, .. } = decode_body(&body).expect("decodes") else {
            panic!("a submit decodes as a submit")
        };
        assert_eq!(got.qty.to_bits(), money.to_bits(), "qty {money:e}");
        assert_eq!(got.price.map(f64::to_bits), Some((-money).to_bits()), "price {money:e}");
        assert_eq!(got.trigger_price.map(f64::to_bits), Some((money / 3.0).to_bits()));

        let fill = FillEvent {
            trade_id: TradeId::new(format!("t{i}")).expect("an id"),
            order_client_id: format!("m{i}"),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            last_qty: money,
            last_price: -money,
            commission: money / 7.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Maker,
            timestamp: i64::MAX,
            mark_price: Some(money),
            position_side: PositionSide::Both,
        };
        let down = TradeDown::Event {
            account: "binance".into(),
            gateway_epoch: u64::MAX,
            sequence: 1,
            timestamp_ms: i64::MIN,
            replayed: false,
            event: Event::OrderFilled(OrderFilled {
                order_client_id: format!("m{i}"),
                fill: fill.clone(),
                timestamp: 7,
            }),
        };
        let body = body_of(BodyCodec::Msgpack, &down).expect("encodes");
        let TradeDown::Event {
            gateway_epoch, timestamp_ms, event: Event::OrderFilled(got), ..
        } = decode_body(&body).expect("decodes")
        else {
            panic!("a fill decodes as a fill")
        };
        assert_eq!((gateway_epoch, timestamp_ms), (u64::MAX, i64::MIN));
        let bits = |f: &FillEvent| {
            [f.last_qty, f.last_price, f.commission, f.mark_price.unwrap_or(f64::NAN)]
                .map(f64::to_bits)
        };
        assert_eq!(bits(&got.fill), bits(&fill), "fill money {money:e}");
        assert_eq!(got.fill, fill);
    }
}

/// msgpack carries a non-finite `f64` as its bits, where JSON writes `null`; the client still refuses such an
/// order before it is written (`ledger.rs`'s `screen_finite`), so the lease never carries one either way.
#[test]
fn a_non_finite_float_keeps_its_bits_through_msgpack_where_json_writes_null() {
    let nan = f64::from_bits(0x7ff8_0000_dead_beef);
    for value in [nan, f64::INFINITY, f64::NEG_INFINITY] {
        let body = body_of(BodyCodec::Msgpack, &value).expect("encodes");
        let back: f64 = decode_body(&body).expect("decodes");
        assert_eq!(back.to_bits(), value.to_bits());
        assert_eq!(body_of(BodyCodec::Json, &value).expect("encodes"), b"null");
    }
}

#[test]
fn a_body_that_is_neither_codecs_frame_is_invalid_data() {
    let mut truncated =
        body_of(BodyCodec::Msgpack, &TradeUp::Submit { command_sequence: 1, order: order("t") })
            .expect("encodes");
    truncated.truncate(truncated.len() / 2);
    // msgpack's magic over `0xc1` (the one marker msgpack never uses), a cut msgpack body, a JSON body naming no
    // verb, and another NUL-led magic.
    let bodies: [&[u8]; 4] = [b"\0MPK\xc1", truncated.as_slice(), br#"{"NoSuchVerb":1}"#, b"\0BAR"];
    for body in bodies {
        let err = decode_body::<TradeUp>(body).expect_err("not a frame");
        assert_eq!(err.kind(), ErrorKind::InvalidData, "{body:?}: {err}");
    }
    for codec in [BodyCodec::Json, BodyCodec::Msgpack] {
        let body = body_of(codec, &TradeUp::Ping).expect("encodes");
        assert_eq!(decode_body::<TradeUp>(&body).expect("decodes"), TradeUp::Ping, "{codec:?}");
    }
}

/// An OLDER daemon: every string but the codec's.
fn older_daemon() -> TradePeerConfig {
    let config = TradePeerConfig::new();
    let features = config.features.iter().filter(|f| *f != FEATURE_WIRE_MSGPACK).cloned().collect();
    config.with_features(features)
}

/// Whether the `TradeOpen` came in msgpack against `config`; an order and an event cross the lease.
fn lease_against(config: TradePeerConfig) -> bool {
    let peer = TradePeer::start(config);
    let (session, mut lane) = open(&peer, "tradehub-1");
    let msgpack = peer
        .seen()
        .iter()
        .find_map(|s| match s {
            Seen::Open { msgpack, .. } => Some(*msgpack),
            _ => None,
        })
        .expect("a TradeOpen");
    let mut exec = session.execution_client();
    let mut sent = order("c1");
    sent.qty = 0.1 + 0.2;
    exec.submit(&sent);
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));
    let got = peer.ups().into_iter().find_map(|f| match f {
        TradeUp::Submit { order, .. } => Some(order),
        _ => None,
    });
    assert_eq!(got, Some(sent), "the order crossed whole");
    peer.push_event(accepted("c1"));
    let seen = lane.collect_until(WAIT, 1, |i| matches!(i, Ingest::GatewayEvent(_)));
    assert!(
        matches!(seen.first(), Some(Ingest::GatewayEvent(g)) if g.event == accepted("c1")),
        "the event crossed whole: {seen:?}"
    );
    msgpack
}

#[test]
fn a_daemon_that_advertises_the_codec_is_written_msgpack_and_the_lease_runs() {
    assert!(lease_against(TradePeerConfig::new()));
}

#[test]
fn an_older_daemon_is_written_json_and_the_lease_runs() {
    assert!(!lease_against(older_daemon()));
}

/// What the daemon says when it cannot decode a frame, under either codec: the written, unacknowledged submits
/// were not read, and each gets its terminal reject; none vanishes.
fn a_protocol_bye_rejects_the_unacknowledged_submits(config: TradePeerConfig) {
    let peer = TradePeer::start(config.never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit(&order("w1"));
    exec.submit(&order("w2"));
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 2));
    let why = "gatewayhub: an undecodable trade frame: a frame it could not read";
    assert!(peer.send(&TradeDown::Bye(TradeBye::Protocol(why.into()))));
    let mut rejected = rejects(&lane.collect_until(WAIT, 2, |_| true));
    rejected.sort();
    assert_eq!(
        rejected,
        [
            ("w1".to_string(), LEASE_CLOSED_SUBMIT_REASON.to_string()),
            ("w2".to_string(), LEASE_CLOSED_SUBMIT_REASON.to_string()),
        ]
    );
    assert!(session.unknown_orders().is_empty(), "nothing is both rejected and unknown");
}

#[test]
fn a_protocol_bye_on_a_msgpack_lease_rejects_every_unacknowledged_submit() {
    a_protocol_bye_rejects_the_unacknowledged_submits(TradePeerConfig::new());
}

#[test]
fn a_protocol_bye_on_a_json_lease_rejects_every_unacknowledged_submit() {
    a_protocol_bye_rejects_the_unacknowledged_submits(older_daemon());
}
