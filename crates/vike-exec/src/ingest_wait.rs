//! Waiting on an ingest lane (the `event_channel()` receiver) from a TEST: the bounded wait the
//! `exec_actor_*` integration binaries of `vike-bridge-core` and `vike-tradehub-core` share, compiled
//! ONCE here under the `test-support` feature instead of once per binary that `#[path]`-included
//! `vike-bridge-core`'s `tests/support/ingest.rs`. Reached as `vike_exec::testing::recv_event`, and
//! as `vike_exec::testing::recv_event_within` by a live smoke whose venue round trips take seconds;
//! `vike_exec::testing::await_event` is the live smokes' wait for ONE order's terminal among the rest.
//!
//! No ambient clock read: the bound is tokio's timer, so this crate stays inside
//! `crates/vike-ops/tests/architecture/clock_pin.rs`'s "no ambient time read" scope.

use std::time::Duration;

use tokio::sync::mpsc::Receiver;
use vike_model::events::Event;

use crate::Ingest;

/// Block up to 10s for one `Ingest::Event`; panics on a timeout, a closed lane or a non-event.
pub fn recv_event(rx: &mut Receiver<Ingest>) -> Event {
    recv_event_within(rx, Duration::from_secs(10))
}

/// Drains `rx` until `pick` recognises an event or `within` passes: what `pick` mapped the first
/// recognised event to, or `None` on the deadline or a closed lane. Every other ingest is skipped —
/// a live smoke waiting for ITS order's terminal among everything else the venue says.
pub fn await_event<T>(
    rx: &mut Receiver<Ingest>,
    within: Duration,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> Option<T> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let drained = async {
            while let Some(ingest) = rx.recv().await {
                if let Ingest::Event(ev) = ingest
                    && let Some(found) = pick(&ev)
                {
                    return Some(found);
                }
            }
            None
        };
        tokio::time::timeout(within, drained).await.unwrap_or(None)
    })
}

/// [`recv_event`] with the caller's bound.
pub fn recv_event_within(rx: &mut Receiver<Ingest>, within: Duration) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(within, rx.recv()).await })
        .expect("timed out waiting for event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_channel;
    use crate::testing::canceled;

    #[test]
    fn await_event_skips_what_pick_ignores_and_answers_none_at_the_deadline_or_a_closed_lane() {
        let pick_b = |e: &Event| {
            matches!(e, Event::OrderCanceled(c) if c.client_order_id == "b").then_some("b")
        };
        let (tx, mut rx) = event_channel(8);
        tx.blocking_send(canceled("a", "", 0)).unwrap();
        tx.blocking_send(canceled("b", "", 0)).unwrap();
        assert_eq!(await_event(&mut rx, Duration::from_secs(5), pick_b), Some("b"));
        assert_eq!(await_event(&mut rx, Duration::from_millis(20), pick_b), None, "the deadline");
        drop(tx);
        assert_eq!(await_event(&mut rx, Duration::from_secs(5), pick_b), None, "a closed lane");
    }
}
