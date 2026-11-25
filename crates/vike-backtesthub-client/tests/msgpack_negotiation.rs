//! The msgpack body's negotiation (`FEATURE_WIRE_MSGPACK`, "JSON only at the edges", 2026-11-19), driven against the
//! fake peer, which records the codec each request came in and answers in it: a client writes msgpack only
//! toward a backtesthub that advertises it, an older one is spoken JSON, a thin-link client asks LZ4, and a report
//! read back over any of them is the very text the peer held.
//!
//! Loopback only, no store, no server crate. The server's half (an old client against this daemon is answered
//! JSON) is `crates/vike-backtesthub/src/backtesthub_server/connection_tests.rs`.

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::test_support::spawn_fake_peer_with_codecs;
use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_hub_protocol::frame::BodyCodec;

/// A report long enough that LZ4 shrinks it, every byte `serde_json`'s own.
fn report() -> String {
    let rows: Vec<serde_json::Value> = (0..300)
        .map(|i| serde_json::json!({ "point": i, "sharpe": f64::from(i) / 7.0, "note": "no trade closed" }))
        .collect();
    serde_json::to_string(&serde_json::json!({ "rank_by": "sharpe", "rows": rows }))
        .expect("encodes")
}

/// Dial a fake backtesthub advertising `features`, ask the client for `wanted`, send a `Ping` and a `RunBacktest`,
/// and answer what the client chose, the codecs the peer saw, and the report text read back.
fn exchange(features: &[&str], wanted: Option<BodyCodec>) -> (BodyCodec, Vec<BodyCodec>, String) {
    let held = report();
    let features = features.iter().map(|f| f.to_string()).collect();
    let (addr, rx, handle) = spawn_fake_peer_with_codecs(features, move |request| match request {
        Request::RunBacktest(_) => Some(Response::Report(held.clone())),
        _ => None,
    });
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let chosen = match wanted {
        Some(wanted) => client.prefer_body(wanted),
        None => client.body_codec(),
    };
    client.ping().expect("pong");
    let text = client.run_backtest("name = \"x\"").expect("a report");
    drop(client);
    let codecs = rx.recv().expect("what arrived");
    handle.join().expect("fake server thread joins cleanly");
    (chosen, codecs, text)
}

/// A backtesthub that predates the msgpack body is spoken JSON, whatever the client prefers: a NEW client against
/// an OLD server falls back. MUTATION PROOF: set the codec without the feature check and the peer sees msgpack.
#[test]
fn a_backtesthub_without_the_word_is_spoken_json() {
    for wanted in [None, Some(BodyCodec::Msgpack), Some(BodyCodec::MsgpackLz4)] {
        let (chosen, codecs, text) = exchange(&["backtest"], wanted);
        assert_eq!(chosen, BodyCodec::Json, "{wanted:?}");
        assert_eq!(codecs, [BodyCodec::Json, BodyCodec::Json], "{wanted:?}");
        assert_eq!(text, report());
    }
}

/// A backtesthub that advertises it is spoken msgpack by default, LZ4 when asked, JSON when asked, and the report
/// reads back as the very text over each.
#[test]
fn a_backtesthub_with_the_word_is_spoken_what_the_client_chose() {
    let served = ["backtest", FEATURE_WIRE_MSGPACK];
    for (wanted, expected) in [
        (None, BodyCodec::Msgpack),
        (Some(BodyCodec::MsgpackLz4), BodyCodec::MsgpackLz4),
        (Some(BodyCodec::Json), BodyCodec::Json),
    ] {
        let (chosen, codecs, text) = exchange(&served, wanted);
        assert_eq!(chosen, expected, "{wanted:?}");
        assert_eq!(codecs, [expected, expected], "every request in the chosen codec");
        assert_eq!(text, report(), "{wanted:?}: the very text");
    }
}
