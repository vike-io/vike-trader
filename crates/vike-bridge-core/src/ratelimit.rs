//! [`RateGate`]: a blocking "N occurrences per sliding window" limiter, std-only (`Mutex<State>` +
//! `Condvar`, no `governor`, no async). Callers are venue-adapter threads (`ExecActor`, feeds,
//! backfill), NEVER the vike-core fold, so blocking is by design. `Clone` is a shared handle: every
//! clone rides the same window. Each `proceed` records an `Instant` that ages out individually one
//! `window` later; throttling is logged through `proceed_logged`, never silent latency.
//!
//! **Weighted costs** (`proceed_cost`/`try_proceed_cost`) serve venues that meter some requests
//! heavier (a Binance depth snapshot or `allOrders` scan costs many times an order, so a
//! post-reconnect resync burst can blow a weight budget a count-only limiter thinks is fine). A
//! cost-`n` call is ALL-OR-NOTHING: it reserves `n` permits atomically or waits until `n` are free
//! at once, never holding a partial `k < n`. `proceed`/`try_proceed` are exactly cost 1.
//!
//! **Fairness is FIFO, and it is load-bearing.** A caller that must block takes a ticket and is
//! served in arrival order; a later arrival, including a non-blocking `try_proceed*` probe, may not
//! take a slot while somebody is queued, *even when its own cost would fit*. Without that rule a
//! cost-`n` call starves INDEFINITELY: a stream of cost-1 callers comfortably inside quota keeps a
//! slot live forever, so `n` are never free at once, and on a REST gate shared by a transport and a
//! backfill the starved call can be an order. Held by `a_heavy_caller_is_not_starved_*` and
//! `a_probe_yields_to_a_caller_already_blocked`. The cost: on a SATURATED gate light callers wait
//! behind a queued heavy one (a bounded delay for all beats an unbounded one for the heaviest);
//! with an empty queue every path is unchanged.
//!
//! ⚠ **Sharing a gate is something a composition root DOES, never something a `Clone` bound
//! implies.** A transport constructor that calls its own `*_gate()` factory mints a FRESH full
//! budget per transport, and on a per-IP meter that is a multiple of the venue's cap with every
//! window reporting itself inside quota. `crates/bridges/hyperliquid/src/ratelimit.rs` shows who
//! resolves the shared handle and which consumers take a clone.
//!
//! [`KeyedRateGate`] bundles several gates under string keys (`"subscribe"`, `"login"`, `"order"`)
//! with an optional shared default; the WS send seam (`ws::send_gated`) consults it before
//! each write.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

struct Inner {
    state: Mutex<State>,
    /// woken when a waiter should re-evaluate: by the head's own timer, and by a departing head
    /// handing the queue on.
    cv: Condvar,
    occurrences: usize,
    window: Duration,
}

/// Everything the gate mutates, under ONE lock: the live window plus the FIFO ticket pair.
struct State {
    /// timestamps of the live occurrences, oldest at the front
    slots: VecDeque<Instant>,
    /// FIFO ticket source: the number the NEXT caller that has to block will take.
    next_ticket: u64,
    /// The ticket entitled to reserve right now. `serving == next_ticket` means nobody is blocked,
    /// the ONLY state in which a fresh arrival may take a slot.
    serving: u64,
    /// TEST-ONLY: the ticket of every QUEUED caller, appended **under the lock that makes its
    /// reservation**. Service order is otherwise unobservable: [`RateGate::proceed_cost`] returns
    /// holding nothing, so a test recording after the return measures scheduler PUSH order and goes
    /// red on a fair gate (`blocked_callers_are_served_in_arrival_order` did). The ticket is the
    /// caller's identity once arrival order is established (see that test's setup note).
    #[cfg(test)]
    served_tickets: Vec<u64>,
}

/// "N occurrences per `window`" limiter. `Clone` = shared handle.
#[derive(Clone)]
pub struct RateGate(Arc<Inner>);

impl RateGate {
    /// `occurrences` slots per sliding `window`. **Panics** if `occurrences` is `0`.
    ///
    /// ⚠ A plain `assert!`, not `debug_assert!`: a zero-occurrence gate fails **OPEN** (the fit
    /// test in [`reserve_cost_if_free`](Self::reserve_cost_if_free) asks `0 + 0 <= 0` forever, so
    /// every call passes), and debug assertions are off in the release binaries that reach a venue.
    /// The guard is for a budget parsed at run time from a venue's response
    /// ([`crate::rate_discovery`]); the const `Meter` callers are covered at compile time.
    pub fn new(occurrences: usize, window: Duration) -> Self {
        assert!(occurrences >= 1, "RateGate needs at least one occurrence per window");
        RateGate(Arc::new(Inner {
            state: Mutex::new(State {
                slots: VecDeque::with_capacity(occurrences),
                next_ticket: 0,
                serving: 0,
                #[cfg(test)]
                served_tickets: Vec::new(),
            }),
            cv: Condvar::new(),
            occurrences,
            window,
        }))
    }

