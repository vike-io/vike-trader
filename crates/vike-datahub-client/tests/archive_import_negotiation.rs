//! The ARCHIVE IMPORT verb's CAPABILITY NEGOTIATION, driven end-to-end, both directions
//! (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §2.5).
//!
//! The verb shipped WITHOUT a `PROTO_VERSION` bump, so the version handshake cannot protect it.
//! What protects it is `Welcome.features`: a server advertises [`FEATURE_ARCHIVE_IMPORT`] plus one
//! `import_format=<id>` entry per format only when an import lane is mounted, and the CLIENT refuses
//! locally — without sending — when either is absent. These tests pin:
//!
//! - **new client, server without the lane** — refused client-side with nothing sent, for a missing
//!   capability, for a missing FORMAT, and for a dataset the shared validator refuses; and a raw
//!   frame that skips the check is answered with a clean `Response::Error` by the real `serve`,
//!   over a connection that SURVIVES;
//! - **new client, server OLDER than the verb** — the real frame, with a tag the decoder does not
//!   know (exactly what an older build sees), is answered `Response::Error` and the connection
//!   survives;
//! - **new client, advertising server** — the frame the server receives IS the request, and the
//!   answer round-trips through the client;
//! - **old client, new server** — a `Welcome` carrying the new entries changes nothing for a client
//!   that never sends the verb.
//!
//! Placed in `vike-datahub-client` (the FAST CI lane) for `backfill_negotiation.rs`'s reason: a
//! proto-only PR must run these every time `-p vike-datahub-client` runs. Loopback + in-memory only.

// `common::spawn_server` is the REAL `serve`, which mounts no import lane.
mod common;

use std::io::Write as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use vike_data::MemHistStore;
use vike_datahub_client::archive::{
    ArchiveInventory, DatasetDir, ImportDone, ImportPlan, ImportSpec,
};
use vike_datahub_client::proto::{
    FEATURE_ARCHIVE_IMPORT, Request, Response, advertised_import_formats, import_format_feature,
    write_frame,
};
use vike_datahub_client::{DatahubClient, PROTO_VERSION, read_frame};

use common::spawn_server;

const FORMAT: &str = "dukascopy-bi5";

fn spec() -> ImportSpec {
    ImportSpec {
        format: FORMAT.to_string(),
        dataset: "EURUSD".to_string(),
        from_day: None,
        to_day: None,
        bars: vec!["1m".to_string()],
        dry_run: true,
        verify: false,
    }
}

fn plan_only_answer() -> ImportDone {
    ImportDone {
        plan: ImportPlan {
            format: FORMAT.to_string(),
            dataset: "EURUSD".to_string(),
            venue: "dukascopy".to_string(),
            server_dir: "/srv/vike-<unit>/market_data/imports/dukascopy-bi5/EURUSD".to_string(),
            dir: DatasetDir::Absent,
            admission: "point value 100000".to_string(),
            inventory: ArchiveInventory::default(),
            from_day: None,
            to_day: None,
            days: Vec::new(),
            gaps: Vec::new(),
            bars: vec!["1m".to_string()],
            series: None,
        },
        outcome: None,
    }
}

