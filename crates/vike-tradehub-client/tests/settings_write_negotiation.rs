//! Feature negotiation for the SETTINGS WRITE verb (split-plane REQ-7, write half): the client
//! REFUSES CLIENT-SIDE — nothing on the wire after the handshake — against a node whose
//! `Welcome.features` does not advertise [`FEATURE_SETTINGS_WRITE`], and round-trips the write
//! (the `SetSetting` command with its typed confirm, the `SettingsWritten { restart_required }`
//! reply) against one that does.
//!
//! Same design as `tests/settings_show_negotiation.rs` (the read half) and deliberately a
//! SEPARATE file for the same reason that one is: the scripted node here scripts only the write
//! verb, so the suite stays self-contained. Why a feature string instead of a
//! `NODE_PROTO_VERSION` bump: the version is folded into the signed auth MAC, so a bump breaks
//! the handshake against every running node — `Welcome.features` is the designed forward-compat
//! hook, and the refusal assertion is "the server saw NOTHING", not merely "the call errored".
//! ⚠ The write rides its OWN capability, not `settings-show`: a read-half node advertises that
//! string yet cannot decode `SetSetting` — pinned below by an old node that DOES advertise
//! `settings-show`.

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_tradehub_client::proto::{
    read_frame, write_frame, Request, Response, FEATURE_SETTINGS_SHOW, FEATURE_SETTINGS_WRITE,
    NODE_PROTO_VERSION,
};
use vike_tradehub_client::wire::WireCommand;
use vike_tradehub_client::{auth, set_setting};

/// The one key the scripted node accepts for EITHER scope (scope separation is the real server's
/// concern, proven in `crates/vike-tradehub/tests/daemon/settings_write.rs`; these tests are
/// about feature negotiation).
const KEY: &[u8] = b"settings-write-negotiation-test-key";

/// Spawn a one-connection scripted node on loopback advertising exactly `features`. It runs the
/// real handshake (verifying the client's mac with [`KEY`] for whatever scope it claims), then
/// answers each post-auth `Command{SetSetting}` with a canned `SettingsWritten` while RECORDING
/// a rendering of every post-auth request, and finally — when the client hangs up — reports that
/// list through the returned channel. An EMPTY list is the client-side-refusal proof: the client
/// authenticated and then sent NOTHING.
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
                Request::Command {
                    cmd: WireCommand::SetSetting { file, key, value, confirm },
                    reason,
                } => (
                    format!(
                        "SetSetting {file} {key}={value} confirm={} reason={}",
                        confirm.as_deref().unwrap_or("-"),
                        reason.as_deref().unwrap_or("-")
                    ),
                    Response::SettingsWritten { restart_required: true },
                ),
                other => panic!("unscripted post-auth request: {other:?}"),
            };
            seen.push(name);
            write_frame(&mut stream, &reply).expect("write scripted reply");
        }
        report_tx.send(seen).expect("report the post-auth frame list");
    });
    (addr, report_rx)
}

/// The feature set an OLD node advertises — everything EXCEPT `settings-write`, INCLUDING the
/// read half's `settings-show`: a read-half node advertises that string and still cannot decode
/// `SetSetting`, which is exactly why the write has its own capability.
fn old_node_features() -> Vec<String> {
    vec![
        "observe".into(),
        "subscribe".into(),
        "preview".into(),
        "strategy-verbs".into(),
        FEATURE_SETTINGS_SHOW.into(),
    ]
}

/// ⚠ THE REFUSAL: `set_setting` against a node that does not advertise `settings-write` — even
/// one that DOES advertise `settings-show` — errors [`std::io::ErrorKind::Unsupported`] naming
/// the capability, and the node received NOTHING after the handshake (no frame on the wire),
/// which is what the GUI renders as "server predates settings-write".
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

/// …and against a node that DOES advertise it, the write round-trips: the request carries the
/// command (file/key/value), the typed confirm and the audit reason beside it, and the reply's
/// `restart_required` surfaces to the caller.
#[test]
fn set_setting_round_trips_when_advertised() {
    let mut features = old_node_features();
    features.push(FEATURE_SETTINGS_WRITE.into());
    let (addr, report) = scripted_node(features);
    let restart = set_setting(
        addr,
        KEY,
        "policy.toml",
        "policy.max_notional_per_order",
        "250",
        Some("policy.max_notional_per_order"),
        Some("tighter cap"),
    )
    .expect("advertised ⇒ served");
    assert!(restart, "the reply's restart_required surfaces");
    assert_eq!(
        report.recv().expect("report"),
        vec!["SetSetting policy.toml policy.max_notional_per_order=250 \
             confirm=policy.max_notional_per_order reason=tighter cap"
            .to_string()]
    );
}

/// A node that advertises the feature but REFUSES the write (`Response::Error` — the daemon's
/// confirm contract, the loader's message) surfaces as [`std::io::ErrorKind::InvalidData`]
/// carrying the server's text verbatim — the GUI renders it as the row's refusal.
#[test]
fn a_server_side_refusal_is_invalid_data_with_the_servers_text() {
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
                features: vec![FEATURE_SETTINGS_WRITE.into()],
            },
        )
        .expect("write Welcome");
        let Ok(Request::Auth { scope, .. }) = read_frame::<_, Request>(&mut stream) else {
            panic!("expected Auth");
        };
        write_frame(&mut stream, &Response::AuthOk { scope }).expect("write AuthOk");
        let Ok(Request::Command { .. }) = read_frame::<_, Request>(&mut stream) else {
            panic!("expected Command");
        };
        write_frame(
            &mut stream,
            &Response::Error("unknown field `tradehub_adr`, expected one of …".into()),
        )
        .expect("write Error");
    });
    let err = set_setting(addr, KEY, "config.toml", "config.tradehub_adr", "x", None, None)
        .expect_err("a server refusal must surface");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("tradehub_adr"), "carries the server's text: {err}");
}
