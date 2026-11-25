//! **An amend that waited past `MAX_MODIFY_WAIT` on the exec thread's queue is refused unsent, a fresh one is
//! sent, and a cancel queued behind stale amends goes out** (`vike_bridge_core::exec_actor`'s `run_loop` twin, on
//! IG's own loop; the CI box 2026-11-19: a maker's amends queued behind a slow venue and each went out minutes old).
//! Driven through the real exec client over `fake_ig` (a hand-written loopback gateway, never IG): IG answers the
//! first cancel only after `MAX_MODIFY_WAIT`, so the two amends the actor stamped behind it are stale when the
//! thread reaches them. Each holds a `dealId`, so without the age check each would have been PUT.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::exec_actor::MAX_MODIFY_WAIT;
use vike_execution::{ExecutionClient, Ingest, RestoredOrder, event_channel};
use vike_ig::spawn_execution_client;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

use crate::fake_ig::{self, Route};

const EPIC: &str = "CS.D.USDCHF.MINI.IP";

#[test]
fn a_stale_amend_is_refused_unsent_and_a_cancel_behind_stale_amends_goes_out() {
    let deleted = json!({ "dealStatus": "ACCEPTED", "status": "DELETED" });
    let ig = fake_ig::serve(vec![
        fake_ig::login_route(),
        Route {
            delay: MAX_MODIFY_WAIT + Duration::from_millis(500),
            ..Route::ok(
                "DELETE /workingorders/otc/DEAL_SLOW",
                json!({ "dealReference": "del-slow" }),
            )
        },
        Route::ok("DELETE /workingorders/otc/DEAL_REST", json!({ "dealReference": "del-rest" })),
        Route::ok("GET /confirms/del", deleted),
        Route::ok("PUT /workingorders/otc/DEAL_FRESH", json!({ "dealReference": "amd-fresh" })),
        Route::ok("GET /confirms/amd", json!({ "dealStatus": "ACCEPTED", "status": "AMENDED" })),
    ]);
    let (events, mut rx) = event_channel(64);
    let mut client = spawn_execution_client(ig.config(), events, None);
    let resting = [
        ("slow", "DEAL_SLOW"),
        ("stale-1", "DEAL_S1"),
        ("stale-2", "DEAL_S2"),
        ("resting", "DEAL_REST"),
        ("fresh", "DEAL_FRESH"),
    ];
    client.adopt_restored(&resting.map(|(order_client_id, deal_id)| RestoredOrder {
        venue_order_id: Some(deal_id.into()),
        ..RestoredOrder::new(order_client_id, EPIC)
    }));
    let limit = |order_client_id: &str| OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "ig".into(),
        symbol: EPIC.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(0.9),
        ..Default::default()
    };
    let mut seen = Vec::new();
    let mut pump = |seen: &mut Vec<Event>, done: &dyn Fn(&Event) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !seen.iter().any(done) {
            match rx.try_recv() {
                Ok(Ingest::Event(event)) => seen.push(event),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    };

    client.cancel("slow");
    client.modify(&limit("stale-1"), None, Some(0.91));
    client.modify(&limit("stale-2"), None, Some(0.92));
    client.cancel("resting");
    let rested = |e: &Event| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "resting");
    pump(&mut seen, &rested);
    client.modify(&limit("fresh"), None, Some(0.93));
    pump(&mut seen, &|e: &Event| matches!(e, Event::OrderModified(_)));
    client.detach();

    let label = |event: &Event| match event {
        Event::OrderCanceled(c) => format!("canceled {}", c.order_client_id),
        Event::OrderModifyRejected(r) => format!("amend refused {}", r.order_client_id),
        Event::OrderModified(m) => format!("modified {}", m.order_client_id),
        other => format!("{other:?}"),
    };
    assert_eq!(
        seen.iter().map(label).collect::<Vec<_>>(),
        [
            "canceled slow",
            "amend refused stale-1",
            "amend refused stale-2",
            "canceled resting",
            "modified fresh",
        ]
    );
    for event in &seen {
        if let Event::OrderModifyRejected(refused) = event {
            assert!(
                refused.reason.contains("NOT sent") && refused.reason.contains("5 s"),
                "{}",
                refused.reason
            );
        }
    }
    assert_eq!(
        ig.count("PUT /workingorders/otc/"),
        1,
        "only the fresh amend reached IG: {:?}",
        ig.served()
    );
    assert_eq!(ig.count("PUT /workingorders/otc/DEAL_FRESH"), 1);
    assert_eq!(
        ig.count("DELETE /workingorders/otc/DEAL_REST"),
        1,
        "the cancel behind them went out"
    );
}
