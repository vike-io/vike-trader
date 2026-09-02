//! End-to-end proof that `vike-cli strategy-status` actually REACHES the wire verb — the gap the
//! I10 rehearsal found: `vike_tradehub_client::strategy_status` (split-plane B4) existed, and no
//! CLI called it, so asking a running node "what are you running" meant compiling a throwaway
//! probe (`docs/ops/i10-rehearsal-2026-08-19.md`).
//!
//! Hermetic and loopback-only, the `tests/trade_node_e2e.rs` shape: a REAL paper node
//! (`vike_run::build_paper_maker_core` + the real `vike_tradehub::{server, publish}`) on an
//! ephemeral `127.0.0.1:0` port, driven by the SHIPPED bin via `CARGO_BIN_EXE_vike-cli`. No
//! credentials, no venue, no external network; the observe key is an obviously-fake `DUMMY-*`
//! string written into a throwaway settings directory — deliberately the STORE and not the
//! environment, so every run re-proves the keyring resolves the store the daemon itself reads.
//!
//! # What each test would have caught
//!
//! - [`strategy_status_renders_the_node_identity_and_mount_rows`] — the whole feature: the
//!   shipped binary, the observe handshake, the wire verb, and a human table carrying the daemon
//!   identity line plus one row per mount (an I10 multi-mount node, so BOTH rows must appear —
//!   a client reading only the identity block would print one).
//! - [`strategy_status_json_is_the_wire_payload`] — the machine surface: `--json` parses and
//!   carries the wire's own field names, so a script written against the node's schema works
//!   against the CLI's output unchanged.
//! - [`an_identity_less_node_is_an_honest_error`] — the server-error path: a node publishing no
//!   identity block answers `Response::Error`, and the CLI must exit 1 carrying the server's own
//!   words, never a fabricated empty table.
//! - [`an_old_node_is_refused_client_side_and_nothing_is_sent`] — the feature-negotiation
//!   refusal, against a SCRIPTED node whose `Welcome.features` lacks `strategy-verbs` (the real
//!   server always advertises it, so an "old node" must be scripted). Two halves, both essential:
//!   the CLI's message is actionable (names the capability, says to upgrade the node), and the
//!   scripted node OBSERVES that no frame follows the handshake — sending one would hand an old
//!   server an undecodable frame and the operator an opaque decode error instead of a diagnosis.
//! - [`with_no_observe_key_anywhere_the_message_names_both_sources`] — the honest failure: exit 1
//!   with a message naming the observe key, both places it was looked for, and the store path.

use std::net::{SocketAddr, TcpListener};
use std::process::Command;
use std::sync::mpsc;
use std::thread;

use vike_run::{build_paper_maker_core, MakerMount, MakerMountConfig};
use vike_tradehub::{publish, server};
use vike_tradehub_client::wire::{WireMountRow, WireNodeIdentity};
use vike_tradehub_client::{
    read_frame, write_frame, NodeKeys, Request, Response, Scope, NODE_PROTO_VERSION,
};

/// The paper mount's symbol. Any string works — nothing resolves it against a venue.
const TOKEN: &str = "CLI_STRATEGY_STATUS_E2E_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the trade e2e's mount).
const RESOLUTION_TS: i64 = 3_000_000_000;

/// Obviously fake: an HMAC secret, so any bytes work as long as both sides agree — and the
/// `DUMMY-` prefix makes it unmistakable in a diff or a process listing that no real credential
/// is involved.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-strategy-status-e2e";

/// The identity block the node under test publishes — what the daemon builds from its profile.
fn identity() -> WireNodeIdentity {
    WireNodeIdentity {
        name: "e2e-hub".to_string(),
        strategy: "spread_maker".to_string(),
        params: "spread=0.01".to_string(),
        live: false,
        build: "vike-tradehub test-build".to_string(),
    }
}

/// Two mount rows — an I10 `[[mounts]]` daemon, so the table must carry BOTH.
fn mounts() -> Vec<WireMountRow> {
    vec![
        WireMountRow {
            strategy: "spread_maker".to_string(),
            params: "venue=polymarket symbol=TOK-A spread=0.01".to_string(),
            live: false,
        },
        WireMountRow {
            strategy: "np".to_string(),
            params: "venue=polymarket symbol=TOK-B edge=0.02".to_string(),
            live: false,
        },
    ]
}