    /// Non-blocking: reserve a slot if one is free AND nobody is queued ahead. `false` = would
    /// throttle (no slot taken). The cost-1 twin of [`try_proceed_cost`](Self::try_proceed_cost).
    pub fn try_proceed(&self) -> bool {
        self.try_proceed_cost(1)
    }

    /// Non-blocking weighted reserve: take `cost` slots ALL-OR-NOTHING. `true` = reserved;
    /// `false` = NOTHING taken. `cost == 0` counts as `1`; a `cost` above capacity is clamped to
    /// capacity (it takes the whole window rather than being forever unsatisfiable).
    ///
    /// ⚠ **`false` has TWO causes**: fewer than `cost` slots were free, or **somebody is already
    /// BLOCKED in [`proceed_cost`](Self::proceed_cost)**, even when `cost` would fit. A probe that
    /// could take a slot past a queued heavy caller is a second door onto the starvation the queue
    /// prevents, and it is the door production uses ([`proceed_logged`](Self::proceed_logged) tries
    /// before it blocks). To get the slot, block through `proceed_cost`.
    pub fn try_proceed_cost(&self, cost: usize) -> bool {
        let mut st = self.0.state.lock().unwrap();
        if st.serving != st.next_ticket {
            return false; // somebody is blocked ahead of us; taking a slot now is what starves them
        }
        let now = Instant::now();
        self.reserve_cost_if_free(&mut st.slots, now, cost)
    }

    /// How many callers are BLOCKED in [`proceed_cost`](Self::proceed_cost); `0` when uncontended.
    /// The throttle warnings stamp it ([`proceed_logged`](Self::proceed_logged)).
    pub fn blocked(&self) -> usize {
        let st = self.0.state.lock().unwrap();
        (st.next_ticket - st.serving) as usize
    }

    /// TEST-ONLY: the tickets of the callers that had to QUEUE, in service order
    /// ([`State::served_tickets`]). The fast path and `try_proceed*` take no ticket: FIFO is a claim
    /// about the queue only.
    #[cfg(test)]
    fn served_tickets(&self) -> Vec<u64> {
        self.0.state.lock().unwrap().served_tickets.clone()
    }

    /// Block until a slot frees, then reserve it. The cost-1 twin of [`proceed_cost`](Self::proceed_cost).
    pub fn proceed(&self) {
        self.proceed_cost(1);
    }

    /// Block until `cost` slots are simultaneously free, then reserve them ALL-OR-NOTHING (never a
    /// held partial `k < n`). `cost` handling as [`try_proceed_cost`](Self::try_proceed_cost).
    ///
    /// **FIFO** (module doc): a blocked caller is served in arrival order, so its wait is bounded
    /// by the callers ahead of it, not by how long the gate stays busy.
    pub fn proceed_cost(&self, cost: usize) {
        let mut st = self.0.state.lock().unwrap();
        // Uncontended fast path: nobody is queued and the cost fits, so taking it jumps nobody.
        if st.serving == st.next_ticket
            && self.reserve_cost_if_free(&mut st.slots, Instant::now(), cost)
        {
            return;
        }
        // Join the queue: the head at once if it was empty, else behind whoever is there.
        let ticket = st.next_ticket;
        st.next_ticket += 1;
        loop {
            let now = Instant::now();
            if st.serving != ticket {
                // Behind someone: only the head can progress, and it wakes us when it leaves.
                st = self.0.cv.wait(st).unwrap();
                continue;
            }
            if self.reserve_cost_if_free(&mut st.slots, now, cost) {
                // Service order, recorded under this lock ([`State::served_tickets`]).
                #[cfg(test)]
                st.served_tickets.push(ticket);
                st.serving += 1;
                // Hand the queue on. `notify_all`: `notify_one` wakes an arbitrary waiter, not
                // necessarily the new head.
                self.0.cv.notify_all();
                return;
            }
            // Head, but no room: slots free by time, not by a notify, so sleep until ours ages out.
            let wait = self.time_until_room(&st.slots, now, cost);
            st = self.0.cv.wait_timeout(st, wait).unwrap().0;
        }
    }

    /// How long until `cost` permits are simultaneously free, given the live `slots`.
    ///
    /// Precondition: [`reserve_cost_if_free`] just returned `false` (expired entries purged), so
    /// `need` of the oldest occurrences must age out, the k-th oldest one `window` after its stamp.
    ///
    /// [`reserve_cost_if_free`]: Self::reserve_cost_if_free
    fn time_until_room(&self, slots: &VecDeque<Instant>, now: Instant, cost: usize) -> Duration {
        let want = cost.max(1).min(self.0.occurrences);
        let need = (slots.len() + want).saturating_sub(self.0.occurrences);
        debug_assert!(
            (1..=slots.len()).contains(&need),
            "a blocking cost has 1 <= need <= len (reserve only fails when the window is full for it)"
        );
        let target = slots[need - 1];
        (target + self.0.window).saturating_duration_since(now)
    }

