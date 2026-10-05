//! End-to-end proof, through the PUBLIC materializer surface, that the columnar exec log now
//! records what the live order FSM actually did — not what an unguarded re-spelling of it would
//! have done.
//!
//! ⚠ This gate was crates/vike-ops/tests/journal_mat_fsm_fold.rs under that crate's DEFAULT `full`
//! (that path is deliberately NOT backticked: the file no longer exists, and
//! `crates/vike-ops/tests/citation_gate.rs` rightly refuses an anchored citation to a dead path —
//! the precedent is `crates/vike-ops/tests/reconcile_gate_wiring_gate.rs`'s own history note)
//! feature until 2026-09-25, and it moved with the module it drives. It carries NO `#![cfg]` any
//! more, and that is load-bearing: for one commit it was `#![cfg(feature = "materialize")]`, and
//! since no test lane enabled that feature this file compiled to an empty binary and proved nothing
//! while reading green.
//!
//! The unit tests in `materialize.rs` drive `OrderTracker` directly. This one goes the whole way a
//! production node goes: write a WAL with `vike_journal::CommandJournal`, spawn the real
//! [`vike_journal::materialize::JournalMaterializer`] over it, shut it down (which forces a final
//! drain), and read the `kind=exec_order` rows back out of the store.
//!
//! THE SETUP IS THE BUG. The WAL is WRITE-AHEAD: `spawn_core`'s fold appends each `Ingest::Event`
//! BEFORE `ExecutionEngine::on_event` folds it, so the journal necessarily contains events the FSM
//! went on to REFUSE (`ManagedOrder::apply` -> `InvalidOrderTransition` -> `on_event` returns). The
//! sequence below ends with an `OrderModified` on an order that already FILLED — a venue amend-ack
//! (a REST round trip) landing after the fill (a one-hop user-WS push) that terminalized the order.
//! `OrderStatus::MODIFIABLE` is {ACCEPTED, TRIGGERED, PARTIALLY_FILLED}, so the live order kept
//! qty 1.0 @ 100.0. Before the FSM fold the materializer applied that amend and appended a durable
//! row claiming qty 5.0 @ 123.0 — an order that never existed at any venue.
use std::sync::Arc;
use std::time::Duration;

use vike_data::{HistStore, MemHistStore};
use vike_exec::{Command, Ingest, OrderIntent};
use vike_journal::materialize::{JournalMaterializer, MaterializerConfig};
use vike_journal::{CommandJournal, JournalFileConfig};
use vike_model::OrderRequest;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderFilled, OrderModified, OrderSubmitted,
};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const COID: &str = "c1";

fn submit_intent() -> Ingest {
    Ingest::Command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: COID.to_string(),
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(100.0),
        ..Default::default()
    }))))
}

fn ev(e: Event) -> Ingest {
    Ingest::Event(e)
}

