//! Polymarket **dust-snap fill tracker** — a per-order submitted-qty ledger that reconciles the
//! venue's fill arithmetic with ours before a fill is emitted.
//!
//! WHY THIS IS POLYMARKET-SPECIFIC: the CLOB quotes prices on a **cent tick** (0.01) and matches
//! by *notional*, so the share quantity a match resolves to is a truncated/rounded quotient. Two
//! failure modes fall out of that, both observed only on this venue:
//!
//! 1. **Overfill.** The summed venue-reported match sizes come out a few dust units *above* the
//!    quantity we submitted. `vike_exec`'s `ManagedOrder` FSM rejects a fill that pushes the
//!    cumulative filled qty past the submitted qty, so the tail fill is dropped and the order
//!    never reaches a terminal state.
//! 2. **Dust remainder.** The mirror case: the venue's matches stop a few dust units *short* of
//!    our submitted qty, so the order sits at `PartiallyFilled` forever even though the venue
//!    considers it done.
//!
//! The tracker fixes both, and ONLY within a tight tolerance ([`DEFAULT_DUST_SNAP_THRESHOLD`],
//! further narrowed per order by [`DUST_FRACTION_OF_SUBMITTED`]): an overfill inside the tolerance
//! is **snapped down** to exactly the submitted qty; a residual inside the tolerance emits ONE
//! synthetic completing fill at the last observed fill price. Anything larger is left completely
//! alone — a genuine partial fill must stay a partial fill, and a genuinely bad overfill must
//! still be rejected loudly by the engine.
//!
//! WHEN THE RESIDUAL IS MINTED: **only from a terminal event** — the `decode_order` UPDATE arm
//! where the venue itself reports `size_matched >= original_size`. It is deliberately NOT minted
//! after every fill: mid-life, a remainder under the tolerance is indistinguishable from qty still
//! genuinely RESTING on the book, and force-completing it would double-count the position when
//! the venue later fills that remainder for real (the bare `Fill` folds `Account` regardless of
//! the FSM's terminal state — its only guard is the core's `seen_trade_ids`).
//!
//! STATUS REPEATS: Polymarket delivers the SAME match up to three times (MATCHED → MINED →
//! CONFIRMED — see [`crate::exec_plane::user_ws`]'s `is_fillable_status`). The core dedups those downstream on
//! the composite `"{trade_id}:{order_id}"`; this tracker sits UPSTREAM of that dedup, so it keeps
//! its own bounded per-order seen-set on the SAME composite key. A repeat replays the qty already
//! emitted for that key WITHOUT re-folding the cumulative — otherwise `filled` would inflate 2–3×,
//! which at best disables the feature (`remaining` goes negative) and at worst snaps away a real
//! venue-confirmed fill.
//!
//! WIRING / OPT-IN: the tracker is threaded in as an `Option<&FillTracker>` at the two call sites
//! ([`crate::exec_plane::user_ws::decode_user_with_tracker`] for the snap, [`crate::exec_plane::client`] `spawn_tracked`
//! for the registration). `None` — which is what every pre-existing entry point passes — is a
//! byte-identical no-op, and an order that was never `register`ed is likewise untouched. NOTE the
//! tracker is currently **opt-in and unwired**: no vike-run/vike-mount/vike-tradehub call site
//! constructs one (and the GUI shell, `vike-app` when this list was written, mounts no venue at
//! all now), so every production path takes the `None` wrapper. Wiring it (one `FillTracker`
//! cloned into BOTH `spawn_tracked` and `spawn_polymarket_user_data_with_resync_tracked` — they
//! MUST be the same clone) is a deliberate follow-up. No other bridge has cent-tick notional
//! matching, so no other bridge gets this.
//!
//! CAPACITY: an insertion-ordered [`IndexMap`] capped at [`DEFAULT_CAPACITY`] entries with FIFO
//! eviction of the oldest — a long-lived pump must not grow unboundedly when orders die without a
//! terminal (cancels that never echo, resync gaps). Eviction only costs an untracked order its
//! dust handling, never correctness.
//!
//! Synthetic fills carry a `":dust"` trade-id suffix so they are distinguishable downstream (and
//! so they can never collide with a real venue trade id in the core's dedup sets).

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

/// Max tracked orders before FIFO eviction of the oldest entry.
pub const DEFAULT_CAPACITY: usize = 10_000;

