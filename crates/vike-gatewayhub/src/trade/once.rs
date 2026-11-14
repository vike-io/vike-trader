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
//! stopped here, once, for every venue, outside the core fold. The same holds for an EXECUTION: the stream and
//! the replay report one fill twice (`FillEvent 25115f7a14 0.001@80512` twice in a row in a node's `recent`
//! ring, v0.1.88); the engine drops the second by its exec id, so the books were right, and the journal and the
//! ring carried both.
//!
//! # The rules
//!
//! * **Keyed by order client id inside ONE account** (the session is one `(venue, account)`; two accounts
//!   never share a memory), **first report wins, per class.** The order classes are `OrderAccepted` and the
//!   terminal words `OrderCanceled` / `OrderExpired`. Rejects, modifies and every non-order event are never
//!   looked at; the fill classes are the next bullet.
//! * **Only an order this session saw ACCEPTED can repeat.** A terminal or a fill for an order client id the memory never
//!   saw accepted (a restart, a rebuilt adapter, an order placed before this ring existed) passes untouched, and
//!   so does a lone event of any class.
//! * **A fill is MONEY, so it is dropped only when it is CERTAINLY the same execution** that already passed in
//!   the same lane (`Fill`, `OrderPartiallyFilled` or `OrderFilled`) for an order the memory holds accepted: the
//!   same venue exec id AND the same order AND the same instrument and side AND the same quantity AND the same
//!   price (compared bit for bit; the first four are the engine's own fill print and its order, the fee and the
//!   time a replay restates are not part of the identity). Anything else passes untouched, because a wrongly
//!   kept duplicate is only noise (the engine drops it by exec id) where a wrongly dropped fill is a wrong
//!   order or position: the same exec id with other numbers, another order, instrument or side (it does not
//!   take the first one's place); every other execution of the same order (partial fills have different exec
//!   ids, and so do two fills of the same size and price); a number that is not finite, the commission
//!   included (the engine refuses it BEFORE it burns the exec id, so a corrected retransmission must reach
//!   it); a fill that names no order of ours (a foreign execution is the reconcile's business). An EMPTY exec id
//!   cannot be told apart from anything and cannot exist (`TradeId` refuses it at every door). The ACCEPTED
//!   condition is load-bearing for the wraps: a wrap that finds its order still Submitted (the venue's fill
//!   overtook its accept) is refused by the engine without burning the exec id, and only a redelivery after
//!   the accept completes the order (`known_gap_a_fill_that_finds_the_order_submitted_is_refused_and_not_kept`);
//!   a wrap first told before the accept is therefore never remembered, and its redelivery passes. The three
//!   lanes are told apart: the bare report and each wrap of one execution are three facts, each told once.
//!   The final `OrderFilled` is judged by its own execution like the others, never by `(order, class)`: venues
//!   that wrap EVERY fill in an `OrderFilled` (fxcm, alpaca) would lose their second one. The window is the
//!   memory's `cap` in executions; a fill older than that, or told after [`OnceMemory::clear`], passes again.
//!   A new life of an order client id forgets its executions too (below).
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
//! * **Bounded**: a FIFO ring of order client ids, `cap` of them (and, apart, one of executions). An order older than that
//!   is forgotten and its replay would pass again, which is what it did before this module.
//! * **A Submit the dispatch thread admits starts a NEW LIFE of its order client id** ([`OnceMemory::forget`]). The Submit
//!   dedup set (`crate::trade::dedup`) is per ADAPTER, so a changed start or a panic rebuild empties it and the
//!   same order client id is admitted again while this memory still holds the first life as ended; its accept and
//!   terminal would be dropped as repeats and the order would stick at Initialized. The executions told under
//!   the order client id are forgotten with it (a marker fill a venue mints from the order client id alone, ctrader's and ig's, would
//!   otherwise read as the first life's).
//! * **Counted**: every dropped repeat is counted per class ([`OnceMemoryRepeats`]: accepts, terminals, fills) and the
//!   session reports the totals when it is retired.
//! * **The memory spans one unbroken client view of the ring.** It is cleared with the ring on a rebuild, when a
//!   client opens WITHOUT a replayable cursor and the moment the ring evicts events an attached client has not read (`clear`):
//!   that client reconciles and may leave an order UNKNOWN, and the venue's own row is what resolves it (the
//!   client treats the synthetic cancel as an advisory, `vike-gatewayhub-client`'s `synthetic_cancel_order_client_id`); a
//!   fill that fell out of the ring unread is recovered by the venue's redelivery, which only a cleared memory
//!   lets through.
//!
//! shortcut: the loop's synthetic cancel is told by its shape (`ts == 0` and an empty reason), as the client
//! does; a venue whose OWN cancel row carries neither (polymarket, ibkr) reads as synthetic here, which only
//! makes the memory more permissive, upgrade if a venue ever needs the loop to mark it.

