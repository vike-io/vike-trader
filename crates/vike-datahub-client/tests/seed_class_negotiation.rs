//! The SEED-CLASS capability's negotiation, driven end-to-end against a fake peer — the sibling of
//! `search_method_negotiation.rs`, deliberately its shape down to the counting server.
//!
//! What is being prevented here is the same KIND of failure that file names and a worse INSTANCE of
//! it. `Request` has no `deny_unknown_fields`, so a daemon predating
//! `Request::SeedSeries`'s `class` field decodes the frame, drops the field, routes on the symbol
//! alone and replies with a perfectly well-formed `SeedDone` reporting rows written. For a search
//! selector the cost is a slower search; here the cost is the PERPETUAL's tape written under the
//! SPOT series a chart is about to read — `docs/decisions/0061`'s measured bug, reproduced by its
//! own fix, wearing a success. So the assertion that carries the weight is not "an error came
//! back": it is that NOTHING WAS SENT.
//!
//! Placed in `vike-datahub-client` (the FAST CI lane) for the reason its siblings are: a proto-only
//! PR does not fire the heavier jobs, and this must run every time `-p vike-datahub-client` does.
//! Loopback only, no store, no server crate.

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_datahub_client::{
    DatahubClient, FEATURE_SEED_CLASS, FEATURE_SEED_SERIES, PROTO_VERSION, Request, Response,
    read_frame, write_frame,
};

/// Answer ONE `Hello` with `features`, then count every frame the client sends afterwards and
/// report the total when the connection closes.
///
/// Its own copy rather than a shared helper, exactly as every sibling keeps one: proving "nothing
/// was sent" needs a server that COUNTS, and a real one would answer the frame either way.
fn spawn_counting_fake(
    features: Vec<String>,
) -> (SocketAddr, mpsc::Receiver<usize>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel::<usize>();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            Ok(other) => panic!("fake server expected Hello, got {other:?}"),
            Err(e) => panic!("fake server failed to read Hello: {e}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
        )
        .expect("fake server writes Welcome");
        // It COUNTS and then ANSWERS: these tests drive the SENT direction too, and a fake that
        // stayed silent would park the client in `read_frame` until nextest's timeout killed it.
        let mut frames_after_hello = 0usize;
        while read_frame::<_, Request>(&mut stream).is_ok() {
            frames_after_hello += 1;
            if write_frame(&mut stream, &Response::Error("fake peer".to_string())).is_err() {
                break;
            }
        }
        tx.send(frames_after_hello).expect("report the count");
    });
    (addr, rx, handle)
}

/// A daemon that serves the seed verb and predates the class field — the state every deployment in
/// the field is in today, and the one this capability exists to tell apart from a modern one.
fn an_old_data_daemon() -> Vec<String> {
    ["load_bars", "list_series", "inventory", "series_gaps", FEATURE_SEED_SERIES]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[test]
fn a_class_an_old_daemon_would_drop_is_refused_client_side_without_sending() {
    let (addr, rx, handle) = spawn_counting_fake(an_old_data_daemon());
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");

    let err = client
        .seed_series_classed("bybit", "BTCUSD.P", "5m", Some(vike_model::AssetClass::CryptoPerp))
        .expect_err("a class an old daemon cannot honour must be refused client-side");

    assert!(err.contains(FEATURE_SEED_CLASS), "the refusal names the capability: {err}");
    assert!(
        err.contains("nothing was sent"),
        "the refusal says it did not reach the server, so a reader knows NO ROWS were written from \
         the wrong book: {err}"
    );
    assert!(
        err.contains("DROP"),
        "and it says WHY an old daemon is dangerous rather than merely unsupported: {err}"
    );
    drop(client); // EOF ends the fake server's counting loop

    assert_eq!(rx.recv().expect("count"), 0, "the client sent NOTHING after the refused claim");
    handle.join().expect("fake server thread joins cleanly");
}

/// ⚠ **THE OTHER DIRECTION, and the one a capability check gets wrong most easily.** A seed that
/// names NO class must still be sent to a daemon that advertises nothing, because that is every
/// deployment in the field today and because such a request asks for exactly what it always asked
/// for. Refusing it would break every chart against every older daemon to guard a field the request
/// does not use.
#[test]
fn a_class_less_seed_is_still_sent_to_a_daemon_without_the_capability() {
    let (addr, rx, handle) = spawn_counting_fake(an_old_data_daemon());
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds");
    // The fake answers with a `Response::Error`, so the call fails on the ANSWER — which is what we
    // want: reaching an answer at all proves the write happened.
    let _ = client.seed_series("binance", "BTCUSDT", "5m");
    drop(client);
    assert_eq!(rx.recv().expect("count"), 1, "a class-less seed must be SENT, not refused");
    handle.join().expect("join");
}

/// …and a daemon that DOES advertise it takes the claim. The capability check is the only thing
/// this test exercises — the fake seeds nothing — so it fails on the fake's error, after the write.
#[test]
fn a_daemon_that_advertises_the_capability_receives_the_claim() {
    let mut features = an_old_data_daemon();
    features.push(FEATURE_SEED_CLASS.to_string());
    let (addr, rx, handle) = spawn_counting_fake(features);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds");

    let _ = client.seed_series_classed(
        "bybit",
        "BTCUSD.P",
        "5m",
        Some(vike_model::AssetClass::CryptoPerp),
    );
    drop(client);

    assert_eq!(
        rx.recv().expect("count"),
        1,
        "an advertised capability must be used, not re-checked"
    );
    handle.join().expect("join");
}

/// ⚠ **The two capabilities are INDEPENDENT, and the order of the two client-side refusals is the
/// property.** A daemon whose seed lane is UNARMED but whose build is modern advertises
/// `seed_class` and not `seed_series` — the whole reason `seed_class` is a BUILD fact and
/// `seed_series` a runtime one. Such a request is refused for the LANE, naming the operator's
/// switch, and never for the class: telling an operator their daemon is too old when it is merely
/// switched off is the wrong act.
#[test]
fn an_unarmed_modern_daemon_is_refused_for_its_lane_and_not_for_its_age() {
    let (addr, rx, handle) =
        spawn_counting_fake(vec!["load_bars".to_string(), FEATURE_SEED_CLASS.to_string()]);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds");

    let err = client
        .seed_series_classed("binance", "BTCUSDT", "5m", Some(vike_model::AssetClass::CryptoSpot))
        .expect_err("an unarmed lane is refused");
    assert!(err.contains(FEATURE_SEED_SERIES), "the refusal names the LANE's capability: {err}");
    assert!(err.contains("VIKE_DATAHUB_CHART_SEED"), "...and the switch that arms it: {err}");
    // ⚠ NOT `!err.contains(FEATURE_SEED_CLASS)`: every refusal on this wire prints the ADVERTISED
    // set, and this peer advertises `seed_class` — so the bare substring is in the message as
    // evidence rather than as a diagnosis. What must be absent is the diagnosis.
    assert!(
        !err.contains(&format!("does not advertise `{FEATURE_SEED_CLASS}`")),
        "a modern daemon must never be reported as predating the class field: {err}"
    );
    drop(client);
    assert_eq!(rx.recv().expect("count"), 0, "nothing was sent either way");
    handle.join().expect("join");
}
