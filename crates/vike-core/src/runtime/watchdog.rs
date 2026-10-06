//! Cold-path sweeps — the stuck-order watchdog ladder, safe-state entry, the margin-call sweep, and
//! the equity-drawdown latch — split out of the runtime fold module (behavior byte-identical; the
//! block moved verbatim). `use super::*` re-exports the parent runtime module's full import set +
//! items, so nothing about resolution changes.

mod liveness;
mod orders;
mod risk;

use super::*;

/// "This order will not respond to a cancel" — the predicate BOTH core sweeps (the safe-state
/// working-order sweep and the GTD expiry sweep) filter their registry walk on, named once.
///
/// It is deliberately WIDER than [`vike_exec::OrderStatus::is_terminal`], by exactly `Liquidated`.
/// `is_terminal` answers a lifecycle question — can this order still receive events? — and
/// `Liquidated` is a live perp force-close state there, so it is excluded. These sweeps ask an
/// OPERATIONAL question instead: is there anything left worth cancelling? A force-closed order has
/// no resting quantity, so cancelling it is a pointless round trip. Built ON TOP of `is_terminal`
/// so the two predicates cannot silently drift apart if a new terminal status is added.
fn is_terminal_status(status: vike_exec::OrderStatus) -> bool {
    status.is_terminal() || matches!(status, vike_exec::OrderStatus::Liquidated)
}