use std::collections::{HashMap, VecDeque};

use vike_model::FiniteNumbers;
use vike_model::events::{Event, FillEvent, TradeId};

/// Which event carried a fill. A bit each, in [`Execution::lanes`].
#[derive(Debug, Clone, Copy)]
enum Lane {
    /// `Event::Fill`, the bare execution report.
    Bare,
    /// `Event::OrderPartiallyFilled`.
    Partial,
    /// `Event::OrderFilled`.
    Filled,
}

impl Lane {
    fn bit(self) -> u8 {
        match self {
            Lane::Bare => 1,
            Lane::Partial => 2,
            Lane::Filled => 4,
        }
    }
}

/// What the memory holds of one execution (the first report of its exec id).
#[derive(Debug)]
struct Execution {
    /// The order the report named: the fill's own for a bare report, the wrap's for a wrap.
    order_client_id: String,
    /// The instrument and the side the fill states (the engine's own fill print holds both, so a re-stamped
    /// symbol or a flipped side is not certainly the same execution).
    symbol: String,
    side: i32,
    /// The reported quantity and price as bits: an identity test, never arithmetic.
    qty: u64,
    price: u64,
    /// The lanes it already passed in, as [`Lane::bit`]s.
    lanes: u8,
}

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
    /// How many executions were remembered under this order (an upper bound: the execution ring may have aged some
    /// out). Zero means no scan for them is owed when the order leaves.
    execs: u32,
}

/// How many repeats a memory has dropped, per class, since it was built (a counter, not a bound: `u64`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OnceMemoryRepeats {
    /// Dropped `OrderAccepted`s.
    pub accepted: u64,
    /// Dropped `OrderCanceled` / `OrderExpired`s.
    pub terminal: u64,
    /// Dropped `Fill` / `OrderPartiallyFilled` / `OrderFilled`s.
    pub fill: u64,
}

/// The first-wins memory. See the module doc.
#[derive(Debug)]
pub struct OnceMemory {
    cap: usize,
    order: VecDeque<String>,
    seen: HashMap<String, Seen>,
    /// The executions that passed, by exec id (`order`'s twin for fills; `execution_order` is its FIFO, with stale slots
    /// after a [`OnceMemory::forget`]).
    execs: HashMap<TradeId, Execution>,
    execution_order: VecDeque<TradeId>,
    repeats: OnceMemoryRepeats,
}

