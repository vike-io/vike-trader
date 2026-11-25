//! **An amend whose answer was lost settles to the level IG lists.** IG5, measured on the demo gateway: a modify of a
//! working order to 0.9196 lost its answer, the bridge rightly said its outcome was UNKNOWN, and nothing read the order
//! again, so the node kept 0.948 while IG most likely held 0.9196. Driven through the real exec client over `fake_ig`
//! (a hand-written loopback gateway in IG's documented `GET /workingorders` v2 shape, never IG): the PUT is answered
//! with a gateway 504, the exec thread reads the working-order list, and the order the node holds, folded through the
//! real `OrderRecord`, moves to IG's level.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_execution::{ExecutionClient, Ingest, RestoredOrder, event_channel};
use vike_ig::spawn_execution_client;
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::{OrderRecord, OrderRefusalId, OrderRequest, OrderType};

use crate::fake_ig::{self, Route};

const EPIC: &str = "CS.D.USDCHF.MINI.IP";
const DEAL_ID: &str = "DIAAAAWO1";
const ORDER_CLIENT_ID: &str = "c-stop";

#[test]
fn an_amend_whose_answer_was_lost_moves_the_order_to_the_level_ig_lists() {
    let ig = fake_ig::serve(vec![
        fake_ig::login_route(),
        Route::status("PUT /workingorders/otc/", 504, json!({ "errorCode": "gateway timeout" })),
        Route::ok(
            "GET /workingorders",
            json!({ "workingOrders": [{
                "workingOrderData": {
                    "dealId": DEAL_ID, "epic": EPIC, "direction": "SELL", "orderType": "STOP",
                    "orderSize": 1.0, "orderLevel": 0.9196, "timeInForce": "GOOD_TILL_CANCELLED",
                    "createdDateUTC": "2026-11-09T10:00:00"
                },
                "marketData": { "epic": EPIC }
            }] }),
        ),
    ]);
    let (events, mut rx) = event_channel(64);
    let mut client = spawn_execution_client(ig.config(), events, None);
    // the working order rests at IG under its `dealId`, as the core re-teaches it
    client.adopt_restored(&[RestoredOrder {
        venue_order_id: Some(DEAL_ID.into()),
        ..RestoredOrder::new(ORDER_CLIENT_ID, EPIC)
    }]);
    let stop = OrderRequest {
        order_client_id: ORDER_CLIENT_ID.into(),
        venue: "ig".into(),
        symbol: EPIC.into(),
        side: -1,
        qty: 1.0,
        order_type: OrderType::Stop,
        trigger_price: Some(0.948),
        ..Default::default()
    };

    client.modify(&stop, None, Some(0.9196));

    let (mut seen, deadline) = (Vec::new(), Instant::now() + Duration::from_secs(10));
    while Instant::now() < deadline && !seen.iter().any(|e| matches!(e, Event::OrderModified(_))) {
        match rx.try_recv() {
            Ok(Ingest::Event(event)) => seen.push(event),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    client.detach();

    let unknown = seen.iter().filter(|e| {
        matches!(e, Event::OrderModifyRejected(r)
            if r.refusal_id.as_deref() == Some(OrderRefusalId::OutcomeUnknown.word()))
    });
    assert_eq!(unknown.count(), 1, "the lost PUT is said first: {seen:?}");
    let mut order = OrderRecord::new(stop);
    order
        .apply(&Event::OrderSubmitted(OrderSubmitted {
            order_client_id: ORDER_CLIENT_ID.into(),
            timestamp: 1,
        }))
        .unwrap();
    order
        .apply(&Event::OrderAccepted(OrderAccepted {
            order_client_id: ORDER_CLIENT_ID.into(),
            venue_order_id: Some(DEAL_ID.into()),
            timestamp: 2,
        }))
        .unwrap();
    for event in &seen {
        order.apply(event).expect("every event the bridge sent folds");
    }
    assert_eq!(
        order.request.trigger_price,
        Some(0.9196),
        "the node's order moved to IG's level: {seen:?}"
    );
    assert_eq!(ig.count("GET /workingorders"), 1, "one read settled it: {:?}", ig.served());
}
