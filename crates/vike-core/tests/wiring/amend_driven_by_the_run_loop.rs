//! The unified amend (`crates/vike-core/src/runtime/apply/amend.rs`) driven by the REAL core thread: the
//! white-box suite (`src/runtime/tests/apply/amend.rs`) calls `drive_amends` by hand, and this one proves
//! the drain loop's boundary does it. A reduce-only stop on a venue that cannot move a stop (binance) is
//! asked to move; the node places the new stop first, and cancels the old one only once the venue has
//! answered the new one, with nothing but the venue's own events and the published snapshot to look at.

use std::sync::{Arc, Mutex};

use vike_core::spawn_core;
use vike_exec::{Command, ExecutionClient, OrderIntent};
use vike_model::events::{Event, OrderAccepted, OrderCanceled, OrderSubmitted};
use vike_model::{OrderRequest, OrderStatus, OrderType};

use crate::kit::engines::{engine_on, test_config};
use crate::kit::handle::{CoreJoinGuard, wait_for_snapshot};

/// A client that remembers what it was asked to cancel where the test can read it (the core owns the
/// client once it is spawned).
#[derive(Clone, Default)]
struct Cancels(Arc<Mutex<Vec<String>>>);

impl ExecutionClient for Cancels {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, client_order_id: &str) {
        self.0.lock().unwrap().push(client_order_id.to_string());
    }
}

fn reduce_only_stop(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: -1,
        qty: 1.0,
        order_type: OrderType::Stop,
        trigger_price: Some(90_000.0),
        reduce_only: true,
        ts: 1,
        ..Default::default()
    }
}

fn status_of(snap: &vike_exec::CoreSnapshot, coid: &str) -> Option<OrderStatus> {
    snap.orders.iter().find(|o| o.client_order_id == coid).map(|o| o.status)
}

#[test]
fn the_run_loop_places_the_new_stop_first_and_cancels_the_old_one_after_the_venue_answers() {
    let cancels = Cancels::default();
    let handle = CoreJoinGuard::new(spawn_core(
        engine_on("binance", "BTCUSDT.P", cancels.clone()),
        test_config(10_000.0),
    ));
    let cell = handle.snapshot_cell();
    let events = handle.event_sender();
    let venue_says = |ev: Event| events.blocking_send(ev).unwrap();

    // a resting reduce-only stop, explicit coid
    handle
        .try_command(Command::Order(OrderIntent::Submit(Box::new(reduce_only_stop("s1")))))
        .unwrap();
    venue_says(Event::OrderSubmitted(OrderSubmitted { client_order_id: "s1".into(), ts: 1 }));
    venue_says(Event::OrderAccepted(OrderAccepted {
        client_order_id: "s1".into(),
        venue_order_id: Some("v-s1".into()),
        ts: 1,
    }));
    wait_for_snapshot(&cell, 5, "s1 resting", |s| {
        status_of(s, "s1") == Some(OrderStatus::Accepted)
    });

    // move its trigger: binance cannot, so the node places a NEW stop under a minted coid
    handle
        .try_command(Command::Order(OrderIntent::Modify {
            client_order_id: "s1".into(),
            new_qty: None,
            new_price: Some(91_000.0),
        }))
        .unwrap();
    wait_for_snapshot(&cell, 5, "the replacement stop is in the book", |s| {
        s.orders.iter().any(|o| o.client_order_id != "s1")
    });
    let new = cell
        .load_full()
        .orders
        .iter()
        .find(|o| o.client_order_id != "s1")
        .map(|o| (o.client_order_id.clone(), o.trigger_price))
        .expect("a replacement");
    assert_eq!(new.1, Some(91_000.0), "the replacement carries the new trigger");
    assert!(
        cancels.0.lock().unwrap().is_empty(),
        "the old stop stands until the new one is answered"
    );

    // the venue answers the new stop: the boundary drive now cancels the old one
    venue_says(Event::OrderSubmitted(OrderSubmitted { client_order_id: new.0.clone(), ts: 2 }));
    venue_says(Event::OrderAccepted(OrderAccepted {
        client_order_id: new.0.clone(),
        venue_order_id: Some("v-new".into()),
        ts: 2,
    }));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while cancels.0.lock().unwrap().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the drain loop never cancelled the old stop after the new one was answered"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(*cancels.0.lock().unwrap(), vec!["s1".to_string()]);

    // the venue confirms: the amend ends, the lineage is in the book
    venue_says(Event::OrderCanceled(OrderCanceled {
        client_order_id: "s1".into(),
        reason: "venue".to_string().into(),
        ts: 3,
    }));
    wait_for_snapshot(&cell, 5, "the old stop is gone and the new one rests", |s| {
        status_of(s, "s1") == Some(OrderStatus::Canceled)
            && status_of(s, &new.0) == Some(OrderStatus::Accepted)
    });
    wait_for_snapshot(&cell, 5, "the amend is said in the recent events", |s| {
        s.recent_events.iter().any(|l| l.contains("replaced by") && l.contains(&new.0))
    });
}
