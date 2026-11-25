//! §4.1's three legs of the MARKET-DATA capability contract, driven against the REAL server over a
//! loopback socket — the shape of `backfill_negotiation.rs` and `coverage_negotiation.rs`.
//!
//! Feature-free on purpose: this file and `market_data_lane_caps.rs` are the two highest-stakes market-data
//! gates, so they fire even if the datahub's feature set is ever mis-spelled.

use std::assert_matches;
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub_client::market::MarketDataSpec;
use vike_datahub_client::{DatahubClient, FEATURE_MARKET_DATA};
use vike_hub_protocol::features::{advertised_market_data_venues, market_data_venue_feature};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};

// `spawn_server` is the plain `serve`, which mounts no hub.
use vike_datahub::test_support::spawn_server;
use vike_model::StreamLabel;

/// The PURE half of the pair: the builder and the reader must round-trip, or the server and the
/// client drift on the spelling of a string neither of them owns — the shape of the
/// `datahub_feature` / `advertised_datahub` test in `crates/vike-tradehub-client/src/proto.rs`.
#[test]
fn the_market_data_venue_feature_round_trips() {
    let features = vec![
        market_data_venue_feature("binance"),
        market_data_venue_feature("okx"),
        FEATURE_MARKET_DATA.to_string(),
    ];
    assert_eq!(
        advertised_market_data_venues(&features),
        vec!["binance".to_string(), "okx".to_string()]
    );
    assert_eq!(features[0], "market_data_venue=binance");

    // ⚠ COLLISION SAFETY, asserted rather than assumed: the named capability must not be mistaken
    // for a venue, and a venue entry must not satisfy a whole-string capability check.
    assert!(!advertised_market_data_venues(&features).contains(&FEATURE_MARKET_DATA.to_string()));
    assert!(!features.iter().any(|f| f == "market_data_venue=binance" && f == FEATURE_MARKET_DATA));

    // An EMPTY value advertises NOTHING — the same "absence is the answer" rule the rest of the
    // protocol uses, so a server that pushed a blank row cannot make a client believe in a venue
    // called "".
    assert!(advertised_market_data_venues(&["market_data_venue=".to_string()]).is_empty());
    assert!(advertised_market_data_venues(&["market_data_venue=   ".to_string()]).is_empty());
    // ...and a value IS trimmed, so a stray space in a configured slug is not a different venue.
    assert_eq!(
        advertised_market_data_venues(&["market_data_venue= bybit ".to_string()]),
        vec!["bybit".to_string()]
    );
    // Nothing at all is nothing at all.
    assert!(advertised_market_data_venues(&[]).is_empty());
    assert!(
        advertised_market_data_venues(&["backfill".to_string(), "coverage".to_string()]).is_empty()
    );
}

/// **LEG 1 — a server that cannot serve does not advertise.** `serve` mounts no hub, so
/// `market_data` and every `market_data_venue=` entry are absent from its `Welcome`.
///
/// ⚠ This is also the DEFAULT-BUILD shape: the plane is compiled in (the market data tree is feature-free)
/// and armed by nobody, and the two must be distinguishable from the outside or "compiled with the
/// feature" and "will actually serve" become one word.
#[test]
fn a_server_with_no_hub_advertises_no_market_data() {
    let addr = spawn_server(Arc::new(HistoryStoreMemory::new()));
    let client = DatahubClient::connect(addr).expect("connect");
    let features = client.features().to_vec();
    assert!(
        !features.iter().any(|f| f == FEATURE_MARKET_DATA),
        "an unarmed server must not advertise the plane: {features:?}"
    );
    assert!(advertised_market_data_venues(&features).is_empty(), "{features:?}");
    // The guard against a vacuous assertion: this handshake DID happen and DID carry features.
    assert!(features.iter().any(|f| f == "load_bars"), "{features:?}");
}

