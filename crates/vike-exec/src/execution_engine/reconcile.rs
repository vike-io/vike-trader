//! Engine output/snapshot types: `AppliedFill` + `OrderEventOut` (per-transition captures the runtime
//! delivers to a mounted strategy) and `ReconcileSnapshot` (REST-truth session-start mirror). Split
//! out of the engine module; re-exported by `mod.rs` so external paths are unchanged.

use vike_model::events::FillEvent;

use crate::order::ManagedOrder;

/// One account-accepted fill captured for `Strategy::on_fill` delivery, with the ledger state
/// snapshotted at the fold — per fill, not per batch — so the handler sees exactly what the
/// backtest engine's `fire_on_fill` would show after the same fill.
#[derive(Debug, Clone)]
pub struct AppliedFill {
    pub fill: FillEvent,
    /// signed BOTH-leg position immediately after this fill folded
    pub position_after: f64,
    /// `Account::equity_all(equity_seed)` immediately after this fill folded
    pub equity_after: f64,
}

/// One NON-FILL order-lifecycle transition captured for `Strategy::on_order_event` delivery, tagged
/// with the order's (venue, symbol) so the runtime routes it to the owning mount — the on_fill
/// lane's twin (see [`AppliedFill`]). Captured at the ONE FSM-apply site (accept/reject/cancel/
/// expire, `(venue, symbol)` read from the just-advanced registry order) plus the `RiskGate` veto
/// site (deny, `(venue, symbol)` from the request — a denied order never enters the registry). Only
/// gathered when `collect_applied_fills` is set (a strategy is mounted); a GUI-only engine never
/// grows the buffer. Not serialized (deliveries are not replayed, like `applied_fills`).
#[derive(Debug, Clone)]
pub struct OrderEventOut {
    pub venue: String,
    pub symbol: String,
    pub event: vike_model::strategy::OrderLifecycle,
}

/// Reconcile snapshot (REST truth at session start) — mirror of `exec/reconcile.py`'s shape.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReconcileSnapshot {
    /// (symbol, signed qty), index-aligned with `position_sides`
    pub positions: Vec<(String, f64)>,
    pub open_orders: Vec<ManagedOrder>,
    /// (symbol, avg_px), index-aligned with `position_sides`
    pub position_avg_px: Vec<(String, f64)>,
    pub position_mark_px: Vec<(String, f64)>,
    /// (symbol, 'BOTH'|'LONG'|'SHORT'); empty = net/spot snapshot (all 'BOTH')
    pub position_sides: Vec<(String, String)>,
    /// TOTAL venue wallet balance — MUST include isolated-margin allocations (Binance
    /// `balance` and Bybit `walletBalance` both do): the margin-call watchdog's isolated arm
    /// subtracts each reported isolated wallet from the cross pool's equity, so a venue
    /// feeding a cross-only balance here would silently double-subtract them (see
    /// `margin_call.rs`).
    pub balance: f64,
    /// (symbol, venue-REPORTED margin mode, isolated-wallet balance) — index-aligned with
    /// `positions`, same law as `position_sides` (margin-mode step-2: read-side only). EMPTY =
    /// the venue reported no margin info for this snapshot — `apply_snapshot` then carries each
    /// prior entry's mode forward (the #487 behavior, byte-identical). Non-empty rows OVERWRITE
    /// the entry's carrier with venue truth — including a Cross report flipping a stale local
    /// Isolated back to Cross (the venue is the mode authority). `#[serde(default)]` keeps
    /// pre-field journaled `Command::ApplySnapshot` payloads replayable; skip-if-empty keeps a
    /// no-margin snapshot's JSON byte-identical to pre-field records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub position_margin: Vec<(String, vike_model::MarginMode, Option<f64>)>,
}
