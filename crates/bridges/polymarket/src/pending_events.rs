//! Polymarket **pending user-channel event park** — the bounded, expiring staging area for a
//! user-WS event whose CLOB order id the [`PolymarketRegistry`](crate::registry::PolymarketRegistry)
//! could not re-key **yet**.
//!
//! WHY THIS EXISTS. The CLOB echoes no client id: it keys every order by its EIP-712 order hash and
//! the registry is the only bridge back to the coid the core FSM folds on. [`crate::user_ws`]'s
//! three re-key sites used to have no `else` arm at all — a trade event whose id was absent was
//! simply dropped, and the A3 history resync replayed through the *same* gate, so nothing ever
//! recovered it. A dropped trade event is the worst failure class this repo has: the bare
//! `Event::Fill` folds `Account` (position + realized PnL) independently of the order FSM, so a
//! silent drop leaves the platform's position permanently disagreeing with the venue's, with no log
//! line anywhere.
//!
//! THE TWO REACHABLE RACES that produce an absent id (both verified against the code, not assumed):
//!
//! 1. **The fill beats the HTTP ack.** The exec thread only calls `on_accept` once
//!    `submit_order` returns; the venue can match and push the trade frame down the user WS first.
//!    The mitigation that closes this at the source — `POLY_PRESUBMIT_REGISTER`
//!    ([`crate::mount::presubmit_register_enabled`]) — ships **OFF** (`crate::client`'s
//!    `presubmit_register` field), so the race is live in every default deployment.
//! 2. **A cancel racing a match.** `crate::client`'s Cancel arm treats any HTTP 200 from
//!    `cancel_order` as `CancelOutcome::Canceled` and runs `registry.remove(&coid)` — it never
//!    inspects the `not_canceled` half of the CLOB's response body. So cancelling an order the
//!    venue *just matched* still tears the mapping down, and the trade frame that follows finds
//!    nothing. This one is handled by the registry's **settling grace** rather than by this park
//!    (the id can never come back, so there would be nothing to replay for) — see
//!    [`PolymarketRegistry::remove`](crate::registry::PolymarketRegistry::remove).
//!
//! THE SHAPE (the established one — PR #937, "stage what you cannot place, and make any genuine
//! loss observable"): an event that cannot be re-keyed now is **parked** under the CLOB id it is
//! waiting on, and the registry hands it straight back the instant that id is inserted — atomically,
//! under the registry's own mutex, so the exec thread cannot slip an `on_accept` between the
//! decoder's failed lookup and its park (see [`PendingUserEvents`]'s placement inside `Maps`).
//!
//! "NOT OURS" vs "NOT **YET** OURS" — the distinction the old doc conflated, and the conflation is
//! precisely what hid the bug. They are indistinguishable *at arrival*; what separates them is what
//! happens next, so that is where they are separated:
//!
//! * **not yet ours** — the registry gains the id inside [`DEFAULT_TTL_MS`] ⇒ replayed and emitted.
//!   Logged at `debug` on the way in, `info` on the way out. Never a `warn`: this is the benign,
//!   self-healing case and warning on it would train operators to ignore the real one.
//! * **not ours** — the TTL passes with no insert ⇒ `warn!` + a monotonic counter
//!   ([`PendingStats::expired`]), naming sample ids. This is the genuinely-unrecoverable case:
//!   another client on the same wallet, or our own order from a previous process (the registry is
//!   in-memory, so a restart forgets every resting order). Account-level `POLY_RECONCILE` is the
//!   backstop for it; this lane's job is to make it *visible* instead of silent.
//!
//! Replay is safe by construction: the core dedups bare fills on `trade_id` and the FSM wrapper on
//! the composite `"{trade_id}:{order_id}"`, and [`crate::fill_tracker`] keeps its own per-order seen
//! set on that same composite — so a replayed frame that overlaps something already emitted folds
//! exactly once, the same property the A3 resync already relies on.

use indexmap::IndexMap;

/// Max parked events held across all CLOB ids before the oldest bucket is evicted.
///
/// Sized for the population this can legitimately hold, then given room: the ack-race window is one
/// proxy-routed HTTP submit round-trip, so at most the orders a maker has in flight at once (tens),
/// each with a handful of frames. The number is generous instead of tight because the *other*
/// legitimate producer is a process restart, where every still-resting order from the previous
/// session is unknown at once. At ~500 bytes per parked frame this ceiling is ~2 MB — a bound worth
/// having, not a budget worth economising.
///
/// AT THE BOUND the OLDEST bucket is evicted, and the eviction is reported exactly like an expiry
/// (`warn!` + [`PendingStats::evicted`]) — never a silent discard, which is the whole point of this
/// module. Oldest-first is the right end to drop: the ack race is sub-second, so an entry that has
/// already survived thousands of newer arrivals is overwhelmingly likely to be foreign.
pub const DEFAULT_CAPACITY: usize = 4096;