/// **LEG 2 — the CLIENT refuses LOCALLY, without sending**, when the advertisement is absent, so a
/// desktop can render an honest note instead of stalling on a socket.
///
/// ⚠ **And the connection survives the refusal INTACT**: `market_data_subscribe` CONSUMES the client on
/// success because the socket becomes a push stream, so the failure path must hand it back or a
/// clean capability refusal would cost a reconnect. That is what the `Result<_, (Self, String)>`
/// shape buys, asserted by using the returned client for an ordinary verb afterwards.
#[test]
fn the_client_refuses_locally_and_keeps_the_connection() {
    let addr = spawn_server(Arc::new(HistoryStoreMemory::new()));
    let client = DatahubClient::connect(addr).expect("connect");
    let specs = vec![MarketDataSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        lane: StreamLabel::Depth,
        depth_levels: None,
    }];
    let (mut client, msg) = match client.market_data_subscribe(specs) {
        Ok(_) => panic!("an unarmed server must not open a stream"),
        Err(pair) => pair,
    };
    assert!(msg.contains(FEATURE_MARKET_DATA), "the refusal names the capability: {msg}");
    assert!(msg.contains("nothing was sent"), "...and says it sent nothing: {msg}");
    assert!(msg.contains("flags.datahub_live"), "...and names the arm: {msg}");

    // The connection is INTACT and still positional.
    let series = client.list_series().expect("the connection survives a local refusal");
    assert!(series.is_empty(), "an empty HistoryStoreMemory: {series:?}");
}

/// **LEG 3 — a server that predates (or does not serve) the verb answers a clean `Response::Error`
/// and KEEPS the connection**, for a client that skips the check.
///
/// The leg that matters most on THIS verb: the refusal is what tells a raw client its connection is
/// still POSITIONAL and it must not start a reader thread. Driven at the frame level, because the
/// typed client method refuses before it would reach the wire.
#[test]
fn a_raw_market_data_subscribe_against_no_hub_is_refused_and_the_socket_stays_positional() {
    use vike_datahub_client::PROTO_VERSION;
    use vike_datahub_client::proto::{Request, Response};
    use vike_hub_protocol::frame::{read_frame, write_frame};
    let addr = spawn_server(Arc::new(HistoryStoreMemory::new()));
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &HandshakeFrame::Hello { proto_version: PROTO_VERSION }).unwrap();
    let _welcome = read_frame::<_, HandshakeReply>(&mut s).unwrap();

    write_frame(&mut s, &Request::MarketDataSubscribe { specs: Vec::new() }).unwrap();
    match read_frame::<_, Response>(&mut s).expect("a refusal, not a dropped socket") {
        Response::Error(msg) => {
            assert!(msg.contains("flags.datahub_live"), "the refusal names the arm: {msg}");
        }
        other => panic!("expected Response::Error, got {other:?}"),
    }
    // ⚠ THE POINT OF THE TEST: no mode switch happened, so an ordinary verb still answers on this
    // very socket. A hub-less build that answered an all-refused `MarketDataSubscribed` instead would have
    // turned this into a heartbeat-only writer that never sends a frame.
    write_frame(&mut s, &Request::Ping).unwrap();
    assert_matches!(read_frame::<_, Response>(&mut s).unwrap(), Response::Pong);
}

/// **The quote lane's golden frames**, under `fixtures/` so a rename pass over `crates/` cannot reach
/// them: a request naming the lane, the reply that accepts one key and refuses another `NotHeld`, a
/// quote with no label, with a stated delay and frozen, and a status on the lane.
#[test]
fn the_quote_lane_goldens_decode_and_re_encode() {
    use vike_datahub_client::proto::{Request, Response};
    let goldens = vike_hub_protocol::test_support::Goldens::at(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    );
    let src = "crates/vike-datahub-client/src/market.rs";
    goldens.assert_round_trip::<Request>(
        "fixtures/datahub_wire/v1_market_data_quotes_requests.jsonl",
        2,
        src,
    );
    goldens.assert_round_trip::<Response>(
        "fixtures/datahub_wire/v1_market_data_quotes_responses.jsonl",
        5,
        src,
    );
}