/// A hand-rolled server that answers `Hello` with `features`, then COUNTS every frame after it —
/// answering each `ImportArchive` with `answer` when one is given — until the client drops. Returns
/// the frames it received.
///
/// A fake rather than the real `serve` because proving "nothing was sent" needs a server that can
/// count; the `backfill_negotiation.rs` precedent.
fn spawn_fake(
    features: Vec<String>,
    answer: Option<ImportDone>,
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
            let reply = match (&request, &answer) {
                (Request::ImportArchive(_), Some(done)) => {
                    Response::ArchiveImported(Box::new(done.clone()))
                }
                (Request::Ping, _) => Response::Pong,
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

/// Every refusal below must be LOCAL: the fake sees zero frames after the handshake.
fn assert_refused_without_sending(features: Vec<String>, spec: &ImportSpec, needle: &str) {
    let (addr, rx, handle) = spawn_fake(features, None);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    let err = client.import_archive(spec).expect_err("must be refused client-side");
    assert!(err.contains(needle), "the refusal names {needle:?}: {err}");
    drop(client); // EOF ends the fake's counting loop
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");
}

// ---- new client, server WITHOUT the lane ---------------------------------------------------------

/// No `archive_import` in the `Welcome` — an older server, or one with no lane mounted — is refused
/// before a frame is written, naming the capability.
#[test]
fn a_server_without_the_capability_is_refused_client_side_without_sending() {
    let features = vec!["load_bars".to_string(), import_format_feature(FORMAT)];
    assert_refused_without_sending(features.clone(), &spec(), FEATURE_ARCHIVE_IMPORT);
    assert_refused_without_sending(features, &spec(), "nothing was sent");
}

/// The capability WITHOUT the named format — a server whose registry lacks it — is refused before a
/// frame is written, and the refusal names what the server DOES import.
#[test]
fn a_server_without_the_named_format_is_refused_client_side_without_sending() {
    let features = vec![FEATURE_ARCHIVE_IMPORT.to_string(), import_format_feature("tardis-csv")];
    assert_refused_without_sending(features.clone(), &spec(), "tardis-csv");
    // ...and an advertised capability with NO format at all imports nothing.
    assert_refused_without_sending(vec![FEATURE_ARCHIVE_IMPORT.to_string()], &spec(), FORMAT);
}

/// An advertising server is still not sent a dataset the SHARED validator refuses — the client's
/// half of "both ends call one validator".
#[test]
fn an_invalid_dataset_is_refused_client_side_without_sending() {
    let features = vec![FEATURE_ARCHIVE_IMPORT.to_string(), import_format_feature(FORMAT)];
    for bad in ["..", "EUR/USD", "eurusd", "CON"] {
        let mut s = spec();
        s.dataset = bad.to_string();
        let needle = if bad == "CON" { "DEVICE NAME" } else { "import dataset" };
        assert_refused_without_sending(features.clone(), &s, needle);
    }
}

/// Leg 1, the server half: a server with no lane mounted advertises neither the capability nor a
/// format.
#[test]
fn a_server_with_no_lane_mounted_advertises_neither_the_capability_nor_a_format() {
    let client = DatahubClient::connect(spawn_server(Arc::new(MemHistStore::new())))
        .expect("handshake on connect");
    assert!(
        !client.features().iter().any(|f| f == FEATURE_ARCHIVE_IMPORT),
        "{:?}",
        client.features()
    );
    assert!(advertised_import_formats(client.features()).is_empty(), "{:?}", client.features());
}

/// A raw `ImportArchive` frame that reaches a server with no lane (a caller that skipped the check)
/// gets a clean `Response::Error` NAMING the capability, and the connection SURVIVES.
#[test]
fn a_server_with_no_lane_answers_a_raw_import_frame_with_the_capability_refusal() {
    let mut stream =
        TcpStream::connect(spawn_server(Arc::new(MemHistStore::new()))).expect("connect");
    write_frame(&mut stream, &Request::ImportArchive(spec())).expect("send ImportArchive");
    match read_frame::<_, Response>(&mut stream).expect("the server answers, never hangs") {
        Response::Error(msg) => {
            assert!(msg.contains(FEATURE_ARCHIVE_IMPORT), "names the capability: {msg}");
            assert!(msg.contains("Nothing was read"), "says it touched nothing: {msg}");
        }
        other => panic!("a lane-less server must answer Error, got {other:?}"),
    }
    write_frame(&mut stream, &Request::Ping).expect("send Ping on the surviving connection");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
}

// ---- new client, server OLDER than the verb --------------------------------------------------------

/// Leg 3: what a server built before this verb sees is THIS frame with a tag its decoder does not
/// know. The real frame is sent with exactly that one difference, and the server answers a clean
/// `Response::Error` — never a dropped connection, never a hang — and keeps serving.
#[test]
fn an_older_server_answers_the_import_frame_with_an_error_and_keeps_the_connection() {
    let real = serde_json::to_value(Request::ImportArchive(spec())).expect("encode");
    let payload = real["ImportArchive"].clone();
    assert!(payload.is_object(), "the import payload is a struct: {real}");
    let unknown = serde_json::json!({ "ImportArchiveFromANewerClient": payload });
    let body = serde_json::to_vec(&unknown).expect("encode");

    let mut stream =
        TcpStream::connect(spawn_server(Arc::new(MemHistStore::new()))).expect("connect");
    let len = u32::try_from(body.len()).expect("fits u32");
    stream.write_all(&len.to_be_bytes()).expect("length prefix");
    stream.write_all(&body).expect("body");
    stream.flush().expect("flush");
    match read_frame::<_, Response>(&mut stream).expect("the server answers, never hangs") {
        Response::Error(msg) => assert!(!msg.is_empty(), "the decode failure carries a message"),
        other => panic!("an unknown variant must get Response::Error, got {other:?}"),
    }
    write_frame(&mut stream, &Request::Ping).expect("send Ping");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
}

// ---- new client, ADVERTISING server ----------------------------------------------------------------

/// The frame an advertising server receives IS the request, and its answer comes back through the
/// client intact.
#[test]
fn an_advertising_server_receives_the_request_and_its_answer_round_trips() {
    let features = vec![FEATURE_ARCHIVE_IMPORT.to_string(), import_format_feature(FORMAT)];
    let (addr, rx, handle) = spawn_fake(features, Some(plan_only_answer()));
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let done = client.import_archive(&spec()).expect("an advertising server is sent the verb");
    assert_eq!(done, plan_only_answer());
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert_eq!(received.len(), 1, "exactly one frame: {received:?}");
    match &received[0] {
        Request::ImportArchive(got) => assert_eq!(*got, spec()),
        other => panic!("the server was sent {other:?}"),
    }
    handle.join().expect("fake joins");
}

// ---- OLD client, new server ------------------------------------------------------------------------

/// A client that never sends the verb — every client before it existed — is unaffected by a
/// `Welcome` carrying the new entries: the handshake decodes, the entries are strings it ignores,
/// and its own verbs are served as before. That is the room `Welcome.features` reserved, and the
/// reason no version bump was needed.
#[test]
fn an_old_client_is_unaffected_by_the_new_advertisement() {
    let features = vec![
        "load_bars".to_string(),
        FEATURE_ARCHIVE_IMPORT.to_string(),
        import_format_feature(FORMAT),
    ];
    // The Welcome as an old client's decoder sees it: the same variant and fields it always had.
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

    let (addr, rx, handle) = spawn_fake(features, None);
    let mut client = DatahubClient::connect(addr).expect("an old-shaped handshake still succeeds");
    client.ping().expect("an old client's verbs are served as before");
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(matches!(received.as_slice(), [Request::Ping]), "{received:?}");
    handle.join().expect("fake joins");
}
