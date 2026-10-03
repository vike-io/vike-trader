//! The CAPABILITY NEGOTIATION of the two verbs that list and stop RUNNING backfills —
//! `ListBackfills` and `CancelBackfill`, negotiated by [`FEATURE_BACKFILL_CANCEL`]
//! (`docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §4 and §6's T9).
//!
//! The verbs shipped WITHOUT a `PROTO_VERSION` bump, so the version handshake cannot protect them.
//! What protects them is `Welcome.features`, and these tests pin each leg, in both directions —
//! `crates/vike-datahub-client/tests/archive_import_negotiation.rs` is the model:
//!
//! - **the advertisement is exactly the MOUNTED table** — a server with a collector table advertises
//!   the capability beside `backfill`, and one without advertises neither;
//! - **new client, server without the capability** — refused CLIENT-SIDE with nothing sent, and a
//!   raw frame that skips the check is answered with a clean `Response::Error` naming the
//!   capability, over a connection that SURVIVES;
//! - **new client, server OLDER than the verbs** — the real frames with a tag the decoder does not
//!   know (exactly what an older build sees) are answered `Response::Error` and the connection
//!   survives;
//! - **new client, advertising server** — the frame the server receives IS the request, and the
//!   answer round-trips through the client;
//! - **old client, new server** — a `Welcome` carrying the new string changes nothing for a client
//!   that never sends the verbs, and its own `Backfill` is answered by a server that advertises the
//!   capability exactly as before.
//!
//! Placed in `vike-datahub-client` (the FAST CI lane) for `backfill_negotiation.rs`'s reason: a
//! proto-only PR must run these every time `-p vike-datahub-client` runs. Loopback + in-memory only.

use std::io::Write as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::backfill::{BackfillFn, BackfillTable};
use vike_datahub::{serve, serve_with_backfill};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{
    BackfillCancelDone, BackfillDone, FEATURE_BACKFILL, FEATURE_BACKFILL_CANCEL, PROTO_VERSION,
    Request, Response, RunningBackfill, read_frame, write_frame,
};

fn running() -> RunningBackfill {
    RunningBackfill {
        id: 1,
        venue: "oanda".to_string(),
        symbol: "EUR_USD".to_string(),
        interval: "5s".to_string(),
        start: 1_104_710_400_000,
        end: 1_104_796_799_999,
        peer: Some("127.0.0.1:50000".to_string()),
        started_ms: 1_700_000_000_000,
        elapsed_ms: 1_000,
        lane: "CredentialedKlines".to_string(),
        stoppable: true,
        cancelled: false,
        boundaries: 2,
    }
}

/// A hand-rolled server that answers `Hello` with `features`, then COUNTS every frame after it —
/// answering `ListBackfills` and `CancelBackfill` from `list` and `cancel`, a `Backfill` with an
/// empty `BackfillDone`, and `Ping` with `Pong` — until the client drops. Returns the frames it
/// received.
///
/// A fake rather than the real `serve` because proving "nothing was sent" needs a server that can
/// count; the `backfill_negotiation.rs` precedent.
fn spawn_fake(
    features: Vec<String>,
    list: Vec<RunningBackfill>,
    cancel: BackfillCancelDone,
) -> (SocketAddr, mpsc::Receiver<Vec<Request>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel::<Vec<Request>>();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("fake server expected Hello, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
        )
        .expect("fake server writes Welcome");
        let mut received = Vec::new();
        while let Ok(request) = read_frame::<_, Request>(&mut stream) {
            let reply = match &request {
                Request::ListBackfills => Response::RunningBackfills(list.clone()),
                Request::CancelBackfill { .. } => Response::BackfillsCancelled(cancel.clone()),
                Request::Backfill { .. } => Response::BackfillDone(BackfillDone {
                    rows_written: 0,
                    first_ts: None,
                    last_ts: None,
                }),
                Request::Ping => Response::Pong,
                _ => Response::Error("fake server: unexpected frame".to_string()),
            };
            received.push(request);
            if write_frame(&mut stream, &reply).is_err() {
                break;
            }
        }
        tx.send(received).expect("report what arrived");
    });
    (addr, rx, handle)
}

