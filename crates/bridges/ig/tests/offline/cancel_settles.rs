//! **A cancel whose DELETE lost its reply is OUTCOME UNKNOWN and settles on IG's activity** (S4b IG-1, decision 0303).
//! The DELETE's send failure read as "never sent" and was answered as a refused cancel with nothing reading the order
//! again, while IG may have deleted it. Driven through the real exec client over `fake_ig` (a hand-written loopback
//! gateway, never IG): the socket closes with the DELETE read and no answer, the cancel is said of unknown outcome with
//! `order.outcome_unknown`, the exec thread reads IG's activity history (a row in the documented `GET /history/activity`
//! v3 shape naming the order deleted) and the order ends, and the DELETE was sent once.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_execution::{ExecutionClient, Ingest, RestoredOrder, event_channel};
use vike_ig::spawn_execution_client;
use vike_model::OrderRefusalId;
use vike_model::events::Event;

use crate::fake_ig::{self, Route};

const EPIC: &str = "CS.D.USDCHF.MINI.IP";
const DEAL_ID: &str = "DIAAAAWO1";
const ORDER_CLIENT_ID: &str = "c-limit";

#[test]
fn a_delete_whose_reply_was_lost_is_said_unknown_and_ends_on_igs_activity() {
    let ig = fake_ig::serve(vec![
        fake_ig::login_route(),
        Route::hang_up("DELETE /workingorders/otc/"),
        Route::ok(
            "GET /history/activity",
            json!({ "activities": [{
                "date": "2026-11-23T10:00:05", "epic": EPIC, "period": "-", "dealId": "DIAAAANEW",
                "channel": "PUBLIC_WEB_API", "type": "WORKING_ORDER", "status": "ACCEPTED",
                "description": "Working order deleted",
                "details": { "dealReference": "IGREF1",
                             "actions": [{ "actionType": "WORKING_ORDER_DELETED", "affectedDealId": DEAL_ID }] }
            }], "metadata": { "paging": { "size": 1, "next": null } } }),
        ),
    ]);
    let (events, mut rx) = event_channel(64);
    let mut client = spawn_execution_client(ig.config(), events, None);
    // the working order rests at IG under its `dealId`, as the core re-teaches it
    client.adopt_restored(&[RestoredOrder {
        venue_order_id: Some(DEAL_ID.into()),
        ..RestoredOrder::new(ORDER_CLIENT_ID, EPIC)
    }]);

    client.cancel(ORDER_CLIENT_ID);

    let (mut seen, deadline) = (Vec::new(), Instant::now() + Duration::from_secs(10));
    while Instant::now() < deadline && !seen.iter().any(|e| matches!(e, Event::OrderCanceled(_))) {
        match rx.try_recv() {
            Ok(Ingest::Event(event)) => seen.push(event),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    client.detach();

    let answers: Vec<&Event> = seen
        .iter()
        .filter(|e| matches!(e, Event::OrderCancelRejected(_) | Event::OrderCanceled(_)))
        .collect();
    assert!(
        matches!(answers.as_slice(), [Event::OrderCancelRejected(r), Event::OrderCanceled(c)]
            if r.order_client_id == ORDER_CLIENT_ID
                && r.refusal_id.as_deref() == Some(OrderRefusalId::OutcomeUnknown.word())
                && c.order_client_id == ORDER_CLIENT_ID),
        "the lost DELETE is said of unknown outcome, then IG's activity ends the order: {seen:?}"
    );
    assert_eq!(ig.count("DELETE /workingorders/otc/"), 1, "never sent again: {:?}", ig.served());
    assert_eq!(ig.count("GET /history/activity"), 1, "one read settled it: {:?}", ig.served());
}
