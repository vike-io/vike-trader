//! **One codec switch per connection, on the daemon's side** (`src/server/connection.rs`'s module doc), for the
//! classes the trade lease does not cover: a request written in msgpack (a client that saw `wire_msgpack` advertised)
//! is answered in msgpack, a fetch's every frame included, and one written in JSON (an older client) in JSON, with
//! the same rows; an undecodable msgpack request is refused in words on a connection that survives; and the
//! `stream` class speaks the form its `DataOpen` came in, its ticks as the tube's binary bodies to a session that
//! asked (`stream_binary_ticks`).

use crate::harness;
use vike_gatewayhub::testkit::raw_peer;

use vike_gatewayhub_client::proto::{
    FEATURE_GATEWAY_STREAM_BINARY_TICKS, FetchRows, FetchWhat, Request, Response,
};
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_hub_protocol::frame::{
    BodyCodec, MSGPACK_BODY_MAGIC, decode_body, read_frame_raw, write_frame_as,
};
use vike_market_stream::binary_ticks::{BINARY_TICKS_MARKER, decode_stream_down};
use vike_market_stream::wire::{StreamDown, StreamUp, SubscriptionOperation};

use harness::{Harness, bars, test_keys};
use raw_peer::Raw;

fn klines(rid: u64) -> Request {
    Request::Fetch {
        rid,
        what: FetchWhat::Klines {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            start_ms: 0,
            end_ms: 1,
        },
    }
}

#[test]
fn a_fetch_written_in_each_codec_is_answered_in_it_with_the_same_bars() {
    let h = Harness::start();
    let served = bars(30_000);
    let rows = served.clone();
    h.rest.set_fetch(move |_, _, _, _| Ok(FetchRows::Bars(rows.clone())));
    for codec in [BodyCodec::Msgpack, BodyCodec::Json] {
        let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
        assert!(raw.features.iter().any(|f| f == FEATURE_WIRE_MSGPACK), "{:?}", raw.features);
        write_frame_as(&mut raw.stream, codec, &klines(9)).expect("written");
        let (mut got, mut frames) = (Vec::new(), 0);
        loop {
            let body = read_frame_raw(&mut raw.stream).expect("a frame");
            assert_eq!(BodyCodec::of(&body), codec, "every frame of the fetch");
            frames += 1;
            match decode_body::<Response>(&body).expect("decodes") {
                Response::FetchProgress(_) => {}
                Response::FetchPage { rows: FetchRows::Bars(page), .. } => got.extend(page),
                Response::FetchDone { rows, .. } => {
                    assert_eq!(rows, served.len() as u64);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(frames >= 3, "a progress, a page and the end");
        assert_eq!(
            serde_json::to_string(&got).expect("encodes"),
            serde_json::to_string(&served).expect("encodes"),
            "{codec:?}: the bars moved"
        );
        assert!(got == served, "{codec:?}");
    }
}

#[test]
fn an_undecodable_msgpack_request_is_refused_in_words_on_a_connection_that_survives() {
    let h = Harness::start();
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    // The msgpack magic, then `0xc1`, the one marker msgpack never uses.
    let mut frame = 5u32.to_be_bytes().to_vec();
    frame.extend_from_slice(&MSGPACK_BODY_MAGIC);
    frame.push(0xc1);
    raw.send_bytes(&frame);
    let body = read_frame_raw(&mut raw.stream).expect("a reply");
    assert_eq!(BodyCodec::of(&body), BodyCodec::Msgpack, "answered in the codec it came in");
    match decode_body::<Response>(&body).expect("decodes") {
        Response::Error(words) => assert!(words.contains("undecodable"), "{words}"),
        other => panic!("{other:?}"),
    }
    write_frame_as(&mut raw.stream, BodyCodec::Msgpack, &Request::Ping).expect("written");
    let body = read_frame_raw(&mut raw.stream).expect("a reply");
    assert!(matches!(decode_body::<Response>(&body), Ok(Response::Pong)));
}

/// The stream session in each form: a `DataOpen` written in msgpack (a current datahub) gets every frame down in
/// msgpack and reads its frames up in msgpack, one in JSON (an older datahub) stays JSON; and a session that sent
/// `StreamUp::BinaryTicks` gets its ticks as the tube's binary body, whatever its form, the same quote either way.
#[test]
fn a_stream_session_speaks_its_open_form_and_its_ticks_are_binary_once_asked() {
    let h = Harness::start();
    let quote = vike_marketdata::test_support::quote(7, 91_234.5, 91_235.25);
    let cases = [(BodyCodec::Json, false), (BodyCodec::Json, true), (BodyCodec::Msgpack, true)];
    for (codec, ask) in cases {
        let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
        assert!(raw.features.iter().any(|f| f == FEATURE_GATEWAY_STREAM_BINARY_TICKS));
        let open = Request::DataOpen { venue: "binance".into(), network: None };
        write_frame_as(&mut raw.stream, codec, &open).expect("written");
        let body = read_frame_raw(&mut raw.stream).expect("the open reply");
        assert_eq!(BodyCodec::of(&body), codec, "{codec:?}: the open reply");
        assert!(matches!(decode_body::<Response>(&body), Ok(Response::DataOpened { .. })));
        if ask {
            write_frame_as(&mut raw.stream, codec, &StreamUp::BinaryTicks).expect("written");
        }
        let op = SubscriptionOperation::Quotes { symbol: "BTCUSDT".into() };
        write_frame_as(&mut raw.stream, codec, &StreamUp::Subscribe { sid: 1, op, class: None })
            .expect("written");
        let mut acknowledged = false;
        loop {
            let body = read_frame_raw(&mut raw.stream).expect("a frame");
            let binary = body.first() == Some(&BINARY_TICKS_MARKER)
                && !body.starts_with(&MSGPACK_BODY_MAGIC);
            match decode_stream_down(&body).expect("decodes") {
                StreamDown::SubscribeOk { sid: 1, .. } => {
                    assert_eq!(
                        BodyCodec::of(&body),
                        codec,
                        "{codec:?}: a frame that is not a tick"
                    );
                    acknowledged = true;
                    let sink = h.data.session(h.data.opened() - 1).sink.clone();
                    sink.quote("binance", "BTCUSDT", quote.clone());
                }
                StreamDown::Quote { quote: got, .. } => {
                    assert!(acknowledged);
                    assert_eq!(binary, ask, "{codec:?} ask={ask}");
                    if !ask {
                        assert_eq!(
                            BodyCodec::of(&body),
                            codec,
                            "{codec:?}: a tick not asked binary"
                        );
                    }
                    assert_eq!(got, quote);
                    break;
                }
                StreamDown::Heartbeat => {}
                other => panic!("{other:?}"),
            }
        }
    }
}
