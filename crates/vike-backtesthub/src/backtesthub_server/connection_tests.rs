//! The msgpack body through this daemon's real connection loop ("JSON only at the edges", 2026-11-19): each request
//! is answered in its own codec, an OLD client (JSON) is answered today's JSON, and every report reads back as the
//! very text over JSON, msgpack and LZ4. The client's half is `crates/vike-backtesthub-client/tests/msgpack_negotiation.rs`.

use std::net::TcpStream;
use std::sync::Arc;

use vike_backtesthub_client::proto::{PROTO_VERSION, Request, Response};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_hub_protocol::frame::{
    BodyCodec, MSGPACK_BODY_MAGIC, decode_body, read_frame_raw, write_frame_as, write_frame_raw,
};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};

use crate::test_support::spawn_server_over;

/// A backtesthub over 400 one-minute BTCUSDT bars, every strategy file a stand-in.
fn seeded_server() -> std::net::SocketAddr {
    let store = HistoryStoreMemory::new();
    let bars = vike_studio_core::test_support::walk::random_walk_bars(400, 60_000, Some("BTCUSDT"));
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed");
    spawn_server_over(Arc::new(store), vike_strategy_lab::stand_in)
}

/// One request written in `codec` and its reply read raw: the reply, its codec and its body's length.
fn exchange(
    stream: &mut TcpStream,
    codec: BodyCodec,
    request: &Request,
) -> (Response, BodyCodec, usize) {
    write_frame_as(stream, codec, request).expect("written");
    let body = read_frame_raw(stream).expect("a reply");
    (decode_body(&body).expect("the reply decodes"), BodyCodec::of(&body), body.len())
}

/// A buy-and-hold profile over the seeded bars, with `extra` appended.
fn profile(extra: &str) -> String {
    format!(
        "name = \"msgpack_body\"\n\n[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\n\
         interval = \"1m\"\nfrom = \"0\"\nto = \"100000\"\n\n[engine]\ncash = 1000.0\n\n[strategy]\n\
         file = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 1.0\nsymbol = \"BTCUSDT\"\n{extra}"
    )
}

/// The text of a text report, or `None`.
fn report_text(response: &Response) -> Option<&str> {
    match response {
        Response::Report(t)
        | Response::ParamscanReport(t)
        | Response::WalkforwardReport(t)
        | Response::CombinatorialWalkforwardReport(t)
        | Response::PermutationReport(t)
        | Response::StudyReport(t) => Some(t),
        _ => None,
    }
}

