//! Shared, thread-safe CLOB-order-id ↔ vike client-order-id (coid) registry. The Polymarket exec
//! thread writes it when an order goes live — at acceptance, or (behind `POLY_PRESUBMIT_REGISTER`,
//! default OFF — see [`PolymarketRegistry::on_accept`] and `crate::exec_plane::client`'s submit arm) just BEFORE
//! the submit; the user-WS pump (a separate thread) reads it to re-key decoded fills/cancels — the
//! CLOB server assigns the order id (no client-id echo), so this map is the only bridge back to the
//! coid the core FSM keys on. The order side is stored alongside so a maker fill (whose top-level
//! trade `side` is the taker's) folds with OUR order's side.
//!
//! # An absent id is a QUESTION, not an answer
//!
//! Because the exec thread and the pump are different threads, "this id is not in the map" has two
//! completely different meanings, and treating them as one is what let executed fills vanish:
//!
//! * **not ours** — another client on this wallet, a counterparty leg of a match we were in, or one
//!   of our own orders from a *previous process* (this map is in-memory; a restart forgets every
//!   resting order). Nothing local can re-key it, ever.
//! * **not YET ours** — ours, but the id has not been written yet. Two verified races produce it:
//!   the venue matching an order before `submit_order`'s HTTP response gets back to the exec thread
//!   (the mitigation for that, `POLY_PRESUBMIT_REGISTER`, ships OFF), and a cancel racing a match.
//!
//! So this type answers the question instead of guessing at it. Two mechanisms, one per race, both
//! ON by default and neither behind a flag — the bug being fixed here is *precisely* what an
//! off-by-default mitigation costs:
//!
//! 1. **The park** ([`crate::exec_plane::pending_events`]) — an event whose id is absent is staged under that
//!    id and handed straight back by [`on_accept`](Self::on_accept) the moment it is written. The
//!    park lives INSIDE this type's `Mutex` rather than owning its own, which is what makes it
//!    race-free instead of merely likely: "look up, and park iff absent" and "insert, and take back
//!    what was parked" are each one critical section, so the exec thread cannot slip an `on_accept`
//!    between the decoder's failed lookup and its park. Anything never claimed inside
//!    [`DEFAULT_TTL_MS`] is "not ours" and dies with a `warn!` and a counter — never silently.
//! 2. **The settling grace** ([`remove`](Self::remove)) — a cancelled order's clob→coid row is
//!    DEMOTED for [`DEFAULT_SETTLING_GRACE_MS`] rather than deleted, so a match that raced its own
//!    cancel still re-keys. The park cannot help there (the id can never be re-inserted, so there is
//!    nothing to replay against); without the grace, that fill is real money that folds nowhere.
//!    `crate::exec_plane::client` makes this reachable in the ordinary case, not just the exotic one: it treats
//!    ANY HTTP 200 from `cancel_order` as success and never reads the `not_canceled` half of the
//!    body, so cancelling an order the venue just matched still runs `remove`.
//!
//! [`lookup_clob`](PolymarketRegistry::lookup_clob) is deliberately left alone by both — it means
//! "live right now", which is what `crate::exec_plane::recon_client` needs when it decides whether a venue order
//! report has a local twin. The decoder uses [`rekey_for_decode`](PolymarketRegistry::rekey_for_decode)
//! instead.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use crate::exec_plane::pending_events::{
    DEFAULT_CAPACITY, DEFAULT_MAX_PER_ORDER, ParkedEvent, PendingStats, PendingUserEvents,
    UserEventKind, now_ms,
};

/// How long a cancelled order's clob→coid row stays re-keyable after [`PolymarketRegistry::remove`].
///
/// Covers exactly one thing: the venue matched the order and the trade frame is already on the wire
/// when our cancel tears the mapping down. That gap is one WS delivery, i.e. milliseconds; the
/// window is set at the same order as the park's TTL so both races are reported on the same
/// timescale, and generously rather than tightly because the failure mode of being too SHORT is a
/// permanently wrong position while the failure mode of being too LONG is re-emitting a fill the
/// core already deduped on its `trade_id`.
pub const DEFAULT_SETTLING_GRACE_MS: u64 = 30_000;

/// Max demoted rows held at once, FIFO. Bounds a long-lived maker's cancel churn; an evicted row
/// simply reverts to today's behaviour for that order (unresolvable), so the bound can never make
/// anything worse than it was before the grace existed.
pub const DEFAULT_SETTLING_CAPACITY: usize = 1024;

#[derive(Clone, Default)]
pub struct PolymarketRegistry(Arc<Mutex<Maps>>);

