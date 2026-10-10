//! Section 6: the MARKET-DATA mode switch, each test on a FRESH connection.

use super::*;
use std::assert_matches;

// ---- 6. the MARKET-DATA mode switch ------------------------------------------------------------
//
// ⚠ Every test here uses a FRESH connection, deliberately: `MdSubscribe` ends the server's read
// loop on the socket it arrives on, so none of these may share one with a sweep. See the comment
// beside `MdSubscribe`'s omission from `every_request`.

/// A hub over a builder that opens nothing — enough to be MOUNTED, which is what the server keys
/// its advertisement and its mode switch on. No venue is served, so every spec is refused by name;
/// what these tests exercise is the CONNECTION contract, and `crates/vike-datahub/tests/md_hub.rs`
/// is where the hub's own behaviour is driven.
fn mounted_hub() -> Arc<MdHub> {
    MdHub::new(
        Box::new(|venue: &str, _sink: Arc<dyn vike_data::LiveDataSink>| {
            Err(format!("no venue client in this test build: {venue}"))
        }),
        vec!["binance".to_string()],
    )
}

/// **A MOUNTED hub advertises the plane and its venues; the mode switch then makes `MdSubscribed`
/// the LAST positional frame that socket carries.**
///
/// The assertion that matters is the second one: after the switch, a `Ping` gets NO `Pong` — the
/// server has left the read loop — and what does arrive inside the read deadline is a pushed
/// `Response::Md`. A server that kept reading would answer the `Ping`, and every "no correlation
/// id" claim on this wire would be false.
#[test]
fn md_subscribe_is_the_last_positional_frame_on_its_socket() {
    let addr = spawn_with_md(None, Some(mounted_hub()));
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).unwrap();
    let features = match read_frame::<_, Response>(&mut s).unwrap() {
        Response::Welcome { features, .. } => features,
        other => panic!("expected Welcome, got {other:?}"),
    };
    assert!(
        features.iter().any(|f| f == vike_datahub_client::FEATURE_MARKET_DATA),
        "a MOUNTED hub advertises the plane: {features:?}"
    );
    assert_eq!(
        vike_datahub_client::advertised_md_venues(&features),
        vec!["binance".to_string()],
        "...and names the venues this build links: {features:?}"
    );

    // THE SWITCH. One spec, refused (this build links no venue client), which is deliberate: an
    // all-refused subscribe still opens the stream, because `refused` is PER-SPEC and a whole-request
    // failure is a `Response::Error` instead.
    let spec = MdSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        lane: MdLane::Depth,
        depth_levels: None,
    };
    write_frame(&mut s, &Request::MdSubscribe { specs: vec![spec] }).unwrap();
    let heartbeat_ms = match read_frame::<_, Response>(&mut s).unwrap() {
        Response::MdSubscribed { heartbeat_ms, accepted, .. } => {
            assert_eq!(accepted.len(), 1, "binance depth is servable by the caps matrix");
            heartbeat_ms
        }
        other => panic!("expected MdSubscribed, got {other:?}"),
    };
    assert!(
        heartbeat_ms > 0,
        "the server SENDS its heartbeat period so the client can deadline it"
    );

    // ⚠ THE POINT. A positional verb now gets NOTHING back positionally.
    //
    // ONE read, not a loop with a deadline: the attach already queued this key's `Status` frame, so
    // the very next frame on this socket is a pushed `Response::Md` and it is there IMMEDIATELY. A
    // server that had kept reading would have answered the `Ping` first, and `Pong` would arrive
    // here instead — which is what makes a single read the sharpest form of the assertion rather
    // than a weaker one. (A deadline loop would also have to outlive one heartbeat to mean
    // anything, and that is 15 s of wall clock for a property that resolves in microseconds.)
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut s, &Request::Ping).unwrap();
    match read_frame::<_, Response>(&mut s).expect("a subscribed socket keeps writing") {
        Response::Md(_) => {}
        Response::Pong => panic!(
            "the server answered a positional verb AFTER the mode switch — it is still reading \
             this socket, and the whole no-correlation-id contract rests on it not being"
        ),
        other => panic!("a subscribed socket carries only Response::Md, got {other:?}"),
    }
}

