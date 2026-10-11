//! A dead exec-actor thread must NOT silently swallow orders. When the venue `run()` thread has
//! exited (login failure, panic), `submit`/`cancel` land on a closed channel — the actor must
//! synthesize a terminal `OrderRejected` (submit) / advisory `OrderCancelRejected` (cancel) so the
//! core FSM never strands the order. Regression guard for the CLAUDE.md hard rule
//! ("no order may silently vanish").

use vike_bridge_core::exec_actor::ExecActor;
use vike_exec::{ExecutionClient, event_channel};
use vike_model::OrderRequest;
use vike_model::events::Event;

#[path = "support/ingest.rs"]
mod ingest;

use ingest::recv_event;

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

/// Spawn an actor whose venue thread dies immediately, and block until its command receiver is
/// provably dropped — so the subsequent `submit`/`cancel` deterministically hit a closed channel.
fn dead_actor(events: vike_exec::EventSender) -> ExecActor {
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let actor = ExecActor::spawn("dead-venue", events, move |cmd_rx| {
        drop(cmd_rx); // venue login failed → the command receiver is gone
        let _ = done_tx.send(());
    });
    done_rx.recv().expect("dead-venue thread signalled");
    actor
}

#[test]
fn submit_to_dead_actor_synthesizes_terminal_rejection() {
    let (events, mut rx) = event_channel(16);
    let mut client = dead_actor(events);

    client.submit(&order("c1"));

    match recv_event(&mut rx) {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "c1");
            assert!(!r.reason.is_empty(), "rejection must carry a reason");
        }
        other => panic!("expected synthesized OrderRejected, got {other:?}"),
    }
}

#[test]
fn cancel_to_dead_actor_synthesizes_cancel_rejection() {
    let (events, mut rx) = event_channel(16);
    let mut client = dead_actor(events);

    client.cancel("c1");

    match recv_event(&mut rx) {
        Event::OrderCancelRejected(r) => assert_eq!(r.client_order_id, "c1"),
        other => panic!("expected synthesized OrderCancelRejected, got {other:?}"),
    }
}

// -- the core gone as well: ONE warning per actor, never one per dropped message ----------------

/// The core-gone warnings among `events`: the `vike_bridge_core` `warn!`s that say the core exited.
fn core_gone_warnings(
    events: &[vike_log::capture::CapturedEvent],
) -> Vec<&vike_log::capture::CapturedEvent> {
    events
        .iter()
        .filter(|e| e.level == tracing::Level::WARN && e.message.contains("the core has exited"))
        .collect()
}

/// ⚠ With the venue thread dead AND the core gone (the ingest receiver dropped), every synthesized
/// terminal event is dropped on the floor. That used to be silent. The first drop must raise ONE
/// `warn!` naming the actor, and the other dropped sends (submits, cancels, a batch) must stay
/// silent: this runs per order, so a line per drop would be a flood.
#[test]
fn a_core_that_is_gone_is_warned_about_once_per_actor_not_once_per_drop() {
    let (events, rx) = event_channel(16);
    let mut client = dead_actor(events);
    drop(rx); // the core thread has exited: every send on the lane now fails with `CoreGone`

    let ((), captured) = vike_log::capture::captured(|| {
        for i in 0..5 {
            client.submit(&order(&format!("c{i}")));
        }
        client.cancel("c0");
        client.cancel_batch_with_intent(
            &["c1".to_string(), "c2".to_string()],
            vike_exec::CancelIntent::Unspecified,
        );
    });

    let warnings = core_gone_warnings(&captured);
    assert_eq!(
        warnings.len(),
        1,
        "8 dropped sends must log exactly ONE core-gone warning, got: {captured:?}"
    );
    assert_eq!(warnings[0].target, "vike_bridge_core");
    assert_eq!(warnings[0].field("actor"), Some("dead-venue"), "the warning names the actor");
}

/// …and a SECOND actor on the same dead lane warns once on its own: the flag is per actor, not a
/// process-wide latch that would hide the second venue's silence.
#[test]
fn each_actor_warns_for_itself() {
    let (events, rx) = event_channel(16);
    let mut first = dead_actor(events.clone());
    let mut second = dead_actor(events);
    drop(rx);

    let ((), captured) = vike_log::capture::captured(|| {
        first.submit(&order("a1"));
        first.submit(&order("a2"));
        second.submit(&order("b1"));
        second.submit(&order("b2"));
    });

    assert_eq!(core_gone_warnings(&captured).len(), 2, "one per actor, got: {captured:?}");
}

/// The negative: a core that is alive costs no warning at all.
#[test]
fn a_live_core_logs_no_core_gone_warning() {
    let (events, mut rx) = event_channel(16);
    let mut client = dead_actor(events);

    let ((), captured) = vike_log::capture::captured(|| {
        client.submit(&order("c1"));
        client.submit(&order("c2"));
    });

    assert!(core_gone_warnings(&captured).is_empty(), "got: {captured:?}");
    let _ = recv_event(&mut rx);
}