struct Maps {
    clob_to_coid: HashMap<String, (String, i32)>,
    coid_to_clob: HashMap<String, String>,
    /// clob id → (coid, side, removed_ms) for orders `remove`d within the settling grace.
    /// Insertion-ordered so the FIFO bound and the age sweep are both front-of-map operations.
    settling: IndexMap<String, (String, i32, u64)>,
    settling_grace_ms: u64,
    settling_capacity: usize,
    pending: PendingUserEvents,
    stats: PendingStats,
}

impl Default for Maps {
    fn default() -> Self {
        Self {
            clob_to_coid: HashMap::new(),
            coid_to_clob: HashMap::new(),
            settling: IndexMap::new(),
            settling_grace_ms: DEFAULT_SETTLING_GRACE_MS,
            settling_capacity: DEFAULT_SETTLING_CAPACITY,
            pending: PendingUserEvents::default(),
            stats: PendingStats::default(),
        }
    }
}

impl Maps {
    /// Drop demoted rows past the grace. Front-of-map because `settling` is insertion-ordered and
    /// every row is stamped on insert.
    fn sweep_settling(&mut self, now: u64) {
        let grace = self.settling_grace_ms;
        let dead = self
            .settling
            .values()
            .take_while(|(_, _, at)| now.saturating_sub(*at) >= grace)
            .count();
        for _ in 0..dead {
            let _ = self.settling.shift_remove_index(0);
        }
    }
}

