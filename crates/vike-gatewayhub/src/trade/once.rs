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
//! * **Keyed by client order id inside ONE account** (the session is one `(venue, account)`; two accounts
//!   never share a memory), **first report wins, per class.** The order classes are `OrderAccepted` and the
//!   terminal words `OrderCanceled` / `OrderExpired`. Rejects, modifies and every non-order event are never
//!   looked at; the fill classes are the next bullet.
//! * **Only an order this session saw ACCEPTED can repeat.** A terminal or a fill for a coid the memory never
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
//!   A new life of a coid forgets its executions too (below).
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
//! * **Bounded**: a FIFO ring of coids, `cap` of them (and, apart, one of executions). An order older than that
//!   is forgotten and its replay would pass again, which is what it did before this module.
//! * **A Submit the dispatch thread admits starts a NEW LIFE of its coid** ([`OnceMemory::forget`]). The Submit
//!   dedup set (`crate::trade::dedup`) is per ADAPTER, so a changed start or a panic rebuild empties it and the
//!   same coid is admitted again while this memory still holds the first life as ended; its accept and
//!   terminal would be dropped as repeats and the order would stick at Initialized. The executions told under
//!   the coid are forgotten with it (a marker fill a venue mints from the coid alone, ctrader's and ig's, would
//!   otherwise read as the first life's).
//! * **Counted**: every dropped repeat is counted per class ([`Repeats`]: accepts, terminals, fills) and the
//!   session reports the totals when it is retired.
//! * **The memory spans one unbroken client view of the ring.** It is cleared with the ring on a rebuild, when a
//!   client opens WITHOUT a replayable cursor and the moment the ring evicts events an attached client has not read (`clear`):
//!   that client reconciles and may leave an order UNKNOWN, and the venue's own row is what resolves it (the
//!   client treats the synthetic cancel as an advisory, `vike-gatewayhub-client`'s `synthetic_cancel_coid`); a
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
    coid: String,
    /// The instrument and the side the fill states (the engine's own fill print holds both, so a re-stamped
    /// symbol or a flipped side is not certainly the same execution).
    symbol: String,
    side: i32,
    /// The reported quantity and price as bits: an identity test, never arithmetic.
    qty: u64,
    px: u64,
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
pub struct Repeats {
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
    /// The executions that passed, by exec id (`order`'s twin for fills; `exec_order` is its FIFO, with stale slots
    /// after a [`OnceMemory::forget`]).
    execs: HashMap<TradeId, Execution>,
    exec_order: VecDeque<TradeId>,
    repeats: Repeats,
}

