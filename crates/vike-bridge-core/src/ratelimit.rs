//! `RateGate` — a blocking "N occurrences per sliding window" limiter (net-hardening spec §A).
//!
//! LEAN-shaped (`RateGate`), std-only: `Mutex<State> + Condvar`, no `governor`, no
//! async. Callers are venue-adapter threads (`ExecActor`, feeds, backfill) — NEVER the vike-core
//! fold — so blocking is by design. `Clone` is a shared handle (`Arc` inside): every clone rides
//! the same window, which is how a venue gives its REST transport and its WS-subscribe path each a
//! gate while a backfill loop shares the REST one.
//!
//! Sliding window (not fixed buckets): each `proceed` records an `Instant`; occurrences age out
//! individually once they fall `window` behind `now`. Throttling is always visible in logs via
//! `proceed_logged` — never silent latency.
//!
//! Weighted costs (`proceed_cost`/`try_proceed_cost`): a call may consume more than one slot when a
//! venue meters some requests heavier than others (e.g. Binance request-weight schedules — a depth
//! snapshot or an `allOrders` scan costs many times a single order, so a post-reconnect resync burst
//! can blow the venue's weight budget while a count-only limiter still thinks it's fine). A cost-`n`
//! call is ALL-OR-NOTHING: it reserves `n` permits atomically or waits until `n` are simultaneously
//! free — it never takes a partial `k < n`. The uniform `proceed`/`try_proceed` are exactly cost-1
//! (one shared code path via [`RateGate::reserve_cost_if_free`]), byte-for-byte the pre-weighting
//! behavior. `n` permits are `n` `now` timestamps that each age out one `window` later.
//!
//! **Fairness is FIFO, and it is load-bearing rather than tidiness.** A caller that has to block
//! takes a ticket and is served in arrival order; a caller that arrives later — including a
//! non-blocking `try_proceed*` probe — may not take a slot while somebody is queued, *even when its
//! own cost would fit*. Without that rule an all-or-nothing cost-`n` call starves INDEFINITELY:
//! it needs `n` permits free at the same instant, and a stream of cost-1 callers that is itself
//! comfortably inside quota keeps at least one slot live forever, so the moment never comes. Measured
//! before the fix at capacity 4 / window 400 ms against one cost-1 call every 120 ms, the cost-4
//! caller completed only when the chatter stopped — at 1.968 s for 1.6 s of contention and 3.301 s
//! for 3.0 s of it, tracking the CONTENTION rather than the window. That is not a slow gate, it is
//! an unbounded one, and this module's own sharing story is what makes it reachable: one REST gate
//! per venue, shared by the transport and a backfill loop, so a weighted resync burst could delay
//! whatever else rides it — on the exec path, an order. The regression tests are
//! `a_heavy_caller_is_not_starved_*` and `a_probe_yields_to_a_caller_already_blocked`.
//!
//! ⚠ **Sharing a gate is something a composition root DOES, never something a `Clone` bound
//! implies.** Every clone rides one window, but nothing here makes anybody clone: a venue whose
//! transport constructor calls its own `*_gate()` factory mints a FRESH full budget per transport,
//! and on a per-IP meter that is not caution, it is a multiple of the venue's cap with every
//! window correctly reporting itself inside quota. Hyperliquid is the live instance and the one
//! that was wrong — see `vike_hyperliquid::ratelimit` for who resolves its handle and which
//! consumers take a clone.
//!
//! ⚠ The **cost** of that rule, stated plainly: while a heavy caller is queued, light callers that
//! would have sailed through now wait behind it, so a saturated gate trades throughput for a bound.
//! That is the intended trade — a bounded delay for everyone beats an unbounded one for whoever is
//! heaviest — and it engages ONLY on a saturated gate, because an empty queue leaves every path
//! byte-identical to the pre-fairness behavior.
//!
//! [`KeyedRateGate`] bundles several `RateGate`s under string keys (e.g. `"subscribe"`, `"login"`,
//! `"order"`) with an optional shared default — the blocking analog of NautilusTrader's keyed WS
//! limiter, which gates EVERY outbound WS send (subscribe/login/order/reconnect-replay) through one
//! per-key limiter. vike's WS send seam ([`crate::ws::send_gated`]) consults it before each write.

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