/// Build a PAPER node and serve it on an ephemeral loopback port, observe scope only (no control
/// key, no command sink — this suite drives a READ verb). The returned mount keeps the core alive.
fn spawn_node(
    identity: Option<WireNodeIdentity>,
    mounts: Vec<WireMountRow>,
) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn_with_mounts(mount.handle.snapshot_cell(), identity, mounts);
    let keys = NodeKeys::new(OBSERVE_KEY.as_bytes().to_vec(), Vec::new());
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            None,
            server::ControlLimitsConfig::default(),
            None,
            // No REQ-2 datahub advertisement — this suite exercises the strategy-status read path only.
            None,
        );
    });
    (mount, addr)
}

/// A throwaway `<project>/settings` directory holding a credential store with (only) the observe
/// key — the daemon's own documented configuration, and the resolution path under test.
fn settings_dir_with_store(keys: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut body = String::new();
    for (k, v) in keys {
        body.push_str(&format!("{k}={v}\n"));
    }
    let path = dir.path().join(vike_secrets::SECRETS_FILE);
    std::fs::write(&path, body).expect("write store");
    // 0600 like a real store, so the permission finding does not bury the assertions' transcripts.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod 600");
    }
    dir
}

/// What one `vike-cli strategy-status` run produced.
struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

/// Run the SHIPPED bin against `addr`. The node-key variables are explicitly REMOVED from the
/// child's environment: the configuration under test is "key in the store, nothing exported", and
/// a developer box exporting them would otherwise pass these vacuously.
fn run_cli(addr: &str, settings_dir: &std::path::Path, extra: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["strategy-status", "--node", addr])
        .args(extra)
        .env("VIKE_SETTINGS_DIR", settings_dir)
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY")
        // The two REMOVED risk variables refuse startup when set — cleared so an operator box's
        // stale export cannot fail an unrelated read verb's test.
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .expect("run vike-cli strategy-status");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        ok: out.status.success(),
    }
}

/// The whole feature, through the shipped binary: identity line + BOTH mount rows.
#[test]
fn strategy_status_renders_the_node_identity_and_mount_rows() {
    let (_mount, addr) = spawn_node(Some(identity()), mounts());
    let settings = settings_dir_with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);

    let run = run_cli(&addr.to_string(), settings.path(), &[]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(run.ok, "expected success:\n{transcript}");

    // The daemon identity line: name, mode, build.
    assert!(
        run.stdout.contains("node e2e-hub — paper — build vike-tradehub test-build"),
        "{transcript}"
    );
    assert!(run.stdout.contains("params: spread=0.01"), "{transcript}");
    // One row per mount — both of them, with their own params.
    assert!(run.stdout.contains("spread_maker"), "{transcript}");
    assert!(run.stdout.contains("venue=polymarket symbol=TOK-A spread=0.01"), "{transcript}");
    assert!(run.stdout.contains("venue=polymarket symbol=TOK-B edge=0.02"), "{transcript}");
    // Read-only surface: nothing lands on stderr on the happy path.
    assert!(run.stderr.trim().is_empty(), "{transcript}");
}

/// `--json` is the WIRE payload: it parses, and it carries the wire's own field names.
#[test]
fn strategy_status_json_is_the_wire_payload() {
    let (_mount, addr) = spawn_node(Some(identity()), mounts());
    let settings = settings_dir_with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);

    let run = run_cli(&addr.to_string(), settings.path(), &["--json"]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(run.ok, "expected success:\n{transcript}");

    let v: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{transcript}"));
    assert_eq!(v["identity"]["name"], "e2e-hub", "{transcript}");
    assert_eq!(v["identity"]["live"], false, "{transcript}");
    assert_eq!(v["effective_params"], "spread=0.01", "{transcript}");
    let rows = v["mounts"].as_array().expect("mounts is an array");
    assert_eq!(rows.len(), 2, "{transcript}");
    assert_eq!(rows[1]["strategy"], "np", "{transcript}");
}