    /// Try first; on throttle, warn ONCE then block.
    ///
    /// The warning stamps `queued_ahead` ([`blocked`](Self::blocked)): `0` means the venue's budget
    /// is spent, non-zero means this call waits on OTHER callers of the gate (a backfill, a resync
    /// burst), not on the venue.
    pub fn proceed_logged(&self, venue: &str, what: &str) {
        if !self.try_proceed() {
            tracing::warn!(venue, what, queued_ahead = self.blocked(), "rate limited — throttling");
            self.proceed();
        }
    }

    /// Weighted twin of [`proceed_logged`](Self::proceed_logged): try `cost` slots; on throttle,
    /// warn ONCE (stamping the `cost` and `queued_ahead`) then block until `cost` are free.
    pub fn proceed_cost_logged(&self, venue: &str, what: &str, cost: usize) {
        if !self.try_proceed_cost(cost) {
            tracing::warn!(
                venue,
                what,
                cost,
                queued_ahead = self.blocked(),
                "rate limited — throttling"
            );
            self.proceed_cost(cost);
        }
    }

    /// Purge expired occurrences, then reserve `cost` slots iff the WHOLE `cost` fits. Caller holds
    /// the lock. Each permit is a `now` timestamp aging out one `window` later. `cost == 0` counts
    /// as 1; a `cost` above capacity is clamped to capacity.
    fn reserve_cost_if_free(
        &self,
        slots: &mut VecDeque<Instant>,
        now: Instant,
        cost: usize,
    ) -> bool {
        while let Some(&front) = slots.front() {
            if now.duration_since(front) >= self.0.window {
                slots.pop_front();
            } else {
                break;
            }
        }
        let want = cost.max(1).min(self.0.occurrences);
        if slots.len() + want <= self.0.occurrences {
            for _ in 0..want {
                slots.push_back(now);
            }
            true
        } else {
            false
        }
    }
}

struct KeyedInner {
    venue: &'static str,
    /// per-key gates; small (2–4 keys per venue) so a linear scan beats a map.
    gates: Vec<(&'static str, RateGate)>,
    /// one shared fallback gate for any key not in `gates`; `None` = unlisted keys are ungated.
    default: Option<RateGate>,
}

/// A per-key bundle of [`RateGate`]s (e.g. `"subscribe"`/`"login"`/`"order"`), each with its own
/// window, plus an optional shared default: the blocking, keyed WS-send limiter. `Clone` = shared
/// handle.
#[derive(Clone)]
pub struct KeyedRateGate(Arc<KeyedInner>);

impl KeyedRateGate {
    /// `venue` stamps the throttle logs. `gates` are the per-key limiters; `default` (if any) is the
    /// single shared fallback for keys not listed.
    pub fn new(
        venue: &'static str,
        gates: Vec<(&'static str, RateGate)>,
        default: Option<RateGate>,
    ) -> Self {
        KeyedRateGate(Arc::new(KeyedInner { venue, gates, default }))
    }

    fn lookup(&self, key: &str) -> Option<&RateGate> {
        self.0.gates.iter().find(|(k, _)| *k == key).map(|(_, g)| g).or(self.0.default.as_ref())
    }

    /// Gate an outbound send tagged `key`: BLOCK until the key's window (or the default) has room,
    /// warning once on throttle. No configured gate for `key` and no default → no-op (ungated).
    pub fn gate(&self, key: &str) {
        if let Some(g) = self.lookup(key) {
            g.proceed_logged(self.0.venue, key);
        }
    }

    /// Weighted twin of [`gate`](Self::gate): charge `cost` permits for one heavier `key` send,
    /// BLOCKING (with a single warn) until they are free. Ungated key → no-op.
    pub fn gate_cost(&self, key: &str, cost: usize) {
        if let Some(g) = self.lookup(key) {
            g.proceed_cost_logged(self.0.venue, key, cost);
        }
    }

    /// Non-blocking twin of [`gate`](Self::gate), with [`RateGate::try_proceed`]'s two causes of
    /// `false` (no room, or a caller already queued on the key's gate). `true` for an ungated key.
    pub fn try_gate(&self, key: &str) -> bool {
        self.lookup(key).is_none_or(|g| g.try_proceed())
    }

    /// Non-blocking weighted twin of [`try_gate`](Self::try_gate): reserve `cost` permits for `key`
    /// all-or-nothing. `true` = reserved (also `true` for an ungated key); `false` = would throttle
    /// (nothing taken).
    pub fn try_gate_cost(&self, key: &str, cost: usize) -> bool {
        self.lookup(key).is_none_or(|g| g.try_proceed_cost(cost))
    }

    pub fn venue(&self) -> &'static str {
        self.0.venue
    }
}

#[path = "ratelimit_tests.rs"]
#[cfg(test)]
mod ratelimit_tests;
