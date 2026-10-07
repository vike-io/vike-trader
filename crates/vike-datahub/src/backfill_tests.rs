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
        .with("binance", BackfillLane::Funding, quiet())
        .with("dukascopy", BackfillLane::TickBars, quiet())
        .with("oanda", BackfillLane::CredentialedKlines, quiet())
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
        (BackfillLane::Klines, false),
        (BackfillLane::TickBars, true),
        (BackfillLane::Funding, false),
        (BackfillLane::CredentialedKlines, true),
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
    assert!(t.mounts("binance", BackfillLane::Klines));
    assert!(t.mounts("binance", BackfillLane::Funding));
    assert!(!t.mounts("binance", BackfillLane::TickBars), "another lane is another row");
    assert!(!t.mounts("bybit", BackfillLane::Klines), "an unlisted venue is not mounted");
    assert!(t.mounts("oanda", BackfillLane::CredentialedKlines));

    assert_eq!(
        t.credential_presence("binance", BackfillLane::Klines),
        CredentialPresence::NotNeeded
    );
    assert_eq!(
        t.credential_presence("oanda", BackfillLane::CredentialedKlines),
        CredentialPresence::NotChecked,
        "no probe attached: nothing was read, so it is not Absent"
    );
    for word in
        [CredentialPresence::Present, CredentialPresence::Absent, CredentialPresence::Unreadable]
    {
        let probed = table().with_credential_probe("oanda", Box::new(move || word));
        assert_eq!(probed.credential_presence("oanda", BackfillLane::CredentialedKlines), word);
        assert_eq!(
            probed.credential_presence("dukascopy", BackfillLane::CredentialedKlines),
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
    let r = t.register(
        "oanda",
        "EUR_USD",
        "5s",
        (10, 20),
        Some(peer),
        BackfillLane::CredentialedKlines,
    );
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
            t.register("dukascopy", "EURUSD", "1m", (0, 1), None, BackfillLane::TickBars);
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
    let r = t.register("dukascopy", "EURUSD", "1m", (0, 1), None, BackfillLane::TickBars);
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
    let first = t.register("dukascopy", "EURUSD", "1m", (0, 9), None, BackfillLane::TickBars);
    let second = t.register("dukascopy", "EURUSD", "1m", (10, 19), None, BackfillLane::TickBars);
    let other_symbol =
        t.register("dukascopy", "GBPUSD", "1m", (0, 9), None, BackfillLane::TickBars);
    let other_interval =
        t.register("dukascopy", "EURUSD", "5m", (0, 9), None, BackfillLane::TickBars);

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
    let klines = t.register("binance", "BTCUSDT", "1h", (0, 9), None, BackfillLane::Klines);
    let funding =
        t.register("binance", "BTCUSDT.P", "funding", (0, 9), None, BackfillLane::Funding);
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
    let a = t.register("dukascopy", "EURUSD", "1m", (0, 1), None, BackfillLane::TickBars);
    let b = t.register("dukascopy", "EURUSD", "1m", (0, 1), None, BackfillLane::TickBars);
    let (id_a, id_b) = (a.request().id, b.request().id);
    assert!(id_a < id_b);
    drop(a);
    let c = t.register("dukascopy", "EURUSD", "1m", (0, 1), None, BackfillLane::TickBars);
    assert!(c.request().id > id_b, "a freed id is not handed out again");
    assert_eq!(ids(&t.running()), vec![id_b, c.request().id], "listed in the order begun");
}