/// The real `serve` — NO collector table — over an in-memory store.
fn spawn_tableless_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// The real `serve_with_backfill` with a one-row table whose collector writes nothing and answers
/// `rows` at once.
fn spawn_server_with_a_table(rows: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let collect: BackfillFn =
        Box::new(move |_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| Ok(rows));
    let table = BackfillTable::new(vec![("binance".to_string(), collect)]);
    thread::spawn(move || {
        let _ = serve_with_backfill(listener, store, Some(table));
    });
    addr
}

// ---- the advertisement is exactly the MOUNTED table ---------------------------------------------

/// Leg 1, the server half, both ways: a mounted table advertises the capability beside `backfill`;
/// no table advertises neither.
#[test]
fn the_capability_is_advertised_exactly_when_a_table_is_mounted() {
    let with = DatahubClient::connect(spawn_server_with_a_table(0)).expect("handshake");
    assert!(with.features().iter().any(|f| f == FEATURE_BACKFILL), "{:?}", with.features());
    assert!(with.serves_backfill_cancel(), "a mounted table advertises it: {:?}", with.features());

    let without = DatahubClient::connect(spawn_tableless_server()).expect("handshake");
    assert!(!without.features().iter().any(|f| f == FEATURE_BACKFILL), "{:?}", without.features());
    assert!(
        !without.serves_backfill_cancel(),
        "a server with no table runs no backfill to list or stop: {:?}",
        without.features()
    );
}

// ---- new client, server WITHOUT the capability --------------------------------------------------

/// A server advertising `backfill` and NOT `backfill_cancel` — an older server that runs backfills
/// but predates the door onto them — is refused BOTH verbs before a frame is written, naming the
/// capability.
#[test]
fn a_server_without_the_capability_is_refused_client_side_without_sending() {
    let features = vec!["load_bars".to_string(), FEATURE_BACKFILL.to_string()];
    let (addr, rx, handle) = spawn_fake(features, Vec::new(), BackfillCancelDone::default());
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    assert!(!client.serves_backfill_cancel());

    let err = client.list_backfills().expect_err("the list must be refused client-side");
    assert!(err.contains(FEATURE_BACKFILL_CANCEL), "names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    let err = client.cancel_backfill("oanda", "EUR_USD", "5s").expect_err("so must the cancel");
    assert!(err.contains(FEATURE_BACKFILL_CANCEL), "names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");

    drop(client); // EOF ends the fake's counting loop
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");
}

/// Leg 3 on a server with no table: a raw frame that skips the check is answered with a clean
/// `Response::Error` naming the capability, and the connection SURVIVES — both verbs.
#[test]
fn a_tableless_server_answers_raw_registry_frames_with_the_capability_refusal() {
    let mut stream = TcpStream::connect(spawn_tableless_server()).expect("connect");
    let frames = [
        Request::ListBackfills,
        Request::CancelBackfill {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1h".to_string(),
        },
    ];
    for request in frames {
        write_frame(&mut stream, &request).expect("send the raw frame");
        match read_frame::<_, Response>(&mut stream).expect("the server answers, never hangs") {
            Response::Error(msg) => {
                assert!(msg.contains(FEATURE_BACKFILL_CANCEL), "names the capability: {msg}");
                assert!(msg.contains("Nothing was changed"), "says it changed nothing: {msg}");
            }
            other => panic!("a tableless server must answer {request:?} with Error: {other:?}"),
        }
    }
    write_frame(&mut stream, &Request::Ping).expect("send Ping on the surviving connection");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
}

// ---- new client, server OLDER than the verbs -----------------------------------------------------

/// What a server built before these verbs sees is THESE frames with tags its decoder does not
/// know. Each is sent with exactly that one difference — the unit variant as a bare string, the
/// cancel as a tagged object carrying the real payload — and answered with a clean
/// `Response::Error`, never a dropped connection and never a hang.
#[test]
fn an_older_server_answers_the_registry_frames_with_an_error_and_keeps_the_connection() {
    let list = serde_json::to_value(Request::ListBackfills).expect("encode");
    assert_eq!(list, serde_json::json!("ListBackfills"), "the list is a bare unit tag");
    let cancel = serde_json::to_value(Request::CancelBackfill {
        venue: "oanda".to_string(),
        symbol: "EUR_USD".to_string(),
        interval: "5s".to_string(),
    })
    .expect("encode");
    let payload = cancel["CancelBackfill"].clone();
    assert!(payload.is_object(), "the cancel payload is a struct: {cancel}");
    let unknown = [
        serde_json::json!("ListBackfillsFromANewerClient"),
        serde_json::json!({ "CancelBackfillFromANewerClient": payload }),
    ];

    let mut stream = TcpStream::connect(spawn_tableless_server()).expect("connect");
    for frame in unknown {
        let body = serde_json::to_vec(&frame).expect("encode");
        let len = u32::try_from(body.len()).expect("fits u32");
        stream.write_all(&len.to_be_bytes()).expect("length prefix");
        stream.write_all(&body).expect("body");
        stream.flush().expect("flush");
        match read_frame::<_, Response>(&mut stream).expect("the server answers, never hangs") {
            Response::Error(msg) => assert!(!msg.is_empty(), "the decode failure has a message"),
            other => panic!("an unknown variant must get Response::Error, got {other:?}"),
        }
    }
    write_frame(&mut stream, &Request::Ping).expect("send Ping");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
}

// ---- new client, ADVERTISING server --------------------------------------------------------------

/// The frames an advertising server receives ARE the requests, and the answers come back through
/// the client intact.
#[test]
fn an_advertising_server_receives_the_requests_and_their_answers_round_trip() {
    let features = vec![FEATURE_BACKFILL.to_string(), FEATURE_BACKFILL_CANCEL.to_string()];
    let cancel = BackfillCancelDone {
        flagged: vec![RunningBackfill { cancelled: true, ..running() }],
        unstoppable: Vec::new(),
    };
    let (addr, rx, handle) = spawn_fake(features, vec![running()], cancel.clone());
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.serves_backfill_cancel());
    assert_eq!(client.list_backfills().expect("listed"), vec![running()]);
    assert_eq!(client.cancel_backfill("oanda", "EUR_USD", "5s").expect("cancelled"), cancel);
    drop(client);

    let received = rx.recv().expect("fake reports");
    assert_eq!(received.len(), 2, "exactly two frames: {received:?}");
    assert!(matches!(received[0], Request::ListBackfills), "{received:?}");
    match &received[1] {
        Request::CancelBackfill { venue, symbol, interval } => {
            assert_eq!(
                (venue.as_str(), symbol.as_str(), interval.as_str()),
                ("oanda", "EUR_USD", "5s")
            )
        }
        other => panic!("the server was sent {other:?}"),
    }
    handle.join().expect("fake joins");
}