#[test]
fn amend_ack_after_the_fill_is_not_written_into_the_durable_exec_order_log() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j = CommandJournal::open(dir.path(), JournalFileConfig::default()).unwrap();
        // The real WAL prefix for a live order: the write-ahead Submit intent, then the adapter's
        // own OrderSubmitted (emitted BEFORE the venue round trip), then the venue's accept.
        j.append_cmd(1, &submit_intent()).unwrap();
        j.append_cmd(
            2,
            &ev(Event::OrderSubmitted(OrderSubmitted { client_order_id: COID.into(), ts: 10 })),
        )
        .unwrap();
        j.append_cmd(
            3,
            &ev(Event::OrderAccepted(OrderAccepted {
                client_order_id: COID.into(),
                venue_order_id: Some("v1".into()),
                ts: 11,
            })),
        )
        .unwrap();
        // …the fill that terminalizes it…
        j.append_cmd(
            4,
            &ev(Event::OrderFilled(OrderFilled {
                client_order_id: COID.into(),
                fill: FillEvent {
                    trade_id: "t1".into(),
                    client_order_id: COID.to_string(),
                    venue: VENUE.into(),
                    symbol: SYMBOL.into(),
                    side: 1,
                    last_qty: 1.0,
                    last_px: 100.0,
                    commission: 0.0,
                    commission_asset: String::new().into(),
                    liquidity_side: "taker".into(),
                    ts: 12,
                    mark_price: None,
                    position_side: "BOTH".into(),
                },
                ts: 12,
            })),
        )
        .unwrap();
        // …and the losing side of the race: the venue's amend-ack, journaled write-ahead and then
        // REFUSED by the FSM (`is_modifiable()` is false for FILLED), so the live order never
        // took these terms.
        j.append_cmd(
            5,
            &ev(Event::OrderModified(OrderModified {
                client_order_id: COID.into(),
                venue_order_id: None,
                new_qty: Some(5.0),
                new_price: Some(123.0),
                ts: 13,
            })),
        )
        .unwrap();
        j.flush().unwrap();
    }

    let store = Arc::new(MemHistStore::new());
    let (mat, handle) = JournalMaterializer::spawn(
        dir.path().to_path_buf(),
        store.clone(),
        // Short interval only so a wedged Condvar shows up as a hang rather than a 5s pause; the
        // pass we assert on is the FIRST one, which runs before any sleep.
        MaterializerConfig { interval: Duration::from_millis(20) },
    );
    handle.shutdown(); // stop + final drain + join — deterministic, nothing in flight after this
    assert!(mat.materialized_seq() > 0, "the checkpoint advanced past the drained window");

    let rows = store.scan_exec_orders(VENUE, SYMBOL).unwrap();
    let row = rows.iter().find(|r| r.client_order_id == COID).expect("the order materialized");
    assert_eq!(row.status, "FILLED");
    assert_eq!(row.venue_order_id.as_deref(), Some("v1"));
    assert_eq!(row.filled_qty, 1.0);
    assert_eq!(row.avg_fill_px, 100.0);
    // The point of the whole change: the refused amend is NOT in the durable log.
    assert_eq!(row.qty, 1.0, "the FSM refused the amend, so the durable qty stays 1.0 (was 5.0)");
    assert_eq!(
        row.price,
        Some(100.0),
        "the FSM refused the amend, so the durable price stays 100.0 (was 123.0)"
    );
    assert_eq!(
        row.ts, 12,
        "a refused event does not advance the row clock past the fill that terminalized the order"
    );

    // And nothing was appended FOR the refused event: the coid is not marked touched, so this pass
    // wrote exactly one snapshot row for it.
    assert_eq!(rows.iter().filter(|r| r.client_order_id == COID).count(), 1);
}

/// The materializer's own idempotency is unaffected by folding through the FSM: a re-drain of the
/// same window (a lost checkpoint after a crash) still appends nothing new, because the per-window
/// `commit_key` — not the fold — is what makes at-least-once safe.
#[test]
fn refold_of_the_same_window_is_still_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j = CommandJournal::open(dir.path(), JournalFileConfig::default()).unwrap();
        j.append_cmd(1, &submit_intent()).unwrap();
        j.append_cmd(
            2,
            &ev(Event::OrderSubmitted(OrderSubmitted { client_order_id: COID.into(), ts: 10 })),
        )
        .unwrap();
        j.append_cmd(
            3,
            &ev(Event::OrderAccepted(OrderAccepted {
                client_order_id: COID.into(),
                venue_order_id: Some("v1".into()),
                ts: 11,
            })),
        )
        .unwrap();
        j.flush().unwrap();
    }

    let store = Arc::new(MemHistStore::new());
    let cfg = MaterializerConfig { interval: Duration::from_millis(20) };
    JournalMaterializer::spawn(dir.path().to_path_buf(), store.clone(), cfg.clone()).1.shutdown();
    let after_first = store.scan_exec_orders(VENUE, SYMBOL).unwrap().len();
    assert_eq!(after_first, 1, "one ACCEPTED snapshot");
    assert_eq!(store.scan_exec_orders(VENUE, SYMBOL).unwrap()[0].status, "ACCEPTED");

    // lose the checkpoint (crash before it was stored) and re-drain the identical window cold
    std::fs::remove_file(dir.path().join("materializer.ckpt")).unwrap();
    JournalMaterializer::spawn(dir.path().to_path_buf(), store.clone(), cfg).1.shutdown();
    assert_eq!(
        store.scan_exec_orders(VENUE, SYMBOL).unwrap().len(),
        after_first,
        "the per-window commit_key still makes the re-drain a store no-op"
    );
}
