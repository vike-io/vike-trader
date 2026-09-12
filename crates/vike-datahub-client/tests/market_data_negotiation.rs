//! §4.1's three legs of the MARKET-DATA capability contract, driven against the REAL server over a
//! loopback socket — the sibling of `backfill_negotiation.rs` and `coverage_negotiation.rs`, which
//! are the same shape for the two verbs that came before it.
//!
//! Feature-free, so it runs in the derived ROSTER lane on every PR. That is deliberate: this file
//! and `md_lane_caps.rs` are the two highest-stakes market-data gates, and placing them here means
//! they fire even if the new `live-feeds` suite arm is ever mis-spelled.

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::market::{MdLane, MdSpec};
use vike_datahub_client::proto::{FEATURE_MARKET_DATA, advertised_md_venues, md_venue_feature};

/// The PURE half of the pair: the builder and the reader must round-trip, or the server and the
/// client drift on the spelling of a string neither of them owns.
///
/// Mirrors `crates/vike-tradehub-client/src/proto.rs`'s own test for `datahub_feature` /
/// `advertised_datahub`, whose doc says why the pair ships together: it *"is the round-trip
/// authority (pinned by test), so the server and the client cannot drift on the spelling."*
#[test]
fn the_md_venue_feature_round_trips() {
    let features =
        vec![md_venue_feature("binance"), md_venue_feature("okx"), FEATURE_MARKET_DATA.to_string()];
    assert_eq!(advertised_md_venues(&features), vec!["binance".to_string(), "okx".to_string()]);
    assert_eq!(features[0], "md_venue=binance");

    // ⚠ COLLISION SAFETY, asserted rather than assumed: the named capability must not be mistaken
    // for a venue, and a venue entry must not satisfy a whole-string capability check.
    assert!(!advertised_md_venues(&features).contains(&FEATURE_MARKET_DATA.to_string()));
    assert!(!features.iter().any(|f| f == "md_venue=binance" && f == FEATURE_MARKET_DATA));

    // An EMPTY value advertises NOTHING — the same "absence is the answer" rule the rest of the
    // protocol uses, so a server that pushed a blank row cannot make a client believe in a venue
    // called "".
    assert!(advertised_md_venues(&["md_venue=".to_string()]).is_empty());
    assert!(advertised_md_venues(&["md_venue=   ".to_string()]).is_empty());
    // ...and a value IS trimmed, so a stray space in a configured slug is not a different venue.
    assert_eq!(advertised_md_venues(&["md_venue= bybit ".to_string()]), vec!["bybit".to_string()]);
    // Nothing at all is nothing at all.
    assert!(advertised_md_venues(&[]).is_empty());
    assert!(advertised_md_venues(&["backfill".to_string(), "coverage".to_string()]).is_empty());
}

fn spawn_serverless_datahub() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });
    addr
}

/// **LEG 1 — a server that cannot serve does not advertise.** `serve` mounts no hub, so
/// `market_data` and every `md_venue=` entry are absent from its `Welcome`.
///
/// ⚠ This is also the DEFAULT-BUILD shape: the plane is compiled in (the md tree is feature-free)
/// and armed by nobody, and the two must be distinguishable from the outside or "compiled with the
/// feature" and "will actually serve" become one word.
#[test]
fn a_server_with_no_hub_advertises_no_market_data() {
    let addr = spawn_serverless_datahub();
    let client = DatahubClient::connect(addr).expect("connect");
    let features = client.features().to_vec();
    assert!(
        !features.iter().any(|f| f == FEATURE_MARKET_DATA),
        "an unarmed server must not advertise the plane: {features:?}"
    );
    assert!(advertised_md_venues(&features).is_empty(), "{features:?}");
    // The guard against a vacuous assertion: this handshake DID happen and DID carry features.
    assert!(features.iter().any(|f| f == "load_bars"), "{features:?}");
}

