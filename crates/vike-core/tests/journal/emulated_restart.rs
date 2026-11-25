//! An EMULATED order survives a restart (decision 0275, `crates/vike-core/src/runtime/emulated.rs`): the real
//! core journals the parent in every `Snapshot` (`Snapshot.emulated`, format 27 only once one is held), the restore
//! hands it back with its child, and a new core works it again, awaiting its child's confirmation and listing
//! it in the one `emulated_orders` list. A journaled step (`Command::Emulated`) re-folds through the replay
//! core with no law, no clock and no price: the parent is armed by its own re-applied submit, the child is
//! placed by the step and linked to it.

use crate::scratch::Scratch;

use vike_core::replay::restore_from_journal;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{Command, EmulatedStep, EmulatedVerb, Ingest, OrderIntent};
use vike_journal::{
    Journal, JournalFileConfig, JournalSnapshotOrderEmulation, JournalSnapshotParts,
};
use vike_model::{OrderPeg, OrderPegAnchor, OrderRequest, OrderType};

use crate::kit::handle::{CoreJoinGuard, wait_for_snapshot};

fn config(dir: &Scratch, emulated: Vec<JournalSnapshotOrderEmulation>) -> CoreConfig {
    CoreConfig {
        clock: Box::new(vike_model::ClockTest::new(1_000)),
        order_client_id_session: Some(("em".into(), 0)),
        emulated,
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig::default(),
            snapshot_every: u64::MAX,
        }),
        ..CoreConfig::default()
    }
}

/// A pegged BUY of 1 at the best bid on `sim` (off the roster: no book vike runs holds a peg, so it is
/// emulated).
fn pegged(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Pegged,
        peg: Some(OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 }),
        ..Default::default()
    }
}

/// **Armed, journaled, restored, worked again.** The parent rides the exit `Snapshot` and stamps format 27; the
/// restore hands it back whole and in no registry; a new core given it lists it in the one list.
#[test]
fn an_emulated_order_survives_a_restart_and_is_listed_once() {
    let dir = Scratch::reserved("emulated-restart");
    {
        let handle = CoreJoinGuard::new(spawn_core(
            engine_on("sim", "BTCUSDT", RecordingClient::default()),
            config(&dir, Vec::new()),
        ));
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(pegged("")))));
        let cell = handle.snapshot_cell();
        wait_for_snapshot(&cell, 5, "the emulated row", |s| {
            s.emulated_orders.iter().any(|e| e.kind == "chase") && s.orders.is_empty()
        });
    } // the guard's drop shuts the core down: the exit `Snapshot` is written
    assert_eq!(
        Journal::latest_segment_version(&dir).unwrap(),
        Some(34),
        "stamped 34: the floor since format 34"
    );
    let state = restore_from_journal(&dir).unwrap().expect("a Snapshot was written");
    let [parent] = state.emulated.as_slice() else {
        panic!("one emulated order restored: {:?}", state.emulated)
    };
    assert_eq!(
        (parent.route_key.as_str(), parent.request.order_type.clone()),
        ("sim", OrderType::Pegged)
    );
    assert!(state.engines.iter().all(|e| e.registry.is_empty()), "the parent is in no registry");

    let again = Scratch::reserved("emulated-restart-2");
    let handle = CoreJoinGuard::new(spawn_core(
        engine_on("sim", "BTCUSDT", RecordingClient::default()),
        config(&again, state.emulated.clone()),
    ));
    let cell = handle.snapshot_cell();
    let order_client_id = parent.request.order_client_id.clone();
    wait_for_snapshot(&cell, 5, "worked again and listed once", |s| {
        s.emulated_orders.iter().filter(|e| e.id == order_client_id).count() == 1
            && s.orders.is_empty()
    });
}

/// **A journaled step re-folds, and the restore keeps the child linked**: a base `Snapshot`, the operator's
/// pegged submit, then the step the live boundary journaled placing its child. The replay core re-arms the
/// parent off its submit, places the child off the step (minted from the restored session) and links it;
/// the restore hands back the parent WITH that child, which a restarted core then awaits (never cancels).
#[test]
fn a_journaled_child_step_refolds_and_the_restore_keeps_it_linked() {
    let dir = Scratch::reserved("emulated-refold");
    let engine = engine_on("sim", "BTCUSDT", RecordingClient::default());
    let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
    let engines = vec![engine.snapshot_state()];
    j.append_snap(
        1_000,
        &engines,
        "em",
        1,
        0,
        JournalSnapshotParts::default(),
        vike_execution::state_hash(&engines),
    )
    .unwrap();
    let submit = Ingest::Command(Command::Order(OrderIntent::Submit(Box::new(pegged("em-p")))));
    j.append_command(1_000, &submit).unwrap();
    let child =
        OrderRequest { order_type: OrderType::Limit, price: Some(100.0), peg: None, ..pegged("") };
    let step = Ingest::Command(Command::Emulated(Box::new(EmulatedStep {
        parent: "em-p".into(),
        mount_id: None,
        verb: EmulatedVerb::Child(OrderIntent::Submit(Box::new(child))),
    })));
    j.append_command(2_000, &step).unwrap();
    j.flush().unwrap();
    drop(j);

    let state = restore_from_journal(&dir).unwrap().expect("a Snapshot to restore from");
    let [parent] = state.emulated.as_slice() else {
        panic!("the parent is restored: {:?}", state.emulated)
    };
    let linked = parent.child.clone().expect("the step's child is linked");
    assert_eq!(parent.children, vec![linked.clone()]);
    let registry = &state.engines[0].registry;
    assert!(registry.iter().any(|(c, _)| *c == linked), "the child is a registered order");
    assert!(!registry.iter().any(|(c, _)| c == "em-p"), "the parent is in no registry");
    assert_eq!(parent.child_qty, 1.0);
}
