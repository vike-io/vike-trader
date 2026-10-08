//! **The history-channels read over real loopback sockets** —
//! `docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §4, PR 1's server rows. The
//! pure halves (the builder, the fold, the clamp) are `crates/vike-datahub-client/src/history.rs`'s
//! tests; the credential probe over real stores, and the token that must never reach the reply, are
//! `crates/vike-datahub/src/datahub_cli_tests.rs`'s, where the probe is reachable; the negotiation
//! against an older server is `crates/vike-datahub-client/tests/history_channels_negotiation.rs`.
//!
//! | test | the property |
//! |---|---|
//! | [`every_roster_venue_appears_once_with_the_catalogs_rows`] | the reply is the catalog's table, roster order, one entry per venue |
//! | [`mounted_follows_the_planted_table_and_no_table_reads_false_everywhere`] | the overlay is the server's own table, per (venue, lane) |
//! | [`a_table_whose_collectors_panic_still_answers`] | the read asks the table a lookup and never runs a lane |
//! | [`the_credential_word_is_the_tables_probe_and_never_absent_by_omission`] | `NotChecked` where nothing was read, the probe's word where it was |
//! | [`what_the_store_holds_rides_the_answer`] | `held` is the store's inventory, per kind |
//! | [`an_observe_key_is_served_the_read`] | the scope verdict over the wire: Observe may ask |

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_catalog::{ChannelState, history_channels_for};
use vike_data::{HistStore, MemHistStore};
use vike_datahub::backfill::{BackfillFn, BackfillLane, BackfillTable};
use vike_datahub::{serve_authed, serve_with_backfill};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::history::{CredentialPresence, HistoryChannelsReport};
use vike_model::Bar;
use vike_node_proto::auth::{NodeKeys, Scope};

const OBSERVE_KEY: &[u8] = b"history-observe-key";
const CONTROL_KEY: &[u8] = b"history-control-key";

/// A collector that must never run — the read under test asks the table a lookup and nothing else.
fn must_not_run() -> BackfillFn {
    Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| {
        panic!("the history-channels read ran a collector")
    })
}

/// Serve `store` with `table` on an ephemeral loopback port, key-less.
fn serve(store: Arc<dyn HistStore + Send + Sync>, table: Option<BackfillTable>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        let _ = serve_with_backfill(listener, store, table);
    });
    addr
}

fn empty_store() -> Arc<dyn HistStore + Send + Sync> {
    Arc::new(MemHistStore::new())
}

fn ask(addr: SocketAddr) -> HistoryChannelsReport {
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert!(client.serves_history_channels(), "every build advertises the read");
    client.history_channels().expect("the read is served")
}

/// The row of `venue` named `name` in a reply.
fn row<'r>(
    report: &'r HistoryChannelsReport,
    venue: &str,
    name: &str,
) -> &'r vike_datahub_client::history::ChannelReport {
    report
        .venue(venue)
        .unwrap_or_else(|| panic!("{venue} is not in the reply"))
        .channels
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("{venue} has no row {name}"))
}

/// The planted table: binance's kline lane, binance's funding lane and dukascopy's tick lane — and
/// NO credentialed row, so OANDA's built row must read not mounted.
fn planted_table() -> BackfillTable {
    BackfillTable::new(vec![("binance".to_string(), must_not_run())])
        .with("binance", BackfillLane::Funding, must_not_run())
        .with("dukascopy", BackfillLane::TickBars, must_not_run())
}

/// **The reply is the catalog's table**: every roster venue once, in roster order, each with the
/// rows `vike_catalog::history_channels_for` declares, in the table's order, every rolling window
/// resolved against the server's `as_of_ms`.
#[test]
fn every_roster_venue_appears_once_with_the_catalogs_rows() {
    let report = ask(serve(empty_store(), Some(planted_table())));
    let venues: Vec<&str> = report.venues.iter().map(|v| v.venue.as_str()).collect();
    assert_eq!(venues, vike_model::VENUES.to_vec(), "every roster venue, once, roster order");
    assert!(report.as_of_ms > 1_700_000_000_000, "the server's own clock: {}", report.as_of_ms);
    for venue in &report.venues {
        let rows = history_channels_for(&venue.venue);
        let names: Vec<&str> = venue.channels.iter().map(|c| c.name.as_str()).collect();
        let want: Vec<&str> = rows.iter().map(|r| r.name).collect();
        assert_eq!(names, want, "{}", venue.venue);
        for (r, c) in rows.iter().zip(&venue.channels) {
            assert_eq!(
                c.depth.text,
                r.depth_text(Some(report.as_of_ms)),
                "{}/{}",
                venue.venue,
                r.name
            );
            assert_eq!(c.access.text, r.access_text());
            assert_eq!(c.state.text, r.state_text());
        }
    }
}