/// **LEG 2 — the CLIENT refuses LOCALLY, without sending**, when the advertisement is absent. The
/// `coverage_report` shape, and the reason a desktop can render an honest note instead of stalling
/// on a socket.
///
/// ⚠ **And the connection survives the refusal INTACT**, which on this verb is more than politeness:
/// `md_subscribe` CONSUMES the client on success because the socket becomes a push stream, so a
/// caller needs the failure path to hand it back — otherwise a clean capability refusal would cost a
/// reconnect. That is what the `Result<_, (Self, String)>` shape buys, and it is asserted here by
/// using the returned client for an ordinary verb afterwards.
#[test]
fn the_client_refuses_locally_and_keeps_the_connection() {
    let addr = spawn_serverless_datahub();
    let client = DatahubClient::connect(addr).expect("connect");
    let specs = vec![MdSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        lane: MdLane::Depth,
        depth_levels: None,
    }];
    let (mut client, msg) = match client.md_subscribe(specs) {
        Ok(_) => panic!("an unarmed server must not open a stream"),
        Err(pair) => pair,
    };
    assert!(msg.contains(FEATURE_MARKET_DATA), "the refusal names the capability: {msg}");
    assert!(msg.contains("nothing was sent"), "...and says it sent nothing: {msg}");
    assert!(msg.contains("live-feeds"), "...and names the rebuild: {msg}");

    // The connection is INTACT and still positional.
    let series = client.list_series().expect("the connection survives a local refusal");
    assert!(series.is_empty(), "an empty MemHistStore: {series:?}");
}

/// **LEG 3 — a server that predates (or does not serve) the verb answers a clean `Response::Error`
/// and KEEPS the connection**, for a client that skips the check.
///
/// This is the leg that makes the whole thing degrade legibly rather than desyncing, and it is the
/// one that matters most on THIS verb: the refusal is what tells a raw client its connection is
/// still POSITIONAL and it must not start a reader thread. Driven at the frame level, because the
/// typed client method refuses before it would reach the wire.
#[test]
fn a_raw_md_subscribe_against_no_hub_is_refused_and_the_socket_stays_positional() {
    use vike_datahub_client::proto::{PROTO_VERSION, Request, Response, read_frame, write_frame};
    let addr = spawn_serverless_datahub();
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).unwrap();
    let _welcome = read_frame::<_, Response>(&mut s).unwrap();

    write_frame(&mut s, &Request::MdSubscribe { specs: Vec::new() }).unwrap();
    match read_frame::<_, Response>(&mut s).expect("a refusal, not a dropped socket") {
        Response::Error(msg) => {
            assert!(msg.contains("live-feeds"), "the refusal names the rebuild: {msg}");
            assert!(msg.contains("VIKE_DATAHUB_LIVE"), "...and the arm: {msg}");
        }
        other => panic!("expected Response::Error, got {other:?}"),
    }
    // ⚠ THE POINT OF THE TEST: no mode switch happened, so an ordinary verb still answers on this
    // very socket. A hub-less build that answered an all-refused `MdSubscribed` instead would have
    // turned this into a heartbeat-only writer that never sends a frame.
    write_frame(&mut s, &Request::Ping).unwrap();
    assert!(matches!(read_frame::<_, Response>(&mut s).unwrap(), Response::Pong));
}

