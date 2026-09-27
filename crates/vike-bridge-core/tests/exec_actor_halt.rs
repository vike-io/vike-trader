//! HALT file sentinel kill-switch at the `ExecActor` submit boundary. While the sentinel file
//! exists, a submit must be REFUSED with a synthesized terminal `OrderRejected` (no silent vanish —
//! the CLAUDE.md venue-adapter contract), and the order must NOT reach the venue thread. Cancels
//! must STILL pass through while halted (halt lets you reduce/exit — it must never trap a position).
//! Removing the file lets submits proceed again.
//!
//! The sentinel path is injected via `ExecActor::with_halt_path` so these tests never mutate the
//! process env (the repo avoids `set_var` under threads — see `credentials.rs`); each test uses its
//! own unique temp path so they run in parallel without interfering.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use vike_bridge_core::HALT_REJECT_REASON;
use vike_bridge_core::exec_actor::{ExecActor, ExecCommand, cancel_batch_undeclared};
use vike_exec::{EventSender, ExecutionClient, Ingest, event_channel};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderCanceled, OrderSubmitted};

/// A fake venue loop that emits a marker event only when a command actually reaches the venue
/// thread: Submit -> `OrderSubmitted`, Cancel -> `OrderCanceled`. This lets a test distinguish
/// "refused at the submit boundary" (no `OrderSubmitted`; a synthesized `OrderRejected` instead)
/// from "passed through to the venue" (the loop emits the marker).
fn fake_run(events: EventSender, rx: Receiver<ExecCommand>) {
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

/// A unique, not-yet-existing sentinel path in the temp dir (pid + counter → no collision across
/// parallel tests or stale runs).
fn unique_halt_path() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("vike-halt-{}-{n}.sentinel", std::process::id()))
}

fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "test".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        ts: 5,
        ..Default::default()
    }
}

fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .expect("timed out waiting for event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

fn actor_with_halt(halt_file: PathBuf) -> (ExecActor, tokio::sync::mpsc::Receiver<Ingest>) {
    let (events, rx) = event_channel(16);
    let client = ExecActor::spawn("halt-test", events.clone(), move |rx| fake_run(events, rx))
        .with_halt_path(halt_file);
    (client, rx)
}

#[test]
fn submit_refused_and_terminal_rejection_synthesized_while_halted() {
    let halt_file = unique_halt_path();
    std::fs::write(&halt_file, b"").expect("touch HALT sentinel");
    let (mut client, mut rx) = actor_with_halt(halt_file.clone());

    client.submit(&order("during-halt"));

    // The ONLY event must be the synthesized terminal rejection — the intent did not vanish, and
    // the venue thread never saw it (no OrderSubmitted marker).
    match recv_event(&mut rx) {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "during-halt");
            assert_eq!(r.reason, HALT_REJECT_REASON);
        }
        other => panic!("expected synthesized OrderRejected under halt, got {other:?}"),
    }

    let _ = std::fs::remove_file(&halt_file);
}

#[test]
fn cancel_passes_through_while_halted() {
    let halt_file = unique_halt_path();
    std::fs::write(&halt_file, b"").expect("touch HALT sentinel");
    let (mut client, mut rx) = actor_with_halt(halt_file.clone());

    // Halt must never trap a position: a cancel still reaches the venue thread (OrderCanceled).
    client.cancel("resting-1");

    match recv_event(&mut rx) {
        Event::OrderCanceled(c) => assert_eq!(c.client_order_id, "resting-1"),
        other => panic!("expected OrderCanceled to pass through halt, got {other:?}"),
    }

    let _ = std::fs::remove_file(&halt_file);
}

