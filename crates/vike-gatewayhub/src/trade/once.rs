//! ONE record per fact: the first-wins memory in front of an account's event ring.
//!
//! Every venue reports the same order fact through up to three producers that cannot see each other: the
//! command loop (`vike_bridge_core::exec_actor::run_loop` answers a submit with `OrderAccepted` and a cancel
//! with `OrderCanceled { ts: 0, reason: "" }` the moment the venue's REST call returns), the venue's private
//! stream (its own row), and the history replay (the gap-sentinel re-lists a window of venue history five
//! seconds after EVERY command and at every reconnect). Measured on the v0.1.88 node (DEMO, 2026-10-25): one
//! far order, placed and cancelled, left `OrderAccepted` x2 and `OrderCanceled` x3 in the journal on Binance,
//! OKX and Hyperliquid. The engine folded each copy as a no-op, but the journal, the ring and `recent` kept all
//! of them. Every producer's events pass `AccountSession::emit_event` and nothing else, so the repeat is
//! stopped here, once, for every venue, outside the core fold.
//!
//! # The rules
//!
//! * **Keyed by client order id inside ONE account** (the session is one `(venue, account)`; two accounts
//!   never share a memory), **first report wins, per class.** The classes are `OrderAccepted` and the
//!   terminal words `OrderCanceled` / `OrderExpired`. Fills, partial fills, rejects, modifies and every
//!   non-order event are never looked at (a fill's dedup is the engine's trade id).
//! * **Only an order this session saw ACCEPTED can repeat.** A terminal for a coid the memory never saw
//!   accepted (a restart, a rebuilt adapter, an order placed before this ring existed) passes untouched, and
//!   so does a lone event of any class.
//! * **`OrderAccepted`**: the first one that NAMES the venue's order passes and is remembered; a later one is
//!   dropped. An accept that names none (the resolver's optimistic answer to an UNKNOWN submit, which the client
//!   does not take for the venue's word on the order) always passes and is NOT remembered: remembering it would
//!   make the venue's own stamped terminal, which is what resolves that order, read as a repeat of the loop's
//!   synthetic one.
//! * **Terminal**: the first passes. Once the VENUE has spoken (a terminal stamped with its time) every later
//!   terminal of the order is dropped, whatever its word. While the only one so far is the LOOP's synthetic
//!   cancel (`ts: 0`, no reason: the answer to OUR cancel, not the venue's word on the order; "too late" and
//!   "not found" are swallowed into it), a venue-stamped terminal of the SAME word is its repeat and is
//!   dropped, but one of ANOTHER word (an `OrderExpired` after our `OrderCanceled`) is a disagreement about the
//!   order's final state and passes once.
//! * **Bounded**: a FIFO ring of coids, `cap` of them. An order older than that is forgotten and its replay
//!   would pass again, which is what it did before this module.
//! * **A Submit the dispatch thread admits starts a NEW LIFE of its coid** ([`OnceMemory::forget`]). The Submit
//!   dedup set (`crate::trade::dedup`) is per ADAPTER, so a changed start or a panic rebuild empties it and the
//!   same coid is admitted again while this memory still holds the first life as ended; its accept and
//!   terminal would be dropped as repeats and the order would stick at Initialized.
//! * **Counted**: every dropped repeat is counted per class ([`Repeats`]) and the session reports the totals when
//!   it is retired.
//! * **The memory spans one unbroken client view of the ring.** It is cleared with the ring on a rebuild and
//!   when a client opens WITHOUT a replayable cursor (`clear`): that client reconciles and may leave an order
//!   UNKNOWN, and the venue's own row is what resolves it (the client treats the synthetic cancel as an
//!   advisory, `vike-gatewayhub-client`'s `synthetic_cancel_coid`).
//!
//! shortcut: the loop's synthetic cancel is told by its shape (`ts == 0` and an empty reason), as the client
//! does; a venue whose OWN cancel row carries neither (polymarket, ibkr) reads as synthetic here, which only
//! makes the memory more permissive, upgrade if a venue ever needs the loop to mark it.

use std::collections::{HashMap, VecDeque};

use vike_model::events::Event;

