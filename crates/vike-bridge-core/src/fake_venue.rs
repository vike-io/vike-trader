//! The fake venue loop shared by the two HALT test binaries (`vike-bridge-core`'s
//! `exec_actor_halt.rs` and `vike-tradehub-core`'s `exec_actor_halt_unwired.rs`), compiled ONCE here
//! behind `test-support` (the `scripted` shape: only dev edges turn it on) instead of once per binary
//! that `#[path]`-included `tests/support/fake_venue.rs`. [`order`] is the request those binaries
//! and `exec_actor_dead_thread.rs` submit.

use std::sync::mpsc::Receiver;

use crate::exec_actor::{ExecCommand, cancel_batch_undeclared};
use vike_exec::EventSender;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderCanceled, OrderSubmitted};

/// The plain limit buy every `ExecActor` boundary test submits: venue `"test"`, `EURUSD`, qty 1 at
/// price 1, `ts` 5.
pub fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "test".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        ts: 5,
        ..Default::default()
    }
}

/// A fake venue loop that emits a marker event only when a command actually reaches the venue
/// thread: Submit -> `OrderSubmitted`, Cancel -> `OrderCanceled`. This lets a test distinguish
/// "refused at the submit boundary" (no `OrderSubmitted`; a synthesized `OrderRejected` instead)
/// from "passed through to the venue" (the loop emits the marker).
pub fn fake_run(events: EventSender, rx: Receiver<ExecCommand>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
            }
            ExecCommand::Cancel { client_order_id: coid, .. } => {
                let _ = events.blocking_send(Event::OrderCanceled(OrderCanceled {
                    client_order_id: coid,
                    reason: String::new().into(),
                    ts: 0,
                }));
            }
            // Unreachable: this fake declares no bulk lane, so `ExecActor` fans a batch out into
            // the per-id `Cancel`s above. Shaped like the real venue loops' arm.
            ExecCommand::CancelBatch { client_order_ids, .. } => {
                cancel_batch_undeclared(&events, &client_order_ids)
            }
            ExecCommand::Modify { .. } => {}
            ExecCommand::Shutdown => break,
        }
    }
}