/// A node publishing no identity block answers `Response::Error`; the CLI exits 1 carrying the
/// server's own words — never a fabricated empty table.
#[test]
fn an_identity_less_node_is_an_honest_error() {
    let (_mount, addr) = spawn_node(None, Vec::new());
    let settings = settings_dir_with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);

    let run = run_cli(&addr.to_string(), settings.path(), &[]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(!run.ok, "must fail:\n{transcript}");
    assert!(run.stderr.contains("publishes no identity block"), "{transcript}");
}

/// A SCRIPTED "old node": completes the handshake but advertises no `strategy-verbs` capability.
/// The real server cannot be configured this way (it always advertises what it serves), which is
/// the point — this is what EVERY node looked like before B4 shipped.
///
/// The channel carries the scripted node's own observation: whether anything arrived after the
/// handshake. `Ok(true)` = the connection closed with NOTHING sent (the contract); `Ok(false)` = a
/// frame arrived (the client sent a verb it was told the server cannot decode); `Err` = the script
/// itself desynced, with the step named.
fn spawn_old_node() -> (SocketAddr, mpsc::Receiver<Result<bool, String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let verdict = (|| -> Result<bool, String> {
            let (mut stream, _) = listener.accept().map_err(|e| format!("accept: {e}"))?;
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Hello { .. }) => {}
                other => return Err(format!("expected Hello, got {other:?}")),
            }
            write_frame(
                &mut stream,
                &Response::Welcome {
                    proto_version: NODE_PROTO_VERSION,
                    nonce: [7u8; 32],
                    // The pre-B4 world: reads and pushes, no strategy verbs.
                    features: vec!["subscribe".to_string()],
                },
            )
            .map_err(|e| format!("write Welcome: {e}"))?;
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Auth { .. }) => {}
                other => return Err(format!("expected Auth, got {other:?}")),
            }
            // A scripted double: accept the mac unseen — the client under test signs honestly,
            // and what this node exists to observe is the NEXT frame, not the auth.
            write_frame(&mut stream, &Response::AuthOk { scope: Scope::Observe })
                .map_err(|e| format!("write AuthOk: {e}"))?;
            // The contract under test: the client refuses CLIENT-SIDE, so the next read must be
            // the connection closing, never a frame.
            Ok(read_frame::<_, Request>(&mut stream).is_err())
        })();
        let _ = tx.send(verdict);
    });
    (addr, rx)
}

/// The feature-negotiation refusal, both halves: an actionable message at the CLI, and the
/// scripted node's own proof that nothing was sent after the handshake.
#[test]
fn an_old_node_is_refused_client_side_and_nothing_is_sent() {
    let (addr, rx) = spawn_old_node();
    let settings = settings_dir_with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);

    let run = run_cli(&addr.to_string(), settings.path(), &[]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(!run.ok, "must fail:\n{transcript}");
    // The client's own diagnosis names the missing capability…
    assert!(run.stderr.contains("strategy-verbs"), "{transcript}");
    assert!(run.stderr.contains("older vike-tradehub"), "{transcript}");
    // …and the CLI adds the action that fixes it.
    assert!(run.stderr.contains("upgrade the node at"), "{transcript}");

    let nothing_sent = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the scripted node reported")
        .unwrap_or_else(|e| panic!("the scripted node desynced: {e}\n{transcript}"));
    assert!(
        nothing_sent,
        "a frame arrived at the old node after the handshake — the refusal must be client-side, \
         with nothing sent\n{transcript}"
    );
}

/// No observe key anywhere: exit 1 with a message naming the key, both sources, and the store
/// path — the operator is pointed at where to put one, not told "nothing to do".
#[test]
fn with_no_observe_key_anywhere_the_message_names_both_sources() {
    // A store that EXISTS but holds no node key — the "present — but…" arm of the message.
    let settings = settings_dir_with_store(&[("SOME_OTHER_KEY", "not-a-node-key")]);

    let run = run_cli("127.0.0.1:1", settings.path(), &[]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(!run.ok, "must fail:\n{transcript}");
    assert!(run.stderr.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{transcript}");
    assert!(run.stderr.contains("process environment"), "{transcript}");
    assert!(run.stderr.to_lowercase().contains("secrets.env"), "must name the store: {transcript}");
    // No connection is attempted without a key, so nothing about the (dead) address is reported.
    assert!(!run.stderr.contains("127.0.0.1:1"), "{transcript}");
}