/// Which terminal word a report carries. A bit each, in [`Seen::words`].
#[derive(Debug, Clone, Copy)]
enum Word {
    Canceled,
    Expired,
}

impl Word {
    fn bit(self) -> u8 {
        match self {
            Word::Canceled => 1,
            Word::Expired => 2,
        }
    }
}

/// Who has ended the order so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// No terminal has passed.
    No,
    /// Only the loop's synthetic cancel has: the venue has not spoken.
    Synthetic,
    /// A terminal stamped by the venue has: the venue has spoken.
    Venue,
}

/// What the memory holds of one order (it exists only once an accept NAMING the venue's order passed).
#[derive(Debug, Clone, Copy)]
struct Seen {
    ended: Ended,
    /// The terminal words that passed, as [`Word::bit`]s.
    words: u8,
}

/// How many repeats a memory has dropped, per class, since it was built (a counter, not a bound: `u64`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Repeats {
    /// Dropped `OrderAccepted`s.
    pub accepted: u64,
    /// Dropped `OrderCanceled` / `OrderExpired`s.
    pub terminal: u64,
}

/// The first-wins memory. See the module doc.
#[derive(Debug)]
pub struct OnceMemory {
    cap: usize,
    order: VecDeque<String>,
    seen: HashMap<String, Seen>,
    repeats: Repeats,
}