/// **The refusal is the non-permanent one, the verdict's whole point**: a client keeps the key desired,
/// and the existing non-permanent ones are caps. A permanent answer would stop a window for good when a
/// strategy is merely not running yet.
#[test]
fn not_held_is_not_permanent() {
    assert!(!vike_datahub_client::market::MarketDataRefusal::NotHeld.is_permanent());
}

/// A datahub with a hub MOUNTED but no venue client behind it: the connection contract is what its
/// callers test, and `crates/vike-datahub/tests/default_build/market_data_hub.rs` is where the hub's own behaviour is
/// driven.
fn spawn_with_mounted_hub() -> std::net::SocketAddr {
    use vike_datahub::market_data::MarketDataHub;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let hub = MarketDataHub::new(
        Box::new(|venue: &str, _sink: Arc<dyn vike_data::DataClientSink>| {
            Err(format!("no venue client in this test build: {venue}"))
        }),
        vec!["binance".to_string()],
    );
    thread::spawn(move || {
        let _ = vike_datahub::serve_authed(listener, store, None, None, Some(hub), None, None);
    });
    addr
}

/// **A server older than the quote lane is sent no quote spec, and the verb's other keys survive that**
/// (decision 0112's leg two): it could not decode the frame, and one bad spec fails a whole request. A
/// mounted hub today advertises the word, so the fake peer stands in for the older server, and it
/// records every frame, so "nothing was sent" is `is_empty()`.
#[test]
fn a_server_without_the_quote_word_is_sent_no_quote_spec() {
    use vike_datahub_client::proto::FEATURE_MARKET_DATA_LANE_QUOTES;
    use vike_datahub_client::test_support::spawn_fake_peer;

    let spec = |lane| MarketDataSpec {
        venue: "oanda".into(),
        symbol: "EUR_USD".into(),
        lane,
        depth_levels: None,
    };

    let (addr, received, peer) = spawn_fake_peer(vec![FEATURE_MARKET_DATA.to_string()], |_| None);
    let client = DatahubClient::connect(addr).expect("connect");
    let (mut client, msg) = match client.market_data_subscribe(vec![spec(StreamLabel::Quotes)]) {
        Ok(_) => panic!("an older server must not be sent a quote spec"),
        Err(pair) => pair,
    };
    assert!(msg.contains(FEATURE_MARKET_DATA_LANE_QUOTES), "the refusal names the word: {msg}");
    assert!(msg.contains("Nothing was sent"), "{msg}");
    let update = client.market_data_update(
        vike_datahub_client::market::MarketDataSessionId(1),
        vec![spec(StreamLabel::Quotes)],
        Vec::new(),
    );
    assert!(
        update.is_err_and(|e| e.contains(FEATURE_MARKET_DATA_LANE_QUOTES)),
        "the second door too"
    );
    drop(client);
    let sent = received.recv().expect("the peer reports what it read");
    assert!(sent.is_empty(), "nothing past the handshake: {sent:?}");
    peer.join().expect("the peer ends cleanly");

    // The hub this crate's server mounts says the word, beside the plane's own.
    let addr = spawn_with_mounted_hub();
    let served = DatahubClient::connect(addr).expect("connect");
    assert!(served.features().iter().any(|f| f == FEATURE_MARKET_DATA_LANE_QUOTES));
}

