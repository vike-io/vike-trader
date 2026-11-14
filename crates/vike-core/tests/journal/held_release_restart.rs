//! An order the core HOLDS for its release survives a restart (`crates/vike-core/src/runtime/held_release.rs`):
//! the real core journals it in every `Snapshot` (`Snapshot.held_releases`), the restore hands it back, and a new core
//! holds it again and lists it; a journaled RELEASE (`Command::OrderOn` of the request, its release taken off)
//! re-folds through the replay core into a registered order and an empty table, with no timer and no clock.

use crate::scratch::Scratch;

use vike_core::replay::restore_from_journal;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{Command, Ingest, OrderIntent, RoutedOrder};
use vike_journal::{Journal, JournalFileConfig, JournalSnapshotParts, OrderHeldRelease};
use vike_model::{OrderRelease, OrderReleasePrice, OrderReleasePriceSide, OrderRequest, OrderType};

use crate::kit::handle::{CoreJoinGuard, wait_for_snapshot};

/// A `gtd`-free release far after any clock a test core reads (2100-01-01).
const FAR: i64 = 4_102_444_800_000;

fn config(dir: &Scratch, held: Vec<OrderHeldRelease>) -> CoreConfig {
    CoreConfig {
        clock: Box::new(vike_model::ClockTest::new(1_000)),
        order_client_id_session: Some(("hr".into(), 0)),
        held_releases: held,
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig::default(),
            snapshot_every: u64::MAX,
        }),
        ..CoreConfig::default()
    }
}

fn held_limit(order_client_id: &str, release: i64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        release: Some(OrderRelease { at: release, price: None }),
        ..Default::default()
    }
}

/// **Held, journaled, restored, held again.** A held order rides the exit `Snapshot`; the restore hands it back
/// whole; a new core given it holds it again (its timer armed, nothing sent) and lists it as a held row.
#[test]
fn a_held_order_survives_a_restart_and_is_listed_held() {
    let dir = Scratch::reserved("held-release-restart");
    {
        let handle = CoreJoinGuard::new(spawn_core(
            engine_on("sim", "BTCUSDT", RecordingClient::default()),
            config(&dir, Vec::new()),
        ));
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(held_limit("", FAR)))));
        let cell = handle.snapshot_cell();
        wait_for_snapshot(&cell, 5, "the held row", |s| {
            s.held_exits.iter().any(|h| h.release.is_some()) && s.orders.is_empty()
        });
    } // the guard's drop shuts the core down: the exit `Snapshot` is written
    let state = restore_from_journal(&dir).unwrap().expect("a Snapshot was written");
    let [held] = state.held_releases.as_slice() else {
        panic!("one held order restored: {:?}", state.held_releases)
    };
    assert_eq!(held.route_key, "sim");
    assert_eq!(held.request.release, Some(OrderRelease { at: FAR, price: None }));
    assert!(state.engines.iter().all(|e| e.registry.is_empty()), "a held order is in no registry");

    let again = Scratch::reserved("held-release-restart-2");
    let handle = CoreJoinGuard::new(spawn_core(
        engine_on("sim", "BTCUSDT", RecordingClient::default()),
        config(&again, state.held_releases.clone()),
    ));
    let cell = handle.snapshot_cell();
    wait_for_snapshot(&cell, 5, "held again and listed", |s| {
        s.held_exits.iter().any(|h| {
            h.order_client_id == held.request.order_client_id
                && h.release == Some(OrderRelease { at: FAR, price: None })
        })
    });
}

