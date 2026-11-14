//! The per-account EVENT RING: the bounded, ordered record of every event a venue adapter produced, with
//! the position (`gateway_epoch`, `seq`) each one holds.
//!
//! The ring is what makes a link flap and a tradehub restart survivable without a venue-history
//! forward (B.8 of the wave-2 plan): an event the adapter produced while no client was attached is not
//! lost, it is waiting at a `seq`, and a client that presents the last `seq` it folded is replayed
//! everything after it, in order. It is also the ONLY queue between the adapter and a client's socket
//! (the writer pulls from it by `seq`), so a slow client costs the ring's bound and nothing else.
//!
//! # Rules
//!
//! * **`seq` is stamped BEFORE any drop decision.** [`EventRing::push`] assigns the next `seq` and
//!   only then evicts the oldest entry past the bound, so an eviction is a visible hole in the
//!   numbering (`floor` moves past it), never a silent renumbering.
//! * **The ring never drops silently.** A reader whose next `seq` has fallen below [`EventRing::floor`]
//!   is told so (`ReplayUnavailable{floor}`) and reconciles; [`EventRing::plan_resume`] answers the same
//!   for a client that presents a cursor older than the floor.
//! * **`gateway_epoch` is random per SESSION (see `session.rs`) and NOT ordered.** A cursor of another epoch
//!   is not "older" or "newer": it names a ring that no longer exists, and is answered as no cursor
//!   (`EventRingResume::Unavailable`), which is what W3c's old-epoch-straggler doubt asks of the gateway.
//! * `seq` starts at 1 and rises by exactly one per event inside an epoch; `0` means "nothing yet".

use std::collections::VecDeque;

use vike_model::EventRingCursor;
use vike_model::events::Event;

/// One event and its place in the ring.
#[derive(Debug, Clone)]
pub struct EventRingEntry {
    /// The event's position: rises by one per event, from 1.
    pub sequence: u64,
    /// The gatewayhub's clock when the event entered the ring, epoch ms. A bound on "everything at or
    /// before this has been seen", which is what the journal cursor uses it for.
    pub timestamp_ms: i64,
    /// The event, as the adapter produced it (already scoped to the account's route key).
    pub event: Event,
}

/// What a client that presents `cursor` at open is owed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventRingResume {
    /// No cursor: a fresh open. Nothing is replayed; the client reconciles for what it missed.
    Fresh,
    /// Deliver every entry from this `seq` on, in order, marked as replayed.
    Replay {
        /// The first `seq` to deliver (the cursor's `seq` + 1).
        from: u64,
    },
    /// The ring cannot honour the cursor (another epoch, an older `seq` than the floor, a `seq` from the
    /// future): tell the client and let it reconcile.
    Unavailable {
        /// The oldest `seq` the ring still holds (`0` = nothing yet).
        floor: u64,
    },
}

/// A bounded ring of events. See the module doc.
#[derive(Debug)]
pub struct EventRing {
    gateway_epoch: u64,
    cap: usize,
    entries: VecDeque<EventRingEntry>,
    last_sequence: u64,
}

