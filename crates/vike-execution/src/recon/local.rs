//! Owned cross-thread snapshot of one [`crate::Engine`]'s local state, for the
//! reconcile driver: `recon::diff` takes a borrowed [`ReconcileLocalView`], and the driver runs on a
//! DIFFERENT thread from the engine's single-writer fold, so it needs an OWNED copy it can carry
//! across the thread boundary and borrow back. `Engine::local_view`
//! (execution_engine/reconcile_apply.rs) builds one by cloning the registry and the seen-trade-id
//! dedup set and folding `Account::positions` (filtered to the engine's own venue) into the
//! `(symbol, position_side) -> signed qty` shape `ReconcileLocalView` expects. Pure data — no I/O, no clock.

use std::collections::HashSet;

use indexmap::IndexMap;

use vike_model::OrderRecord;
use vike_model::events::PositionSide;

use super::types::{ReconcileLocalCash, ReconcileLocalView};

/// An OWNED, point-in-time read of one engine's local state — the cross-thread twin of
/// [`ReconcileLocalView`]. Build via [`crate::Engine::local_view`]; borrow it back for `diff` via
/// [`ReconcileLocalState::as_view`].
pub struct ReconcileLocalState {
    pub venue: String,
    /// order client id -> managed order (the engine registry).
    pub orders: IndexMap<String, OrderRecord>,
    /// venue trade_ids already folded (fill dedup source of truth).
    pub seen_trade_ids: HashSet<String>,
    /// (symbol, position_side) -> signed net qty.
    pub positions: IndexMap<(String, PositionSide), f64>,
    /// tolerance for PositionDrift (absolute qty).
    pub quantity_tolerance: f64,
    /// cash/balance slice for the first-class cash reconcile (`recon::diff_balance`); see
    /// [`ReconcileLocalCash`]. `ReconcileLocalCash::default()` (Delta / no baseline) is inert to the balance diff.
    pub cash: ReconcileLocalCash,
}

impl ReconcileLocalState {
    /// Borrow this owned snapshot as the [`ReconcileLocalView`] `recon::diff` consumes.
    pub fn as_view(&self) -> ReconcileLocalView<'_> {
        ReconcileLocalView {
            venue: &self.venue,
            orders: &self.orders,
            seen_trade_ids: &self.seen_trade_ids,
            positions: &self.positions,
            quantity_tolerance: self.quantity_tolerance,
            cash: self.cash,
        }
    }
}