/// **`mounted` is the server's own table, per (venue, lane)**: a built row whose lane the planted
/// table carries reads `true`, OANDA's credentialed row — whose lane it does not — reads `false`, a
/// designed row carries no answer at all, and a server with NO table reads `false` on every built
/// row (it still answers: the capability is a build fact).
#[test]
fn mounted_follows_the_planted_table_and_no_table_reads_false_everywhere() {
    let report = ask(serve(empty_store(), Some(planted_table())));
    assert_eq!(row(&report, "binance", "klines (spot and futures)").mounted, Some(true));
    assert_eq!(row(&report, "binance", "funding-rate history").mounted, Some(true));
    assert_eq!(
        row(&report, "dukascopy", "HTTP datafeed, one .bi5 file per instrument-hour").mounted,
        Some(true)
    );
    assert_eq!(row(&report, "bybit", "kline history").mounted, Some(false), "not in the table");
    assert_eq!(row(&report, "oanda", "v20 REST candles").mounted, Some(false), "no oanda row");
    assert_eq!(row(&report, "oanda", "bulk archive").mounted, None, "a designed row has none");

    let bare = ask(serve(empty_store(), None));
    for venue in &bare.venues {
        for (r, c) in history_channels_for(&venue.venue).iter().zip(&venue.channels) {
            let want = match r.state {
                ChannelState::Built(_) => Some(false),
                ChannelState::Designed(_) => None,
            };
            assert_eq!(c.mounted, want, "{}/{} with no table", venue.venue, r.name);
        }
    }
}

/// **The read never runs a lane.** Every collector in the table panics, so a read that reached one
/// would kill its connection thread and the client would get no answer; it answers, twice, on one
/// connection.
#[test]
fn a_table_whose_collectors_panic_still_answers() {
    let table = planted_table().with("oanda", BackfillLane::CredentialedKlines, must_not_run());
    let addr = serve(empty_store(), Some(table));
    let mut client = DatahubClient::connect(addr).expect("connect");
    let first = client.history_channels().expect("answered without running a lane");
    assert_eq!(row(&first, "oanda", "v20 REST candles").mounted, Some(true));
    let again = client.history_channels().expect("and the connection is still alive");
    assert_eq!(first.venues.len(), again.venues.len());
}

/// **The credential word**: a keyless lane's row is `NotNeeded`; the credentialed row is
/// `NotChecked` when its lane is mounted without a probe and when no table is mounted at all —
/// nothing was read, so it is never `Absent` by omission — and is the probe's own word when one is
/// attached, asked per request.
#[test]
fn the_credential_word_is_the_tables_probe_and_never_absent_by_omission() {
    let oanda =
        |report: &HistoryChannelsReport| row(report, "oanda", "v20 REST candles").credential;

    let unprobed = planted_table().with("oanda", BackfillLane::CredentialedKlines, must_not_run());
    let report = ask(serve(empty_store(), Some(unprobed)));
    assert_eq!(oanda(&report), CredentialPresence::NotChecked);
    assert_eq!(
        row(&report, "binance", "klines (spot and futures)").credential,
        CredentialPresence::NotNeeded
    );
    assert_eq!(oanda(&ask(serve(empty_store(), None))), CredentialPresence::NotChecked);

    for word in
        [CredentialPresence::Present, CredentialPresence::Absent, CredentialPresence::Unreadable]
    {
        let table = planted_table()
            .with("oanda", BackfillLane::CredentialedKlines, must_not_run())
            .with_credential_probe("oanda", Box::new(move || word));
        assert_eq!(oanda(&ask(serve(empty_store(), Some(table)))), word);
    }
}

/// **What the store holds rides the answer**: one entry per kind for the venue that holds it, and
/// nothing for a venue that holds nothing.
#[test]
fn what_the_store_holds_rides_the_answer() {
    let store = MemHistStore::new();
    let bar = |ts: i64| Bar {
        ts,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    store
        .append_bars("oanda", "EUR_USD", "1m", &[bar(60_000), bar(120_000), bar(180_000)], None)
        .expect("planted bars");
    let report = ask(serve(Arc::new(store), None));
    let held = &report.venue("oanda").expect("oanda").held;
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!((held[0].kind.as_str(), held[0].series, held[0].rows), ("bar", 1, 3));
    assert_eq!((held[0].first_ts, held[0].last_ts), (60_000, 180_000));
    assert!(report.venue("binance").expect("binance").held.is_empty());
}

/// **The scope verdict over the wire** (`docs/decisions/0102`): a KEYED server serves the read to a
/// connection holding only the Observe key.
#[test]
fn an_observe_key_is_served_the_read() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec());
    let served = keys.clone();
    thread::spawn(move || {
        let _ = serve_authed(
            listener,
            empty_store(),
            Some(planted_table()),
            Some(served),
            None,
            None,
            None,
        );
    });
    let mut observe =
        DatahubClient::connect_authed(addr, &keys, Scope::Read).expect("an Observe connection");
    let report = observe.history_channels().expect("Observe is SERVED the read");
    assert_eq!(report.venues.len(), vike_model::VENUES.len());
}