/// Everything the gate mutates, under ONE lock: the live window, plus the FIFO ticket pair that
/// makes it fair (see the module doc's fairness section).
struct State {
    /// timestamps of the live occurrences, oldest at the front
    slots: VecDeque<Instant>,
    /// FIFO ticket source: the number the NEXT caller that has to block will take.
    next_ticket: u64,
    /// The ticket entitled to reserve right now. `serving == next_ticket` means nobody is blocked —
    /// the uncontended case, and the ONLY one in which a fresh arrival may take a slot.
    serving: u64,
    /// TEST-ONLY observation seam: the ticket of every QUEUED caller, appended at the instant its
    /// reservation is made and **under the same lock that makes it**. `#[cfg(test)]`, so no shipped
    /// build carries the field, the push, or the accessor.
    ///
    /// It exists because the gate's service order is otherwise UNOBSERVABLE from outside.
    /// [`RateGate::proceed_cost`] returns holding nothing, so between a caller's release and any
    /// record it could make of it there is an unsynchronised gap the OS scheduler may reorder at
    /// will. A test that records after the return therefore measures PUSH order, not SERVICE order,
    /// and goes red on a perfectly fair gate — which is exactly what
    /// [`tests::blocked_callers_are_served_in_arrival_order`] did, on a loaded box and on an idle
    /// one alike (measured: `[0, 2, 1, 3]` on the first local run, and `[0, 2, 3, 1]` /
    /// `[0, 3, 1, 2]` on the CI box, all with the ticket discipline intact).
    ///
    /// Recording the TICKET rather than a caller-supplied tag is sound because the ticket IS the
    /// caller's identity once arrival order is established: `blocked()` is `next_ticket - serving`,
    /// so after the i-th waiter has been confirmed queued, `blocked() >= i + 1` can only hold when
    /// `serving == 0` and `next_ticket == i + 1` — see that test's own setup note.
    #[cfg(test)]
    served_tickets: Vec<u64>,
}

/// "N occurrences per `window`" limiter. `Clone` = shared handle.
#[derive(Clone)]
pub struct RateGate(Arc<Inner>);

impl RateGate {
    /// `occurrences` slots per sliding `window`. **Panics** if `occurrences` is `0`.
    ///
    /// ⚠ A plain `assert!`, not a `debug_assert!`, and the direction of the failure is why. A
    /// zero-occurrence gate does not throttle everything — it fails **OPEN**: with `occurrences == 0`
    /// the fit test in [`reserve_cost_if_free`](Self::reserve_cost_if_free) clamps `want` to `0` and
    /// asks `0 + 0 <= 0`, against a deque the purge leaves permanently empty, so every call returns
    /// immediately and forever. A shipped binary would meter nothing while looking metered. The root
    /// `[profile.release]` sets only `opt-level`/`lto`, so debug assertions are OFF in every binary
    /// that reaches a venue — exactly where an unmetered gate is least survivable. Construction
    /// happens once at mount, so a real assertion costs nothing and fails loudly.
    ///
    /// Nothing in this tree can build one today: every non-test caller passes
    /// `Meter::admitted()`/`window()`, which are `const fn`s a compile-time assertion already covers.
    /// The guard is for the runtime path [`crate::rate_discovery`]'s module doc invites — a budget
    /// parsed from a venue's own response.
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