/// **The SUCCESS path of the consuming method.** It proves the three things `market_data_subscribe`'s
/// signature exists to guarantee, each a thing a caller would otherwise have to remember:
///
/// 1. the client is CONSUMED and the raw `TcpStream` handed back — the socket is a push stream and
///    no positional verb may be sent on it again;
/// 2. the returned stream already has `max(MARKET_DATA_READ_TIMEOUT, 3 × heartbeat_ms)` armed as its read
///    deadline, so nobody can forget the deadline the heartbeat exists to serve;
/// 3. `accepted` echoes the SERVER's authoritative spec, depth clamped — a client that asked for
///    more levels than the ceiling LEARNS the number it got.
#[test]
fn market_data_subscribe_consumes_the_client_and_arms_the_stream() {
    use std::time::Duration;
    use vike_datahub_client::market::{MARKET_DATA_DEPTH_LEVELS_CEILING, MARKET_DATA_READ_TIMEOUT};
    use vike_datahub_client::proto::Response;
    use vike_hub_protocol::frame::read_frame;

    let addr = spawn_with_mounted_hub();
    let client = DatahubClient::connect(addr).expect("connect");
    assert!(client.features().iter().any(|f| f == FEATURE_MARKET_DATA));
    let (info, stream) = client
        .market_data_subscribe(vec![MarketDataSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            lane: StreamLabel::Depth,
            // Above the ceiling on purpose: the echo below is what makes a clamp an ACCEPTANCE
            // rather than a silent narrowing.
            depth_levels: Some(9_999),
        }])
        .unwrap_or_else(|(_, e)| panic!("market_data_subscribe against a mounted hub: {e}"));

    assert_eq!(info.accepted.len(), 1, "{:?}", info.accepted);
    assert_eq!(
        info.accepted[0].depth_levels,
        Some(MARKET_DATA_DEPTH_LEVELS_CEILING),
        "the ACCEPTED spec is the server's, clamped — and the client learns the number"
    );
    assert!(info.refused.is_empty(), "{:?}", info.refused);
    assert!(info.heartbeat_ms > 0, "the server sends its own period");

    let armed = stream.read_timeout().expect("read the armed timeout").expect("...and it IS armed");
    let want = MARKET_DATA_READ_TIMEOUT.max(Duration::from_millis(info.heartbeat_ms * 3));
    assert_eq!(armed, want, "the deadline is armed BY the method, not by its caller");

    // ...and the socket is a PUSH STREAM: the attach frames are already on their way, so the very
    // next frame decodes as `Response::MarketData`.
    let mut stream = stream;
    match read_frame::<_, Response>(&mut stream).expect("a subscribed socket keeps writing") {
        Response::MarketData(_) => {}
        other => panic!("a subscribed socket carries only Response::MarketData, got {other:?}"),
    }
}

/// **THE SERVER's gate, over the wire** — a RAW frame rather than `DatahubClient::market_data_subscribe`, so
/// the client's own pre-check is bypassed and what this proves is the SERVER refusing.
///
/// Two legs, and the second is the non-vacuity floor: the over-length spec comes back in
/// `MarketDataSubscribed.refused` with its typed reason, and the GOOD spec sent alongside it is still
/// ACCEPTED and still opens the stream. A per-spec refusal must not become a whole-request one —
/// `vike_datahub_client::market::MarketDataRefusal`'s own doc carries that invariant.
#[test]
fn an_over_length_symbol_is_refused_by_the_server_over_the_wire() {
    use vike_datahub_client::PROTO_VERSION;
    use vike_datahub_client::market::{MARKET_DATA_MAX_SYMBOL_BYTES, MarketDataRefusal};
    use vike_datahub_client::proto::{Request, Response};
    use vike_hub_protocol::frame::{read_frame, write_frame};

    let addr = spawn_with_mounted_hub();
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &HandshakeFrame::Hello { proto_version: PROTO_VERSION }).unwrap();
    let _welcome = read_frame::<_, HandshakeReply>(&mut s).unwrap();

    let good = MarketDataSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        lane: StreamLabel::Depth,
        depth_levels: None,
    };
    let bad =
        MarketDataSpec { symbol: "A".repeat(MARKET_DATA_MAX_SYMBOL_BYTES + 1), ..good.clone() };
    write_frame(&mut s, &Request::MarketDataSubscribe { specs: vec![bad.clone(), good.clone()] })
        .unwrap();

    match read_frame::<_, Response>(&mut s).expect("a reply, not a dropped socket") {
        Response::MarketDataSubscribed { accepted, refused, .. } => {
            assert_eq!(refused.len(), 1, "exactly the bad spec: {refused:?}");
            assert_eq!(refused[0].0.symbol, bad.symbol, "{refused:?}");
            let MarketDataRefusal::SymbolRejected(why) = &refused[0].1 else {
                panic!("expected SymbolRejected, got {:?}", refused[0].1);
            };
            assert!(
                why.contains(&MARKET_DATA_MAX_SYMBOL_BYTES.to_string()),
                "the refusal names the cap: {why}"
            );
            // THE FLOOR: the request is not refused as a whole, and the good spec is served.
            assert_eq!(accepted.len(), 1, "{accepted:?}");
            assert_eq!(accepted[0].symbol, good.symbol);
        }
        other => panic!("expected MarketDataSubscribed, got {other:?}"),
    }
}

