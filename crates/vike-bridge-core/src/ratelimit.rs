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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_n_then_throttles() {
        let g = RateGate::new(3, Duration::from_secs(10));
        assert!(g.try_proceed());
        assert!(g.try_proceed());
        assert!(g.try_proceed());
        assert!(!g.try_proceed(), "the 4th within the window is throttled");
    }

    #[test]
    fn clone_shares_the_same_window() {
        let g = RateGate::new(1, Duration::from_secs(10));
        let g2 = g.clone();
        assert!(g.try_proceed());
        assert!(!g2.try_proceed(), "a clone rides the same quota, not its own");
    }

    #[test]
    fn slot_frees_after_the_window_elapses() {
        let g = RateGate::new(1, Duration::from_millis(100));
        assert!(g.try_proceed());
        assert!(!g.try_proceed());
        std::thread::sleep(Duration::from_millis(160));
        assert!(g.try_proceed(), "the window elapsed, so the one slot is free again");
    }

    #[test]
    fn proceed_blocks_until_a_slot_frees() {
        let g = RateGate::new(1, Duration::from_millis(120));
        g.proceed(); // take the only slot immediately
        let t = Instant::now();
        g.proceed(); // must block ~window until the first ages out
        let waited = t.elapsed();
        assert!(
            waited >= Duration::from_millis(90),
            "proceed should block ~one window; waited {waited:?}"
        );
    }

    #[test]
    fn window_slides_occurrences_age_out_individually() {
        let g = RateGate::new(2, Duration::from_millis(200));
        assert!(g.try_proceed()); // t≈0
        std::thread::sleep(Duration::from_millis(100));
        assert!(g.try_proceed()); // t≈100
        assert!(!g.try_proceed(), "two in the window → throttled");
        std::thread::sleep(Duration::from_millis(140)); // t≈240: only the t≈0 one aged out
        assert!(g.try_proceed(), "exactly one slot freed as the window slid");
        assert!(!g.try_proceed(), "the t≈100 occurrence still occupies the second slot");
    }

    // --- weighted costs --------------------------------------------------------------------------

    #[test]
    fn cost_n_consumes_n_slots() {
        let g = RateGate::new(5, Duration::from_secs(10));
        assert!(g.try_proceed_cost(3), "cost-3 fits in a fresh 5-slot window");
        // 3 of 5 taken; exactly 2 remain, provable by two cost-1 calls then a throttle.
        assert!(g.try_proceed(), "1st of the 2 remaining slots");
        assert!(g.try_proceed(), "2nd of the 2 remaining slots");
        assert!(!g.try_proceed(), "all 5 slots consumed (3 + 1 + 1) — the 6th throttles");
    }

    #[test]
    fn cost_one_is_identical_to_plain_try_proceed() {
        // try_proceed IS try_proceed_cost(1); mixing the two must fold into one shared window with the
        // exact pre-weighting throttle point.
        let g = RateGate::new(2, Duration::from_secs(10));
        assert!(g.try_proceed_cost(1));
        assert!(g.try_proceed(), "the plain call rides the same window as the weighted cost-1");
        assert!(!g.try_proceed(), "2-slot window full after two cost-1 reservations");
        assert!(!g.try_proceed_cost(1), "the weighted cost-1 path throttles identically");
    }

    #[test]
    fn failed_cost_n_reserves_nothing() {
        // All-or-nothing: a cost that does not fit must leave the window untouched (no partial take).
        let g = RateGate::new(5, Duration::from_secs(10));
        assert!(g.try_proceed_cost(4), "4 of 5 taken");
        assert!(!g.try_proceed_cost(2), "cost-2 won't fit in the 1 free slot — reserve nothing");
        assert!(
            g.try_proceed(),
            "the lone free slot survived the failed cost-2 (nothing was nibbled)"
        );
        assert!(!g.try_proceed(), "now genuinely full (4 + 1)");
    }

    #[test]
    fn proceed_cost_returns_immediately_when_the_cost_fits() {
        let g = RateGate::new(5, Duration::from_secs(10));
        let t = Instant::now();
        g.proceed_cost(3); // fits in the empty window — must not block
        assert!(t.elapsed() < Duration::from_millis(50), "a fitting cost must return promptly");
        assert!(g.try_proceed_cost(2), "3 taken, 2 free");
        assert!(!g.try_proceed(), "5/5 consumed");
    }

    #[test]
    fn cost_n_blocks_until_n_slots_free_not_just_one() {
        // window=200ms, capacity=2, staggered so the two occupied slots age out 100ms apart. A cost-2
        // must wait for the SECOND (~+200ms), not merely the first (~+100ms, which a cost-1 would take).
        let g = RateGate::new(2, Duration::from_millis(200));
        g.proceed(); // slot A at t≈0
        std::thread::sleep(Duration::from_millis(100));
        g.proceed(); // slot B at t≈100 — window now full (2/2)
        let t = Instant::now(); // ≈100
        g.proceed_cost(2); // needs BOTH free: A ages out ≈200, B ≈300 → unblocks ≈300
        let waited = t.elapsed();
        assert!(
            waited >= Duration::from_millis(160),
            "cost-2 waits for the 2nd occurrence to age out (~200ms), not just the 1st (~100ms); \
             waited {waited:?}"
        );
    }

    #[test]
    fn cost_above_capacity_is_clamped_not_unsatisfiable() {
        // A cost larger than the whole window must not deadlock: it clamps to capacity and takes the
        // entire window (and, when empty, returns without blocking).
        let g = RateGate::new(3, Duration::from_secs(10));
        let t = Instant::now();
        g.proceed_cost(9); // clamps to 3, the whole window; must not block on an empty gate
        assert!(
            t.elapsed() < Duration::from_millis(50),
            "an over-capacity cost on an empty gate returns"
        );
        assert!(!g.try_proceed(), "the window is now fully consumed");
    }

    // --- KeyedRateGate ---------------------------------------------------------------------------

    fn wide() -> Duration {
        Duration::from_secs(30)
    }

    #[test]
    fn keyed_quota_applies_per_key() {
        let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(2, wide()))], None);
        assert!(g.try_gate("subscribe"));
        assert!(g.try_gate("subscribe"));
        assert!(!g.try_gate("subscribe"), "3rd subscribe within the window is throttled");
    }

    #[test]
    fn unlisted_key_is_ungated_without_a_default() {
        let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(1, wide()))], None);
        // no "order" gate and no default → always allowed, never throttles
        assert!(g.try_gate("order"));
        assert!(g.try_gate("order"));
        assert!(g.try_gate("order"));
    }

    #[test]
    fn default_is_one_shared_bucket_for_unlisted_keys() {
        let g = KeyedRateGate::new("okx", vec![], Some(RateGate::new(1, wide())));
        assert!(g.try_gate("anything"));
        // the default is a SINGLE shared gate, so a different unlisted key hits the same window
        assert!(!g.try_gate("something-else"), "default bucket is shared across unlisted keys");
    }

    #[test]
    fn distinct_keys_have_independent_windows() {
        let g = KeyedRateGate::new(
            "okx",
            vec![("login", RateGate::new(1, wide())), ("subscribe", RateGate::new(1, wide()))],
            None,
        );
        assert!(g.try_gate("login"));
        assert!(g.try_gate("subscribe"), "subscribe has its own window, unaffected by login");
        assert!(!g.try_gate("login"));
        assert!(!g.try_gate("subscribe"));
    }

    #[test]
    fn clone_shares_the_keyed_windows() {
        let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(1, wide()))], None);
        let g2 = g.clone();
        assert!(g.try_gate("subscribe"));
        assert!(!g2.try_gate("subscribe"), "a clone rides the same per-key windows");
    }

    #[test]
    fn keyed_cost_consumes_multiple_slots_of_its_key() {
        let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(4, wide()))], None);
        assert!(g.try_gate_cost("subscribe", 3), "cost-3 fits in the 4-slot subscribe window");
        assert!(
            !g.try_gate_cost("subscribe", 2),
            "only 1 slot left — cost-2 throttles (nothing taken)"
        );
        assert!(g.try_gate("subscribe"), "the lone remaining slot still admits a cost-1 send");
        assert!(!g.try_gate("subscribe"), "subscribe window now full (3 + 1)");
    }

    #[test]
    fn keyed_cost_on_an_ungated_key_is_always_allowed() {
        let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(1, wide()))], None);
        // no "order" gate and no default → the weighted gate is a pure passthrough, any cost allowed.
        assert!(g.try_gate_cost("order", 100));
        assert!(g.try_gate_cost("order", 100));
    }

    #[test]
    fn keyed_cost_one_matches_plain_try_gate() {
        let g = KeyedRateGate::new("okx", vec![("subscribe", RateGate::new(2, wide()))], None);
        assert!(g.try_gate_cost("subscribe", 1));
        assert!(g.try_gate("subscribe"), "plain gate rides the same window as the weighted cost-1");
        assert!(!g.try_gate("subscribe"), "2-slot window full after two cost-1 sends");
    }

    /// How often the competing cost-1 caller talks in the two starvation tests, against their shared
    /// 4-per-400 ms gate.
    ///
    /// ⚠ **This number is load-bearing and 120 ms — the value the defect was first measured at — is
    /// the WRONG one to commit.** The chatter has to stay far enough inside quota that it never has
    /// to block on its own account, because the moment it blocks it joins the queue, stops taking
    /// slots, and hands the heavy caller the window it was being denied — turning a broken gate
    /// green. At 120 ms its demand is 400/120 = 3.33 of 4 slots (83 % of quota), close enough to the
    /// ceiling that it transiently fills the window and self-queues. At 150 ms the demand is 2.67 of
    /// 4 (67 %), so a slot is always free and the chatter never blocks unless the gate makes it.
    ///
    /// Measured, not reasoned: with 120 ms, deliberately breaking the fast-path guard
    /// (`proceed_cost` reserving without checking the queue) left all 23 tests GREEN. With 150 ms the
    /// same break is caught.
    const CHATTER_PERIOD: Duration = Duration::from_millis(150);

    /// How long a readiness spin may wait for a caller to show up as
    /// [`blocked`](RateGate::blocked) before the test gives up and says so.
    ///
    /// A BACKSTOP, not a measurement: the spins it guards normally complete in microseconds, and
    /// this only exists so a violated setup assumption fails by name instead of hanging. It must
    /// stay comfortably UNDER the window of whichever gate it is spinning against — past that point
    /// the gate starts serving, `blocked()` falls rather than rises, and the condition the spin is
    /// waiting for can never be reached.
    const SETUP_BUDGET: Duration = Duration::from_millis(1_500);

    /// A zero-occurrence gate is refused at construction, because the alternative is silent.
    ///
    /// The failure direction is the whole point: such a gate does NOT throttle everything, it admits
    /// everything — instantly and forever — while still presenting as a limiter. This pins the
    /// `assert!` in [`RateGate::new`] rather than the `debug_assert!` it replaced, which every
    /// release binary drops; see that constructor's doc for why a shipped unmetered gate is the
    /// worst-placed version of this bug.
    #[test]
    #[should_panic(expected = "at least one occurrence")]
    fn a_zero_occurrence_gate_is_refused_rather_than_failing_open() {
        let _ = RateGate::new(0, Duration::from_secs(1));
    }

    /// **Regression test for the starvation fixed by the FIFO queue** (was `#[ignore]`d as a failing
    /// reproduction while the defect stood; it now runs on every PR).
    ///
    /// A cost-`n` caller must not be starved by a stream of cost-1 callers that are themselves well
    /// inside quota.
    ///
    /// # The mechanism, as it was
    ///
    /// [`RateGate::proceed_cost`] had no QUEUE. A blocked waiter slept on a timeout, and on waking
    /// had to re-acquire the mutex and re-check — by which time a freshly ARRIVING cost-1 caller may
    /// already have taken the slot it was waiting for. Because a cost-`n` reservation is
    /// all-or-nothing (it needs `n` SIMULTANEOUSLY free slots — see
    /// [`RateGate::reserve_cost_if_free`]), a trickle that kept even ONE slot live meant the heavy
    /// caller never saw its window. The cure is the ticket pair on [`State`]: a caller that blocks is
    /// served in arrival order, and nobody — probe or blocker — may take a slot ahead of it.
    ///
    /// # Measured 2026-08-23 BEFORE the fix — capacity 4, window 400 ms, one cost-1 call every 120 ms
    ///
    /// (The committed test paces the chatter at [`CHATTER_PERIOD`] instead, for the reason given
    /// there. The 120 ms figures below are the original measurement and are kept as the record.)
    ///
    /// | contention held for | cost-4 completed at |
    /// |---|---|
    /// | 1.6 s | 1.968 s |
    /// | 3.0 s | 3.301 s |
    ///
    /// It tracked the CONTENTION, not the window: the heavy caller finished only once the chatter
    /// stopped. The competing caller is not misbehaving — one call per 120 ms against 4-per-400 ms is
    /// inside quota, and would look healthy in any single-caller test. (Inside, but only just: 83 %,
    /// which is why the committed test paces at [`CHATTER_PERIOD`] instead.) Under the fair gate the
    /// same setup completes in about ONE window, whatever the chatter does afterwards.
    ///
    /// # Why no existing test caught it
    ///
    /// Every other test in this module is SINGLE-CALLER.
    /// `cost_n_blocks_until_n_slots_free_not_just_one` proves the wait DURATION with nobody
    /// contending; `clone_shares_the_same_window` proves the sharing MECHANISM with two sequential
    /// non-blocking calls. Neither puts two callers in contention, and that interaction is the only
    /// place this defect lives.
    ///
    /// # Why it matters
    ///
    /// This module's own doc describes a venue sharing ONE REST gate between its transport and a
    /// backfill loop. A weighted resync burst, or a chatty feed, could therefore indefinitely delay
    /// whatever else rides that gate — and on the exec path that is a delayed ORDER. Hyperliquid is
    /// the live instance: one per-IP weight pool (`vike_model::venue_rate_limits::HYPERLIQUID`'s
    /// `rest_ip_weight` row owns the numbers), clone-shared by the mount across the exec thread,
    /// the funding poller and the reconcile client, where a `userRole` read costs 60 and a batched
    /// order submit costs `1 + len/40`. NOT across the feeds: `vike_hyperliquid`'s market and
    /// user-data pumps are WebSocket and charge this gate nothing. That mis-naming is worth keeping
    /// in view — it described contention on a lane that does not exist while omitting the funding
    /// poller, which does, and the mount it described shipped three UNSHARED windows.
    #[test]
    fn a_heavy_caller_is_not_starved_by_a_stream_of_light_ones() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let g = RateGate::new(4, Duration::from_millis(400));
        // Seed one occurrence BEFORE either thread starts. Without it the heavy caller races the
        // chatter for the first lock, and when it wins it takes all four slots of an empty gate and
        // returns instantly — the test then passes having contended with nothing. Measured: that
        // false green is what let a deliberately broken gate look fixed during the mutation check.
        assert!(g.try_proceed(), "the gate is contended before the heavy caller ever arrives");
        let stop = Arc::new(AtomicBool::new(false));
        let chatter = {
            let (g, stop) = (g.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    g.proceed(); // cost-1, comfortably inside 4-per-400ms
                    std::thread::sleep(CHATTER_PERIOD);
                }
            })
        };
        let t = Instant::now();
        let heavy = {
            let g = g.clone();
            std::thread::spawn(move || {
                g.proceed_cost(4);
                t.elapsed()
            })
        };
        // Four windows is far longer than any honest wait for this gate.
        std::thread::sleep(Duration::from_millis(1_600));
        let finished_while_contended = heavy.is_finished();
        stop.store(true, Ordering::Relaxed);
        let waited = heavy.join().unwrap();
        chatter.join().unwrap();
        assert!(
            finished_while_contended,
            "a cost-4 caller was still blocked after 1600 ms (4x the window) while a cost-1 stream \
             that is itself inside quota kept talking; it completed at {waited:?}, i.e. only once \
             the contention stopped. A gate that starves its heavy caller indefinitely delays \
             whatever shares it — on the exec path, an order."
        );
    }

    /// **Regression test: the sibling above, driven through the door production actually uses.**
    ///
    /// Every real blocking call site reaches the gate through [`RateGate::proceed_logged`] /
    /// [`RateGate::proceed_cost_logged`] — LEAN's "try first, warn once, then block" — never through
    /// a bare `proceed`. `vike_bridge_core::transport`, okx's and bybit's transports, deribit's
    /// `gate()`, and hyperliquid's weighted `/info` + `/exchange` charges are all that shape.
    ///
    /// That matters because the try half is [`RateGate::try_proceed`], and a NON-BLOCKING probe that
    /// is allowed to take a free slot while a blocked caller waits for one re-creates the starvation
    /// through a second door. So a fix that queues only `proceed_cost` turns the sibling above green
    /// and leaves this one red — which is the whole reason this test exists separately.
    #[test]
    fn a_heavy_caller_is_not_starved_through_the_logged_door() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let g = RateGate::new(4, Duration::from_millis(400));
        // Seed one occurrence BEFORE either thread starts. Without it the heavy caller races the
        // chatter for the first lock, and when it wins it takes all four slots of an empty gate and
        // returns instantly — the test then passes having contended with nothing. Measured: that
        // false green is what let a deliberately broken gate look fixed during the mutation check.
        assert!(g.try_proceed(), "the gate is contended before the heavy caller ever arrives");
        let stop = Arc::new(AtomicBool::new(false));
        let chatter = {
            let (g, stop) = (g.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    g.proceed_logged("test", "chatter"); // try_proceed, then block — the LEAN pattern
                    std::thread::sleep(CHATTER_PERIOD);
                }
            })
        };
        let t = Instant::now();
        let heavy = {
            let g = g.clone();
            std::thread::spawn(move || {
                g.proceed_cost_logged("test", "resync", 4);
                t.elapsed()
            })
        };
        std::thread::sleep(Duration::from_millis(1_600));
        let finished_while_contended = heavy.is_finished();
        stop.store(true, Ordering::Relaxed);
        let waited = heavy.join().unwrap();
        chatter.join().unwrap();
        assert!(
            finished_while_contended,
            "a cost-4 caller was still blocked after 1600 ms (4x the window) while a cost-1 stream \
             rode the try-then-block path every venue transport uses; it completed at {waited:?}. \
             Queueing only the BLOCKING path leaves the non-blocking probe barging past it."
        );
    }

    /// **Regression test: the mechanism above, isolated and made deterministic.**
    ///
    /// The two tests above are timing races by nature — they prove the SYMPTOM. This one pins the
    /// RULE that fixes them, with no race in it: while a caller is blocked waiting for room, a
    /// non-blocking probe must not take a slot the blocked caller is waiting on, **even when that
    /// probe would fit**. Free capacity is not the question; arrival order is.
    #[test]
    fn a_probe_yields_to_a_caller_already_blocked() {
        // ONE occurrence of four, so three slots stay free for the whole test — room for a cost-1,
        // never for the cost-4 that is waiting. ⚠ No sleep stands between the setup and the
        // assertion: the probe fires the instant `blocked()` proves the heavy caller is queued. An
        // earlier draft instead aged occurrences out on a clock and asserted inside a 250 ms window,
        // which CI (`cargo nextest`, tests in PARALLEL) could drift straight past — and once the
        // heavy caller completes, the queue is empty and the probe legitimately succeeds. A timing
        // window that flips the expected answer is a flake, not a test.
        //
        // ⚠ The window is 2 s for the same reason the sibling below uses one, and it was 600 ms:
        // that is BELOW [`SETUP_BUDGET`], which inverts the guard. Past 600 ms the lone occurrence
        // ages out, the heavy caller completes, `blocked()` returns to 0 permanently, and the spin
        // then burns the rest of its budget before failing by name — a spurious red bought by a slow
        // thread spawn rather than by an unfair gate. The budget must stay under the window of every
        // gate it spins against, which is what its own doc says and what this now satisfies.
        let g = RateGate::new(4, Duration::from_millis(2_000));
        assert!(g.try_proceed(), "one occurrence taken; three of four slots stay free");
        let heavy = {
            let g = g.clone();
            std::thread::spawn(move || g.proceed_cost(4))
        };
        // Same cumulative-deadline rule as the sibling below, and needed for the same reason: if this
        // spin ever outran the window the heavy caller would complete, `blocked()` would sit at 0
        // forever, and an unbounded spin would hang rather than report.
        let setup = Instant::now();
        while g.blocked() == 0 {
            assert!(
                setup.elapsed() < SETUP_BUDGET,
                "the cost-4 caller never showed up as blocked within {SETUP_BUDGET:?}"
            );
            std::thread::yield_now();
        }
        assert!(
            !g.try_proceed(),
            "3 of 4 slots are free, but a cost-4 caller queued first. Admitting this probe is \
             precisely what starves it: the probe's occurrence keeps the window from ever showing \
             4 free at once."
        );
        heavy.join().unwrap(); // unblocks once the lone occurrence ages out
    }

    /// **Regression test: the queue is FIFO, not merely "a queue".**
    ///
    /// The three tests above prove that nobody is starved. This one proves the stronger property the
    /// fix actually implements — blocked callers are served in ARRIVAL order — because a gate that
    /// releases waiters in an arbitrary order is still unbounded for whichever one keeps losing the
    /// race. That is the same defect wearing a different face, and "not starved" alone would not
    /// catch it.
    ///
    /// Deterministic by construction, in BOTH halves — and the second half is the one that had to be
    /// repaired:
    ///
    /// * **Arrival order is established, not assumed.** Each thread is confirmed BLOCKED via
    ///   [`RateGate::blocked`] before the next is spawned, rather than inferred from a sleep a loaded
    ///   box could reorder. That spin is sound, and the arithmetic is worth spelling out because it
    ///   is what makes waiter `i` provably the holder of ticket `i`: `blocked()` is
    ///   `next_ticket - serving`, and only waiters `0..=i` exist when the i-th spin runs, so
    ///   `next_ticket <= i + 1` always. Therefore `blocked() >= i + 1` can hold ONLY when
    ///   `serving == 0` and `next_ticket == i + 1` — nobody served yet, exactly `i + 1` arrived.
    ///   The condition cannot be satisfied by "some other combination" once servicing has begun; a
    ///   run whose setup outran the window sees the count FALL, spins, and fails by name on
    ///   [`SETUP_BUDGET`] instead of proceeding to assert ordering.
    ///
    /// * **⚠ Service order is read from the gate, because it is unobservable from outside.** This is
    ///   the repair. The test used to have each waiter `push(i)` onto a shared `Vec` after its
    ///   `proceed()` returned — and [`RateGate::proceed_cost`] returns holding NOTHING, so between
    ///   the release and that push sits an unsynchronised gap the scheduler may reorder at will. A
    ///   waiter preempted in that gap records after a later one, and the vector comes out scrambled
    ///   while the gate released in perfect ticket order. So the old assertion measured PUSH order
    ///   and called it service order: it could go red on a flawless gate, which it duly did — three
    ///   the CI box merge gates blocked in one day (`[0, 2, 3, 1]` and `[0, 3, 1, 2]` at different test
    ///   indices, green 3/3 when run alone), and `[0, 2, 1, 3]` on the first local run of the
    ///   untouched tree.
    ///
    ///   Measured with both orders captured in the SAME run, 40 rounds (20 idle, 20 with eight
    ///   concurrent suite runs loading the box): the recorded push order was wrong 5 times, the
    ///   gate's own service order 0 times — e.g. `PUSH [1, 2, 0, 3]` against `SERVICE [0, 1, 2, 3]`.
    ///   The load-sensitivity is visible in the split (2/20 idle vs 3/20 loaded), which is why this
    ///   presented as "green alone, red in the full suite".
    ///
    ///   [`State::served_tickets`] is the fix: the ticket is appended at the instant the reservation
    ///   is made, under the same lock that makes it, so the recorded sequence IS the service order
    ///   with no gap to race. It is `#[cfg(test)]` — no shipped build carries it.
    ///
    /// * **⚠ The waiters are NOT interchangeable, and that is what gives the test teeth.** Sound
    ///   observation is not the same as detection: with four equal cost-1 waiters against four slots
    ///   that free simultaneously, an unfair gate merely races, and the race is heavily FIFO-biased
    ///   anyway because the waiters enter their timed waits in arrival order and their timers fire in
    ///   the order they were set. Measured on exactly that shape with the head guard deleted, the
    ///   ordering assertion caught it **4 runs in 10** — a mutation that survives 60 % of the time is
    ///   not a gate. So the scenario is built so the head CANNOT fit while everyone behind it CAN:
    ///   ticket 0 wants a cost of 4 against 3 free slots, tickets 1–3 want 1 each. A gate that honours
    ///   arrival order serves nobody at all until the window opens; a gate that does not lets the
    ///   light callers through immediately, with no timing to get lucky about. That turns detection
    ///   from a coin flip into a structural certainty, and it is also the exact production hazard —
    ///   the heavy resync burst held behind nothing while light chatter walks past it.
    #[test]
    fn blocked_callers_are_served_in_arrival_order() {
        // Five of eight slots taken in ONE go at t≈0, so all five age out together one window later
        // and the whole queue drains in a single opening (the head takes 4 of the 8, the three light
        // callers 1 each — nothing has to wait for a second window, which is what keeps this test as
        // short as the one it replaces).
        //
        // The 3 slots left free are the load-bearing part: they are room for every cost-1 caller
        // behind the head and never room for the cost-4 head itself, and they stay that way for the
        // entire setup. So "nothing has been served yet" is a STRUCTURAL statement about a gate that
        // honours arrival order, not a race the test hopes to win.
        //
        // ⚠ The window is the SETUP BUDGET, not a tuning knob: nothing may be served until every
        // waiter is queued, because the spin below waits for `blocked()` to COUNT UP — if the head
        // were served mid-setup the count would fall and the loop could never reach its target.
        // Hence a window far longer than four thread spawns need, and the deadline that turns a
        // violated assumption into a named failure instead of a hung test on a loaded CI box.
        //
        // ⚠ That deadline is CUMULATIVE and must stay under the window, which an earlier revision got
        // wrong in a way worth naming: it started the clock INSIDE the loop, so the budget reset on
        // every iteration while the window it guards runs once, from t0. Four iterations could each
        // sit just under the per-iteration cap and still outrun the window between them — and past
        // that point `serving` climbs, `blocked()` FALLS, and iteration i's target of `i + 1` becomes
        // unreachable, so the restarting deadline was the only thing left to end the test. One clock,
        // hoisted, budgeted well below the window.
        let g = RateGate::new(8, Duration::from_millis(2_000));
        assert!(
            g.try_proceed_cost(5),
            "5 of 8 taken: room for a cost-1, never for the cost-4 head"
        );
        let mut waiters = Vec::new();
        let setup = Instant::now();
        // Ticket 0 is the head and cannot fit; tickets 1..=3 each fit RIGHT NOW and must still wait.
        for (i, cost) in [4usize, 1, 1, 1].into_iter().enumerate() {
            let gate = g.clone();
            waiters.push(std::thread::spawn(move || gate.proceed_cost(cost)));
            // Everyone spawned so far, this one included, must be queued before the next arrives —
            // that is what pins waiter `i` to ticket `i`. Bound rather than spelled `>= i + 1` at the
            // comparison, which clippy's `int_plus_one` rejects.
            let all_queued = i + 1;
            // ⚠ A `loop`, not a `while`, and deliberately so: both guards must run at least once per
            // waiter. As a `while` the body is skipped entirely whenever the condition is already
            // true, so a spin that never iterates checks NEITHER the budget nor the barge rule — and
            // the barge rule is the one an unfair gate trips first.
            loop {
                assert!(
                    setup.elapsed() < SETUP_BUDGET,
                    "waiter {i} never showed up as blocked within {SETUP_BUDGET:?} of setup start — \
                     the setup outran the window, so this run proves nothing about ordering"
                );
                // The FIFO rule's teeth, checked while the queue is still assembling: three slots are
                // free and every cost-1 caller behind the head would fit in one, so a gate that lets
                // arrival order be overtaken has already served somebody by now. It also gives that
                // failure its own name — without it an overtaking gate stalls `blocked()` below its
                // target and reports the budget message instead, blaming a slow box for unfairness.
                assert!(
                    g.served_tickets().is_empty(),
                    "the gate served ticket(s) {:?} while ticket 0 was queued and could not fit. \
                     3 of 8 slots are free, so every cost-1 caller behind the head fits — and \
                     admitting one is exactly what starves the head: its occurrence keeps the window \
                     from ever showing the 4 free at once that the head is waiting for.",
                    g.served_tickets()
                );
                if g.blocked() >= all_queued {
                    break;
                }
                std::thread::yield_now();
            }
            // Past this point waiter `i` provably holds ticket `i`: the spin above can only have
            // ended with `serving == 0` and `next_ticket == i + 1` (see this test's doc), and
            // waiters `0..i` were pinned to tickets `0..i` by the same argument on earlier passes.
        }
        for w in waiters {
            w.join().unwrap();
        }
        // The gate's OWN record, written under the reservation lock — not a vector the waiters
        // assembled after returning, which is the same list only when the scheduler cooperates.
        assert_eq!(
            g.served_tickets(),
            vec![0u64, 1, 2, 3],
            "blocked callers must be served in ARRIVAL order: waiter i queued i-th and so holds \
             ticket i, and the gate must hand the window to the tickets in ascending order. A \
             permutation here means a caller that arrived later reserved first, which leaves \
             whoever keeps losing that race unbounded — the starvation the queue exists to prevent, \
             wearing a different face."
        );
    }
}