/// **The SUCCESS path of the consuming method** — the one piece of §4 surface with no other
/// consumer in this change, because §9's `MdSession` is a separate PR.
///
/// It proves the three things `md_subscribe`'s signature exists to guarantee, and each is a thing a
/// caller would otherwise have to remember:
///
/// 1. the client is CONSUMED and the raw `TcpStream` handed back — the socket is a push stream and
///    no positional verb may be sent on it again;
/// 2. the returned stream already has `max(MD_READ_TIMEOUT, 3 × heartbeat_ms)` armed as its read
///    deadline, so nobody can forget the deadline the heartbeat exists to serve;
/// 3. `accepted` echoes the SERVER's authoritative spec, depth clamped — a client that asked for
///    more levels than the ceiling LEARNS the number it got.
#[test]
fn md_subscribe_consumes_the_client_and_arms_the_stream() {
    use std::time::Duration;
    use vike_datahub::md::MdHub;
    use vike_datahub_client::market::{MD_DEPTH_LEVELS_CEILING, MD_READ_TIMEOUT};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    // A hub that MOUNTS but opens no venue: the connection contract is what this test is about, and
    // `crates/vike-datahub/tests/md_hub.rs` is where the hub's own behaviour is driven.
    let hub = MdHub::new(
        Box::new(|venue: &str, _sink: Arc<dyn vike_data::live::LiveDataSink>| {
            Err(format!("no venue client in this test build: {venue}"))
        }),
        vec!["binance".to_string()],
    );
    thread::spawn(move || {
        let _ = vike_datahub::serve_authed(listener, store, None, None, Some(hub));
    });

    let client = DatahubClient::connect(addr).expect("connect");
    assert!(client.features().iter().any(|f| f == FEATURE_MARKET_DATA));
    let (info, stream) = client
        .md_subscribe(vec![MdSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            lane: MdLane::Depth,
            // Above the ceiling on purpose: the echo below is what makes a clamp an ACCEPTANCE
            // rather than a silent narrowing.
            depth_levels: Some(9_999),
        }])
        .unwrap_or_else(|(_, e)| panic!("md_subscribe against a mounted hub: {e}"));

    assert_eq!(info.accepted.len(), 1, "{:?}", info.accepted);
    assert_eq!(
        info.accepted[0].depth_levels,
        Some(MD_DEPTH_LEVELS_CEILING),
        "the ACCEPTED spec is the server's, clamped — and the client learns the number"
    );
    assert!(info.refused.is_empty(), "{:?}", info.refused);
    assert!(info.heartbeat_ms > 0, "the server sends its own period");

    let armed = stream.read_timeout().expect("read the armed timeout").expect("...and it IS armed");
    let want = MD_READ_TIMEOUT.max(Duration::from_millis(info.heartbeat_ms * 3));
    assert_eq!(armed, want, "the deadline is armed BY the method, not by its caller");

    // ...and the socket is a PUSH STREAM: the attach frames are already on their way, so the very
    // next frame decodes as `Response::Md`.
    let mut stream = stream;
    match vike_datahub_client::proto::read_frame::<_, vike_datahub_client::proto::Response>(
        &mut stream,
    )
    .expect("a subscribed socket keeps writing")
    {
        vike_datahub_client::proto::Response::Md(_) => {}
        other => panic!("a subscribed socket carries only Response::Md, got {other:?}"),
    }
}

/// A datahub with a hub MOUNTED but no venue client behind it — the connection contract is what
/// the two tests below are about, and `crates/vike-datahub/tests/md_hub.rs` is where the hub's own
/// behaviour is driven.
fn spawn_with_mounted_hub() -> std::net::SocketAddr {
    use vike_datahub::md::MdHub;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let hub = MdHub::new(
        Box::new(|venue: &str, _sink: Arc<dyn vike_data::live::LiveDataSink>| {
            Err(format!("no venue client in this test build: {venue}"))
        }),
        vec!["binance".to_string()],
    );
    thread::spawn(move || {
        let _ = vike_datahub::serve_authed(listener, store, None, None, Some(hub));
    });
    addr
}

/// **THE SERVER's gate, over the wire** — built as a RAW frame rather than through
/// `DatahubClient::md_subscribe`, precisely so the client's own pre-check is bypassed and what this
/// proves is the SERVER refusing rather than the client declining to ask.
///
/// Two legs, and the second is the non-vacuity floor: the over-length spec comes back in
/// `MdSubscribed.refused` with its typed reason, and the GOOD spec sent alongside it is still
/// ACCEPTED and still opens the stream. A per-spec refusal must not become a whole-request one —
/// `vike_datahub_client::market::MdRefusal`'s own doc carries that invariant.
#[test]
fn an_over_length_symbol_is_refused_by_the_server_over_the_wire() {
    use vike_datahub_client::market::{MD_MAX_SYMBOL_BYTES, MdRefusal};
    use vike_datahub_client::proto::{PROTO_VERSION, Request, Response, read_frame, write_frame};

    let addr = spawn_with_mounted_hub();
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).unwrap();
    let _welcome = read_frame::<_, Response>(&mut s).unwrap();

    let good = MdSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        lane: MdLane::Depth,
        depth_levels: None,
    };
    let bad = MdSpec { symbol: "A".repeat(MD_MAX_SYMBOL_BYTES + 1), ..good.clone() };
    write_frame(&mut s, &Request::MdSubscribe { specs: vec![bad.clone(), good.clone()] }).unwrap();

    match read_frame::<_, Response>(&mut s).expect("a reply, not a dropped socket") {
        Response::MdSubscribed { accepted, refused, .. } => {
            assert_eq!(refused.len(), 1, "exactly the bad spec: {refused:?}");
            assert_eq!(refused[0].0.symbol, bad.symbol, "{refused:?}");
            let MdRefusal::SymbolRejected(why) = &refused[0].1 else {
                panic!("expected SymbolRejected, got {:?}", refused[0].1);
            };
            assert!(
                why.contains(&MD_MAX_SYMBOL_BYTES.to_string()),
                "the refusal names the cap: {why}"
            );
            // THE FLOOR: the request is not refused as a whole, and the good spec is served.
            assert_eq!(accepted.len(), 1, "{accepted:?}");
            assert_eq!(accepted[0].symbol, good.symbol);
        }
        other => panic!("expected MdSubscribed, got {other:?}"),
    }
}

