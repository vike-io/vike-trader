//! Feature negotiation for the DIRECTORY read verb (the Trade window's venue · account list): the
//! client REFUSES CLIENT-SIDE, with nothing on the wire after the handshake, against a node whose
//! `Welcome.features` does not advertise [`FEATURE_DIRECTORY`], and round-trips the venues and
//! active accounts against one that does.
//!
//! Why a feature string instead of a `NODE_PROTO_VERSION` bump: the version is folded into the
//! signed auth MAC, so a bump breaks the handshake against every running node. The refusal assertion
//! is "the node saw NOTHING" (`support/fake_node.rs` records it), not merely "the call errored".

#[path = "support/fake_node.rs"]
mod fake_node;

use std::net::SocketAddr;
use std::sync::mpsc;

use vike_tradehub_client::directory;
use vike_tradehub_client::proto::{FEATURE_DIRECTORY, FEATURE_SETTINGS_SHOW, Request, Response};
use vike_tradehub_client::wire::{WireDirectory, WireDirectoryAccount, WireDirectoryVenue};

/// The one key the scripted node accepts (scope separation is proven in
/// `crates/vike-tradehub/tests/daemon/directory.rs`).
const KEY: &[u8] = b"directory-negotiation-test-key";

/// Two named venues and one labelled account, so the round-trip covers each field of every row.
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

/// A scripted node advertising `features` that answers `Directory` with [`canned_directory`].
fn scripted_node(features: Vec<String>) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    fake_node::scripted_node(KEY, features, |req| {
        let (name, reply) = match req {
            Request::Directory => ("Directory", Response::Directory(Box::new(canned_directory()))),
            other => panic!("unscripted post-auth request: {other:?}"),
        };
        (name.to_string(), reply)
    })
}

/// An OLD node: everything EXCEPT `directory`. It still carries `settings-show` and B4's strategy
/// verbs, so the refusal below checks THIS capability and not a settings-shaped one.
fn old_node_features() -> Vec<String> {
    fake_node::features(&[
        "observe",
        "subscribe",
        "preview",
        "strategy-verbs",
        FEATURE_SETTINGS_SHOW,
    ])
}

/// ⚠ THE REFUSAL: `directory` against a node that does not advertise the feature errors
/// [`std::io::ErrorKind::Unsupported`] naming the capability, and the node received NOTHING after
/// the handshake: what a caller reads as "this node predates directory".
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

/// …and against a node that DOES advertise it, the same call round-trips the canned payload.
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
/// source) surfaces as [`std::io::ErrorKind::InvalidData`] carrying the server's text: an honest
/// error, never an empty directory.
#[test]
fn a_server_side_error_is_invalid_data_with_the_servers_text() {
    let (addr, _) = fake_node::scripted_node(KEY, vec![FEATURE_DIRECTORY.into()], |req| {
        let Request::Directory = req else {
            panic!("expected Directory");
        };
        ("Directory".to_string(), Response::Error("no settings source".into()))
    });
    let err = directory(addr, KEY).expect_err("a server error must surface");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("no settings source"), "carries the server's text: {err}");
}
