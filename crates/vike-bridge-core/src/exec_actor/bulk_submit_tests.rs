//! The actor's two declared lanes for a batch-first venue (X1b): `with_bulk_submit` queues ONE `SubmitBatch`
//! (all-or-nothing under a HALT), `with_adopt_queued` queues one `Adopt` per restored order on the cancel lane after
//! the caller-thread hook; an undeclared actor keeps the trait's fan-out and queues no adopt. Also `serving_since`,
//! and `run_loop` handing a too-old order to `VenueRest::submit_refused`.

use super::*;
use crate::execution_drive::drain_ready;
use crate::transport::VenueApiError;
use std::sync::Mutex;
use std::sync::mpsc;
use vike_execution::event_channel;

type Lane = tokio::sync::mpsc::Receiver<vike_execution::Ingest>;

/// What the command loop saw, in arrival order.
#[derive(Debug, PartialEq)]
enum Seen {
    Submit(String, i64),
    Batch(Vec<(String, i64)>),
    Cancel(String),
    Adopt(String, Option<String>),
}

/// An actor whose command loop records every command it receives.
fn recording(name: &str) -> (ExecutionClientActor, mpsc::Receiver<Seen>, Lane) {
    let (events, lane) = event_channel(32);
    let (seen_tx, seen) = mpsc::channel();
    let actor = ExecutionClientActor::spawn(name, events, move |rx| {
        while let Ok(cmd) = rx.recv() {
            let seen = match cmd {
                ExecutionClientActorCommand::Submit(r, _) => {
                    Seen::Submit(r.order_client_id.clone(), r.timestamp)
                }
                ExecutionClientActorCommand::SubmitBatch(rs, _) => {
                    Seen::Batch(rs.into_iter().map(|r| (r.order_client_id, r.timestamp)).collect())
                }
                ExecutionClientActorCommand::Cancel { order_client_id, .. } => {
                    Seen::Cancel(order_client_id)
                }
                ExecutionClientActorCommand::Adopt(o) => {
                    Seen::Adopt(o.order_client_id, o.venue_order_id)
                }
                ExecutionClientActorCommand::Shutdown => break,
                ExecutionClientActorCommand::CancelBatch { .. }
                | ExecutionClientActorCommand::Modify { .. } => continue,
            };
            seen_tx.send(seen).unwrap();
        }
    });
    (actor, seen, lane)
}

fn order(order_client_id: &str, ts: i64) -> OrderRequest {
    OrderRequest {
        timestamp: ts,
        ..crate::execution_drive::limit("test", order_client_id, "BTC", 0.01, 50_000.0)
    }
}

fn reducing(order_client_id: &str, ts: i64) -> OrderRequest {
    OrderRequest { reduce_only: true, ..order(order_client_id, ts) }
}

/// Everything the loop saw once `detach` joined it.
fn seen_after_detach(mut actor: ExecutionClientActor, seen: &mpsc::Receiver<Seen>) -> Vec<Seen> {
    actor.detach();
    seen.try_iter().collect()
}

#[test]
fn a_declared_batch_is_one_command_and_each_order_passes_the_hook() {
    let (actor, seen, mut lane) = recording("bulk-declared");
    let mut actor = actor
        .with_bulk_submit()
        .with_before_submit(Arc::new(|r: &mut OrderRequest| r.timestamp += 100));
    actor.submit_batch(&[order("a", 1), order("b", 2)]);
    actor.submit_batch(&[]);
    assert_eq!(
        seen_after_detach(actor, &seen),
        [Seen::Batch(vec![("a".into(), 101), ("b".into(), 102)])],
        "one command, the empty batch none"
    );
    assert!(drain_ready(|| lane.try_recv().ok()).is_empty(), "nothing is said for it here");
}

#[test]
fn an_undeclared_batch_fans_out_into_single_submits() {
    let (actor, seen, _lane) = recording("bulk-undeclared");
    let mut actor = actor;
    actor.submit_batch(&[order("a", 1), order("b", 2)]);
    assert_eq!(
        seen_after_detach(actor, &seen),
        [Seen::Submit("a".into(), 1), Seen::Submit("b".into(), 2)]
    );
}

/// The HALT boundary judges a declared batch as a unit: one opening leg refuses every order (terminally, the batch's
/// first `ts`) and nothing is queued; a batch of closes goes, each said in the shared admit line.
#[test]
fn under_a_halt_a_declared_batch_goes_whole_or_not_at_all() {
    let halt = tempfile::NamedTempFile::new().expect("a HALT sentinel that exists");
    let (actor, seen, mut lane) = recording("bulk-halt");
    let mut actor = actor.with_bulk_submit().with_halt_path(halt.path().to_path_buf());

    actor.submit_batch(&[reducing("exit", 7), order("open", 8)]);
    let refused = drain_ready(|| lane.try_recv().ok());
    let words: Vec<_> = refused
        .iter()
        .map(|event| match event {
            Event::OrderRejected(r) => {
                (r.order_client_id.as_str(), r.reason.to_string(), r.timestamp)
            }
            other => panic!("only terminal rejects, got {other:?}"),
        })
        .collect();
    let halt_words = vike_execution::halt::HALT_REJECT_REASON.to_string();
    assert_eq!(words, [("exit", halt_words.clone(), 7), ("open", halt_words, 7)]);

    let ((), log) =
        vike_log::capture::captured(|| actor.submit_batch(&[reducing("x1", 9), reducing("x2", 9)]));
    let admits = log.iter().filter(|e| e.target == "vike_bridge_core::halt").count();
    assert_eq!(admits, 2, "one admit line per order: {log:?}");
    assert_eq!(
        seen_after_detach(actor, &seen),
        [Seen::Batch(vec![("x1".into(), 9), ("x2".into(), 9)])],
        "only the batch of closes reached the loop"
    );
}

