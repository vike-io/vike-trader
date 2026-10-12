//! The fake venue loop shared by the two HALT test binaries (`exec_actor_halt.rs`,
//! `exec_actor_halt_unwired.rs`); included only where every item is used.

use std::sync::mpsc::Receiver;

use vike_bridge_core::exec_actor::{ExecCommand, cancel_batch_undeclared};
use vike_exec::EventSender;
use vike_model::events::{Event, OrderCanceled, OrderSubmitted};

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
