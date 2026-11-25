//! **The codec switch, negotiated both ways** (`FEATURE_WIRE_MSGPACK`, `vike_hub_protocol::frame::BodyCodec`): a client writes
//! msgpack only to a datahub that advertises it, and the datahub answers each request in the form it came in.
//!
//! - A NEW client against an OLD server (a peer that advertises nothing and reads JSON alone, as every build
//!   before the codec did) writes JSON, and its verbs work.
//! - An OLD client (JSON frames, a JSON decoder) against the NEW server is answered byte-for-byte JSON.
//! - A NEW client against the NEW server writes msgpack and is answered msgpack; asked in LZ4, answered in LZ4.
//! - The same verbs asked in JSON, msgpack and msgpack-in-LZ4 answer the same values, refusals word for word.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use vike_data::{HistoryStore, HistoryStoreMemory, SeriesId};
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{PROTO_VERSION, Request, Response};
use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_hub_protocol::frame::{
    BodyCodec, MSGPACK_BODY_MAGIC, MSGPACK_LZ4_BODY_MAGIC, read_frame_raw, write_frame,
    write_frame_as,
};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_marketdata::Bar;
use vike_model::StoreKindId;
use vike_model::{EquitySample, SymbolProperties};

/// A datahub from before the codec: it advertises no `wire_msgpack` and decodes with `serde_json` alone,
/// answering what it cannot decode with an `Error` (the decode-vs-drop rule). Every request body it reads is
/// handed back on the channel.
fn spawn_old_server() -> (SocketAddr, mpsc::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("old-datahub".into())
        .spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            let hello = read_frame_raw(&mut s).expect("Hello");
            assert!(matches!(serde_json::from_slice(&hello), Ok(HandshakeFrame::Hello { .. })));
            let welcome = HandshakeReply::Welcome {
                proto_version: PROTO_VERSION,
                features: vec!["load_bars".into()],
                nonce: None,
            };
            write_frame(&mut s, &welcome).expect("Welcome");
            while let Ok(body) = read_frame_raw(&mut s) {
                let answer = match serde_json::from_slice::<Request>(&body) {
                    Ok(Request::Ping) => Response::Pong,
                    Ok(_) => Response::Error("old server: not served".into()),
                    Err(e) => Response::Error(format!("unrecognized/undecodable request: {e}")),
                };
                let _ = tx.send(body);
                if write_frame(&mut s, &answer).is_err() {
                    break;
                }
            }
        })
        .expect("spawn");
    (addr, rx)
}

/// One bar at `ts`, its prices `base` apart and a decimal JSON shortens and msgpack does not.
fn bar(ts: i64, base: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: base,
        high: base + 0.5,
        low: base - 0.25,
        close: 0.1 + 0.2,
        volume: 1.5,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A store with a properties row, an equity sample and a bar series, so the reads below carry values.
fn seeded_store() -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    let properties = SymbolProperties {
        tick_size: 0.1,
        step_size: 0.001,
        min_notional: 5.0,
        ..Default::default()
    };
    store
        .append_symbol_properties("binance", "BTCUSDT", &[(1_000, properties)], Some("seed"))
        .expect("properties");
    let equity = EquitySample {
        timestamp: 2_000,
        venue: "binance".into(),
        equity: 10_000.25,
        realized: 0.1 + 0.2,
        unrealized: -3.5,
        missing_prices: 1,
    };
    let _ = store.append_equity("binance", "BTCUSDT", &[equity], Some("seed"));
    let bars: Vec<Bar> = (0..50).map(|i| bar(i * 60_000, 3.0)).collect();
    let _ = store.append_bars("binance", "BTCUSDT", "1m", &bars, None);
    Arc::new(store)
}