/// Max parked events for ONE CLOB id, so a single pathological order cannot consume the whole park
/// and evict every other order's genuinely-recoverable fills. Oldest-first within the bucket, and
/// reported on the same eviction path as [`DEFAULT_CAPACITY`].
pub const DEFAULT_MAX_PER_ORDER: usize = 64;

/// How long an unresolved event is held before it is declared **not ours** and discarded loudly.
///
/// It must comfortably exceed the window it exists to cover — the gap between the venue accepting
/// an order and our exec thread running `on_accept`, i.e. the response leg of one blocking
/// `submit_order` POST, which on this venue is routed through the Dublin SOCKS proxy. 30s is well
/// past any submit that has not already failed on its own transport timeout, while still being
/// short enough that the `warn!` reaches an operator near the event rather than long after it.
pub const DEFAULT_TTL_MS: u64 = 30_000;

/// Which decoder a parked frame must be replayed through. Kept alongside the frame because the
/// user channel multiplexes both kinds and a `/data/*` history row may omit `event_type` entirely
/// (the reason [`crate::user_ws::decode_typed`] takes the kind as a parameter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserEventKind {
    /// A `trade` frame — the one that carries money (fills).
    Trade,
    /// An `order` frame — CANCELLATION or a terminal UPDATE. A PLACEMENT is never parked: it
    /// decodes to nothing, so parking it would spend capacity to replay a no-op.
    Order,
}

/// One user-channel frame held for a CLOB id the registry could not re-key when it arrived.
#[derive(Debug, Clone)]
pub struct ParkedEvent {
    /// Which decoder replays it.
    pub kind: UserEventKind,
    /// The frame VERBATIM. Replay re-runs the real decoder over it rather than reconstructing the
    /// fill from extracted fields, so a replayed event is byte-identical to what the live path
    /// would have emitted had the id been present — no second, drifting extraction to maintain.
    pub frame: serde_json::Value,
    /// Monotonic park time (see [`now_ms`]), for the TTL sweep and the age reported when it dies.
    pub parked_ms: u64,
}

/// Cumulative, monotonic counters for everything this lane did — the observable half of "a silent
/// drop must not survive in any form". Read via
/// [`PolymarketRegistry::pending_stats`](crate::registry::PolymarketRegistry::pending_stats).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PendingStats {
    /// Events parked because their CLOB id was not registered at arrival.
    pub parked: u64,
    /// Parked events handed back and re-emitted because the registry gained the id ("not YET ours").
    pub replayed: u64,
    /// Parked events discarded by the TTL — the genuine, unrecoverable loss ("not ours").
    pub expired: u64,
    /// Parked events discarded by a capacity bound rather than by age. Distinct from `expired`
    /// because it means the bound is too small, not that the event was foreign.
    pub evicted: u64,
    /// Events re-keyed through the settling grace — i.e. matches that raced their own cancel and
    /// would otherwise have been dropped outright (race 2 above).
    pub settled: u64,
}

/// Process-monotonic milliseconds. `Instant`-based deliberately: the TTL must not be steerable by a
/// wall-clock step (NTP, DST, a VM resume), and this park is the last thing standing between a real
/// fill and a silent discard.
pub fn now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The bounded park itself. **Not** independently synchronised: it lives inside the registry's
/// `Mutex` so that "look the id up, and park iff it is absent" and "insert the id, and take back
/// whatever was parked for it" are each ONE critical section. That is what makes the fix race-free
/// rather than merely likely — a park that owned a separate lock would still let the exec thread's
/// `on_accept` land between the decoder's failed lookup and its park, stranding the event forever.
#[derive(Debug)]
pub(crate) struct PendingUserEvents {
    /// clob id → frames waiting on it. Insertion-ordered so eviction can take the OLDEST bucket in
    /// O(1) without a scan; a bucket's own `Vec` is likewise oldest-first.
    by_clob: IndexMap<String, Vec<ParkedEvent>>,
    /// Total parked events across every bucket (kept alongside so the bound is O(1) to test).
    len: usize,
    capacity: usize,
    max_per_order: usize,
    ttl_ms: u64,
}

