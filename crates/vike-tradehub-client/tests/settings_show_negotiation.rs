//! Feature negotiation for the SETTINGS read verb (split-plane REQ-7, read half): the client
//! REFUSES CLIENT-SIDE, with nothing on the wire after the handshake, against a node whose
//! `Welcome.features` does not advertise [`FEATURE_SETTINGS_SHOW`], and round-trips the rendered
//! rows against one that does.
//!
//! Why a feature string instead of a `NODE_PROTO_VERSION` bump: the version is folded into the
//! signed auth MAC, so a bump breaks the handshake against every running node. The refusal assertion
//! is "the node saw NOTHING" (`support/fake_node.rs` records it), not merely "the call errored".

#[path = "support/fake_node.rs"]
mod fake_node;

use std::net::SocketAddr;
use std::sync::mpsc;

use vike_tradehub_client::proto::{FEATURE_SETTINGS_SHOW, Request, Response};
use vike_tradehub_client::settings_show;
use vike_tradehub_client::wire::{WireSettingsRow, WireSettingsShow};

/// The one key the scripted node accepts (scope separation is proven in
/// `crates/vike-tradehub/tests/daemon/settings_show.rs`).
const KEY: &[u8] = b"settings-show-negotiation-test-key";

/// One plain row and one redacted one, so the round-trip covers both value shapes.
fn canned_show() -> WireSettingsShow {
    WireSettingsShow {
        settings_dir: Some("/srv/vike-<unit>/settings".into()),
        rows: vec![
            WireSettingsRow {
                section: "config.toml".into(),
                key: "config.tradehub_addr".into(),
                value: "127.0.0.1:7879".into(),
                origin: "config.toml".into(),
                read_by: "tradehub".into(),
            },
            WireSettingsRow {
                section: "config.toml".into(),
                key: "config.bot_token".into(),
                value: "<set>".into(),
                origin: "config.toml".into(),
                read_by: "NO".into(),
            },
        ],
    }
}

/// A scripted node advertising `features` that answers `SettingsShow` with [`canned_show`].
fn scripted_node(features: Vec<String>) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    fake_node::scripted_node(KEY, features, |req| {
        let (name, reply) = match req {
            Request::SettingsShow => {
                ("SettingsShow", Response::SettingsShow(Box::new(canned_show())))
            }
            other => panic!("unscripted post-auth request: {other:?}"),
        };
        (name.to_string(), reply)
    })
}

/// An OLD node: everything EXCEPT `settings-show` (including B4's strategy verbs: a node can serve
/// those and still predate this verb).
fn old_node_features() -> Vec<String> {
    fake_node::features(&["observe", "subscribe", "preview", "strategy-verbs"])
}

/// ⚠ THE REFUSAL: `settings_show` against a node that does not advertise the feature errors
/// [`std::io::ErrorKind::Unsupported`] naming the capability, and the node received NOTHING after
/// the handshake: what the GUI renders as "server predates settings-show".
#[test]
fn settings_show_is_refused_client_side_against_an_old_node() {
    let (addr, report) = scripted_node(old_node_features());
    let err = settings_show(addr, KEY).expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        err.to_string().contains(FEATURE_SETTINGS_SHOW),
        "the refusal names the missing capability: {err}"
    );
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// …and against a node that DOES advertise it, the same call round-trips the canned rows.
#[test]
fn settings_show_round_trips_when_advertised() {
    let mut features = old_node_features();
    features.push(FEATURE_SETTINGS_SHOW.into());
    let (addr, report) = scripted_node(features);
    let show = settings_show(addr, KEY).expect("advertised ⇒ served");
    assert_eq!(show, canned_show());
    assert_eq!(report.recv().expect("report"), vec!["SettingsShow".to_string()]);
}

/// A node that advertises the feature but answers `Response::Error` (started without a settings
/// source) surfaces as [`std::io::ErrorKind::InvalidData`] carrying the server's text: an honest
/// error, never an empty settings table.
#[test]
fn a_server_side_error_is_invalid_data_with_the_servers_text() {
    let (addr, _) = fake_node::scripted_node(KEY, vec![FEATURE_SETTINGS_SHOW.into()], |req| {
        let Request::SettingsShow = req else {
            panic!("expected SettingsShow");
        };
        ("SettingsShow".to_string(), Response::Error("no settings source".into()))
    });
    let err = settings_show(addr, KEY).expect_err("a server error must surface");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("no settings source"), "carries the server's text: {err}");
}
