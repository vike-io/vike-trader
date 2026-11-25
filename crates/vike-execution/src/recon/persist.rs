//! The PERSISTENCE FILTER of a live reconcile pass (decision 0043 as amended 2026-11-21): a divergence is held,
//! alerted or applied only when [`RECONCILE_CONFIRM_PASSES`] delivered passes IN A ROW raise it, under every policy.
//! Pure: the caller keeps each account's [`ReconcileStreak`] between passes (`vike_core`'s `ReconcilePassMark`) and
//! hands [`persist`]'s confirmed divergences, and nothing else, to [`crate::recon::resolve`].
//!
//! **Why.** A pass compares venue reads taken one after another (orders, fills, positions, balance: 1 to 3.5 s on
//! bybit) with the book at FOLD time, so an order created or filled in between reads as an `OrphanLocalOrder`, a
//! one-lot `PositionDrift` or a `MissingFill` that the next pass no longer raises: 242 holds on bybit#DEMO on
//! 2026-11-20, every one healed by itself, and under `hybrid` each `PositionDrift` among them would have rewritten
//! the book from a stale read. One pass is never proof; a real divergence is held one pass interval later.
//!
//! **The same divergence** is its kind and [`ReconcileDivergence::identity_detail`], the identity a held row is
//! refreshed under, so the orphan orders `resolve` folds into one row ([`crate::recon::ORPHAN_LOCAL_ORDER_KEY`])
//! are judged one ORDER at a time. A `PositionDrift` also carries its signed gap (venue minus local): the race
//! moves the gap by the fill in flight, in either direction, while a missed fill leaves it standing.
//!
//! **What a pass can judge.** A pass that could not raise a kind (`covers` false: no position rows, no balance
//! figure, no journal view) neither breaks nor extends that kind's streaks; one that could and did not raise a
//! divergence drops its streak.
//!
//! **Proven at once.** A divergence other evidence already proves (`proven`) is confirmed on its first pass and
//! counts as seen. `vike_core` names two: a fill of our own order that the restore found FILLED WHILE DOWN, and a
//! balance move that a fill or a liquidation this book refused explains.
//!
//! Cold path: one call per delivered pass, bounded by the divergences the pass raised.

use std::collections::BTreeMap;

use super::types::ReconcileDivergence;
use vike_model::ReconcileDivergenceKind;

/// How many delivered passes in a row must raise one divergence before it is held, alerted or applied. A mechanism
/// constant, kept apart from `RESTORE_QUIET_PASSES` (how many quiet passes prove a restored order gone) because the
/// reasons differ.
pub const RECONCILE_CONFIRM_PASSES: u32 = 2;

/// One account's memory between passes: how many passes in a row raised each divergence (the module doc says
/// what "the same divergence" is). Empty for an account with no pass yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileStreak(BTreeMap<(ReconcileDivergenceKind, String), u32>);

impl ReconcileStreak {
    /// No divergence is pending or standing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether a divergence of a kind `of` accepts is raised and not confirmed yet: a pass that holds nothing still
    /// SAW it (`vike_core`'s balance adoptions do not take a figure beside one that may owe the same money).
    pub fn pending(&self, of: impl Fn(ReconcileDivergenceKind) -> bool) -> bool {
        self.0.iter().any(|((kind, _), &n)| n < RECONCILE_CONFIRM_PASSES && of(*kind))
    }
}

/// Split this pass's `raised` divergences: the CONFIRMED ones (raised by the previous pass too, or `proven`), in
/// their order, and the streak the next pass reads (the module doc). `covers` answers whether this pass could have
/// raised a kind.
pub fn persist(
    previous: &ReconcileStreak,
    raised: Vec<ReconcileDivergence>,
    covers: impl Fn(ReconcileDivergenceKind) -> bool,
    proven: impl Fn(&ReconcileDivergence) -> bool,
) -> (Vec<ReconcileDivergence>, ReconcileStreak) {
    let mut next: BTreeMap<_, _> = previous
        .0
        .iter()
        .filter(|((kind, _), _)| !covers(*kind))
        .map(|(key, &n)| (key.clone(), n))
        .collect();
    let confirmed = raised
        .into_iter()
        .filter(|d| {
            let key = streak_key(d);
            let n = if proven(d) {
                RECONCILE_CONFIRM_PASSES
            } else {
                previous.0.get(&key).map_or(1, |n| (n + 1).min(RECONCILE_CONFIRM_PASSES))
            };
            next.insert(key, n);
            n >= RECONCILE_CONFIRM_PASSES
        })
        .collect();
    (confirmed, ReconcileStreak(next))
}

/// What makes two passes' divergences the same one (the module doc).
fn streak_key(d: &ReconcileDivergence) -> (ReconcileDivergenceKind, String) {
    let identity = d.identity_detail();
    match d {
        // Eight places: a gap of whole lots never moves there by float noise.
        ReconcileDivergence::PositionDrift { report, local_quantity } => {
            (d.kind(), format!("{identity} gap={:.8}", report.qty - local_quantity))
        }
        _ => (d.kind(), identity),
    }
}

#[path = "persist_tests.rs"]
#[cfg(test)]
mod persist_tests;