#[test]
fn submit_proceeds_after_halt_file_removed() {
    let halt_file = unique_halt_path();
    let (mut client, mut rx) = actor_with_halt(halt_file.clone());

    // Halted: refused.
    std::fs::write(&halt_file, b"").expect("touch HALT sentinel");
    client.submit(&order("during-halt"));
    match recv_event(&mut rx) {
        Event::OrderRejected(r) => assert_eq!(r.client_order_id, "during-halt"),
        other => panic!("expected OrderRejected while halted, got {other:?}"),
    }

    // Operator clears the sentinel → the next submit proceeds to the venue thread (OrderSubmitted).
    std::fs::remove_file(&halt_file).expect("remove HALT sentinel");
    client.submit(&order("post-halt"));
    match recv_event(&mut rx) {
        Event::OrderSubmitted(s) => assert_eq!(s.client_order_id, "post-halt"),
        other => panic!("expected OrderSubmitted after halt cleared, got {other:?}"),
    }
}

// ── a HALT must not trap you in a position ───────────────────────────────────────────────────
//
// `cancel_passes_through_while_halted` above carries the comment "Halt must never trap a
// position", and for this sentinel's whole life that was the ONLY thing discharging it — which is
// not enough, and the gap was real. A cancel removes a resting ORDER; getting out of a POSITION
// means SENDING one. `OrderIntent::Flatten` mints a `reduce_only` MARKET for `|position|`, and it
// was refused right here, so `market-exit` under a HALT file cancelled everything and then closed
// nothing. `vike_bridge_core::halt::halt_admits_submit` is the rule that fixes it.

/// The exact shape `OrderIntent::Flatten` mints — `reduce_only`, MARKET, closing side — must reach
/// the venue thread with the sentinel engaged.
#[test]
fn a_reduce_only_submit_passes_through_halt_so_the_operator_can_still_get_out() {
    let halt_file = unique_halt_path();
    std::fs::write(&halt_file, b"").expect("touch HALT sentinel");
    let (mut client, mut rx) = actor_with_halt(halt_file.clone());

    let flatten = OrderRequest {
        client_order_id: "flatten-me".into(),
        venue: "test".into(),
        symbol: "EURUSD".into(),
        side: -1, // closing a long
        qty: 2.0,
        order_type: "market".into(),
        reduce_only: true,
        ts: 5,
        ..Default::default()
    };
    client.submit(&flatten);

    // The venue thread saw it (`fake_run` emits OrderSubmitted only when a command actually
    // arrives), so this is "reached the venue", not merely "was not rejected".
    match recv_event(&mut rx) {
        Event::OrderSubmitted(s) => assert_eq!(s.client_order_id, "flatten-me"),
        other => panic!("a reduce_only exit must reach the venue under HALT, got {other:?}"),
    }

    let _ = std::fs::remove_file(&halt_file);
}

/// THE MUTATION SENTINEL for the test above, in ONE actor so the two verdicts cannot come from
/// different setups: with the same sentinel engaged, an OPENING order is still refused with a
/// synthesized terminal rejection while the reducing one goes through. A change that simply stopped
/// enforcing HALT would pass the test above and fail this.
#[test]
fn halt_still_refuses_an_opening_submit_while_admitting_the_reducing_one() {
    let halt_file = unique_halt_path();
    std::fs::write(&halt_file, b"").expect("touch HALT sentinel");
    let (mut client, mut rx) = actor_with_halt(halt_file.clone());

    // opening (side +1, no reduce_only) — refused, terminal rejection synthesized
    client.submit(&order("opening"));
    match recv_event(&mut rx) {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "opening");
            assert_eq!(r.reason, HALT_REJECT_REASON, "refused BY THE HALT, not by something else");
        }
        other => panic!("an opening order must still be refused under HALT, got {other:?}"),
    }

    // reducing — admitted, same actor, same engaged sentinel
    let mut exit = order("reducing");
    exit.reduce_only = true;
    exit.side = -1;
    client.submit(&exit);
    match recv_event(&mut rx) {
        Event::OrderSubmitted(s) => assert_eq!(s.client_order_id, "reducing"),
        other => panic!("the reducing order must reach the venue, got {other:?}"),
    }

    let _ = std::fs::remove_file(&halt_file);
}