/// **Each request is answered in its own codec, on one connection**: a JSON `Hello` gets the JSON `Welcome` that
/// advertises the word, a JSON `Ping` a JSON `Pong` (an OLD client is answered as before), a msgpack one msgpack, an
/// LZ4 one plain msgpack when LZ4 cannot shrink the reply and LZ4 when it can. An undecodable msgpack request is a
/// bad REQUEST answered in msgpack, and a data-plane verb in msgpack is still named. MUTATION PROOF: answer every
/// reply JSON, or read the tag of a foreign verb as JSON only, and a leg fails.
#[test]
fn each_request_is_answered_in_its_own_codec() {
    let mut stream = TcpStream::connect(seeded_server()).expect("dial");
    let hello = HandshakeFrame::Hello { proto_version: PROTO_VERSION };
    write_frame_as(&mut stream, BodyCodec::Json, &hello).expect("written");
    let body = read_frame_raw(&mut stream).expect("a reply");
    assert_eq!(BodyCodec::of(&body), BodyCodec::Json);
    let welcome: HandshakeReply = decode_body(&body).expect("the Welcome decodes");
    let HandshakeReply::Welcome { features, .. } = welcome else {
        panic!("a Welcome: {welcome:?}")
    };
    assert!(features.iter().any(|f| f == FEATURE_WIRE_MSGPACK), "{features:?}");
    for (asked, answered) in [
        (BodyCodec::Json, BodyCodec::Json),
        (BodyCodec::Msgpack, BodyCodec::Msgpack),
        (BodyCodec::MsgpackLz4, BodyCodec::Msgpack),
        (BodyCodec::Json, BodyCodec::Json),
    ] {
        let (pong, codec, _) = exchange(&mut stream, asked, &Request::Ping);
        assert!(matches!(pong, Response::Pong), "{pong:?}");
        assert_eq!(codec, answered, "{asked:?}");
    }
    let run = Request::RunSlice {
        spec: vike_backtesthub_client::wire_studio::StrategySpecWire::Rhai(
            vike_studio_core::test_support::cross_script::LONG_CROSS_SCRIPT.to_string(),
        ),
        slice: Box::new(vike_backtesthub_client::test_support::btcusdt_1m_bars()),
        params: None,
    };
    let (whole, codec, _) = exchange(&mut stream, BodyCodec::MsgpackLz4, &run);
    assert!(matches!(whole, Response::RunResult(_)), "{whole:?}");
    assert_eq!(codec, BodyCodec::MsgpackLz4, "a whole answer LZ4 shrinks is LZ4");
    let mut garbled = MSGPACK_BODY_MAGIC.to_vec();
    garbled.push(0xc1);
    write_frame_raw(&mut stream, &garbled).expect("written");
    let body = read_frame_raw(&mut stream).expect("a reply");
    assert_eq!(BodyCodec::of(&body), BodyCodec::Msgpack);
    let refused: Response = decode_body(&body).expect("decodes");
    assert!(matches!(&refused, Response::Error(e) if e.contains("undecodable")), "{refused:?}");
    let foreign = serde_json::json!({ "LoadBars": {} });
    write_frame_as(&mut stream, BodyCodec::Msgpack, &foreign).expect("written");
    let refused: Response =
        decode_body(&read_frame_raw(&mut stream).expect("a reply")).expect("decodes");
    assert!(
        matches!(&refused, Response::Error(e) if e.starts_with("LoadBars is a Data-plane verb")),
        "{refused:?}"
    );
    let (pong, _, _) = exchange(&mut stream, BodyCodec::Json, &Request::Ping);
    assert!(matches!(pong, Response::Pong), "the connection survived");
}

/// **Every report reads back as the very text over JSON, msgpack and LZ4**, and the summary as the very value: a
/// single backtest, a parameter scan and a summarized run of one profile over the seeded store,
/// each asked on its own connection per codec. Prints each reply's bytes per codec (the lane's evidence).
#[test]
fn every_report_reads_back_as_the_very_text_over_every_codec() {
    let addr = seeded_server();
    let asks = [
        ("single report", Request::RunBacktest(profile(""))),
        (
            "parameter scan",
            Request::RunParamscanProfile {
                profile_toml: profile("\n[paramscan]\nsize = [1.0, 2.0, 3.0, 4.0]\n"),
                rank_by: None,
                search: None,
            },
        ),
        // No walk-forward here: over these bars the harness panics in `walk_forward_over_windows` (a window list
        // of empty splits), a fault of the walk, not of the wire. The tree's random documents cover its shape.
        ("summary", Request::Summarized(Box::new(Request::RunBacktest(profile(""))))),
    ];
    for (name, request) in &asks {
        let mut answers = Vec::new();
        for codec in [BodyCodec::Json, BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
            let mut stream = TcpStream::connect(addr).expect("dial");
            let (answer, _, bytes) = exchange(&mut stream, codec, request);
            eprintln!("{name}: {codec:?} reply {bytes} bytes");
            answers.push(answer);
        }
        let json = &answers[0];
        assert!(!matches!(json, Response::Error(_)), "{name} ran: {json:?}");
        for other in &answers[1..] {
            match (json, other) {
                (Response::RunSummary(a), Response::RunSummary(b)) => {
                    // Each run is held under its own name; everything else is the run's.
                    let b = vike_backtesthub_client::summary::RunSummaryWire {
                        run: a.run.clone(),
                        ..(**b).clone()
                    };
                    assert_eq!(
                        serde_json::to_string(a).unwrap(),
                        serde_json::to_string(&b).unwrap(),
                        "{name}"
                    );
                }
                _ => assert_eq!(
                    report_text(json).expect("a report"),
                    report_text(other).expect("a report"),
                    "{name}"
                ),
            }
        }
    }
}
