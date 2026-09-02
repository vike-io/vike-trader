//! A dead exec-actor thread must NOT silently swallow orders. When the venue `run()` thread has
//! exited (login failure, panic), `submit`/`cancel` land on a closed channel — the actor must
//! synthesize a terminal `OrderRejected` (submit) / advisory `OrderCancelRejected` (cancel) so the
//! core FSM never strands the order. Regression guard for the CLAUDE.md hard rule
//! ("no order may silently vanish").

use std::time::Duration;

use vike_bridge_core::exec_actor::ExecActor;
use vike_exec::{event_channel, ExecutionClient, Ingest};
use vike_model::events::Event;
use vike_model::OrderRequest;

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