/// Default snap tolerance, an ABSOLUTE ceiling in *share quantity* units (NOT price ticks): the
/// largest divergence we are willing to silently synthesize or discard. 0.05 shares is worth well
/// under a cent of notional at any Polymarket price (prices live in `(0, 1)`), i.e. below the
/// smallest economically meaningful unit on this venue. An over/undershoot larger than this is a
/// real divergence and is left untouched.
///
/// Note this is only the CEILING — the per-order tolerance is additionally capped at
/// [`DUST_FRACTION_OF_SUBMITTED`] of that order's own submitted qty, so 0.05 can never be a wide
/// band relative to a small order.
pub const DEFAULT_DUST_SNAP_THRESHOLD: f64 = 0.05;

/// The per-order tolerance is `min(threshold, submitted * DUST_FRACTION_OF_SUBMITTED)` — dust must
/// stay dust-sized *relative to the order*, so a 1-share order tolerates 0.01, not 0.05 (5%).
pub const DUST_FRACTION_OF_SUBMITTED: f64 = 0.01;

/// Max composite trade keys remembered per order for the status-repeat dedup (MATCHED/MINED/
/// CONFIRMED). Bounded so a long-lived order cannot grow unboundedly; FIFO, oldest first.
pub const DEFAULT_TRADE_KEYS_PER_ORDER: usize = 256;

/// Trade-id suffix marking a synthetic completing fill minted by [`FillTracker`].
pub const DUST_TRADE_ID_SUFFIX: &str = ":dust";

/// What [`FillTracker::snap_fill_qty`] decided for one venue-reported fill.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SnapOutcome {
    /// Emit a fill of this qty (unchanged, snapped down, or replayed for a status repeat). A
    /// `0.0` here is a venue-REPORTED zero (malformed/zero-size match) and is emitted exactly as
    /// the untracked path would, so its trade id still reaches the core's dedup set.
    Emit(f64),
    /// Emit nothing: the snap itself reduced a positive reported qty to zero (the order is already
    /// full), so there is no qty left to hand the engine.
    Suppress,
}

#[derive(Debug, Clone)]
struct Entry {
    submitted: f64,
    /// Cumulative qty we have already EMITTED (post-snap), not what the venue reported.
    filled: f64,
    /// Last observed fill price — the price a synthetic completing fill is minted at.
    last_px: f64,
    /// Composite `"{trade_id}:{order_id}"` keys already folded → the qty emitted for each. A
    /// status repeat replays its value instead of re-folding. Insertion-ordered, FIFO-capped.
    seen: IndexMap<String, f64>,
}

/// Shared, thread-safe dust-snap ledger. Cheap `Clone` (an `Arc` handle): the exec thread
/// registers submitted quantities, the user-WS pump thread snaps and completes.
#[derive(Clone)]
pub struct FillTracker {
    inner: Arc<Mutex<Inner>>,
    threshold: f64,
    capacity: usize,
}

#[derive(Default)]
struct Inner {
    orders: IndexMap<String, Entry>,
}

impl Default for FillTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FillTracker {
    /// Tracker with the default capacity and dust threshold.
    pub fn new() -> Self {
        Self::with_config(DEFAULT_CAPACITY, DEFAULT_DUST_SNAP_THRESHOLD)
    }

    /// Tracker with an explicit capacity and dust threshold (tests / venue tuning). A
    /// non-positive threshold disables snapping and residual synthesis entirely (every qty passes
    /// through unchanged) — the "off" setting.
    pub fn with_config(capacity: usize, threshold: f64) -> Self {
        Self { inner: Arc::new(Mutex::new(Inner::default())), threshold, capacity: capacity.max(1) }
    }

