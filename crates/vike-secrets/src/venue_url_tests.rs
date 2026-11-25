//! `venue_url` against a real store in a temp directory: the seed lands whole, a second seeding
//! writes nothing, an operator row survives every seeding, a reset goes back to the seed, and a box
//! with no store or no table reads the seed.

use super::*;
use crate::store_locator::db_path_in;
use vike_change_journal::{ChangeActor, ChangeProcess};

fn store() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp");
    crate::create_empty_store_for_test(&db_path_in(dir.path())).expect("a store");
    dir
}

fn journal() -> crate::ChangeStamp {
    crate::ChangeStamp {
        actor: ChangeActor::cli("test"),
        process: ChangeProcess::new("test", 1, "0.0.0"),
        now_ms: 1,
    }
}

fn key(
    venue: &str,
    ty: UrlType,
    channel: UrlChannel,
    protocol: UrlProtocol,
    product: &str,
) -> UrlKey {
    UrlKey { venue: venue.into(), ty, channel, protocol, product: product.into() }
}

fn okx_demo_websocket() -> UrlKey {
    key("okx", UrlType::Demo, UrlChannel::User, UrlProtocol::Websocket, "")
}

fn sql(dir: &Path, statement: &str) {
    rusqlite::Connection::open(db_path_in(dir))
        .expect("open")
        .execute_batch(statement)
        .expect("sql");
}

#[test]
fn no_store_and_no_table_both_read_the_seed() {
    let empty = tempfile::tempdir().expect("temp");
    assert_eq!(read_venue_urls(empty.path()).expect("read"), seed_rows());
    let dir = store();
    assert_eq!(read_venue_urls(dir.path()).expect("read"), seed_rows(), "no table yet");
    assert_eq!(seed_venue_urls_in(empty.path()).expect("seed"), Seeded::default());
    assert!(
        !database_present(&db_path_in(empty.path())).expect("stat"),
        "a seeding never creates a store"
    );
}

#[test]
fn the_seed_lands_whole_and_a_second_seeding_writes_nothing() {
    let dir = store();
    let first = seed_venue_urls_in(dir.path()).expect("seed");
    assert_eq!(first, Seeded { inserted: VENUE_URL_SEED.len(), updated: 0 });
    assert_eq!(read_venue_urls(dir.path()).expect("read"), seed_rows());
    assert_eq!(seed_venue_urls_in(dir.path()).expect("seed"), Seeded::default());
}

#[test]
fn a_moved_default_is_updated_and_an_operator_row_is_never_touched() {
    let dir = store();
    seed_venue_urls_in(dir.path()).expect("seed");
    // A row an older release seeded with another address, and one the operator wrote.
    sql(
        dir.path(),
        "UPDATE venue_url SET url = 'wss://old.example' WHERE venue = 'okx' AND type = 'demo' AND channel = 'user' AND product = '';
         UPDATE venue_url SET url = 'https://mine.example', source = 'operator' WHERE venue = 'bybit' AND type = 'live' AND protocol = 'rest';",
    );
    assert_eq!(seed_venue_urls_in(dir.path()).expect("seed"), Seeded { inserted: 0, updated: 1 });
    let rows = read_venue_urls(dir.path()).expect("read");
    let url_of = |k: &UrlKey| rows.iter().find(|r| r.key == *k).map(|r| r.url.clone());
    assert_eq!(url_of(&okx_demo_websocket()).as_deref(), Some("wss://wspap.okx.com:8443"));
    let bybit = key("bybit", UrlType::Live, UrlChannel::All, UrlProtocol::Rest, "");
    assert_eq!(url_of(&bybit).as_deref(), Some("https://mine.example"), "an operator row stays");
}

#[test]
fn set_writes_an_operator_row_and_reset_goes_back_to_the_seed() {
    let dir = store();
    let k = okx_demo_websocket();
    let (previous, journal_error) =
        set_venue_url_in(dir.path(), &k, "wss://wspap2.okx.com:8443", Some("moved"), journal())
            .expect("set");
    assert_eq!(previous.as_deref(), Some("wss://wspap.okx.com:8443"), "the table was seeded first");
    assert!(journal_error.is_none());
    let row = |dir: &Path| read_venue_urls(dir).expect("read").into_iter().find(|r| r.key == k);
    let set = row(dir.path()).expect("the row");
    assert_eq!(
        (set.url.as_str(), set.source, set.note.as_str()),
        ("wss://wspap2.okx.com:8443", UrlSource::Operator, "moved")
    );
    seed_venue_urls_in(dir.path()).expect("seed");
    assert_eq!(
        row(dir.path()).expect("the row").url,
        "wss://wspap2.okx.com:8443",
        "a seeding keeps it"
    );

    reset_venue_url_in(dir.path(), &k, journal()).expect("reset");
    let reset = row(dir.path()).expect("the row");
    assert_eq!(
        (reset.url.as_str(), reset.source),
        ("wss://wspap.okx.com:8443", UrlSource::Default)
    );
}

