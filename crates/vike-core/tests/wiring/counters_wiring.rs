//! The core publishes its health counters on every snapshot (`vike_execution::CoreCounters`, plan S14):
//! a counter an engine folds reaches the published cell, and the counters nothing moved stay zero.

use crate::kit::engines::sim_engine_with;

use vike_core::{CoreConfig, spawn_core};
use vike_execution::Engine;
use vike_execution::testing::{TestExecutionClient, canceled};
use vike_model::events::Event;

fn engine() -> Engine<TestExecutionClient> {
    sim_engine_with(TestExecutionClient::new("sim", 100.0))
}

/// An `OrderCanceled` for an order client id that was never registered — the engine folds it, drops it (unknown
/// order client id), and counts it into `events_dropped_order_unknown` (audit C2). A deterministic, public-API way
/// to move a counter the snapshot must then carry.
fn ghost_cancel(order_client_id: &str) -> Event {
    canceled(order_client_id, "", 0)
}

#[test]
fn the_published_snapshot_carries_the_core_counters() {
    let handle = spawn_core(engine(), CoreConfig::default());
    let cell = handle.snapshot_cell();
    let sender = handle.event_sender();
    for i in 0..3 {
        sender.blocking_send(ghost_cancel(&format!("ghost{i}"))).unwrap();
    }
    // Lossless shutdown folds every queued event, then the teardown publish carries the final counters.
    handle.shutdown_and_join();

    let counters = cell.load().counters;
    assert_eq!(
        counters.events_dropped_order_unknown, 3,
        "the three unknown-order cancels are counted"
    );
    // The counters nothing here moved stay zero: the snapshot invents no value.
    assert_eq!(
        counters,
        vike_execution::CoreCounters { events_dropped_order_unknown: 3, ..Default::default() },
        "nothing else moved, and nothing sent a non-finite number"
    );
}