/// **THE CLIENT's half of the same rule** — `market_data_subscribe` refuses a bad symbol LOCALLY, without
/// sending, in the shape of `the_client_refuses_locally_and_keeps_the_connection`, which also says
/// why the connection must SURVIVE. One definition (`vike_datahub_client::market::validate_market_data_symbol`),
/// two ends, so the client cannot guard a rule the server does not know — nor the reverse.
#[test]
fn the_client_refuses_a_bad_symbol_locally_and_keeps_the_connection() {
    use vike_datahub_client::market::MARKET_DATA_MAX_SYMBOL_BYTES;

    let addr = spawn_with_mounted_hub();
    let client = DatahubClient::connect(addr).expect("connect");
    assert!(
        client.features().iter().any(|f| f == FEATURE_MARKET_DATA),
        "floor: this server DOES advertise the plane, so a refusal here is about the SYMBOL and \
         not about the capability"
    );
    let specs = vec![MarketDataSpec {
        venue: "binance".into(),
        symbol: "A".repeat(MARKET_DATA_MAX_SYMBOL_BYTES + 1),
        lane: StreamLabel::Depth,
        depth_levels: None,
    }];
    let (mut client, msg) = match client.market_data_subscribe(specs) {
        Ok(_) => panic!("an over-length symbol must not open a stream"),
        Err(pair) => pair,
    };
    assert!(
        msg.contains(&MARKET_DATA_MAX_SYMBOL_BYTES.to_string()),
        "the refusal names the cap: {msg}"
    );
    assert!(msg.contains("nothing was sent"), "...and says it sent nothing: {msg}");

    // The connection is INTACT and still positional.
    let series = client.list_series().expect("the connection survives a local refusal");
    assert!(series.is_empty(), "an empty HistoryStoreMemory: {series:?}");
}

/// **THE WHOLE-REQUEST LENGTH BOUND, over the wire** — the field the symbol gate above did NOT
/// close.
///
/// `MARKET_DATA_MAX_SPECS_PER_SESSION` bounds the keys a session may HOLD; it does not bound how many specs
/// one REQUEST carries. `crates/vike-datahub/src/server/market_data.rs`'s `run_market_writer` CLONES
/// every refusal of a client-sized `Vec` into one reply, which `write_frame` materialises whole
/// before comparing it to `MAX_FRAME_LEN` — so without the bound, refusing costs more than
/// accepting (a `SymbolRejected(String)` carries a ~300-byte message).
///
/// A RAW frame, so what is proven is the SERVER refusing. Two legs, and the second is the
/// non-vacuity floor:
///
/// 1. one `Response::Error` naming the cap — NOT a `MarketDataSubscribed` carrying 65 refusals;
/// 2. the connection is still POSITIONAL, so no mode switch happened and no session was opened.
#[test]
fn an_over_length_spec_list_is_refused_whole_by_the_server() {
    use vike_datahub::market_data::MARKET_DATA_MAX_SPECS_PER_SESSION;
    use vike_datahub_client::PROTO_VERSION;
    use vike_datahub_client::proto::{Request, Response};
    use vike_hub_protocol::frame::{read_frame, write_frame};

    let addr = spawn_with_mounted_hub();
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &HandshakeFrame::Hello { proto_version: PROTO_VERSION }).unwrap();
    let _welcome = read_frame::<_, HandshakeReply>(&mut s).unwrap();

    // Every spec is individually VALID — a real venue, a served lane, a legal symbol — so the only
    // thing wrong with this request is its LENGTH. That is what makes the refusal a statement about
    // the list rather than about any member of it.
    let specs: Vec<MarketDataSpec> = (0..=MARKET_DATA_MAX_SPECS_PER_SESSION)
        .map(|i| MarketDataSpec {
            venue: "binance".into(),
            symbol: format!("BTC{i}USDT.P"),
            lane: StreamLabel::Depth,
            depth_levels: None,
        })
        .collect();
    assert_eq!(
        specs.len(),
        MARKET_DATA_MAX_SPECS_PER_SESSION as usize + 1,
        "exactly ONE over the cap"
    );
    write_frame(&mut s, &Request::MarketDataSubscribe { specs }).unwrap();

    match read_frame::<_, Response>(&mut s).expect("a reply, not a dropped socket") {
        Response::Error(why) => {
            assert!(
                why.contains(&MARKET_DATA_MAX_SPECS_PER_SESSION.to_string()),
                "the refusal names the cap: {why}"
            );
            assert!(why.contains("MARKET_DATA_MAX_SPECS_PER_SESSION"), "...by name: {why}");
            assert!(why.contains("nothing was changed"), "...and says it did nothing: {why}");
        }
        // The DEFECT's own shape, named so a failure reads as itself.
        Response::MarketDataSubscribed { accepted, refused, .. } => panic!(
            "the request was SERVED rather than refused whole: {} accepted, {} refusals cloned \
             into the reply",
            accepted.len(),
            refused.len()
        ),
        other => panic!("expected Response::Error, got {other:?}"),
    }

    // ⚠ THE FLOOR: no mode switch, so the socket is still positional and no session was opened.
    write_frame(&mut s, &Request::Ping).unwrap();
    assert_matches!(read_frame::<_, Response>(&mut s).unwrap(), Response::Pong);
}