impl Default for PendingUserEvents {
    fn default() -> Self {
        Self::with_limits(DEFAULT_CAPACITY, DEFAULT_MAX_PER_ORDER, DEFAULT_TTL_MS)
    }
}

impl PendingUserEvents {
    pub(crate) fn with_limits(capacity: usize, max_per_order: usize, ttl_ms: u64) -> Self {
        Self {
            by_clob: IndexMap::new(),
            len: 0,
            capacity: capacity.max(1),
            max_per_order: max_per_order.max(1),
            ttl_ms,
        }
    }

    /// The live gauge. TEST-ONLY on purpose: production reads the cumulative
    /// [`PendingStats`] instead (a counter survives the sweep that clears the gauge, so it is the
    /// one an operator can actually alert on), and the bounds are enforced against `self.len`
    /// directly. Gated rather than kept `pub(crate)` so `-D dead-code` keeps telling the truth
    /// about which accessors production really uses.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }

    /// Park `frame` under `clob_id`. Returns everything the bounds forced OUT, `(clob_id, events)`
    /// per dropped bucket — the caller reports it; nothing is dropped on the floor here.
    pub(crate) fn park(
        &mut self,
        clob_id: &str,
        kind: UserEventKind,
        frame: &serde_json::Value,
        now_ms: u64,
    ) -> Vec<(String, Vec<ParkedEvent>)> {
        let mut dropped = Vec::new();
        let max_per_order = self.max_per_order;
        let bucket = self.by_clob.entry(clob_id.to_string()).or_default();
        bucket.push(ParkedEvent { kind, frame: frame.clone(), parked_ms: now_ms });
        self.len += 1;
        // Per-order bound first: trimming one runaway id in place is preferable to letting it push
        // out other orders' recoverable fills through the global bound below.
        let over = bucket.len().saturating_sub(max_per_order);
        if over > 0 {
            let overflow: Vec<ParkedEvent> = bucket.drain(..over).collect();
            self.len -= overflow.len();
            dropped.push((clob_id.to_string(), overflow));
        }
        // Global bound: shed whole buckets, oldest-parked first.
        while self.len > self.capacity {
            let Some((id, events)) = self.by_clob.shift_remove_index(0) else { break };
            self.len -= events.len();
            dropped.push((id, events));
        }
        dropped
    }

    /// Hand back everything parked under `clob_id` (the registry just gained it).
    pub(crate) fn take(&mut self, clob_id: &str) -> Vec<ParkedEvent> {
        let taken = self.by_clob.shift_remove(clob_id).unwrap_or_default();
        self.len -= taken.len();
        taken
    }

