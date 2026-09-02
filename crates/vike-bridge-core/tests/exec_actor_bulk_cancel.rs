//! The bulk-cancel seam: what a venue command loop actually RECEIVES when the core cancels many
//! orders at once.
//!
//! Until `ExecCommand::CancelBatch` existed, `ExecActor::cancel_batch_with_intent` fanned every
//! batch out into `n` per-id `Cancel`s, so a venue's own `cancel_batch` was unreachable through
//! this actor no matter what that venue implemented — the batch was already shredded by the time
//! any venue code ran. These tests pin BOTH sides of the fix: a venue that DECLARED the lane
//! (`with_bulk_cancel`) sees exactly one command, and a venue that did not still sees the `n`
//! singles it always saw, in order, each carrying the intent.
//!
//! No network and no venue: the loop under test records commands and returns on Shutdown, which is
//! the only thing being asserted about — what crosses the channel.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use vike_bridge_core::exec_actor::{cancel_batch_undeclared, ExecActor, ExecCommand};
use vike_exec::{event_channel, CancelIntent, EventSender, ExecutionClient, Ingest};
use vike_model::events::Event;

/// The cancel-shaped commands a venue loop saw, in arrival order.
#[derive(Debug, PartialEq)]
enum Seen {
    Cancel(String, CancelIntent),
    Batch(Vec<String>, CancelIntent),
}

type Log = Arc<Mutex<Vec<Seen>>>;

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// An actor whose venue loop records the cancel commands it receives. `bulk` is the venue's own
/// declaration — the exact knob a real bridge turns in its `spawn` (Polymarket's `spawn_live` is
/// the only `true` in the tree).
fn recording_actor(events: EventSender, bulk: bool) -> (ExecActor, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let actor = ExecActor::spawn("recording-venue", events, move |rx: Receiver<ExecCommand>| {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                ExecCommand::Cancel { client_order_id, intent } => {
                    sink.lock().unwrap().push(Seen::Cancel(client_order_id, intent));
                }
                ExecCommand::CancelBatch { client_order_ids, intent } => {
                    sink.lock().unwrap().push(Seen::Batch(client_order_ids, intent));
                }
                ExecCommand::Shutdown => break,
                _ => {}
            }
        }
    });
    (if bulk { actor.with_bulk_cancel() } else { actor }, log)
}

/// Everything currently on the ingest lane (the receiver is a tokio mpsc; `try_recv` is
/// non-blocking, and every send these tests provoke happens on the CALLING thread).
fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::new();
    while let Ok(ing) = rx.try_recv() {
        if let Ingest::Event(e) = ing {
            out.push(e);
        }
    }
    out
}

/// THE POINT OF THE LANE: one batch in, ONE command out — ids and intent intact. `detach` sends
/// Shutdown and JOINS the loop, so everything queued ahead of it is recorded by the time it
/// returns.
#[test]
fn a_declared_venue_receives_the_batch_as_one_command() {
    let (events, _ingest) = event_channel(16);
    let (mut actor, log) = recording_actor(events, true);

    actor.cancel_batch_with_intent(&ids(&["c1", "c2", "c3"]), CancelIntent::RiskOff);
    actor.detach();

    assert_eq!(
        *log.lock().unwrap(),
        vec![Seen::Batch(ids(&["c1", "c2", "c3"]), CancelIntent::RiskOff)],
        "a declared venue must see ONE CancelBatch carrying every id and the batch's intent"
    );
}

/// The other fourteen venues: no declaration, so the fan-out that always ran still runs. This is
/// the byte-identical half — `n` singles, in the caller's order, each naming why it was issued —
/// and it is what lets the shared `CancelBatch` arm those loops must now compile stay unreachable.
#[test]
fn an_undeclared_venue_still_receives_n_individual_cancels() {
    let (events, _ingest) = event_channel(16);
    let (mut actor, log) = recording_actor(events, false);

    actor.cancel_batch_with_intent(&ids(&["c1", "c2", "c3"]), CancelIntent::Routine);
    actor.detach();

    assert_eq!(
        *log.lock().unwrap(),
        vec![
            Seen::Cancel("c1".to_string(), CancelIntent::Routine),
            Seen::Cancel("c2".to_string(), CancelIntent::Routine),
            Seen::Cancel("c3".to_string(), CancelIntent::Routine),
        ],
        "an undeclared venue must see the pre-existing per-id fan-out, in order, with the intent"
    );
}

/// A dead venue thread must not swallow a BATCH either (the no-silent-vanish contract): one
/// undeliverable batch has to look exactly like `n` undeliverable singles downstream, which means
/// one NON-terminal advisory per id — the orders, if live, are still resting.
#[test]
fn a_batch_to_a_dead_declared_thread_is_refused_per_id() {
    let (events, mut ingest) = event_channel(16);
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let mut actor = ExecActor::spawn("dead-venue", events, move |cmd_rx| {
        drop(cmd_rx); // venue login failed → the command receiver is gone
        let _ = done_tx.send(());
    })
    .with_bulk_cancel();
    done_rx.recv().expect("dead-venue thread signalled");

    actor.cancel_batch_with_intent(&ids(&["c1", "c2"]), CancelIntent::RiskOff);

    let refused: Vec<String> = drain(&mut ingest)
        .into_iter()
        .map(|ev| match ev {
            Event::OrderCancelRejected(r) => r.client_order_id,
            other => panic!("a dead-thread batch must be NON-terminal per id, got {other:?}"),
        })
        .collect();
    assert_eq!(refused, ids(&["c1", "c2"]));
}

/// The shared fallback every undeclared loop's `CancelBatch` arm calls. It is unreachable today —
/// the declaration is the interlock — but a mis-wiring must cost a loud refusal, never `n` orders
/// vanishing with no event at all.
#[test]
fn the_undeclared_fallback_refuses_every_id_non_terminally() {
    let (events, mut ingest) = event_channel(16);

    cancel_batch_undeclared(&events, &ids(&["a", "b"]));

    let seen = drain(&mut ingest);
    assert_eq!(seen.len(), 2, "one advisory per id: {seen:?}");
    for ev in &seen {
        assert!(
            matches!(ev, Event::OrderCancelRejected(_)),
            "the orders are still resting, so nothing here may be terminal: {ev:?}"
        );
    }
}