/// **A NEW client against an OLD server writes JSON**: the server advertised no `wire_msgpack`, so every frame
/// it reads is JSON its decoder takes, and the verb works.
#[test]
fn a_new_client_writes_json_to_a_server_without_the_codec() {
    let (addr, bodies) = spawn_old_server();
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert_eq!(client.body_codec(), BodyCodec::Json, "nothing advertised, nothing negotiated");
    client.ping().expect("the old server answers");
    drop(client);
    let body = bodies.recv().expect("the Ping arrived");
    assert_eq!(BodyCodec::of(&body), BodyCodec::Json);
    assert!(matches!(serde_json::from_slice(&body), Ok(Request::Ping)), "{body:?}");
}

/// What the msgpack the old server would have been sent does to it: undecodable, refused in words, the
/// connection kept. The reason the client asks only of a server that advertises the codec.
#[test]
fn an_old_server_cannot_read_msgpack_which_is_why_it_is_never_sent_one() {
    let (addr, _bodies) = spawn_old_server();
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &HandshakeFrame::Hello { proto_version: PROTO_VERSION }).expect("Hello");
    let _welcome = read_frame_raw(&mut s).expect("Welcome");
    write_frame_as(&mut s, BodyCodec::Msgpack, &Request::Ping).expect("msgpack Ping");
    let reply = read_frame_raw(&mut s).expect("an answer");
    match serde_json::from_slice::<Response>(&reply) {
        Ok(Response::Error(why)) => assert!(why.contains("undecodable"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// **An OLD client against the NEW server is answered JSON, byte for byte what it always read**: the server
/// advertises the codec, and a client that writes JSON is answered JSON its own `serde_json` decodes.
#[test]
fn an_old_client_is_answered_json_by_the_new_server() {
    let addr = spawn_server(seeded_store());
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &HandshakeFrame::Hello { proto_version: PROTO_VERSION }).expect("Hello");
    let welcome = read_frame_raw(&mut s).expect("Welcome");
    let Ok(HandshakeReply::Welcome { features, .. }) = serde_json::from_slice(&welcome) else {
        panic!("Welcome")
    };
    assert!(features.iter().any(|f| f == FEATURE_WIRE_MSGPACK), "{features:?}");
    write_frame(&mut s, &Request::Ping).expect("Ping");
    assert_eq!(read_frame_raw(&mut s).expect("Pong"), b"\"Pong\"");
    write_frame(&mut s, &Request::ListSeries).expect("ListSeries");
    let body = read_frame_raw(&mut s).expect("the list");
    assert_eq!(BodyCodec::of(&body), BodyCodec::Json);
    assert!(matches!(serde_json::from_slice(&body), Ok(Response::SeriesList(_))));
}

/// **A NEW client against the NEW server writes msgpack, and each request is answered in its own form**: msgpack
/// for msgpack, LZ4 for LZ4 (stored when LZ4 cannot shrink it), JSON for JSON, on one connection.
#[test]
fn the_new_server_answers_each_request_in_the_form_it_came_in() {
    let addr = spawn_server(seeded_store());
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert_eq!(client.body_codec(), BodyCodec::Msgpack, "advertised, so negotiated");
    client.ping().expect("Pong in msgpack");
    client.list_series().expect("the list in msgpack");

    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &HandshakeFrame::Hello { proto_version: PROTO_VERSION }).expect("Hello");
    let _welcome = read_frame_raw(&mut s).expect("Welcome");
    // An LZ4 request is answered LZ4 where that shrinks the answer: a Pong it cannot, so plain msgpack.
    for (codec, answered) in [
        (BodyCodec::Msgpack, BodyCodec::Msgpack),
        (BodyCodec::MsgpackLz4, BodyCodec::Msgpack),
        (BodyCodec::Json, BodyCodec::Json),
        (BodyCodec::Msgpack, BodyCodec::Msgpack),
    ] {
        write_frame_as(&mut s, codec, &Request::Ping).expect("Ping");
        let body = read_frame_raw(&mut s).expect("Pong");
        assert_eq!(BodyCodec::of(&body), answered, "{codec:?}: {body:?}");
    }
    write_frame_as(&mut s, BodyCodec::MsgpackLz4, &Request::Ping).expect("Ping");
    assert!(
        read_frame_raw(&mut s).expect("Pong").starts_with(&MSGPACK_BODY_MAGIC),
        "a Pong LZ4 cannot shrink"
    );
}

/// **The same verbs in each form answer the same values**, the refusals word for word: JSON, msgpack, and msgpack in
/// LZ4 (what the desktop asks) decode to one answer, as JSON compares it.
#[test]
fn json_msgpack_and_lz4_answer_the_same_values() {
    let addr = spawn_server(seeded_store());
    let series = SeriesId::per_symbol(StoreKindId::Bar, "binance", "BTCUSDT", Some("1m".into()));
    let requests = vec![
        Request::ListSeries,
        Request::Inventory,
        Request::Coverage,
        Request::SeriesFacts { id: series.clone() },
        Request::SeriesGaps { id: series },
        Request::SeriesDays {
            venue: "binance".into(),
            source: None,
            symbol: "BTCUSDT".into(),
            interval: Some("1m".into()),
            start: None,
            end: None,
            kind: None,
        },
        Request::PropertiesAsOf {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            timestamp: 5_000,
        },
        Request::ScanEquity {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
            columns: None,
        },
        Request::LoadBars {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            start: None,
            end: None,
            limit: None,
            source: None,
            columns: None,
            resample_to: None,
        },
        // A refusal: its words cross every form unchanged.
        Request::LoadBars {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "not-an-interval".into(),
            start: Some(10),
            end: Some(1),
            limit: None,
            source: Some("no such lane".into()),
            columns: None,
            resample_to: None,
        },
    ];
    let answers = |codec: BodyCodec| -> Vec<serde_json::Value> {
        let mut client = DatahubClient::connect(addr).expect("connect");
        client.set_body_codec(codec);
        requests
            .iter()
            .map(|r| {
                let answer: Response = client.exchange_frame("verb", None, r).expect("an answer");
                serde_json::to_value(answer).expect("json")
            })
            .collect()
    };
    let json = answers(BodyCodec::Json);
    // A connection that reads the refusal frame (every `DatahubClient`'s) is answered the frame.
    assert!(json.iter().any(|a| a.get("Refused").is_some()), "the refusal is among them: {json:?}");
    assert_eq!(answers(BodyCodec::Msgpack), json);
    assert_eq!(answers(BodyCodec::MsgpackLz4), json);
}

/// A large answer in LZ4 is COMPRESSED on the wire and reads back the same: the series list of a store of many
/// series, the shape the desktop's Data Manager asks.
#[test]
fn a_large_lz4_answer_is_compressed_and_reads_back_the_same() {
    let store = HistoryStoreMemory::new();
    for i in 0..300 {
        let bar = bar(60_000, 3.0);
        let _ = store.append_bars("binance", &format!("SYM{i:04}USDT"), "1m", &[bar], None);
    }
    let addr = spawn_server(Arc::new(store));
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &HandshakeFrame::Hello { proto_version: PROTO_VERSION }).expect("Hello");
    let _welcome = read_frame_raw(&mut s).expect("Welcome");
    write_frame(&mut s, &Request::ListSeries).expect("JSON");
    let json = read_frame_raw(&mut s).expect("JSON list");
    write_frame_as(&mut s, BodyCodec::MsgpackLz4, &Request::ListSeries).expect("LZ4");
    let packed = read_frame_raw(&mut s).expect("LZ4 list");
    let as_json = |body: &[u8]| -> serde_json::Value {
        let response: Response = vike_hub_protocol::frame::decode_body(body).expect("decodes");
        serde_json::to_value(response).expect("json")
    };
    assert_eq!(as_json(&packed), as_json(&json));
    if json.len() > 4_096 {
        assert!(
            packed.starts_with(&MSGPACK_LZ4_BODY_MAGIC),
            "{} JSON bytes went uncompressed",
            json.len()
        );
        assert!(packed.len() < json.len(), "{} LZ4 bytes for {} JSON", packed.len(), json.len());
    }
}
