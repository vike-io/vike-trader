//! Unit tests of `backfill.rs`, out of line.
//! The REGISTRY of running requests, white-box: what `ListBackfills` and `CancelBackfill` read
//! and flag. The wire half — a cancel from a second connection stopping a real backfill — is
//! `crates/vike-datahub/tests/backfill_cancel.rs`'s.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;

fn quiet() -> BackfillFn {
    Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| Ok(0))
}

fn table() -> BackfillTable {
    BackfillTable::new(vec![("binance".to_string(), quiet())])
        .with("binance", HistoryLane::Funding, quiet())
        .with("dukascopy", HistoryLane::TickBars, quiet())
        .with("oanda", HistoryLane::CredentialedKlines, quiet())
}

fn ids(rows: &[RunningBackfill]) -> Vec<u64> {
    rows.iter().map(|r| r.id).collect()
}

/// The two lanes that stop are the two CHUNKED rows `BackfillFn`'s doc names; the one-batch
/// rows do not. Pinned verbatim, so moving a lane across is a visible diff here as well as a
/// decision in `stops_at_a_chunk_boundary`'s match.
#[test]
fn the_lanes_a_cancel_can_stop_are_the_chunked_ones() {
    let table = [
        (HistoryLane::Klines, false),
        (HistoryLane::TickBars, true),
        (HistoryLane::Funding, false),
        (HistoryLane::CredentialedKlines, true),
    ];
    for (lane, stops) in table {
        assert_eq!(stops_at_a_chunk_boundary(lane), stops, "{lane:?}");
    }
}

/// The history-channels read's two table questions: `mounts` answers per (venue, lane) and
/// nothing broader, and `credential_presence` is `NotNeeded` on a keyless lane, the probe's
/// word on a credentialed one, and `NotChecked` — never `Absent` — where no probe was attached.
#[test]
fn the_table_answers_mounted_and_presence_per_venue_and_lane() {
    let t = table();
    assert!(t.mounts("binance", HistoryLane::Klines));
    assert!(t.mounts("binance", HistoryLane::Funding));
    assert!(!t.mounts("binance", HistoryLane::TickBars), "another lane is another row");
    assert!(!t.mounts("bybit", HistoryLane::Klines), "an unlisted venue is not mounted");
    assert!(t.mounts("oanda", HistoryLane::CredentialedKlines));

    assert_eq!(
        t.credential_presence("binance", HistoryLane::Klines),
        CredentialPresence::NotNeeded
    );
    assert_eq!(
        t.credential_presence("oanda", HistoryLane::CredentialedKlines),
        CredentialPresence::NotChecked,
        "no probe attached: nothing was read, so it is not Absent"
    );
    for word in
        [CredentialPresence::Present, CredentialPresence::Absent, CredentialPresence::Unreadable]
    {
        let probed = table().with_credential_probe("oanda", Box::new(move || word));
        assert_eq!(probed.credential_presence("oanda", HistoryLane::CredentialedKlines), word);
        assert_eq!(
            probed.credential_presence("dukascopy", HistoryLane::CredentialedKlines),
            CredentialPresence::NotChecked,
            "a probe answers for ITS venue only"
        );
    }
}

/// A registered request is listed — with what it carried, its lane, and the counters — for
/// exactly as long as its registration lives.
#[test]
fn a_registered_request_is_listed_until_its_registration_drops() {
    let t = table();
    assert!(t.running().is_empty(), "nothing registered, nothing listed");
    let peer: SocketAddr = "127.0.0.1:50000".parse().expect("addr");
    let r =
        t.register("oanda", "EUR_USD", "5s", (10, 20), Some(peer), HistoryLane::CredentialedKlines);
    r.request().reached_a_boundary();
    r.request().reached_a_boundary();

    let listed = t.running();
    assert_eq!(listed.len(), 1, "{listed:?}");
    let row = &listed[0];
    assert_eq!(
        (row.venue.as_str(), row.symbol.as_str(), row.interval.as_str()),
        ("oanda", "EUR_USD", "5s")
    );
    assert_eq!((row.start, row.end), (10, 20));
    assert_eq!(row.peer.as_deref(), Some("127.0.0.1:50000"));
    assert_eq!(row.lane, "CredentialedKlines");
    assert!(row.stoppable && !row.cancelled);
    assert_eq!(row.boundaries, 2, "the probe's call count is the progress figure");
    assert!(row.started_ms > 0);

    drop(r);
    assert!(t.running().is_empty(), "a dropped registration leaves no entry");
}

/// ⚠ **A PANICKING collector leaves no entry** — the property the removal's placement in `Drop`
/// exists for. The registration is held across a "collector" that panics, exactly as
/// `crate::server`'s `backfill_verb` holds it; the unwind must take the entry with it.
#[test]
fn a_panicking_collector_leaves_no_entry_behind() {
    let t = table();
    let unwound = catch_unwind(AssertUnwindSafe(|| {
        let _registration =
            t.register("dukascopy", "EURUSD", "1m", (0, 1), None, HistoryLane::TickBars);
        assert_eq!(t.running().len(), 1, "guard: the request is registered while it runs");
        panic!("the collector panicked mid-request");
    }));
    assert!(unwound.is_err(), "guard: the collector did panic");
    assert!(t.running().is_empty(), "the unwind leaked a registry entry: {:?}", t.running());
}