impl OnceMemory {
    /// An empty memory of at most `cap` orders and `cap` executions (at least one each).
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            order: VecDeque::new(),
            seen: HashMap::new(),
            execs: HashMap::new(),
            exec_order: VecDeque::new(),
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
            Event::Fill(f) => {
                (self.execution(Lane::Bare, &f.client_order_id, f), &mut self.repeats.fill)
            }
            Event::OrderPartiallyFilled(w) => {
                (self.execution(Lane::Partial, &w.client_order_id, &w.fill), &mut self.repeats.fill)
            }
            Event::OrderFilled(w) => {
                (self.execution(Lane::Filled, &w.client_order_id, &w.fill), &mut self.repeats.fill)
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
    /// one, its executions included, is forgotten, so the new order's accept, terminal and fills are first reports.
    pub fn forget(&mut self, coid: &str) {
        // The coid stays in `order` (and its executions in `exec_order`) until it ages out; a slot there only
        // evicts a newer entry of the same key early, which forgets more, never less.
        if let Some(gone) = self.seen.remove(coid) {
            self.forget_executions(coid, gone);
        }
    }

    /// The executions told under `coid` leave with the order that held them. A scan of at most `cap` executions, owed
    /// only to an order that had some: every execution is remembered under an order the memory holds accepted, and an
    /// order leaves `seen` only through here, [`Self::accepted`]'s ageing or [`Self::clear`].
    fn forget_executions(&mut self, coid: &str, gone: Seen) {
        if gone.execs > 0 {
            self.execs.retain(|_, e| e.coid != coid);
        }
    }

    /// Forget everything: the next report of every order and every execution is a first one. The counts are kept.
    pub fn clear(&mut self) {
        self.order.clear();
        self.seen.clear();
        self.exec_order.clear();
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
                if let Some(old) = self.order.pop_front()
                    && let Some(gone) = self.seen.remove(&old)
                {
                    self.forget_executions(&old, gone);
                }
            }
            self.seen.insert(coid.to_string(), Seen { ended: Ended::No, words: 0, execs: 0 });
        }
        true
    }

    /// Whether a fill reported in `lane` for order `coid` is a first report (`true`) or CERTAINLY the same execution
    /// that already passed in that lane (`false`). See the module doc: everything else passes.
    fn execution(&mut self, lane: Lane, coid: &str, fill: &FillEvent) -> bool {
        // Judged only for an order this ring saw ACCEPTED (an empty coid is never in `seen`) and only for a fill the
        // engine would take: it refuses a number that is not finite BEFORE it burns the exec id, so a corrected
        // retransmission must still reach it.
        if !self.seen.contains_key(coid) || !fill.numbers_finite() {
            return true;
        }
        let (qty, px) = (fill.last_qty.to_bits(), fill.last_px.to_bits());
        // `get_mut(&str)`: a repeat (the hot case) allocates nothing.
        if let Some(seen) = self.execs.get_mut(fill.trade_id.as_str()) {
            if seen.coid != coid
                || seen.symbol != fill.symbol.as_str()
                || seen.side != fill.side
                || seen.qty != qty
                || seen.px != px
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
        if let Some(order) = self.seen.get_mut(coid) {
            order.execs = order.execs.saturating_add(1);
        }
        self.exec_order.push_back(fill.trade_id.clone());
        while self.exec_order.len() > self.cap {
            if let Some(old) = self.exec_order.pop_front() {
                self.execs.remove(old.as_str());
            }
        }
        self.execs.insert(
            fill.trade_id.clone(),
            Execution {
                coid: coid.to_string(),
                symbol: fill.symbol.as_str().to_string(),
                side: fill.side,
                qty,
                px,
                lanes: lane.bit(),
            },
        );
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
        FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderExpired, OrderFilled,
        OrderPartiallyFilled, OrderRejected,
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
        assert_eq!(mem.repeats(), Repeats { accepted: 2, terminal: 2, fill: 0 });
        mem.clear();
        assert_eq!(mem.repeats(), Repeats { accepted: 2, terminal: 2, fill: 0 });
    }

    fn exec(coid: &str, id: &'static str, qty: f64, px: f64) -> FillEvent {
        FillEvent {
            trade_id: id.into(),
            client_order_id: coid.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: qty,
            last_px: px,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: Default::default(),
            ts: 1,
            mark_price: None,
            position_side: Default::default(),
        }
    }

    fn fill(coid: &str, id: &'static str) -> Event {
        Event::Fill(exec(coid, id, 0.001, 80_512.0))
    }

    fn partial(coid: &str, id: &'static str) -> Event {
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: coid.into(),
            fill: exec(coid, id, 0.001, 80_512.0),
            ts: 1,
        })
    }

    fn filled(coid: &str, id: &'static str) -> Event {
        Event::OrderFilled(OrderFilled {
            client_order_id: coid.into(),
            fill: exec(coid, id, 0.001, 80_512.0),
            ts: 1,
        })
    }

    /// A memory that has seen each of `coids` accepted (fills are judged only for such orders).
    fn with_orders(cap: usize, coids: &[&str]) -> OnceMemory {
        let mut mem = OnceMemory::new(cap);
        for coid in coids {
            assert!(mem.admit(&accepted(coid)));
        }
        mem
    }

    #[test]
    fn the_execution_the_stream_states_and_every_replay_states_again_is_one_record() {
        let mut mem = with_orders(8, &["a"]);
        assert_eq!(
            passes(&mut mem, &[fill("a", "t1"), fill("a", "t1"), fill("a", "t1")]),
            [true, false, false]
        );
        // A fee and a time restated by the replay are not part of the identity.
        let mut restated = exec("a", "t1", 0.001, 80_512.0);
        restated.commission = 0.5;
        restated.ts = 99;
        assert!(!mem.admit(&Event::Fill(restated)));
        // Counted apart from the accepts and the terminals.
        assert_eq!(mem.repeats(), Repeats { accepted: 0, terminal: 0, fill: 3 });
    }

    #[test]
    fn a_fill_and_its_two_wraps_are_each_stated_once() {
        let mut mem = with_orders(8, &["a"]);
        // The bare report and the two wraps of ONE execution are three different facts, each told once.
        let one = [fill("a", "t1"), partial("a", "t1"), filled("a", "t1")];
        assert_eq!(passes(&mut mem, &one), [true, true, true]);
        assert_eq!(passes(&mut mem, &one), [false, false, false]);
        assert_eq!(mem.repeats().fill, 3);
        assert_eq!(
            mem.executions(),
            1,
            "one execution is remembered once, whatever lanes carried it"
        );
    }

    #[test]
    fn only_the_same_exec_id_order_and_numbers_are_a_repeat() {
        let mut mem = with_orders(8, &["a", "b"]);
        assert!(mem.admit(&fill("a", "t1")));
        // Another execution of the same order and the same numbers.
        assert!(mem.admit(&fill("a", "t2")));
        // The same exec id with another quantity, price, order, instrument or side is not certainly the same
        // execution ...
        assert!(mem.admit(&Event::Fill(exec("a", "t1", 0.002, 80_512.0))));
        assert!(mem.admit(&Event::Fill(exec("a", "t1", 0.001, 80_513.0))));
        assert!(mem.admit(&fill("b", "t1")));
        assert!(mem.admit(&Event::Fill(FillEvent {
            symbol: "ETHUSDT".into(),
            ..exec("a", "t1", 0.001, 80_512.0)
        })));
        assert!(
            mem.admit(&Event::Fill(FillEvent { side: -1, ..exec("a", "t1", 0.001, 80_512.0) }))
        );
        // ... and none of those took the first report's place.
        assert!(!mem.admit(&fill("a", "t1")));
        assert!(!mem.admit(&fill("a", "t2")));
        assert_eq!(mem.repeats().fill, 2);
    }

    #[test]
    fn a_fill_with_no_order_or_a_number_that_is_not_finite_is_never_judged() {
        let mut mem = with_orders(8, &["a"]);
        for _ in 0..3 {
            assert!(mem.admit(&fill("", "t1")), "a foreign execution is not this door's to judge");
            assert!(mem.admit(&filled("", "t1")));
            assert!(mem.admit(&Event::Fill(exec("a", "t9", f64::NAN, 80_512.0))));
            assert!(mem.admit(&Event::Fill(exec("a", "t9", 0.001, f64::INFINITY))));
            // The engine refuses a non-finite commission BEFORE it burns the exec id: the corrected
            // retransmission must still reach it, so the first copy is not remembered.
            assert!(mem.admit(&Event::Fill(FillEvent {
                commission: f64::NAN,
                ..exec("a", "t8", 0.001, 80_512.0)
            })));
        }
        assert_eq!(mem.executions(), 0, "and nothing of them is remembered");
        assert_eq!(mem.repeats(), Repeats::default());
    }

    #[test]
    fn a_fill_of_an_order_the_ring_never_saw_accepted_is_never_judged() {
        // A restart, a rebuilt adapter, a venue that fills with no accept, an accept that names no venue order.
        let mut mem = OnceMemory::new(8);
        assert!(mem.admit(&bare_accepted("u")));
        for coid in ["ghost", "u"] {
            assert_eq!(
                passes(
                    &mut mem,
                    &[fill(coid, "t1"), fill(coid, "t1"), filled(coid, "t1"), filled(coid, "t1")]
                ),
                [true, true, true, true]
            );
        }
        assert_eq!(mem.executions(), 0);
        assert_eq!(mem.repeats(), Repeats::default());
    }

    #[test]
    fn a_wrap_that_comes_before_the_accept_is_not_remembered_so_the_redelivery_after_it_passes() {
        // The engine refuses a wrap that finds its order still Submitted WITHOUT burning the exec id; only a
        // redelivery after the accept completes the order. That redelivery must reach it.
        let mut mem = OnceMemory::new(8);
        assert!(mem.admit(&filled("a", "t1")), "the fill overtook its accept");
        assert!(mem.admit(&accepted("a")));
        assert!(
            mem.admit(&filled("a", "t1")),
            "the sentinel's redelivery: the engine takes it now"
        );
        assert!(!mem.admit(&filled("a", "t1")), "and the next one is a repeat");
    }

    #[test]
    fn executions_are_a_bounded_ring_and_clear_forgets_them() {
        let mut mem = with_orders(2, &["a"]);
        assert!(
            mem.admit(&fill("a", "t1"))
                && mem.admit(&fill("a", "t2"))
                && mem.admit(&fill("a", "t3"))
        );
        assert_eq!(mem.executions(), 2, "bounded");
        assert!(!mem.admit(&fill("a", "t2")) && !mem.admit(&fill("a", "t3")), "still remembered");
        assert!(mem.admit(&fill("a", "t1")), "forgotten: a first report again");
        mem.clear();
        assert_eq!(mem.executions(), 0);
        assert!(mem.admit(&fill("a", "t3")), "cleared: a first report again");
        assert_eq!(mem.repeats().fill, 2, "the counts are kept");
    }

    #[test]
    fn an_order_that_ages_out_takes_its_executions_with_it() {
        let mut mem = with_orders(2, &["a", "b"]);
        assert!(mem.admit(&fill("a", "t1")) && mem.admit(&fill("b", "t2")));
        assert_eq!(mem.executions(), 2);
        // `c` pushes `a` out of the two-order ring; `a`'s execution leaves with it, `b`'s stays.
        assert!(mem.admit(&accepted("c")));
        assert_eq!(mem.executions(), 1);
        assert!(!mem.admit(&fill("b", "t2")));
    }

    #[test]
    fn a_new_life_of_a_coid_forgets_its_executions_and_leaves_the_others() {
        let mut mem = with_orders(8, &["a", "b"]);
        assert!(mem.admit(&fill("a", "t1")) && mem.admit(&fill("b", "t2")));
        assert!(!mem.admit(&fill("a", "t1")));
        mem.forget("a");
        // The new life's marker fill (a venue that mints the exec id from the coid alone) is a first report
        // once its accept has passed ...
        assert!(mem.admit(&accepted("a")));
        assert_eq!(passes(&mut mem, &[fill("a", "t1"), fill("a", "t1")]), [true, false]);
        // ... and the other order's execution is untouched.
        assert!(!mem.admit(&fill("b", "t2")));
        mem.forget("never-seen");
    }

    #[test]
    fn rejects_modifies_and_advisories_are_never_judged() {
        let mut mem = OnceMemory::new(8);
        assert!(mem.admit(&accepted("a")));
        let reject = Event::OrderRejected(OrderRejected {
            cause: None,
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