impl OnceMemory {
    /// An empty memory of at most `cap` orders (at least one).
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            order: VecDeque::new(),
            seen: HashMap::new(),
            repeats: Repeats::default(),
        }
    }

    /// Whether `event` is a first report (`true`: let it through, it is remembered) or a repeat of a fact the
    /// ring already carries (`false`: drop it, and count it).
    pub fn admit(&mut self, event: &Event) -> bool {
        let (admitted, count) = match event {
            Event::OrderAccepted(a) => {
                let named = a.venue_order_id.as_ref().is_some_and(|v| !v.is_empty());
                (self.accepted(&a.client_order_id, named), &mut self.repeats.accepted)
            }
            Event::OrderCanceled(c) => {
                let synthetic = c.ts == 0 && c.reason.is_empty();
                (
                    self.ended(&c.client_order_id, Word::Canceled, synthetic),
                    &mut self.repeats.terminal,
                )
            }
            Event::OrderExpired(e) => {
                (self.ended(&e.client_order_id, Word::Expired, false), &mut self.repeats.terminal)
            }
            _ => return true,
        };
        if !admitted {
            *count = count.saturating_add(1);
        }
        admitted
    }

    /// The repeats dropped so far.
    pub fn repeats(&self) -> Repeats {
        self.repeats
    }

    /// A new life of `coid` begins (the dispatch thread admitted a Submit for it): what was remembered of an earlier
    /// one is forgotten, so the new order's accept and terminal are first reports.
    pub fn forget(&mut self, coid: &str) {
        // The coid stays in `order` until it ages out; its slot there only evicts a newer entry of the same coid
        // early, which forgets more, never less.
        self.seen.remove(coid);
    }

    /// Forget everything: the next report of every order is a first one. The counts are kept.
    pub fn clear(&mut self) {
        self.order.clear();
        self.seen.clear();
    }

    /// How many orders are remembered.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    fn accepted(&mut self, coid: &str, named: bool) -> bool {
        if coid.is_empty() {
            return true;
        }
        if self.seen.contains_key(coid) {
            return false;
        }
        // An accept that names no venue order passes and is not remembered (module doc).
        if named {
            self.order.push_back(coid.to_string());
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.seen.remove(&old);
                }
            }
            self.seen.insert(coid.to_string(), Seen { ended: Ended::No, words: 0 });
        }
        true
    }

    fn ended(&mut self, coid: &str, word: Word, synthetic: bool) -> bool {
        let Some(seen) = self.seen.get_mut(coid) else { return true };
        let first = match seen.ended {
            Ended::No => true,
            Ended::Synthetic => !synthetic && seen.words & word.bit() == 0,
            Ended::Venue => false,
        };
        if first {
            seen.ended = if synthetic { Ended::Synthetic } else { Ended::Venue };
            seen.words |= word.bit();
        }
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::{
        OrderAccepted, OrderCancelRejected, OrderCanceled, OrderExpired, OrderRejected,
    };

    fn accepted(coid: &str) -> Event {
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.into(),
            venue_order_id: Some("v1".into()),
            ts: 5,
        })
    }

    fn bare_accepted(coid: &str) -> Event {
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.into(),
            venue_order_id: None,
            ts: 5,
        })
    }

    /// The loop's answer to our cancel: `cancel_event`'s shape.
    fn loop_canceled(coid: &str) -> Event {
        Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.into(),
            reason: "".into(),
            ts: 0,
        })
    }

    /// A venue row (the stream's or the replay's): stamped with the venue's time.
    fn venue_canceled(coid: &str) -> Event {
        Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.into(),
            reason: "user".into(),
            ts: 9,
        })
    }

    fn venue_expired(coid: &str) -> Event {
        Event::OrderExpired(OrderExpired { client_order_id: coid.into(), ts: 9 })
    }

    fn passes(mem: &mut OnceMemory, events: &[Event]) -> Vec<bool> {
        events.iter().map(|e| mem.admit(e)).collect()
    }

    #[test]
    fn the_accept_the_loop_states_and_the_stream_states_again_is_one_record() {
        let mut mem = OnceMemory::new(8);
        // The loop, the venue's NEW row, and a later replay of the open order.
        assert_eq!(
            passes(&mut mem, &[accepted("a"), accepted("a"), accepted("a")]),
            [true, false, false]
        );
    }

    #[test]
    fn a_cancel_the_loop_the_stream_and_every_replay_state_is_one_record() {
        let mut mem = OnceMemory::new(8);
        let got = passes(
            &mut mem,
            &[
                accepted("a"),
                loop_canceled("a"),
                venue_canceled("a"),
                // The gap-sentinel, once after this command and again after the next.
                venue_canceled("a"),
                venue_canceled("a"),
            ],
        );
        assert_eq!(got, [true, true, false, false, false]);
    }

    #[test]
    fn the_stream_row_that_beats_the_rest_answer_wins_and_the_loop_answer_is_the_repeat() {
        let mut mem = OnceMemory::new(8);
        assert_eq!(
            passes(&mut mem, &[accepted("a"), venue_canceled("a"), loop_canceled("a")]),
            [true, true, false]
        );
    }

    #[test]
    fn a_venue_row_of_another_word_after_the_loops_cancel_is_a_disagreement_and_passes_once() {
        let mut mem = OnceMemory::new(8);
        assert_eq!(
            passes(
                &mut mem,
                &[
                    accepted("a"),
                    loop_canceled("a"),
                    venue_expired("a"),
                    // Now the venue has spoken: neither word, from anyone, passes again.
                    venue_expired("a"),
                    venue_canceled("a"),
                    loop_canceled("a"),
                ]
            ),
            [true, true, true, false, false, false]
        );
    }

    #[test]
    fn once_the_venue_has_spoken_a_later_terminal_of_any_word_is_dropped() {
        let mut mem = OnceMemory::new(8);
        assert_eq!(
            passes(&mut mem, &[accepted("a"), venue_expired("a"), venue_canceled("a")]),
            [true, true, false]
        );
    }

    #[test]
    fn a_lone_event_of_every_class_passes_untouched() {
        let mut mem = OnceMemory::new(8);
        assert!(mem.admit(&accepted("lone-accept")));
        // A terminal for an order this memory never saw accepted (a restart, a rebuilt adapter): not a repeat,
        // however often the venue says it.
        for _ in 0..3 {
            assert!(mem.admit(&venue_canceled("never-accepted")));
            assert!(mem.admit(&venue_expired("never-accepted")));
            assert!(mem.admit(&loop_canceled("never-accepted")));
        }
        assert_eq!(mem.len(), 1, "a terminal without an accept remembers nothing");
    }

    #[test]
    fn an_accept_that_names_no_venue_order_passes_and_is_never_remembered() {
        let mut mem = OnceMemory::new(8);
        // Bare accepts pass however often they come and leave nothing behind, so the venue's stamped terminal that
        // resolves an UNKNOWN order is never a repeat of the loop's synthetic one.
        assert_eq!(
            passes(
                &mut mem,
                &[
                    bare_accepted("a"),
                    bare_accepted("a"),
                    loop_canceled("a"),
                    venue_canceled("a"),
                    venue_canceled("a"),
                ]
            ),
            [true, true, true, true, true]
        );
        assert!(mem.is_empty());
        // The first accept that names the venue's order is remembered; a bare one after it is its repeat.
        assert_eq!(
            passes(&mut mem, &[accepted("b"), bare_accepted("b"), accepted("b")]),
            [true, false, false]
        );
    }

    #[test]
    fn forget_makes_the_next_life_of_a_coid_a_first_report_and_leaves_the_others() {
        let mut mem = OnceMemory::new(8);
        assert!(mem.admit(&accepted("a")) && mem.admit(&accepted("b")));
        assert!(mem.admit(&loop_canceled("a")) && !mem.admit(&venue_canceled("a")));
        mem.forget("a");
        // a's second life: its accept and its terminal are first reports; b is untouched.
        assert_eq!(
            passes(
                &mut mem,
                &[accepted("a"), loop_canceled("a"), venue_canceled("a"), accepted("b")]
            ),
            [true, true, false, false]
        );
        mem.forget("never-seen");
    }

    #[test]
    fn every_dropped_repeat_is_counted_by_class_and_clear_keeps_the_counts() {
        let mut mem = OnceMemory::new(8);
        assert_eq!(mem.repeats(), Repeats::default());
        let _ = passes(
            &mut mem,
            &[
                accepted("a"),
                accepted("a"),
                accepted("a"),
                loop_canceled("a"),
                venue_canceled("a"),
                venue_expired("a"),
                venue_expired("a"),
                bare_accepted("c"),
            ],
        );
        // Two accepts dropped; the venue's cancel (same word as the loop's) and the second expiry dropped.
        assert_eq!(mem.repeats(), Repeats { accepted: 2, terminal: 2 });
        mem.clear();
        assert_eq!(mem.repeats(), Repeats { accepted: 2, terminal: 2 });
    }

    #[test]
    fn fills_rejects_modifies_and_advisories_are_never_judged() {
        let mut mem = OnceMemory::new(8);
        assert!(mem.admit(&accepted("a")));
        let reject = Event::OrderRejected(OrderRejected {
            client_order_id: "a".into(),
            reason: "r".into(),
            ts: 1,
        });
        let advisory = Event::OrderCancelRejected(OrderCancelRejected {
            client_order_id: "a".into(),
            reason: "r".into(),
            ts: 0,
        });
        for _ in 0..3 {
            assert!(mem.admit(&reject) && mem.admit(&advisory));
        }
    }

    #[test]
    fn an_event_without_a_coid_is_never_judged() {
        let mut mem = OnceMemory::new(8);
        assert!(mem.admit(&accepted("")) && mem.admit(&accepted("")));
        assert!(mem.is_empty());
    }

    #[test]
    fn the_memory_is_a_ring_the_oldest_order_is_forgotten_and_its_repeat_passes_again() {
        let mut mem = OnceMemory::new(2);
        assert!(
            mem.admit(&accepted("a")) && mem.admit(&accepted("b")) && mem.admit(&accepted("c"))
        );
        assert_eq!(mem.len(), 2, "bounded");
        assert!(!mem.admit(&accepted("b")) && !mem.admit(&accepted("c")), "still remembered");
        assert!(mem.admit(&accepted("a")), "forgotten: a first report again");
        assert_eq!(mem.len(), 2);
    }

    #[test]
    fn clear_makes_every_next_report_a_first_one() {
        let mut mem = OnceMemory::new(8);
        assert!(mem.admit(&accepted("a")) && mem.admit(&venue_canceled("a")));
        assert!(!mem.admit(&venue_canceled("a")));
        mem.clear();
        assert!(mem.is_empty());
        assert!(mem.admit(&venue_canceled("a")), "unwitnessed again: passes untouched");
        assert!(mem.admit(&accepted("a")));
    }
}