    /// The configured dust tolerance.
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Number of orders currently tracked (tests/diagnostics).
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().orders.len()
    }

    /// Whether nothing is tracked.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Record the qty we submitted for `coid`, at submit-accept. Re-registering the same coid
    /// resets its ledger (a coid is unique per order, so this only happens on a replayed accept).
    /// Evicts the oldest entry FIFO once capacity is reached.
    pub fn register(&self, coid: &str, submitted_qty: f64) {
        if submitted_qty <= 0.0 {
            return; // nothing meaningful to reconcile against
        }
        let mut g = self.inner.lock().unwrap();
        let entry =
            Entry { submitted: submitted_qty, filled: 0.0, last_px: 0.0, seen: IndexMap::new() };
        if g.orders.insert(coid.to_string(), entry).is_none() {
            while g.orders.len() > self.capacity {
                g.orders.shift_remove_index(0);
            }
        }
    }

    /// Forget an order (terminal cancel/reject).
    pub fn remove(&self, coid: &str) {
        self.inner.lock().unwrap().orders.shift_remove(coid);
    }

    /// The effective tolerance for an order of `submitted` size — the configured ceiling, capped
    /// at [`DUST_FRACTION_OF_SUBMITTED`] of the order itself.
    fn tolerance_for(&self, submitted: f64) -> f64 {
        self.threshold.min(submitted * DUST_FRACTION_OF_SUBMITTED)
    }

    /// Snap a venue-reported fill qty for `coid` and fold it into the ledger. `trade_key` is the
    /// composite `"{trade_id}:{order_id}"` this fill will be emitted under — the SAME key the
    /// core dedups on, and the key this tracker dedups the venue's MATCHED/MINED/CONFIRMED status
    /// repeats on. Returns what should actually be emitted:
    ///
    /// - untracked coid, or a non-positive threshold → `Emit(reported_qty)` unchanged;
    /// - a `trade_key` already folded (a status repeat) → `Emit`/`Suppress` exactly as the FIRST
    ///   delivery decided, with NO second fold of the cumulative. Re-emitting is correct and is
    ///   what the untracked path does: the core dedups it on the same composite key.
    /// - the fill would push cumulative filled past submitted by **at most** the tolerance →
    ///   capped at exactly the remaining qty; if that leaves nothing, `Suppress`;
    /// - a larger overshoot → `Emit(reported_qty)` unchanged (a real divergence the engine must
    ///   see).
    ///
    /// NOTE the accounting is **incremental**: Polymarket's user-channel trade events report a
    /// per-match size (top-level `size` for taker, `maker_orders[].matched_amount` for maker), not
    /// a cumulative total, so the cumulative figure is maintained here from the emitted qtys.
    pub fn snap_fill_qty(
        &self,
        coid: &str,
        trade_key: &str,
        reported_qty: f64,
        px: f64,
    ) -> SnapOutcome {
        if self.threshold <= 0.0 {
            return SnapOutcome::Emit(reported_qty);
        }
        let mut g = self.inner.lock().unwrap();
        let Some(e) = g.orders.get_mut(coid) else {
            return SnapOutcome::Emit(reported_qty);
        };
        // Status repeat (MATCHED → MINED → CONFIRMED): replay the first decision, never re-fold.
        if let Some(&prev) = e.seen.get(trade_key) {
            return outcome(prev, reported_qty);
        }
        if px > 0.0 {
            e.last_px = px;
        }
        let remaining = e.submitted - e.filled;
        let overshoot = reported_qty - remaining;
        let emit = if overshoot > 0.0 && overshoot <= self.tolerance_for(e.submitted) {
            remaining.max(0.0) // dust overfill: cap at exactly the submitted qty
        } else {
            reported_qty
        };
        e.filled += emit;
        e.seen.insert(trade_key.to_string(), emit);
        while e.seen.len() > DEFAULT_TRADE_KEYS_PER_ORDER {
            e.seen.shift_remove_index(0);
        }
        outcome(emit, reported_qty)
    }

    /// Terminal-side dust check for `coid`: if the emitted cumulative qty is short of submitted by
    /// at most the tolerance (and by more than zero), returns `(residual_qty, last_fill_px)` for
    /// ONE synthetic completing fill and **evicts** the order — so a repeated terminal (duplicate
    /// UPDATE / replayed history) can never mint a second one. Returns `None` when the order is
    /// untracked, already completed, or short by more than the tolerance (a genuine remainder).
    ///
    /// Call this ONLY from a terminal path (see the module doc): mid-life, a sub-tolerance
    /// remainder may simply still be resting on the book.
    pub fn check_dust_residual(&self, coid: &str) -> Option<(f64, f64)> {
        if self.threshold <= 0.0 {
            return None;
        }
        let mut g = self.inner.lock().unwrap();
        let e = g.orders.get(coid)?;
        let residual = e.submitted - e.filled;
        let (tol, last_px) = (self.tolerance_for(e.submitted), e.last_px);
        if residual > 0.0 && residual <= tol && last_px > 0.0 {
            g.orders.shift_remove(coid);
            return Some((residual, last_px));
        }
        None
    }
}

/// `Suppress` only when the SNAP itself zeroed a positive reported qty; a venue-reported zero
/// still emits (byte-identical to the untracked path).
fn outcome(emit: f64, reported_qty: f64) -> SnapOutcome {
    if emit <= 0.0 && reported_qty > 0.0 { SnapOutcome::Suppress } else { SnapOutcome::Emit(emit) }
}

impl std::fmt::Debug for FillTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FillTracker")
            .field("tracked", &self.len())
            .field("threshold", &self.threshold)
            .field("capacity", &self.capacity)
            .finish()
    }
}

#[path = "fill_tracker_tests.rs"]
#[cfg(test)]
mod fill_tracker_tests;
