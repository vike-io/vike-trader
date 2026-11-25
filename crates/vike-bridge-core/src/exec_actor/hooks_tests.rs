//! The actor's venue hooks (`with_before_submit`, `with_adopt_restored`, `with_wire_id_for`): each answers its
//! `ExecutionClient` method through the venue's closure, and an actor given none answers as the trait's
//! default does.

use super::*;
use crate::execution_drive::drain_ready;
use std::sync::Mutex;
use std::sync::mpsc;
use vike_execution::event_channel;

type Lane = tokio::sync::mpsc::Receiver<vike_execution::Ingest>;

/// An actor whose command loop hands every queued order to the returned receiver.
fn actor() -> (ExecutionClientActor, mpsc::Receiver<OrderRequest>, Lane) {
    let (events, lane) = event_channel(16);
    let (queued_tx, queued) = mpsc::channel();
    let actor = ExecutionClientActor::spawn("hooks-test", events, move |rx| {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                ExecutionClientActorCommand::Submit(request, _) => {
                    queued_tx.send(*request).unwrap()
                }
                ExecutionClientActorCommand::Shutdown => break,
                _ => {}
            }
        }
    });
    (actor, queued, lane)
}

fn order(order_client_id: &str) -> OrderRequest {
    crate::execution_drive::limit("test", order_client_id, "BTCUSDT", 0.01, 50_000.0)
}

fn queued(rx: &mpsc::Receiver<OrderRequest>) -> OrderRequest {
    rx.recv_timeout(Duration::from_secs(5)).expect("the order reached the command loop")
}

/// A hook that stamps `ts` 42 and records each order client id it saw.
fn stamping(seen: &Arc<Mutex<Vec<String>>>) -> BeforeSubmitFn {
    let seen = Arc::clone(seen);
    Arc::new(move |request: &mut OrderRequest| {
        seen.lock().unwrap().push(request.order_client_id.clone());
        request.timestamp = 42;
    })
}

#[test]
fn an_actor_with_no_hook_answers_as_the_trait_default() {
    let (mut actor, queued_rx, mut lane) = actor();
    assert_eq!(actor.wire_id_for("c1"), None);
    actor.adopt_restored(&[RestoredOrder::new("c1", "ETHUSDT")]);
    let sent = order("c1");
    actor.submit(&sent);
    assert_eq!(queued(&queued_rx), sent, "the order goes as the caller gave it");
    assert!(drain_ready(|| lane.try_recv().ok()).is_empty(), "nothing is said for it here");
}

#[test]
fn before_submit_rewrites_the_order_the_loop_receives_and_runs_once_per_batch_order() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (actor, queued_rx, _lane) = actor();
    let mut actor = actor.with_before_submit(stamping(&seen));
    actor.submit(&order("c1"));
    let got = queued(&queued_rx);
    assert_eq!((got.order_client_id.as_str(), got.timestamp), ("c1", 42));
    actor.submit_batch(&[order("c2"), order("c3")]);
    assert_eq!(queued(&queued_rx).timestamp, 42);
    assert_eq!(queued(&queued_rx).timestamp, 42);
    assert_eq!(*seen.lock().unwrap(), ["c1", "c2", "c3"]);
}

#[test]
fn before_submit_runs_ahead_of_the_halt_reject_which_carries_what_it_wrote() {
    let halt = tempfile::NamedTempFile::new().expect("a HALT sentinel that exists");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (actor, queued_rx, mut lane) = actor();
    let mut actor =
        actor.with_halt_path(halt.path().to_path_buf()).with_before_submit(stamping(&seen));
    actor.submit(&order("halted"));
    match drain_ready(|| lane.try_recv().ok()).as_slice() {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.order_client_id, "halted");
            assert_eq!(r.reason, vike_execution::halt::HALT_REJECT_REASON);
            assert_eq!(r.timestamp, 42, "the reject carries the hook's stamp");
        }
        other => panic!("expected the HALT reject alone, got {other:?}"),
    }
    assert_eq!(*seen.lock().unwrap(), ["halted"], "the hook ran before the refusal");
    assert!(queued_rx.try_recv().is_err(), "a halted order is never queued");
}

#[test]
fn before_submit_runs_ahead_of_the_dead_thread_reject_which_carries_what_it_wrote() {
    let (events, mut lane) = event_channel(16);
    // The loop returns at once (a refused login); `detach` joins the thread, so its receiver is provably gone.
    let mut actor = ExecutionClientActor::spawn("hooks-dead", events, |_commands| {});
    actor.detach();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut actor = actor.with_before_submit(stamping(&seen));
    actor.submit(&order("orphan"));
    match drain_ready(|| lane.try_recv().ok()).as_slice() {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.order_client_id, "orphan");
            assert_eq!(r.reason, "venue exec thread unavailable");
            assert_eq!(r.timestamp, 42, "the reject carries the hook's stamp");
        }
        other => panic!("expected the dead-thread reject alone, got {other:?}"),
    }
}

#[test]
fn adopt_restored_hands_every_order_to_the_hook() {
    let taught = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&taught);
    let (actor, _queued, _lane) = actor();
    let mut actor = actor.with_adopt_restored(Arc::new(move |orders: &[RestoredOrder]| {
        sink.lock().unwrap().extend_from_slice(orders);
    }));
    let orders = [RestoredOrder::new("c1", "ETHUSDT"), RestoredOrder::new("c2", "")];
    actor.adopt_restored(&orders);
    assert_eq!(*taught.lock().unwrap(), orders);
}

#[test]
fn wire_id_for_answers_through_the_hook() {
    let (actor, _queued, _lane) = actor();
    let actor = actor.with_wire_id_for(Arc::new(|order_client_id: &str| {
        (!order_client_id.is_empty()).then(|| format!("wire-{order_client_id}"))
    }));
    assert_eq!(actor.wire_id_for("c1").as_deref(), Some("wire-c1"));
    assert_eq!(actor.wire_id_for(""), None);
}
