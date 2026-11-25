//! A dead exec-actor thread must NOT silently swallow orders. When the venue `run()` loop has
//! returned (a refused login), `submit`/`cancel`/`modify` land on a closed channel — the actor must
//! synthesize a terminal `OrderRejected` (submit) / advisory `OrderCancelRejected` (cancel) /
//! advisory `OrderModifyRejected` (amend) so the core FSM never strands the order. When the loop
//! PANICKED, the thread stays to answer every command as never sent and the account's link reads
//! failed. Regression guard for the CLAUDE.md hard rule ("no order may silently vanish").

use std::sync::mpsc;
use std::time::Duration;

use vike_bridge_core::exec_actor::{
    DEAD_THREAD_REASON, ExecutionClientActor, ExecutionClientActorCommand,
};
use vike_bridge_core::fake_venue::order;
use vike_bridge_core::venue_link::venue_link;
use vike_execution::testing::recv_event;
use vike_execution::{ExecutionClient, event_channel};
use vike_model::events::Event;
use vike_model::feed_status::DataClientConnectionState;

/// Spawn an actor whose venue loop returns at once (a refused login), and join its thread (`detach`) so
/// its command receiver is provably dropped — so the subsequent commands deterministically hit a closed channel.
fn dead_actor(events: vike_execution::EventSender) -> ExecutionClientActor {
    let mut actor = ExecutionClientActor::spawn("dead-venue", events, |_commands| {});
    actor.detach();
    actor
}

#[test]
fn modify_to_dead_actor_synthesizes_modify_rejection() {
    let (events, mut rx) = event_channel(16);
    let mut client = dead_actor(events);

    client.modify(&order("c1"), Some(2.0), None);

    match recv_event(&mut rx) {
        Event::OrderModifyRejected(r) => assert_eq!(r.order_client_id, "c1"),
        other => panic!("expected synthesized OrderModifyRejected, got {other:?}"),
    }
}

/// **A loop that PANICS leaves a thread that still answers.** The submit it was sending when it died may have
/// reached the venue, so it gets no answer here (the core's confirm and reconcile settle it); the commands queued
/// behind it and every one after it were never sent and are answered so, each in its own shape; the account's link
/// reads failed (`Error`), never up and never a clean stop.
#[test]
fn a_panicking_exec_job_fails_the_link_and_answers_every_command_unsent() {
    let route_key = "binance#EXEC-PANIC";
    let (lane, mut rx) = event_channel(16);
    let events = lane.routed(route_key);
    let (release, released) = mpsc::channel::<()>();
    let (took, taken) = mpsc::channel::<String>();
    let mut client = ExecutionClientActor::spawn("panicking-venue", events, move |commands| {
        if let Ok(ExecutionClientActorCommand::Submit(request, _)) = commands.recv() {
            took.send(request.order_client_id).unwrap();
        }
        released.recv().unwrap(); // the venue call the next commands queue behind
        panic!("a bug in the venue's response mapper");
    });

    client.submit(&order("in-flight"));
    assert_eq!(taken.recv_timeout(Duration::from_secs(5)).unwrap(), "in-flight");
    client.submit(&order("queued"));
    client.cancel("resting");
    client.modify(&order("amended"), Some(2.0), None);
    release.send(()).unwrap();

    let mut answered = Vec::new();
    for _ in 0..3 {
        answered.push(recv_event(&mut rx));
    }
    client.submit(&order("after"));
    client.modify(&order("after-amend"), None, Some(1.5));
    answered.push(recv_event(&mut rx));
    answered.push(recv_event(&mut rx));
    let said: Vec<(&str, &str, &str)> = answered
        .iter()
        .map(|event| match event {
            Event::OrderRejected(r) => ("rejected", r.order_client_id.as_str(), r.reason.as_str()),
            Event::OrderCancelRejected(r) => {
                ("cancel refused", r.order_client_id.as_str(), r.reason.as_str())
            }
            Event::OrderModifyRejected(r) => {
                ("amend refused", r.order_client_id.as_str(), r.reason.as_str())
            }
            other => panic!("only never-sent answers, got {other:?}"),
        })
        .collect();
    assert_eq!(
        said,
        [
            ("rejected", "queued", DEAD_THREAD_REASON),
            ("cancel refused", "resting", DEAD_THREAD_REASON),
            ("amend refused", "amended", DEAD_THREAD_REASON),
            ("rejected", "after", DEAD_THREAD_REASON),
            ("amend refused", "after-amend", DEAD_THREAD_REASON),
        ],
        "the in-flight submit is left to reconcile, everything else is answered unsent"
    );
    assert_eq!(
        venue_link(route_key).state().connection_state(),
        DataClientConnectionState::Error,
        "the account's link reads failed"
    );

    client.detach(); // the dead thread ends on the actor's stop, so teardown still joins
    venue_link(route_key).reset();
}

#[test]
fn submit_to_dead_actor_synthesizes_terminal_rejection() {
    let (events, mut rx) = event_channel(16);
    let mut client = dead_actor(events);

    client.submit(&order("c1"));

    match recv_event(&mut rx) {
        Event::OrderRejected(r) => {
            assert_eq!(r.order_client_id, "c1");
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
        Event::OrderCancelRejected(r) => assert_eq!(r.order_client_id, "c1"),
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
            vike_model::OrderCancelIntent::Unspecified,
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
