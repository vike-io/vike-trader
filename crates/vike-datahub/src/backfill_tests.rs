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

/// **OANDA's row fetches a UTC day at a time at EVERY interval.** The width is part of the key
/// identity of every series OANDA has ever stored (the commit key is the grid chunk's bounds), so a
/// width that moved for any interval would re-key that series and store each bar twice. The
/// ingest's own keys and words are pinned byte for byte by `vike-backfill`'s `oanda_golden`.
#[cfg(feature = "backfill-serve")]
#[test]
fn oanda_is_fetched_a_utc_day_at_a_time_at_every_interval() {
    for interval in ["5s", "10s", "15s", "30s", "1m", "5m", "15m", "30m", "1h", "4h", "1d", "x"] {
        assert_eq!(oanda_chunk_days(interval), 1, "{interval}");
    }
}

/// **IG's row takes its chunk width from the pinned catalog table**, interval by interval. The table
/// itself is pinned where it lives (`vike-catalog`'s `the_chunk_width_table_is_pinned`); this holds the
/// row to it, with three widths spelled out so a function that ignored the table cannot pass.
#[cfg(feature = "backfill-serve")]
#[test]
fn the_ig_row_takes_its_chunk_width_from_the_pinned_table() {
    for step in vike_catalog::IG_HISTORY_STEPS {
        assert_eq!(ig_chunk_days(step.interval), step.chunk_days, "{}", step.interval);
    }
    assert_eq!(
        (ig_chunk_days("1m"), ig_chunk_days("1h"), ig_chunk_days("1d")),
        (1, 21, 28),
        "a day of minutes, three weeks of hours and four weeks of days"
    );
}

/// **IG's three refusals, each before anything is fetched**: a second spelling of an epic, an interval
/// IG's history does not serve here, and a first chunk that starts beyond IG's documented depth. The
/// depth is judged on the grid: the earliest start is the first grid line at or after the oldest
/// permitted instant, one millisecond earlier lands in the chunk before it, and the refusal names the
/// date of that line.
#[cfg(feature = "backfill-serve")]
#[test]
fn ig_history_refuses_a_second_spelling_an_unserved_interval_and_a_chunk_past_the_depth() {
    const DAY: i64 = 86_400_000;
    let now = 1_792_776_143_000; // 2026-10-23T17:22:23Z
    let epic = "CS.D.EURUSD.MINI.IP";

    for bad in ["EURUSD", "cs.d.eurusd.mini.ip", "EUR_USD", ""] {
        let why = ig_history_refusal_at(now, bad, "1h", now - 5 * DAY, now).expect(bad);
        assert!(why.contains("OWN epic") && why.contains("Nothing was fetched"), "{why}");
    }
    for bad in ["1s", "1w", "1mo", "7m", "x"] {
        let why = ig_history_refusal_at(now, epic, bad, now - 5 * DAY, now).expect(bad);
        assert!(why.contains("1m 2m 3m 5m") && why.contains("Nothing was fetched"), "{why}");
    }
    assert_eq!(ig_history_refusal_at(now, epic, "1h", now - 5 * DAY, now), None);

    // Hours: 360 days deep, 21-day chunks. The oldest permitted chunk start is a day beyond the depth.
    // (The windows are one chunk long: the whole-window size check is its own test.)
    let width = 21 * DAY;
    let oldest = now - 361 * DAY;
    let line = oldest + (width - oldest.rem_euclid(width)) % width;
    assert_eq!(
        ig_history_refusal_at(now, epic, "1h", line, line + width - 1),
        None,
        "the grid line itself"
    );
    let why = ig_history_refusal_at(now, epic, "1h", line - 1, line - 1 + width)
        .expect("the chunk before it");
    let date = vike_model::time::epoch_ms_to_utc_date(line);
    for part in ["360 days", "21-day chunk", "not clamped", date.as_str()] {
        assert!(why.contains(part), "{part:?} in {why}");
    }

    // ⚠ Minutes: 40 days deep, one-day chunks. Their assertions (38 days back passes, 42 is refused)
    // are not here while a minute request is refused as unmeasured before the depth is looked at;
    // they must come back when minute bars are admitted.
    // Days: 15 years, 28-day chunks.
    let start = now - 14 * 365 * DAY;
    assert_eq!(ig_history_refusal_at(now, epic, "1d", start, start + 100 * DAY), None);
    let start = now - 16 * 365 * DAY;
    assert!(ig_history_refusal_at(now, epic, "1d", start, start + 100 * DAY).is_some());
}