// ---- OLD client, new server ------------------------------------------------------------------------

/// A client that never sends the new verbs — every client before they existed — is unaffected by a
/// `Welcome` carrying the new string: the `Welcome` keeps its shape, and its own verbs are served as
/// before. That is the room `Welcome.features` reserved, and the reason no version bump was needed.
#[test]
fn an_old_client_is_unaffected_by_the_new_advertisement() {
    let features = vec![
        "load_bars".to_string(),
        FEATURE_BACKFILL.to_string(),
        FEATURE_BACKFILL_CANCEL.to_string(),
    ];
    let welcome = serde_json::to_string(&Response::Welcome {
        proto_version: PROTO_VERSION,
        features: features.clone(),
        nonce: None,
    })
    .expect("encode");
    let old_shape = format!(
        r#"{{"Welcome":{{"proto_version":{PROTO_VERSION},"features":{}}}}}"#,
        serde_json::to_string(&features).expect("encode features")
    );
    assert_eq!(welcome, old_shape, "the Welcome's SHAPE is unchanged; only its strings grew");

    let (addr, rx, handle) = spawn_fake(features, Vec::new(), BackfillCancelDone::default());
    let mut client = DatahubClient::connect(addr).expect("an old-shaped handshake still succeeds");
    client.ping().expect("an old client's verbs are served as before");
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(matches!(received.as_slice(), [Request::Ping]), "{received:?}");
    handle.join().expect("fake joins");
}

/// ...and against the REAL new server: an old client's `Backfill` — the frame it always sent — is
/// answered with the `BackfillDone` it always got, by a server that now registers the request and
/// advertises the door onto it.
#[test]
fn an_old_clients_backfill_is_answered_unchanged_by_a_server_that_advertises_the_cancel() {
    let addr = spawn_server_with_a_table(3);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.serves_backfill_cancel(), "guard: this server is the NEW one");
    let done = client.backfill("binance", "BTCUSDT", "1h", 0, 3_600_000).expect("served");
    assert_eq!(done.rows_written, 3, "{done:?}");
    // The request was registered while it ran and is gone now that it has answered.
    assert!(client.list_backfills().expect("listed").is_empty());
}
