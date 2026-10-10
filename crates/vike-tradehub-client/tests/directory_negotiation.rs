//! Feature negotiation for the DIRECTORY read verb (the Trade window's venue · account list, the
//! owner's rulings of 2026-09-30): the client REFUSES CLIENT-SIDE — nothing on the wire after the
//! handshake — against a node whose `Welcome.features` does not advertise [`FEATURE_DIRECTORY`] (so
//! it cannot decode `Request::Directory`), and round-trips the venues and active accounts against
//! one that does.
//!
//! Same design as `tests/settings_show_negotiation.rs`, and deliberately a SEPARATE file: the
//! scripted node here scripts only the one read verb, so the suite stays self-contained while
//! sibling agents extend the other verb files. Why a feature string instead of a
//! `NODE_PROTO_VERSION` bump: the version is folded into the signed auth MAC, so a bump breaks the
//! handshake against every running node — `Welcome.features` is the designed forward-compat hook,
//! and the refusal assertion is "the server saw NOTHING", not merely "the call errored".

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_tradehub_client::proto::{
    FEATURE_DIRECTORY, FEATURE_SETTINGS_SHOW, NODE_PROTO_VERSION, Request, Response, read_frame,
    write_frame,
};
use vike_tradehub_client::wire::{WireDirectory, WireDirectoryAccount, WireDirectoryVenue};
use vike_tradehub_client::{auth, directory};

/// The one key the scripted node accepts for EITHER scope (scope separation is the real server's
/// concern, proven in `crates/vike-tradehub/tests/daemon/directory.rs`; these tests are about
/// feature negotiation).
const KEY: &[u8] = b"directory-negotiation-test-key";

/// The canned payload a feature-advertising scripted node answers — two venues (both named) and
/// one labelled account, so the round-trip covers each field of every row.
fn canned_directory() -> WireDirectory {
    WireDirectory {
        venues: vec![
            WireDirectoryVenue { name: "binance".into(), title: Some("Binance".into()) },
            WireDirectoryVenue { name: "ctrader".into(), title: Some("cTrader".into()) },
        ],
        accounts: vec![WireDirectoryAccount {
            id: 7,
            venue: "binance".into(),
            label: Some("HEDGE".into()),
            tier: "demo".into(),
            venue_account_id: None,
        }],
    }
}

/// Spawn a one-connection scripted node on loopback advertising exactly `features`. It runs the
/// real handshake (verifying the client's mac with [`KEY`] for whatever scope it claims), then
/// answers each post-auth request while RECORDING the request variant names, and finally — when
/// the client hangs up — reports that list through the returned channel. An EMPTY list is the
/// client-side-refusal proof: the client authenticated and then sent NOTHING.
fn scripted_node(features: Vec<String>) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (report_tx, report_rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        let nonce = [7u8; 32];
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: NODE_PROTO_VERSION, nonce, features },
        )
        .expect("write Welcome");
        let Ok(Request::Auth { scope, mac }) = read_frame::<_, Request>(&mut stream) else {
            panic!("expected Auth after Welcome");
        };
        assert!(
            auth::verify(KEY, &nonce, NODE_PROTO_VERSION, scope, &mac),
            "the scripted node's one key must verify"
        );
        write_frame(&mut stream, &Response::AuthOk { scope }).expect("write AuthOk");

        let mut seen: Vec<String> = Vec::new();
        while let Ok(req) = read_frame::<_, Request>(&mut stream) {
            let (name, reply) = match req {
                Request::Directory => {
                    ("Directory", Response::Directory(Box::new(canned_directory())))
                }
                other => panic!("unscripted post-auth request: {other:?}"),
            };
            seen.push(name.to_string());
            write_frame(&mut stream, &reply).expect("write scripted reply");
        }
        report_tx.send(seen).expect("report the post-auth frame list");
    });
    (addr, report_rx)
}

/// The feature set an OLD node advertises — everything EXCEPT `directory`. It still carries
/// `settings-show` and B4's strategy verbs: a node can serve those and predate this verb, which is
/// what makes the refusal below a check on THIS capability and not on a settings-shaped one.
fn old_node_features() -> Vec<String> {
    vec![
        "observe".into(),
        "subscribe".into(),
        "preview".into(),
        "strategy-verbs".into(),
        FEATURE_SETTINGS_SHOW.into(),
    ]
}

/// ⚠ THE REFUSAL: `directory` against a node that does not advertise the feature errors
/// [`std::io::ErrorKind::Unsupported`] naming the capability — and the node received NOTHING after
/// the handshake (no frame went on the wire), which is what a caller reads as "this node predates
/// directory".
#[test]
fn directory_is_refused_client_side_against_an_old_node() {
    let (addr, report) = scripted_node(old_node_features());
    let err = directory(addr, KEY).expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        err.to_string().contains(FEATURE_DIRECTORY),
        "the refusal names the missing capability: {err}"
    );
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// …and against a node that DOES advertise it, the same call round-trips the canned payload — both
/// venues with their titles, and the account with every field intact.
#[test]
fn directory_round_trips_when_advertised() {
    let mut features = old_node_features();
    features.push(FEATURE_DIRECTORY.into());
    let (addr, report) = scripted_node(features);
    let got = directory(addr, KEY).expect("advertised ⇒ served");
    assert_eq!(got, canned_directory());
    assert_eq!(report.recv().expect("report"), vec!["Directory".to_string()]);
}

/// A node that advertises the feature but answers `Response::Error` (started without a settings
/// source) surfaces as [`std::io::ErrorKind::InvalidData`] carrying the server's text — an honest
/// error, never an empty directory.
#[test]
fn a_server_side_error_is_invalid_data_with_the_servers_text() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let Ok(Request::Hello { .. }) = read_frame::<_, Request>(&mut stream) else {
            panic!("expected Hello");
        };
        let nonce = [9u8; 32];
        write_frame(
            &mut stream,
            &Response::Welcome {
                proto_version: NODE_PROTO_VERSION,
                nonce,
                features: vec![FEATURE_DIRECTORY.into()],
            },
        )
        .expect("write Welcome");
        let Ok(Request::Auth { scope, .. }) = read_frame::<_, Request>(&mut stream) else {
            panic!("expected Auth");
        };
        write_frame(&mut stream, &Response::AuthOk { scope }).expect("write AuthOk");
        let Ok(Request::Directory) = read_frame::<_, Request>(&mut stream) else {
            panic!("expected Directory");
        };
        write_frame(&mut stream, &Response::Error("no settings source".into()))
            .expect("write Error");
    });
    let err = directory(addr, KEY).expect_err("a server error must surface");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("no settings source"), "carries the server's text: {err}");
}
