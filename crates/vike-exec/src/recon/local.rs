//! Owned cross-thread snapshot of one [`crate::ExecutionEngine`]'s local state, for the
//! reconcile driver. `recon::diff` (Task 3) takes a borrowed [`LocalView`]; the reconcile driver
//! runs on a DIFFERENT thread from the engine's single-writer fold, so it needs an OWNED copy it
//! can carry across the thread boundary and then borrow back as a `LocalView`.
//! `ExecutionEngine::local_view` (execution_engine/mod.rs) builds one by cloning the registry,
//! the seen-trade-id dedup set, and folding `Account::positions` (filtered to the engine's own
//! venue) into the `(symbol, position_side) -> signed qty` shape `LocalView` expects. Pure data —
//! no I/O, no clock.

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::order::ManagedOrder;

use super::types::{LocalCash, LocalView};

/// An OWNED, point-in-time read of one engine's local state — the cross-thread twin of
/// [`LocalView`]. Build via [`crate::ExecutionEngine::local_view`]; borrow it back for `diff` via
/// [`OwnedLocalState::as_view`].
pub struct OwnedLocalState {
    pub venue: String,
    /// coid -> managed order (the engine registry).
    pub orders: IndexMap<String, ManagedOrder>,
    /// venue trade_ids already folded (fill dedup source of truth).
    pub seen_trade_ids: HashSet<String>,
    /// (symbol, position_side) -> signed net qty.
    pub positions: IndexMap<(String, String), f64>,
    /// tolerance for PositionDrift (absolute qty).
    pub qty_tol: f64,
    /// cash/balance slice for the first-class cash reconcile (`recon::diff_balance`); see
    /// [`LocalCash`]. `LocalCash::default()` (Delta / no baseline) is inert to the balance diff.
    pub cash: LocalCash,
}

impl OwnedLocalState {
    /// Borrow this owned snapshot as the [`LocalView`] `recon::diff` consumes.
    pub fn as_view(&self) -> LocalView<'_> {
        LocalView {
            venue: &self.venue,
            orders: &self.orders,
            seen_trade_ids: &self.seen_trade_ids,
            positions: &self.positions,
            qty_tol: self.qty_tol,
            cash: self.cash,
        }
    }
}