/// ⚠ WHAT THIS BOUNDARY CANNOT DO, pinned so nobody later mistakes it for a verified check.
/// `ExecActor` holds no position book, so `halt_admits_submit` can only read the caller-asserted
/// flag: a `reduce_only` order on a FLAT book — an OPENING order that a strategy bug mis-tagged —
/// is admitted here. `vike_exec::RiskGate` is the layer that refuses it (`is_covered_reduce`
/// verifies side and coverage against the real position), and every submit reaching this boundary
/// has already passed it. This test documents the asymmetry rather than asserting it is fine.
#[test]
fn this_boundary_trusts_the_flag_because_it_cannot_see_a_position() {
    let halt_file = unique_halt_path();
    std::fs::write(&halt_file, b"").expect("touch HALT sentinel");
    let (mut client, mut rx) = actor_with_halt(halt_file.clone());

    // Indistinguishable from a genuine exit AT THIS LAYER: same flag, same shape, no position.
    let mut mistagged = order("mistagged-open");
    mistagged.reduce_only = true;
    client.submit(&mistagged);

    match recv_event(&mut rx) {
        Event::OrderSubmitted(s) => assert_eq!(s.client_order_id, "mistagged-open"),
        other => panic!("the flag is all this layer has, so it is admitted here: {other:?}"),
    }

    let _ = std::fs::remove_file(&halt_file);
}

// ── a blocked modify must SAY it was blocked (kill-switch gap 5) ─────────────────────────────
//
// The verdict was never in doubt: a modify can add size or chase price, `reduce_only` describes the
// ORIGINAL order, and the exit path under a halt is CANCEL. What was in doubt was whether the
// operator could tell. This arm used to `return` with no event and no log line, so a halted modify
// and a modify the venue thread had dropped produced the identical observation — no event, resting
// order unchanged — and `docs/ops/kill-switches.md` carried that as a register entry against three
// clients of four (cTrader was the one that spoke).

/// A halted modify is REFUSED and SAYS SO, with the shared halt wording, exactly once.
///
/// The advisory is deliberately NON-terminal (`vike_exec::order`'s FSM maps `OrderModifyRejected`
/// back to MODIFIABLE): the resting order keeps its terms, so this adds an observation and changes
/// no verdict and no state.
///
/// ⚠ Mutation proof: delete the `blocking_send` from `ExecActor::modify`'s halt arm and this goes
/// red on the timeout; delete the whole `halt_engaged()` arm and it goes red on the `OrderModified`
/// the venue thread then emits.
#[test]
fn a_halted_modify_is_refused_out_loud_rather_than_dropped_in_silence() {
    let halt_file = unique_halt_path();
    std::fs::write(&halt_file, b"").expect("touch HALT sentinel");
    let (mut client, mut rx) = actor_with_halt(halt_file.clone());

    client.modify(&order("resting-1"), Some(9.0), Some(101.0));

    match recv_event(&mut rx) {
        Event::OrderModifyRejected(m) => {
            assert_eq!(m.client_order_id, "resting-1");
            assert_eq!(
                m.reason, HALT_REJECT_REASON,
                "the SAME bytes the submit refusal carries, so one recogniser matches both"
            );
        }
        other => panic!(
            "a halted modify must be refused OUT LOUD — a silent one is indistinguishable from a \
             lost one, which is what an operator holding the kill switch must not have to guess \
             about. Got: {other:?}"
        ),
    }

    // …and the mutation sentinel in the SAME actor: with the sentinel gone the same amend reaches
    // the venue thread, so the assertion above is about the HALT and not about a broken modify.
    // `fake_run`'s `Modify` arm emits nothing, so the proof is the SUBMIT that follows it arriving
    // — the venue thread is alive and taking commands.
    std::fs::remove_file(&halt_file).expect("disengage");
    client.modify(&order("resting-1"), Some(9.0), Some(101.0));
    client.submit(&order("after"));
    match recv_event(&mut rx) {
        Event::OrderSubmitted(s) => assert_eq!(
            s.client_order_id, "after",
            "the un-halted modify emitted no advisory of its own — the next command is what proves \
             the channel is live"
        ),
        other => panic!(
            "with the sentinel gone nothing may be refused, and no advisory may be emitted for the \
             modify. Got: {other:?}"
        ),
    }
}
