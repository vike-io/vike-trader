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
    let [venue, tier, channel, protocol, url] = words.map(str::to_string);
    SetCommand { venue, tier, channel, protocol, url, product: String::new(), note: None }
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

/// Plain http to a host off this box is refused with exit 2 and the row's name, and nothing is written
/// or journalled; a loopback host may take it (the owner's G6 yes, 2026-11-22).
#[test]
fn set_refuses_plain_http_off_loopback_and_writes_nothing() {
    let dir = store();
    let refused = set(
        &set_command(["bybit", "live", "all", "rest", "http://api.bybit.com"]),
        ctx(dir.path()),
    )
    .expect_err("plain off the box");
    assert_eq!(refused.exit, crate::exit::Exit::Usage);
    let words = &refused.message;
    assert!(words.starts_with("venue_url.bybit.live.all.rest: "), "{words}");
    assert!(words.contains("https://") && words.ends_with("nothing was written"), "{words}");
    let rows = read_venue_urls(dir.path()).expect("read");
    assert!(rows.iter().all(|r| r.source == UrlSource::Default), "nothing was written");
    set(&set_command(["bybit", "live", "all", "rest", "http://127.0.0.1:8080"]), ctx(dir.path()))
        .expect("a loopback host may be plain");
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
    unset(
        &UnsetCommand {
            venue: "bybit".into(),
            tier: "demo".into(),
            channel: "user".into(),
            protocol: "ws".into(),
            product: String::new(),
        },
        ctx(dir.path()),
    )
    .expect("unset");
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
        &[(
            key.clone(),
            "https://www.okx.com".into(),
            probe(Some(false), "connect failed: refused"),
        )],
    )
    .expect("record");
    let warned = health_check(Some(dir.path())).expect("a row");
    assert_eq!(warned.status, Status::Warn);
    assert!(warned.detail.starts_with("1 of 1 in use are down"), "{}", warned.detail);
    assert_eq!(warned.items, ["okx live all rest https://www.okx.com: connect failed: refused"]);
    // A 429 or a timeout is a refusal FOR NOW (a venue's pacing), worded so and never as down.
    let timed_out = format!(
        "{}: connect timed out (connection timed out)",
        vike_secrets::venue_url::PROBE_REFUSED_FOR_NOW
    );
    record_venue_url_probes(
        dir.path(),
        &[(key, "https://www.okx.com".into(), probe(Some(false), &timed_out))],
    )
    .expect("record");
    let limited = health_check(Some(dir.path())).expect("a row");
    assert_eq!(limited.status, Status::Warn);
    assert!(
        limited.detail.starts_with("1 of 1 in use are rate-limited or temporarily refused"),
        "{}",
        limited.detail
    );
    assert_eq!(
        limited.detail,
        "1 of 1 in use are rate-limited or temporarily refused (a 429 or a timeout: the venue may \
         be pacing this box; not a dead endpoint); 1 not in use; `vike-cli config url probes`",
        "a refusal FOR NOW, never worded as down"
    );
}

/// `reset` and `check` are retired with no alias: each refuses by its id, naming the word that replaced
/// it, and the new word parses.
#[test]
fn reset_and_check_refuse_naming_unset_and_probes() {
    use crate::cmd::config::{Sub, parse};
    use crate::refusals::RetiredConfigSpelling;
    let argv = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
    for (was, now, refusal) in [
        ("reset", "unset", RetiredConfigSpelling::UrlReset),
        ("check", "probes", RetiredConfigSpelling::UrlCheck),
    ] {
        let said = parse(argv(&["url", was, "bybit", "demo", "user", "ws"])).expect_err(was);
        assert!(said.contains(&format!("error [{}]", refusal.id().word())), "{}", &*said);
        assert!(said.contains(&format!("is now `config url {now}`")), "{}", &*said);
    }
    let unset = parse(argv(&["url", "unset", "bybit", "demo", "user", "ws"]));
    assert!(matches!(unset, Ok(Sub::Url { verb: UrlVerb::Unset(_) })), "{unset:?}");
    let probes = parse(argv(&["url", "probes", "--venue", "bybit"]));
    assert!(matches!(probes, Ok(Sub::Url { verb: UrlVerb::Probes(_) })), "{probes:?}");
}