/// **A build with NO hub refuses `MdSubscribe` with `Response::Error`, and the connection SURVIVES
/// POSITIONALLY** — the DEFAULT-BUILD path, so this runs in the roster lane on every PR.
///
/// ⚠ This is the test that holds the deviation from §4.4: that section proposes an
/// `MdRefusal::HubNotMounted`, which — being PER-SPEC — could only be delivered inside an
/// all-refused `MdSubscribed`, and that would MODE-SWITCH a hub-less build into a heartbeat-only
/// writer that will never send a frame. Worse than the refusal it replaces, and it breaks leg (3)
/// of `FEATURE_MARKET_DATA`'s contract. The `Ping`/`Pong` below is the property that variant would
/// have cost.
#[test]
fn a_hubless_server_refuses_md_subscribe_and_stays_positional() {
    let addr = spawn(None);
    let mut s = TcpStream::connect(addr).expect("connect");
    match exchange(&mut s, &Request::MdSubscribe { specs: Vec::new() }) {
        Response::Error(msg) => {
            assert!(msg.contains("live-feeds"), "the refusal names the rebuild: {msg}");
            assert!(msg.contains("VIKE_DATAHUB_LIVE"), "...and the operator's arm: {msg}");
        }
        other => panic!("expected a clean Response::Error, got {other:?}"),
    }
    // ...and NOTHING switched: the same socket still answers positionally.
    assert_matches!(exchange(&mut s, &Request::Ping), Response::Pong);
    // ...and it never advertised the plane, so a well-behaved client would not have sent one.
    let mut s2 = TcpStream::connect(addr).expect("connect");
    match exchange(&mut s2, &Request::Hello { proto_version: PROTO_VERSION }) {
        Response::Welcome { features, .. } => assert!(
            !features.iter().any(|f| f == vike_datahub_client::FEATURE_MARKET_DATA),
            "{features:?}"
        ),
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// **THE HIGHEST-STAKES SINGLE TEST IN THIS CHANGE: a KEYED server refuses `MdSubscribe` BEFORE
/// authentication, and closes.**
///
/// `MdSubscribe` is the only verb on this wire that converts a connection into an unbounded writer
/// AND takes a venue refcount. If the `handle_connection` arm ever lands BEFORE the scope check
/// rather than after it, an unauthenticated peer on a keyed server gets a market-data firehose —
/// and every other test in this file still passes, because the sweep that would have caught it
/// cannot carry this verb.
#[test]
fn a_keyed_server_refuses_md_subscribe_before_auth() {
    let addr = spawn_with_md(Some(keys()), Some(mounted_hub()));
    let mut s = TcpStream::connect(addr).expect("connect");
    match exchange(&mut s, &Request::MdSubscribe { specs: Vec::new() }) {
        Response::AuthDenied { reason } => {
            assert!(reason.contains("not authenticated"), "{reason}");
        }
        other => panic!("an unauthenticated MdSubscribe must be DENIED, got {other:?}"),
    }
    // ...and the socket is closed, not switched: a further frame gets nothing back.
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let _ = write_frame(&mut s, &Request::Ping);
    assert!(
        read_frame::<_, Response>(&mut s).is_err(),
        "a refused pre-auth verb closes the connection"
    );
}

/// **A FOREIGN or expired `MdSessionId` is refused and mutates nothing.**
///
/// Without this, a session id would be a bearer token on a shared authenticated channel and one
/// desktop could unsubscribe another's keys — landing as a silently frozen ladder on a machine
/// whose operator did nothing. The `u128` from the OS generator means guessing is not the threat; a
/// buggy client reusing a stale id after a reconnect is.
#[test]
fn a_foreign_market_data_session_is_refused() {
    let addr = spawn_with_md(None, Some(mounted_hub()));
    let mut s = TcpStream::connect(addr).expect("connect");
    let stranger = MdSessionId::fresh();
    match exchange(
        &mut s,
        &Request::MdUpdate { session: stranger, add: Vec::new(), remove: Vec::new() },
    ) {
        Response::Error(msg) => {
            assert!(msg.contains("no such session"), "{msg}");
            assert!(msg.contains("nothing was changed"), "...and says so: {msg}");
        }
        other => panic!("an unknown session must be a whole-request Error, got {other:?}"),
    }
    // The connection survives — it is a bad *request*, not a bad *connection*.
    assert_matches!(exchange(&mut s, &Request::Ping), Response::Pong);
}

/// **THE STREAM-CONNECTION CAP IS A REFUSAL, NOT A DROPPED SOCKET** — the connection past
/// `MD_MAX_STREAM_CONNS` gets `Response::Error` and stays POSITIONAL.
///
/// ⚠ This was the one `MdSubscribe` refusal path with no test, and it was the one that got it wrong:
/// `run_market_writer` called `hub.open_session()` itself, wrote the error and then DROPPED the
/// stream. Every doc on this path states the opposite invariant —
/// `vike_datahub_client::market`'s module doc ("A server that answers `Response::Error` … has NOT
/// switched: the connection is still positional and the client must not start a reader thread") and
/// `DatahubClient::md_subscribe`, which promises the caller a client "returned intact, so the caller
/// can keep using it positionally". On this path the returned client sat on a closed socket.
///
/// The `Ping`/`Pong` at the end is the same property
/// `a_hubless_server_refuses_md_subscribe_and_stays_positional` asserts for the OTHER whole-request
/// refusal, and the two must not answer differently.
#[test]
fn the_stream_connection_cap_refuses_and_stays_positional() {
    use vike_datahub::md::MD_MAX_STREAM_CONNS;

    let addr = spawn_with_md(None, Some(mounted_hub()));
    // Fill the stream budget. Each of these mode-switches its own socket and is HELD open for the
    // rest of the test — dropping one would free the slot this test is trying to exhaust.
    let mut held = Vec::new();
    for i in 0..MD_MAX_STREAM_CONNS {
        let mut s = TcpStream::connect(addr).expect("connect");
        write_frame(&mut s, &Request::MdSubscribe { specs: Vec::new() }).unwrap();
        match read_frame::<_, Response>(&mut s) {
            Ok(Response::MdSubscribed { .. }) => {}
            other => panic!("stream {i} of the budget did not open: {other:?}"),
        }
        held.push(s);
    }
    assert_eq!(held.len(), MD_MAX_STREAM_CONNS, "floor: the budget was actually spent");

    // The one over.
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::MdSubscribe { specs: Vec::new() }).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    match read_frame::<_, Response>(&mut s).expect("a refusal, not a dropped socket") {
        Response::Error(msg) => {
            assert!(
                msg.contains(&MD_MAX_STREAM_CONNS.to_string()),
                "the refusal names its own number: {msg}"
            );
            assert!(
                msg.contains("nothing was subscribed"),
                "...and says it changed nothing: {msg}"
            );
        }
        other => panic!("expected a clean Response::Error, got {other:?}"),
    }
    // ⚠ THE POINT: NOTHING switched, so the same socket still answers positionally. A writer that
    // had taken this socket would answer a pushed `Response::Md` here, or nothing at all.
    assert_matches!(
        exchange(&mut s, &Request::Ping),
        Response::Pong,
        "a cap refusal must leave the connection positional, exactly as the hub-less one does"
    );
    drop(held);
}

/// **THE CLIENT-SIDE LOCAL REFUSAL — leg 2 of the named run's capability negotiation**
/// (`docs/decisions/0064-a-named-run-carries-no-source.md`).
///
/// The DATA daemon is the ideal negative peer: it decodes `Request::RunNamed` (one schema, two
/// daemons) and advertises no `named_run` capability, which is exactly the shape of a COMPUTE daemon
/// that PREDATES the verb. A client must refuse BEFORE writing a frame.
///
/// ⚠ **Proving "nothing was sent" is the point, and the Ping is how.** `DatahubClient::run_named`
/// returning an `Err` proves nothing on its own — a server refusal reads identically at the call
/// site. So the test sends a `Ping` on the SAME connection afterwards and requires a `Pong`: if a
/// `RunNamed` frame had gone out, its answer would be sitting in the stream and the `Ping` would
/// read THAT instead, positionally. This is the failure
/// `DatahubClient::run_paramscan_profile`'s doc names in the general case — *"there is no reply to
/// inspect and no way to tell afterwards"* — caught the one way it can be caught.
#[test]
fn a_client_refuses_a_named_run_against_a_server_that_does_not_advertise_it() {
    let addr = spawn(None);
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert!(
        !client.serves_named_run(),
        "the DATA daemon must not advertise the compute daemon's capability: {:?}",
        client.features()
    );

    let spec = vike_datahub_client::named_run::NamedRunSpec {
        strategy: "buy_hold".into(),
        params: Vec::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        start: 0,
        end: 3_600_000,
    };
    let err = client.run_named(&spec).expect_err("an unadvertised verb must be refused locally");
    assert!(err.contains("named_run"), "the refusal names the missing capability: {err}");
    assert!(
        err.contains("backtest --addr"),
        "…and the daemon that DOES serve it, because the commonest cause is the wrong address: \
         {err}"
    );
    // ...and the roster verb shares the one refusal, so the two cannot say different things about
    // the same missing string.
    let err = client.named_strategies().expect_err("the roster verb is refused too");
    assert!(err.contains("named_run"), "{err}");

    // THE PROOF THAT NOTHING WAS WRITTEN: the connection is still positional.
    assert_matches!(client.ping(), Ok(()), "a frame was sent, so this stream is desynced");
}
