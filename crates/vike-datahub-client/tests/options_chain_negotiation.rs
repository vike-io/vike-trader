//! The CAPABILITY NEGOTIATION of the options-chain verb — [`Request::OptionsChain`], negotiated by
//! [`FEATURE_OPTIONS_CHAIN`]. The model is `crates/vike-datahub-client/tests/history_channels_negotiation.rs`,
//! and the legs:
//!
//! - **the advertisement is a BUILD fact** — a server with NO options lane advertises it and
//!   answers `NotArmed`, a success; it is never withheld for want of a lane;
//! - **new client, server without the capability** — refused CLIENT-SIDE by name, nothing sent, no
//!   fallback;
//! - **a malformed request** — refused CLIENT-SIDE with the server's own validator, nothing sent;
//! - **new client that skips the check, server OLDER than the verb** — the real frame is answered
//!   `Response::Error` and the connection SURVIVES;
//! - **a desynced peer** — an answer of the wrong variant is an `Err` naming both.
//!
//! Loopback + in-memory only.

// `common::spawn_server` is the real `serve` — NO options lane — over the store it is handed.
mod common;
#[path = "support/fake_peer.rs"]
mod fake_peer;

use std::assert_matches;
use std::io::Write as _;
use std::net::TcpStream;
use std::sync::Arc;

use vike_data::MemHistStore;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::options::OptionsChainOutcome;
use vike_datahub_client::proto::{
    FEATURE_OPTIONS_CHAIN, Request, Response, read_frame, write_frame,
};

use common::spawn_server;
use fake_peer::spawn_fake_peer;

/// Leg 1: a server with no options lane advertises the verb and ANSWERS it — `NotArmed`, a
/// SUCCESS whose sentence names the rebuild — so an unarmed server is never mistaken for an old one.
#[test]
fn a_laneless_server_advertises_the_verb_and_answers_not_armed() {
    let mut client =
        DatahubClient::connect(spawn_server(Arc::new(MemHistStore::new()))).expect("handshake");
    assert!(client.supports_options_chain(), "a build fact: {:?}", client.features());
    let reply = client.options_chain("deribit", "BTC", &[]).expect("answered, not an error");
    assert_eq!(reply.outcome, OptionsChainOutcome::NotArmed);
    assert!(reply.served_underlyings.is_empty(), "an unarmed server serves nothing");
    assert_eq!((reply.venue.as_str(), reply.underlying.as_str()), ("deribit", "BTC"));
    assert!(reply.describe().contains("options-serve"), "{}", reply.describe());
    // ...and the connection is still positional: the next verb works.
    let again = client.options_chain("deribit", "ETH", &["2026-06-26".to_string()]).expect("again");
    assert_eq!(again.underlying, "ETH");
}

/// Leg 2: a server that does not advertise the capability is refused BEFORE a frame is written,
/// naming the capability — the owner's ruling: an old server is refused by name, no fallback.
#[test]
fn a_server_without_the_capability_is_refused_client_side_without_sending() {
    let (addr, rx, handle) =
        spawn_fake_peer(vec!["load_bars".to_string(), "venue_catalog".to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    assert!(!client.supports_options_chain());
    let err = client.options_chain("deribit", "BTC", &[]).expect_err("refused client-side");
    assert!(err.contains(FEATURE_OPTIONS_CHAIN), "names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");
}

/// Leg 3: a malformed request is refused locally by the validator the server's door runs, so a
/// server that DOES advertise the verb still receives nothing.
#[test]
fn a_malformed_request_is_refused_client_side_without_sending() {
    let (addr, rx, handle) = spawn_fake_peer(vec![FEATURE_OPTIONS_CHAIN.to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.supports_options_chain());
    for (venue, underlying, expiries) in [
        ("Deribit", "BTC", vec![]),
        ("deribit", "btc", vec![]),
        ("deribit", "BTC", vec!["26JUN26".to_string()]),
    ] {
        let err = client.options_chain(venue, underlying, &expiries).expect_err("malformed");
        assert!(err.contains("options"), "{err}");
    }
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");
}

/// Leg 4: what a server built before the verb sees is this frame with a tag its decoder does not
/// know, and it answers `Response::Error` over a connection that SURVIVES.
#[test]
fn an_older_server_answers_the_frame_with_an_error_and_keeps_the_connection() {
    let mut stream =
        TcpStream::connect(spawn_server(Arc::new(MemHistStore::new()))).expect("connect");
    let body = serde_json::to_vec(&serde_json::json!({
        "OptionsChainFromANewerClient": {"venue": "deribit", "underlying": "BTC", "expiries": []}
    }))
    .expect("encode");
    let len = u32::try_from(body.len()).expect("fits u32");
    stream.write_all(&len.to_be_bytes()).expect("length prefix");
    stream.write_all(&body).expect("body");
    stream.flush().expect("flush");
    match read_frame::<_, Response>(&mut stream).expect("the server answers, never hangs") {
        Response::Error(msg) => assert!(!msg.is_empty(), "the decode failure has a message"),
        other => panic!("an unknown variant must get Response::Error, got {other:?}"),
    }
    write_frame(&mut stream, &Request::Ping).expect("send Ping");
    assert_matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong));
}

/// Leg 5: a peer that answers the verb with the WRONG variant is a desync, reported by name — never
/// read as an empty chain.
#[test]
fn a_reply_of_the_wrong_variant_is_a_named_desync() {
    let (addr, rx, handle) = spawn_fake_peer(vec![FEATURE_OPTIONS_CHAIN.to_string()], |r| {
        matches!(r, Request::OptionsChain { .. }).then_some(Response::Pong)
    });
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let err = client.options_chain("deribit", "BTC", &[]).expect_err("desync");
    assert!(err.contains("protocol desync: expected OptionsChain, got Pong"), "{err}");
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert_eq!(received.len(), 1, "exactly the one request was sent: {received:?}");
    assert_matches!(&received[0], Request::OptionsChain { venue, .. } if venue == "deribit");
    handle.join().expect("fake joins");
}