impl OnceMemory {
    /// An empty memory of at most `cap` orders and `cap` executions (at least one each).
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            order: VecDeque::new(),
            seen: HashMap::new(),
            execs: HashMap::new(),
            execution_order: VecDeque::new(),
            repeats: OnceMemoryRepeats::default(),
        }
    }

    /// Whether `event` is a first report (`true`: let it through, it is remembered) or a repeat of a fact the
    /// ring already carries (`false`: drop it, and count it).
    pub fn admit(&mut self, event: &Event) -> bool {
        let (admitted, count) = match event {
            Event::OrderAccepted(a) => {
                let named = a.venue_order_id.as_ref().is_some_and(|v| !v.is_empty());
                (self.accepted(&a.order_client_id, named), &mut self.repeats.accepted)
            }
            Event::OrderCanceled(c) => {
                let synthetic = c.timestamp == 0 && c.reason.is_empty();
                (
                    self.ended(&c.order_client_id, Word::Canceled, synthetic),
                    &mut self.repeats.terminal,
                )
            }
            Event::OrderExpired(e) => {
                (self.ended(&e.order_client_id, Word::Expired, false), &mut self.repeats.terminal)
            }
            Event::Fill(f) => {
                (self.execution(Lane::Bare, &f.order_client_id, f), &mut self.repeats.fill)
            }
            Event::OrderPartiallyFilled(w) => {
                (self.execution(Lane::Partial, &w.order_client_id, &w.fill), &mut self.repeats.fill)
            }
            Event::OrderFilled(w) => {
                (self.execution(Lane::Filled, &w.order_client_id, &w.fill), &mut self.repeats.fill)
            }
            _ => return true,
        };
        if !admitted {
            *count = count.saturating_add(1);
        }
        admitted
    }

    /// The repeats dropped so far.
    pub fn repeats(&self) -> OnceMemoryRepeats {
        self.repeats
    }

    /// A new life of `order_client_id` begins (the dispatch thread admitted a Submit for it): what was remembered of an earlier
    /// one, its executions included, is forgotten, so the new order's accept, terminal and fills are first reports.
    pub fn forget(&mut self, order_client_id: &str) {
        // The order client id stays in `order` (and its executions in `execution_order`) until it ages out; a slot there only
        // evicts a newer entry of the same key early, which forgets more, never less.
        if let Some(gone) = self.seen.remove(order_client_id) {
            self.forget_executions(order_client_id, gone);
        }
    }

    /// The executions told under `order_client_id` leave with the order that held them. A scan of at most `cap` executions, owed
    /// only to an order that had some: every execution is remembered under an order the memory holds accepted, and an
    /// order leaves `seen` only through here, [`Self::accepted`]'s ageing or [`Self::clear`].
    fn forget_executions(&mut self, order_client_id: &str, gone: Seen) {
        if gone.execs > 0 {
            self.execs.retain(|_, e| e.order_client_id != order_client_id);
        }
    }

    /// Forget everything: the next report of every order and every execution is a first one. The counts are kept.
    pub fn clear(&mut self) {
        self.order.clear();
        self.seen.clear();
        self.execution_order.clear();
        self.execs.clear();
    }

    /// How many executions are remembered.
    pub fn executions(&self) -> usize {
        self.execs.len()
    }

    /// How many orders are remembered.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    fn accepted(&mut self, order_client_id: &str, named: bool) -> bool {
        if order_client_id.is_empty() {
            return true;
        }
        if self.seen.contains_key(order_client_id) {
            return false;
        }
        // An accept that names no venue order passes and is not remembered (module doc).
        if named {
            self.order.push_back(order_client_id.to_string());
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front()
                    && let Some(gone) = self.seen.remove(&old)
                {
                    self.forget_executions(&old, gone);
                }
            }
            self.seen
                .insert(order_client_id.to_string(), Seen { ended: Ended::No, words: 0, execs: 0 });
        }
        true
    }

    /// Whether a fill reported in `lane` for order `order_client_id` is a first report (`true`) or CERTAINLY the same execution
    /// that already passed in that lane (`false`). See the module doc: everything else passes.
    fn execution(&mut self, lane: Lane, order_client_id: &str, fill: &FillEvent) -> bool {
        // Judged only for an order this ring saw ACCEPTED (an empty order client id is never in `seen`) and only for a fill the
        // engine would take: it refuses a number that is not finite BEFORE it burns the exec id, so a corrected
        // retransmission must still reach it.
        if !self.seen.contains_key(order_client_id) || !fill.numbers_finite() {
            return true;
        }
        let (qty, px) = (fill.last_qty.to_bits(), fill.last_price.to_bits());
        // `get_mut(&str)`: a repeat (the hot case) allocates nothing.
        if let Some(seen) = self.execs.get_mut(fill.trade_id.as_str()) {
            if seen.order_client_id != order_client_id
                || seen.symbol != fill.symbol.as_str()
                || seen.side != fill.side
                || seen.qty != qty
                || seen.price != px
            {
                // Not certainly the same execution: it passes, and the first report keeps its place.
                return true;
            }
            if seen.lanes & lane.bit() != 0 {
                return false;
            }
            seen.lanes |= lane.bit();
            return true;
        }
        if let Some(order) = self.seen.get_mut(order_client_id) {
            order.execs = order.execs.saturating_add(1);
        }
        self.execution_order.push_back(fill.trade_id.clone());
        while self.execution_order.len() > self.cap {
            if let Some(old) = self.execution_order.pop_front() {
                self.execs.remove(old.as_str());
            }
        }
        self.execs.insert(
            fill.trade_id.clone(),
            Execution {
                order_client_id: order_client_id.to_string(),
                symbol: fill.symbol.as_str().to_string(),
                side: fill.side,
                qty,
                price: px,
                lanes: lane.bit(),
            },
        );
        true
    }

    fn ended(&mut self, order_client_id: &str, word: Word, synthetic: bool) -> bool {
        let Some(seen) = self.seen.get_mut(order_client_id) else { return true };
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
        FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderExpired, OrderFilled,
        OrderPartiallyFilled, OrderRejected,
    };

    fn accepted(order_client_id: &str) -> Event {
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.into(),
            venue_order_id: Some("v1".into()),
            timestamp: 5,
        })
    }

    fn bare_accepted(order_client_id: &str) -> Event {
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.into(),
            venue_order_id: None,
            timestamp: 5,
        })
    }

    /// The loop's answer to our cancel: `cancel_event`'s shape.
    fn loop_canceled(order_client_id: &str) -> Event {
        Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.into(),
            reason: "".into(),
            timestamp: 0,
        })
    }

    /// A venue row (the stream's or the replay's): stamped with the venue's time.
    fn venue_canceled(order_client_id: &str) -> Event {
        Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.into(),
            reason: "user".into(),
            timestamp: 9,
        })
    }

    fn venue_expired(order_client_id: &str) -> Event {
        Event::OrderExpired(OrderExpired { order_client_id: order_client_id.into(), timestamp: 9 })
    }

    fn passes(memory: &mut OnceMemory, events: &[Event]) -> Vec<bool> {
        events.iter().map(|e| memory.admit(e)).collect()
    }

    #[test]
    fn the_accept_the_loop_states_and_the_stream_states_again_is_one_record() {
        let mut memory = OnceMemory::new(8);
        // The loop, the venue's NEW row, and a later replay of the open order.
        assert_eq!(
            passes(&mut memory, &[accepted("a"), accepted("a"), accepted("a")]),
            [true, false, false]
        );
    }

    #[test]
    fn a_cancel_the_loop_the_stream_and_every_replay_state_is_one_record() {
        let mut memory = OnceMemory::new(8);
        let got = passes(
            &mut memory,
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
        let mut memory = OnceMemory::new(8);
        assert_eq!(
            passes(&mut memory, &[accepted("a"), venue_canceled("a"), loop_canceled("a")]),
            [true, true, false]
        );
    }

    #[test]
    fn a_venue_row_of_another_word_after_the_loops_cancel_is_a_disagreement_and_passes_once() {
        let mut memory = OnceMemory::new(8);
        assert_eq!(
            passes(
                &mut memory,
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
        let mut memory = OnceMemory::new(8);
        assert_eq!(
            passes(&mut memory, &[accepted("a"), venue_expired("a"), venue_canceled("a")]),
            [true, true, false]
        );
    }

    #[test]
    fn a_lone_event_of_every_class_passes_untouched() {
        let mut memory = OnceMemory::new(8);
        assert!(memory.admit(&accepted("lone-accept")));
        // A terminal for an order this memory never saw accepted (a restart, a rebuilt adapter): not a repeat,
        // however often the venue says it.
        for _ in 0..3 {
            assert!(memory.admit(&venue_canceled("never-accepted")));
            assert!(memory.admit(&venue_expired("never-accepted")));
            assert!(memory.admit(&loop_canceled("never-accepted")));
        }
        assert_eq!(memory.len(), 1, "a terminal without an accept remembers nothing");
    }

    #[test]
    fn an_accept_that_names_no_venue_order_passes_and_is_never_remembered() {
        let mut memory = OnceMemory::new(8);
        // Bare accepts pass however often they come and leave nothing behind, so the venue's stamped terminal that
        // resolves an UNKNOWN order is never a repeat of the loop's synthetic one.
        assert_eq!(
            passes(
                &mut memory,
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
        assert!(memory.is_empty());
        // The first accept that names the venue's order is remembered; a bare one after it is its repeat.
        assert_eq!(
            passes(&mut memory, &[accepted("b"), bare_accepted("b"), accepted("b")]),
            [true, false, false]
        );
    }

    #[test]
    fn forget_makes_the_next_life_of_an_order_client_id_a_first_report_and_leaves_the_others() {
        let mut memory = OnceMemory::new(8);
        assert!(memory.admit(&accepted("a")) && memory.admit(&accepted("b")));
        assert!(memory.admit(&loop_canceled("a")) && !memory.admit(&venue_canceled("a")));
        memory.forget("a");
        // a's second life: its accept and its terminal are first reports; b is untouched.
        assert_eq!(
            passes(
                &mut memory,
                &[accepted("a"), loop_canceled("a"), venue_canceled("a"), accepted("b")]
            ),
            [true, true, false, false]
        );
        memory.forget("never-seen");
    }

    #[test]
    fn every_dropped_repeat_is_counted_by_class_and_clear_keeps_the_counts() {
        let mut memory = OnceMemory::new(8);
        assert_eq!(memory.repeats(), OnceMemoryRepeats::default());
        let _ = passes(
            &mut memory,
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
        assert_eq!(memory.repeats(), OnceMemoryRepeats { accepted: 2, terminal: 2, fill: 0 });
        memory.clear();
        assert_eq!(memory.repeats(), OnceMemoryRepeats { accepted: 2, terminal: 2, fill: 0 });
    }

    fn exec(order_client_id: &str, id: &'static str, qty: f64, px: f64) -> FillEvent {
        FillEvent {
            trade_id: id.into(),
            order_client_id: order_client_id.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: qty,
            last_price: px,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: Default::default(),
            timestamp: 1,
            mark_price: None,
            position_side: Default::default(),
        }
    }

    fn fill(order_client_id: &str, id: &'static str) -> Event {
        Event::Fill(exec(order_client_id, id, 0.001, 80_512.0))
    }

    fn partial(order_client_id: &str, id: &'static str) -> Event {
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            order_client_id: order_client_id.into(),
            fill: exec(order_client_id, id, 0.001, 80_512.0),
            timestamp: 1,
        })
    }

    fn filled(order_client_id: &str, id: &'static str) -> Event {
        Event::OrderFilled(OrderFilled {
            order_client_id: order_client_id.into(),
            fill: exec(order_client_id, id, 0.001, 80_512.0),
            timestamp: 1,
        })
    }

    /// A memory that has seen each of `order_client_ids` accepted (fills are judged only for such orders).
    fn with_orders(cap: usize, order_client_ids: &[&str]) -> OnceMemory {
        let mut memory = OnceMemory::new(cap);
        for order_client_id in order_client_ids {
            assert!(memory.admit(&accepted(order_client_id)));
        }
        memory
    }

    #[test]
    fn the_execution_the_stream_states_and_every_replay_states_again_is_one_record() {
        let mut memory = with_orders(8, &["a"]);
        assert_eq!(
            passes(&mut memory, &[fill("a", "t1"), fill("a", "t1"), fill("a", "t1")]),
            [true, false, false]
        );
        // A fee and a time restated by the replay are not part of the identity.
        let mut restated = exec("a", "t1", 0.001, 80_512.0);
        restated.commission = 0.5;
        restated.timestamp = 99;
        assert!(!memory.admit(&Event::Fill(restated)));
        // Counted apart from the accepts and the terminals.
        assert_eq!(memory.repeats(), OnceMemoryRepeats { accepted: 0, terminal: 0, fill: 3 });
    }

    #[test]
    fn a_fill_and_its_two_wraps_are_each_stated_once() {
        let mut memory = with_orders(8, &["a"]);
        // The bare report and the two wraps of ONE execution are three different facts, each told once.
        let one = [fill("a", "t1"), partial("a", "t1"), filled("a", "t1")];
        assert_eq!(passes(&mut memory, &one), [true, true, true]);
        assert_eq!(passes(&mut memory, &one), [false, false, false]);
        assert_eq!(memory.repeats().fill, 3);
        assert_eq!(
            memory.executions(),
            1,
            "one execution is remembered once, whatever lanes carried it"
        );
    }

    #[test]
    fn only_the_same_execution_id_order_and_numbers_are_a_repeat() {
        let mut memory = with_orders(8, &["a", "b"]);
        assert!(memory.admit(&fill("a", "t1")));
        // Another execution of the same order and the same numbers.
        assert!(memory.admit(&fill("a", "t2")));
        // The same exec id with another quantity, price, order, instrument or side is not certainly the same
        // execution ...
        assert!(memory.admit(&Event::Fill(exec("a", "t1", 0.002, 80_512.0))));
        assert!(memory.admit(&Event::Fill(exec("a", "t1", 0.001, 80_513.0))));
        assert!(memory.admit(&fill("b", "t1")));
        assert!(memory.admit(&Event::Fill(FillEvent {
            symbol: "ETHUSDT".into(),
            ..exec("a", "t1", 0.001, 80_512.0)
        })));
        assert!(
            memory.admit(&Event::Fill(FillEvent { side: -1, ..exec("a", "t1", 0.001, 80_512.0) }))
        );
        // ... and none of those took the first report's place.
        assert!(!memory.admit(&fill("a", "t1")));
        assert!(!memory.admit(&fill("a", "t2")));
        assert_eq!(memory.repeats().fill, 2);
    }

    #[test]
    fn a_fill_with_no_order_or_a_number_that_is_not_finite_is_never_judged() {
        let mut memory = with_orders(8, &["a"]);
        for _ in 0..3 {
            assert!(
                memory.admit(&fill("", "t1")),
                "a foreign execution is not this door's to judge"
            );
            assert!(memory.admit(&filled("", "t1")));
            assert!(memory.admit(&Event::Fill(exec("a", "t9", f64::NAN, 80_512.0))));
            assert!(memory.admit(&Event::Fill(exec("a", "t9", 0.001, f64::INFINITY))));
            // The engine refuses a non-finite commission BEFORE it burns the exec id: the corrected
            // retransmission must still reach it, so the first copy is not remembered.
            assert!(memory.admit(&Event::Fill(FillEvent {
                commission: f64::NAN,
                ..exec("a", "t8", 0.001, 80_512.0)
            })));
        }
        assert_eq!(memory.executions(), 0, "and nothing of them is remembered");
        assert_eq!(memory.repeats(), OnceMemoryRepeats::default());
    }

    #[test]
    fn a_fill_of_an_order_the_ring_never_saw_accepted_is_never_judged() {
        // A restart, a rebuilt adapter, a venue that fills with no accept, an accept that names no venue order.
        let mut memory = OnceMemory::new(8);
        assert!(memory.admit(&bare_accepted("u")));
        for order_client_id in ["ghost", "u"] {
            assert_eq!(
                passes(
                    &mut memory,
                    &[
                        fill(order_client_id, "t1"),
                        fill(order_client_id, "t1"),
                        filled(order_client_id, "t1"),
                        filled(order_client_id, "t1")
                    ]
                ),
                [true, true, true, true]
            );
        }
        assert_eq!(memory.executions(), 0);
        assert_eq!(memory.repeats(), OnceMemoryRepeats::default());
    }

    #[test]
    fn a_wrap_that_comes_before_the_accept_is_not_remembered_so_the_redelivery_after_it_passes() {
        // The engine refuses a wrap that finds its order still Submitted WITHOUT burning the exec id; only a
        // redelivery after the accept completes the order. That redelivery must reach it.
        let mut memory = OnceMemory::new(8);
        assert!(memory.admit(&filled("a", "t1")), "the fill overtook its accept");
        assert!(memory.admit(&accepted("a")));
        assert!(
            memory.admit(&filled("a", "t1")),
            "the sentinel's redelivery: the engine takes it now"
        );
        assert!(!memory.admit(&filled("a", "t1")), "and the next one is a repeat");
    }

    #[test]
    fn executions_are_a_bounded_ring_and_clear_forgets_them() {
        let mut memory = with_orders(2, &["a"]);
        assert!(
            memory.admit(&fill("a", "t1"))
                && memory.admit(&fill("a", "t2"))
                && memory.admit(&fill("a", "t3"))
        );
        assert_eq!(memory.executions(), 2, "bounded");
        assert!(
            !memory.admit(&fill("a", "t2")) && !memory.admit(&fill("a", "t3")),
            "still remembered"
        );
        assert!(memory.admit(&fill("a", "t1")), "forgotten: a first report again");
        memory.clear();
        assert_eq!(memory.executions(), 0);
        assert!(memory.admit(&fill("a", "t3")), "cleared: a first report again");
        assert_eq!(memory.repeats().fill, 2, "the counts are kept");
    }

    #[test]
    fn an_order_that_ages_out_takes_its_executions_with_it() {
        let mut memory = with_orders(2, &["a", "b"]);
        assert!(memory.admit(&fill("a", "t1")) && memory.admit(&fill("b", "t2")));
        assert_eq!(memory.executions(), 2);
        // `c` pushes `a` out of the two-order ring; `a`'s execution leaves with it, `b`'s stays.
        assert!(memory.admit(&accepted("c")));
        assert_eq!(memory.executions(), 1);
        assert!(!memory.admit(&fill("b", "t2")));
    }

    #[test]
    fn a_new_life_of_an_order_client_id_forgets_its_executions_and_leaves_the_others() {
        let mut memory = with_orders(8, &["a", "b"]);
        assert!(memory.admit(&fill("a", "t1")) && memory.admit(&fill("b", "t2")));
        assert!(!memory.admit(&fill("a", "t1")));
        memory.forget("a");
        // The new life's marker fill (a venue that mints the exec id from the order client id alone) is a first report
        // once its accept has passed ...
        assert!(memory.admit(&accepted("a")));
        assert_eq!(passes(&mut memory, &[fill("a", "t1"), fill("a", "t1")]), [true, false]);
        // ... and the other order's execution is untouched.
        assert!(!memory.admit(&fill("b", "t2")));
        memory.forget("never-seen");
    }

    #[test]
    fn rejects_modifies_and_advisories_are_never_judged() {
        let mut memory = OnceMemory::new(8);
        assert!(memory.admit(&accepted("a")));
        let reject = Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: "a".into(),
            reason: "r".into(),
            timestamp: 1,
        });
        let advisory = Event::OrderCancelRejected(OrderCancelRejected {
            order_client_id: "a".into(),
            reason: "r".into(),
            timestamp: 0,
        });
        for _ in 0..3 {
            assert!(memory.admit(&reject) && memory.admit(&advisory));
        }
    }

    #[test]
    fn an_event_without_an_order_client_id_is_never_judged() {
        let mut memory = OnceMemory::new(8);
        assert!(memory.admit(&accepted("")) && memory.admit(&accepted("")));
        assert!(memory.is_empty());
    }

    #[test]
    fn the_memory_is_a_ring_the_oldest_order_is_forgotten_and_its_repeat_passes_again() {
        let mut memory = OnceMemory::new(2);
        assert!(
            memory.admit(&accepted("a"))
                && memory.admit(&accepted("b"))
                && memory.admit(&accepted("c"))
        );
        assert_eq!(memory.len(), 2, "bounded");
        assert!(!memory.admit(&accepted("b")) && !memory.admit(&accepted("c")), "still remembered");
        assert!(memory.admit(&accepted("a")), "forgotten: a first report again");
        assert_eq!(memory.len(), 2);
    }

    #[test]
    fn clear_makes_every_next_report_a_first_one() {
        let mut memory = OnceMemory::new(8);
        assert!(memory.admit(&accepted("a")) && memory.admit(&venue_canceled("a")));
        assert!(!memory.admit(&venue_canceled("a")));
        memory.clear();
        assert!(memory.is_empty());
        assert!(memory.admit(&venue_canceled("a")), "unwitnessed again: passes untouched");
        assert!(memory.admit(&accepted("a")));
    }
}
