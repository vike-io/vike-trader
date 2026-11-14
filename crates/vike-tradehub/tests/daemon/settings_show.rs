//! The `SettingsShow` verb end-to-end (split-plane REQ-7, read half): a real PAPER node
//! ([`vike_tradehub_core::test_support::paper_maker`]) + the real [`vike_tradehub::server`] with a
//! [`vike_tradehub::server::settings::SettingsShowSource`] over a REAL settings directory, driven through
//! the REAL client verb ([`vike_tradehub_client::settings_show`]) under [`Scope::Read`].
//!
//! What is proven:
//! - **Advertisement:** `Welcome.features` carries `"settings-show"`, so the client verb sends.
//! - **Real effective rows:** the daemon's answer is the SAME rows `vike-cli config show`'s
//!   settings table renders — a row-set key reports `db` as its origin (with its value and its
//!   `READ` cell), a row-set flag does too, an untouched key reports `default`.
//! - **The no-source shape:** a server started without a [`SettingsShowSource`] answers an honest
//!   error, never a fabricated empty table.
//!
//! Grouped into the `daemon` binary: plain tests, no `#[ignore]`, no crate-level `#![cfg]`, and no
//! process-global mutation — the settings directory is a `tempfile::TempDir`, so nothing touches
//! `std::env` or the CWD.

use vike_hub_protocol::auth::Scope;
use vike_tradehub::server;
use vike_tradehub_client::proto::{
    FEATURE_SETTINGS_SHOW, NODE_PROTO_VERSION, Request, Response, read_frame, write_frame,
};
use vike_tradehub_client::{auth, settings_show};

use crate::support::{settings_rows, spawn_maker_hub};

const TOKEN: &str = "SETTINGS_SHOW_TOKEN";
const OBSERVE_KEY: &[u8] = b"settings-show-observe-key";

/// A real settings directory: a `config.tradehub_addr` row and a `flags.reconcile` row are
/// planted, everything else is untouched.
fn settings_dir_with_config() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp settings dir");
    // ONE plant: `plant_settings_rows` replaces the whole table.
    let mut rows = settings_rows("config", &[("tradehub_addr", "\"127.0.0.1:7979\"")]);
    rows.settings.extend(settings_rows("flags", &[("reconcile", "true")]).settings);
    vike_secrets::plant_settings_rows(dir.path(), &rows).expect("plant the fixture's rows");
    dir
}

/// The whole read half on one connection, through the REAL client verb: advertisement, the
/// boot-dir header, a row-set row (value + origin + READ cell), a row-set flag and a defaulted row.
#[test]
fn the_daemons_real_effective_rows_cross_the_wire() {
    let dir = settings_dir_with_config();
    let settings = server::settings::SettingsShowSource {
        settings_dir: Some(dir.path().to_path_buf()),
        // The REQ-7 v2 hot-apply seam: absent here — this file tests the READ half, and a
        // source without a seam keeps the pre-v2 restart-to-apply answer for every key.
        hot: None,
        journal: None,
    };
    let (_mount, addr) = spawn_maker_hub(TOKEN, OBSERVE_KEY, None, Some(settings));

    let show = settings_show(addr, OBSERVE_KEY).expect("advertised ⇒ served");

    // The header: the node answers from the directory it was booted with.
    let reported = show.settings_dir.as_deref().expect("a settings dir was resolved");
    assert_eq!(reported, dir.path().display().to_string());

    // The row-set row: `config` is the section, the value is the row's, and the READ cell
    // names the one binary that reads it (`config.tradehub_addr` → vike-tradehub's main.rs).
    let address_row = show
        .rows
        .iter()
        .find(|r| r.key == "config.tradehub_addr")
        .expect("the tradehub_addr row exists");
    assert_eq!(address_row.section, "config");
    assert_eq!(address_row.value, "127.0.0.1:7979");
    assert_eq!(address_row.origin, "db");
    assert_eq!(address_row.read_by, "tradehub");

    // The row-set flag reports its row — the CLI's exact ORIGIN cell.
    let recon = show.rows.iter().find(|r| r.key == "flags.reconcile").expect("the reconcile row");
    assert_eq!(recon.value, "true");
    assert_eq!(recon.origin, "db");

    // An untouched key honestly reports the compiled-in default.
    let policy = show
        .rows
        .iter()
        .find(|r| r.key == "policy.max_notional_per_order")
        .expect("the policy ceiling row");
    assert_eq!(policy.origin, "default");
    assert_eq!(policy.section, "policy");
    assert_eq!(show.refusal, None, "a store that reads refuses nothing");
}

/// **A store that will not read, or whose rows the loader refuses, is said beside the rows.** Those rows
/// are then every key's compiled-in default, `origin: default`, a ceiling reading as absent, and a peer
/// with only the rows would take that for what the box holds. Both of the loader's marks, through the
/// real client verb.
#[test]
fn the_rows_of_a_store_the_node_does_not_trust_carry_the_refusal() {
    let answer = |dir: &std::path::Path| {
        let settings = server::settings::SettingsShowSource {
            settings_dir: Some(dir.to_path_buf()),
            hot: None,
            journal: None,
        };
        let (_mount, addr) = spawn_maker_hub(TOKEN, OBSERVE_KEY, None, Some(settings));
        settings_show(addr, OBSERVE_KEY).expect("advertised ⇒ served")
    };

    let unreadable = tempfile::tempdir().expect("temp settings dir");
    let db = vike_secrets::db_path_in(unreadable.path());
    std::fs::create_dir_all(db.parent().expect("the database has a directory")).expect("mkdir");
    std::fs::write(&db, "this is not a database, and it is long enough to be read as one")
        .expect("plant the file");
    let show = answer(unreadable.path());
    let why = show.refusal.expect("a store that will not read is said");
    assert!(why.contains("could not be read"), "{why}");

    let refused = tempfile::tempdir().expect("temp settings dir");
    vike_secrets::plant_settings_rows(
        refused.path(),
        &settings_rows("config", &[("no_such_key", "1")]),
    )
    .expect("plant the fixture's rows");
    let show = answer(refused.path());
    let why = show.refusal.expect("a row the loader refuses is said");
    assert!(why.contains("no_such_key"), "{why}");
}

/// The feature is advertised in the REAL server's `Welcome` — driven at the frame level so the
/// assertion is on the advertisement itself, not on the client verb's behaviour above it.
#[test]
fn the_welcome_advertises_settings_show() {
    let (_mount, addr) = spawn_maker_hub(TOKEN, OBSERVE_KEY, None, None);
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
    let (_mount, addr) = spawn_maker_hub(TOKEN, OBSERVE_KEY, None, None);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(OBSERVE_KEY, &nonce, NODE_PROTO_VERSION, Scope::Read);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Read, mac }).expect("auth");
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