#[test]
fn a_declared_batch_on_a_dead_thread_rejects_every_order() {
    let (events, mut lane) = event_channel(16);
    let (gone_tx, gone) = mpsc::channel();
    let actor = ExecutionClientActor::spawn("bulk-dead", events, move |rx| {
        drop(rx);
        gone_tx.send(()).unwrap();
    });
    gone.recv_timeout(Duration::from_secs(5)).expect("the command loop exited");
    let mut actor = actor.with_bulk_submit();
    actor.submit_batch(&[order("a", 3), order("b", 4)]);
    let rejected: Vec<_> = drain_ready(|| lane.try_recv().ok())
        .into_iter()
        .map(|event| match event {
            Event::OrderRejected(r) => (r.order_client_id, r.reason.to_string(), r.timestamp),
            other => panic!("only terminal rejects, got {other:?}"),
        })
        .collect();
    let dead = "venue exec thread unavailable".to_string();
    assert_eq!(rejected, [("a".into(), dead.clone(), 3), ("b".into(), dead, 3)]);
}

/// The hook runs first (on the caller's thread), then one `Adopt` per order rides the lane a later cancel rides.
#[test]
fn a_declared_adopt_is_queued_after_the_hook_and_ahead_of_a_later_cancel() {
    let taught = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&taught);
    let (actor, seen, _lane) = recording("adopt-declared");
    let mut actor = actor.with_adopt_queued().with_adopt_restored(Arc::new(
        move |orders: &[RestoredOrder]| {
            sink.lock().unwrap().extend(orders.iter().map(|o| o.order_client_id.clone()));
        },
    ));
    let held =
        RestoredOrder { venue_order_id: Some("42".into()), ..RestoredOrder::new("c1", "BTC") };
    actor.adopt_restored(&[held, RestoredOrder::new("c2", "ETH")]);
    assert_eq!(*taught.lock().unwrap(), ["c1", "c2"], "the hook ran before this returned");
    actor.cancel("c1");
    assert_eq!(
        seen_after_detach(actor, &seen),
        [
            Seen::Adopt("c1".into(), Some("42".into())),
            Seen::Adopt("c2".into(), None),
            Seen::Cancel("c1".into())
        ]
    );
}

#[test]
fn an_undeclared_actor_queues_no_adopt() {
    let (actor, seen, _lane) = recording("adopt-undeclared");
    let mut actor = actor;
    actor.adopt_restored(&[RestoredOrder::new("c1", "BTC")]);
    assert!(seen_after_detach(actor, &seen).is_empty());
}

/// The shared refusal a loop with no bulk lane answers a batch with: every order `[Submitted, Rejected]`, none sent.
#[test]
fn the_undeclared_batch_arm_refuses_every_order_terminally() {
    let (events, mut lane) = event_channel(16);
    submit_batch_undeclared(&events, &[order("a", 1), order("b", 2)]);
    let said: Vec<_> = drain_ready(|| lane.try_recv().ok())
        .into_iter()
        .map(|event| match event {
            Event::OrderSubmitted(s) => format!("submitted {}", s.order_client_id),
            Event::OrderRejected(r) => {
                assert_eq!(r.reason, SUBMIT_BATCH_UNDECLARED_REASON);
                format!("rejected {}", r.order_client_id)
            }
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(said, ["submitted a", "rejected a", "submitted b", "rejected b"]);
}

#[test]
fn serving_since_is_the_queued_instant_inside_serving_submit_only() {
    let queued_at = Instant::now() - Duration::from_secs(3);
    assert_eq!(serving_since(), None);
    assert_eq!(serving_submit(queued_at, serving_since), Some(queued_at));
    assert_eq!(serving_since(), None);
}

/// A `VenueRest` that answers a too-old order its own way, and says which door was asked.
#[derive(Default)]
struct OwnRefusal {
    refused: Mutex<Vec<String>>,
    submitted: Mutex<Vec<String>>,
}

impl VenueRest for OwnRefusal {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        self.submitted.lock().unwrap().push(request.order_client_id.clone());
        Vec::new()
    }
    fn cancel_order(&self, _: &str) -> Result<OrderCancelAck, VenueApiError> {
        Ok(OrderCancelAck::Canceled)
    }
    fn submit_refused(&self, request: &OrderRequest, why: String) -> Vec<Event> {
        self.refused.lock().unwrap().push(request.order_client_id.clone());
        crate::rest::refused(request, why)
    }
}

#[test]
fn run_loop_hands_a_too_old_order_to_submit_refused_and_never_to_submit_order() {
    let (events, mut lane) = event_channel(16);
    let rest = OwnRefusal::default();
    let (tx, rx) = channel();
    let old = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    tx.send(ExecutionClientActorCommand::Submit(Box::new(order("old", 1)), old)).unwrap();
    tx.send(ExecutionClientActorCommand::Submit(Box::new(order("young", 2)), Instant::now()))
        .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&rest, &events, rx, Vec::<Event>::new, Duration::from_secs(3600));
    assert_eq!(*rest.refused.lock().unwrap(), ["old"]);
    assert_eq!(*rest.submitted.lock().unwrap(), ["young"]);
    let kinds: Vec<_> = drain_ready(|| lane.try_recv().ok())
        .iter()
        .map(|event| match event {
            Event::OrderSubmitted(_) => "submitted",
            Event::OrderRejected(_) => "rejected",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["submitted", "rejected"]);
}