/// **Only the two measured intervals reach the lane** (1h and 1d): the other ten of the pinned table
/// are refused in words before any login, whatever the window, because only the hour and day
/// resolutions were probed and a chunk width becomes a series' key identity at its first fetch.
#[cfg(feature = "backfill-serve")]
#[test]
fn ig_history_admits_only_the_two_measured_intervals() {
    const DAY: i64 = 86_400_000;
    let now = 1_792_776_143_000;
    let epic = "CS.D.EURUSD.MINI.IP";

    for step in vike_catalog::IG_HISTORY_STEPS {
        let refused = ig_history_refusal_at(now, epic, step.interval, now - 2 * DAY, now - DAY);
        if matches!(step.interval, "1h" | "1d") {
            assert_eq!(refused, None, "{}", step.interval);
        } else {
            let why = refused.unwrap_or_else(|| panic!("{} was admitted", step.interval));
            for part in
                [step.interval, "unmeasured", "lean acceptance first", "Nothing was fetched"]
            {
                assert!(why.contains(part), "{part:?} in {why}");
            }
        }
    }
}

/// **One wrong request cannot spend the week.** The whole window is costed before any login — the sum of
/// its chunks' calendar-bound points, chunks rounded outward to the grid, the window cut at now — and
/// refused above a quarter of IG's week (2500 of 10000). Four 21-day chunks of hours are 2016 and go, five
/// are 2520 and do not; 89 chunks of days are 2492 and go, 90 are 2520 and do not; a window that
/// starts one millisecond into a chunk costs that whole chunk; a window that runs past now costs only the
/// chunks that have begun.
#[cfg(feature = "backfill-serve")]
#[test]
fn ig_history_refuses_a_window_above_a_quarter_of_the_week() {
    const DAY: i64 = 86_400_000;
    let now = 1_792_776_143_000; // 2026-10-23T17:22:23Z
    let epic = "CS.D.EURUSD.MINI.IP";
    let first_line = |width: i64, depth_days: i64| {
        let oldest = now - (depth_days + 1) * DAY;
        oldest + (width - oldest.rem_euclid(width)) % width
    };

    let (w, line) = (21 * DAY, first_line(21 * DAY, 360));
    assert_eq!(ig_history_refusal_at(now, epic, "1h", line, line + 4 * w - 1), None, "2016 points");
    let why = ig_history_refusal_at(now, epic, "1h", line, line + 5 * w - 1).expect("2520 points");
    for part in ["2520 points", "25 percent", "2500", "10000", "Nothing was fetched", "split"] {
        assert!(why.contains(part), "{part:?} in {why}");
    }
    assert!(
        ig_history_refusal_at(now, epic, "1h", line + 1, line + 4 * w).is_some(),
        "one millisecond into a chunk costs the whole chunk: five chunks"
    );

    let (w, line) = (28 * DAY, first_line(28 * DAY, 5_475));
    assert_eq!(ig_history_refusal_at(now, epic, "1d", line, line + 89 * w - 1), None, "2492");
    assert!(ig_history_refusal_at(now, epic, "1d", line, line + 90 * w - 1).is_some(), "2520");

    // A window cut at now: a year ahead of now costs the chunks up to now only (two of hours).
    assert_eq!(ig_history_refusal_at(now, epic, "1h", now - 5 * DAY, now + 400 * DAY), None);
    // Everything from the start of the depth to now is far above a quarter of the week.
    assert!(ig_history_refusal_at(now, epic, "1h", line_of_depth(now), now).is_some());
}

