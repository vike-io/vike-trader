//! The fake venue loop shared by the two HALT test binaries (`vike-bridge-core`'s
//! `exec_actor_halt.rs` and `vike-tradehub-core`'s `exec_actor_halt_unwired.rs`), compiled ONCE here
//! behind `test-support` (the `scripted` shape: only dev edges turn it on) instead of once per binary
//! that `#[path]`-included `tests/support/fake_venue.rs`. [`order`] is the request those binaries
//! and `exec_actor_dead_thread.rs` submit.

use std::sync::mpsc::Receiver;

use crate::exec_actor::{
    ExecutionClientActorCommand, adopt_undeclared, cancel_batch_undeclared, submit_batch_undeclared,
};
use vike_execution::EventSender;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderCanceled, OrderSubmitted};

/// The plain limit buy every `ExecutionClientActor` boundary test submits: venue `"test"`, `EURUSD`, qty 1 at
/// price 1, `ts` 5.
pub fn order(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "test".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        timestamp: 5,
        ..Default::default()
    }
}

/// A fake venue loop that emits a marker event only when a command actually reaches the venue
/// thread: Submit -> `OrderSubmitted`, Cancel -> `OrderCanceled`. This lets a test distinguish
/// "refused at the submit boundary" (no `OrderSubmitted`; a synthesized `OrderRejected` instead)
/// from "passed through to the venue" (the loop emits the marker).
pub fn fake_run(events: EventSender, rx: Receiver<ExecutionClientActorCommand>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecutionClientActorCommand::Submit(req, _) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: req.order_client_id.clone(),
                    timestamp: req.timestamp,
                }));
            }
            ExecutionClientActorCommand::Cancel { order_client_id, .. } => {
                let _ = events.blocking_send(Event::OrderCanceled(OrderCanceled {
                    order_client_id,
                    reason: String::new().into(),
                    timestamp: 0,
                }));
            }
            // Unreachable: this fake declares no bulk lane, so `ExecutionClientActor` fans a batch out into
            // the per-id `Cancel`s above. Shaped like the real venue loops' arm.
            ExecutionClientActorCommand::CancelBatch { order_client_ids, .. } => {
                cancel_batch_undeclared(&events, &order_client_ids)
            }
            ExecutionClientActorCommand::SubmitBatch(requests, _) => {
                submit_batch_undeclared(&events, &requests)
            }
            ExecutionClientActorCommand::Adopt(order) => adopt_undeclared(&order),
            ExecutionClientActorCommand::Modify { .. } => {}
            ExecutionClientActorCommand::Shutdown => break,
        }
    }
}