    /// Non-blocking weighted reserve: take `cost` slots ALL-OR-NOTHING. `true` = all `cost` were free
    /// and are now reserved; `false` = the reservation would not have been fair or would not have fit,
    /// so NOTHING was taken (the window is left exactly as it was — a failed cost-`n` never nibbles a
    /// partial `k < n`). `cost == 0` is treated as `1`; a `cost` above the whole-window capacity is
    /// clamped to capacity (it takes the entire window rather than being forever unsatisfiable).
    ///
    /// ⚠ **`false` has TWO causes, and the second one is the fairness rule.** Either fewer than
    /// `cost` slots were free, or **somebody is already BLOCKED in
    /// [`proceed_cost`](Self::proceed_cost)** — in which case this probe is refused even when its
    /// `cost` would have fit. That is not a conservatism: a non-blocking probe allowed to take a
    /// free slot while a heavier caller waits for `n` of them is a second door onto the very
    /// starvation the queue exists to prevent, and it is the door production actually uses (every
    /// venue transport reaches the gate through [`proceed_logged`](Self::proceed_logged), which
    /// tries before it blocks). A caller that gets `false` and wants the slot should block through
    /// `proceed_cost`, which joins the queue in arrival order.
    pub fn try_proceed_cost(&self, cost: usize) -> bool {
        let mut st = self.0.state.lock().unwrap();
        if st.serving != st.next_ticket {
            return false; // somebody is blocked ahead of us; taking a slot now is what starves them
        }
        let now = Instant::now();
        self.reserve_cost_if_free(&mut st.slots, now, cost)
    }

    /// How many callers are currently BLOCKED in [`proceed_cost`](Self::proceed_cost) waiting for
    /// room. `0` on an uncontended gate.
    ///
    /// Exposed because "throttled by a full window" and "throttled by a queue ahead of me" are
    /// different operational problems with the same symptom, and an operator staring at a delayed
    /// order needs to tell them apart. [`proceed_logged`](Self::proceed_logged) and its weighted twin
    /// stamp this on their throttle warning for exactly that reason.
    pub fn blocked(&self) -> usize {
        let st = self.0.state.lock().unwrap();
        (st.next_ticket - st.serving) as usize
    }

    /// TEST-ONLY: the tickets of the callers that had to QUEUE, in the order the gate actually
    /// served them. Empty on a gate nobody ever blocked on (the fast path and `try_proceed*` take no
    /// ticket, so they are deliberately not recorded — this is the order of the QUEUE, which is the
    /// only thing FIFO is a claim about).
    ///
    /// See [`State::served_tickets`] for why the order has to be read from in here rather than
    /// assembled by the callers after they return.
    #[cfg(test)]
    fn served_tickets(&self) -> Vec<u64> {
        self.0.state.lock().unwrap().served_tickets.clone()
    }

    /// Block until a slot frees, then reserve it. The cost-1 twin of [`proceed_cost`](Self::proceed_cost).
    pub fn proceed(&self) {
        self.proceed_cost(1);
    }

    /// Block until `cost` slots are simultaneously free, then reserve them ALL-OR-NOTHING. A cost-`n`
    /// caller waits for `n` permits to be free at once and takes them atomically — it never reserves
    /// a partial `k < n` and holds it while waiting for the rest. `cost` clamping / `0` handling are
    /// as [`try_proceed_cost`](Self::try_proceed_cost).
    ///
    /// **FIFO**: a caller that has to block takes a ticket and is served in arrival order, so its
    /// wait is bounded by the callers already ahead of it rather than by how long the gate stays
    /// busy. Without that, an all-or-nothing cost-`n` starves indefinitely against a stream of
    /// cost-1 callers that never lets `n` slots be free at the same instant — see the module doc.
    pub fn proceed_cost(&self, cost: usize) {
        let mut st = self.0.state.lock().unwrap();
        // Uncontended fast path: nobody is queued and the cost fits, so taking it jumps nobody. This
        // is the overwhelmingly common case and costs one integer comparison over the old behavior.
        if st.serving == st.next_ticket
            && self.reserve_cost_if_free(&mut st.slots, Instant::now(), cost)
        {
            return;
        }
        // Join the queue. Note this is correct in both entry cases: when the gate was uncontended the
        // fast path's `serving == next_ticket` makes us the head immediately; when it was contended
        // we land behind whoever is already there.
        let ticket = st.next_ticket;
        st.next_ticket += 1;
        loop {
            let now = Instant::now();
            if st.serving != ticket {
                // Behind someone. There is nothing to compute and no timer worth setting: only the
                // head can make progress, and it wakes us when it leaves.
                st = self.0.cv.wait(st).unwrap();
                continue;
            }
            if self.reserve_cost_if_free(&mut st.slots, now, cost) {
                // The service order, recorded where it actually happens: under this lock, before it
                // is handed on. See [`State::served_tickets`] for why no observation taken after the
                // return can be sound.
                #[cfg(test)]
                st.served_tickets.push(ticket);
                st.serving += 1;
                // Hand the queue on. `notify_all` rather than `notify_one` because the condvar wakes
                // an arbitrary waiter, not necessarily the new head — and an emptied queue also has
                // to let fresh arrivals back onto the fast path.
                self.0.cv.notify_all();
                return;
            }
            // Head of the queue, but the window has no room yet. Slots free by the passage of time,
            // not by a notify, so sleep exactly until the occurrence that unblocks us ages out.
            let wait = self.time_until_room(&st.slots, now, cost);
            st = self.0.cv.wait_timeout(st, wait).unwrap().0;
        }
    }