impl EventRing {
    /// An empty ring of `gateway_epoch`, holding at most `cap` events (at least one).
    pub fn new(gateway_epoch: u64, cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            gateway_epoch,
            cap,
            entries: VecDeque::with_capacity(cap.min(1024)),
            last_sequence: 0,
        }
    }

    /// The epoch this ring belongs to.
    pub fn gateway_epoch(&self) -> u64 {
        self.gateway_epoch
    }

    /// Append `event`, stamp its `seq` and return it. The oldest entry past the bound is evicted AFTER
    /// the stamp.
    pub fn push(&mut self, ts_ms: i64, event: Event) -> u64 {
        self.last_sequence += 1;
        let seq = self.last_sequence;
        self.entries.push_back(EventRingEntry { sequence: seq, timestamp_ms: ts_ms, event });
        while self.entries.len() > self.cap {
            self.entries.pop_front();
        }
        seq
    }

    /// The `seq` of the newest event (`0` = nothing yet).
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// The `seq` of the oldest event still held (`0` = the ring is empty).
    pub fn floor(&self) -> u64 {
        self.entries.front().map_or(0, |e| e.sequence)
    }

    /// How many events are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ring holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry with this `seq`, if it is still held. `seq`s are contiguous, so this is an index.
    pub fn get(&self, seq: u64) -> Option<&EventRingEntry> {
        let first = self.entries.front()?.sequence;
        let at = usize::try_from(seq.checked_sub(first)?).ok()?;
        self.entries.get(at)
    }

    /// What a client presenting `cursor` is owed. See [`EventRingResume`].
    ///
    /// A cursor at `seq` 0 of THIS epoch is a client that has heard nothing: "from the first entry" while the
    /// ring still holds it (a client anchors there when it opened an empty ring, W19-D3), `Unavailable` once it
    /// has been evicted.
    pub fn plan_resume(&self, cursor: Option<&EventRingCursor>) -> EventRingResume {
        let Some(cursor) = cursor else { return EventRingResume::Fresh };
        if cursor.gateway_epoch != self.gateway_epoch || cursor.sequence > self.last_sequence {
            return EventRingResume::Unavailable { floor: self.floor() };
        }
        if cursor.sequence == self.last_sequence {
            // Caught up (or nothing has ever happened): nothing to replay, live events follow.
            return EventRingResume::Replay { from: self.last_sequence + 1 };
        }
        // The client needs seq cursor.seq + 1 ..= last_sequence, so the oldest held must not be later.
        let floor = self.floor();
        if floor != 0 && cursor.sequence + 1 >= floor {
            EventRingResume::Replay { from: cursor.sequence + 1 }
        } else {
            EventRingResume::Unavailable { floor }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::OrderAccepted;

    fn make_event(n: u64) -> Event {
        Event::OrderAccepted(OrderAccepted {
            order_client_id: format!("c{n}"),
            venue_order_id: None,
            timestamp: 1,
        })
    }

    fn order_client_id(e: &EventRingEntry) -> String {
        match &e.event {
            Event::OrderAccepted(a) => a.order_client_id.clone(),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sequence_rises_by_one_from_one_and_zero_means_nothing_yet() {
        let mut ring = EventRing::new(7, 8);
        assert_eq!((ring.last_sequence(), ring.floor(), ring.len()), (0, 0, 0));
        assert!(ring.is_empty());
        assert_eq!(ring.push(10, make_event(1)), 1);
        assert_eq!(ring.push(11, make_event(2)), 2);
        assert_eq!((ring.last_sequence(), ring.floor()), (2, 1));
        assert_eq!(order_client_id(ring.get(2).expect("held")), "c2");
        assert!(ring.get(3).is_none() && ring.get(0).is_none());
    }

    #[test]
    fn an_overflow_evicts_the_oldest_after_the_stamp_and_moves_the_floor() {
        let mut ring = EventRing::new(7, 3);
        for n in 1..=5 {
            assert_eq!(ring.push(n, make_event(n.unsigned_abs())), n.unsigned_abs());
        }
        // 1 and 2 are gone; the numbering did not shift.
        assert_eq!((ring.floor(), ring.last_sequence(), ring.len()), (3, 5, 3));
        assert!(ring.get(2).is_none());
        assert_eq!(order_client_id(ring.get(3).expect("held")), "c3");
        assert_eq!(order_client_id(ring.get(5).expect("held")), "c5");
    }

    #[test]
    fn a_cursor_of_this_epoch_inside_the_ring_replays_from_the_next_sequence() {
        let mut ring = EventRing::new(7, 4);
        for n in 1..=4 {
            ring.push(n, make_event(n.unsigned_abs()));
        }
        let at = |seq| EventRingCursor { gateway_epoch: 7, sequence: seq, timestamp_ms: 0 };
        assert_eq!(ring.plan_resume(Some(&at(2))), EventRingResume::Replay { from: 3 });
        assert_eq!(ring.plan_resume(Some(&at(0))), EventRingResume::Replay { from: 1 });
        // Caught up: nothing to replay, live events follow.
        assert_eq!(ring.plan_resume(Some(&at(4))), EventRingResume::Replay { from: 5 });
        assert_eq!(ring.plan_resume(None), EventRingResume::Fresh);
    }

    #[test]
    fn a_cursor_older_than_the_floor_a_foreign_epoch_and_the_future_are_all_unavailable() {
        let mut ring = EventRing::new(7, 3);
        for n in 1..=6 {
            ring.push(n, make_event(n.unsigned_abs()));
        }
        // floor is 4: a cursor at 2 needs seq 3, which is gone; a cursor at 3 needs 4, which is held.
        let at =
            |epoch, seq| EventRingCursor { gateway_epoch: epoch, sequence: seq, timestamp_ms: 0 };
        assert_eq!(ring.plan_resume(Some(&at(7, 2))), EventRingResume::Unavailable { floor: 4 });
        assert_eq!(ring.plan_resume(Some(&at(7, 3))), EventRingResume::Replay { from: 4 });
        // Another epoch is not older or newer: it names a ring that no longer exists.
        assert_eq!(ring.plan_resume(Some(&at(8, 5))), EventRingResume::Unavailable { floor: 4 });
        assert_eq!(ring.plan_resume(Some(&at(8, 0))), EventRingResume::Unavailable { floor: 4 });
        // A seq the ring never produced.
        assert_eq!(ring.plan_resume(Some(&at(7, 99))), EventRingResume::Unavailable { floor: 4 });
    }

    #[test]
    fn an_empty_ring_replays_nothing_to_a_cursor_at_zero_and_refuses_one_ahead() {
        let ring = EventRing::new(7, 4);
        let at = |seq| EventRingCursor { gateway_epoch: 7, sequence: seq, timestamp_ms: 0 };
        assert_eq!(ring.plan_resume(Some(&at(0))), EventRingResume::Replay { from: 1 });
        assert_eq!(ring.plan_resume(Some(&at(1))), EventRingResume::Unavailable { floor: 0 });
    }
}
