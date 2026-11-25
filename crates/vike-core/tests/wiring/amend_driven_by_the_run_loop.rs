//! The unified amend (`crates/vike-core/src/runtime/apply/amend.rs`) driven by the REAL core thread: the
//! white-box suite (`src/runtime/tests/apply/amend.rs`) calls `drive_amends` by hand, and this one proves
//! the drain loop's boundary does it. A reduce-only stop on a venue that cannot move a stop (binance) is
//! asked to move; the node places the new stop first, and cancels the old one only once the venue has
//! answered the new one, with nothing but the venue's own events and the published snapshot to look at.

use std::sync::{Arc, Mutex};

use vike_core::spawn_core;
use vike_execution::{Command, ExecutionClient, OrderIntent};
use vike_model::events::{Event, OrderAccepted, OrderCanceled, OrderSubmitted};
use vike_model::{OrderRequest, OrderStatus};

use crate::kit::handle::{CoreJoinGuard, wait_for_snapshot};
use crate::kit::requests::binance_perp_reduce_only_stop;
use vike_core::testkit::test_config;
use vike_execution::testing::engine_on;

/// A client that remembers what it was asked to cancel where the test can read it (the core owns the
/// client once it is spawned).
#[derive(Clone, Default)]
struct Cancels(Arc<Mutex<Vec<String>>>);

impl ExecutionClient for Cancels {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, order_client_id: &str) {
        self.0.lock().unwrap().push(order_client_id.to_string());
    }
}

fn status_of(snap: &vike_execution::CoreSnapshot, order_client_id: &str) -> Option<OrderStatus> {
    snap.orders.iter().find(|o| o.order_client_id == order_client_id).map(|o| o.status)
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
    let venue_says = |event: Event| events.blocking_send(event).unwrap();

    // a resting reduce-only stop, explicit order client id
    handle
        .try_command(Command::Order(OrderIntent::Submit(Box::new(binance_perp_reduce_only_stop(
            "s1", 90_000.0,
        )))))
        .unwrap();
    venue_says(Event::OrderSubmitted(OrderSubmitted {
        order_client_id: "s1".into(),
        timestamp: 1,
    }));
    venue_says(Event::OrderAccepted(OrderAccepted {
        order_client_id: "s1".into(),
        venue_order_id: Some("v-s1".into()),
        timestamp: 1,
    }));
    wait_for_snapshot(&cell, 5, "s1 resting", |s| {
        status_of(s, "s1") == Some(OrderStatus::Accepted)
    });

    // move its trigger: binance cannot, so the node places a NEW stop under a minted order client id
    handle
        .try_command(Command::Order(OrderIntent::Modify {
            order_client_id: "s1".into(),
            new_qty: None,
            new_price: Some(91_000.0),
            new_trigger_price: None,
        }))
        .unwrap();
    wait_for_snapshot(&cell, 5, "the replacement stop is in the book", |s| {
        s.orders.iter().any(|o| o.order_client_id != "s1")
    });
    let new = cell
        .load_full()
        .orders
        .iter()
        .find(|o| o.order_client_id != "s1")
        .map(|o| (o.order_client_id.clone(), o.trigger_price))
        .expect("a replacement");
    assert_eq!(new.1, Some(91_000.0), "the replacement carries the new trigger");
    assert!(
        cancels.0.lock().unwrap().is_empty(),
        "the old stop stands until the new one is answered"
    );

    // the venue answers the new stop: the boundary drive now cancels the old one
    venue_says(Event::OrderSubmitted(OrderSubmitted {
        order_client_id: new.0.clone(),
        timestamp: 2,
    }));
    venue_says(Event::OrderAccepted(OrderAccepted {
        order_client_id: new.0.clone(),
        venue_order_id: Some("v-new".into()),
        timestamp: 2,
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
        order_client_id: "s1".into(),
        reason: "venue".to_string().into(),
        timestamp: 3,
        post_only_would_cross: false,
        refusal_id: None,
    }));
    wait_for_snapshot(&cell, 5, "the old stop is gone and the new one rests", |s| {
        status_of(s, "s1") == Some(OrderStatus::Canceled)
            && status_of(s, &new.0) == Some(OrderStatus::Accepted)
    });
    wait_for_snapshot(&cell, 5, "the amend is said in the recent events", |s| {
        s.recent_events.iter().any(|l| l.contains("replaced by") && l.contains(&new.0))
    });
}