#[test]
fn a_reset_of_an_added_key_deletes_it() {
    let dir = store();
    let added = key("okx", UrlType::Demo, UrlChannel::Data, UrlProtocol::Websocket, "");
    set_venue_url_in(dir.path(), &added, "wss://ws.okx.com:8443", None, journal()).expect("set");
    reset_venue_url_in(dir.path(), &added, journal()).expect("reset");
    assert!(!read_venue_urls(dir.path()).expect("read").iter().any(|r| r.key == added));
}

#[test]
fn the_operator_writers_refuse_a_box_with_no_store() {
    let empty = tempfile::tempdir().expect("temp");
    let refused =
        set_venue_url_in(empty.path(), &okx_demo_websocket(), "wss://x.example", None, journal());
    assert!(matches!(refused.map_err(|e| e.0.kind), Err(StoreErrorKind::NoDatabase)));
    assert!(!database_present(&db_path_in(empty.path())).expect("stat"));
}

#[test]
fn a_probe_is_recorded_only_against_the_url_it_probed_and_a_moved_url_forgets_it() {
    let dir = store();
    seed_venue_urls_in(dir.path()).expect("seed");
    let k = okx_demo_websocket();
    let probe = UrlProbe { at_ms: 7, ok: Some(false), words: "404".into() };
    record_venue_url_probes(
        dir.path(),
        &[
            (k.clone(), "wss://wspap.okx.com:8443".into(), probe.clone()),
            (k.clone(), "wss://stale".into(), probe.clone()),
        ],
    )
    .expect("record");
    let row = |dir: &Path| {
        read_venue_urls(dir).expect("read").into_iter().find(|r| r.key == k).expect("row")
    };
    assert_eq!(row(dir.path()).probe, Some(probe));
    set_venue_url_in(dir.path(), &k, "wss://wspap2.okx.com:8443", None, journal()).expect("set");
    assert_eq!(row(dir.path()).probe, None, "a new url has not been probed");
}

/// A row no active account dials is recorded NOT IN USE (`ok: None`), and reads back as that, never as
/// down: the reader once turned every probe that was not `1` into `false`.
#[test]
fn a_probe_of_a_row_not_in_use_reads_back_not_in_use() {
    let dir = store();
    seed_venue_urls_in(dir.path()).expect("seed");
    let k = okx_demo_websocket();
    let probe = UrlProbe {
        at_ms: 7,
        ok: None,
        words: "not in use (no active okx demo account); TLS ok".into(),
    };
    record_venue_url_probes(
        dir.path(),
        &[(k.clone(), "wss://wspap.okx.com:8443".into(), probe.clone())],
    )
    .expect("record");
    let read = read_venue_urls(dir.path()).expect("read").into_iter().find(|r| r.key == k);
    assert_eq!(read.and_then(|r| r.probe), Some(probe));
}

#[test]
fn the_guard_refuses_a_demo_address_that_equals_live_and_a_bad_shape() {
    let rows = seed_rows();
    let demo_trade = key("bybit", UrlType::Demo, UrlChannel::Trade, UrlProtocol::Rest, "");
    assert!(
        refusal_of(&rows, &demo_trade, "https://api.bybit.com").is_some_and(|w| w.contains("LIVE"))
    );
    assert!(refusal_of(&rows, &demo_trade, "https://api-demo2.bybit.com").is_none());
    assert!(refusal_of(&rows, &demo_trade, "wss://api-demo.bybit.com").is_some(), "shape");
    // A LIVE write that makes an existing demo row equal to live is refused too.
    let live_rest = key("bybit", UrlType::Live, UrlChannel::All, UrlProtocol::Rest, "");
    assert!(refusal_of(&rows, &live_rest, "https://api-demo.bybit.com").is_some());
    // okx shares its REST host by design.
    let okx = key("okx", UrlType::Demo, UrlChannel::Trade, UrlProtocol::Rest, "");
    assert!(refusal_of(&rows, &okx, "https://www.okx.com").is_none());
    // Plain http is a loopback host's alone (the owner's G6 yes, 2026-11-22).
    assert!(
        refusal_of(&rows, &live_rest, "http://api.bybit.com")
            .is_some_and(|w| w.contains("https://"))
    );
    assert!(refusal_of(&rows, &live_rest, "http://127.0.0.1:8080").is_none());
}

#[test]
fn load_venue_settings_hands_every_venue_its_addresses() {
    let empty = tempfile::tempdir().expect("temp");
    let all = crate::venue_setting::load_venue_settings(empty.path()).expect("load");
    let binance = all.get("binance").expect("a venue with no setting row has an entry");
    assert_eq!(
        binance.urls().url(UrlType::Demo, UrlChannel::Trade, UrlProtocol::Rest, "spot"),
        Ok("https://demo-api.binance.com")
    );
    assert_eq!(all, crate::venue_setting::seeded_venue_settings());
    assert!(all.contains_key("tardis"), "a history vendor is keyed like a venue");
}

#[test]
fn an_empty_table_reads_as_the_seed() {
    let dir = store();
    // A probe record on a store no seeding reached creates the table and writes no row.
    record_venue_url_probes(dir.path(), &[]).expect("record");
    assert_eq!(read_venue_urls(dir.path()).expect("read"), seed_rows());
}