/// **THE CLIENT's half of the same rule** — `md_subscribe` refuses a bad symbol LOCALLY, without
/// sending, in the shape `the_client_refuses_locally_and_keeps_the_connection` already uses for the
/// capability negotiation. One definition (`vike_datahub_client::market::validate_md_symbol`), two
/// ends, so the client cannot guard a rule the server does not know — nor the reverse.
///
/// And the connection SURVIVES: `md_subscribe` consumes the client on success, so a caller needs
/// the failure path to hand it back or a clean local refusal would cost a reconnect.
#[test]
fn the_client_refuses_a_bad_symbol_locally_and_keeps_the_connection() {
    use vike_datahub_client::market::MD_MAX_SYMBOL_BYTES;

    let addr = spawn_with_mounted_hub();
    let client = DatahubClient::connect(addr).expect("connect");
    assert!(
        client.features().iter().any(|f| f == FEATURE_MARKET_DATA),
        "floor: this server DOES advertise the plane, so a refusal here is about the SYMBOL and \
         not about the capability"
    );
    let specs = vec![MdSpec {
        venue: "binance".into(),
        symbol: "A".repeat(MD_MAX_SYMBOL_BYTES + 1),
        lane: MdLane::Depth,
        depth_levels: None,
    }];
    let (mut client, msg) = match client.md_subscribe(specs) {
        Ok(_) => panic!("an over-length symbol must not open a stream"),
        Err(pair) => pair,
    };
    assert!(msg.contains(&MD_MAX_SYMBOL_BYTES.to_string()), "the refusal names the cap: {msg}");
    assert!(msg.contains("nothing was sent"), "...and says it sent nothing: {msg}");

    // The connection is INTACT and still positional.
    let series = client.list_series().expect("the connection survives a local refusal");
    assert!(series.is_empty(), "an empty MemHistStore: {series:?}");
}