/// The SAME bound on the OTHER verb, and on BOTH of its lists.
///
/// `MarketDataUpdate` carries `add` and `remove`, and `MarketDataHub::update` loops and clones over each — so a
/// bound on `MarketDataSubscribe` alone would leave the identical cost reachable one verb later. A session
/// holds at most that many keys, so a longer `remove` list names keys it cannot be holding.
///
/// ⚠ The session id is a FICTION, deliberately: the length check must precede the session lookup,
/// so a refusal naming the cap — rather than "no such session" — proves it runs at the door.
#[test]
fn an_over_length_update_list_is_refused_before_the_session_is_looked_up() {
    use vike_datahub::market_data::MARKET_DATA_MAX_SPECS_PER_SESSION;
    use vike_datahub_client::PROTO_VERSION;
    use vike_datahub_client::market::MarketDataSessionId;
    use vike_datahub_client::proto::{Request, Response};
    use vike_hub_protocol::frame::{read_frame, write_frame};

    let long: Vec<MarketDataSpec> = (0..=MARKET_DATA_MAX_SPECS_PER_SESSION)
        .map(|i| MarketDataSpec {
            venue: "binance".into(),
            symbol: format!("BTC{i}USDT.P"),
            lane: StreamLabel::Depth,
            depth_levels: None,
        })
        .collect();

    for (field, add, remove) in
        [("add", long.clone(), Vec::new()), ("remove", Vec::new(), long.clone())]
    {
        let addr = spawn_with_mounted_hub();
        let mut s = std::net::TcpStream::connect(addr).expect("connect");
        write_frame(&mut s, &HandshakeFrame::Hello { proto_version: PROTO_VERSION }).unwrap();
        let _welcome = read_frame::<_, HandshakeReply>(&mut s).unwrap();

        write_frame(
            &mut s,
            &Request::MarketDataUpdate { session: MarketDataSessionId(0xdead_beef), add, remove },
        )
        .unwrap();

        match read_frame::<_, Response>(&mut s).expect("a reply, not a dropped socket") {
            Response::Error(why) => {
                assert!(
                    why.contains("MARKET_DATA_MAX_SPECS_PER_SESSION"),
                    "`{field}` must be refused for its LENGTH, not for the fictional session — \
                     a message that names the session instead proves the check runs too late: \
                     {why}"
                );
                assert!(why.contains(field), "the refusal names the FIELD: {why}");
            }
            other => panic!("expected Response::Error for `{field}`, got {other:?}"),
        }
    }
}