/// **The pre-flight counts only the chunks the ingest can settle.** The chunk that holds now is never
/// fetched (it is stored once it has ended), so a window that runs to now is costed up to the last
/// chunk that has ended: four settled hourly chunks (2016 points) pass, five (2520) do not, whether or
/// not the window also reaches into the chunk that holds now.
#[cfg(feature = "backfill-serve")]
#[test]
fn ig_history_costs_a_window_only_up_to_the_last_settled_chunk() {
    const DAY: i64 = 86_400_000;
    let now: i64 = 1_792_776_143_000; // 2026-10-23T17:22:23Z, a day and 17 hours into its 21-day chunk
    let epic = "CS.D.EURUSD.MINI.IP";
    let width = 21 * DAY;
    let current = now.div_euclid(width) * width; // the chunk that holds now

    assert_eq!(
        ig_history_refusal_at(now, epic, "1h", current - 4 * width, now),
        None,
        "four settled chunks and the one that holds now: only the four are costed"
    );
    assert!(
        ig_history_refusal_at(now, epic, "1h", current - 5 * width, now).is_some(),
        "five settled chunks are 2520 points"
    );
    assert_eq!(
        ig_history_refusal_at(now, epic, "1h", current, now),
        None,
        "a window inside the chunk that holds now costs nothing: nothing in it can be fetched yet"
    );
}

/// The first 21-day grid line inside the hour depth, for a window that runs to now.
#[cfg(feature = "backfill-serve")]
fn line_of_depth(now: i64) -> i64 {
    const DAY: i64 = 86_400_000;
    let oldest = now - 361 * DAY;
    oldest + (21 * DAY - oldest.rem_euclid(21 * DAY)) % (21 * DAY)
}

/// **cTrader's chunk widths, pinned interval by interval.** The width is part of the key identity of
/// every series the lane stores (the commit key is the grid chunk's bounds), so a width that moved for
/// any interval would re-key that series and store each bar twice: one UTC day for the minute intervals
/// up to `30m`, seven days for `1h` and coarser, and one for anything the source does not serve (it is
/// refused before the width is used).
#[cfg(feature = "backfill-serve")]
#[test]
fn ctrader_chunk_widths_are_pinned() {
    let pinned = [
        ("1m", 1),
        ("2m", 1),
        ("3m", 1),
        ("4m", 1),
        ("5m", 1),
        ("10m", 1),
        ("15m", 1),
        ("30m", 1),
        ("1h", 7),
        ("4h", 7),
        ("12h", 7),
        ("1d", 7),
        ("1w", 1),
        ("1M", 1),
        ("x", 1),
    ];
    for (interval, days) in pinned {
        assert_eq!(ctrader_chunk_days(interval), days, "{interval}");
    }
}

/// The datahub refuses only what could never be a broker symbol name; a name the broker may list is
/// let through to the gatewayhub, whose source checks it against the account's own list.
#[cfg(feature = "backfill-serve")]
#[test]
fn ctrader_history_refuses_only_what_could_never_be_a_symbol_name() {
    for fine in
        ["EURUSD", "EURUSD.r", "NAS100.cash", "#AAPL", "US500", "BTC-USD", "XAUUSD_i", "eurusd"]
    {
        assert_eq!(ctrader_history_refusal(fine, "1m", 0, 1), None, "{fine}");
    }
    let too_long = "A".repeat(41);
    for bad in ["", "EUR/USD", "EURUSD:1m", "EUR USD", "EURUSD\n", "ЕURUSD", too_long.as_str()] {
        let why = ctrader_history_refusal(bad, "1m", 0, 1).unwrap_or_else(|| panic!("{bad:?}"));
        assert!(why.contains("Nothing was fetched or written"), "{why}");
    }
}
