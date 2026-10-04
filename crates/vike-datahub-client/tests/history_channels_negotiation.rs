//! The CAPABILITY NEGOTIATION of the history-channels read — [`Request::HistoryChannels`],
//! negotiated by [`FEATURE_HISTORY_CHANNELS`]
//! (`docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §2.3). The model is
//! `crates/vike-datahub-client/tests/backfill_cancel_negotiation.rs`, and the legs:
//!
//! - **the advertisement is a BUILD fact** — a server with NO collector table advertises it and
//!   answers, every built row reading not mounted; it is never withheld for want of a table;
//! - **new client, server without the capability** — refused CLIENT-SIDE with nothing sent; the
//!   caller's answer is then its own compiled table under the caption;
//! - **new client that skips the check, server OLDER than the verb** — the real frame with a tag the
//!   decoder does not know is answered `Response::Error` and the connection SURVIVES;
//! - **a reply from a NEWER server** — a cell `kind` this client has never heard of decodes, and its
//!   `text` is what renders.
//!
//! Placed in `vike-datahub-client` (the FAST CI lane) for that model's reason. Loopback + in-memory.

use std::io::Write as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::history::{
    COMPILED_TABLE_CAPTION, CredentialPresence, HistoryChannelsReport, bars_lookback_floor_ms,
    compiled_report,
};
use vike_datahub_client::proto::{
    FEATURE_HISTORY_CHANNELS, PROTO_VERSION, Request, Response, read_frame, write_frame,
};

/// A hand-rolled server that answers `Hello` with `features`, then counts every frame after it —
/// answering `Ping` with `Pong` and anything else with an error — until the client drops. A fake,
/// because proving "nothing was sent" needs a server that can count.
fn spawn_fake(
    features: Vec<String>,
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

/// Leg 1: a server with no collector table advertises the read and ANSWERS it — every built row
/// not mounted — so an unmounted server is never mistaken for an old one.
#[test]
fn a_tableless_server_advertises_and_answers_the_read() {
    let mut client = DatahubClient::connect(spawn_tableless_server()).expect("handshake");
    assert!(client.serves_history_channels(), "a build fact: {:?}", client.features());
    let report = client.history_channels().expect("served without a table");
    let built: Vec<_> = report
        .venues
        .iter()
        .flat_map(|v| &v.channels)
        .filter(|c| c.state.kind == "built")
        .collect();
    assert!(!built.is_empty(), "guard: the table has built rows");
    assert!(built.iter().all(|c| c.mounted == Some(false)), "no table, nothing mounted");
}

/// Leg 2: a server that does not advertise the capability is refused BEFORE a frame is written,
/// naming the capability — and the answer a caller gives instead is its own compiled table, which
/// carries no overlay and never clamps.
#[test]
fn a_server_without_the_capability_is_refused_client_side_without_sending() {
    let (addr, rx, handle) = spawn_fake(vec!["load_bars".to_string(), "inventory".to_string()]);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    assert!(!client.serves_history_channels());
    let err = client.history_channels().expect_err("refused client-side");
    assert!(err.contains(FEATURE_HISTORY_CHANNELS), "names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");

    let fallback = compiled_report(1_790_899_200_000);
    assert_eq!(fallback.venues.len(), vike_model::VENUES.len());
    assert!(fallback.venues.iter().all(|v| v.held.is_empty()));
    assert!(
        fallback
            .venues
            .iter()
            .flat_map(|v| &v.channels)
            .all(|c| c.mounted.is_none() && c.credential != CredentialPresence::Absent),
        "the fallback claims no overlay it never asked for"
    );
    assert!(fallback.venues.iter().all(|v| bars_lookback_floor_ms(v, fallback.as_of_ms).is_none()));
    assert!(COMPILED_TABLE_CAPTION.contains("older than the history-channels verb"));
}

/// Leg 3: what a server built before the verb sees is this frame with a tag its decoder does not
/// know — the request is a bare unit tag — and it answers `Response::Error` over a connection that
/// SURVIVES.
#[test]
fn an_older_server_answers_the_frame_with_an_error_and_keeps_the_connection() {
    let tag = serde_json::to_value(Request::HistoryChannels).expect("encode");
    assert_eq!(tag, serde_json::json!("HistoryChannels"), "the request is a bare unit tag");
    let mut stream = TcpStream::connect(spawn_tableless_server()).expect("connect");
    let body =
        serde_json::to_vec(&serde_json::json!("HistoryChannelsFromANewerClient")).expect("encode");
    let len = u32::try_from(body.len()).expect("fits u32");
    stream.write_all(&len.to_be_bytes()).expect("length prefix");
    stream.write_all(&body).expect("body");
    stream.flush().expect("flush");
    match read_frame::<_, Response>(&mut stream).expect("the server answers, never hangs") {
        Response::Error(msg) => assert!(!msg.is_empty(), "the decode failure has a message"),
        other => panic!("an unknown variant must get Response::Error, got {other:?}"),
    }
    write_frame(&mut stream, &Request::Ping).expect("send Ping");
    assert!(matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong)));
}

/// Leg 4: a NEWER server's reply carrying a depth form this client has never heard of — a
/// candle-count window, say, with a field it does not know either — DECODES, and the server's
/// `text` is what a client renders. A closed enum would refuse the whole reply.
#[test]
fn a_reply_carrying_an_unknown_cell_kind_decodes_and_renders_its_text() {
    let mut doc =
        serde_json::to_value(Response::HistoryChannels(compiled_report(1_790_899_200_000)))
            .expect("encode");
    let depth = &mut doc["HistoryChannels"]["venues"][0]["channels"][0]["depth"];
    *depth = serde_json::json!({
        "kind": "candle_count_from_a_newer_server",
        "text": "the last 5000 candles of any granularity",
        "candles": 5000,
    });
    let decoded: Response = serde_json::from_value(doc).expect("an unknown cell kind decodes");
    let Response::HistoryChannels(report) = decoded else { panic!("not a HistoryChannels reply") };
    let report: HistoryChannelsReport = report;
    let cell = &report.venues[0].channels[0].depth;
    assert_eq!(cell.kind, "candle_count_from_a_newer_server");
    assert_eq!(cell.text, "the last 5000 candles of any granularity");
    assert_eq!(
        bars_lookback_floor_ms(&report.venues[0], report.as_of_ms),
        None,
        "unknown: no clamp"
    );
}
