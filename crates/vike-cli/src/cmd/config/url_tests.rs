//! `config url` over a real store in a temp directory: a write is refused on its shape and by the
//! demo guard, an operator row shows its release default, and the `trade health` row warns only on
//! a failed probe of a row in use.

use vike_client::health::Status;
use vike_secrets::venue_url::{UrlProbe, record_venue_url_probes, seed_venue_urls_in};

use super::*;

fn store() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp");
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path()))
        .expect("store");
    dir
}

fn set_command(words: [&str; 5]) -> SetCommand {
    let [venue, ty, channel, protocol, url] = words.map(str::to_string);
    SetCommand { venue, ty, channel, protocol, url, product: String::new(), note: None }
}

fn ctx(dir: &Path) -> Context<'_> {
    Context { settings_dir: Some(dir), now_ms: 1 }
}

#[test]
fn a_word_outside_the_vocabulary_or_an_unknown_venue_is_a_usage_error() {
    for bad in [
        ["nosuchvenue", "live", "all", "rest"],
        ["bybit", "testnet", "all", "rest"],
        ["bybit", "live", "orders", "rest"],
        ["bybit", "live", "all", "grpc"],
    ] {
        let refused = key_of(bad[0], bad[1], bad[2], bad[3], "").expect_err("refused");
        assert_eq!(refused.exit, crate::exit::Exit::Usage, "{bad:?}");
    }
    assert!(key_of("tardis", "live", "history", "rest", "").is_ok(), "a vendor has an address");
}

#[test]
fn set_refuses_a_demo_trade_address_equal_to_live_and_writes_nothing() {
    let dir = store();
    let refused = set(
        &set_command(["bybit", "demo", "trade", "rest", "https://api.bybit.com"]),
        ctx(dir.path()),
    )
    .expect_err("the guard");
    assert!(refused.message.contains("LIVE"), "{}", refused.message);
    let rows = read_venue_urls(dir.path()).expect("read");
    assert!(rows.iter().all(|r| r.source == UrlSource::Default), "nothing was written");
}

#[test]
fn set_refuses_a_bad_shape_and_writes_an_operator_row_that_ls_shows_with_its_default() {
    let dir = store();
    assert!(
        set(&set_command(["bybit", "demo", "user", "ws", "https://x"]), ctx(dir.path())).is_err()
    );
    set(
        &set_command(["bybit", "demo", "user", "ws", "wss://stream-demo2.bybit.com"]),
        ctx(dir.path()),
    )
    .expect("set");
    let shown = ls(&LsCommand { json: false, venue: Some("bybit".into()) }, Some(dir.path()))
        .expect("ls")
        .join("\n");
    assert!(
        shown.contains("wss://stream-demo2.bybit.com  [operator]  release default now: wss://stream-demo.bybit.com"),
        "{shown}"
    );
    reset(
        &ResetCommand {
            venue: "bybit".into(),
            ty: "demo".into(),
            channel: "user".into(),
            protocol: "ws".into(),
            product: String::new(),
        },
        ctx(dir.path()),
    )
    .expect("reset");
    let json =
        ls(&LsCommand { json: true, venue: Some("bybit".into()) }, Some(dir.path())).expect("ls");
    let doc: serde_json::Value = serde_json::from_str(&json[0]).expect("json");
    let demo_websocket = doc
        .as_array()
        .expect("an array")
        .iter()
        .find(|r| r["type"] == "demo" && r["protocol"] == "ws")
        .expect("the row");
    assert_eq!(demo_websocket["url"], "wss://stream-demo.bybit.com");
    assert_eq!(demo_websocket["source"], "default");
    assert!(demo_websocket["release_default"].is_null());
}

#[test]
fn the_health_row_is_absent_before_a_probe_and_warns_on_a_failed_one() {
    let dir = store();
    assert!(health_check(Some(dir.path())).is_none(), "nothing probed: no row");
    seed_venue_urls_in(dir.path()).expect("seed");
    let key = UrlKey {
        venue: "okx".into(),
        ty: UrlType::Live,
        channel: UrlChannel::All,
        protocol: UrlProtocol::Rest,
        product: String::new(),
    };
    let probe = |ok, words: &str| UrlProbe { at_ms: 1, ok, words: words.into() };
    // A row no active account dials was probed and found nothing, and is never down.
    let unused = UrlKey {
        venue: "aster".into(),
        ty: UrlType::Demo,
        channel: UrlChannel::User,
        protocol: UrlProtocol::Websocket,
        product: "spot".into(),
    };
    let not_in_use = probe(None, "not in use (no active aster demo account); no answer: timeout");
    record_venue_url_probes(
        dir.path(),
        &[
            (key.clone(), "https://www.okx.com".into(), probe(Some(true), "TLS ok")),
            (unused, "wss://sstream.asterdex-testnet.com".into(), not_in_use),
        ],
    )
    .expect("record");
    let passed = health_check(Some(dir.path())).expect("a row");
    assert_eq!(passed.status, Status::Pass);
    assert_eq!(passed.detail, "1 in use probed, every one answered; 1 not in use");
    record_venue_url_probes(
        dir.path(),
        &[(key, "https://www.okx.com".into(), probe(Some(false), "connect failed: refused"))],
    )
    .expect("record");
    let warned = health_check(Some(dir.path())).expect("a row");
    assert_eq!(warned.status, Status::Warn);
    assert!(warned.detail.starts_with("1 of 1 in use are down"), "{}", warned.detail);
    assert_eq!(warned.items, ["okx live all rest https://www.okx.com: connect failed: refused"]);
}