    /// How long until `cost` permits are simultaneously free, given the live `slots`.
    ///
    /// Caller must already have found that `cost` does NOT fit — i.e. [`reserve_cost_if_free`] just
    /// returned `false`, having purged the expired entries — so every remaining slot is live and at
    /// least one of them must age out. We need `need` of the oldest occurrences gone before `cost`
    /// permits fit, and the k-th oldest frees exactly one `window` after its own timestamp. For
    /// cost-1 this is `need == 1` → the front's `oldest + window`, exactly the pre-weighting wait.
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

    /// LEAN's call pattern in one line: try first; on throttle, warn ONCE then block.
    ///
    /// The warning stamps `queued_ahead` ([`blocked`](Self::blocked)) because a full window and a
    /// queue are different problems wearing the same symptom: `queued_ahead = 0` means the venue's
    /// budget is genuinely spent, while a non-zero value means this call is waiting on OTHER callers
    /// of the same gate — a backfill or a resync burst, not the venue.
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

    /// Purge expired occurrences, then reserve `cost` slots iff the WHOLE `cost` fits (all-or-nothing).
    /// Caller holds the lock. Each reserved permit is a `now` timestamp that ages out individually one
    /// `window` later, so cost-1 is byte-identical to a single `push_back(now)` — the pre-weighting
    /// behavior. `cost == 0` counts as 1; a `cost` above capacity is clamped to capacity.
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
    /// one shared fallback gate for any key not in `gates` (NautilusTrader's `default_quota`);
    /// `None` = unlisted keys are ungated (no-op).
    default: Option<RateGate>,
}

/// A per-key bundle of [`RateGate`]s (e.g. `"subscribe"`/`"login"`/`"order"`), each with its own
/// window, plus an optional shared default. `Clone` = shared handle (every clone rides the same
/// per-key windows). The blocking, keyed WS-send limiter — Nautilus-shaped.
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

    /// Weighted twin of [`gate`](Self::gate): charge `cost` permits for one `key` send, BLOCKING
    /// (with a single warn) until `cost` are free. Use it for a heavier send that should consume more
    /// of the venue's budget than a single frame. Ungated key (no gate, no default) → no-op.
    pub fn gate_cost(&self, key: &str, cost: usize) {
        if let Some(g) = self.lookup(key) {
            g.proceed_cost_logged(self.0.venue, key, cost);
        }
    }

    /// Non-blocking twin of [`gate`](Self::gate): reserve a slot for `key` if one is free AND nobody
    /// is queued ahead on that key's gate. `true` = allowed (also `true` for an ungated key);
    /// `false` = would throttle, which — since this delegates to
    /// [`RateGate::try_proceed`](RateGate::try_proceed) — includes the fairness case where the key's
    /// window has room but a caller is already blocked on it. Take the slot by joining the queue
    /// through [`gate`](Self::gate) instead.
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
