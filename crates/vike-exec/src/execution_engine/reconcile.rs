//! Engine output/snapshot types: `AppliedFill` + `OrderEventOut` (per-transition captures the
//! runtime delivers to a mounted strategy) and `ReconcileSnapshot` (REST-truth session-start
//! mirror). Re-exported by `mod.rs`.

use vike_model::events::{FillEvent, PositionSide};

use vike_model::ManagedOrder;

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
    /// What the account folded this fill WITH, when the account holds a foreign-quoted symbol; `None`
    /// otherwise (every single-currency account, and then the mount ledger reads exactly what it
    /// always read). A per-mount ledger that re-folds a fill from the fill alone, at the time it is
    /// dispatched, would price it at a rate that may have moved since the account folded it, and
    /// would add a fee in the symbol's currency to a ledger in the account's.
    pub fx: Option<AppliedFx>,
}

impl AppliedFill {
    /// Whether this is not a fill but a FEE CORRECTION of one the account already folded
    /// (`Event::FeeAdjustment`, decision 0238): a fill of zero quantity and no side whose `fx.charge` is
    /// the CHANGE the account booked. A per-mount ledger takes the charge into its `fees_paid` and moves
    /// no position; a strategy hears nothing (there was no execution). Read off the fill itself rather
    /// than a flag, so the hot `Fill` arm that builds the ordinary entry carries nothing extra.
    pub fn is_fee_only(&self) -> bool {
        self.fill.side == 0 && self.fill.last_qty == 0.0
    }
}

/// The two numbers a per-mount ledger must take from the account instead of recomputing: see
/// [`AppliedFill::fx`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppliedFx {
    /// The contract value the account folded the fill with: the EFFECTIVE value, in the account's
    /// currency.
    pub value: vike_model::ContractValue,
    /// The commission as the account booked it into its ledger, in the account's currency (`0.0`
    /// when the fee was in a currency the account cannot convert and was left out).
    pub charge: f64,
}

/// One NON-FILL order-lifecycle transition captured for `Strategy::on_order_event` delivery, tagged
/// with the order's (venue, symbol) so the runtime routes it to the owning mount — the on_fill
/// lane's twin (see [`AppliedFill`]). Captured at the ONE FSM-apply site (accept/reject/cancel/
/// expire) plus the `RiskGate` veto site (deny — a denied order never enters the registry). Only
/// gathered when `collect_applied_fills` is set. Not serialized (deliveries are not replayed).
#[derive(Debug, Clone)]
pub struct OrderEventOut {
    pub venue: String,
    pub symbol: String,
    pub event: vike_model::OrderLifecycle,
}

/// Reconcile snapshot (REST truth at session start).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReconcileSnapshot {
    /// (symbol, signed qty), index-aligned with `position_sides`
    pub positions: Vec<(String, f64)>,
    pub open_orders: Vec<ManagedOrder>,
    /// (symbol, avg_px), index-aligned with `position_sides`
    pub position_avg_px: Vec<(String, f64)>,
    pub position_mark_px: Vec<(String, f64)>,
    /// (symbol, side) per row; empty = a net/spot snapshot (every row [`PositionSide::Both`]).
    /// Serialized as the side's own uppercase word (`"BOTH"`/`"LONG"`/`"SHORT"`).
    pub position_sides: Vec<(String, PositionSide)>,
    /// TOTAL venue wallet balance — MUST include isolated-margin allocations (Binance
    /// `balance` and Bybit `walletBalance` both do): the margin-call watchdog's isolated arm
    /// subtracts each reported isolated wallet from the cross pool's equity, so a venue
    /// feeding a cross-only balance here would silently double-subtract them (see
    /// `margin_call.rs`).
    pub balance: f64,
    /// (symbol, venue-REPORTED margin mode, isolated-wallet balance) — index-aligned with
    /// `positions`, like `position_sides`. EMPTY = the venue reported no margin info, and
    /// `apply_snapshot` carries each prior entry's mode forward. Non-empty rows OVERWRITE the
    /// entry's carrier with venue truth, including a Cross report flipping a stale local Isolated
    /// back (the venue is the mode authority). `#[serde(default)]` keeps pre-field journaled
    /// `Command::ApplySnapshot` payloads replayable; skip-if-empty keeps a no-margin snapshot's
    /// JSON byte-identical to pre-field records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub position_margin: Vec<(String, vike_model::MarginMode, Option<f64>)>,
}