/// **A condition read on ANOTHER venue stamps the journal 24, survives the restart, and waits for that
/// venue's price** (decision 0267's 2.13). The held order's release names binance: the exit `Snapshot` and the
/// write-ahead submit carry the key a v23 reader cannot parse, so the segment is stamped 24; the restore
/// hands the condition back whole; a restarted core past the release time, with no binance account, holds
/// it and its HELD row says why, never releasing it on no price.
#[test]
fn a_cross_venue_condition_stamps_24_and_waits_for_its_venues_price_after_a_restart() {
    let dir = Scratch::reserved("held-release-venue");
    let condition = OrderReleasePrice {
        symbol: "BTCUSDT".into(),
        level: 100_000.0,
        side: OrderReleasePriceSide::Above,
        venue: Some("binance".into()),
    };
    let release = OrderRelease { at: 2_000, price: Some(condition) };
    {
        let handle = CoreJoinGuard::new(spawn_core(
            engine_on("sim", "BTCUSDT", RecordingClient::default()),
            config(&dir, Vec::new()),
        ));
        let req = OrderRequest { release: Some(release), ..held_limit("", 2_000) };
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
        let cell = handle.snapshot_cell();
        wait_for_snapshot(&cell, 5, "the held row", |s| {
            s.held_exits.iter().any(|h| h.release.is_some())
        });
    }
    assert_eq!(
        Journal::latest_segment_version(&dir).unwrap(),
        Some(30),
        "stamped 30: the floor since format 30"
    );
    let state = restore_from_journal(&dir).unwrap().expect("a Snapshot was written");
    let [held] = state.held_releases.as_slice() else {
        panic!("one held order restored: {:?}", state.held_releases)
    };
    assert_eq!(held.request.release, Some(release), "the condition and its venue come back whole");

    let again = Scratch::reserved("held-release-venue-2");
    let config_value = CoreConfig {
        clock: Box::new(vike_model::ClockTest::new(5_000)),
        ..config(&again, state.held_releases.clone())
    };
    let handle = CoreJoinGuard::new(spawn_core(
        engine_on("sim", "BTCUSDT", RecordingClient::default()),
        config_value,
    ));
    // any message brings the core to its timer boundary, where the passed release is judged
    handle.send_command(Command::Order(OrderIntent::Cancel("none".into())));
    let why = "no binance account runs on this node now, so its BTCUSDT price is not read";
    let cell = handle.snapshot_cell();
    wait_for_snapshot(&cell, 5, "held, saying why", |s| {
        s.held_exits.iter().any(|h| h.waiting.as_deref() == Some(why)) && s.orders.is_empty()
    });
}

/// **A journaled release re-folds**: a `Snapshot` holding one order, then the `OrderOn` the release timer
/// journaled. The restore's replay core (no timer: `CoreConfig::release_timer` is false there) re-applies it
/// through `lower_submit`: the order is registered and nothing is held any more.
#[test]
fn a_journaled_release_refolds_into_a_registered_order_and_an_empty_table() {
    let dir = Scratch::reserved("held-release-refold");
    let engine = engine_on("sim", "BTCUSDT", RecordingClient::default());
    let held =
        vec![OrderHeldRelease { route_key: "sim".into(), request: held_limit("hr1", 2_000) }];
    let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
    let books = JournalSnapshotParts { held_releases: &held, ..JournalSnapshotParts::default() };
    let engines = vec![engine.snapshot_state()];
    j.append_snap(1_000, &engines, "hr", 1, 0, books, vike_execution::state_hash(&engines))
        .unwrap();
    let mut released = held_limit("hr1", 2_000);
    released.release = None;
    let release = Ingest::Command(Command::OrderOn(Box::new(RoutedOrder {
        route_key: "sim".into(),
        intent: OrderIntent::Submit(Box::new(released)),
    })));
    j.append_command(2_000, &release).unwrap();
    j.flush().unwrap();
    drop(j);

    let state = restore_from_journal(&dir).unwrap().expect("a Snapshot to restore from");
    assert!(state.held_releases.is_empty(), "released in the tail: {:?}", state.held_releases);
    assert!(
        state.engines[0].registry.iter().any(|(order_client_id, _)| order_client_id == "hr1"),
        "the release registered the order: {:?}",
        state.engines[0].registry
    );
}
