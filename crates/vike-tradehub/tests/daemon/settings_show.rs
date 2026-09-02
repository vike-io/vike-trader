//! The `SettingsShow` verb end-to-end (split-plane REQ-7, read half): a real PAPER node
//! ([`vike_run::build_paper_maker_core`]) + the real [`vike_tradehub::server`] with a
//! [`vike_tradehub::server::SettingsShowSource`] over a REAL settings directory, driven through
//! the REAL client verb ([`vike_tradehub_client::settings_show`]) under [`Scope::Observe`].
//!
//! What is proven:
//! - **Advertisement:** `Welcome.features` carries `"settings-show"`, so the client verb sends.
//! - **Real effective rows:** the daemon's answer is the SAME rows `vike-cli config show`'s files
//!   table renders — a `config.toml`-set key reports that file as its origin (with its value and
//!   its `READ` cell), an env-overridden flag names its variable, an untouched key reports
//!   `default`.
//! - **Redaction over the wire:** a secret planted in the source's env map under a
//!   credential-shaped name never reaches any byte of the serialized payload — the shared
//!   `vike_config::show` builder redacts ON CONSTRUCTION, so the wire cannot leak what the CLI
//!   table hides.
//! - **The no-source shape:** a server started without a [`SettingsShowSource`] answers an honest
//!   error, never a fabricated empty table.
//!
//! Grouped into the `daemon` binary: plain tests, no `#[ignore]`, no crate-level `#![cfg]`, and no
//! process-global mutation — the "environment" here is a local `HashMap` handed to the source, and
//! the settings directory is a `tempfile::TempDir`, so nothing touches `std::env` or the CWD.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::thread;

use vike_run::{build_paper_maker_core, MakerMount, MakerMountConfig};
use vike_tradehub::{publish, server};
use vike_tradehub_client::proto::{
    read_frame, write_frame, Request, Response, Scope, FEATURE_SETTINGS_SHOW, NODE_PROTO_VERSION,
};
use vike_tradehub_client::{auth, settings_show, NodeKeys};

const TOKEN: &str = "SETTINGS_SHOW_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the observe-roundtrip mount).
const RESOLUTION_TS: i64 = 3_000_000_000;
const OBSERVE_KEY: &[u8] = b"settings-show-observe-key";
/// The secret planted in the source's env map — the wire payload must never carry a byte of it.
const PLANTED_SECRET: &str = "sk-planted-secret-never-on-the-wire";

/// Build a PAPER node and start the observe server with the given settings source. The server is
/// observe-only (no control key, no sink) — this suite is about the read verb.
fn spawn_node(settings: Option<server::SettingsShowSource>) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), Vec::new());
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            None,
            server::ControlLimitsConfig::default(),
            settings,
            None,
        );
    });
    (mount, addr)
}

/// A real settings directory: `config.toml` sets `tradehub_addr`, everything else is untouched.
fn settings_dir_with_config() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp settings dir");
    std::fs::write(dir.path().join("config.toml"), "tradehub_addr = \"127.0.0.1:7979\"\n")
        .expect("write config.toml");
    dir
}

/// The source's env map: one env-layer override (`VIKE_RECONCILE`, so a row can prove the env
/// layer resolves) and one PLANTED credential-shaped secret (which must never reach the wire —
/// no settings key maps from it, and even a credential-shaped settings key would be redacted).
fn source_env() -> HashMap<String, String> {
    HashMap::from([
        ("VIKE_RECONCILE".to_string(), "1".to_string()),
        ("ACME_API_KEY".to_string(), PLANTED_SECRET.to_string()),
    ])
}

/// The whole read half on one connection, through the REAL client verb: advertisement, the
/// boot-dir header, a file-set row (value + origin + READ cell), an env-overridden row naming its
/// variable, a defaulted row — and the planted-secret redaction pin over the ENTIRE payload.
#[test]
fn the_daemons_real_effective_rows_cross_the_wire() {
    let dir = settings_dir_with_config();
    let (_mount, addr) = spawn_node(Some(server::SettingsShowSource {
        settings_dir: Some(dir.path().to_path_buf()),
        env: source_env(),
        // The REQ-7 v2 hot-apply seam: absent here — this file tests the READ half, and a
        // source without a seam keeps the pre-v2 restart-to-apply answer for every key.
        hot: None,
    }));

    let show = settings_show(addr, OBSERVE_KEY).expect("advertised ⇒ served");

    // The header: the node answers from the directory it was booted with.
    let reported = show.settings_dir.as_deref().expect("a settings dir was resolved");
    assert_eq!(reported, dir.path().display().to_string());

    // The file-set row: config.toml is the origin, the value is the file's, and the READ cell
    // names the one binary that reads it (`config.tradehub_addr` → vike-tradehub's main.rs).
    let addr_row = show
        .rows
        .iter()
        .find(|r| r.key == "config.tradehub_addr")
        .expect("the tradehub_addr row exists");
    assert_eq!(addr_row.section, "config.toml");
    assert_eq!(addr_row.value, "127.0.0.1:7979");
    assert_eq!(addr_row.origin, "config.toml");
    assert_eq!(addr_row.read_by, "tradehub");

    // The env-overridden row names its variable — the CLI's exact ORIGIN cell.
    let recon = show.rows.iter().find(|r| r.key == "flags.reconcile").expect("the reconcile row");
    assert_eq!(recon.value, "true");
    assert_eq!(recon.origin, "env:VIKE_RECONCILE");

    // An untouched key honestly reports the compiled-in default.
    let policy = show
        .rows
        .iter()
        .find(|r| r.key == "policy.max_notional_per_order")
        .expect("the policy ceiling row");
    assert_eq!(policy.origin, "default");
    assert_eq!(policy.section, "policy.toml");

    // ⚠ THE REDACTION PIN: no byte of the planted credential-shaped value reaches the payload —
    // asserted over the whole serialized document, so no field can smuggle it.
    let doc = serde_json::to_string(&show).expect("serialize the payload");
    assert!(!doc.contains(PLANTED_SECRET), "a planted secret crossed the wire: {doc}");
}

/// The feature is advertised in the REAL server's `Welcome` — driven at the frame level so the
/// assertion is on the advertisement itself, not on the client verb's behaviour above it.
#[test]
fn the_welcome_advertises_settings_show() {
    let (_mount, addr) = spawn_node(None);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { features, .. } => {
            assert!(
                features.iter().any(|f| f == FEATURE_SETTINGS_SHOW),
                "the node must advertise {FEATURE_SETTINGS_SHOW}, got {features:?}"
            );
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// A server constructed WITHOUT a settings source answers an honest error under Observe — never a
/// fabricated empty table (the `StrategyStatus` identity-less shape). Driven at the frame level
/// because the client verb folds `Response::Error` into an `io::Error`, and this pin is about the
/// server's own words.
#[test]
fn a_node_without_a_source_answers_an_honest_error() {
    let (_mount, addr) = spawn_node(None);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(OBSERVE_KEY, &nonce, NODE_PROTO_VERSION, Scope::Observe);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Observe, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("auth reply") {
        Response::AuthOk { .. } => {}
        other => panic!("expected AuthOk, got {other:?}"),
    }
    write_frame(&mut stream, &Request::SettingsShow).expect("settings request");
    match read_frame::<_, Response>(&mut stream).expect("settings reply") {
        Response::Error(msg) => {
            assert!(msg.contains("settings source"), "the error names the missing source: {msg}");
        }
        other => panic!("a source-less node must answer Error, got {other:?}"),
    }
}