    /// Sweep out everything older than the TTL. Returns `(clob_id, events)` per expired bucket so
    /// the caller can name ids in its `warn!`.
    ///
    /// A bucket expires as a unit, on its OLDEST entry: a later frame for the same unknown id is
    /// more of the same unattributable activity, and letting it extend the bucket's life would let
    /// a steady foreign stream pin capacity indefinitely.
    pub(crate) fn expire(&mut self, now_ms: u64) -> Vec<(String, Vec<ParkedEvent>)> {
        if self.is_empty() {
            return Vec::new();
        }
        let ttl = self.ttl_ms;
        // Buckets are insertion-ordered and every entry is stamped on arrival, so the oldest bucket
        // is always at the front: count the dead prefix, then shed exactly that many. Counting
        // first (rather than removing inside the scan) keeps the read and the write in separate
        // borrows of `by_clob`.
        let dead = self
            .by_clob
            .values()
            .take_while(|events| {
                let oldest = events.first().map(|e| e.parked_ms).unwrap_or(now_ms);
                now_ms.saturating_sub(oldest) >= ttl
            })
            .count();
        let mut expired = Vec::with_capacity(dead);
        for _ in 0..dead {
            let Some((id, events)) = self.by_clob.shift_remove_index(0) else { break };
            self.len -= events.len();
            expired.push((id, events));
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(tag: &str) -> serde_json::Value {
        serde_json::json!({ "tag": tag })
    }

    fn park(p: &mut PendingUserEvents, id: &str, at: u64) -> Vec<(String, Vec<ParkedEvent>)> {
        p.park(id, UserEventKind::Trade, &frame(id), at)
    }

    #[test]
    fn park_then_take_round_trips_and_keeps_len_honest() {
        let mut p = PendingUserEvents::default();
        assert!(p.is_empty());
        assert!(park(&mut p, "0xA", 0).is_empty(), "no bound hit");
        assert!(park(&mut p, "0xA", 1).is_empty());
        assert!(park(&mut p, "0xB", 2).is_empty());
        assert_eq!(p.len(), 3);

        let a = p.take("0xA");
        assert_eq!(a.len(), 2, "both frames for the id come back");
        assert_eq!(a[0].parked_ms, 0, "oldest first");
        assert_eq!(p.len(), 1, "len tracks the take");
        assert!(p.take("0xA").is_empty(), "taking twice yields nothing");
        assert_eq!(p.len(), 1, "…and does not corrupt len");
        assert!(p.take("0xNEVER").is_empty());
        assert_eq!(p.len(), 1);
    }

    /// The GLOBAL bound: at capacity the OLDEST bucket is shed, and it is RETURNED (so the caller
    /// can warn + count) rather than silently dropped.
    #[test]
    fn the_global_bound_sheds_the_oldest_bucket_and_reports_it() {
        let mut p = PendingUserEvents::with_limits(2, 8, DEFAULT_TTL_MS);
        assert!(park(&mut p, "0xOLD", 0).is_empty());
        assert!(park(&mut p, "0xMID", 1).is_empty());
        assert_eq!(p.len(), 2, "exactly at capacity, nothing shed");

        let dropped = park(&mut p, "0xNEW", 2);
        assert_eq!(dropped.len(), 1, "one bucket shed: {dropped:?}");
        assert_eq!(dropped[0].0, "0xOLD", "the OLDEST, not the newest");
        assert_eq!(dropped[0].1.len(), 1);
        assert_eq!(p.len(), 2, "back inside the bound");
        assert!(p.take("0xOLD").is_empty(), "really gone");
        assert_eq!(p.take("0xNEW").len(), 1, "the newest survived");
    }

    /// The PER-ORDER bound: one runaway id is trimmed in place instead of evicting other orders'
    /// recoverable fills.
    #[test]
    fn the_per_order_bound_trims_that_id_only() {
        let mut p = PendingUserEvents::with_limits(64, 2, DEFAULT_TTL_MS);
        assert!(park(&mut p, "0xOTHER", 0).is_empty());
        for at in 1..=2 {
            assert!(park(&mut p, "0xLOUD", at).is_empty());
        }
        let dropped = park(&mut p, "0xLOUD", 3);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].0, "0xLOUD");
        assert_eq!(dropped[0].1.len(), 1, "one frame trimmed");
        assert_eq!(dropped[0].1[0].parked_ms, 1, "the OLDEST of that id");
        assert_eq!(p.take("0xLOUD").len(), 2, "capped at max_per_order");
        assert_eq!(p.take("0xOTHER").len(), 1, "the innocent bucket is untouched");
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn expire_takes_only_buckets_past_the_ttl_and_stops_at_the_first_live_one() {
        let mut p = PendingUserEvents::with_limits(64, 8, 100);
        park(&mut p, "0xOLD", 0);
        park(&mut p, "0xYOUNG", 90);
        // at t=100 the first bucket is exactly at the TTL, the second is not
        let gone = p.expire(100);
        assert_eq!(gone.len(), 1, "{gone:?}");
        assert_eq!(gone[0].0, "0xOLD");
        assert_eq!(p.len(), 1);
        assert!(p.expire(100).is_empty(), "the young bucket is still live");
        assert_eq!(p.expire(190).len(), 1, "…until it is not");
        assert!(p.is_empty());
    }

    /// A bucket ages on its OLDEST entry, so a steady foreign stream cannot pin capacity forever by
    /// keeping one id perpetually "fresh".
    #[test]
    fn a_bucket_ages_on_its_oldest_entry_not_its_newest() {
        let mut p = PendingUserEvents::with_limits(64, 8, 100);
        park(&mut p, "0xNOISY", 0);
        park(&mut p, "0xNOISY", 99);
        let gone = p.expire(100);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].1.len(), 2, "the whole bucket goes, refresher included");
        assert!(p.is_empty());
    }

    #[test]
    fn expire_on_an_empty_park_is_a_no_op() {
        let mut p = PendingUserEvents::default();
        assert!(p.expire(now_ms() + 1_000_000).is_empty());
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn now_ms_is_monotonic() {
        let a = now_ms();
        let b = now_ms();
        assert!(b >= a, "{a} -> {b}");
    }
}