/// **THE WHOLE-REQUEST LENGTH BOUND, over the wire** — the sibling of the symbol gate above, and
/// the field that gate did NOT close.
///
/// `MD_MAX_SPECS_PER_SESSION` bounds the keys a session may HOLD, so `MdHub::acquire` refuses the
/// 65th and the caller keeps looping; nothing bounded how many specs a REQUEST could carry.
/// `crates/vike-datahub/src/server.rs`'s `run_market_writer` loops over the client-sized `Vec` and
/// CLONES every refusal into one reply, which `write_frame` materialises whole before comparing it
/// to `MAX_FRAME_LEN` — so the refusal path cost more than the acceptance path, and bounding the
/// SYMBOL made it worse (a typed `SymbolRejected(String)` carries a ~300-byte message where
/// `SpecCapSession` carries 38).
///
/// Sent as a RAW frame so the client's own pre-check is bypassed and what is proven is the SERVER
/// refusing. Two legs, and the second is the non-vacuity floor:
///
/// 1. one `Response::Error` naming the cap — NOT an `MdSubscribed` carrying 65 refusals;
/// 2. the connection is still POSITIONAL, so no mode switch happened and no session was opened.
#[test]
fn an_over_length_spec_list_is_refused_whole_by_the_server() {
    use vike_datahub::md::MD_MAX_SPECS_PER_SESSION;
    use vike_datahub_client::proto::{PROTO_VERSION, Request, Response, read_frame, write_frame};

    let addr = spawn_with_mounted_hub();
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).unwrap();
    let _welcome = read_frame::<_, Response>(&mut s).unwrap();

    // Every spec is individually VALID — a real venue, a served lane, a legal symbol — so the only
    // thing wrong with this request is its LENGTH. That is what makes the refusal a statement about
    // the list rather than about any member of it.
    let specs: Vec<MdSpec> = (0..=MD_MAX_SPECS_PER_SESSION)
        .map(|i| MdSpec {
            venue: "binance".into(),
            symbol: format!("BTC{i}USDT.P"),
            lane: MdLane::Depth,
            depth_levels: None,
        })
        .collect();
    assert_eq!(specs.len(), MD_MAX_SPECS_PER_SESSION as usize + 1, "exactly ONE over the cap");
    write_frame(&mut s, &Request::MdSubscribe { specs }).unwrap();

    match read_frame::<_, Response>(&mut s).expect("a reply, not a dropped socket") {
        Response::Error(why) => {
            assert!(
                why.contains(&MD_MAX_SPECS_PER_SESSION.to_string()),
                "the refusal names the cap: {why}"
            );
            assert!(why.contains("MD_MAX_SPECS_PER_SESSION"), "...by name: {why}");
            assert!(why.contains("nothing was changed"), "...and says it did nothing: {why}");
        }
        // The DEFECT's own shape, named so a failure reads as itself: 65 specs accepted into the
        // writer, the surplus refused one at a time and every refusal cloned into this reply.
        Response::MdSubscribed { accepted, refused, .. } => panic!(
            "the request was SERVED rather than refused whole: {} accepted, {} refusals cloned \
             into the reply",
            accepted.len(),
            refused.len()
        ),
        other => panic!("expected Response::Error, got {other:?}"),
    }

    // ⚠ THE FLOOR: no mode switch, so the socket is still positional and no session was opened.
    write_frame(&mut s, &Request::Ping).unwrap();
    assert!(matches!(read_frame::<_, Response>(&mut s).unwrap(), Response::Pong));
}

/// The SAME bound on the OTHER verb, and on BOTH of its lists.
///
/// `MdUpdate` carries `add` and `remove`, and `MdHub::update` loops and clones over each — so a
/// bound on `MdSubscribe` alone would leave the identical cost reachable one verb later. `remove`
/// is held to the same number for the same reason: a session holds at most that many keys, so a
/// longer removal list names keys it cannot be holding.
///
/// ⚠ The session id is a FICTION here, and deliberately: the length check must precede the session
/// lookup, so a refusal naming the cap — rather than "no such session" — is what proves it runs at
/// the door.
#[test]
fn an_over_length_update_list_is_refused_before_the_session_is_looked_up() {
    use vike_datahub::md::MD_MAX_SPECS_PER_SESSION;
    use vike_datahub_client::market::MdSessionId;
    use vike_datahub_client::proto::{PROTO_VERSION, Request, Response, read_frame, write_frame};

    let long: Vec<MdSpec> = (0..=MD_MAX_SPECS_PER_SESSION)
        .map(|i| MdSpec {
            venue: "binance".into(),
            symbol: format!("BTC{i}USDT.P"),
            lane: MdLane::Depth,
            depth_levels: None,
        })
        .collect();

    for (field, add, remove) in
        [("add", long.clone(), Vec::new()), ("remove", Vec::new(), long.clone())]
    {
        let addr = spawn_with_mounted_hub();
        let mut s = std::net::TcpStream::connect(addr).expect("connect");
        write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).unwrap();
        let _welcome = read_frame::<_, Response>(&mut s).unwrap();

        write_frame(&mut s, &Request::MdUpdate { session: MdSessionId(0xdead_beef), add, remove })
            .unwrap();

        match read_frame::<_, Response>(&mut s).expect("a reply, not a dropped socket") {
            Response::Error(why) => {
                assert!(
                    why.contains("MD_MAX_SPECS_PER_SESSION"),
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