/// A lock poisoned by a holder that panicked is RECOVERED, not re-panicked on: registering,
/// listing and the `Drop` removal all still work — the last one may run during an unwind, where
/// a second panic aborts the process.
#[test]
fn a_poisoned_registry_still_registers_and_removes() {
    let t = table();
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _held = t.running.lock();
        panic!("a holder panicked under the registry's lock");
    }));
    assert!(t.running.state.is_poisoned(), "guard: the lock is poisoned");
    let r = t.register("dukascopy", "EURUSD", "1m", (0, 1), None, HistoryLane::TickBars);
    assert_eq!(t.running().len(), 1);
    drop(r);
    assert!(t.running().is_empty());
}

/// A cancel flags EVERY stoppable request on its series and nothing else — not another symbol,
/// not another interval of the same symbol — reports them, and is idempotent. A series with
/// nothing running is an empty success.
#[test]
fn a_cancel_flags_the_stoppable_requests_on_its_series_and_nothing_else() {
    let t = table();
    let first = t.register("dukascopy", "EURUSD", "1m", (0, 9), None, HistoryLane::TickBars);
    let second = t.register("dukascopy", "EURUSD", "1m", (10, 19), None, HistoryLane::TickBars);
    let other_symbol = t.register("dukascopy", "GBPUSD", "1m", (0, 9), None, HistoryLane::TickBars);
    let other_interval =
        t.register("dukascopy", "EURUSD", "5m", (0, 9), None, HistoryLane::TickBars);

    let done = t.cancel("dukascopy", "EURUSD", "1m");
    assert_eq!(ids(&done.flagged), vec![first.request().id, second.request().id]);
    assert!(done.unstoppable.is_empty(), "{done:?}");
    assert!(done.flagged.iter().all(|r| r.cancelled), "the answer shows the raised flags");
    assert!(first.request().cancelled() && second.request().cancelled());
    assert!(!other_symbol.request().cancelled(), "another symbol is another series");
    assert!(!other_interval.request().cancelled(), "another interval is another series");

    let again = t.cancel("dukascopy", "EURUSD", "1m");
    assert_eq!(ids(&again.flagged), ids(&done.flagged), "asking twice is not an error");
    assert_eq!(
        t.cancel("dukascopy", "XAUUSD", "1m"),
        BackfillCancelDone::default(),
        "nothing running on the series: an empty success"
    );
    // The cancel removes nothing: every request is still listed until its own registration
    // drops — a flagged one stops at ITS next boundary, on its own thread.
    assert_eq!(t.running().len(), 4);
}

/// ⚠ A ONE-BATCH request is LISTED, reported under `unstoppable` by a cancel, and its flag is
/// NOT raised — the design's Q6: refused by name rather than pretended at.
#[test]
fn a_one_batch_request_is_listed_and_never_flagged() {
    let t = table();
    let klines = t.register("binance", "BTCUSDT", "1h", (0, 9), None, HistoryLane::Klines);
    let funding = t.register("binance", "BTCUSDT.P", "funding", (0, 9), None, HistoryLane::Funding);
    let listed = t.running();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|r| !r.stoppable), "{listed:?}");

    let done = t.cancel("binance", "BTCUSDT", "1h");
    assert!(done.flagged.is_empty(), "{done:?}");
    assert_eq!(ids(&done.unstoppable), vec![klines.request().id]);
    assert!(!klines.request().cancelled(), "a flag nothing will read must not be raised");
    assert!(!done.unstoppable[0].cancelled);

    let done = t.cancel("binance", "BTCUSDT.P", "funding");
    assert_eq!(ids(&done.unstoppable), vec![funding.request().id]);
    assert!(!funding.request().cancelled());
}

/// Ids ascend in registration order and are never reused, so two requests on one series stay
/// apart in the list and in a cancel's answer.
#[test]
fn ids_ascend_in_registration_order_and_are_never_reused() {
    let t = table();
    let a = t.register("dukascopy", "EURUSD", "1m", (0, 1), None, HistoryLane::TickBars);
    let b = t.register("dukascopy", "EURUSD", "1m", (0, 1), None, HistoryLane::TickBars);
    let (id_a, id_b) = (a.request().id, b.request().id);
    assert!(id_a < id_b);
    drop(a);
    let c = t.register("dukascopy", "EURUSD", "1m", (0, 1), None, HistoryLane::TickBars);
    assert!(c.request().id > id_b, "a freed id is not handed out again");
    assert_eq!(ids(&t.running()), vec![id_b, c.request().id], "listed in the order begun");
}