impl PolymarketRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Explicit bounds, for tests that need to observe a boundary without sleeping (park capacity /
    /// per-order cap / park TTL / settling grace). Production uses [`new`](Self::new), i.e. the
    /// `DEFAULT_*` constants.
    pub fn with_limits(
        capacity: usize,
        max_per_order: usize,
        ttl_ms: u64,
        settling_grace_ms: u64,
    ) -> Self {
        Self(Arc::new(Mutex::new(Maps {
            pending: PendingUserEvents::with_limits(capacity, max_per_order, ttl_ms),
            settling_grace_ms,
            ..Maps::default()
        })))
    }

    /// Record a live order so the user-WS pump can re-key its fills/cancels back to the coid — at
    /// acceptance (the venue's `orderID`), or pre-submit under `POLY_PRESUBMIT_REGISTER` (the derived
    /// `crate::exec_plane::order::derive_order_id`, which the CLOB returns verbatim as `orderID`).
    ///
    /// **Returns everything the park was holding for `clob_id`** — the user-channel events that
    /// arrived before this write and could not be re-keyed then. They are handed back here, under
    /// the same lock as the insert, because this is the exact instant "not yet ours" becomes "ours";
    /// the caller must decode and emit them (`crate::exec_plane::user_ws::replay_parked` builds the events,
    /// `crate::exec_plane::client`'s submit arm sends them). Dropping the return value silently re-creates the
    /// executed-fill-vanishes bug, which is why it is `#[must_use]`.
    ///
    /// **Idempotency contract — a fixed `(coid, clob_id, side)` re-registers as a true no-op.** Both
    /// directions are plain `HashMap::insert`s, so calling this twice with the SAME triple overwrites
    /// each entry with an identical value and leaves both maps byte-for-byte unchanged: no duplicate
    /// row, no append, no counter. That is exactly what the DEFAULT-OFF pre-submit path relies on
    /// (`crate::exec_plane::client`'s submit arm): the exec thread may register coid↔`derive_order_id(&order)`
    /// BEFORE the network submit to close the ack-race (a user-WS fill can beat the HTTP ack), and the
    /// real server `OrderAccepted` — which carries the SAME id (the CLOB keys orders by that EIP-712
    /// hash) and the SAME side — then calls this again, changing nothing. The park makes that
    /// idempotency total rather than partial: the second call simply finds an empty park and returns
    /// an empty `Vec`. (Re-registering a coid with a *different* clob_id last-writer-wins on
    /// `coid_to_clob` and leaves the previous `clob_to_coid` row orphaned; the pre-submit path never
    /// does that, because the derived id is LIVE-VERIFIED equal to the server id — see
    /// `crate::exec_plane::client::tests::live_place_and_cancel`.)
    #[must_use = "the parked events returned here MUST be decoded and emitted — dropping them is \
                  exactly the silent executed-fill drop this return value exists to prevent"]
    pub fn on_accept(&self, coid: &str, clob_id: &str, side: i32) -> Vec<ParkedEvent> {
        let mut m = self.0.lock().unwrap();
        m.clob_to_coid.insert(clob_id.to_string(), (coid.to_string(), side));
        m.coid_to_clob.insert(coid.to_string(), clob_id.to_string());
        // The order is live again: whatever it was demoted for no longer applies.
        let _ = m.settling.shift_remove(clob_id);
        let claimed = m.pending.take(clob_id);
        if claimed.is_empty() {
            return claimed;
        }
        m.stats.replayed += claimed.len() as u64;
        let waited = now_ms().saturating_sub(claimed.first().map(|e| e.parked_ms).unwrap_or(0));
        drop(m);
        // "not YET ours", resolved. INFO, not WARN: this is the benign self-healing case, and
        // warning on it would train operators to ignore the expiry warn that means real loss.
        // Per accepted order, never per message — the hot-fold logging rule.
        tracing::info!(
            venue = "polymarket",
            clob_id,
            coid,
            events = claimed.len(),
            waited_ms = waited,
            "replaying user-channel events that arrived before this order was registered"
        );
        claimed
    }

    /// Re-key a WS/replay event: CLOB id → (coid, order side). `None` = not one of our orders.
    ///
    /// **LIVE orders only** — a `remove`d order reads `None` here even inside its settling grace.
    /// `crate::exec_plane::recon_client` depends on that: it asks this to decide whether a venue order report
    /// has a local twin, and a demoted row is by definition one we believe is gone. The decoder
    /// wants the other question and calls [`rekey_for_decode`](Self::rekey_for_decode).
    pub fn lookup_clob(&self, clob_id: &str) -> Option<(String, i32)> {
        self.0.lock().unwrap().clob_to_coid.get(clob_id).cloned()
    }

    /// The DECODER's re-key path: [`lookup_clob`](Self::lookup_clob), then the settling grace.
    ///
    /// The second lookup is what saves a match that raced its own cancel (see this module's doc):
    /// the money moved, so folding it is correct and dropping it is not. The FSM may already have
    /// terminated that order, in which case the `OrderPartiallyFilled` wrapper is an invalid
    /// transition the engine drops — but the bare `Event::Fill` still folds `Account`, which is the
    /// half that actually diverges from the venue. `crate::exec_plane::client`'s submit arm already reasons this
    /// way about the pre-registered rows it deliberately leaves behind after a reject.
    pub(crate) fn rekey_for_decode(&self, clob_id: &str) -> Option<(String, i32)> {
        let mut m = self.0.lock().unwrap();
        if let Some(hit) = m.clob_to_coid.get(clob_id).cloned() {
            return Some(hit);
        }
        let now = now_ms();
        m.sweep_settling(now);
        let (coid, side, _) = m.settling.get(clob_id).cloned()?;
        m.stats.settled += 1;
        drop(m);
        tracing::info!(
            venue = "polymarket",
            clob_id,
            coid,
            "re-keying a user-channel event for an order cancelled moments ago (a match that raced \
             its own cancel) — the fill is real and must fold"
        );
        Some((coid, side))
    }

    /// Park one user-channel frame under every CLOB id it is waiting on (a trade frame can name
    /// several). Called by the decoder when NOTHING in the frame re-keyed — see
    /// `crate::exec_plane::user_ws::decode_trade` for why a frame with at least one resolved leg parks nothing.
    pub(crate) fn park(&self, clob_ids: &[String], kind: UserEventKind, frame: &serde_json::Value) {
        if clob_ids.is_empty() {
            return;
        }
        let now = now_ms();
        let mut evicted = Vec::new();
        {
            let mut m = self.0.lock().unwrap();
            for id in clob_ids {
                evicted.extend(m.pending.park(id, kind, frame, now));
                m.stats.parked += 1;
            }
            let n: usize = evicted.iter().map(|(_, e)| e.len()).sum();
            m.stats.evicted += n as u64;
        }
        // Benign-until-proven-otherwise: DEBUG on the way in. The `warn!` is spent on the expiry,
        // which is the only place we actually know something was lost.
        tracing::debug!(
            venue = "polymarket",
            ids = clob_ids.len(),
            ?kind,
            "user-channel frame names no registered order — parked pending registration"
        );
        if !evicted.is_empty() {
            let n: usize = evicted.iter().map(|(_, e)| e.len()).sum();
            tracing::warn!(
                venue = "polymarket",
                dropped_events = n,
                dropped_orders = evicted.len(),
                sample = ?sample_ids(&evicted),
                capacity = DEFAULT_CAPACITY,
                max_per_order = DEFAULT_MAX_PER_ORDER,
                "pending user-event park hit a bound and shed its OLDEST entries — if these were \
                 ours, their fills are LOST; raise the bound or find what is flooding it"
            );
        }
    }

    /// Sweep the park's TTL. Everything it returns is the genuine "not ours" case, reported loudly.
    /// Returns how many events were discarded, so a caller (and the tests) can observe it without
    /// scraping logs. Cheap on the common path — an empty park short-circuits.
    pub(crate) fn expire_pending(&self) -> usize {
        self.expire_pending_at(now_ms())
    }

    /// [`expire_pending`](Self::expire_pending) against an explicit clock (tests).
    pub(crate) fn expire_pending_at(&self, now: u64) -> usize {
        let (expired, ttl) = {
            let mut m = self.0.lock().unwrap();
            if m.pending.is_empty() {
                return 0;
            }
            let ttl = m.pending.ttl_ms();
            let expired = m.pending.expire(now);
            let n: usize = expired.iter().map(|(_, e)| e.len()).sum();
            m.stats.expired += n as u64;
            (expired, ttl)
        };
        let n: usize = expired.iter().map(|(_, e)| e.len()).sum();
        if n > 0 {
            let oldest = expired
                .iter()
                .flat_map(|(_, e)| e.iter())
                .map(|e| now.saturating_sub(e.parked_ms))
                .max()
                .unwrap_or(0);
            // The one place we KNOW something was lost. "not ours" — no local mapping can ever
            // arrive for these ids, so account-level POLY_RECONCILE is the only remaining backstop.
            tracing::warn!(
                venue = "polymarket",
                discarded_events = n,
                discarded_orders = expired.len(),
                oldest_age_ms = oldest,
                ttl_ms = ttl,
                sample = ?sample_ids(&expired),
                "discarding user-channel events for CLOB orders this process never registered — \
                 NOT ours (another client on this wallet, a counterparty leg, or our own order from \
                 a previous run); if any were ours their fills never folded, and only reconcile \
                 can repair it"
            );
        }
        n
    }

    /// Cumulative counters for the park + settling lanes (see [`PendingStats`]).
    pub fn pending_stats(&self) -> PendingStats {
        self.0.lock().unwrap().stats
    }

    /// How many events are parked right now — TEST-ONLY, the gauge behind `PendingUserEvents::len`.
    /// Production observes this lane through [`pending_stats`](Self::pending_stats), whose counters
    /// are cumulative and therefore still readable after the sweep that returns this gauge to zero.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.0.lock().unwrap().pending.len()
    }

    /// The venue order id for a coid (exec thread, to cancel).
    pub fn coid_to_clob(&self, coid: &str) -> Option<String> {
        self.0.lock().unwrap().coid_to_clob.get(coid).cloned()
    }

    /// Does `coids` name EVERY order this map currently holds — i.e. is this batch the whole of
    /// what this mount believes is resting?
    ///
    /// The one question the bulk-cancel arm has to answer (`crate::exec_plane::exec::CancelScope`), asked HERE
    /// because it is a question about this map's contents: answering it outside would mean copying
    /// the key set out from under the lock and racing an `on_accept` against the comparison.
    ///
    /// ⚠ **It answers about what THIS PROCESS knows, which is strictly less than the account.** The
    /// map is in-memory, so an order a PREVIOUS process left resting — or one another client placed
    /// on the same signer — is invisible to it, while `DELETE /cancel-all` is account-wide and
    /// would cancel that one too. `crate::exec_plane::exec::CancelScope::WholeBook`'s doc owns that residual;
    /// this is only the strongest assertion available in-tree, not a guarantee about the venue.
    pub fn covers_all_live(&self, coids: &[String]) -> bool {
        let named: HashSet<&str> = coids.iter().map(String::as_str).collect();
        let m = self.0.lock().unwrap();
        m.coid_to_clob.keys().all(|c| named.contains(c.as_str()))
    }

    /// Drop an order on a terminal (cancel/reject) — exec thread.
    ///
    /// The coid→clob direction is deleted outright: the exec thread must not be able to cancel a
    /// dead order twice. The clob→coid direction is **demoted**, not deleted — held for
    /// [`DEFAULT_SETTLING_GRACE_MS`] where only [`rekey_for_decode`](Self::rekey_for_decode) can see
    /// it — because a cancel and a match race, and the losing side of that race is an executed fill
    /// already on the wire. `crate::exec_plane::client` makes this the ordinary case rather than an exotic one:
    /// it reads any HTTP 200 from `cancel_order` as success without inspecting the `not_canceled`
    /// half of the body, so an order the venue *just matched* is `remove`d exactly like one that
    /// really rested. `lookup_clob` still reports the order as gone, which is what reconcile means.
    pub fn remove(&self, coid: &str) {
        let now = now_ms();
        let mut m = self.0.lock().unwrap();
        let Some(clob) = m.coid_to_clob.remove(coid) else { return };
        let demoted = m.clob_to_coid.remove(&clob);
        if let Some((coid, side)) = demoted {
            let _ = m.settling.insert(clob, (coid, side, now));
            m.sweep_settling(now);
            while m.settling.len() > m.settling_capacity {
                let _ = m.settling.shift_remove_index(0);
            }
        }
    }
}

/// Up to four ids, for a log line that names names without dumping the whole park.
fn sample_ids(buckets: &[(String, Vec<ParkedEvent>)]) -> Vec<&str> {
    buckets.iter().take(4).map(|(id, _)| id.as_str()).collect()
}

#[path = "registry_tests.rs"]
#[cfg(test)]
mod registry_tests;
