//! Feature negotiation for the SETTINGS WRITE verb (split-plane REQ-7, write half): the client
//! REFUSES CLIENT-SIDE, with nothing on the wire after the handshake, against a node whose
//! `Welcome.features` does not advertise [`FEATURE_SETTINGS_WRITE`], and round-trips the write (the
//! `SetSetting` command: a row named by its key, the file-era `file` carrying the key's section word,
//! `confirm` empty; the `SettingsWritten { restart_required }` reply) against one that does.
//!
//! Why a feature string instead of a `NODE_PROTO_VERSION` bump: the version is folded into the
//! signed auth MAC, so a bump breaks the handshake against every running node. The refusal assertion
//! is "the node saw NOTHING" (`support/fake_node.rs` records it), not merely "the call errored".
//! ⚠ The write rides its OWN capability, not `settings-show`: a read-half node advertises that
//! string yet cannot decode `SetSetting`, pinned below by an old node that DOES advertise it.

#[path = "support/fake_node.rs"]
mod fake_node;

use std::net::SocketAddr;
use std::sync::mpsc;

use vike_tradehub_client::proto::{
    FEATURE_SETTINGS_SHOW, FEATURE_SETTINGS_WRITE, Request, Response,
};
use vike_tradehub_client::set_setting;
use vike_tradehub_client::wire::WireCommand;

/// The one key the scripted node accepts (scope separation is proven in
/// `crates/vike-tradehub/tests/daemon/settings_write.rs`).
const KEY: &[u8] = b"settings-write-negotiation-test-key";

/// A scripted node advertising `features` that answers each `Command{SetSetting}` with a canned
/// `SettingsWritten`, recording a rendering of every post-auth request.
fn scripted_node(features: Vec<String>) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    fake_node::scripted_node(KEY, features, |req| match req {
        Request::Command { cmd: WireCommand::SetSetting { file, key, value, confirm }, reason } => {
            (
                format!(
                    "SetSetting {file} {key}={value} confirm={} reason={}",
                    confirm.as_deref().unwrap_or("-"),
                    reason.as_deref().unwrap_or("-")
                ),
                Response::SettingsWritten { restart_required: true },
            )
        }
        other => panic!("unscripted post-auth request: {other:?}"),
    })
}

/// An OLD node: everything EXCEPT `settings-write`, INCLUDING the read half's `settings-show`.
fn old_node_features() -> Vec<String> {
    fake_node::features(&[
        "observe",
        "subscribe",
        "preview",
        "strategy-verbs",
        FEATURE_SETTINGS_SHOW,
    ])
}

/// ⚠ THE REFUSAL: `set_setting` against a node that does not advertise `settings-write`, even one
/// that DOES advertise `settings-show`, errors [`std::io::ErrorKind::Unsupported`] naming the
/// capability, and the node received NOTHING after the handshake: what the GUI renders as "server
/// predates settings-write".
#[test]
fn set_setting_is_refused_client_side_against_an_old_node() {
    let (addr, report) = scripted_node(old_node_features());
    let err =
        set_setting(addr, KEY, "config.toml", "config.tradehub_addr", "127.0.0.1:9000", None, None)
            .expect_err("must refuse client-side");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        err.to_string().contains(FEATURE_SETTINGS_WRITE),
        "the refusal names the missing capability: {err}"
    );
    let seen = report.recv().expect("scripted node reports");
    assert!(seen.is_empty(), "NOTHING may be sent after auth on a refusal, saw: {seen:?}");
}

/// …and against a node that DOES advertise it, the write round-trips: key/value, the key's section
/// word in the file-era `file` (the released v0.1.35 daemon still requires it), an EMPTY confirm
/// (`docs/decisions/0086` point 7) and the audit reason; the reply's `restart_required` surfaces.
#[test]
fn set_setting_round_trips_when_advertised() {
    let mut features = old_node_features();
    features.push(FEATURE_SETTINGS_WRITE.into());
    let (addr, report) = scripted_node(features);
    let restart = set_setting(
        addr,
        KEY,
        "policy",
        "policy.max_notional_per_order",
        "250",
        None,
        Some("tighter cap"),
    )
    .expect("advertised ⇒ served");
    assert!(restart, "the reply's restart_required surfaces");
    assert_eq!(
        report.recv().expect("report"),
        vec![
            "SetSetting policy policy.max_notional_per_order=250 confirm=- reason=tighter cap"
                .to_string()
        ]
    );
}

/// A node that advertises the feature but REFUSES the write (`Response::Error`, the loader's
/// message) surfaces as [`std::io::ErrorKind::InvalidData`] carrying the server's text verbatim.
#[test]
fn a_server_side_refusal_is_invalid_data_with_the_servers_text() {
    let (addr, _) = fake_node::scripted_node(KEY, vec![FEATURE_SETTINGS_WRITE.into()], |req| {
        let Request::Command { .. } = req else {
            panic!("expected Command");
        };
        (
            "Command".to_string(),
            Response::Error("unknown field `tradehub_adr`, expected one of …".into()),
        )
    });
    let err = set_setting(addr, KEY, "config.toml", "config.tradehub_adr", "x", None, None)
        .expect_err("a server refusal must surface");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("tradehub_adr"), "carries the server's text: {err}");
}