// --- The presence probe: a question to the gatewayhub, never a read of a store ---------------------
//
// The practice token is the GATEWAYHUB's (decision 0164): this daemon holds no store handle for it
// and reads no credential, so the history-channels read's `credential` word for OANDA's row is what
// the daemon answers when asked. These tests dial the real client over the testkit's loopback
// gateway; the daemon side of the five words is `crates/vike-gatewayhub`'s own suite.

use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::ScriptedRest;
use vike_gatewayhub_client::credential::CredentialPresence as Asked;

/// A link whose address nothing listens on, with BOTH hub keys: a refused dial, not a missing key.
fn dead_link() -> GatewayLink {
    let addr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        listener.local_addr().expect("its address")
        // dropped here: the port now refuses connections
    };
    GatewayLink::new(addr.to_string(), vike_gatewayhub::testkit::gateway::keys())
}

/// **Every word the gatewayhub can answer maps to ITS twin, and no word is folded into another.**
/// The five-word set is the history-channels report's (`vike_datahub_client::history`), unchanged by
/// the move: `Unreadable` stays distinct from `Absent` (0097's verdict 6) and `NotChecked` from both.
#[test]
fn the_probe_maps_each_of_the_five_words_to_its_twin() {
    let rest = ScriptedRest::new().with_presence("oanda", Asked::NotNeeded);
    let gateway = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
    let probe = gateway_credential_probe(gateway.link(), "oanda");
    let twins = [
        (Asked::NotNeeded, CredentialPresence::NotNeeded),
        (Asked::Present, CredentialPresence::Present),
        (Asked::Absent, CredentialPresence::Absent),
        (Asked::Unreadable, CredentialPresence::Unreadable),
        (Asked::NotChecked, CredentialPresence::NotChecked),
    ];
    assert_eq!(twins.len(), Asked::ALL.len(), "a sixth word needs a row here and an arm there");
    for (asked, word) in twins {
        assert!(Asked::ALL.contains(&asked), "{asked:?}");
        rest.with_presence("oanda", asked);
        assert_eq!(probe(), word, "the gatewayhub said {asked:?}");
    }
    assert_eq!(
        rest.calls().len(),
        twins.len(),
        "one question per call, nothing cached: {:?}",
        rest.calls()
    );
}

/// **An unreachable gatewayhub is `NotChecked`, never `Absent`** — nothing was read, so "not
/// stored" would send an operator to store a key they may already have stored. Both ways a link can
/// be dead: a refused dial, and the production link's EMPTY hub keys (the keys change that provisions
/// them is not in yet), which refuses locally before dialling anything.
#[test]
fn an_unreachable_gatewayhub_is_not_checked_never_absent() {
    let probe = gateway_credential_probe(dead_link(), "oanda");
    assert_eq!(probe(), CredentialPresence::NotChecked, "a refused dial");

    let keyless =
        GatewayLink::new("127.0.0.1:1", vike_hub_proto::auth::HubKeys::new(vec![], vec![]));
    let probe = gateway_credential_probe(keyless, "oanda");
    assert_eq!(probe(), CredentialPresence::NotChecked, "no key to sign with");
}

/// **The history-channels reply carries a WORD for OANDA's token, asked afresh per request**, and
/// reading it runs no lane: the table's collector PANICS, so the word must come from the probe. The
/// gatewayhub's answer changing between two requests changes the next reply with no restart. (The old
/// form of this test also proved no token value reached the reply; the datahub holds no value now.)
#[test]
fn the_history_channels_reply_carries_a_word_asked_afresh_per_request() {
    use vike_datahub_client::proto::Response;

    let rest = ScriptedRest::new().with_presence("oanda", Asked::Present);
    let gateway = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
    let store: Arc<dyn vike_data::HistStore + Send + Sync> =
        Arc::new(vike_data::MemHistStore::new());
    let panics: BackfillFn = Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| {
        panic!("the history-channels read called a collector")
    });
    let table = BackfillTable::new(Vec::new())
        .with("oanda", HistoryLane::CredentialedKlines, panics)
        .with_credential_probe("oanda", gateway_credential_probe(gateway.link(), "oanda"));
    let oanda_word = |response: &Response| match response {
        Response::HistoryChannels(report) => {
            let row = &report.venue("oanda").expect("oanda is on the roster").channels[0];
            assert_eq!(row.mounted, Some(true), "the planted row is mounted");
            row.credential
        }
        other => panic!("expected HistoryChannels, got {other:?}"),
    };

    let first = crate::history::history_channels_verb(&store, Some(&table), 1_790_899_200_000);
    assert_eq!(oanda_word(&first), CredentialPresence::Present);
    rest.with_presence("oanda", Asked::Absent);
    let second = crate::history::history_channels_verb(&store, Some(&table), 1_790_899_200_000);
    assert_eq!(
        oanda_word(&second),
        CredentialPresence::Absent,
        "the key was removed: the next answer says so, uncached"
    );
}
